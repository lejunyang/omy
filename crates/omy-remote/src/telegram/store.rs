//! `TelegramStore`：把「对话即目录」映射到 [`RemoteStore`]。
//!
//! # 映射关系
//!
//! ```text
//! RemoteStore 的概念        Telegram 的东西
//! ─────────────────────    ──────────────────────────────
//! 根目录（dir_id = ""）     对话列表（messages.getDialogs）
//! 一个子目录               一个对话（群 / 频道 / 私聊 / 收藏夹）
//! 目录里的一个文件          该对话里一条带媒体的消息
//! read_range               分片下载（upload.getFile）
//! ```
//!
//! # 为什么没有 rename 与 create_dir
//!
//! 这两个在 Telegram 下**没有真实语义**，所以不实现（走 trait 的默认实现报
//! `Unsupported`）：
//!
//! - **rename**：消息里的文件名是媒体属性的一部分，改名等于重新发一条消息、
//!   删掉旧的——那是「重新上传」，不是「重命名」。假装支持会让用户以为改了个
//!   名字，实际上消息 id 变了、别人的引用全断。
//! - **create_dir**：目录就是对话列表本身。「新建目录」等于「建一个群」，那是
//!   个有社交后果的动作（会产生邀请链接、通知联系人），不该藏在文件管理器的
//!   右键菜单里。
//!
//! 这不是「暂未实现」，而是**如实声明做不到**，见
//! [`Capabilities::conversation_writable`]。
//!
//! # 能力随对话而变，所以必须实现 `effective_capabilities`
//!
//! 同一个 Telegram 位置里：收藏夹可写、自己的频道可写、别人的公开频道只读、
//! 某些群禁止发文件。位置级只能给上界，目录级才是实际能力。
//!
//! # 本模块的完成度（如实标注）
//!
//! 结构、id 编解码、分片对齐、以及 `list` / `read_range` 的真实 RPC **均已实现**。
//!
//! 分片的 `offset` / `limit` 约束**已于 2026-09-20 用真实账号实测钉死**，
//! 见 [`CHUNK`] 上的表格。其中两条与广为流传的文档记载不符：`limit=1024`
//! 会被拒，越过文件尾返回 0 字节而不是报错。**不要按文档把它们改回去。**

use std::sync::Mutex;

use grammers_client::media::{Downloadable, Media, PhotoSize};
use grammers_client::message::InputMessage;
use grammers_client::{tl, Client, InvocationError};
use grammers_session::types::PeerRef;

use crate::store::{Entry, RemoteStore};
use crate::{Capabilities, Error, Result};

/// 分片下载的单片大小。**已由实测钉死，不要凭文档改。**
///
/// # 实测出的真实约束（2026-09-20，真实账号，经 socks5 代理）
///
/// | offset | limit | 结果 |
/// |---|---|---|
/// | 0 | 4096 | 接受 |
/// | 0 | 1024 | **拒绝** `LIMIT_INVALID` |
/// | 0 | 1 MiB | 接受 |
/// | 0 | 1 MiB + 1 KiB | 拒绝 `LIMIT_INVALID` |
/// | 0 | 1000 | 拒绝 `LIMIT_INVALID` |
/// | 0 | 3072 | 拒绝 `LIMIT_INVALID`（3 KiB 不整除 1 MiB） |
/// | 1 | 4096 | 拒绝 `OFFSET_INVALID` |
/// | 4096 | 4096 | 接受 |
/// | 512 KiB | 512 KiB | 接受（越过文件尾，返回 0 字节） |
///
/// # 两条与「文档记载」不符、必须记住的地方
///
/// 1. **`limit=1024` 被拒**。广为流传的说法是「limit 为 1 KiB 的倍数即可」，
///    实测不成立——1 KiB 本身就会收到 `LIMIT_INVALID`。所以不要把下界当成
///    1 KiB，那会写出一段偶尔报 400 的代码。
/// 2. **越过文件尾不报错，返回 0 字节**。所以「读到 0 字节」是正常的结束信号，
///    不是失败；把它当错误会让最后一片总是报错。
///
/// 选 [`CHUNK`] = 512 KiB 的理由：实测接受，且同时满足「1 KiB 的倍数」与
/// 「整除 1 MiB」两条（服务端真正校验的似乎正是后者）。它也足够大，不会让
/// 一个中等大小的文件被切成上百次请求——而请求数直接关系到会不会撞限流。
const CHUNK: u64 = 512 * 1024;

/// `read_range` 撞到 CDN 重定向时，`Error::Unsupported` 带的标记。
///
/// 做成公开常量而不是就地写字面量：GUI 要据此给一句专门的提示，
/// 两边各写一份字面量的话，改动一侧会让提示静默退回通用错误文案，
/// 而这不会有任何报错。
pub const CDN_REDIRECT: &str = "telegram cdn redirect";

/// 广播频道不提供消息视图时给出的标记。
///
/// 原因见 [`TelegramStore::messages`]：ToS 3.3 的 sponsored messages 要求。
/// 做成公开常量供 GUI 映射成一句用户看得懂的话——否则界面只会显示一句
/// 笼统的「不支持此操作」，而用户完全不知道为什么这个频道没有消息视图。
pub const BROADCAST_NO_MESSAGES: &str = "telegram broadcast has no message view";

/// 进一个对话时默认拉多少条带文件的消息。
///
/// 不是「全部」：一个活跃群里可能有上万条，全拉会让进目录等很久，也白白占
/// 限流配额。取小是刻意对齐官方客户端（DEC-26）：TDesktop/Web 每页
/// `messages.getHistory` 约 20 条，进对话先出一屏、滚动再续，而不是一次性
/// 拉满。15 条够填首屏并留一点滚动余量，不够时 `list_more(before=...)` 续。
///
/// 早先这里是 100——那是「进目录卡很久」的一个来源：活跃群里要等服务端
/// 把 100 条都筛完才出第一屏。
const DEFAULT_MESSAGE_PAGE: usize = 15;

/// 为凑够一页文件，最多往回扫多少条消息。
///
/// 文件视图要的是「文件」不是「消息」，而很多群里绝大多数是文本，只看
/// 一屏消息可能一个文件都没有（实测有群最新 15 条全是文本）。所以扫描
/// 深度必须大于展示条数。但也要有上界：纯聊天群里可能翻几千条都没文件，
/// 不设界就会一直翻下去。200 条约等于十几次 getHistory 往返，够覆盖
/// 「文件夹杂在聊天里」的常见情形，又不至于失控。
const MAX_MESSAGE_SCAN: usize = 200;

/// 头像/图片下载定位类型的再导出，供命令层构造后台头像补齐任务时命名，
/// 不必让上层直接依赖 grammers 的模块路径。
pub use grammers_client::media::ChatPhoto;

/// 服务端接受的最大单片大小：**1 MiB**。实测 1 MiB 接受、1 MiB + 1 KiB 被拒。
///
/// 有这个常量是为了让「别把 chunk 调过头」这件事有个可检查的上界，
/// 而不是等运行时收到 `LIMIT_INVALID` 才发现。
pub const MAX_CHUNK: u64 = 1024 * 1024;

/// 服务端接受的最小单片大小：**4 KiB**。
///
/// 实测 1 KiB 与 1000 都被拒，4 KiB 通过。**不要改成 1024**——那正是文档记载
/// 与实测不符的地方，改了会得到偶发的 `LIMIT_INVALID`。
pub const MIN_CHUNK: u64 = 4 * 1024;

/// 这个分片大小服务端会不会接受。
///
/// 规则由实测归纳：落在 [`MIN_CHUNK`, `MAX_CHUNK`] 内、且能整除 1 MiB。
/// 后一条是实测里 3072 被拒而 4096 通过所揭示的——3 KiB 是 1 KiB 的倍数却
/// 不整除 1 MiB。
#[must_use]
pub const fn chunk_is_valid(chunk: u64) -> bool {
    chunk >= MIN_CHUNK && chunk <= MAX_CHUNK && MAX_CHUNK % chunk == 0
}

/// 判断对话列表还有没有下一页。
///
/// # 为什么不能看服务端给的总数
///
/// 实测（2026-09-19，真实账号）：`limit=1` 时服务端返回 1 条并声明总数
/// `Some(4)`；而 `limit=20/100/500` 时都返回 4 条、总数是 **`None`**。
/// 也就是说**服务端只在还没取完时给总数，取完了就不给**。
///
/// 不这样会怎样：按总数判断，取到最后一页会拿到 `None`——把它当 0 则少一页
/// （对话凭空消失），当「未知」则永远以为还有下一页（翻页翻不到头）。
/// 两种症状都不指向真正的原因，会让人去查分页偏移、缓存、甚至服务端限流。
///
/// 所以唯一可靠的终止条件是**返回条数 < 请求的 limit**。
#[must_use]
pub const fn has_more_dialogs(returned: usize, requested: usize) -> bool {
    returned >= requested
}

/// 一次 `read_range` 要发的分片请求。
///
/// 把「对齐 + 裁剪」算成数据而不是直接发请求，是为了让这段最容易出错的逻辑
/// 能被单测覆盖——它不需要账号也不需要网络。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkPlan {
    /// 第一片的起始偏移（已按 `chunk` 对齐，必然 <= 请求的 offset）。
    pub start: u64,
    /// 要取几片。
    pub count: u64,
    /// 拼好之后，从头丢掉多少字节。
    ///
    /// 因为服务端只接受对齐的 offset，我们不得不从更早的位置开始取。
    pub skip: u64,
    /// 丢掉 `skip` 之后保留多少字节。
    pub take: u64,
}

/// 把任意 `[offset, offset+len)` 拆成对齐的分片请求。
///
/// # 为什么要单独一个纯函数
///
/// 这是整个驱动里最容易写错、且错了最难发现的地方：少算一片会让播放器在
/// seek 后拿到短了一截的数据，而解密层只会报「认证失败」——看起来像密钥问题，
/// 和 offset 算错完全联系不起来。做成纯函数才能用单测把边界钉住。
#[must_use]
pub fn plan_chunks(offset: u64, len: u64, chunk: u64) -> ChunkPlan {
    // chunk 为 0 会让下面的除法炸掉。调用方不该传，但这一层不 panic：
    // 这个 crate 服务于 GUI，而 GUI 里禁止 panic。
    let chunk = chunk.max(1);
    let start = (offset / chunk) * chunk;
    let skip = offset - start;
    // 向上取整：末尾那不足一片的部分也必须取回来，否则尾部数据会缺。
    let count = if len == 0 {
        0
    } else {
        (skip + len).div_ceil(chunk)
    };
    ChunkPlan {
        start,
        count,
        skip,
        take: len,
    }
}

/// 文件在这个位置里的标识。
///
/// Telegram 没有路径，要定位一个文件必须同时知道**在哪个对话**与**哪条消息**。
/// 所以 id 是复合的，而 [`Entry::id`] 是个字符串——这里定义它的编码。
///
/// # 为什么不用 `file_reference` 当 id
///
/// `file_reference` 会过期（服务端只保证短期有效）。用它做 id，缓存键就会随
/// 刷新而变，同一个文件会被反复重新下载；而 `(对话, 消息号)` 是稳定的，过期时
/// 拿它重取消息即可换到新引用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelegramId {
    /// 对话标识。
    pub chat: i64,
    /// 消息号。
    pub message: i32,
}

impl TelegramId {
    /// 编码成 [`Entry::id`]。
    ///
    /// 形如 `tg:<chat>:<message>`。带前缀是为了让一眼能看出这是谁的 id，
    /// 也避免将来与别的 provider 的 id 混淆时无从分辨。
    #[must_use]
    pub fn encode(&self) -> String {
        format!("tg:{}:{}", self.chat, self.message)
    }

    /// 从 [`Entry::id`] 解回来。
    ///
    /// # Errors
    ///
    /// 格式不对时返回 [`Error::Protocol`]。**不做任何猜测式容错**：
    /// 一个解不开的 id 意味着上层传错了东西，猜一个值出来只会让错误在更远的
    /// 地方以更难懂的方式冒出来。
    pub fn decode(s: &str) -> Result<Self> {
        let rest = s
            .strip_prefix("tg:")
            .ok_or_else(|| Error::Protocol(format!("不是 Telegram 的 id：{s}")))?;
        // 从第一个 ':' 切。用 rsplit_once 也一样对：`tg:1:2:3` 两种切法都会
        // 被拒（split 拒在消息号 "2:3" 不是整数，rsplit 拒在对话号 "1:2"
        // 不是整数），已穷举核实两者的接受/拒绝结论完全一致。
        // 记这一笔是因为变异测试里换成 rsplit_once 不会让任何断言失败——
        // 那不是断言不足，而是这两种写法在这里等价。别当成缺陷去"修"。
        let (chat, message) = rest
            .split_once(':')
            .ok_or_else(|| Error::Protocol(format!("Telegram id 缺少消息号：{s}")))?;
        Ok(Self {
            chat: chat
                .parse()
                .map_err(|_| Error::Protocol(format!("对话号不是整数：{s}")))?,
            message: message
                .parse()
                .map_err(|_| Error::Protocol(format!("消息号不是整数：{s}")))?,
        })
    }
}

