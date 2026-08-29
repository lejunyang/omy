//! 会话密钥缓存与自动锁定。
//!
//! # 这个模块解决的核心问题
//!
//! 需求 F-12 要求「扫描目录、自动匹配多个浏览密码」。若每个文件都跑一次
//! Argon2id，代价是灾难性的：
//!
//! ```text
//! 10000 文件 × 5 密码 × 180 ms ≈ 2.5 小时
//! ```
//!
//! 两级密钥体系让慢速 KDF 只在「密码 → KEK」这一步执行**一次**，此后每个文件
//! 只做 HKDF（13.2 µs，快 13,700×）。前提是 KEK 必须被缓存复用——这正是本模块
//! 的职责。实测：5 密码派生 765 ms，随后扫描 500 文件仅 171 ms。
//!
//! # 缓存键的设计
//!
//! 缓存键是 `(vault_salt, 凭据标识)`（规范 §11.1）。
//!
//! **为什么必须含 `vault_salt`**：同一个密码在不同 vault（不同 salt）下派生出的
//! KEK 完全不同。若只用密码做键，切换 vault 后会复用错误的 KEK，导致本该解开的
//! 文件解不开——而且这种 bug 表现为「密码错误」，极难排查。
//!
//! # 内存卫生
//!
//! [`SessionKeys`] 持有的 KEK 在移除或整体析构时清零。缓存**只存在于内存**，
//! 绝不落盘（规范 §10 内存卫生）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use zeroize::Zeroize as _;

use crate::crypto::{Argon2Params, Kek, SecretKey};
use crate::error::Result;

/// 默认空闲锁定时长：15 分钟（规范 §11.2）。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// 凭据来源。
///
/// 决定该凭据是否参与自动扫描——这是性能与安全的权衡点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialKind {
    /// 用户输入的浏览密码，`Argon2id(密码, vault_salt)`。参与扫描。
    Vault,
    /// 由 OS 密钥库经生物识别解出的设备密钥。参与扫描。
    Device,
    /// 单文件独立密码，salt 不是 `vault_salt`。**不参与扫描**。
    Portable,
    /// 恢复码，熵已足够，直接作 KEK。**不参与扫描**。
    Recovery,
}

impl CredentialKind {
    /// 该类型是否参与目录自动扫描。
    ///
    /// `Portable` 与 `Recovery` 不参与：前者的 salt 因文件而异，无法复用 KEK；
    /// 后者需用户手动输入。两者都只在「手动打开单个文件」时尝试。
    #[must_use]
    pub const fn scannable(self) -> bool {
        matches!(self, Self::Vault | Self::Device)
    }

    /// 稳定标识串，用于日志与错误信息（不含任何秘密）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::Device => "device",
            Self::Portable => "portable",
            Self::Recovery => "recovery",
        }
    }
}

/// 缓存键：`(vault_salt, 凭据标识)`。
///
/// `label` 是调用方给凭据起的名字（如 "主密码"、"诱饵"），**不是密码本身**。
/// 这样既能区分多个凭据，又不必在缓存键里保留秘密。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    vault_salt: [u8; 16],
    kind: CredentialKind,
    label: String,
}

/// 一个已解锁的凭据条目。
#[derive(Debug)]
pub struct UnlockedCredential {
    /// 凭据名称，供 UI 显示（不含秘密）。
    pub label: String,
    /// 凭据类型。
    pub kind: CredentialKind,
    /// 派生出的 KEK。
    pub kek: Kek,
}

/// 会话密钥缓存。
///
/// # 生命周期
///
/// 会话内有效。任一锁定条件触发（空闲超时、系统睡眠、切后台、手动锁定）
/// 即调用 [`SessionKeys::lock`] 清零全部 KEK。
///
/// # 示例
///
/// ```
/// use omy_core::session::{SessionKeys, CredentialKind};
/// use omy_core::crypto::Argon2Params;
///
/// let mut s = SessionKeys::new();
/// let salt = [0x11u8; 16];
/// // 第一次会跑 Argon2（慢）
/// s.unlock_password("主密码", &salt, "hunter2", Argon2Params::TEST_WEAK)?;
/// // 第二次命中缓存，不再跑 Argon2
/// s.unlock_password("主密码", &salt, "hunter2", Argon2Params::TEST_WEAK)?;
/// assert_eq!(s.len(), 1);
/// # Ok::<(), omy_core::Error>(())
/// ```
#[derive(Debug)]
pub struct SessionKeys {
    cache: HashMap<CacheKey, Kek>,
    /// 最近一次活动时间，用于空闲判定。
    last_activity: Instant,
    /// 空闲锁定阈值。
    idle_timeout: Duration,
    /// 累计的 Argon2 执行次数，用于验证缓存确实生效。
    kdf_runs: u64,
}

