//! 密码学层：两级 KDF、AEAD 抽象、密钥材料的安全擦除。
//!
//! # 两级 KDF 是可行性前提，不是优化
//!
//! 规范要求「给定 N 个密码，快速判断某文件是否匹配」。实测（见验证报告 §3.2）：
//!
//! | 操作 | 耗时 |
//! |---|---|
//! | Argon2id (m=64MiB, t=3) | ~180 ms |
//! | HKDF-SHA256 | ~13 µs |
//! | 比值 | ~13,700× |
//!
//! 若每个文件都跑一次 Argon2，扫描 10,000 个文件需要约 2.5 小时；两级 KDF 下约 3.4 秒。
//! 所以 `Kek` 必须在**会话内缓存**，绝不能每文件重新派生。
//!
//! # 域分隔
//!
//! 四种用途的子密钥使用互不相同的 HKDF `info`，确保任一密钥泄露不影响其它用途：
//!
//! - slot 包裹密钥：`"omy/v1/slot" || u16le(slot_index)`，salt = `file_uuid`
//! - 载荷密钥：`"omy/v1/payload"`，salt = `file_uuid`
//! - 头部 MAC 密钥：`"omy/v1/header-mac"`，salt = `file_uuid`
//! - TLV 密钥：`"omy/v1/tlv" || u16le(tlv_type)`，salt = 空

use crate::error::{Error, Result};
use aes_gcm::Aes256Gcm;
use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// HKDF info 前缀：slot 包裹密钥。
const INFO_SLOT: &[u8] = b"omy/v1/slot";
/// HKDF info：载荷密钥。
const INFO_PAYLOAD: &[u8] = b"omy/v1/payload";
/// HKDF info：头部 MAC 密钥。
const INFO_HEADER_MAC: &[u8] = b"omy/v1/header-mac";
/// HKDF info 前缀：TLV 密钥。
const INFO_TLV: &[u8] = b"omy/v1/tlv";
/// HKDF info：会话内区分不同 KEK 的指纹。
///
/// 与上面四个的关键区别：**它派生出的不是密钥，而是一个允许比较的标识**。
/// 单独一个 info 串是为了让它与任何真实子密钥落在不同的域里——万一指纹
/// 泄露（它本就是用来在内存里传递比较的），也推不出任何解密用的密钥。
const INFO_FINGERPRINT: &[u8] = b"omy/v1/kek-fingerprint";

/// slot info 缓冲区长度：前缀 + u16 序号。
const SLOT_INFO_LEN: usize = INFO_SLOT.len() + 2;
/// TLV info 缓冲区长度：前缀 + u16 类型号。
const TLV_INFO_LEN: usize = INFO_TLV.len() + 2;

// 这些长度由 info 串推导而来。若 info 串变更，缓冲区自动跟随，
// 不会出现硬编码长度与实际前缀不符导致的静默错误。
const _: () = assert!(SLOT_INFO_LEN == 13, "omy/v1/slot 长度应为 11");
const _: () = assert!(TLV_INFO_LEN == 12, "omy/v1/tlv 长度应为 10");

/// 对称密钥长度（字节）。
pub const KEY_LEN: usize = 32;
/// AEAD 认证标签长度（字节）。
pub const TAG_LEN: usize = 16;
/// 参考实现使用的 nonce 长度（ChaCha20-Poly1305 / AES-GCM 均为 12）。
pub const NONCE_LEN: usize = 12;

/// 32 字节密钥材料，析构时自动擦除。
///
/// 刻意不实现 `Copy`/`Clone`，避免密钥在栈上留下无法追踪的副本；确实需要复制时用
/// [`SecretKey::duplicate`]，让复制行为在代码中显式可见。
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SecretKey([u8; KEY_LEN]);

impl SecretKey {
    /// 由原始字节构造。
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// 借出底层字节。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// 显式复制。命名刻意不叫 `clone`，以便审计时能搜出所有密钥复制点。
    #[must_use]
    pub fn duplicate(&self) -> Self {
        Self(self.0)
    }

    /// 生成随机密钥。
    #[must_use]
    pub fn random() -> Self {
        let mut k = [0u8; KEY_LEN];
        crate::util::fill_random(&mut k);
        Self(k)
    }
}

impl core::fmt::Debug for SecretKey {
    /// 刻意不打印密钥内容，避免日志泄露。
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// 库主密钥（Key Encryption Key）。
///
/// 由 `Argon2id(密码, vault_salt)` 派生，**每密码每会话只应派生一次**，
/// 之后用 [`Kek::derive_slot_key`] 等方法为每个文件微秒级派生子密钥。
#[derive(Debug)]
pub struct Kek(SecretKey);

impl Kek {
    /// 由已有的 32 字节密钥材料构造（如从 OS 密钥库取出的设备密钥、或恢复码）。
    #[must_use]
    pub const fn from_key(key: SecretKey) -> Self {
        Self(key)
    }