/// 对话在这个位置里既是目录、也需要带上它的能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// 对话标识。
    pub chat: i64,
    /// 显示名。
    pub title: String,
    /// 当前账号能不能往里发文件。
    pub can_send: bool,
    /// 当前账号能不能删里面的消息。
    pub can_delete: bool,
    /// 是不是**广播频道**（`broadcast=true`，不是超级群）。
    ///
    /// 单独记这一位是因为消息视图要据它决定出不出现，见
    /// [`TelegramStore::messages`] 上关于 ToS 3.3 的说明。
    /// 超级群（megagroup）不算——它在协议里也是 Channel，但不是广播频道。
    pub broadcast: bool,
    /// 这个对话是否开了「受保护内容」（`noforwards`）。
    ///
    /// # 必须从对话上读，不能从消息上读
    ///
    /// 实测结论（见本模块文档第 8 条）：频道级保护开着时，单条消息上的
    /// `noforwards` 是 **0**。按消息级判断会得出「没开保护」的错误结论，
    /// 而界面上那行告知就永远不出现。
    ///
    /// # 它不影响读取
    ///
    /// 同样是实测：`noforwards` 拦的是转发，**不拦 `upload.getFile`**，
    /// 取文件字节与 `file_reference` 都正常。所以界面上这是一行低权重的
    /// 事实告知而不是警告——用户要知道的是「这个群开了保护，但不影响你
    /// 在这里打开文件」，而不是以为有什么坏了。
    pub protected: bool,
    /// 对话类型：`user` | `group` | `channel`。
    ///
    /// 界面用它选图标。**对话不是文件夹**，用 📁 表示会让人以为里面是
    /// 目录结构；拿不到头像时按类型给 Telegram 风格的图标才说得通。
    pub kind: &'static str,
    /// 头像原始字节（JPEG），拿不到就是 `None`。
    ///
    /// 用的是服务端已有的头像文件（`Peer::photo(false)` 给出小尺寸），
    /// 不是下载什么大图再缩——列表里够用，也省流量。
    pub avatar: Option<Vec<u8>>,
}

/// 取媒体的缩略图字节。
///
/// # 为什么要挑尺寸而不是拿第一个
///
/// `thumbs()` 里可能有 `StrippedSize`——它的 `bytes` **不是完整 JPEG**：
/// Telegram 裁掉了标准量化表与霍夫曼表以省几十字节，要还原得自己拼回去。
/// 直接把它交给 `<img>` 会得到一张**永远加载失败、而且不报错**的图，
/// 界面上表现为「缩略图位置一直空着」，完全看不出原因。
///
/// 所以这里跳过它，取一个能直接渲染的尺寸；没有就返回 `None`，
/// 界面回落到类型图标。
async fn thumb_bytes(
    client: &grammers_client::Client,
    media: &grammers_client::media::Media,
) -> Option<Vec<u8>> {
    use grammers_client::media::Media;

    let thumbs = match media {
        Media::Photo(p) => p.thumbs(),
        Media::Document(d) => d.thumbs(),
        Media::Sticker(s) => s.document.thumbs(),
        _ => return None,
    };
    // 按面积挑一个最小但仍可直接渲染的。太大的没必要——列表里只占几十像素
    let mut best: Option<grammers_client::media::PhotoSize> = None;
    for t in thumbs {
        // StrippedSize 的 photo_type 是 "i"，它不是完整 JPEG，跳过
        if t.photo_type() == "i" {
            continue;
        }
        let better = best.as_ref().is_none_or(|b| t.size() < b.size());
        if better {
            best = Some(t);
        }
    }
    let t = best?;
    download_bytes(client, &t).await
}

/// 视频时长（秒）。非视频返回 `None`。
fn media_duration(media: &grammers_client::media::Media) -> Option<u32> {
    use grammers_client::media::Media;
    match media {
        // grammers 给的是 f64 秒。向下取整——「1 分 30 秒」比
        // 「1 分 30.7 秒」更像时长角标
        Media::Document(d) => d.duration().map(|x| x.max(0.0) as u32),
        _ => None,
    }
}

/// 把一个可下载对象整体读成字节。
///
/// 只用于头像这类**很小**的东西。大文件走 `read_range` 的分块路径，
/// 不要用这个——它会把整个内容读进内存。
async fn download_bytes<D: grammers_client::media::Downloadable>(
    client: &grammers_client::Client,
    d: &D,
) -> Option<Vec<u8>> {
    let mut it = client.iter_download(d);
    let mut buf = Vec::new();
    loop {
        match it.next().await {
            Ok(Some(chunk)) => buf.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(_) => return None,
        }
    }
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

impl Conversation {
    /// 这个对话当作目录时的 `dir_id`。
    #[must_use]
    pub fn dir_id(&self) -> String {
        format!("tg:{}", self.chat)
    }

    /// 从 `dir_id` 取回对话号。
    ///
    /// # Errors
    ///
    /// 格式不对时返回 [`Error::Protocol`]。
    pub fn parse_dir_id(dir_id: &str) -> Result<i64> {
        dir_id
            .strip_prefix("tg:")
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| Error::Protocol(format!("不是 Telegram 的目录 id：{dir_id}")))
    }

    /// 这个对话里的有效能力。
    #[must_use]
    pub fn capabilities(&self) -> Capabilities {
        Capabilities::conversation_writable(self.can_send, self.can_delete)
    }
}

/// 一条带媒体的消息在目录里的样子。
///
/// 缓存它是为了让 `read_range` 不必为每次读都重新拉一遍消息——那会让一次视频
/// seek 变成两次请求，而 Telegram 对请求频率很敏感。
#[derive(Clone)]
struct CachedMedia {
    /// 下载位置。**会过期**（`file_reference` 有时效），过期后要重新 list。
    location: tl::enums::InputFileLocation,
    /// 字节数。
    size: u64,
}

/// 把一次读取收敛到文件尾之内。
///
/// 返回实际应该读多少字节；0 表示整段都在文件尾之外、不必发任何请求。
///
/// # 为什么要有这一步
///
/// 服务端对越过文件尾的请求回 **0 字节而不是报错**（实测）。也就是说发出去
/// 也「成功」，只是白白花掉一次往返和一次限流配额——而配额是真的会被打光的。
///
/// `size == 0` 当作**大小未知**（文档没给 size 时就是这样），此时不做任何
/// 收敛：猜 0 会把每一次读都判成越界，表现为「文件全是空的」。
#[must_use]
pub const fn clamp_to_eof(offset: u64, len: u64, size: u64) -> u64 {
    if size == 0 {
        return len;
    }
    if offset >= size {
        return 0;
    }
    let left = size - offset;
    if len < left {
        len
    } else {
        left
    }
}

/// 这个对话是不是一个「已经迁移走」的旧基础群。
///
/// 判据是 `Chat` 上的 `migrated_to` 有值，或 `deactivated` 为真。两个都看：
/// 前者说明它升级成了超级群，后者覆盖群被解散之类的情况，两种的共同点是
/// **再也发不进去东西**。
fn is_migrated_away(peer: &grammers_client::peer::Peer) -> bool {
    use grammers_client::peer::Peer;
    let Peer::Group(g) = peer else {
        return false;
    };
    match &g.raw {
        tl::enums::Chat::Chat(c) => c.migrated_to.is_some() || c.deactivated,
        // Empty / Forbidden 也进不去，但那属于「没权限」而不是「迁移走了」，
        // 交给权限那条路径去表达，这里只管迁移
        _ => false,
    }
}

/// 一个媒体的字节数；拿不到时给 0（表示「大小未知」）。
///
/// 图片与文档取大小的路径不同：文档直接有 size，而图片在服务端存的是若干种
/// 尺寸，要取最大的那一档。给 0 而不是猜一个数——`clamp_to_eof` 把 0 当作
/// 「未知」从而不做收敛，猜一个数则会把文件在那个位置截断。
fn media_size(m: &Media) -> u64 {
    u64::try_from(Downloadable::size(m).unwrap_or(0)).unwrap_or(0)
}

/// 取一个媒体**随消息一起送来**的内嵌缩略图，转成可直接解码的图片字节。
///
/// # 零额外请求
///
/// Telegram 的消息对象里本来就带着若干档缩略图。其中 `Stripped` 与 `Cached`
/// 两档的字节**就在消息里**，取它们不发任何请求——这正是「列目录顺便出图、
/// 且不下载原图」的依据。其余档（`Size` / `Progressive`）只有下载位置，
/// 要另发请求才拿得到，所以一概跳过：为一屏几十个文件各发一次请求，
/// 既慢又容易撞 `FLOOD_WAIT`，与这条优化的本意相反。
///
/// # 为什么用 `to_data()` 而不是自己拼 JPEG 头
///
/// `Stripped` 档在协议里是**去掉了标准量化表的残缺 JPEG**（3 字节头 + 熵编码
/// 数据），直接喂给 `<img>` 会得到一张永远加载失败、且不报任何错的图。
///
/// 补头这件事 grammers 已按 Telegram 文档做好（那段头含量化表与霍夫曼表，
/// 还要把宽高回填到固定偏移），并通过**公开的** `Downloadable::to_data()`
/// 暴露出来。`StrippedSize::data()` 确实是私有的，但不该因此手抄一份：
/// 抄错一个字节就是一张解不开的图，而且上游改了格式我们不会跟着变。
/// 要用的是它的公开出口。
///
/// # 返回 `None` 是正常情况
///
/// 纯文档（zip、pdf 之类）本来就没有内嵌缩略图，界面据此回退类型图标。
/// 这段字节像不像一张**结构完整、解码器不会当场拒绝**的图片。
///
/// # 为什么需要这道关
///
/// Telegram 的 stripped 缩略图是**去掉了标准量化表的残缺 JPEG**。它有正确的
/// JPEG 魔数，却缺少解码必需的 DQT（量化表）与 DHT（霍夫曼表）——把它直接交
/// 给 `<img>`，得到的是一张**永远加载失败、且不抛任何错**的图。界面上只会
/// 悄悄回退成类型图标，没人看得出这里坏了。
///
/// 所以「只验魔数」是不够的：那正好是残缺数据能通过的那一关。这里要求段齐全，
/// 让「补头这一步有没有真的做」成为一件能当场发现的事，而不是等用户报
/// 「图不出来」。
///
/// # 这不是完整的 JPEG 校验
///
/// 真正判定能否解码要跑一遍解码器，那对列目录这条路径太重（一屏几十张）。
/// 这里只挡住**最可能出现且最难察觉**的那种坏法：段缺失。
/// 单元测试里另有一条真的走解码器的断言。
///
/// WebP / PNG 不做逐段检查：它们不存在「被剥掉一部分再传过来」这种约定，
/// 认出魔数即可。
fn is_complete_image(b: &[u8]) -> bool {
    if b.is_empty() {
        return false;
    }
    // WebP：RIFF....WEBP，两段都要看，只看 RIFF 会把 wav / avi 也认进来
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(&b"WEBP"[..]) {
        return true;
    }
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        return true;
    }
    if !b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return false;
    }
    // JPEG：必须带量化表与霍夫曼表，并以 EOI 收尾。
    // 这三样正是 stripped 缩略图缺的东西
    let has = |marker: u8| b.windows(2).any(|w| w == [0xFF, marker]);
    has(0xDB) && has(0xC4) && b.ends_with(&[0xFF, 0xD9])
}

fn embedded_thumb(m: &Media) -> Option<Vec<u8>> {
    let sizes = match m {
        Media::Photo(p) => p.thumbs(),
        Media::Document(d) => d.thumbs(),
        // 其余媒体（投票、位置、联系人）没有缩略图这个概念
        _ => return None,
    };

    // 只认字节已经在手里的那两档。
    //
    // 不直接靠「to_data() 返回 Some」来筛：结果虽然一样，但意图不明显——
    // 读代码的人会以为其余档只是碰巧没有数据，进而在某次重构里给它们
    // 补上一次下载，而那正是这里要避免的。
    let mut best: Option<Vec<u8>> = None;
    for s in sizes {
        if !matches!(s, PhotoSize::Stripped(_) | PhotoSize::Cached(_)) {
            continue;
        }
        // to_data() 负责把 stripped 补成完整 JPEG，见上面的说明。
        //
        // 补完仍要过一道 is_complete_image：那条是本函数最要紧的保证，
        // 而它失效时**没有任何报错**——界面只是回退成类型图标
        let Some(bytes) = s.to_data().filter(|b| is_complete_image(b)) else {
            continue;
        };
        // 有多档内嵌时取字节最多的那一档：Cached 通常比 Stripped 清楚，
        // 而两者都很小（几百字节到几 KB），不必为省这点流量牺牲清晰度
        if best.as_ref().is_none_or(|b| bytes.len() > b.len()) {
            best = Some(bytes);
        }
    }
    best
}

