//! 错误类型。
//!
//! 设计要求（见 `docs/research/08-ui-ux-design.md` §7.2）：错误必须是**结构化的错误码
//! 加参数**，而不是拼装好的自然语言字符串。这样 GUI 层才能按用户语言渲染，CLI 层才能
//! 映射到稳定的退出码。
//!
//! 因此本模块的 `Display` 实现只用于开发者调试与日志，**不应**直接展示给终端用户。

use core::fmt;

/// CLI 退出码。数值定义见 `docs/research/09-cli-design.md` §3.1，
/// 脚本依赖这些数值区分「密码错」与「文件坏」，**不得随意改动**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// 成功。
    Success = 0,
    /// 一般错误。
    General = 1,
    /// 参数错误。
    Usage = 2,
    /// 密码错误或无匹配 slot。
    WrongPassword = 3,
    /// 文件损坏或被篡改（MAC / AEAD 校验失败）。
    Corrupted = 4,
    /// 格式版本不支持。
    UnsupportedVersion = 5,
    /// 缺少分片。
    MissingShard = 6,
    /// 权限不足。
    Permission = 7,
    /// 用户取消。
    Cancelled = 8,
}

/// omy 核心库的错误类型。
///
/// 每个变体都携带定位问题所需的结构化字段，供上层渲染本地化消息。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// 文件头 magic 不匹配，不是 omy 文件。
    #[error("not an omy file: magic mismatch (got {got:02x?})")]
    BadMagic {
        /// 实际读到的前 8 字节。
        got: [u8; 8],
    },

    /// 主版本号高于本实现支持的上限，必须拒绝打开（规范 §9 步骤 2）。
    #[error("unsupported format version {found_major}.{found_minor}, this build supports up to {supported_major}.x")]
    UnsupportedVersion {
        /// 文件声明的主版本。
        found_major: u16,
        /// 文件声明的次版本。
        found_minor: u16,
        /// 本实现支持的最高主版本。
        supported_major: u16,
    },

    /// 输入数据长度不足，无法解析出完整结构。
    #[error("truncated input while reading {context}: need {need} bytes, got {got}")]
    Truncated {
        /// 正在解析的结构名，便于定位。
        context: &'static str,
        /// 需要的字节数。
        need: usize,
        /// 实际可用的字节数。
        got: usize,
    },

    /// 头部字段取值非法或自相矛盾。
    #[error("malformed header: {reason}")]
    MalformedHeader {
        /// 具体原因。
        reason: &'static str,
    },

    /// 没有任何候选密钥能解开任何一个 key slot。
    ///
    /// 注意：这**不代表**文件损坏，也可能只是该文件不属于当前解锁的密码。
    /// 面向用户的文案必须同时表达这两种可能。
    #[error("no key slot could be unwrapped with the provided credentials")]
    NoMatchingSlot,

    /// 头部 HMAC 校验失败：文件被篡改，或 FEK 不正确。
    ///
    /// 规范 §7 要求此时**必须终止**，不得继续读取载荷。
    #[error("header MAC verification failed: file tampered or truncated")]
    HeaderMacMismatch,

    /// 载荷块的 AEAD 认证失败。
    #[error("chunk {index} failed authentication: data corrupted or tampered")]
    ChunkAuthFailed {
        /// 出错的块序号。
        index: u64,
    },

    /// 遇到未知且标记为 CRITICAL 的 TLV，按规范必须拒绝打开（§4.2）。
    #[error("unknown critical TLV type 0x{tlv_type:04x}: cannot safely open this file")]
    UnknownCriticalTlv {
        /// 未知的 TLV 类型号。
        tlv_type: u16,
    },

    /// 必需的 TLV 缺失。
    #[error("required TLV type 0x{tlv_type:04x} is missing")]
    MissingTlv {
        /// 缺失的 TLV 类型号。
        tlv_type: u16,
    },

    /// TLV 内容解析失败。
    #[error("malformed TLV type 0x{tlv_type:04x}: {reason}")]
    MalformedTlv {
        /// 出错的 TLV 类型号。
        tlv_type: u16,
        /// 具体原因。
        reason: &'static str,
    },

    /// 请求的块序号超出文件实际块数。
    #[error("chunk index {index} out of range (file has {total} chunks)")]
    ChunkOutOfRange {
        /// 请求的块序号。
        index: u64,
        /// 实际总块数。
        total: u64,
    },

    /// key slot 数量超过格式上限。
    #[error("too many key slots: {got} exceeds maximum {max}")]
    TooManySlots {
        /// 请求的数量。
        got: usize,
        /// 格式允许的上限。
        max: usize,
    },

    /// 分块大小超出允许范围。
    #[error("chunk size {got} out of allowed range [{min}, {max}]")]
    InvalidChunkSize {
        /// 请求的分块大小。
        got: u32,
        /// 下限。
        min: u32,
        /// 上限。
        max: u32,
    },

    /// 分片的 CRC-32 校验失败。
    #[error("shard {index} CRC mismatch: expected {expected:08x}, computed {computed:08x}")]
    ShardCrcMismatch {
        /// 分片序号。
        index: u32,
        /// 头部记录的 CRC。
        expected: u32,
        /// 实际算出的 CRC。
        computed: u32,
    },

    /// 分片集合中缺少某些序号。
    #[error("missing shards: {missing:?} (expected {total} total)")]
    MissingShards {
        /// 缺失的分片序号列表。
        missing: Vec<u32>,
        /// 应有的总片数。
        total: u32,
    },

    /// 分片的 `file_uuid` 与其它片不一致，属于不同文件。
    #[error("shard {index} belongs to a different file")]
    ShardUuidMismatch {
        /// 不一致的分片序号。
        index: u32,
    },

    /// 解密后内容哈希与 `TLV_CONTENT_HASH` 记录不符。
    #[error("content hash mismatch after decryption")]
    ContentHashMismatch,

    /// 压缩或解压失败。
    #[error("compression error: {reason}")]
    Compression {
        /// 具体原因。
        reason: String,
    },

    /// 密钥派生失败。
    #[error("key derivation failed: {reason}")]
    KeyDerivation {
        /// 具体原因。
        reason: &'static str,
    },

    /// 底层 IO 错误。
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// 映射到 CLI 退出码。
    ///
    /// 见 `docs/research/09-cli-design.md` §3.1。
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::NoMatchingSlot => ExitCode::WrongPassword,
            Self::BadMagic { .. }
            | Self::HeaderMacMismatch
            | Self::ChunkAuthFailed { .. }
            | Self::ShardCrcMismatch { .. }
            | Self::ContentHashMismatch
            | Self::Truncated { .. }
            | Self::MalformedHeader { .. }
            | Self::MalformedTlv { .. } => ExitCode::Corrupted,
            Self::UnsupportedVersion { .. } | Self::UnknownCriticalTlv { .. } => {
                ExitCode::UnsupportedVersion
            }
            Self::MissingShards { .. } | Self::ShardUuidMismatch { .. } => ExitCode::MissingShard,
            Self::InvalidChunkSize { .. } | Self::TooManySlots { .. } => ExitCode::Usage,
            Self::Io(_) => ExitCode::Permission,
            _ => ExitCode::General,
        }
    }

    /// 稳定的机器可读错误码，供 `--json` 输出与前端 i18n 查表使用。
    ///
    /// 这些字符串是**接口契约**，脚本会匹配它们，因此不得随意更名。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::BadMagic { .. } => "BAD_MAGIC",
            Self::UnsupportedVersion { .. } => "UNSUPPORTED_VERSION",
            Self::Truncated { .. } => "TRUNCATED",
            Self::MalformedHeader { .. } => "MALFORMED_HEADER",
            Self::NoMatchingSlot => "WRONG_PASSWORD",
            Self::HeaderMacMismatch => "HEADER_MAC_MISMATCH",
            Self::ChunkAuthFailed { .. } => "CHUNK_AUTH_FAILED",
            Self::UnknownCriticalTlv { .. } => "UNKNOWN_CRITICAL_TLV",
            Self::MissingTlv { .. } => "MISSING_TLV",
            Self::MalformedTlv { .. } => "MALFORMED_TLV",
            Self::ChunkOutOfRange { .. } => "CHUNK_OUT_OF_RANGE",
            Self::TooManySlots { .. } => "TOO_MANY_SLOTS",
            Self::InvalidChunkSize { .. } => "INVALID_CHUNK_SIZE",
            Self::ShardCrcMismatch { .. } => "SHARD_CRC_MISMATCH",
            Self::MissingShards { .. } => "MISSING_SHARDS",
            Self::ShardUuidMismatch { .. } => "SHARD_UUID_MISMATCH",
            Self::ContentHashMismatch => "CONTENT_HASH_MISMATCH",
            Self::Compression { .. } => "COMPRESSION_ERROR",
            Self::KeyDerivation { .. } => "KEY_DERIVATION_FAILED",
            Self::Io(_) => "IO_ERROR",
        }
    }
}

impl fmt::Display for ExitCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", *self as u8)
    }
}

/// 本库统一的 `Result` 别名。
pub type Result<T> = core::result::Result<T, Error>;
