//! 已打开的远程存储文件，与全局密文块缓存。
//!
//! # 为什么需要一张「打开句柄表」
//!
//! 远程浏览（[`crate::place_cmds`]）只列目录、读前几百字节识别文件，
//! 并不持有可播放的来源。而播放是**多次** Range 请求：播放器先取开头，
//! 再按拖动位置取中段、尾段。这些请求必须复用同一个 [`RemoteSource`]——
//! 它内部带着 1 MiB 对齐的密文块缓存，若每次请求都新建一个，缓存永远
//! 命中不了，seek 一次就重新拉一遍。
//!
//! 所以前端点开一个远程 `.omy` 时，后端构造好 `RemoteSource` 登记到这里，
//! 换回一个不透明 token；之后 `omystream://pfile/<token>` 的每次 Range
//! 请求都凭 token 取回同一个来源。
//!
//! # 缓存的只有密文
//!
//! [`RemoteSource`] 持有的头部字节、[`BlockCache`] 落盘的块，全部是
//! **加密的**。每次请求时 [`omy_core::file::open`] 用会话里的 KEK 当场
//! 重新推导 payload key，解出请求的那一段、随响应发走、立即丢弃——
//! 不在内存里长期持有 `OpenedFile`，磁盘上也只有密文。锁定时关闭句柄即可。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use omy_remote::cache::BlockCache;
use omy_remote::source::RemoteSource;
use omy_remote::PlaceStore;

/// 全局密文块缓存。
///
/// 所有远程位置、所有打开的文件共用同一个缓存根目录和同一个上限，
/// 这样「已用空间 / 上限」是全局口径，设置页里的数字才与磁盘真实占用一致。
#[derive(Default)]
pub struct RemoteCache {
    /// `None` 表示缓存目录无法建立，此时退化为「不缓存、每次走网络」，
    /// 而不是让整个远程功能不可用。
    inner: Mutex<Option<BlockCache>>,
}

impl RemoteCache {
    /// 按当前配置建立缓存根。
    #[must_use]
    pub fn from_config() -> Self {
        let cfg = omy_config::Config::load().unwrap_or_default();
        Self::from_root(Self::resolve_root(cfg.remote.cache_dir.clone()), cfg.remote.cache_limit)
    }

    /// 指定根目录与上限（测试与设置更新用）。
    fn from_root(root: Option<PathBuf>, limit: u64) -> Self {
        let cache = root.and_then(|r| match BlockCache::new(&r, limit) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[omy] 远程缓存目录 {r:?} 无法建立，本次不缓存: {e}");
                None
            }
        });
        Self {
            inner: Mutex::new(cache),
        }
    }

    /// 取一份缓存句柄的克隆传给新建的 `RemoteSource`。
    ///
    /// `BlockCache` 的克隆共享同一根目录（LRU 靠文件 atime 维护），
    /// 所以多个来源能真正共享同一份磁盘缓存。
    #[must_use]
    pub fn snapshot(&self) -> Option<BlockCache> {
        self.inner.lock().ok()?.clone()
    }

    /// 当前缓存占用字节数。
    #[must_use]
    pub fn used(&self) -> u64 {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(BlockCache::used))
            .unwrap_or(0)
    }

    /// 配置的上限字节数（0 表示不限）。
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.limit()))
            .unwrap_or(0)
    }

    /// 缓存根目录，供设置页「打开缓存目录」。
    #[must_use]
    pub fn root(&self) -> Option<PathBuf> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.root().to_path_buf()))
    }

    /// 清空全部缓存块，返回清空后的占用（应为 0）。
    pub fn clear(&self) -> u64 {
        if let Ok(g) = self.inner.lock() {
            if let Some(c) = g.as_ref() {
                // 清失败不致命：下次淘汰仍会回收，这里只如实返回当前占用
                let _ = c.clear();
            }
            return g.as_ref().map(BlockCache::used).unwrap_or(0);
        }
        0
    }

    /// 设置变更后重建：换根目录或调整上限。
    ///
    /// 换目录不搬运旧块——它们是密文，按新键也读不到，留着只会占空间，
    /// 但这里不主动删用户指定过的旧目录，交给「立即清理」处理。
    pub fn reload(&self, root: Option<PathBuf>, limit: u64) {
        if let Ok(mut g) = self.inner.lock() {
            *g = root.and_then(|r| BlockCache::new(&r, limit).ok());
        }
    }

    /// 解析远程块缓存根目录：用户自定义优先，否则落在应用缓存目录下的
    /// `remote/`。初始化与「设置保存后重建」共用这一处，避免两处算出
    /// 不同的目录（新增字段时两处都要改）。
    #[must_use]
    pub fn resolve_root(custom: Option<String>) -> Option<PathBuf> {
        custom
            .map(PathBuf::from)
            .or_else(omy_config::cache_dir)
            .map(|base| base.join("remote"))
    }
}

