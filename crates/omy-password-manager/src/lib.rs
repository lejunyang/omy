//! 第三方密码管理器桥接。
//!
//! 这一层只负责“经用户授权取得或保存一段秘密”，不理解 `.omy` 文件、
//! `vault_salt` 或 key slot。调用方决定把秘密用于密码 KDF、Passkey PRF，还是
//! 将来的其它凭据类型。
//!
//! 首个 provider 是 [`keepassxc`]。Android Credential Manager 由应用壳层调用
//! 原生 API，但使用本 crate 的同一套领域类型，避免把 KeePassXC 写死进 GUI。

#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

use rand_core::RngCore as _;
use zeroize::Zeroizing;

pub mod keepassxc;

/// 生成的同步密钥字节数。
pub const GENERATED_SECRET_LEN: usize = 32;
/// 可识别的同步密钥前缀。
pub const GENERATED_SECRET_PREFIX: &str = "omy1_";

/// 一段只应短暂存在于内存中的凭据秘密。
///
/// 不实现 `Clone`，复制必须在调用点显式可见；`Debug` 永远不显示内容。
pub struct CredentialSecret(Zeroizing<String>);

impl CredentialSecret {
    /// 收进一段外部 provider 返回的秘密。
    #[must_use]
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// 以字符串借用秘密，不产生第二份副本。
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// 以字节借用秘密，供 KDF 使用。
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl core::fmt::Debug for CredentialSecret {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("CredentialSecret(<redacted>)")
    }
}

/// 生成一把适合交给密码管理器同步的 256 bit 随机秘密。
#[must_use]
pub fn generate_sync_secret() -> CredentialSecret {
    use base64::Engine as _;

    let mut raw = zeroize::Zeroizing::new([0u8; GENERATED_SECRET_LEN]);
    rand_core::OsRng.fill_bytes(raw.as_mut());
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_ref());
    CredentialSecret::new(format!("{GENERATED_SECRET_PREFIX}{encoded}"))
}

/// 密码管理器中的一个凭据。
pub struct Credential {
    /// provider 内部的稳定条目标识。
    pub id: String,
    /// 给用户看的名称；没有标题时退回登录名。
    pub name: String,
    /// 登录名。omy 用它保存用户给同步密钥起的名字。
    pub login: String,
    /// 所属分组。
    pub group: String,
    /// 真正的秘密。
    pub secret: CredentialSecret,
}

impl core::fmt::Debug for Credential {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("login", &self.login)
            .field("group", &self.group)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// 不含秘密、可以安全发给界面的凭据摘要。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CredentialSummary {
    /// provider 内部条目标识。
    pub id: String,
    /// 显示名称。
    pub name: String,
    /// 用户给密钥起的名字。
    pub login: String,
    /// 所属分组。
    pub group: String,
}

impl From<&Credential> for CredentialSummary {
    fn from(value: &Credential) -> Self {
        Self {
            id: value.id.clone(),
            name: value.name.clone(),
            login: value.login.clone(),
            group: value.group.clone(),
        }
    }
}

/// 密码管理器 provider 的稳定标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// KeePassXC-Browser 本地协议。
    KeePassXc,
    /// Android Credential Manager；具体提供者可能是 KeePassDX、Google 等。
    AndroidCredentialManager,
}

/// provider 可以完成的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capabilities {
    /// 能选择已有秘密。
    pub select: bool,
    /// 能保存新秘密。
    pub create: bool,
    /// 能更新已有秘密。
    pub update: bool,
    /// 能返回 WebAuthn PRF。
    pub passkey_prf: bool,
}

/// 一个已建立用户授权的 provider 会话。
pub trait PasswordManager {
    /// provider 类型。
    fn kind(&self) -> ProviderKind;

    /// 当前实现真正支持的能力。
    fn capabilities(&self) -> Capabilities;

    /// 查询指定命名空间下的凭据。
    ///
    /// # Errors
    ///
    /// provider 不可用、锁定、授权失效或协议错误时返回。
    fn list(&mut self, namespace: &str) -> Result<Vec<Credential>>;

    /// 保存新凭据，返回 provider 分配的条目标识（若协议提供）。
    ///
    /// # Errors
    ///
    /// 用户取消、数据库只读或写入失败时返回。
    fn create(
        &mut self,
        namespace: &str,
        label: &str,
        secret: &CredentialSecret,
    ) -> Result<Option<String>>;
}

/// 密码管理器操作失败。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 没找到 provider 或它未运行。
    #[error("密码管理器不可用：{0}")]
    Unavailable(String),
    /// provider 的数据库尚未解锁。
    #[error("密码管理器数据库未解锁")]
    Locked,
    /// 尚未由用户授权关联。
    #[error("密码管理器尚未关联")]
    NotAssociated,
    /// 用户取消或拒绝。
    #[error("用户取消了密码管理器操作")]
    UserCancelled,
    /// 没有匹配凭据。
    #[error("密码管理器中没有匹配凭据")]
    NotFound,
    /// provider 版本或能力不满足要求。
    #[error("密码管理器不支持此操作：{0}")]
    Unsupported(String),
    /// 传输失败。
    #[error("密码管理器通信失败：{0}")]
    Transport(String),
    /// 对端消息无效、被篡改或与请求不匹配。
    #[error("密码管理器返回了无效消息：{0}")]
    Protocol(String),
    /// provider 明确返回失败。
    #[error("密码管理器操作失败（代码 {code}）：{message}")]
    Provider {
        /// provider 错误码。
        code: i32,
        /// provider 错误说明。
        message: String,
    },
}

/// 本 crate 的结果类型。
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secret_has_version_and_full_entropy_length() {
        let secret = generate_sync_secret();
        let encoded = secret
            .expose()
            .strip_prefix(GENERATED_SECRET_PREFIX)
            .expect("必须带版本前缀");
        // 32 字节的无 padding base64url 必须是 43 字符。不这样会怎样：
        // 少生成一个字节也可能肉眼看不出，实际却降低所有文件的强度下限。
        assert_eq!(encoded.len(), 43, "256 bit 的 base64url 长度应为 43");
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "生成结果必须能安全放进密码字段"
        );
    }

    #[test]
    fn two_generated_secrets_are_different() {
        let a = generate_sync_secret();
        let b = generate_sync_secret();
        // 不这样会怎样：RNG 未真正参与时，所有安装会共享一把所谓“随机”密钥。
        assert_ne!(a.expose(), b.expose(), "两次生成不应得到同一秘密");
    }

    #[test]
    fn debug_never_prints_secret() {
        let secret = CredentialSecret::new(String::from("must-not-leak"));
        let rendered = format!("{secret:?}");
        // 不这样会怎样：错误日志打印 Credential 时会把所有文件的钥匙一起带走。
        assert!(!rendered.contains("must-not-leak"), "Debug 泄露了秘密");
        assert!(rendered.contains("redacted"), "脱敏输出应可被审计识别");
    }
}
