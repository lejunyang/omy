//! Telegram provider。
//!
//! # 已实测确认的几件事（写实现时别按网上示例改回去）
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
//! 4. **分片 `limit` 必须能整除 1 MiB**（实测 `limit=1024` 被拒
//!    `LIMIT_INVALID`），`offset` 必须是 `limit` 的倍数，越过文件尾返回
//!    **0 字节而不报错**。详见 [`store::CHUNK`] 上的表。
//! 5. **`getDialogs` 的终止条件要看「返回条数 < 请求 limit」**，不能看服务端
//!    给的总数：取完时它返回 `None`。
//! 6. **受保护内容（`noforwards`）不拦 `upload.getFile`**：转发被拒，
//!    但取文件字节与 `file_reference` 都正常。
//! 7. **寻址必须走对话枚举，不能用 `resolve_username`**：后者只认有公开用户名
//!    的对话，私有群 / 私聊 / 收藏夹一律找不到。
//! 8. **对话级 `noforwards` 才是真相**：频道级保护开着时，单条消息上的
//!    `noforwards` 是 0，按消息级判断会得出「没开保护」的错误结论。
//!
//! 以上均为 2026-09 在真实账号上的实测，记录见 `docs/research/15-telegram-remote.md`
//! 的 §8.6 / §8.7。**这些是实测结论而非文档推断，改动前先复测，不要凭官方文档
//! 改回去。**
//!
//! # 仍未实测
//!
//! - 上传单文件大小上限；
//! - `file_reference` 过期后的刷新时机；
//! - CDN 重定向（`upload.getFile` 回 `CdnRedirect`）的真实触发条件——
//!   目前明确返回 `Unsupported`，因为 grammers 在那条分支上 panic，
//!   而 omy-gui 禁 panic；
//! - `getDialogs` 单页返回量的上限（样本账号只有 4 个对话，测不出）。

pub mod appid;
pub mod appid_store;
pub mod auth;
pub mod connect;
pub mod device;
pub mod login;
pub mod phonelogin;
pub mod place_secret;
pub mod proxy;
pub mod qr;
pub mod qrlogin;
pub mod register;
pub mod session;
pub mod store;
pub mod tdata;

pub use appid::{AppId, AppIdError, SessionIdentity, SessionMismatch, BUILTIN_API_ID};
pub use auth::{SendCodeOutcome, SentCodeKind};
pub use connect::{connect_saved, connect_saved_with_keks, connect_with_password, ConnectError, Connection};
pub use device::{DeviceInfo, DEVICE_MODEL};
pub use login::{LoginError, LoginFlow, LoginMethod, LoginState};
pub use proxy::{normalize as normalize_proxy, ProxyError, ProxyUrl};
pub use qr::{decide as decide_qr, encode_matrix, token_url, QrMatrix, QrOutcome, QrStep};
pub use qrlogin::{QrError, QrEvent, QrSession};
pub use session::{SavedSession, SessionError};
pub use store::{
    plan_chunks, ChunkPlan, Conversation, ForwardTarget, TelegramId, TelegramStore,
    MAX_FORWARD_BATCH,
};
pub use tdata::{read_tdata, to_saved_session, MtpAuthorization, TdataError};