/// 一个已打开、可按需解密播放的远程文件。
///
/// 只保留服务一次 Range 请求所需的东西：完整头部（每次重新 `open`）、
/// 接好缓存与网络的来源、解密内容的 MIME。位置 id 与路径在构造
/// `RemoteSource` 时已固化进缓存键，这里不重复持有；真实文件名、明文大小、
/// 预览类别在打开命令的返回值里给前端，播放阶段用不到。
pub struct OpenPlaceFile {
    /// 完整文件头（密文）。每次请求据此重新 `open`，不长期持有 payload key。
    ///
    /// **普通文件（非 omy）这里是空的**，见 [`Self::plain`]。
    pub header: Vec<u8>,
    /// 这是一个**未加密的普通文件**吗。
    ///
    /// 远程位置里普通文件是主体内容（Telegram 频道里的图片、视频、文档），
    /// 它们没有 omy 头部，也不需要解密——直接把远程字节按 Range 转出去。
    ///
    /// 用一个字段而不是另开一张句柄表：两张表意味着锁定时要记得清两处，
    /// 漏一处就是「锁定后仍能凭旧 token 读到内容」的安全缺口。
    ///
    /// `Some(总字节数)` 表示普通文件；`None` 表示 omy 文件。
    pub plain: Option<u64>,
    /// 接好缓存与网络的密文来源。
    ///
    /// 泛型参数用 [`PlaceStore`]（各 provider 的统一外壳）而不是某个具体驱动：
    /// 写死成 `RemoteSource<WebDavStore>` 会让播放链路也单态化到 WebDAV，
    /// 加第二个 provider 时只剩「另拉一条平行链路」这一条路。
    pub source: RemoteSource<PlaceStore>,
    /// 解密后内容的 MIME。
    pub mime: String,
}

/// 已打开远程文件的句柄表。
#[derive(Default)]
pub struct PlaceFiles {
    files: Mutex<HashMap<String, Arc<OpenPlaceFile>>>,
    seq: Mutex<u64>,
}impl PlaceFiles {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个打开的文件，返回不透明 token。
    pub fn insert(&self, f: OpenPlaceFile) -> Option<String> {
        let mut seq = self.seq.lock().ok()?;
        *seq += 1;
        let token = format!("pf{seq}");
        // token 自增后再拿文件表锁插入，保证「拿到的 token 必已登记」
        let mut files = self.files.lock().ok()?;
        files.insert(token.clone(), Arc::new(f));
        Some(token)
    }

    /// 按 token 取打开的文件。
    #[must_use]
    pub fn get(&self, token: &str) -> Option<Arc<OpenPlaceFile>> {
        self.files.lock().ok()?.get(token).cloned()
    }

    /// 关闭单个文件（播放结束）。
    pub fn remove(&self, token: &str) {
        if let Ok(mut m) = self.files.lock() {
            m.remove(token);
        }
    }

    /// 关闭全部文件（锁定时调用）。
    ///
    /// 句柄里虽只有密文，但锁定的语义是「从现在起什么都读不到」，
    /// 留着一批可凭 token 直接 Range 的来源与该语义不符。
    ///
    /// 自增序号不清零：token 只在本次进程生命周期内有效，清零可能让
    /// 锁定前后的两个文件复用同一 token。
    pub fn clear(&self) {
        if let Ok(mut m) = self.files.lock() {
            m.clear();
        }
    }
}