    /// 借出底层密钥。
    #[must_use]
    pub const fn as_key(&self) -> &SecretKey {
        &self.0
    }

    /// 显式复制。
    ///
    /// 命名与 [`SecretKey::duplicate`] 一致，刻意不实现 `Clone`——这样审计时
    /// 能搜出所有 KEK 复制点。扫描器需要它把会话缓存中的 KEK 传给解包函数。
    #[must_use]
    pub fn duplicate(&self) -> Self {
        Self(self.0.duplicate())
    }

    /// 用 Argon2id 从密码派生 KEK。
    ///
    /// # 开销
    ///
    /// 这是**故意慢**的操作（默认参数约 180 ms）。调用方必须缓存结果，
    /// 不得在循环中对每个文件重复调用。
    ///
    /// # Errors
    ///
    /// Argon2 参数非法（如内存成本过小）时返回 [`Error::KeyDerivation`]。
    pub fn from_password(password: &[u8], vault_salt: &[u8], params: Argon2Params) -> Result<Self> {
        use argon2::{Algorithm, Argon2, Version};

        let p = argon2::Params::new(params.m_kib, params.t, params.p, Some(KEY_LEN))
            .map_err(|_| Error::KeyDerivation { reason: "invalid Argon2 parameters" })?;
        let a2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, p);

        let mut out = [0u8; KEY_LEN];
        a2.hash_password_into(password, vault_salt, &mut out)
            .map_err(|_| Error::KeyDerivation { reason: "Argon2 hashing failed" })?;
        Ok(Self(SecretKey::from_bytes(out)))
    }

    /// 派生 vault 级的目录名加密密钥（树形模式，文档 05 §3.3）。
    ///
    /// `salt = vault_salt`，`info` 由调用方给出（见
    /// [`crate::dirname::DirnameKey::derive`]）。
    ///
    /// # 为什么 salt 是 vault_salt 而不是 file_uuid
    ///
    /// 目录名不属于任何单个文件，没有 file_uuid 可用。用 vault_salt 的
    /// 副作用是**目录结构的可见性绑定整个 vault**：能解开 vault 的任一
    /// 密码都能看到完整目录树，无法按 slot 区分权限。这是树形模式的固有
    /// 约束，UI 必须告知用户。
    #[must_use]
    pub fn derive_dirname_key(&self, vault_salt: &[u8; 16], info: &[u8]) -> SecretKey {
        hkdf_expand(self.0.as_bytes(), vault_salt, info)
    }

    /// 派生第 `slot_index` 个 key slot 的包裹密钥。
    ///
    /// `info = "omy/v1/slot" || u16le(slot_index)`，`salt = file_uuid`
    #[must_use]
    pub fn derive_slot_key(&self, file_uuid: &[u8; 16], slot_index: u16) -> SecretKey {
        let mut info = [0u8; SLOT_INFO_LEN];
        let (prefix, suffix) = info.split_at_mut(INFO_SLOT.len());
        prefix.copy_from_slice(INFO_SLOT);
        suffix.copy_from_slice(&slot_index.to_le_bytes());
        hkdf_expand(self.0.as_bytes(), file_uuid, &info)
    }

    /// 派生该 KEK 的会话内指纹，用于判断两个 KEK 是否相同。
    ///
    /// # 为什么需要它
    ///
    /// 会话要同时装多个密码，就必须回答「刚输的这个，是不是已经装过的那个」。
    /// 直接比较 KEK 字节是可以的，但那要求把密钥本身搬来搬去做相等比较；
    /// 而按名字比较又不成立——同一个密码可以被起两个名字，不同密码也可以
    /// 重名（上一轮「`label` 让同一个密码被算成两条凭据」正是后者）。
    ///
    /// # 为什么不能落盘
    ///
    /// 指纹是**密码的稳定标识**：同一密码在同一 vault 下恒为同一值。写进
    /// 磁盘就等于给攻击者一个离线可比对的口令哈希——他能据此判断两个库是不是
    /// 同一个密码、能拿字典逐个比对候选密码，而 Argon2 那几百毫秒的代价
    /// 在这里完全不起作用（KEK 已经是派生结果，指纹只是一次 HKDF）。
    ///
    /// 所以它**只允许在内存里存在**，不得写入任何文件、日志或 IPC 响应。
    ///
    /// # 为什么 salt 是 vault_salt
    ///
    /// 让指纹天然绑定 vault。同一密码在不同 vault 下 KEK 本就不同，指纹也就
    /// 不同——这与缓存键必须含 `vault_salt` 是同一个道理，避免跨库误判成
    /// 「已经装过了」。
    #[must_use]
    pub fn fingerprint(&self, vault_salt: &[u8; 16]) -> [u8; KEY_LEN] {
        let k = hkdf_expand(self.0.as_bytes(), vault_salt, INFO_FINGERPRINT);
        *k.as_bytes()
    }
}