/// 一个媒体在文件列表里显示的名字。
///
/// # 为什么要给图片和视频编名字
///
/// 只有「作为文件发送」的东西才带文件名。作为照片发送的图片、作为视频发送的
/// 视频，在协议里都**没有文件名**——直接用 name() 会得到 None，界面上就是一行
/// 看不出是什么的空条目。
///
/// 所以按 mime 推一个扩展名，并用消息号保证唯一。带上扩展名不只是好看：
/// omy 上层按扩展名决定能不能内嵌预览，没有扩展名的视频会被当成未知类型。
fn media_name(m: &Media, msg_id: i32) -> String {
    match m {
        Media::Document(d) => {
            if let Some(n) = d.name().filter(|n| !n.is_empty()) {
                return n.to_string();
            }
            let ext = d.mime_type().and_then(ext_for_mime).unwrap_or("bin");
            format!("message-{msg_id}.{ext}")
        }
        // 图片一律 .jpg：Telegram 服务端把作为照片发送的图片统一转成 JPEG
        // （原图若是 PNG 也会被转），所以这不是猜测而是服务端的行为
        Media::Photo(_) => format!("photo-{msg_id}.jpg"),
        _ => format!("message-{msg_id}"),
    }
}

/// 从 mime 推一个扩展名。
///
/// 只覆盖常见的几种，认不出就让调用方回落到 `bin`。这里不引第三方 mime 库：
/// 为几个分支拉一个依赖不划算，而且真正要紧的是「有个扩展名让上层能判类型」，
/// 不是覆盖全 IANA 列表。
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    // 去掉 "; charset=..." 之类的参数部分再比，否则 "text/plain; charset=utf-8"
    // 会一个都匹配不上
    let base = mime.split(';').next().unwrap_or(mime).trim();
    Some(match base {
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" => "wav",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "text/plain" => "txt",
        _ => return None,
    })
}

/// 消息视图里的一条消息。
///
/// 刻意**很窄**：只有「以文件为主线的消息视图」用得上的字段。
/// 不做回复关系、转发链、reactions、已读状态——那些属于聊天客户端，
/// 而 omy 是文件管理器（文档 §1.3 把它们列在 out of scope）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MessageRow {
    /// 消息号。与文件条目 id 里的那个是同一个，便于两个视图互相跳转。
    pub message: i32,
    /// 消息文字 / 媒体说明。可能为空。
    pub text: String,
    /// Unix 秒。给界面排时间轴用。
    pub date: i64,
    /// 是不是自己发的。
    pub outgoing: bool,
    /// 这条消息带的文件在文件视图里的 id；没有可下载媒体时为 `None`。
    ///
    /// **判据是「能不能取出下载位置」，不是 `Media` 的变体**——`Media` 是
    /// `non_exhaustive` 的，按变体列举会在 grammers 加新类型时静默漏掉；
    /// 而且实测过服务端按**发送方式**分类，一条 `Media::Document` 的视频
    /// 并不被 `Document` 过滤器命中。
    ///
    /// 纯文本消息在这里就是 `None`，那是正常的一行，不是错误。
    pub file_id: Option<String>,
    /// 文件显示名。`file_id` 为 `None` 时也为 `None`。
    pub file_name: Option<String>,
    /// 文件字节数。
    pub file_size: Option<u64>,
    /// 缩略图原始字节（JPEG），没有就是 `None`。
    ///
    /// 用的是服务端已有的小尺寸缩略图，不下载原图再缩——消息列表里
    /// 一屏可能有几十条，拉原图既慢又费流量。
    ///
    /// **只取能直接渲染的尺寸**：`StrippedSize` 那种的字节不是完整 JPEG
    /// （Telegram 裁掉了标准量化表与霍夫曼表），直接交给 `<img>` 会得到
    /// 一张永远加载失败、而且不报错的图。
    pub thumb: Option<Vec<u8>>,
    /// 视频时长（秒），非视频为 `None`。界面用它画时长角标。
    pub duration: Option<u32>,
}

/// Telegram 驱动。
///
/// # 两种构造方式
///
/// [`TelegramStore::new`] / [`with_conversations`](TelegramStore::with_conversations)
/// 不带客户端，只能用于纯逻辑与单测；[`TelegramStore::connected`] 带客户端，
/// 才能真正发 RPC。
///
/// 分开而不是让 client 变成 `Option` 之外的必填项，是因为「对话即目录」的那套
/// id 编解码与分片对齐是纯逻辑，值得能在没有账号的情况下测。
pub struct TelegramStore {
    /// 已连接的客户端。`None` 表示这是个只用于纯逻辑的实例。
    client: Option<Client>,
    /// 后台收发任务的句柄。
    ///
    /// **必须一直活着**：它被丢弃后所有请求只会排进队列、再也发不出去，
    /// 而且不会有任何报错——表现是界面一直转圈。
    ///
    /// 把它放在 store 里而不是 `mem::forget` 掉，是为了让这条约束由类型系统
    /// 表达：store 活着连接就活着。用 forget 的话这条约束只存在于注释里，
    /// 后来的人删掉那行不会有任何编译错误。
    runner: Option<tokio::task::JoinHandle<()>>,
    /// 已知对话，充当目录表。
    ///
    /// 由 `list("")` 填充。做成缓存而不是每次查，是因为 `getDialogs` 有分页
    /// 且会被限流；但**这也意味着它可能过期**，所以 `effective_capabilities`
    /// 查不到对话时必须报错而不是假设可写。
    conversations: Mutex<Vec<Conversation>>,
    /// 对话号 → 访问该对话所需的引用。
    ///
    /// Telegram 的多数请求要的不是裸 id 而是带 `access_hash` 的引用，而那个
    /// hash 只能从 `getDialogs` / `resolve` 之类的结果里拿到。存下来，
    /// 这样进目录时不必为了拿 hash 再列一次全部对话。
    peers: Mutex<std::collections::HashMap<i64, PeerRef>>,
    /// 文件 id → 下载位置。由 `list` 填充，`read_range` 读。
    ///
    /// **不把 `file_reference` 编进 id**：它会过期，编进去会让缓存键随刷新而
    /// 变，同一个文件被反复重新下载。所以 id 用稳定的 `(对话, 消息号)`，
    /// 而易变的位置放在这张表里。
    media: Mutex<std::collections::HashMap<String, CachedMedia>>,
    /// 上一次 refresh_conversations 攒下的、还没下载的头像任务：(对话号, 头像定位)。
    ///
    /// 为什么把它放在 store 上让命令层来取：头像下载慢（每个一次经代理往返），
    /// 若在 refresh 里同步下完，对话列表就要等 72 张头像都到手才返回——那正是
    /// 「冷启动 11 秒」。改成 refresh 只收集任务、立刻返回列表，命令层再在后台
    /// 逐个下、下完一个就通过事件把那张卡片的图标补上（对齐官方「列表先出、
    /// 头像后补」）。下载本身仍在 store（要用 client），只是由命令层驱动节奏。
    pending_avatars: Mutex<Vec<(i64, grammers_client::media::ChatPhoto)>>,
    /// 分片大小。
    ///
    /// 可配置而不是写死常量，这样实测出真实约束后改一处即可，也便于单测。
    chunk: u64,
}

impl Drop for TelegramStore {
    /// 主动停掉后台任务。
    ///
    /// 不这样会怎样：位置被移除后那条连接仍在跑，账号的「已登录设备」里留着
    /// 一条已经没人用的活动会话，而且每次重连都会再多一条。
    fn drop(&mut self) {
        if let Some(r) = self.runner.take() {
            r.abort();
        }
    }
}

impl std::fmt::Debug for TelegramStore {
    /// 不打印 client 与缓存内容：前者没有有意义的 Debug，后者含
    /// `file_reference`（短期凭据）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramStore")
            .field("connected", &self.client.is_some())
            .field(
                "conversations",
                &self.conversations.lock().map(|c| c.len()).unwrap_or(0),
            )
            .field("chunk", &self.chunk)
            .finish_non_exhaustive()
    }
}

/// 把 grammers 的错误翻成本 crate 的错误。
///
/// `FLOOD_WAIT` 单独归到 [`Error::RateLimited`]：上层据此决定「等一下自动重试」
/// 而不是「报错让用户重来」，两者对用户的观感差别很大。
fn map_rpc(e: &InvocationError) -> Error {
    if let InvocationError::Rpc(r) = e {
        if r.name.starts_with("FLOOD_WAIT") {
            return Error::RateLimited;
        }
        if r.code == 401 {
            return Error::Unauthorized;
        }
        if r.code == 403 {
            return Error::Forbidden;
        }
        // 只带错误名与代码，不带服务端 message——那里面可能回显请求参数
        return Error::Protocol(format!("{} ({})", r.name, r.code));
    }
    Error::Network(e.to_string())
}

