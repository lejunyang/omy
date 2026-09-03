//! # omy-core
//!
//! omy 加密文件格式的核心实现：格式读写、密钥体系、分块 AEAD、压缩、分片。
//!
//! 本 crate **不依赖 FFmpeg**，许可为 `MIT OR Apache-2.0`，可被任意项目复用。
//! 媒体解码相关能力位于独立的 crate 中，以隔离 GPL 传染。
//!
//! ## 权威规范
//!
//! 一切实现细节以 `docs/research/02-file-format-spec.md` 为准。本 crate 的
//! 每个模块都在文档注释中引用对应章节。
//!
//! ## 文件结构
//!
//! ```text
//! ┌────────────────────────┐ 0
//! │ Fixed Header    96 B   │
//! ├────────────────────────┤ 96
//! │ Key Slot Area  384 B   │  8 槽 × 48 B
//! ├────────────────────────┤ 480
//! │ TLV Area    tlv_len B  │  变长扩展区
//! ├────────────────────────┤ 480 + tlv_len
//! │ Header MAC      32 B   │  HMAC-SHA256
//! ├────────────────────────┤ header_len
//! │ Encrypted Payload      │  分块 AEAD
//! └────────────────────────┘
//! ```
//!
//! `header_len = 96 + 384 + tlv_len + 32`
//!
//! ## 字节序
//!
//! **全部多字节整数为小端序**，唯一例外是载荷 nonce 与 AAD 中的块序号
//! （大端序，遵循 STREAM 构造惯例）。
//!
//! ## 安全要点
//!
//! - 两级 KDF：`Argon2id` 派生的 [`Kek`] 必须在会话内缓存，
//!   每文件用 HKDF 微秒级派生子密钥。详见 [`crypto`] 模块文档。
//! - 头部 MAC 在解包出 FEK 后**立即**验证，失败必须终止。
//! - 未使用的 key slot 填充随机字节，这是可否认性的前提。

#![forbid(unsafe_code)]
#![warn(missing_docs)]
// 测试代码中 unwrap 与索引是合理的：失败即测试失败，且下标均为已知常量。
// 只对 cfg(test) 编译单元放宽，库代码仍受严格 lint 约束——
// 库代码处理不可信输入，任何 panic 路径都是拒绝服务缺陷。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation,
        clippy::arithmetic_side_effects
    )
)]

pub mod container;
pub mod crypto;
pub mod error;
pub mod file;
pub mod fsatomic;
pub mod header;
pub mod keyslot;
pub mod pack;
pub mod payload;
pub mod reencrypt;
pub mod restore;
pub mod scan;
pub mod session;
pub mod shard;
pub mod slot;
pub mod source;
pub mod unpack;
pub mod dirname;
pub mod tlv;
pub mod tree;
pub mod util;

pub use container::{ContainerBuilder, ContainerEntry, ContainerIndex, EntryKind, EntryMeta};
pub use crypto::{Argon2Params, CipherId, Fek, Kek, SecretKey};
pub use error::{Error, ExitCode, Result};
pub use file::{
    EncryptOptions, EncryptedFile, OpenedFile, RandomMaterial, encrypt, is_omy_file, open,
    open_with_password, peek_header,
};
pub use fsatomic::{AtomicWriter, TempPlaintext, write_atomic};
pub use header::{FixedHeader, MAGIC_FILE, MAGIC_SHARD, flags};
pub use scan::{ScanHit, ScanOptions, ScanResult, UnlockOutcome, probe_file, scan_dir};
pub use session::{CredentialKind, SessionKeys};
pub use shard::{ShardHeader, merge, split};
pub use source::{BlockSource, ContainerEntryView, LocalFileSource, MemorySource, read_source_range};
pub use tlv::{TlvEntry, TlvSet};

/// 本 crate 实现的格式版本。
pub const FORMAT_VERSION: (u16, u16) = (header::VERSION_MAJOR, header::VERSION_MINOR);

/// omy 加密文件的扩展名（不含点）。
pub const FILE_EXTENSION: &str = "omy";