/// 文件加密密钥（File Encryption Key）。
///
/// 每个文件独立随机生成，被各个 key slot 包裹。所有文件级子密钥都由它派生。
#[derive(Debug)]
pub struct Fek(SecretKey);

impl Fek {
    /// 生成随机 FEK。
    #[must_use]
    pub fn random() -> Self {
        Self(SecretKey::random())
    }

    /// 由原始字节构造。
    #[must_use]
    pub const fn from_key(key: SecretKey) -> Self {
        Self(key)
    }

    /// 借出底层密钥。
    #[must_use]
    pub const fn as_key(&self) -> &SecretKey {
        &self.0
    }

    /// 派生载荷加密密钥。
    #[must_use]
    pub fn derive_payload_key(&self, file_uuid: &[u8; 16]) -> SecretKey {
        hkdf_expand(self.0.as_bytes(), file_uuid, INFO_PAYLOAD)
    }

    /// 派生头部 MAC 密钥。
    #[must_use]
    pub fn derive_header_mac_key(&self, file_uuid: &[u8; 16]) -> SecretKey {
        hkdf_expand(self.0.as_bytes(), file_uuid, INFO_HEADER_MAC)
    }

    /// 派生某个 TLV 类型的加密密钥。
    ///
    /// 注意 `salt` 为**空**（规范 §4.3），与其它派生不同。
    #[must_use]
    pub fn derive_tlv_key(&self, tlv_type: u16) -> SecretKey {
        let mut info = [0u8; TLV_INFO_LEN];
        let (prefix, suffix) = info.split_at_mut(INFO_TLV.len());
        prefix.copy_from_slice(INFO_TLV);
        suffix.copy_from_slice(&tlv_type.to_le_bytes());
        hkdf_expand(self.0.as_bytes(), &[], &info)
    }
}

/// HKDF-SHA256 提取加扩展，输出 32 字节。
fn hkdf_expand(ikm: &[u8], salt: &[u8], info: &[u8]) -> SecretKey {
    let hk = Hkdf::<Sha256>::new(Some(salt), ikm);
    let mut out = [0u8; KEY_LEN];
    // 32 字节远小于 SHA-256 的 255*32 上限，不可能失败
    hk.expand(info, &mut out)
        .unwrap_or_else(|_| unreachable!("HKDF output length 32 is always valid"));
    SecretKey::from_bytes(out)
}

/// Argon2id 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argon2Params {
    /// 内存成本（KiB）。
    pub m_kib: u32,
    /// 迭代次数。
    pub t: u32,
    /// 并行度。
    pub p: u32,
}

impl Argon2Params {
    /// 移动端档位：m=32 MiB, t=4。
    pub const MOBILE: Self = Self { m_kib: 32 * 1024, t: 4, p: 1 };
    /// 交互档位（默认）：m=64 MiB, t=3。约 180 ms。
    pub const INTERACTIVE: Self = Self { m_kib: 64 * 1024, t: 3, p: 1 };
    /// 中等档位：m=256 MiB, t=4。
    pub const MODERATE: Self = Self { m_kib: 256 * 1024, t: 4, p: 1 };
    /// 高强度档位：m=1 GiB, t=4。移动端可能无法完成。
    pub const SENSITIVE: Self = Self { m_kib: 1024 * 1024, t: 4, p: 1 };

    /// 测试向量使用的弱参数。
    ///
    /// # 警告
    ///
    /// 仅用于复现测试向量，**生产环境禁用**。
    pub const TEST_WEAK: Self = Self { m_kib: 64, t: 1, p: 1 };
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::INTERACTIVE
    }
}

/// AEAD 算法选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CipherId {
    /// ChaCha20-Poly1305（RFC 8439），12 字节 nonce。
    ///
    /// 参考实现与测试向量使用此算法。生产默认为 XChaCha20-Poly1305（24 字节 nonce），
    /// 目前尚未实现，见 `docs/research/02-file-format-spec.md` §5.2 的说明。
    ChaCha20Poly1305 = 1,
    /// AES-256-GCM。有 AES-NI 硬件加速时更快。
    Aes256Gcm = 2,
}

impl CipherId {
    /// 由 header 中的数值解析。
    ///
    /// # Errors
    ///
    /// 未知取值时返回 [`Error::MalformedHeader`]。
    pub const fn from_u8(v: u8) -> Result<Self> {
        match v {
            1 => Ok(Self::ChaCha20Poly1305),
            2 => Ok(Self::Aes256Gcm),
            _ => Err(Error::MalformedHeader { reason: "unknown cipher_id" }),
        }
    }