impl Default for TelegramStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramStore {
    /// 建一个**不带连接**的驱动，只能用于纯逻辑与单测。
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: None,
            runner: None,
            conversations: Mutex::new(Vec::new()),
            peers: Mutex::new(std::collections::HashMap::new()),
            media: Mutex::new(std::collections::HashMap::new()),
            pending_avatars: Mutex::new(Vec::new()),
            chunk: CHUNK,
        }
    }

    /// 用一组已知对话构造（测试与恢复配置用），仍然不带连接。
    #[must_use]
    pub fn with_conversations(conversations: Vec<Conversation>) -> Self {
        Self {
            client: None,
            runner: None,
            conversations: Mutex::new(conversations),
            peers: Mutex::new(std::collections::HashMap::new()),
            media: Mutex::new(std::collections::HashMap::new()),
            pending_avatars: Mutex::new(Vec::new()),
            chunk: CHUNK,
        }
    }

    /// 用一个已登录的客户端构造，可真正发 RPC。
    ///
    /// 只在调用方自己保证后台 runner 活着时用（比如探针）。产品代码走
    /// [`TelegramStore::from_connection`]，那条路把 runner 一并交给 store，
    /// 不必靠调用方记得。
    #[must_use]
    pub fn connected(client: Client) -> Self {
        Self {
            client: Some(client),
            runner: None,
            conversations: Mutex::new(Vec::new()),
            peers: Mutex::new(std::collections::HashMap::new()),
            media: Mutex::new(std::collections::HashMap::new()),
            pending_avatars: Mutex::new(Vec::new()),
            chunk: CHUNK,
        }
    }

    /// 用一条已建立的连接构造，**并接管后台任务的生命周期**。
    ///
    /// 这是产品代码该走的路：runner 交给 store 持有之后，
    /// 「连接必须保持活着」这件事由类型系统保证，不再依赖调用方记得
    /// 别把某个句柄丢掉。
    #[must_use]
    pub fn from_connection(client: Client, runner: tokio::task::JoinHandle<()>) -> Self {
        Self {
            client: Some(client),
            runner: Some(runner),
            conversations: Mutex::new(Vec::new()),
            peers: Mutex::new(std::collections::HashMap::new()),
            media: Mutex::new(std::collections::HashMap::new()),
            pending_avatars: Mutex::new(Vec::new()),
            chunk: CHUNK,
        }
    }

    /// 是不是一个已连接的实例。
    ///
    /// 从配置恢复出来的位置是未连接占位（恢复流程不碰网络，否则应用会卡在
    /// 启动那一刻），要能和真连上的区分开——否则用户点进去只会看到一句
    /// 「尚未登录」，而他明明登录过。
    #[must_use]
    pub const fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// 当前用的分片大小。
    #[must_use]
    pub const fn chunk(&self) -> u64 {
        self.chunk
    }

    /// 取客户端；没有连接时报错而不是静默返回空结果。
    ///
    /// 静默返回空会让界面显示「这个位置是空的」——而真实情况是根本没连上，
    /// 两者给用户的下一步动作完全不同。
    fn client(&self) -> Result<&Client> {
        self.client
            .as_ref()
            .ok_or_else(|| Error::Protocol(String::from("Telegram 尚未登录")))
    }

    /// 找一个对话（从缓存）。
    #[must_use]
    pub fn conversation(&self, chat: i64) -> Option<Conversation> {
        self.conversations
            .lock()
            .ok()?
            .iter()
            .find(|c| c.chat == chat)
            .cloned()
    }

    /// 这个目录（对话）是否开了「受保护内容」。
    ///
    /// 根目录返回 `false`——它不是对话。
    ///
    /// 读的是**对话级**标记。消息级那个在频道保护开启时是 0，
    /// 见本模块文档第 8 条。
    ///
    /// # 为什么查不到要先刷新，而不是直接答 `false`
    ///
    /// 对话表是 `list("")` 填的缓存。用户从配置恢复后**直接进入某个对话**
    /// （比如上次就停在那儿）时它还是空的，这时直接答 `false` 会让那行
    /// 告知在最该出现的时候不出现，而且完全看不出是错的——实测过：冷启动
    /// 问受保护的群得到 `false`，先 `browse` 一次再问才得到 `true`。
    ///
    /// 与本文件里 `list` / `effective_capabilities` 的做法一致：
    /// `conversation(chat)` 为 `None` 时先 `refresh_conversations()`。
    ///
    /// # Errors
    ///
    /// 不返回错误：刷新失败时按「没开保护」处理。这只是一行提示，
    /// 不该因为它让整个目录打不开。
    pub async fn dir_protected(&self, dir_id: &str) -> bool {
        let Ok(chat) = Conversation::parse_dir_id(dir_id) else {
            return false;
        };
        if let Some(c) = self.conversation(chat) {
            return c.protected;
        }
        if self.client.is_some() && self.refresh_conversations().await.is_err() {
            return false;
        }
        self.conversation(chat).is_some_and(|c| c.protected)
    }

    /// 已缓存的对话数，供界面显示「已加载 N 个对话」。
    #[must_use]
    pub fn conversation_count(&self) -> usize {
        self.conversations.lock().map(|c| c.len()).unwrap_or(0)
    }

    /// 拉取对话列表并刷新缓存。
    ///
    /// # 分页终止条件
    ///
    /// **看「这一轮取到的条数」，不看服务端给的总数。** 实测：服务端只在还没
    /// 取完时给总数，取完返回 `None`。把 `None` 当 0 会少一页（对话凭空消失），
    /// 当「未知」会永远以为还有下一页。这里用 grammers 的迭代器，它内部已经
    /// 按这个语义处理，我们只要不自己再用 count 做判断。
    ///
    /// # Errors
    ///
    /// 网络失败、限流、未登录时返回。
    async fn refresh_conversations(&self) -> Result<Vec<Conversation>> {
        let client = self.client()?;
        let mut it = client.iter_dialogs();
        let mut out = Vec::new();
        let mut peers = std::collections::HashMap::new();
        // 待下载的头像：(对话号, 头像定位)。先攒着，循环结束后交给命令层后台下。
        // 用 chat_id 而不是下标：下载在别处异步做，那时要按对话号回填。
        let mut avatar_jobs: Vec<(i64, grammers_client::media::ChatPhoto)> = Vec::new();
        // 上一次已经下好的头像，按 chat id 索引。头像基本不变，重复 refresh
        // （每次 browse 根目录都会 refresh）不该把 72 张头像再下一遍——那是
        // 「二次进入还要等 7 秒」的来源。命中缓存的直接复用，只有新对话才下。
        let prev_avatars: std::collections::HashMap<i64, Vec<u8>> = self
            .conversations
            .lock()
            .map(|c| {
                c.iter()
                    .filter_map(|conv| conv.avatar.clone().map(|a| (conv.chat, a)))
                    .collect()
            })
            .unwrap_or_default();

        loop {
            let d = match it.next().await {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => return Err(map_rpc(&e)),
            };
            let peer = d.peer();
            let title = peer.name().unwrap_or("(无标题)").to_string();
            // 跳过已迁移走的旧基础群。
            //
            // 基础群升级成超级群后，旧的那条**仍然留在对话列表里**
            // （官方客户端会把它藏起来），带着 deactivated=true 和一个
            // migrated_to 指向新群。它是个空壳：消息都在新群里，
            // 而往它发任何东西都会被服务端拒成 PEER_ID_INVALID (400)。
            //
            // 不列出来，而不是列成只读：它连「看」的价值都没有。列出来的
            // 后果是用户看到两个同名群，点进去一个是空的、往里传文件必然
            // 失败，而错误只是一句 PEER_ID_INVALID，指不到「这个群升级过了」。
            //
            // 实测确认：用户账号里那两个同名 omytest 正是同一个群迁移前后的
            // 两条记录，不是两个群。
            if is_migrated_away(peer) {
                continue;
            }

            // 用 bot_api_dialog_id 而**不是** bare_id 当对话号。
            // bare_id 去掉了类型标记，于是 id 为 123 的用户和 id 为 123 的群
            // 会得到同一个 "tg:123"——而「对话即目录」的前提正是目录 id 唯一，
            // 撞了就会出现「点进 A 群却看到 B 私聊的文件」。
            let Some(chat_id) = peer.id().bot_api_dialog_id() else {
                // 只有「自己」这个特殊 peer 会没有 id，跳过即可
                continue;
            };
            // 频道默认只读：只有管理员能发，而判断管理员要额外请求。
            // 拿不到权限时按**不能**处理——猜「能」会让界面点亮一个
            // 点了才失败的按钮，猜「不能」只是少一个入口，代价不对称。
            let is_channel = matches!(peer, grammers_client::peer::Peer::Channel(_));
            let (can_send, can_delete) = if is_channel {
                (false, false)
            } else {
                (true, false)
            };
            // grammers 把「超级群」也归进 Peer::Group，只有真正的广播频道
            // 才是 Peer::Channel——正好是我们要区分的那条线
            let broadcast = is_channel;
            if let Ok(Some(r)) = peer.to_ref().await {
                peers.insert(chat_id, r);
            }
            // 从**对话**上读受保护标记。消息级的那个在频道保护开启时是 0，
            // 见本模块文档第 8 条
            let protected = match peer {
                grammers_client::peer::Peer::Channel(ch) => ch.raw.noforwards,
                grammers_client::peer::Peer::Group(g) => match &g.raw {
                    grammers_client::tl::enums::Chat::Chat(c) => c.noforwards,
                    // 超级群在协议里也是 Channel
                    grammers_client::tl::enums::Chat::Channel(c) => c.noforwards,
                    _ => false,
                },
                // 私聊没有这个概念
                grammers_client::peer::Peer::User(_) => false,
            };
            let kind = match peer {
                grammers_client::peer::Peer::User(_) => "user",
                grammers_client::peer::Peer::Group(_) => "group",
                grammers_client::peer::Peer::Channel(_) => "channel",
            };
            // 头像**先不下载**，只把它的定位信息记下来，等对话列表本身
            // 收齐后再并发下。
            //
            // 为什么改：实测 72 个对话的账号，`refresh_conversations` 要 48 秒
            // ——几乎全花在这里逐个 `download_bytes` 上（每个头像一次经代理
            // 的完整往返，约 0.67s × 72）。头像取小尺寸也没用，慢的是**串行**
            // 与**次数**，不是大小。对齐官方客户端（DEC-26）「列表先出、媒体
            // 后补」：对话列表不该被头像下载卡住。
            //
            // `peer.photo(false)` 本身只查 session、不下载，放在循环里没问题；
            // 真正的下载挪到下面并发做。
            // 头像：先看上次有没有下过这个对话的。有就直接复用（省一次往返），
            // 没有才在下面并发下。photo(false) 只查 session、不下载
            let cached_avatar = prev_avatars.get(&chat_id).cloned();
            if cached_avatar.is_none() {
                if let Some(p) = peer.photo(false).await.ok().flatten() {
                    avatar_jobs.push((chat_id, p));
                }
            }
            out.push(Conversation {
                chat: chat_id,
                title,
                can_send,
                can_delete,
                broadcast,
                kind,
                // 命中缓存的直接带上，未命中的先留空、下面并发填
                avatar: cached_avatar,
                protected,
            });
        }

        // **不在这里下载头像**：那会让对话列表等所有头像到手才返回（72 个约
        // 11s，就是「冷启动很慢」）。改成把待下任务攒起来，先把对话列表返回，
        // 头像交给命令层在后台逐个下、下完一个补一个（见
        // [`Self::take_pending_avatars`] / [`Self::download_avatar`]）。
        if let Ok(mut pa) = self.pending_avatars.lock() {
            *pa = avatar_jobs;
        }

        if let Ok(mut c) = self.conversations.lock() {
            c.clone_from(&out);
        }
        if let Ok(mut p) = self.peers.lock() {
            *p = peers;
        }
        Ok(out)
    }

    /// 取走上一次 refresh 攒下的待下载头像任务（对话号, 头像定位）。
    ///
    /// 命令层在 `list("")` 返回后调它，拿到任务在后台逐个下载、下完一个就把
    /// 那张卡片的图标补上。取走即清空：避免同一批被重复下。
    #[must_use]
    pub fn take_pending_avatars(&self) -> Vec<(i64, grammers_client::media::ChatPhoto)> {
        self.pending_avatars
            .lock()
            .map(|mut p| std::mem::take(&mut *p))
            .unwrap_or_default()
    }

    /// 下载一张头像的字节。失败返回 `None`（头像是锦上添花，取不到不报错）。
    ///
    /// 单独一个方法而不是让命令层直接碰 client：client 是私有的，且下载要走
    /// 同一条连接。命令层并发调用多次即可（自己限流）。
    pub async fn download_avatar(
        &self,
        photo: &grammers_client::media::ChatPhoto,
    ) -> Option<Vec<u8>> {
        let client = self.client.as_ref()?;
        download_bytes(client, photo).await
    }

    /// 把下好的头像写回对话缓存，让下次 refresh 能命中缓存、不再重下。
    ///
    /// 找不到该对话（用户已离开、列表已变）就静默忽略——那张头像本就没人要了。
    pub fn set_conversation_avatar(&self, chat: i64, avatar: Option<Vec<u8>>) {
        if let Ok(mut c) = self.conversations.lock() {
            if let Some(conv) = c.iter_mut().find(|c| c.chat == chat) {
                conv.avatar = avatar;
            }
        }
    }

    /// 取访问某个对话所需的引用。
    ///
    /// 缓存里没有就重新列一次对话——那个 hash 只能从对话列表里拿到，
    /// 而没有它任何针对该对话的请求都发不出去。
    async fn peer_ref(&self, chat: i64) -> Result<PeerRef> {
        if let Some(r) = self.peers.lock().ok().and_then(|p| p.get(&chat).copied()) {
            return Ok(r);
        }
        self.refresh_conversations().await?;
        self.peers
            .lock()
            .ok()
            .and_then(|p| p.get(&chat).copied())
            .ok_or_else(|| Error::NotFound(format!("未知对话：tg:{chat}")))
    }

    /// 列一个对话里带文件的消息。
    ///
    /// # 为什么用 `getHistory` 而不是 `messages.search`
    ///
    /// **实测坐实的坑**：文件视图原来走 `messages.search` + `filterEmpty`，在很多
    /// 超级群里返回 0 条或只返回极少数，而同一个群用 `getHistory`（消息视图那
    /// 条路）能取到全部。真实账号数据：「ShuMale 二十元店」文件视图 0、消息
    /// 视图 52 个带文件；「胖熊零导」文件视图 5、消息视图 67——`search` 漏掉了
    /// 绝大多数。表现是「进群一个文件都看不到（或只看到几个），可它们明明
    /// 都在」，而且不报错。
    ///
    /// `filterEmpty` 名义上不过滤，但 `messages.search` 对某些对话/服务端分片
    /// 本就不可靠（Telegram 侧的已知行为）。改用 `getHistory` 拉全部消息、
    /// 再在本地按「有没有可下载的媒体」（`to_raw_input_location()` 返回 Some）
    /// 筛——与消息视图用的完全同一条路径、同一个判据，两个视图不再各走各的。
    ///
    /// # 为什么判据是「能不能下载」而不是 Media 变体
    ///
    /// 变体不可靠：一条 `video/mp4` 的 Media 类型是 `Document` 却按发送方式归到
    /// `Video`；按变体列举会在 grammers 加新变体时静默漏掉。`to_raw_input_location()`
    /// 返回 Some 才是「这东西真能取字节」的真实判据，投票/位置/联系人自然返回
    /// None 被过滤掉。
    async fn list_messages(&self, chat: i64, limit: usize) -> Result<Vec<Entry>> {
        self.list_messages_before(chat, limit, None).await
    }

    /// 同 [`Self::list_messages`]，但能从某条消息之前继续取。
    ///
    /// # 为什么需要分页
    ///
    /// 固定拉一页的话，活跃群里**第 N+1 个文件之后永远看不到，而且界面上
    /// 没有任何迹象**——用户会以为那些文件不在这个群里。
    ///
    /// `before` 用消息号而不是日期：同一秒可能有多条，按日期翻页会重复或
    /// 漏掉（与消息视图那条同源）。
    async fn list_messages_before(
        &self,
        chat: i64,
        limit: usize,
        before: Option<i32>,
    ) -> Result<Vec<Entry>> {
        let client = self.client()?;
        let peer = self.peer_ref(chat).await?;

        // 用 getHistory（iter_messages）而不是 messages.search：见 list_messages
        // 的文档，search 在很多超级群里会漏掉绝大多数文件。
        //
        // **扫到够 limit 个文件为止，而不是只看 limit 条消息**——这是「进群
        // 一个文件都看不到」的真正根因：实测「ShuMale 二十元店」最新 15 条
        // 全是文本公告，52 个文件都在更靠后，只看 15 条就得到 0。所以这里不
        // 给迭代器设 .limit()，一直往回扫、把带媒体的收进结果，直到凑够 limit
        // 个文件，或扫过 MAX_SCAN 条消息（防止在纯聊天群里无限翻）。
        //
        // 返回的最后一个文件条目的消息号供上层作下一页的 before，翻页语义不变。
        let mut it = client.iter_messages(peer);
        if let Some(off) = before {
            it = it.offset_id(off);
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut scanned = 0usize;
        loop {
            let msg = match it.next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => return Err(map_rpc(&e)),
            };
            scanned += 1;
            // 收一条（collect_media 会顺带缓存下载位置）；没媒体的返回空
            let mut got = self.collect_media(vec![msg], chat);
            entries.append(&mut got);
            if entries.len() >= limit || scanned >= MAX_MESSAGE_SCAN {
                break;
            }
        }
        Ok(entries)
    }

    /// 列一个对话里的文件，支持从某条消息之前继续取。
    ///
    /// # Errors
    ///
    /// 未登录、网络失败、限流、对话不存在时返回。
    pub async fn list_more(&self, dir_id: &str, before: Option<i32>) -> Result<Vec<Entry>> {
        let chat = Conversation::parse_dir_id(dir_id)?;
        // 不在这里预先 refresh_conversations 全量拉对话——那是「进对话被全量
        // 对话刷新拖住」的来源。进这个对话只需要**它自己**的 peer 引用，
        // 而 peer_ref（list_messages_before 内部会调）在缓存未命中时会自己
        // 补，缺的只是这一个对话所需的引用。
        //
        // 正常路径（先浏览了根目录再进对话）peers 缓存已命中，一次网络都不多。
        // 冷启动直接进对话时，peer_ref 触发的那次刷新不可避免——访问对话所需
        // 的 access hash 只能从对话列表里拿到。
        self.list_messages_before(chat, DEFAULT_MESSAGE_PAGE, before)
            .await
    }

    /// 一页拉多少条。界面据它判断「还有没有更多」。
    ///
    /// 判据是「取回条数是否等于请求的 limit」，不靠「下一次返回空」——
    /// 那要多发一次必然为空的请求，在限流敏感的 Telegram 上是实打实的浪费。
    #[must_use]
    pub const fn page_size() -> usize {
        DEFAULT_MESSAGE_PAGE
    }

    /// 把一串消息收成文件条目，顺带缓存它们的下载位置。
    ///
    /// `list` 与 `search` 共用这一份。两处各写一份的话，以后给条目加个字段，
    /// 改了一处忘了另一处，表现是「浏览时有这个信息、一搜索就没了」，
    /// 而这种不一致很难被注意到。
    fn collect_media(&self, msgs: Vec<grammers_client::message::Message>, chat: i64) -> Vec<Entry> {
        let mut out = Vec::new();
        let mut cache = Vec::new();
        for msg in msgs {
            let Some(media) = msg.media() else { continue };
            // 判据是「能不能下载」，不是「是不是某个 Media 变体」。
            //
            // 按变体列举会在 grammers 加新变体时静默漏掉（Media 是
            // non_exhaustive 的），而 to_raw_input_location() 返回 Some 就
            // 意味着这东西真能取字节——这才是「它算不算一个文件」的真实判据。
            // 投票、位置、联系人这些自然会返回 None 被过滤掉。
            let Some(loc) = media.to_raw_input_location() else {
                continue;
            };
            let id = TelegramId {
                chat,
                message: msg.id(),
            };
            let key = id.encode();
            let size = media_size(&media);
            cache.push((
                key.clone(),
                CachedMedia {
                    location: loc,
                    size,
                },
            ));
            out.push(Entry {
                id: key,
                name: media_name(&media, msg.id()),
                is_dir: false,
                size: Some(size),
                mtime: None,
                // 不用 file_reference 当 etag：它会过期，拿它做变更检测会让
                // 缓存层误以为文件变了，从而反复重新下载
                etag: None,
                // 内嵌缩略图就在这条消息里，取它不发任何请求
                thumb: embedded_thumb(&media),
            });
        }

        if let Ok(mut m) = self.media.lock() {
            for (k, v) in cache {
                m.insert(k, v);
            }
        }
        out
    }

    /// 列一个对话里的消息（**以文件为主线的消息视图**）。
    ///
    /// # 为什么不覆盖广播频道
    ///
    /// Telegram ToS 3.3 要求：允许访问频道内容的应用必须支持官方 sponsored
    /// messages，且不得干扰该功能。文档 §9.3 记着：**只展示文件时这条适用性
    /// 存疑，而真做消息列表页时它就是硬约束**（§11.2 第 10 项）。
    ///
    /// omy 是文件管理器，不打算实现广告投放与曝光回报（`viewSponsoredMessage`
    /// / `clickSponsoredMessage`）。所以这里**直接不给广播频道提供消息视图**——
    /// 不把它渲染成消息时间线，就不落进 3.3 的字面范围。私聊、群、超级群不在
    /// 这条范围内，照常可用。
    ///
    /// 广播频道的**文件视图**不受影响（那是本期既有行为）。
    ///
    /// 这是个可以被推翻的判断，写在这里而不是埋掉：要改的话，要么实现
    /// sponsored messages 支持，要么重新解读 3.3 的适用范围。
    ///
    /// # 纯文本消息是正常的一行
    ///
    /// 它们没有可下载的媒体，`file_id` 为 `None`。**不要把它们当成「取不到
    /// 位置的文件」而过滤掉或报错**——消息视图里本来就该有它们，
    /// 那正是它与文件视图的区别。
    ///
    /// # Errors
    ///
    /// 未登录、对话不存在、是广播频道、网络失败或限流时返回。
    pub async fn messages(
        &self,
        dir_id: &str,
        limit: usize,
        before: Option<i32>,
    ) -> Result<Vec<MessageRow>> {
        let chat = Conversation::parse_dir_id(dir_id)?;

        // 先判**不随状态变化的事实**，再判依赖运行时状态的。
        //
        // 顺序反了的话（先查有没有连接），同一个广播频道会在断线时说
        // 「尚未登录」、连上后才说「不提供消息视图」——而后者才是真正的原因，
        // 且重连也不会变。用户按前一句去重连，只会白试一次。
        if self.conversation(chat).is_some_and(|c| c.broadcast) {
            return Err(Error::Unsupported(BROADCAST_NO_MESSAGES));
        }

        let client = self.client()?;
        if self.conversation(chat).is_none() {
            self.refresh_conversations().await?;
        }
        let conv = self
            .conversation(chat)
            .ok_or_else(|| Error::NotFound(format!("未知对话：{dir_id}")))?;
        // 刚拉回来的对话表可能才认出它是广播频道，这里再判一次
        if conv.broadcast {
            return Err(Error::Unsupported(BROADCAST_NO_MESSAGES));
        }

        let peer = self.peer_ref(chat).await?;
        // `before` 是「从这条消息之前开始取」。Telegram 的消息号在对话内
        // 单调递增，所以「加载更早」就是拿当前最老那条的号再要一页。
        //
        // 不用 offset_date：同一秒里可能有多条消息，按日期翻页会重复或漏掉。
        let mut it = client.iter_messages(peer).limit(limit);
        if let Some(off) = before {
            it = it.offset_id(off);
        }
        let mut out = Vec::new();
        let mut cache = Vec::new();
        loop {
            let msg = match it.next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => return Err(map_rpc(&e)),
            };
            let id = TelegramId {
                chat,
                message: msg.id(),
            };
            // 有没有可下载的媒体。取不到位置的（投票、位置、纯文本）
            // 一律 file_id=None，而不是被丢掉
            let (file_id, file_name, file_size) = match msg.media() {
                Some(media) => match media.to_raw_input_location() {
                    Some(loc) => {
                        let key = id.encode();
                        let size = media_size(&media);
                        cache.push((
                            key.clone(),
                            CachedMedia {
                                location: loc,
                                size,
                            },
                        ));
                        (
                            Some(key),
                            Some(media_name(&media, msg.id())),
                            Some(size),
                        )
                    }
                    None => (None, None, None),
                },
                None => (None, None, None),
            };
            // 缩略图与时长：用服务端已有的小图，不下载原图再缩。
            // 一屏可能有几十条消息，拉原图既慢又费流量
            let (thumb, duration) = match msg.media() {
                Some(m) => (thumb_bytes(client, &m).await, media_duration(&m)),
                None => (None, None),
            };
            out.push(MessageRow {
                message: msg.id(),
                text: msg.text().to_string(),
                date: msg.date().timestamp(),
                outgoing: msg.outgoing(),
                file_id,
                file_name,
                file_size,
                thumb,
                duration,
            });
        }
        // 顺带把下载位置也缓存了：用户多半会从消息视图直接点开那个文件，
        // 不缓存的话又要为此重列一次
        if let Ok(mut m) = self.media.lock() {
            for (k, v) in cache {
                m.insert(k, v);
            }
        }
        Ok(out)
    }

    /// 取一个文件的下载位置；缓存里没有就重新列一次那个对话。
    ///
    /// 会重新列是因为 `file_reference` 有时效：过期后必须靠重新拉消息换新的，
    /// 这正是 id 用 `(对话, 消息号)` 而不是用引用本身的原因。
    async fn locate(&self, id: &TelegramId) -> Result<CachedMedia> {
        let key = id.encode();
        if let Some(m) = self.media.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(m);
        }
        self.list_messages(id.chat, DEFAULT_MESSAGE_PAGE).await?;
        self.media
            .lock()
            .ok()
            .and_then(|m| m.get(&key).cloned())
            .ok_or_else(|| Error::NotFound(format!("找不到消息：{key}")))
    }
}

