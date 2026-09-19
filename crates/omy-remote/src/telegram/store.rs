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
//! 结构、id 编解码、分片对齐**已实现并可测**；真正发 RPC 的部分是骨架，
//! 因为分片的 `offset` / `limit` 真实约束**尚未实测**（缺能收验证码的账号）。
//! 那几个常量标了 TODO，**不要按官方文档的记载把它们钉死**——那是把推断写成
//! 事实。见 `docs/research/15-telegram-remote.md` §11.4。

use crate::store::{Entry, RemoteStore};
use crate::{Capabilities, Error, Result};

/// 分片下载的单片大小。
///
/// TODO(实测后钉死)：**这个值现在是占位**。官方文档记载 `limit` 须为 1 KiB 的
/// 倍数且能整除 1 MiB、`offset` 须为 `limit` 的倍数，但**我们没有实测过**，
/// 而探针里正要试出服务端真实接受与拒绝的边界（见 §11.4.6）。
///
/// 选 512 KiB 只是因为它同时满足上述两条记载；**实测结果出来后以实测为准**。
/// 在那之前不要基于这个值做「已经对齐好了」的推理。
const CHUNK: u64 = 512 * 1024;

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

/// Telegram 驱动。
///
/// # 完成度
///
/// 见模块文档：结构与纯逻辑已就绪，RPC 部分是骨架。
#[derive(Debug)]
pub struct TelegramStore {
    /// 已知对话，充当目录表。
    ///
    /// 由 `list("")` 填充。做成缓存而不是每次查，是因为 `getDialogs` 有分页
    /// 且会被限流；但**这也意味着它可能过期**，所以 `effective_capabilities`
    /// 查不到对话时必须报错而不是假设可写。
    conversations: Vec<Conversation>,
    /// 分片大小。
    ///
    /// 可配置而不是写死常量，这样实测出真实约束后改一处即可，也便于单测。
    chunk: u64,
}

impl Default for TelegramStore {
    fn default() -> Self {
        Self::new()
    }
}

impl TelegramStore {
    /// 建一个空的驱动。
    #[must_use]
    pub fn new() -> Self {
        Self {
            conversations: Vec::new(),
            chunk: CHUNK,
        }
    }

    /// 用一组已知对话构造（测试与恢复配置用）。
    #[must_use]
    pub fn with_conversations(conversations: Vec<Conversation>) -> Self {
        Self {
            conversations,
            chunk: CHUNK,
        }
    }

    /// 当前用的分片大小。
    #[must_use]
    pub const fn chunk(&self) -> u64 {
        self.chunk
    }

    /// 找一个对话。
    #[must_use]
    pub fn conversation(&self, chat: i64) -> Option<&Conversation> {
        self.conversations.iter().find(|c| c.chat == chat)
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
            .map(Conversation::capabilities)
            .ok_or_else(|| Error::NotFound(format!("未知对话：{dir_id}")))
    }

    fn describe(&self) -> String {
        String::from("Telegram")
    }

    /// 列目录。
    ///
    /// `dir_id` 为空 → 列对话（`messages.getDialogs`）；
    /// 否则 → 列该对话里带媒体的消息。
    ///
    /// # Errors
    ///
    /// TODO(实测后完成)：真正的 RPC 还没接。分页行为与单页返回量尚未实测，
    /// 而分页参数写错的表现是「对话列表只有前几个」——不报错、很难发现。
    /// 所以先不猜，等 §11.4.6 的探针给出真实数字。
    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>> {
        if dir_id.is_empty() {
            // 对话即目录：每个对话是一个子目录
            return Ok(self
                .conversations
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
        if self.conversation(chat).is_none() {
            return Err(Error::NotFound(format!("未知对话：{dir_id}")));
        }
        Err(Error::Unsupported("telegram list messages"))
    }

    /// 读取区间。
    ///
    /// 分片对齐由 [`plan_chunks`] 算好（已可测），真正取字节的部分待接。
    ///
    /// # Errors
    ///
    /// TODO(实测后完成)：`offset` / `limit` 的真实约束未实测，见 [`CHUNK`]。
    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        // 先解 id：坏 id 要在发请求之前就报出来
        let _tid = TelegramId::decode(id)?;
        let _plan = plan_chunks(offset, len, self.chunk);
        // 注意：真正实现时，拼好分片后**必须**按 plan.skip / plan.take 裁剪。
        // RemoteStore::read_range 的契约要求实现方在服务端给多了时自行裁剪，
        // 而这里我们本来就会因为对齐而多取——不裁剪的话上层会把多出来的字节
        // 当成密文的一部分，解出来是一堆认证失败，且症状指向密钥而非偏移。
        Err(Error::Unsupported("telegram read_range"))
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
        // 坏 id：报协议错误（而不是 Unsupported，那意味着已经走到发请求那步）
        assert!(matches!(
            rt.block_on(s.read_range("garbage", 0, 1)),
            Err(Error::Protocol(_))
        ));
        // 好 id：走到未实现那一步，说明校验通过了
        assert!(matches!(
            rt.block_on(s.read_range("tg:1:2", 0, 1)),
            Err(Error::Unsupported("telegram read_range"))
        ));
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
