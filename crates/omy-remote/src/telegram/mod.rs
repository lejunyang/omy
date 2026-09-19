//! Telegram provider。
//!
//! # 本模块目前的范围
//!
//! 只有**不依赖网络实测**的那部分：应用身份（api_id / api_hash）与它对 session
//! 的约束、代理地址归一化。`TelegramStore` 本体、`list` / `read_range`、登录状态
//! 机要等几项实测钉下来才写，见下。
//!
//! # 已实测确认的三件事（写实现时别按网上示例改回去）
//!
//! 1. **直连 Telegram 数据中心不通，经 SOCKS5 代理可通。** 本机实测：直连
//!    `149.154.167.51:443` 超时；经 `socks5://127.0.0.1:7897` 的 MTProto 握手
//!    与 `help.getNearestDc` 都成功。
//! 2. **grammers 0.10 只接受 `socks5://`**：`http://` 报
//!    `proxy scheme not supported: http`，`socks5h://` 也拒，缺端口也拒。
//!    所以有 [`proxy::normalize`] 这一层，而不是把用户填的原样递下去。
//! 3. **grammers 0.10 的 API 与网上示例差异很大**：`Session` 是 trait 而不是
//!    结构体、**没有 `Client::connect`**。要走
//!    `SenderPool::with_configuration(session, api_id, params)` 拿到
//!    `{ runner, handle }`，再 `Client::new(handle)`，并把 `runner.run()` spawn
//!    起来。照 docs.rs 上旧版本的示例写会编译不过——**看到示例风格的代码要先
//!    确认它是哪个版本的**，别照着改回去。
//!
//! # 仍未实测、因此不在这里写死的东西
//!
//! 服务端的**授权检查早于参数校验**（实测：无账号时给 `upload.getFile` 传一个
//! 违规 offset，返回的是 `AUTH_KEY_UNREGISTERED` 而不是参数错误）。这意味着
//! 下面几项**必须有已登录账号才能测**，目前都还是文档记载而非实测：
//!
//! - 分片 `offset` / `limit` 的真实约束；
//! - `messages.getDialogs` 的分页行为与单页返回量；
//! - `messages.search` 的实际行为与可用过滤器；
//! - 受保护内容（`noforwards`）下 `upload.getFile` 到底会不会被拦。
//!
//! **不要根据官方文档把这些常量钉死**：那是把推断写成事实。

pub mod appid;
pub mod device;
pub mod login;
pub mod proxy;
pub mod qr;
pub mod store;

pub use appid::{AppId, AppIdError, SessionIdentity, SessionMismatch, BUILTIN_API_ID};
pub use device::{DeviceInfo, DEVICE_MODEL};
pub use login::{LoginError, LoginFlow, LoginMethod, LoginState};
pub use proxy::{normalize as normalize_proxy, ProxyError, ProxyUrl};
pub use qr::{decide as decide_qr, token_url, QrOutcome, QrStep};
pub use store::{plan_chunks, ChunkPlan, Conversation, TelegramId, TelegramStore};