impl RemoteStore for TelegramStore {
    /// 位置级**上界**：所有对话里最宽松的那种情况。
    ///
    /// 用 `conversation_writable(true, true)` 而不是 `cloud_writable()`，
    /// 因为后者会声明 `rename` 与 `create_dir`，而这两个在 Telegram 下没有
    /// 真实语义（见模块文档）。声明了就等于让界面把按钮点亮、用户点了才失败。
    fn capabilities(&self) -> Capabilities {
        Capabilities::conversation_writable(true, true)
    }

    /// 目录级有效能力：取决于这个对话里当前账号的权限。
    ///
    /// 查不到对话时**报错而不是回落到上界**。「没查到」不等于「有权限」，
    /// 而两种猜错的代价不对称：猜有权限会让用户点了才失败，
    /// 猜没权限只是少一个入口。
    async fn effective_capabilities(&self, dir_id: &str) -> Result<Capabilities> {
        // 根目录就是对话列表本身：它不接受任何写操作——
        // 「在根目录新建」等于「建一个群」，见模块文档。
        //
        // 但它**支持搜索**，而且正是跨对话搜索的入口，所以不能直接用
        // `read_only()`——那个构造器把 search 置成 false（对本地目录与网盘
        // 是对的）。用它的后果是：界面按能力位图决定分段控件出不出现，
        // 于是在最需要「搜索整个账号」的那一屏，控件反而消失了；
        // 而搜索本身是好的，从后端完全看不出问题。
        if dir_id.is_empty() {
            return Ok(Capabilities {
                search: true,
                ..Capabilities::read_only()
            });
        }
        let chat = Conversation::parse_dir_id(dir_id)?;
        self.conversation(chat)
            .map(|c| c.capabilities())
            .ok_or_else(|| Error::NotFound(format!("未知对话：{dir_id}")))
    }

    fn describe(&self) -> String {
        String::from("Telegram")
    }

    /// 列目录。
    ///
    /// `dir_id` 为空 → 列对话；否则 → 列该对话里带文件的消息。
    ///
    /// # 为什么不用 `resolve_username` 找对话
    ///
    /// 它**只认有公开用户名的对话**，而私有群、私聊、收藏夹都没有——那恰恰是
    /// 用户最可能存文件的地方。实测踩过：一个存在的私有群被报成「找不到」。
    /// 所以寻址一律基于对话枚举。
    ///
    /// # Errors
    ///
    /// 未登录、网络失败、限流、对话不存在时返回。
    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>> {
        if dir_id.is_empty() {
            // 没有连接时退回缓存（纯逻辑实例与单测走这条）
            let convs = if self.client.is_some() {
                self.refresh_conversations().await?
            } else {
                self.conversations.lock().map(|c| c.clone()).unwrap_or_default()
            };
            return Ok(convs
                .iter()
                .map(|c| Entry {
                    id: c.dir_id(),
                    name: c.title.clone(),
                    is_dir: true,
                    size: None,
                    // 对话没有「修改时间」这个概念上等价的东西。
                    // 硬塞最后一条消息的时间会让缓存层误以为能检测变化。
                    mtime: None,
                    etag: None,
                    // 对话的「缩略图」就是它的头像。复用这条既有通道而不是
                    // 另加字段：GUI 那边 thumb -> thumb_token -> <img> 已经
                    // 打通，多一条并行路径只会多一处会不一致的地方。
                    //
                    // 没有头像时为 None，界面回落到按 kind 给的类型图标——
                    // 对话不是文件夹，不该显示 📁
                    thumb: c.avatar.clone(),
                })
                .collect());
        }
        // 校验 dir_id 形状：早报错好过带着一个坏 id 去发请求
        let chat = Conversation::parse_dir_id(dir_id)?;
        if self.client.is_none() {
            // 纯逻辑实例：保持原来的行为，让单测能区分「对话不存在」与「没接 RPC」
            if self.conversation(chat).is_none() {
                return Err(Error::NotFound(format!("未知对话：{dir_id}")));
            }
            return Err(Error::Unsupported("telegram list messages"));
        }
        // 对话表可能还没拉过（比如从配置恢复后直接进目录），先确保有
        if self.conversation(chat).is_none() {
            self.refresh_conversations().await?;
        }
        if self.conversation(chat).is_none() {
            return Err(Error::NotFound(format!("未知对话：{dir_id}")));
        }
        self.list_messages(chat, DEFAULT_MESSAGE_PAGE).await
    }

