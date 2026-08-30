//! omy-net 的错误类型。

use thiserror::Error;

/// omy-net 的结果类型。
pub type Result<T> = std::result::Result<T, NetError>;

/// 局域网共享过程中可能出现的错误。
///
/// 每个变体都对应一个稳定的机器可读 code，供 CLI 的 `--json` 输出与
/// GUI 的错误提示使用。**不要**依赖 `Display` 的文案做判断——那是给人看的，
/// 会随 i18n 变化。
#[derive(Debug, Error)]
pub enum NetError {
    /// 消息格式非法：长度不符、未知标签或有多余尾部字节。
    #[error("消息格式非法")]
    MalformedFrame,

    /// 消息超过单帧上限。
    #[error("消息超过单帧上限")]
    FrameTooLarge,

    /// 底层 I/O 失败。
    #[error("网络 I/O 失败: {0}")]
    Io(#[from] std::io::Error),

    /// Noise 握手或传输失败。
    #[error("加密信道失败: {0}")]
    Noise(String),

    /// 配对失败。
    ///
    /// 注意 SPAKE2 本身在 PIN 不同时**不报错**（spike 实测），
    /// 双方只是得到不同的密钥。因此这个错误来自后续的**密钥确认**环节。
    #[error("配对失败：PIN 不匹配或对方已取消")]
    PairingFailed,

    /// 对方声称的协议版本不受支持。
    #[error("协议版本不兼容：对方 {theirs}，本机支持 {ours}")]
    VersionMismatch {
        /// 对方声称的协议版本。
        theirs: u16,
        /// 本机支持的协议版本。
        ours: u16,
    },

    /// 服务端返回了错误。
    #[error("对方返回错误 [{code}]: {msg}")]
    Remote {
        /// 对方返回的稳定错误码。
        code: &'static str,
        /// 对方返回的描述文本。
        msg: String,
    },

    /// 操作超时。
    #[error("操作超时")]
    Timeout,

    /// 设备发现失败。
    #[error("设备发现失败: {0}")]
    Discovery(String),

    /// 会话已失效（被吊销或已过期）。
    #[error("会话已失效，需要重新配对")]
    SessionInvalid,
}

impl NetError {
    /// 稳定的机器可读错误码。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::MalformedFrame => "MALFORMED_FRAME",
            Self::FrameTooLarge => "FRAME_TOO_LARGE",
            Self::Io(_) => "IO_ERROR",
            Self::Noise(_) => "NOISE_ERROR",
            Self::PairingFailed => "PAIRING_FAILED",
            Self::VersionMismatch { .. } => "VERSION_MISMATCH",
            Self::Remote { code, .. } => code,
            Self::Timeout => "TIMEOUT",
            Self::Discovery(_) => "DISCOVERY_FAILED",
            Self::SessionInvalid => "SESSION_INVALID",
        }
    }
}

impl From<snow::Error> for NetError {
    fn from(e: snow::Error) -> Self {
        Self::Noise(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_non_empty_and_uppercase() {
        let errs = vec![
            NetError::MalformedFrame,
            NetError::FrameTooLarge,
            NetError::PairingFailed,
            NetError::Timeout,
            NetError::SessionInvalid,
            NetError::VersionMismatch { theirs: 2, ours: 1 },
            NetError::Noise("x".into()),
            NetError::Discovery("x".into()),
        ];
        for e in errs {
            let c = e.code();
            assert!(!c.is_empty(), "错误码不能为空");
            assert!(
                c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'),
                "错误码应为大写常量风格，实际 {c}"
            );
        }
    }

    #[test]
    fn remote_error_preserves_code() {
        let e = NetError::Remote { code: "NO_SUCH_HANDLE", msg: "x".into() };
        assert_eq!(e.code(), "NO_SUCH_HANDLE", "远端错误码应原样透传");
    }
}