    /// AEAD 加密。输出为 `密文 || tag`。
    ///
    /// # Errors
    ///
    /// 底层 AEAD 失败时返回错误（正常输入下不会发生）。
    pub fn encrypt(
        self,
        key: &SecretKey,
        nonce: &[u8; NONCE_LEN],
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>> {
        let payload = Payload { msg: plaintext, aad };
        match self {
            Self::ChaCha20Poly1305 => {
                let c = ChaCha20Poly1305::new(key.as_bytes().into());
                c.encrypt(nonce.into(), payload)
            }
            Self::Aes256Gcm => {
                let c = Aes256Gcm::new(key.as_bytes().into());
                c.encrypt(nonce.into(), payload)
            }
        }
        .map_err(|_| Error::KeyDerivation { reason: "AEAD encryption failed" })
    }

    /// AEAD 解密。输入为 `密文 || tag`。
    ///
    /// 认证失败时返回 `Ok(None)`，让调用方区分「密码不对」和「真的坏了」——
    /// 这在扫描场景下很关键：试错密码是正常流程，不该当作错误处理。
    ///
    /// # Errors
    ///
    /// 目前不返回 `Err`，签名保留以便未来扩展。
    pub fn decrypt(
        self,
        key: &SecretKey,
        nonce: &[u8; NONCE_LEN],
        ciphertext: &[u8],
        aad: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let payload = Payload { msg: ciphertext, aad };
        let r = match self {
            Self::ChaCha20Poly1305 => {
                let c = ChaCha20Poly1305::new(key.as_bytes().into());
                c.decrypt(nonce.into(), payload)
            }
            Self::Aes256Gcm => {
                let c = Aes256Gcm::new(key.as_bytes().into());
                c.decrypt(nonce.into(), payload)
            }
        };
        Ok(r.ok())
    }
}

/// 构造载荷块的 nonce。
///
/// ```text
/// nonce(i) = base_nonce(7B) || u32be(i) || final_flag(1B)
/// ```
///
/// `u32be(i)` 是**全格式唯一使用大端序**的地方，遵循 STREAM 构造惯例。
/// `final_flag` 用于防截断攻击：删掉尾部块后，新的「最后一块」flag 不匹配，解密必然失败。
#[must_use]
pub fn chunk_nonce(base_nonce: &[u8; 7], index: u32, is_final: bool) -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    n[..7].copy_from_slice(base_nonce);
    n[7..11].copy_from_slice(&index.to_be_bytes());
    n[11] = u8::from(is_final);
    n
}

/// 构造载荷块的 AAD。
///
/// ```text
/// aad(i) = file_uuid(16B) || u32be(i)
/// ```
///
/// 绑定文件身份防跨文件块移植，绑定块序号防块重排。
#[must_use]
pub fn chunk_aad(file_uuid: &[u8; 16], index: u32) -> [u8; 20] {
    let mut a = [0u8; 20];
    a[..16].copy_from_slice(file_uuid);
    a[16..].copy_from_slice(&index.to_be_bytes());
    a
}

/// key slot 与 TLV 使用的全零 nonce。
///
/// 安全性依据：每个 slot 的包裹密钥由 `(KEK, file_uuid, slot_index)` 唯一确定，
/// 每个 TLV 密钥由 `(FEK, tlv_type)` 唯一确定，因此同一密钥永不重复使用，
/// 零 nonce 不会导致 nonce 复用。
pub const ZERO_NONCE: [u8; NONCE_LEN] = [0u8; NONCE_LEN];

/// 计算头部 HMAC-SHA256。
///
/// 覆盖 `fixed_header || slot_area || tlv_blob`，即除 MAC 自身外的全部头部内容。
#[must_use]
pub fn header_mac(mac_key: &SecretKey, covered: &[u8]) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(mac_key.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC accepts any key length"));
    m.update(covered);
    m.finalize().into_bytes().into()
}

/// 以常量时间验证头部 MAC。
///
/// 使用常量时间比较避免时序侧信道泄露 MAC 的正确前缀长度。
#[must_use]
pub fn verify_header_mac(mac_key: &SecretKey, covered: &[u8], expected: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    header_mac(mac_key, covered).ct_eq(expected).into()
}

/// 计算 BLAKE2b-256 内容哈希。
#[must_use]
pub fn content_hash(data: &[u8]) -> [u8; 32] {
    use blake2::digest::consts::U32;
    use blake2::{Blake2b, Digest};
    let mut h = Blake2b::<U32>::new();
    h.update(data);
    h.finalize().into()
}
