//! omy 局域网共享。
//!
//! # 核心安全模型
//!
//! **服务端是纯密文块服务器，永远不接触密码、KEK、FEK 或明文。**
//!
//! 这不是一句口号，而是由类型系统保证的：服务端读取数据的唯一途径是
//! [`serve::CiphertextSource`]，该 trait 只有"读密文字节"和"取密文长度"
//! 两个方法，**没有任何办法拿到密钥或明文**。想违反这条约束就得先改
//! trait 定义，那是显眼的改动，不会在日常修改中悄悄发生。
//!
//! 由此得出一个也许违反直觉、但已经实测确认的结论（决策 DEC-16）：
//!
//! > **共享方锁屏后可以继续共享。**
//!
//! 因为服务端本来就不持有任何密钥，锁屏与否对它没有影响。
//! 验证见 `omy-core/examples/verify_keyless_serve.rs`。
//!
//! # 分层
//!
//! ```text
//!   discovery   mDNS 找到局域网内的其他设备
//!       ↓
//!   pairing     SPAKE2 把 6 位 PIN 变成强密钥（含密钥确认）
//!       ↓
//!   channel     Noise IK 加密信道，1-RTT，前向保密
//!       ↓
//!   wire        请求/响应的字节编解码
//!       ↓
//!   serve       服务端：handle → 密文字节
//! ```
//!
//! # 许可
//!
//! 与 `omy-core` 一致的 MIT OR Apache-2.0。本 crate 不链接 `FFmpeg`，
//! 因此不受 LGPL 传染。

#![forbid(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![warn(missing_docs)]
// 库代码不放宽这些：处理不可信的网络输入，任何 panic 路径都是
// 可远程触发的拒绝服务缺陷。测试代码另行放宽。
#![warn(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

pub mod channel;
pub mod discovery;
pub mod error;
pub mod handshake;
pub mod pairing;
pub mod serve;
pub mod wire;

pub use error::{NetError, Result};

/// 线路协议版本。
///
/// 双方在握手时交换并比对。不兼容时立即断开并给出清晰提示，
/// 好过让两个版本用各自的理解解析同一段字节。
pub const PROTOCOL_VERSION: u16 = 1;

/// mDNS 服务类型。
///
/// 下划线前缀与 `_tcp` 后缀是 RFC 6763 的要求，末尾的点不能少。
pub const SERVICE_TYPE: &str = "_omy._tcp.local.";
