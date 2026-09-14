//! omy 的媒体探测、播放分级与缩略图生成。
//!
//! # 设计边界
//!
//! 本 crate **不链接** FFmpeg，只通过**独立子进程 + 管道**与它通信。
//! 这是 `docs/research/04-media-playback.md` §14 的强制要求：FFmpeg 是
//! CVE 高发区，而本应用要用它处理用户的任意文件；一个恶意构造的 MKV
//! 若在主进程内解析，可能直接导致持有密钥的进程被攻陷。
//!
//! 因此：
//!
//! - 子进程**不持有密钥**，主进程解密后把明文字节喂给它；
//! - 子进程**不访问文件系统**，只用 stdin/stdout（已实测 ffprobe 支持
//!   从管道读取，见 `spikes/probe-ffprobe-contract.ps1`）；
//! - 子进程崩溃**不影响**主进程。
//!
//! # FFmpeg 缺失是正常状态，不是错误
//!
//! 用户机器上可能根本没有 FFmpeg。本 crate 的所有探测函数在这种情况下
//! 返回[`MediaError::FfmpegUnavailable`]，调用方应据此**降级**（例如仅按
//! 扩展名给出保守分级），而不是把整个操作判为失败。
//!
//! # 为什么不用 Rust 的 FFmpeg 绑定
//!
//! `ffmpeg-next` 等绑定把 libav* 直接链进本进程，与上述隔离要求直接冲突：
//! 一旦链入，解析漏洞就发生在持有密钥的地址空间里。宁可付子进程的开销。

#![forbid(unsafe_code)]
// 库代码不放宽 panic 相关的 lint：本 crate 处理不可信输入
//（用户的任意媒体文件、FFmpeg 的任意输出），任何 panic 路径都是
// 拒绝服务缺陷。测试代码另行放宽。
#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

pub mod caps;
pub mod convert;
pub mod error;
pub mod ffprobe;
pub mod meta;
pub mod mkv;
pub mod mp4;
pub mod prepare;
pub mod probe;
pub mod remux;
pub mod thumbnail;
pub mod tier;

pub use caps::{Capabilities, capabilities};
pub use error::{MediaError, Result};
pub use meta::MediaMeta;
pub use prepare::{PrepareOptions, Prepared, ThumbSource, prepare};
pub use probe::{AudioStream, MediaInfo, SubtitleStream, VideoStream};
pub use tier::{PlaybackTier, TierVerdict};
