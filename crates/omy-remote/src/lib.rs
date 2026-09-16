//! 远程位置：统一抽象与驱动。
//!
//! # 分层
//!
//! ```text
//! RemoteStore      列举 / 读 Range / 写 / 删 / 改名     ← 驱动实现它
//! Capabilities     能力位图，唯一真相                    ← 界面与命令层都读它
//! RemoteSource     impl omy_core::source::BlockSource    ← 接上现有解密与播放
//! ```
//!
//! 驱动只管字节搬运，识别 omy 文件、解密、播放全部复用 `omy-core` 既有实现。

pub mod cache;
pub mod caps;
pub mod source;
pub mod store;
pub mod webdav;

pub use caps::Capabilities;
pub use source::RemoteSource;
pub use store::{Entry, RemoteStore};

/// 远程操作错误。
///
/// 分得比较细，是因为**界面对不同原因的处理完全不同**：认证失败要弹登录，
/// 网络失败要给重试，而「位置只读」根本不该走到这里（能力位图应该已经
/// 拦住了）。全都归成一个 `Error(String)` 的话，界面只能一律显示
/// 「操作失败」，用户不知道下一步该干什么。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 网络不可达或超时。
    #[error("网络错误: {0}")]
    Network(String),
    /// 认证失败或令牌过期。
    #[error("认证失败，请重新登录")]
    Unauthorized,
    /// 服务端拒绝（权限不足）。
    #[error("没有权限")]
    Forbidden,
    /// 目标不存在。
    #[error("找不到: {0}")]
    NotFound(String),
    /// 被服务端限流。
    #[error("请求过于频繁，请稍后再试")]
    RateLimited,
    /// 该位置不支持此操作。
    ///
    /// 走到这里说明能力位图与实际能力不一致——要么驱动声明错了，
    /// 要么界面没按位图过滤。两者都是 bug，不是用户的问题。
    #[error("该位置不支持此操作: {0}")]
    Unsupported(&'static str),
    /// 服务端返回了无法理解的响应。
    #[error("服务端响应异常: {0}")]
    Protocol(String),
    /// 本地 I/O 失败（缓存读写等）。
    #[error("本地读写失败: {0}")]
    Io(#[from] std::io::Error),
}

/// 结果别名。
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    /// 是否值得重试。
    ///
    /// 认证失败重试没有意义（除非先刷新令牌），而限流与网络抖动重试有用。
    /// 分不清这两类会让界面要么白白重试几次才报错，要么该重试时直接放弃。
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Network(_) | Self::RateLimited)
    }
}
