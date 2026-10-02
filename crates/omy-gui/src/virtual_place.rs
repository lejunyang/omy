//! 虚拟远程位置业务的 **re-export 薄壳**。
//!
//! 核心模型、持久化与位置级加解密已抽到 [`omy_remote::virtuals`]，GUI 与 CLI
//! 共用同一份实现（见该模块文档「为什么抽出来」）。这里只把名字再导出来，
//! 让 GUI 内既有的 `crate::virtual_place::X` 路径继续可用——**不要**在这里再写
//! 第二份模型/落盘/加解密逻辑，否则又回到两套实现漂移的老路。

pub use omy_remote::virtuals::{
    random_id, KdfMaterial, Reference, Snapshot, SourceRef, VFolder, VirtualCryptoError,
    VirtualRegistry,
};