/// 远程缩略图句柄表（只存文件头，不存正文来源）。
///
/// 列表里每个媒体文件都要显示缩略图，但为每张图都构造一个带网络来源的
/// [`OpenPlaceFile`] 既浪费也无必要——缩略图 TLV 就在**文件头**里，
/// `browse` 列目录时已经把完整头部取回来了。这里登记「token → 完整头部」，
/// `omystream://pthumb/<token>` 凭头部当场 `open` 出缩略图，零额外网络请求。
///
/// 远程容器内一个条目的定位信息。
///
/// # 为什么不把内部路径编进 entry id
///
/// 让前端直接传偏移和长度，等于把「读这个容器任意位置」的能力交给 WebView 里
/// 的任何脚本，越过了索引这层约束——本地容器（见 [`crate::citem`]）当初就是
/// 为此改成 token 的，这里沿用同一套。
///
/// 另一个理由是 **entry id 的形状不该动**：Telegram 的 `tg:<对话>:<消息>`
/// 已经是两级，容器内是第三级，硬塞进去会让 id 变成一个需要分情况解析的东西，
/// 而这个决定一旦铺开就很难改。
#[derive(Debug, Clone)]
pub struct PlaceContainerRef {
    /// 所属远程文件的播放句柄 token（`remote_place_open` 颁发的那个）。
    ///
    /// 用它而不是「位置 id + 路径」：容器明文不在磁盘上、要按需解，
    /// 而那个句柄背后正好挂着已经建好的远程源与密文缓存。句柄失效（锁定、
    /// 关闭）时这个 token 自然跟着失效，不必单独清理。
    pub file_token: String,
    /// 相对容器根的路径，供诊断与界面显示。
    pub inner_path: String,
    /// 在容器**明文载荷**里的起始偏移。
    pub offset: u64,
    /// 该条目的字节数。
    pub size: u64,
    /// MIME，按容器内的文件名后缀推导。
    pub mime: String,
}

/// 远程容器条目的访问登记表。
#[derive(Default)]
pub struct PlaceContainers {
    inner: Mutex<HashMap<String, PlaceContainerRef>>,
}

impl PlaceContainers {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个条目，返回访问 token。
    ///
    /// 同一（句柄, 内部路径）重复登记得到**同一个** token：容器面板每次重新列
    /// 都会再登记一遍，token 变来变去会让前端已拿到的 URL 失效，
    /// 表现是列表一刷新图就裂了。
    pub fn register(&self, r: PlaceContainerRef) -> Option<String> {
        let key = token_for_place_item(&r.file_token, &r.inner_path);
        self.inner.lock().ok()?.insert(key.clone(), r);
        Some(key)
    }

    /// 按 token 取回定位信息。
    #[must_use]
    pub fn resolve(&self, token: &str) -> Option<PlaceContainerRef> {
        self.inner.lock().ok()?.get(token).cloned()
    }

    /// 清空（锁定时调用）。留着一批可用 token 与「锁定后什么都看不到」矛盾。
    pub fn clear(&self) {
        if let Ok(mut m) = self.inner.lock() {
            m.clear();
        }
    }