    /// 服务端搜索。
    ///
    /// # 搜索词会离开本机
    ///
    /// 这个方法把 `query` 发给 Telegram 服务器。调用方**必须**已经就此告知
    /// 用户——原型 §5 把它设计成需要显式切换的模式，而不是默认行为，
    /// 正是因为「搜了什么」本身就是敏感信息。
    ///
    /// # 返回候选集，不是最终结果
    ///
    /// 服务端搜的是消息文字与说明。omy 加密文件的真实文件名服务端**永远没有**
    /// ——那是加密掉的东西，上传它等于白加密，还会让用户以为自己是安全的。
    /// 所以结果要在本地按解出来的真实文件名再精筛一轮。
    ///
    /// # Errors
    ///
    /// 未登录、网络失败、限流、对话不存在时返回。
    async fn search(&self, dir_id: &str, query: &str, limit: usize) -> Result<Vec<Entry>> {
        let client = self.client()?;
        if query.is_empty() {
            // 空词不发请求：服务端会把它当成「列全部」，白占一次限流配额，
            // 而调用方要的显然不是这个
            return Ok(Vec::new());
        }
        // 根目录搜索要跨所有对话。实测过的六种过滤器都是**对话内**的，
        // 没有「全局搜文件」那种东西，所以只能逐个对话搜再合并。
        let chats: Vec<i64> = if dir_id.is_empty() {
            if self.conversation_count() == 0 {
                self.refresh_conversations().await?;
            }
            self.conversations
                .lock()
                .map(|c| c.iter().map(|c| c.chat).collect())
                .unwrap_or_default()
        } else {
            vec![Conversation::parse_dir_id(dir_id)?]
        };

        let mut out = Vec::new();
        for chat in chats {
            // 一个对话失败不该让整次搜索失败：跨对话搜索时，某个频道可能刚好
            // 没权限或被限流，因此丢掉其余全部结果是不划算的
            let Ok(peer) = self.peer_ref(chat).await else {
                continue;
            };
            let mut it = client
                .search_messages(peer)
                .query(query)
                // 与 list 一样用 Empty：Document 过滤器不等于「所有文档」，
                // 实测一条 video/mp4 的 Document 不被它命中（见 list_messages）。
                // 这里是**带 query 的服务端搜索**，用 search 是对的（不是浏览
                // 那条无 query 的路，那条已改回 getHistory）。
                .filter(tl::enums::MessagesFilter::InputMessagesFilterEmpty)
                .limit(limit);
            let mut msgs = Vec::new();
            let ok = loop {
                match it.next().await {
                    Ok(Some(m)) => msgs.push(m),
                    Ok(None) => break true,
                    Err(e) if map_rpc(&e).is_retryable() => return Err(map_rpc(&e)),
                    // 非暂时性错误（没权限之类）就跳过这个对话，继续搜别的
                    Err(_) => break false,
                }
            };
            if ok {
                out.append(&mut self.collect_media(msgs, chat));
            }
            if out.len() >= limit {
                out.truncate(limit);
                break;
            }
        }
        Ok(out)
    }

    /// 上传一个文件到某个对话。
    ///
    /// # 一定要「作为文件发送」，不能作为照片或视频
    ///
    /// 用 `InputMessage::file()` 而不是 `.photo()` / `.document()` 的
    /// 自动判别：作为照片发送时**服务端会重新编码**（压缩、改尺寸、剥元数据）。
    /// 对普通图片那只是变糊，对 omy 加密文件是毁灭性的——字节一变，
    /// 解出来就是一堆认证失败，而那个症状指向密钥、完全指不到「上传方式」。
    ///
    /// 代价是发出去的东西在官方客户端里显示为文件而不是图片预览。
    /// 这个取舍是明确的：omy 的文件本来就该原样存取。
    ///
    /// # Errors
    ///
    /// 未登录、对话不存在、没有发送权限、网络失败或被限流时返回。
    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> Result<Entry> {
        let client = self.client()?;
        let chat = Conversation::parse_dir_id(dir_id)?;

        // 先查权限：能力位图说不能发就当场拒绝，别把几 MB 发出去再被拒
        let caps = self.effective_capabilities(dir_id).await?;
        if !caps.write {
            return Err(Error::Forbidden);
        }

        let peer = self.peer_ref(chat).await?;
        let mut cursor = std::io::Cursor::new(data);
        let uploaded = client
            .upload_stream(&mut cursor, data.len(), name.to_string())
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let msg = client
            .send_message(peer, InputMessage::new().file(uploaded))
            .await
            .map_err(|e| map_rpc(&e))?;

        let id = TelegramId {
            chat,
            message: msg.id(),
        };
        let key = id.encode();
        // 顺手把下载位置记进缓存：刚传完的文件多半马上就要被读
        // （上层要校验、界面要出缩略图），没有的话又得重列一次消息
        if let Some(media) = msg.media() {
            if let Some(loc) = media.to_raw_input_location() {
                if let Ok(mut m) = self.media.lock() {
                    m.insert(
                        key.clone(),
                        CachedMedia {
                            location: loc,
                            size: media_size(&media),
                        },
                    );
                }
            }
        }

        Ok(Entry {
            id: key,
            name: String::from(name),
            is_dir: false,
            size: Some(data.len() as u64),
            mtime: None,
            etag: None,
            // 刚上传完的文件，服务端还没回缩略图；下次列目录时会有
            thumb: None,
        })
    }

    /// 删除一条消息。
    ///
    /// # Errors
    ///
    /// 未登录、没有删除权限、消息不存在时返回。
    async fn delete(&self, id: &str) -> Result<()> {
        let client = self.client()?;
        let tid = TelegramId::decode(id)?;
        let dir = format!("tg:{}", tid.chat);
        let caps = self.effective_capabilities(&dir).await?;
        if !caps.delete {
            return Err(Error::Forbidden);
        }
        let peer = self.peer_ref(tid.chat).await?;
        client
            .delete_messages(peer, &[tid.message])
            .await
            .map_err(|e| map_rpc(&e))?;
        // 缓存里的下载位置要跟着失效，否则之后还能「读到」一个已经删掉的文件
        if let Ok(mut m) = self.media.lock() {
            m.remove(id);
        }
        Ok(())
    }

    /// 读取区间。
    ///
    /// # 按实测约束发请求（见 [`CHUNK`] 上的表）
    ///
    /// - `limit` 必须能整除 1 MiB。实测 `limit=1024` 会被拒 `LIMIT_INVALID`，
    ///   所以「1 KiB 的倍数即可」那条流传说法**不能**照搬。
    /// - `offset` 必须是 `limit` 的倍数，否则 `OFFSET_INVALID`。这正是
    ///   [`plan_chunks`] 存在的理由。
    /// - **越过文件尾返回 0 字节、不报错**，所以读到空片是正常的结束信号。
    ///   把它当失败会让每个文件的最后一片都报错。
    ///
    /// 拼好分片后**必须**按 `plan.skip` / `plan.take` 裁剪：`RemoteStore` 的
    /// 契约要求实现方自行裁剪，而我们因为对齐本来就会多取——不裁的话上层会把
    /// 多出来的字节当成密文的一部分，解出来是一堆认证失败，
    /// 而那个症状指向密钥、完全指不到偏移。
    ///
    /// # Errors
    ///
    /// 坏 id、未登录、网络失败、限流时返回。
    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        // 先解 id：坏 id 要在发请求之前就报出来
        let tid = TelegramId::decode(id)?;
        if len == 0 {
            // 0 长度不该发任何请求：白占一次限流配额
            return Ok(Vec::new());
        }
        let client = self.client()?;
        let media = self.locate(&tid).await?;
        // 收敛到文件尾：越界的部分发出去也只会拿回 0 字节，白花配额
        let len = clamp_to_eof(offset, len, media.size);
        if len == 0 {
            return Ok(Vec::new());
        }
        let plan = plan_chunks(offset, len, self.chunk);

        let limit = i32::try_from(self.chunk)
            .map_err(|_| Error::Protocol(String::from("分片大小超出 i32")))?;
        let mut buf = Vec::with_capacity(
            usize::try_from(plan.count.saturating_mul(self.chunk)).unwrap_or(0),
        );
        for i in 0..plan.count {
            let at = plan.start.saturating_add(i.saturating_mul(self.chunk));
            let req = tl::functions::upload::GetFile {
                precise: false,
                // 明确不支持 CDN 重定向：grammers 在那条分支上 panic，
                // 而 omy-gui 禁 panic。要支持得自己实现 CDN 下载，本期不做
                cdn_supported: false,
                location: media.location.clone(),
                offset: i64::try_from(at)
                    .map_err(|_| Error::Protocol(String::from("偏移超出 i64")))?,
                limit,
            };
            match client.invoke(&req).await {
                Ok(tl::enums::upload::File::File(f)) => {
                    let n = f.bytes.len();
                    buf.extend_from_slice(&f.bytes);
                    // 短片或空片 = 到文件尾了。实测越过尾部返回 0 字节而不报错，
                    // 所以这是正常的结束信号，继续请求只会白发
                    if n < usize::try_from(self.chunk).unwrap_or(usize::MAX) {
                        break;
                    }
                }
                Ok(tl::enums::upload::File::CdnRedirect(_)) => {
                    return Err(Error::Unsupported(CDN_REDIRECT));
                }
                Err(e) => return Err(map_rpc(&e)),
            }
        }