impl Default for SessionKeys {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionKeys {
    /// 新建空会话，使用默认 15 分钟空闲锁定。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            last_activity: Instant::now(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            kdf_runs: 0,
        }
    }

    /// 设置空闲锁定时长。
    #[must_use]
    pub const fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// 用密码解锁并缓存 KEK。
    ///
    /// 若 `(vault_salt, kind, label)` 已在缓存中，**直接返回不重跑 Argon2**——
    /// 这是整个扫描性能的基础。
    ///
    /// # Errors
    ///
    /// Argon2 派生失败时返回 [`crate::Error::KeyDerivation`]。
    pub fn unlock_password(
        &mut self,
        label: &str,
        vault_salt: &[u8; 16],
        password: &str,
        params: Argon2Params,
    ) -> Result<()> {
        self.touch();
        let key = CacheKey {
            vault_salt: *vault_salt,
            kind: CredentialKind::Vault,
            label: label.to_owned(),
        };
        if self.cache.contains_key(&key) {
            return Ok(()); // 缓存命中，跳过慢速 KDF
        }
        let kek = Kek::from_password(password.as_bytes(), vault_salt, params)?;
        self.kdf_runs = self.kdf_runs.saturating_add(1);
        self.cache.insert(key, kek);
        Ok(())
    }

    /// 直接插入一个已有的 KEK。
    ///
    /// 用于生物识别（OS 密钥库解出）和恢复码（熵已足够，无需 KDF）两种来源。
    pub fn insert_kek(
        &mut self,
        label: &str,
        kind: CredentialKind,
        vault_salt: &[u8; 16],
        kek: Kek,
    ) {
        self.touch();
        let key = CacheKey { vault_salt: *vault_salt, kind, label: label.to_owned() };
        self.cache.insert(key, kek);
    }

    /// 移除某个凭据（其 KEK 会被清零）。
    ///
    /// 返回是否确实移除了条目。
    pub fn forget(&mut self, label: &str, kind: CredentialKind, vault_salt: &[u8; 16]) -> bool {
        let key = CacheKey { vault_salt: *vault_salt, kind, label: label.to_owned() };
        // Kek 实现了 ZeroizeOnDrop，移除即清零
        self.cache.remove(&key).is_some()
    }

    /// 列出适用于指定 `vault_salt` 且**参与扫描**的凭据。
    ///
    /// 扫描器用这个列表逐一尝试解包 slot。返回的 KEK 是副本，
    /// 调用方用完即析构清零。
    #[must_use]
    pub fn scannable_for(&self, vault_salt: &[u8; 16]) -> Vec<UnlockedCredential> {
        self.cache
            .iter()
            .filter(|(k, _)| &k.vault_salt == vault_salt && k.kind.scannable())
            .map(|(k, v)| UnlockedCredential {
                label: k.label.clone(),
                kind: k.kind,
                kek: v.duplicate(),
            })
            .collect()
    }

    /// 列出适用于指定 `vault_salt` 的**全部**凭据（含不参与扫描的）。
    ///
    /// 用于「手动打开单个文件」——此时可以接受 portable/recovery 的额外成本。
    #[must_use]
    pub fn all_for(&self, vault_salt: &[u8; 16]) -> Vec<UnlockedCredential> {
        self.cache
            .iter()
            .filter(|(k, _)| &k.vault_salt == vault_salt)
            .map(|(k, v)| UnlockedCredential {
                label: k.label.clone(),
                kind: k.kind,
                kek: v.duplicate(),
            })
            .collect()
    }

    /// 缓存中的凭据数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// 缓存是否为空（即处于锁定状态）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// 至今执行过的 Argon2 次数。
    ///
    /// 用于验证缓存生效：解锁 N 个不同凭据后该值应为 N，
    /// 无论调用了多少次 [`SessionKeys::unlock_password`]。
    #[must_use]
    pub const fn kdf_runs(&self) -> u64 {
        self.kdf_runs
    }

    /// 立即锁定：清零所有 KEK。
    ///
    /// 调用方还应同步关闭已打开的媒体流、清理 WebView 缓存（规范 §11.2）。
    pub fn lock(&mut self) {
        // HashMap::clear 会 drop 所有 Kek，触发 ZeroizeOnDrop
        self.cache.clear();
        // label 里虽无秘密，但可能含用户起的名字，一并清理
        self.cache.shrink_to_fit();
    }

    /// 刷新活动时间戳，推迟空闲锁定。
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// 距上次活动的时长。
    #[must_use]
    pub fn idle_for(&self) -> Duration {
        self.last_activity.elapsed()
    }

    /// 是否已达到空闲锁定阈值。
    #[must_use]
    pub fn should_auto_lock(&self) -> bool {
        !self.cache.is_empty() && self.idle_for() >= self.idle_timeout
    }

    /// 若已空闲超时则锁定，返回是否执行了锁定。
    ///
    /// 宿主应用应定期（如每 30 秒）调用此方法。
    pub fn auto_lock_if_idle(&mut self) -> bool {
        if self.should_auto_lock() {
            self.lock();
            true
        } else {
            false
        }
    }
}

impl Drop for SessionKeys {
    /// 会话析构时确保清零。
    fn drop(&mut self) {
        self.cache.clear();
    }
}

/// 从恢复码助记词还原 KEK。
///
/// 恢复码含 256 bit 随机熵，**无需 Argon2**——暴力破解 256 bit 不可行，
/// 慢速 KDF 只会白费时间（规范 §8.2）。
///
/// 这里做的是把词序列归一化后哈希成 32 字节。真正的 BIP-39 校验和验证
/// 由上层实现（需要词表）。
///
/// # Errors
///
/// 当前实现不会失败，返回 `Result` 是为了未来加入校验和验证时保持签名稳定。
pub fn kek_from_recovery_words(words: &[&str]) -> Result<Kek> {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(b"omy/v1/recovery");
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            h.update(b" ");
        }
        // 归一化：去空白 + 转小写，容忍用户手抄时的大小写差异
        h.update(w.trim().to_lowercase().as_bytes());
    }
    let mut out = h.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    out.zeroize();
    Ok(Kek::from_key(SecretKey::from_bytes(key)))
}