    /// 已登记条目数。
    ///
    /// 只有测试用得上——产品代码不需要知道表里有几条。留着是因为
    /// 「锁定后表里什么都不剩」那条断言需要它：只验 resolve 返回 None
    /// 不够，那也可能是 token 算错了而不是表真的清了。
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// 是否为空。理由同 [`PlaceContainers::len`]。
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 由（句柄, 内部路径）算出稳定 token。
///
/// 用哈希而不是把路径放进 URL：容器内的文件名是**解密出来的内容**，
/// 让它出现在 URL 里等于写进 WebView 的网络面板。
///
/// 中间必须有分隔符：直接拼的话 `("ab","c")` 与 `("a","bc")` 会撞成同一个
/// token——一个容器里的文件会被另一个容器的 URL 读到。`\0` 不可能出现在
/// 这两段中的任何一段里。
fn token_for_place_item(file_token: &str, inner_path: &str) -> String {
    // 与 `crate::citem::token_for` 用同一个哈希：同一件事同一个工具，
    // 两条容器路径的 token 算法保持一致，也不必为此多引一个依赖。
    let mut buf = Vec::with_capacity(
        file_token.len().saturating_add(inner_path.len()).saturating_add(1),
    );
    buf.extend_from_slice(file_token.as_bytes());
    buf.push(0);
    buf.extend_from_slice(inner_path.as_bytes());
    let h = omy_core::util::blake2b_256(&buf);
    // 取前 16 字节：够长到不会偶然相撞，又不至于让 URL 长得离谱。
    // token 本身不是凭据——真正的授权是「它在表里」且「所属句柄仍有效」。
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// 一个缩略图 token 背后的东西。
///
/// # 为什么要分两种
///
/// 两条来源的处理方式**完全不同**：
///
/// - omy 加密文件：存的是文件头，缩略图在 TLV 里，要用会话密钥解开才拿得到，
///   没解锁就不该给图；
/// - 远程普通文件（Telegram 的图片 / 视频）：存的是**服务端随消息送来的
///   缩略图字节本身**，它本来就是公开内容，不涉及任何密钥。
///
/// 混成一种（比如都当文件头）会让普通文件的字节被拿去 `peek_header`，
/// 必然失败、于是永远不出图——而那是一条**静默失败**：界面只是回退成
/// 类型图标，没有任何报错。
pub enum ThumbSource {
    /// omy 加密文件的完整文件头，缩略图要解密才拿得到。
    OmyHeader(Vec<u8>),
    /// 已经可以直接返回的图片字节（服务端给的内嵌缩略图）。
    Image(Vec<u8>),
}

/// 与 [`PlaceFiles`] 分开：缩略图只服务当前这一屏列表，刷新目录即清空，
/// 不与播放句柄（要跨多次 Range 请求、锁定才清）混在同一张表里。
#[derive(Default)]
pub struct PlaceThumbs {
    thumbs: Mutex<HashMap<String, ThumbSource>>,
    seq: Mutex<u64>,
    /// 清晰缩略图的**磁盘**缓存目录（`<缓存根>/rthumbs`）。
    ///
    /// 内存里的 `thumbs` 表进程重启即空，于是每次开应用进对话都要重新拉一遍
    /// 清晰缩略图（用户报的「重启全部重新加载」）。这里把清晰图落盘、按媒体 id
    /// 命名，重启后直接读盘出清晰图、不再从零重拉。`None` 表示拿不到缓存目录
    /// （降级为纯内存，不报错）。
    disk: Option<PathBuf>,
}

impl PlaceThumbs {
    /// 带磁盘缩略图缓存目录的表（`None` = 纯内存，无磁盘持久）。
    /// 目录不存在会尝试创建；创建失败则降级为纯内存。
    #[must_use]
    pub fn with_disk_dir(dir: Option<PathBuf>) -> Self {
        if let Some(d) = &dir {
            let _ = std::fs::create_dir_all(d);
        }
        Self {
            disk: dir,
            ..Self::default()
        }
    }

    /// 把媒体 id 映射成磁盘文件名。id 形如 `tg:-100123:456`，含 `:` 等不能直接
    /// 当文件名的字符，用内容哈希命名，避免路径注入也避免超长。
    fn disk_path(&self, media_id: &str) -> Option<PathBuf> {
        let dir = self.disk.as_ref()?;
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in media_id.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Some(dir.join(format!("{hash:016x}.jpg")))
    }

    /// 从磁盘读某个媒体 id 的清晰缩略图字节。没有或读失败返回 `None`。
    #[must_use]
    pub fn disk_image(&self, media_id: &str) -> Option<Vec<u8>> {
        let p = self.disk_path(media_id)?;
        std::fs::read(p).ok().filter(|b| !b.is_empty())
    }

    /// 把某个媒体 id 的清晰缩略图字节写盘。失败静默忽略（缓存是附加项，
    /// 写不进不该影响浏览）。
    pub fn persist_image(&self, media_id: &str, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(p) = self.disk_path(media_id) {
            let _ = std::fs::write(p, bytes);
        }
    }

    /// 登记一份完整文件头用于取缩略图，返回不透明 token。
    pub fn insert(&self, header: Vec<u8>) -> Option<String> {
        self.put(ThumbSource::OmyHeader(header))
    }

    /// 登记一份**已经可以直接返回**的图片字节（服务端内嵌缩略图）。
    ///
    /// 与 [`PlaceThumbs::insert`] 分成两个入口而不是加个 flag：两者背后是
    /// 不同的东西（文件头 vs 图片），合成一个入口迟早有人传错，
    /// 而传错的表现是「图永远不出来、且不报错」。
    pub fn insert_image(&self, bytes: Vec<u8>) -> Option<String> {
        // 内容寻址：token 由字节内容算出，同一张图（头像/内嵌缩略图）永远得到
        // 同一个 token。这样目录刷新时重新登记同一张头像不会换 token，前端
        // 已经拿到的 `omystream://pthumb/<token>` URL 依然有效——**这正是修
        // 「刷新时其他群图标闪成文件夹再变回」的关键**：换 token 会让旧 URL
        // 在新列表到达前的那几百毫秒里 404、回退成文件夹图标。
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in &bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let token = format!("pi{hash:016x}");
        let mut thumbs = self.thumbs.lock().ok()?;
        thumbs.insert(token.clone(), ThumbSource::Image(bytes));
        Some(token)
    }

    fn put(&self, src: ThumbSource) -> Option<String> {
        let mut seq = self.seq.lock().ok()?;
        *seq += 1;
        let token = format!("pt{seq}");
        let mut thumbs = self.thumbs.lock().ok()?;
        thumbs.insert(token.clone(), src);
        Some(token)
    }

    /// 按 token 取 omy 文件头字节；这个 token 若是图片则返回 `None`。
    #[must_use]
    pub fn get(&self, token: &str) -> Option<Vec<u8>> {
        match self.thumbs.lock().ok()?.get(token)? {
            ThumbSource::OmyHeader(h) => Some(h.clone()),
            ThumbSource::Image(_) => None,
        }
    }

    /// 按 token 取已经可直接返回的图片字节；是文件头则返回 `None`。
    #[must_use]
    pub fn get_image(&self, token: &str) -> Option<Vec<u8>> {
        match self.thumbs.lock().ok()?.get(token)? {
            ThumbSource::Image(b) => Some(b.clone()),
            ThumbSource::OmyHeader(_) => None,
        }
    }

    /// 刷新目录或锁定时清空：上一屏的缩略图 token 全部失效。
    pub fn clear(&self) {
        if let Ok(mut m) = self.thumbs.lock() {
            m.clear();
        }
    }

    /// 只清 omy 文件头缩略图（`OmyHeader`），保留图片（`Image`：头像与内嵌
    /// 缩略图）。
    ///
    /// 用于目录刷新：文件头 token 是每屏登记、按序号递增的，必须清否则无限
    /// 涨；而图片 token 已改成内容寻址（见 `insert_image`），同一张图刷新前后
    /// 是同一个 token，清了反而会让当前显示的头像在新列表到达前 404、闪成
    /// 文件夹图标。所以刷新只清文件头、不动图片。
    pub fn clear_headers(&self) {
        if let Ok(mut m) = self.thumbs.lock() {
            m.retain(|_, v| matches!(v, ThumbSource::Image(_)));
        }
    }
}

#[cfg(test)]
mod tests {
    /// 同一条目重复登记必须得到同一个 token。
    ///
    /// 不这样会怎样：容器面板每次重新列都会再登记一遍，token 变来变去会让
    /// 前端已经拿到的 URL 失效——表现是列表一刷新，里面的图就全裂了。
    #[test]
    fn place_container_token_is_stable() {
        let a = token_for_place_item("pf1", "a/b.txt");
        let b = token_for_place_item("pf1", "a/b.txt");
        assert_eq!(a, b);
    }

    /// 不同句柄或不同路径必须得到不同 token，且字段边界不能被跨越。
    ///
    /// 不这样会怎样：("ab","c") 与 ("a","bc") 撞成同一个 token，
    /// 一个容器里的文件会被另一个容器的 URL 读到。分隔符就是为这条而加的。
    #[test]
    fn place_container_token_has_no_field_collision() {
        assert_ne!(
            token_for_place_item("pf1", "a.txt"),
            token_for_place_item("pf2", "a.txt")
        );
        assert_ne!(
            token_for_place_item("pf1", "a.txt"),
            token_for_place_item("pf1", "b.txt")
        );
        assert_ne!(
            token_for_place_item("ab", "c"),
            token_for_place_item("a", "bc")
        );
    }

    /// token 里不能带出容器内的文件名。
    ///
    /// 不这样会怎样：容器内的文件名是**解密出来的内容**，把它放进 URL 等于
    /// 写进 WebView 的网络面板——加密了内容却把目录结构泄露出去。
    #[test]
    fn place_container_token_does_not_leak_inner_name() {
        let t = token_for_place_item("pf1", "工资表/2026-机密.xlsx");
        assert!(!t.contains("工资"), "token 不能含明文文件名：{t}");
        assert!(!t.contains("xlsx"), "连后缀也不该带出来：{t}");
    }

    /// 锁定后不能残留可用 token。
    ///
    /// 不这样会怎样：锁定的语义是「从现在起什么都看不到」，而留着一批能
    /// 解析的 token 与它直接矛盾。目前即使不清，请求也会因为所属句柄已清
    /// 而 404——但那是**另一张表**的保证，这里必须自己成立，
    /// 否则谁把回查那步优化掉就静默破防。
    #[test]
    fn place_containers_clear_leaves_nothing() {
        let r = PlaceContainers::new();
        let t = r
            .register(PlaceContainerRef {
                file_token: String::from("pf1"),
                inner_path: String::from("a.txt"),
                offset: 0,
                size: 10,
                mime: String::from("text/plain"),
            })
            .expect("登记");
        assert!(r.resolve(&t).is_some(), "刚登记的应当解析得出");
        r.clear();
        assert!(r.resolve(&t).is_none(), "锁定后旧 token 必须失效");
        assert!(r.is_empty());
    }

    /// 未登记的 token 一律解析不出。
    ///
    /// 这条守的是「构造一个 token 就能读容器任意位置」。
    #[test]
    fn place_containers_reject_unknown_token() {
        let r = PlaceContainers::new();
        assert!(r.resolve("deadbeef").is_none());
    }

    use super::*;

    /// 根目录为 `None`（无法定位缓存位置）时降级为无缓存，而不是让状态构造失败。
    ///
    /// 不这样会怎样：移动端取不到缓存目录时，整个远程功能在启动时不可用——
    /// 但缓存本就是可有可无的加速层，没它顶多每次都走网络。
    #[test]
    fn cache_degrades_when_root_unknown() {
        let cache = RemoteCache::from_root(None, 1024);
        assert!(cache.snapshot().is_none(), "无根目录时句柄应为 None");
        assert_eq!(cache.used(), 0, "无缓存时占用恒为 0");
        assert_eq!(cache.clear(), 0, "清空无缓存也不应报错");
        assert_eq!(cache.limit(), 0);
    }

    /// 正常临时目录下缓存可用：写入增长、上限如实、清空归零。
    #[test]
    fn cache_works_in_temp_dir() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("omy_place_cache_{}_{nanos}", std::process::id()));
        let cache = RemoteCache::from_root(Some(dir.clone()), 4096);
        let snap = cache.snapshot().expect("临时目录应能建缓存");
        assert_eq!(cache.used(), 0);
        snap.put("p", "/a", 0, &[1u8; 500]);
        assert!(cache.used() >= 500, "写入后占用应增长");
        assert_eq!(cache.limit(), 4096);
        cache.clear();
        assert_eq!(cache.used(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// token 必须唯一，关闭后取不到，锁定清空后全空。
    ///
    /// 不这样会怎样：两个文件拿到同一 token，第二个 Range 请求解到第一个
    /// 文件的数据，表现为播放串台且极难排查。
    #[test]
    fn place_files_tokens_are_unique_and_revocable() {
        let files = PlaceFiles::new();
        let t1 = files.insert(make_holder(10)).expect("token1");
        let t2 = files.insert(make_holder(20)).expect("token2");
        assert_ne!(t1, t2, "两次打开的 token 不能相同");
        assert!(files.get(&t1).is_some() && files.get(&t2).is_some());
        files.remove(&t1);
        assert!(files.get(&t1).is_none(), "关闭后取不到");
        assert!(files.get(&t2).is_some(), "另一个不受影响");
        files.clear();
        assert!(files.get(&t2).is_none(), "锁定清空后旧 token 全失效");
        // 清空后再打开不复用旧 token
        let t3 = files.insert(make_holder(30)).expect("token3");
        assert_ne!(t3, t2, "清空后 token 也不能复用");
    }

    /// 缩略图表：token 唯一可取，刷新 / 锁定清空后旧 token 全失效。
    ///
    /// 不这样会怎样：列表反复进出目录，文件头在句柄表里只增不减，且上一屏
    /// 的 pthumb token 还能取到内容——与「锁定后什么都读不到」相悖。
    #[test]
    fn place_thumbs_register_and_clear() {
        let t = PlaceThumbs::with_disk_dir(None);
        let a = t.insert(vec![1u8, 2, 3]).expect("token a");
        let b = t.insert(vec![4u8]).expect("token b");
        assert_ne!(a, b, "两次登记的 token 不能相同");
        assert_eq!(t.get(&a).as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(t.get(&b).as_deref(), Some(&[4u8][..]));
        t.clear();
        assert!(
            t.get(&a).is_none() && t.get(&b).is_none(),
            "刷新/锁定后旧缩略图 token 必须全失效"
        );
    }

    /// 清晰缩略图落盘：写盘 → **换一个新表实例（模拟重启）** → 按同一媒体 id
    /// 能从盘上读回来。
    ///
    /// 不这样会怎样：只有内存表的话，进程重启后清晰缩略图全丢、每次进对话都
    /// 得重新拉——正是用户报的「重启全部重新加载」。这条断言证明「换实例后
    /// 仍命中」，即真的落到了盘上、不是内存冒充。用独立实例而不是同一个 t，
    /// 才能排除「只是内存 HashMap 还在」这种假通过。
    #[test]
    fn place_thumbs_disk_survives_restart() {
        let dir = std::env::temp_dir().join(format!(
            "omy-thumbtest-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let media_id = "tg:-100999:42";
        let bytes = vec![0xFFu8, 0xD8, 0xFF, 1, 2, 3, 4, 5];

        // 第一个实例：写盘
        {
            let t = PlaceThumbs::with_disk_dir(Some(dir.clone()));
            assert!(t.disk_image(media_id).is_none(), "空目录不该读到东西");
            t.persist_image(media_id, &bytes);
            assert_eq!(
                t.disk_image(media_id).as_deref(),
                Some(&bytes[..]),
                "同一实例写完应能读回"
            );
        }
        // 第二个实例（模拟重启，内存表从零开始）：仍能从盘读回
        {
            let t2 = PlaceThumbs::with_disk_dir(Some(dir.clone()));
            assert_eq!(
                t2.disk_image(media_id).as_deref(),
                Some(&bytes[..]),
                "重启后（新实例）必须仍能从盘读回清晰缩略图，否则等于没落盘"
            );
            // 不同 id 不串
            assert!(t2.disk_image("tg:-100999:43").is_none(), "别的 id 不该命中");
        }
        // 无磁盘目录时降级为纯内存：persist/disk_image 都不 panic、返回 None
        let mem = PlaceThumbs::with_disk_dir(None);
        mem.persist_image(media_id, &bytes);
        assert!(mem.disk_image(media_id).is_none(), "无盘时读不到、也不该崩");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 造一个 holder。`RemoteSource::new` 只 `peek_header`、不发网络，
    /// 所以给它一段真实加密文件的头部即可在没有服务器的情况下构造成功。
    fn make_holder(tag: u64) -> OpenPlaceFile {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{encrypt, EncryptOptions, RandomMaterial};
        use omy_remote::webdav::{Vendor, WebDavConfig, WebDavStore};

        let salt = [7u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"x", &salt, params).expect("KEK");
        let opts = EncryptOptions {
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(
            b"hello-remote",
            std::slice::from_ref(&kek),
            &salt,
            &opts,
            &RandomMaterial::generate(),
        )
        .expect("加密");

        let store = Arc::new(PlaceStore::from(
            WebDavStore::new(WebDavConfig {
                base_url: String::from("http://127.0.0.1:1/"),
                vendor: Vendor::Generic,
                ..WebDavConfig::default()
            })
            .expect("store"),
        ));
        // 独立运行时；holder 测试不真正读取，句柄随测试结束而丢弃
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let path = format!("/{tag}.omy");
        let source = RemoteSource::new(
            store,
            "p1",
            path,
            &enc.bytes,
            enc.bytes.len() as u64,
            None,
            rt.handle().clone(),
        )
        .expect("真实头部应能构造来源");

        OpenPlaceFile {
            header: enc.bytes,
            plain: None,
            source,
            mime: String::from("text/plain; charset=utf-8"),
        }
    }
}
