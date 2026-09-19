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

use grammers_client::media::{Downloadable, Media};
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

/// 进一个对话时默认拉多少条带文件的消息。
///
/// 不是「全部」：一个活跃群里可能有上万条，全拉会让进目录等很久，也白白占
/// 限流配额。100 条足够填满一屏并留出滚动余量，不够时再加载更多。
const DEFAULT_MESSAGE_PAGE: usize = 100;

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

        loop {
            let d = match it.next().await {
                Ok(Some(d)) => d,
                Ok(None) => break,
                Err(e) => return Err(map_rpc(&e)),
            };
            let peer = d.peer();
            let title = peer.name().unwrap_or("(无标题)").to_string();
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
            if let Ok(Some(r)) = peer.to_ref().await {
                peers.insert(chat_id, r);
            }
            out.push(Conversation {
                chat: chat_id,
                title,
                can_send,
                can_delete,
            });
        }

        if let Ok(mut c) = self.conversations.lock() {
            c.clone_from(&out);
        }
        if let Ok(mut p) = self.peers.lock() {
            *p = peers;
        }
        Ok(out)
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
    /// 用 `Document` 过滤器而不是拉全部消息再本地筛：搜索可下推已实测，
    /// 让服务端筛能少传绝大部分无关消息，也少占限流配额。
    async fn list_messages(&self, chat: i64, limit: usize) -> Result<Vec<Entry>> {
        let client = self.client()?;
        let peer = self.peer_ref(chat).await?;

        let mut it = client
            .search_messages(peer)
            .filter(tl::enums::MessagesFilter::InputMessagesFilterDocument)
            .limit(limit);

        let mut out = Vec::new();
        let mut cache = Vec::new();
        loop {
            let msg = match it.next().await {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => return Err(map_rpc(&e)),
            };
            let Some(media) = msg.media() else { continue };
            // 只收文档：图片走 Photo 分支、没有文件名，而 omy 关心的是文件
            let Media::Document(doc) = &media else {
                continue;
            };
            let id = TelegramId {
                chat,
                message: msg.id(),
            };
            let key = id.encode();
            let size = u64::try_from(doc.size().unwrap_or(0)).unwrap_or(0);
            if let Some(loc) = media.to_raw_input_location() {
                cache.push((
                    key.clone(),
                    CachedMedia {
                        location: loc,
                        size,
                    },
                ));
            }
            out.push(Entry {
                id: key,
                // 没有文件名的文档用消息号兜底，而不是留空——留空的条目在
                // 界面上是一行看不出是什么的东西
                name: doc
                    .name()
                    .filter(|n| !n.is_empty())
                    .map_or_else(|| format!("message-{}", msg.id()), ToString::to_string),
                is_dir: false,
                size: Some(size),
                mtime: None,
                // 不用 file_reference 当 etag：它会过期，拿它做变更检测会让
                // 缓存层误以为文件变了，从而反复重新下载
                etag: None,
            });
        }

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
        if dir_id.is_empty() {
            return Ok(Capabilities::read_only());
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
}