        // 按计划裁剪：对齐让我们从更早的位置开始取，多出来的必须切掉
        let skip = usize::try_from(plan.skip).unwrap_or(usize::MAX);
        let take = usize::try_from(plan.take).unwrap_or(usize::MAX);
        if skip >= buf.len() {
            // 整段都在文件尾之外。返回空而不是报错——调用方据此知道读到头了
            return Ok(Vec::new());
        }
        let end = skip.saturating_add(take).min(buf.len());
        Ok(buf.get(skip..end).unwrap_or_default().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(chat: i64, title: &str, send: bool, del: bool) -> Conversation {
        Conversation {
            chat,
            title: String::from(title),
            can_send: send,
            can_delete: del,
            broadcast: false,
            kind: "group",
            avatar: None,
            protected: false,
        }
    }

    /// id 必须能原样往返。
    ///
    /// 不这样会怎样：id 是缓存键的一部分，编解码不一致会让同一个文件在缓存里
    /// 存成两份，或者取的时候找不到——表现为「明明缓存过还是重新下载」。
    #[test]
    fn id_round_trips() {
        for (chat, message) in [
            (1i64, 1i32),
            (-1001234567890, 42),
            (i64::MIN, i32::MIN),
            (i64::MAX, i32::MAX),
        ] {
            let id = TelegramId { chat, message };
            let s = id.encode();
            assert_eq!(TelegramId::decode(&s).expect("应能解回"), id, "id={s}");
        }
    }

    /// 坏 id 必须报错，不能猜一个值出来。
    ///
    /// 不这样会怎样：猜出来的 id 会指向另一条消息，于是用户点开 A 却下载到 B；
    /// 而如果猜成 0，错误会在更远的地方以「消息不存在」冒出来，根本查不到源头。
    #[test]
    fn malformed_ids_are_refused() {
        for bad in [
            "",
            "tg:",
            "tg:1",
            "tg:a:1",
            "tg:1:b",
            "tg:1:2:3",
            "dav:/path/to/file",
            "1:2",
        ] {
            assert!(
                TelegramId::decode(bad).is_err(),
                "{bad:?} 应当被拒绝而不是猜一个值"
            );
        }
    }

    /// 目录 id 与文件 id 不能互相解错。
    ///
    /// 不这样会怎样：`tg:123` 是目录、`tg:123:456` 是文件，如果目录 id 能被
    /// 当成文件 id 解开，「进目录」会变成「下载一个不存在的文件」。
    #[test]
    fn dir_ids_and_file_ids_do_not_collide() {
        let c = conv(123, "群", true, false);
        let dir = c.dir_id();
        assert_eq!(dir, "tg:123");
        assert!(
            TelegramId::decode(&dir).is_err(),
            "目录 id 不该能当文件 id 解开"
        );

        let file = TelegramId { chat: 123, message: 456 }.encode();
        assert!(
            Conversation::parse_dir_id(&file).is_err(),
            "文件 id 不该能当目录 id 解开"
        );
        assert_eq!(Conversation::parse_dir_id(&dir).expect("应能解"), 123);
    }

    /// 分页终止条件必须看「返回条数 < 请求 limit」，不能看服务端给的总数。
    ///
    /// 不这样会怎样：实测发现服务端只在未取完时给总数、取完给 None。按总数判断，
    /// 把 None 当 0 会少一页（对话凭空消失），当「未知」会永远以为还有下一页
    /// （翻页翻不到头）。两种症状都不指向真正原因。
    #[test]
    fn pagination_stops_on_a_short_page() {
        // 满页 → 还有更多
        assert!(has_more_dialogs(20, 20));
        assert!(has_more_dialogs(100, 100));
        // 不满 → 到头了。实测就是 limit=20/100/500 都只回 4 条
        assert!(!has_more_dialogs(4, 20));
        assert!(!has_more_dialogs(4, 100));
        assert!(!has_more_dialogs(4, 500));
        // 一条都没有 → 显然到头
        assert!(!has_more_dialogs(0, 20));
        // 边界：只请求 1 条且真回了 1 条，必须继续——实测 limit=1 时确实还有更多
        assert!(has_more_dialogs(1, 1));
    }

    /// 实测钉下来的分片约束必须被常量如实反映。
    ///
    /// 不这样会怎样：这几个数字是花了一次真实账号的实测才拿到的。写错任何一个
    /// 都会让运行时收到 `LIMIT_INVALID` / `OFFSET_INVALID`——而那两个错误看起来
    /// 像「服务端出问题了」，没人会想到是自己的常量不对。
    #[test]
    fn chunk_constants_match_what_the_server_accepts() {
        // 实测接受的
        assert!(chunk_is_valid(4 * 1024), "4 KiB 实测接受");
        assert!(chunk_is_valid(512 * 1024), "512 KiB 实测接受");
        assert!(chunk_is_valid(1024 * 1024), "1 MiB 实测接受（上限）");
        // 默认值必须在接受范围内，否则第一次下载就报 400
        assert!(chunk_is_valid(CHUNK), "默认分片大小必须是服务端接受的");

        // 实测被拒的
        assert!(
            !chunk_is_valid(1024),
            "1 KiB 实测被拒（LIMIT_INVALID）——文档说「1 KiB 的倍数即可」是错的"
        );
        assert!(!chunk_is_valid(1000), "1000 不是 1 KiB 的倍数，实测被拒");
        assert!(
            !chunk_is_valid(3072),
            "3 KiB 是 1 KiB 的倍数但不整除 1 MiB，实测被拒"
        );
        assert!(
            !chunk_is_valid(1024 * 1024 + 1024),
            "超过 1 MiB 实测被拒"
        );
    }

    /// 分片计划算出来的每一个 offset 都必须是服务端接受的。
    ///
    /// 不这样会怎样：实测 `offset=1` 直接被拒（OFFSET_INVALID），服务端要求
    /// offset 是 limit 的倍数。计划里只要有一个没对齐的 offset，那一片就取不
    /// 回来——而症状是解密报「认证失败」，完全指不到偏移这一步。
    #[test]
    fn every_planned_offset_is_aligned() {
        for (offset, len) in [(0u64, 1u64), (1, 1), (5000, 3000), (1_048_575, 4097)] {
            let p = plan_chunks(offset, len, CHUNK);
            for i in 0..p.count {
                let off = p.start + i * CHUNK;
                assert_eq!(
                    off % CHUNK,
                    0,
                    "第 {i} 片的 offset {off} 不是 limit 的倍数，服务端会回 OFFSET_INVALID"
                );
            }
        }
    }

    /// 对齐不能丢掉请求区间的任何一个字节。
    ///
    /// 不这样会怎样：少算一片会让 seek 后的数据短一截，而解密层只报「认证
    /// 失败」——看起来像密钥问题，跟偏移算错完全联系不起来，极难定位。
    #[test]
    fn chunk_plan_covers_the_whole_request() {
        let chunk = 1024u64;
        for (offset, len) in [
            (0u64, 1u64),
            (0, 1024),
            (0, 1025),
            (1, 1),
            (1023, 1),
            (1023, 2),
            (1024, 1024),
            (5000, 3000),
            (1_048_575, 4097),
        ] {
            let p = plan_chunks(offset, len, chunk);
            // 起点必须对齐，且不晚于请求的起点
            assert_eq!(p.start % chunk, 0, "起点未对齐 offset={offset}");
            assert!(p.start <= offset);
            // 取回来的范围必须完整覆盖 [offset, offset+len)
            let fetched_end = p.start + p.count * chunk;
            assert!(
                fetched_end >= offset + len,
                "覆盖不足 offset={offset} len={len}: 取到 {fetched_end}, 需要 {}",
                offset + len
            );
            // 裁剪参数必须正好还原出请求的区间
            assert_eq!(p.start + p.skip, offset, "skip 算错 offset={offset}");
            assert_eq!(p.take, len);
            assert!(
                p.skip + p.take <= p.count * chunk,
                "裁剪超出了取回的数据 offset={offset} len={len}"
            );
        }
    }

    /// 长度为 0 时不该发任何请求。
    ///
    /// 不这样会怎样：白发一次网络请求，而 Telegram 有限流——浪费的配额会
    /// 变成真实的等待。
    #[test]
    fn zero_length_plans_no_request() {
        let p = plan_chunks(4096, 0, 1024);
        assert_eq!(p.count, 0, "不该取任何分片");
        assert_eq!(p.take, 0);
    }

    /// chunk 为 0 不能让除法炸掉。
    ///
    /// 不这样会怎样：这个 crate 服务于 GUI，而 GUI 里禁止 panic；
    /// 一次除零会直接带走整个应用。
    #[test]
    fn zero_chunk_does_not_panic() {
        let p = plan_chunks(10, 10, 0);
        assert!(p.count > 0, "退化成 1 字节一片也要能算出来");
        assert_eq!(p.take, 10);
    }

    /// 位置级能力是上界，且不能声明改名与新建目录。
    ///
    /// 不这样会怎样：Telegram 下这两个动作没有真实语义（改名要重发消息、
    /// 新建目录等于建群），声明了界面就会把菜单点亮，用户点下去才失败。
    #[test]
    fn place_caps_exclude_rename_and_mkdir() {
        let s = TelegramStore::new();
        let c = s.capabilities();
        assert!(c.read, "至少要能读");
        assert!(!c.rename, "消息没有可改的路径，不能声明 rename");
        assert!(!c.create_dir, "目录就是对话列表，不能声明 create_dir");
    }

    /// 目录级能力必须反映该对话的真实权限，且只能比上界更窄。
    ///
    /// 不这样会怎样：别人的公开频道只读，若沿用位置级上界，删除菜单照样是亮的
    /// ——用户点了才发现不行，而这种失败看起来像 bug 而非权限。
    #[test]
    fn effective_caps_follow_the_conversation() {
        let s = TelegramStore::with_conversations(vec![
            conv(1, "收藏夹", true, true),
            conv(2, "别人的频道", false, false),
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        let upper = s.capabilities();

        let mine = rt.block_on(s.effective_capabilities("tg:1")).expect("应有结果");
        assert!(mine.write && mine.delete, "自己的地方该能写能删");

        let theirs = rt
            .block_on(s.effective_capabilities("tg:2"))
            .expect("应有结果");
        assert!(!theirs.write && !theirs.delete, "别人的频道只能读");

        // 两者都不能超出上界——这是 narrowed_to 的语义，必须成立
        for c in [mine, theirs] {
            assert_eq!(c, c.narrowed_to(upper), "有效能力不能宽于位置级上界");
        }
    }

    /// 未知对话要报错，不能回落成可写。
    ///
    /// 不这样会怎样：对话表可能过期，「没查到」被当成「有权限」时，界面会把
    /// 写入口点亮，用户操作到一半才失败。两种猜错的代价不对称。
    #[test]
    fn unknown_conversation_errors_instead_of_assuming_writable() {
        let s = TelegramStore::with_conversations(vec![conv(1, "收藏夹", true, true)]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        assert!(matches!(
            rt.block_on(s.effective_capabilities("tg:999")),
            Err(Error::NotFound(_))
        ));
        // 形状本身不对的也要报错，而不是当成根目录
        assert!(rt.block_on(s.effective_capabilities("garbage")).is_err());
    }

    /// 根目录（对话列表）不接受写操作。
    ///
    /// 不这样会怎样：「在根目录新建文件夹」等于「建一个群」，那是有社交后果的
    /// 动作（产生邀请链接、通知联系人），不该藏在右键菜单里被误触。
    #[test]
    fn root_is_read_only() {
        let s = TelegramStore::with_conversations(vec![conv(1, "收藏夹", true, true)]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        let c = rt.block_on(s.effective_capabilities("")).expect("应有结果");
        assert!(!c.any_write(), "根目录不该有任何写能力");
    }

    /// 根目录列出的是对话，且每个都是目录。
    #[test]
    fn root_lists_conversations_as_directories() {
        let s = TelegramStore::with_conversations(vec![
            conv(1, "收藏夹", true, true),
            conv(-100123, "某频道", false, false),
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        let items = rt.block_on(s.list("")).expect("应能列出");
        assert_eq!(items.len(), 2);
        for it in &items {
            assert!(it.is_dir, "对话必须是目录");
            assert!(it.size.is_none(), "对话没有大小");
            // 对话没有等价于「修改时间」的东西。塞最后一条消息的时间会让缓存层
            // 误以为能检测变化，从而错误地复用旧数据。
            assert!(it.mtime.is_none(), "不该编造修改时间");
        }
        assert_eq!(items[0].id, "tg:1");
        assert_eq!(items[1].id, "tg:-100123");
    }

    /// 改名与新建目录必须报 Unsupported，而不是静默成功。
    ///
    /// 不这样会怎样：默认实现若返回 Ok，用户以为改了名字，实际什么也没发生
    /// ——在文件管理器里这会让他以为文件丢了。
    #[test]
    fn rename_and_mkdir_report_unsupported() {
        let s = TelegramStore::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        assert!(matches!(
            rt.block_on(s.rename("tg:1:2", "新名字")),
            Err(Error::Unsupported("rename"))
        ));
        assert!(matches!(
            rt.block_on(s.create_dir("", "新群")),
            Err(Error::Unsupported("create_dir"))
        ));
    }

    /// 坏 id 要在发请求之前就被拒。
    ///
    /// 不这样会怎样：带着一个坏 id 去发请求，错误会变成服务端的
    /// 「消息不存在」，而真正的原因是上层传错了东西——诊断方向完全跑偏。
    #[test]
    fn read_range_validates_id_first() {
        let s = TelegramStore::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        // 坏 id：必须停在**解码**这一步。
        //
        // 刻意断言错误正文而不只是 Error::Protocol：「尚未登录」也是
        // Protocol，只断言变体的话，即使 id 校验被整个拿掉、流程一路走到
        // 「尚未登录」，断言照样通过——变异测试确认过这一点，
        // 那样的断言等于没写。
        assert!(matches!(
            rt.block_on(s.read_range("garbage", 0, 1)),
            Err(Error::Protocol(ref m)) if m.contains("不是 Telegram 的 id")
        ));
        // 好 id：走过了 id 校验，停在「没有连接」上。
        // 两条错误正文不同，才能证明前一条确实是被 id 校验挡下来的
        assert!(matches!(
            rt.block_on(s.read_range("tg:1:2", 0, 1)),
            Err(Error::Protocol(ref m)) if m.contains("尚未登录")
        ));
    }

    /// 消息行的字段名是前端契约，不能随手改。
    ///
    /// 不这样会怎样：字段名是前端直接读的属性名，改了**不会有编译错误**，
    /// 只会让界面上那一列变成空白——和 `caps.rs` 的 `field_names_are_stable`
    /// 要拦的是同一类事。
    #[test]
    fn message_row_field_names_are_stable() {
        let r = MessageRow {
            message: 7,
            text: String::from("hi"),
            date: 1_700_000_000,
            outgoing: true,
            file_id: Some(String::from("tg:1:7")),
            file_name: Some(String::from("a.omy")),
            file_size: Some(42),
            thumb: None,
            duration: None,
        };
        let j = serde_json::to_value(&r).expect("序列化");
        for k in [
            "message",
            "text",
            "date",
            "outgoing",
            "file_id",
            "file_name",
            "file_size",
        ] {
            assert!(j.get(k).is_some(), "字段 {k} 不见了——前端会读到 undefined");
        }
    }

    /// 纯文本消息是正常的一行，不是「取不到位置的文件」。
    ///
    /// 不这样会怎样：筛选逻辑若把「没有下载位置」当成错误或直接过滤掉，
    /// 消息视图里就只剩带文件的消息——那它和文件视图就没区别了，
    /// 而用户要的恰恰是「按时间线看到当时聊了什么」。
    #[test]
    fn text_only_message_is_a_valid_row() {
        let r = MessageRow {
            message: 3,
            text: String::from("只是一句话"),
            date: 1_700_000_000,
            outgoing: false,
            file_id: None,
            file_name: None,
            file_size: None,
            thumb: None,
            duration: None,
        };
        // 没有文件不代表这一行无效：它有文字、有时间、有消息号
        assert!(r.file_id.is_none());
        assert!(!r.text.is_empty());
        assert_eq!(r.message, 3);
    }

    /// 广播频道不提供消息视图，而**私聊与群照常提供**。
    ///
    /// 不这样会怎样：ToS 3.3 要求允许访问频道内容的应用必须支持官方
    /// sponsored messages。omy 不实现广告投放与曝光回报，所以不把广播频道
    /// 渲染成消息时间线——不落进那条的字面范围（文档 §9.3 / §11.2 第 10 项）。
    ///
    /// 这条断言同时守住另一半：**不能因噎废食把所有对话的消息视图都关掉**。
    /// 只断言「频道被拒」的话，一个「永远返回 Unsupported」的实现也能通过。
    #[test]
    fn broadcast_channels_have_no_message_view() {
        let s = TelegramStore::with_conversations(vec![
            conv(1, "群聊", true, true),
            Conversation {
                chat: 2,
                title: String::from("某广播频道"),
                can_send: false,
                can_delete: false,
                broadcast: true,
                kind: "channel",
                avatar: None,
                protected: false,
            },
        ]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");

        // 广播频道：必须以「不支持」拒掉，且理由要能被界面区分出来
        assert!(
            matches!(
                rt.block_on(s.messages("tg:2", 10, None)),
                Err(Error::Unsupported(w)) if w == BROADCAST_NO_MESSAGES
            ),
            "广播频道不该有消息视图，且要给出专门的标记而不是笼统的不支持"
        );

        // 普通群：不能被这条规则误伤。没有连接时停在「尚未登录」，
        // 而不是停在「不支持」——两者必须能区分开
        assert!(
            matches!(
                rt.block_on(s.messages("tg:1", 10, None)),
                Err(Error::Protocol(ref m)) if m.contains("尚未登录")
            ),
            "普通群的消息视图不该被广播频道那条规则挡掉"
        );
    }

    /// 广播频道的**文件视图**不受影响。
    ///
    /// 不这样会怎样：为了躲 ToS 3.3 把整个频道都屏蔽掉，会顺手废掉本期已经
    /// 在用的能力——而 3.3 约束的是「消息流」，不是「频道里的文件」。
    #[test]
    fn broadcast_channels_still_list_files() {
        let s = TelegramStore::with_conversations(vec![Conversation {
            chat: 2,
            title: String::from("某广播频道"),
            can_send: false,
            can_delete: false,
            broadcast: true,
            kind: "channel",
            avatar: None,
            protected: false,
        }]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        // 走到「没接 RPC」而不是被拒，说明文件那条路没被这条规则波及
        assert!(matches!(
            rt.block_on(s.list("tg:2")),
            Err(Error::Unsupported("telegram list messages"))
        ));
    }

    /// 根目录必须既只读又可搜。
    ///
    /// 不这样会怎样：根目录是**跨对话搜索**的入口，而通用的 `read_only()`
    /// 把 search 置成 false。界面按这一位决定分段控件出不出现，于是在最需要
    /// 「搜索整个账号」的那一屏控件反而消失——偏偏搜索本身是好的，
    /// 从后端一点异常都看不出来。端到端测试抓到过这个。
    #[test]
    fn root_is_read_only_but_searchable() {
        let s = TelegramStore::with_conversations(vec![conv(1, "收藏夹", true, true)]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        let caps = rt.block_on(s.effective_capabilities("")).expect("取根能力");
        assert!(caps.read, "根目录要能看");
        assert!(caps.search, "根目录是跨对话搜索的入口，必须声明 search");
        assert!(
            !caps.any_write(),
            "根目录不能有写能力——在根目录新建等于建一个群"
        );
    }

    /// 越过文件尾的读取要被收敛掉，而不是照发。
    ///
    /// 不这样会怎样：读文件最后一段时，按分片对齐算出来的请求会伸到文件尾
    /// 之外。服务端对越界请求回 0 字节而**不报错**，所以这个 bug 不会以异常
    /// 的形式暴露——只是每个文件的最后一次读都多发一次白跑的请求，
    /// 在限流敏感的 Telegram 上会累积成真实的卡顿。
    #[test]
    fn clamp_to_eof_trims_past_end() {
        // 完全在范围内：原样返回
        assert_eq!(clamp_to_eof(0, 100, 1000), 100);
        // 跨过尾部：截到尾
        assert_eq!(clamp_to_eof(900, 500, 1000), 100);
        // 正好到尾：不截
        assert_eq!(clamp_to_eof(900, 100, 1000), 100);
        // 起点就在尾之外：一个字节都不该读
        assert_eq!(clamp_to_eof(1000, 100, 1000), 0);
        assert_eq!(clamp_to_eof(5000, 100, 1000), 0);
    }

    /// 大小未知时**不能**收敛。
    ///
    /// 不这样会怎样：文档没给 size 时 size 是 0，若把 0 当成「文件长度为 0」，
    /// 每一次读都会被判成越界返回空——表现是「文件能列出来但内容全是空的」，
    /// 而这个症状会把人引去查解密和缓存，完全指不到这里。
    #[test]
    fn clamp_to_eof_treats_zero_size_as_unknown() {
        assert_eq!(clamp_to_eof(0, 100, 0), 100);
        assert_eq!(clamp_to_eof(9999, 100, 0), 100);
    }

    /// mime 带参数时也要能认出扩展名。
    ///
    /// 不这样会怎样：服务端给的是 `text/plain; charset=utf-8` 这种带参数的
    /// 形式，直接整串去比会一个都匹配不上，于是所有文本文件都被命名成 .bin。
    /// 而 omy 上层按扩展名决定能不能内嵌预览——.bin 会被当成未知类型，
    /// 表现是「明明是文本却不给预览」，症状完全指不到 mime 解析。
    #[test]
    fn ext_for_mime_ignores_parameters() {
        assert_eq!(ext_for_mime("text/plain; charset=utf-8"), Some("txt"));
        assert_eq!(ext_for_mime("video/mp4"), Some("mp4"));
        // 认不出的要如实返回 None，让调用方回落，而不是瞎猜一个
        assert_eq!(ext_for_mime("application/x-whatever"), None);
    }

    /// 扩展名不能带点，否则拼出来会是 `x..mp4`。
    ///
    /// 不这样会怎样：文件名多一个点，按扩展名判类型的地方可能仍然能用，
    /// 但用户看到的是一个明显不对的名字，而且导出到本地后也是错的。
    #[test]
    fn ext_for_mime_returns_bare_extension() {
        for m in ["video/mp4", "image/png", "audio/mpeg", "application/pdf"] {
            let e = ext_for_mime(m).unwrap_or("");
            assert!(!e.starts_with('.'), "{m} 的扩展名不该带点：{e}");
            assert!(!e.is_empty());
        }
    }

    /// 列消息时坏 dir_id 也要先被拒。
    #[test]
    fn list_validates_dir_id_first() {
        let s = TelegramStore::with_conversations(vec![conv(1, "收藏夹", true, true)]);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        assert!(matches!(
            rt.block_on(s.list("garbage")),
            Err(Error::Protocol(_))
        ));
        assert!(matches!(
            rt.block_on(s.list("tg:999")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            rt.block_on(s.list("tg:1")),
            Err(Error::Unsupported("telegram list messages"))
        ));
    }
    /// 残缺的 stripped 载荷必须被挡住，而不是当成一张图交给界面。
    ///
    /// 不这样会怎样：Telegram 的 stripped 缩略图是去掉了标准量化表的残缺
    /// JPEG。它**有正确的 JPEG 魔数**，所以任何「只验魔数」的检查都会放它
    /// 过去；而浏览器拿到它只会得到一张永远加载失败、且不抛错的图——
    /// 界面悄悄回退成类型图标，没人看得出这里坏了。
    ///
    /// 这条断言的要害是「魔数对但段不全 → 必须拒绝」。
    #[test]
    fn incomplete_jpeg_is_rejected_even_though_its_magic_is_right() {
        // 补了 JPEG 头两个字节、但没有量化表/霍夫曼表/EOI 的残缺数据。
        // 用算式生成而不是长串重复字节（AGENTS.md 记过那会让本机安全软件
        // 删掉测试二进制）
        let mut fake = vec![0xFF, 0xD8, 0xFF];
        fake.extend((0..128).map(|i| ((i * 37 + 11) % 251) as u8));

        assert!(
            fake.starts_with(&[0xFF, 0xD8, 0xFF]),
            "夹具本身要能通过魔数检查，否则这条断言就没在测该测的东西"
        );
        assert!(
            !is_complete_image(&fake),
            "魔数对但缺 DQT/DHT/EOI 的残缺 JPEG 必须被拒"
        );

        // 空字节同样要拒：登记一个 0 字节的缩略图 token，界面请求到的是
        // 空响应——比「没有缩略图」更难查，后者至少会老实回退类型图标
        assert!(!is_complete_image(&[]), "空字节不是图片");
        // 完全不是图片的东西也要拒
        assert!(!is_complete_image(b"not an image at all"));
    }

    /// **每一段各缺一次**：只缺 DQT、只缺 DHT、只缺 EOI 都必须被拒。
    ///
    /// 不这样会怎样：这条是上一条的补强，而它的必要性是变异测试逼出来的。
    /// 上一条的夹具三样全缺，于是「把 DQT 那一项检查删掉」时它仍被另外两项
    /// 拒下——断言照样通过，缺陷溜过去。
    ///
    /// 要抓「少查了某一段」，夹具就必须**只缺那一段**。这正是
    /// 「断言必须落在差异真正出现的那一点」。
    #[test]
    fn each_missing_jpeg_segment_is_caught_on_its_own() {
        // 拿一张真 JPEG 当底，逐段破坏，保证每个夹具只差一样东西
        use image::{ImageFormat, RgbImage};
        let img = RgbImage::from_fn(16, 12, |x, y| {
            image::Rgb([
                ((x * 19 + y * 5) % 251) as u8,
                ((x * 3 + y * 23) % 251) as u8,
                ((x * 29 + y * 7) % 251) as u8,
            ])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Jpeg).expect("编码");
        let good = buf.into_inner();
        assert!(is_complete_image(&good), "底图本身必须通过");

        // 把某一种段标记**全部**打坏。
        //
        // 真实 JPEG 通常有两张量化表（亮度 / 色度）与四张霍夫曼表，
        // 只改第一个的话另一个还在，夹具就不是「缺这一段」——
        // 第一版就栽在这里，断言失败的是夹具而不是产品
        let strip = |src: &[u8], marker: u8| -> Vec<u8> {
            let mut out = src.to_vec();
            let mut i = 0;
            while i + 1 < out.len() {
                if out.get(i) == Some(&0xFF) && out.get(i + 1) == Some(&marker) {
                    if let Some(b) = out.get_mut(i + 1) {
                        *b = 0xEE; // 换成一个无意义的段标记
                    }
                }
                i += 1;
            }
            out
        };

        // ① 只缺量化表
        let no_dqt = strip(&good, 0xDB);
        assert!(
            !no_dqt.windows(2).any(|w| w == [0xFF, 0xDB]),
            "夹具本身要真的一个 DQT 都不剩，否则测的不是「缺这一段」"
        );
        assert!(
            no_dqt.windows(2).any(|w| w == [0xFF, 0xC4]),
            "这个夹具只该缺 DQT，霍夫曼表要留着"
        );
        assert!(
            !is_complete_image(&no_dqt),
            "只缺量化表也必须被拒——那正是 stripped 缩略图缺的东西"
        );

        // ② 只缺霍夫曼表
        let no_dht = strip(&good, 0xC4);
        assert!(
            !no_dht.windows(2).any(|w| w == [0xFF, 0xC4]),
            "夹具本身要真的一个 DHT 都不剩"
        );
        assert!(
            no_dht.windows(2).any(|w| w == [0xFF, 0xDB]),
            "这个夹具只该缺 DHT，量化表要留着"
        );
        assert!(!is_complete_image(&no_dht), "只缺霍夫曼表也必须被拒");

        // ③ 只去掉结尾的 EOI
        let mut no_eoi = good.clone();
        no_eoi.truncate(no_eoi.len().saturating_sub(2));
        assert!(
            !is_complete_image(&no_eoi),
            "缺 EOI 的 JPEG 会被解码器当成截断文件"
        );
    }

    /// 一张**真的能被解码器接受**的 JPEG 必须通过。
    ///
    /// 不这样会怎样：上一条只保证「坏的被拒」。若这条不在，一个「一律返回
    /// false」的实现也能让上一条全绿——而那会让所有缩略图都消失，
    /// 同样是静默的。
    ///
    /// 这里刻意真的编码一张图再验，而不是手搓一段看起来像 JPEG 的字节：
    /// 手搓的那种只能证明「我的检查认得我手搓的格式」。
    #[test]
    fn a_real_decodable_jpeg_passes() {
        use image::{ImageFormat, RgbImage};

        // 造一张小图，像素用算式生成
        let img = RgbImage::from_fn(16, 12, |x, y| {
            image::Rgb([
                ((x * 17 + y * 3) % 251) as u8,
                ((x * 7 + y * 29) % 251) as u8,
                ((x * 13 + y * 11) % 251) as u8,
            ])
        });
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Jpeg).expect("编码 JPEG");
        let bytes = buf.into_inner();

        assert!(
            is_complete_image(&bytes),
            "一张真的 JPEG 必须通过，否则缩略图会全部消失"
        );

        // 自证这张图确实解得开——否则上面那条断言可能只是碰巧成立
        assert!(
            image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg).is_ok(),
            "夹具本身必须是能解码的，不然这条测试没有意义"
        );
    }

    /// PNG 与 WebP 认魔数即可，不要求 JPEG 那几个段。
    ///
    /// 不这样会怎样：拿 JPEG 的段要求去卡 PNG，会把本来好好的 PNG 缩略图
    /// 全部挡掉——又是一次「图莫名其妙不出来」。
    #[test]
    fn png_and_webp_are_accepted_by_magic() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend((0..64).map(|i| (i % 251) as u8));
        assert!(is_complete_image(&png), "PNG 应当通过");

        let mut webp = Vec::from(*b"RIFF");
        webp.extend(1000u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        webp.extend((0..64).map(|i| ((i * 5) % 251) as u8));
        assert!(is_complete_image(&webp), "WebP 应当通过");

        // 只有 RIFF 不算：wav / avi 也是 RIFF 开头
        let mut wav = Vec::from(*b"RIFF");
        wav.extend(1000u32.to_le_bytes());
        wav.extend_from_slice(b"WAVE");
        assert!(!is_complete_image(&wav), "RIFF+WAVE 不是图片");
    }
}
