//! 密文块缓存。
//!
//! # 只缓存密文，这条是硬约束
//!
//! 缓存里存的必须是从远端**原样取回的密文字节**，解密后的明文一律不落盘。
//!
//! 这不是性能取舍，是威胁模型的边界。缓存目录若混进明文，等于在用户毫不
//! 知情的情况下，把他特意加密的内容以明文形式长期写回磁盘。而缓存密文
//! 完全没有这个问题：那些字节在云端本来就公开存放着，缓存到本地不增加
//! 任何泄露面——攻击者拿到缓存目录，得到的和他去扒云盘是一样的东西。
//!
//! 由此推出两条实现约束：
//!
//! 1. 写入点必须在「拿到密文、还没解密」时，不能在解密之后；
//! 2. 缓存键不能用文件名（那是解密出来的信息）。这里用
//!    `blake2(位置 ‖ 条目 id ‖ 块号)`，都是明文可见的信息。
//!
//! # 块对齐
//!
//! 播放器请求的区间是任意的，但缓存按固定大小的块存。按任意区间缓存会
//! 产生大量互相重叠的碎片，既浪费空间又几乎命中不了。
//!
//! # 两层：临时块与永久块
//!
//! 同一个 [`BlockCache`] 管两层，**放在两个并列的物理目录下**（口径见
//! `docs/research/14-remote-locations-cloud.md` §8.4.1，那里是唯一真相，
//! 本注释只说实现上的取舍）：
//!
//! | 层 | 位置 | 计入 `limit` | 参与 LRU 淘汰 | `clear` 会删 |
//! |---|---|---|---|---|
//! | 临时 | 调用方给的根，GUI 用 `cache/remote/` | 是 | 是 | 是 |
//! | 永久 | 它的兄弟目录 `cache/pinned/`（[`sibling_pinned_root`]） | **否** | **否** | **否** |
//!
//! 为什么是两个物理目录，而不是在同一个目录里给块加一个「已固定」标记位：
//!
//! 1. **两个用量数字各自 `du` 一下就是准的。** 共用目录时，要算「临时层用了
//!    多少」就得扫全目录再逐块查标记——块按哈希存，无法从路径反查归属，一个
//!    5 GB 的文件就是约 5000 次 stat。
//! 2. **「清空缓存」不会变成危险按钮。** 共用目录时它必须逐块判断标记，漏判
//!    一次就是把用户特意留下的东西删了，而这种缺陷只在用户真的丢了文件之后
//!    才会被发现。分目录后「清空」就是删一个目录，不可能误删。
//! 3. **淘汰器不需要新分支。** 它现在已经有「正在播放豁免」这一个例外，再叠
//!    一个「已固定豁免」就是两个互相影响的条件。分目录后淘汰器**根本扫不到**
//!    永久层——不是「记得跳过」，而是「看不见」。
//!
//! 两层的键算法完全相同，只是根目录不同，所以 [`BlockCache::get`] 命中哪一层
//! 对调用方没有区别。
//!
//! # 永久层为什么和临时层同父目录
//!
//! 便携优先（`omy-config::paths` 的模块文档）：PC 上配置与缓存就在可执行文件
//! 旁的 `omy-data/` 里。永久层取临时层的**兄弟目录**，于是用户改缓存位置时
//! 它跟着走，把便携目录整个拷到另一台机器时永久块也在——而不是留在老机器的
//! 系统目录里，表现为「拷过去了，但特意存下来离线看的电影没了」。


use std::path::{Path, PathBuf};

use blake2::digest::{Update, VariableOutput};

/// 缓存块大小。
///
/// 1 MiB：小了则元数据开销与请求数上升，大了则一次 seek 要多下无用数据。
/// 与加密分块（默认 256 KiB）不必相同——这是网络传输的粒度。
pub const BLOCK_SIZE: u64 = 1024 * 1024;

/// 各平台「不要索引/不要缩略图」的目录标记文件名。
///
/// 这些是 [`omy_core::fsatomic::mark_dir_no_index`] 写在缓存根目录的标记，
/// **不是缓存块**：既不能计入用量（否则设置页永远显示几十字节清不掉），
/// 也不能被 LRU 淘汰或被 `clear` 删掉（删了索引标记就没了，而且 Windows 上
/// `desktop.ini` 可能被资源管理器/Defender 短暂打开，强删会让整个
/// `remove_dir_all` 失败、反而留下真正的数据块）。缓存键是两级十六进制路径，
/// 绝不会与这些名字冲突。
const MARKER_FILES: &[&str] = &["desktop.ini", ".metadata_never_index", ".nomedia"];

/// 永久层里存密文块的子目录。
///
/// 块与标记分两个子目录，是为了让永久层的用量只统计 `blocks/`：标记现在是
/// 0 字节空文件，混在一起眼下看不出差别，但以后若给标记加上内容，用量就会
/// 莫名多出几 KB 而没人知道那是什么。
const PINNED_BLOCKS: &str = "blocks";

/// 永久层里记录「哪些文件被标记为永久」的子目录。
///
/// 这份清单必须落盘、不能只放在内存里：进程重启后标记若丢了，用户之前标为
/// 永久的文件会重新按临时块下载、随后被淘汰，而界面上那个勾还在——一个不兑现
/// 的承诺比没有这个功能更糟。
///
/// 清单按 `(place, 条目键)` 的哈希记，**不记文件名**：文件名是解密出来的信息，
/// 与块路径不含文件名是同一条理由（见本模块开头「只缓存密文」一节）。
const PINNED_MARKS: &str = "marks";

/// 判断一个文件名是否为目录标记文件。
fn is_marker(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|n| MARKER_FILES.contains(&n))
        .unwrap_or(false)
}

/// 永久层的默认位置：临时层根目录的**兄弟目录** `pinned`。
///
/// GUI 的临时层是 `<缓存目录>/remote`，于是永久层落在 `<缓存目录>/pinned`，
/// 与 14 号文档 §8.4.1 的 `omy-data/cache/remote/` 与 `omy-data/cache/pinned/`
/// 一致。**取兄弟而不是子目录**：放成子目录的话 `used()` / `evict()` /
/// `clear()` 会把永久块一并算上、一并删掉，「永久」当场作废且毫无征兆。
///
/// 这样定义还让「更改缓存位置」自动带着永久层一起走——两处各自解析路径迟早
/// 漂移，症状是改完位置后永久块还留在旧盘上、界面却显示 0 字节。
///
/// 返回 `None` 表示 `root` 没有父目录（根目录本身），此时永久缓存能力关闭。
#[must_use]
pub fn sibling_pinned_root(root: &Path) -> Option<PathBuf> {
    root.parent().map(|p| p.join("pinned"))
}

/// 磁盘上的密文块缓存。
#[derive(Debug, Clone)]
pub struct BlockCache {
    root: PathBuf,
    /// 上限字节数；0 表示不限制。
    limit: u64,
    /// 永久块的根目录；`None` 表示这台机器上拿不到应用数据目录，
    /// 此时「永久缓存」整个能力不可用（[`BlockCache::pinning_available`]
    /// 为假）。**不回落到临时层**：那会让「永久保留」变成一句空话，
    /// 用户以为文件离线可用，实际下次淘汰就没了。
    pinned_root: Option<PathBuf>,
}

impl BlockCache {
    /// 在给定目录下建立缓存，永久层落在它的兄弟目录（[`sibling_pinned_root`]）。
    ///
    /// # Errors
    ///
    /// 临时层目录无法创建时返回。永久层建不起来**不算错误**：那只是这台
    /// 机器不支持永久缓存，普通浏览与播放照常可用。
    pub fn new(root: impl Into<PathBuf>, limit: u64) -> std::io::Result<Self> {
        let root = root.into();
        let pinned = sibling_pinned_root(&root);
        Self::with_pinned_root(root, limit, pinned)
    }

    /// 同 [`BlockCache::new`]，但显式指定永久层根目录。
    ///
    /// 给两类调用方：测试要把永久层放到自己的临时目录里；将来若允许用户把
    /// 永久层单独指到另一个盘，也走这里。生产代码的默认路径应当用
    /// [`BlockCache::new`]，让两层保持同父目录。
    ///
    /// # Errors
    ///
    /// 临时层目录无法创建时返回。
    pub fn with_pinned_root(
        root: impl Into<PathBuf>,
        limit: u64,
        pinned_root: Option<PathBuf>,
    ) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        // 标记为不被系统索引：缓存是密文，进搜索索引没有意义，
        // 而索引器读取会造成额外的磁盘与耗电开销
        omy_core::fsatomic::mark_dir_no_index(&root).ok();

        // 永久层不能与临时层相互嵌套：互为子目录（或同一个目录）时，
        // `used()` / `evict()` / `clear()` 会把永久块一并算上、一并删掉，
        // 「永久」这个承诺当场作废，而且失效得毫无征兆。宁可关掉这个能力，
        // 也不要留一个会静默失效的实现。
        let pinned_root = pinned_root
            .filter(|p| !p.starts_with(&root) && !root.starts_with(p))
            .and_then(|p| {
                if std::fs::create_dir_all(p.join(PINNED_BLOCKS)).is_err() {
                    return None;
                }
                if std::fs::create_dir_all(p.join(PINNED_MARKS)).is_err() {
                    return None;
                }
                omy_core::fsatomic::mark_dir_no_index(&p).ok();
                Some(p)
            });
        Ok(Self { root, limit, pinned_root })
    }

    /// 缓存根目录（临时层）。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 永久块的根目录；`None` 表示本机不支持永久缓存。
    #[must_use]
    pub fn pinned_root(&self) -> Option<&Path> {
        self.pinned_root.as_deref()
    }

    /// 本机能否使用永久缓存。界面据此决定「永久保留」这一项是否出现——
    /// 摆一个点了没反应的开关，比没有这个开关更糟。
    #[must_use]
    pub fn pinning_available(&self) -> bool {
        self.pinned_root.is_some()
    }

    /// 配置的上限字节数；`0` 表示不限制。
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// 临时层当前占用字节数。
    ///
    /// **不含永久块**：上限约束的是「可以随时被回收的那部分」，把永久块算
    /// 进来会让「已用 3.9 / 2.0 GB」这种越界数字出现在设置页上，而用户点
    /// 「清空缓存」也降不下去。永久层的占用由 [`BlockCache::pinned_used`]
    /// 单独给出，两个数字分开展示。
    #[must_use]
    pub fn used(&self) -> u64 {
        walk(&self.root).iter().map(|(_, sz, _)| *sz).sum()
    }

    /// 永久层当前占用字节数；不支持永久缓存时恒为 0。
    ///
    /// 只统计 `blocks/`，不含 `marks/` —— 标记不是用户的数据，
    /// 让它进这个数字只会让用户对着一个对不上的值发愁。
    #[must_use]
    pub fn pinned_used(&self) -> u64 {
        match &self.pinned_root {
            Some(r) => walk(&r.join(PINNED_BLOCKS)).iter().map(|(_, sz, _)| *sz).sum(),
            None => 0,
        }
    }

    /// 被标记为永久的文件数。
    ///
    /// 永久层展示的是「4.7 GB（6 个文件）」这种**绝对值 + 计数**，没有分母；
    /// 所以这个计数和字节数一样是必需的，不是附带信息。
    #[must_use]
    pub fn pinned_file_count(&self) -> u64 {
        match &self.pinned_root {
            Some(r) => walk(&r.join(PINNED_MARKS)).len() as u64,
            None => 0,
        }
    }

    /// 两层用量的快照，供设置页分开展示。
    ///
    /// 两层各自 `du` 一次就得到准确数字——这正是分目录存放换来的好处
    /// （共用目录时得逐块 stat 再查标记，一个 5 GB 文件约 5000 次）。
    #[must_use]
    pub fn usage(&self) -> CacheUsage {
        CacheUsage {
            temp_used: self.used(),
            temp_limit: self.limit,
            pinned_used: self.pinned_used(),
            pinned_files: self.pinned_file_count(),
            pinning_available: self.pinning_available(),
        }
    }

    /// 某一块在临时层的缓存路径。
    ///
    /// 键里不含文件名：那需要解密才能得到，写进路径等于把它泄露到
    /// 文件系统里（而文件名往往比内容更能说明问题）。
    #[must_use]
    pub fn path_of(&self, place: &str, id: &str, block: u64) -> PathBuf {
        block_path(&self.root, place, id, block)
    }

    /// 某一块在永久层的缓存路径；不支持永久缓存时返回 `None`。
    #[must_use]
    pub fn pinned_path_of(&self, place: &str, id: &str, block: u64) -> Option<PathBuf> {
        self.pinned_root
            .as_ref()
            .map(|r| block_path(&r.join(PINNED_BLOCKS), place, id, block))
    }

    /// 「这个文件被标记为永久」的标记文件路径。
    ///
    /// 与块路径同一套哈希，只是不含块号——**路径里同样不能出现文件名**。
    fn mark_path(&self, place: &str, key: &str) -> Option<PathBuf> {
        let hex = digest_hex(&[place.as_bytes(), b"\0", key.as_bytes()]);
        let (a, rest) = hex.split_at(2);
        self.pinned_root
            .as_ref()
            .map(|r| r.join(PINNED_MARKS).join(a).join(rest))
    }

    /// 该文件是否已被标记为永久保留。
    #[must_use]
    pub fn is_pinned(&self, place: &str, key: &str) -> bool {
        self.mark_path(place, key)
            .map(|p| p.is_file())
            .unwrap_or(false)
    }

    /// 列出全部永久文件。
    ///
    /// 「管理永久缓存」那个列表靠它：路径是哈希、无法反查归属，所以标记文件
    /// **内容里**存着 `(place, 条目键, 总块数)`。没有这份可枚举的清单，用户就
    /// 只能回到原位置里一个个找回文件才能取消——而 Telegram 这类位置上，那条
    /// 消息他可能根本找不回来了。
    ///
    /// 内容存的是位置 id 与条目 id（远端上的密文名），不是解密后的文件名，
    /// 与 `path_of` 不含文件名是同一条理由。
    #[must_use]
    pub fn list_pinned(&self) -> Vec<PinnedFile> {
        let Some(r) = &self.pinned_root else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (path, _, _) in walk(&r.join(PINNED_MARKS)) {
            let Ok(raw) = std::fs::read(&path) else { continue };
            let Some(mut rec) = PinnedFile::decode(&raw) else {
                continue;
            };
            rec.used_bytes = self.pinned_bytes_of(&rec.place, &rec.key, rec.total_blocks);
            out.push(rec);
        }
        // 按位置、再按条目排序，让界面上的顺序稳定：目录遍历顺序由文件系统
        // 决定，不排序会让同一份列表每次刷新都换一个次序
        out.sort_by(|a, b| (&a.place, &a.key).cmp(&(&b.place, &b.key)));
        out
    }

    /// 某个永久文件在永久层实际占了多少字节。
    fn pinned_bytes_of(&self, place: &str, key: &str, total_blocks: u64) -> u64 {
        let mut n = 0u64;
        for block in 0..total_blocks {
            if let Some(p) = self.pinned_path_of(place, key, block) {
                if let Ok(m) = std::fs::metadata(&p) {
                    if m.is_file() {
                        n = n.saturating_add(m.len());
                    }
                }
            }
        }
        n
    }

    /// 把一个文件标记为永久，并把**临时层里已有的块搬到永久层**。
    ///
    /// 搬而不是复制：留一份在临时层等于同一内容占两倍空间，而且那一份还计入
    /// 上限，用户会看到「标记永久之后临时缓存用量反而涨了」。
    ///
    /// 返回搬过去的字节数。**它不负责把缺的块下下来**：按 §8.4.1，「转为永久」
    /// 在产品上是一次有进度、可取消、失败可重试的真实下载任务，那部分由
    /// [`crate::transfer`] 的任务模型驱动，下载到的块经 [`BlockCache::put`]
    /// 自动落进永久层（因为标记此时已经写好了）。这里只做「标记 + 搬已有的」，
    /// 两件事分开才能让下载失败时标记仍然有效、重试即可续下。
    ///
    /// 重复标记是幂等的。
    ///
    /// # Errors
    ///
    /// 本机不支持永久缓存时返回 [`crate::Error::Unsupported`]——这不该发生，
    /// 界面应当先看 [`BlockCache::pinning_available`] 再给出这一项。
    /// 标记写盘失败时返回 I/O 错误：**不能静默成功**，否则用户以为已经永久了。
    pub fn pin_file(&self, place: &str, key: &str, total_blocks: u64) -> crate::Result<u64> {
        let Some(mark) = self.mark_path(place, key) else {
            return Err(crate::Error::Unsupported(
                "本机没有可写的永久缓存目录，无法永久保留",
            ));
        };

        // 先写标记、再搬块：顺序是刻意的。标记先落盘，之后这个文件的每一次
        // `put` 都直接写进永久层，于是「正在下载时进程被杀」最坏只是少几块，
        // 重试就补上。反过来（先搬后标记）则会有一段时间里下载的块仍落在
        // 临时层、可被淘汰，而用户已经看到「已永久」了。
        if let Some(d) = mark.parent() {
            std::fs::create_dir_all(d)?;
        }
        // 标记写失败必须报上去：静默忽略等于让用户以为已经永久保留了，
        // 而下次启动这个文件会被当成普通临时块淘汰掉
        omy_core::fsatomic::write_atomic(&mark, &PinnedFile::new(place, key, total_blocks).encode())
            .map_err(|e| crate::Error::Io(std::io::Error::other(e.to_string())))?;

        let mut moved = 0u64;
        for block in 0..total_blocks {
            let from = self.path_of(place, key, block);
            if !from.is_file() {
                continue;
            }
            let Some(to) = self.pinned_path_of(place, key, block) else {
                continue;
            };
            if let Some(d) = to.parent() {
                if std::fs::create_dir_all(d).is_err() {
                    continue;
                }
            }
            let size = std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
            if move_file(&from, &to) {
                moved = moved.saturating_add(size);
            }
        }
        Ok(moved)
    }

    /// 取消永久标记，把永久块**搬回临时层**。
    ///
    /// 搬回而不是删掉：用户取消永久多半不是要立刻腾空间，内容还在缓存里下次
    /// 打开就不必重下。但搬回之后它就是普通临时块了——计入上限、参与 LRU，
    /// **可能马上超限被淘汰**。这是正确行为（§8.4.1），界面必须照实说
    /// 「取消后这些内容可能很快被清理」，否则用户以为只是改了个标记。
    ///
    /// 返回搬回的字节数。取消一个本来就不是永久的文件返回 `Ok(0)`，幂等。
    ///
    /// # Errors
    ///
    /// 标记删除失败时返回：标记还在就意味着 `put` 仍往永久层写，
    /// 静默忽略会让「已取消」是假的。
    pub fn unpin_file(&self, place: &str, key: &str, total_blocks: u64) -> crate::Result<u64> {
        let Some(mark) = self.mark_path(place, key) else {
            return Ok(0);
        };

        // 先删标记、后搬块：与 pin 相反，同样是让中途失败落在安全的一侧——
        // 标记已删而块还在永久层，只是暂时多占些空间，下次取消仍会清理；
        // 反过来则是块已搬走而标记还在，`put` 继续往永久层写，用户以为
        // 取消了其实没取消。
        if mark.is_file() {
            std::fs::remove_file(&mark)?;
        }

        let mut moved = 0u64;
        for block in 0..total_blocks {
            let Some(from) = self.pinned_path_of(place, key, block) else {
                continue;
            };
            if !from.is_file() {
                continue;
            }
            let size = std::fs::metadata(&from).map(|m| m.len()).unwrap_or(0);
            let to = self.path_of(place, key, block);
            if let Some(d) = to.parent() {
                if std::fs::create_dir_all(d).is_err() {
                    continue;
                }
            }
            if move_file(&from, &to) {
                moved = moved.saturating_add(size);
            }
        }
        Ok(moved)
    }

    /// 取消永久并直接删掉这些块，返回释放的字节数。
    ///
    /// 与 [`BlockCache::unpin_file`] 并列给出，是因为用户点「取消永久」的动机
    /// 有两种：一种是「不用离线了，但先留着」，另一种就是**腾空间**。后者若
    /// 只把块搬回临时层，磁盘一个字节都没少，而用户明确要的是少。
    ///
    /// # Errors
    ///
    /// 标记删除失败时返回，理由同 [`BlockCache::unpin_file`]。
    pub fn unpin_and_drop(&self, place: &str, key: &str, total_blocks: u64) -> crate::Result<u64> {
        let Some(mark) = self.mark_path(place, key) else {
            return Ok(0);
        };
        if mark.is_file() {
            std::fs::remove_file(&mark)?;
        }
        let mut freed = 0u64;
        for block in 0..total_blocks {
            let Some(p) = self.pinned_path_of(place, key, block) else {
                continue;
            };
            if let Ok(m) = std::fs::metadata(&p) {
                if m.is_file() && std::fs::remove_file(&p).is_ok() {
                    freed = freed.saturating_add(m.len());
                }
            }
        }
        Ok(freed)
    }

    /// 读取一块；未命中返回 `None`。
    ///
    /// **先查永久层**：它是用户明确要求留下的那一份，且不会被淘汰删掉。
    /// 两层的键完全一样，所以命中哪层对调用方没有区别。
    #[must_use]
    pub fn get(&self, place: &str, id: &str, block: u64) -> Option<Vec<u8>> {
        if let Some(p) = self.pinned_path_of(place, id, block) {
            if let Ok(data) = std::fs::read(&p) {
                return Some(data);
            }
        }
        let p = self.path_of(place, id, block);
        let data = std::fs::read(&p).ok()?;
        // 更新访问时间供 LRU 使用。失败不影响读取结果。
        // 永久块不做这一步：它根本不参与 LRU，多读一遍纯属浪费 I/O
        let now = std::time::SystemTime::now();
        filetime_touch(&p, now);
        Some(data)
    }

    /// 写入一块。
    ///
    /// **该文件已被标记为永久时直接写进永久层**，否则写临时层。这一步不能省：
    /// 若标记只影响「已下载的块」，用户先标记、再播放/下载的内容仍然会落在
    /// 临时层被淘汰掉——标记看起来生效了，实际什么也没保住。
    ///
    /// 失败不返回错误：缓存写不进去只该降级为「每次都走网络」，
    /// 不该让用户的播放或浏览失败。
    pub fn put(&self, place: &str, id: &str, block: u64, data: &[u8]) {
        let target = if self.is_pinned(place, id) {
            self.pinned_path_of(place, id, block)
                .unwrap_or_else(|| self.path_of(place, id, block))
        } else {
            self.path_of(place, id, block)
        };
        write_block(&target, data);
    }

    /// 按 LRU 淘汰到上限以内。
    ///
    /// 只遍历临时层（`self.root`）。永久层根本不在遍历范围内——「不参与淘汰」
    /// 因此是结构性的，不依赖任何「记得跳过」的判断（见模块文档「两层」一节）。
    ///
    /// `keep` 是当前正在使用的块路径，**一律豁免**。
    ///
    /// # 为什么必须有 keep
    ///
    /// 播放一个比缓存上限还大的视频时，若不豁免正在用的块，会一边下载
    /// 一边把自己刚下的块淘汰掉，表现为无限循环下载、进度永远不前进。
    /// 这是很容易在实现时漏掉的自噬行为。
    pub fn evict(&self, keep: &[PathBuf]) {
        if self.limit == 0 {
            return;
        }
        let mut files = walk(&self.root);
        let total: u64 = files.iter().map(|(_, sz, _)| *sz).sum();
        if total <= self.limit {
            return;
        }
        // 最久未访问的排前面
        files.sort_by_key(|(_, _, atime)| *atime);

        let mut need = total.saturating_sub(self.limit);
        for (path, size, _) in files {
            if need == 0 {
                break;
            }
            if keep.contains(&path) {
                continue;
            }
            if std::fs::remove_file(&path).is_ok() {
                need = need.saturating_sub(size);
            }
        }
    }

    /// 清空全部缓存块。
    ///
    /// 只删除根目录下的缓存条目（两级哈希子目录与块文件），**保留根目录
    /// 本身和索引标记文件**。这样：
    /// - 标记（Windows 的 `desktop.ini` 等）不被删，免得失效，也不会因为它
    ///   被资源管理器短暂占用而让整个删除失败、反而留下数据块；
    /// - 不重建根目录，语义更接近「清空内容」而非「删库重建」。
    ///
    /// 单个块删除失败不致命：它是密文，下次 `clear` 或 LRU 仍会回收。
    ///
    /// # Errors
    ///
    /// 根目录无法读取时返回。
    pub fn clear(&self) -> std::io::Result<()> {        if !self.root.exists() {
            std::fs::create_dir_all(&self.root)?;
            omy_core::fsatomic::mark_dir_no_index(&self.root).ok();
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.root)? {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            // 标记文件原样保留，见 MARKER_FILES 的说明
            if path.file_name().map(is_marker).unwrap_or(false) {
                continue;
            }
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(())
    }

    /// 统计单个远程文件在缓存里的覆盖情况。
    ///
    /// `key` 是 `RemoteSource` 拼出的逻辑键（id 混入文件版本），`total_blocks`
    /// 是该文件密文载荷按 [`BLOCK_SIZE`] 向上取整的块数。块路径由哈希决定，
    /// 因此只能逐块枚举判断是否存在——这是 O(块数) 的 metadata 查询，**不能**
    /// 在列目录时对每个文件做（一部 5 GB 电影就是约 5000 次 stat），只在用户
    /// 打开该条目的菜单、或主动移除时按需调用。
    ///
    /// **两层都要查**：永久文件的块只在永久层，只看临时层会把一个已完整下好、
    /// 离线可用的文件报成「未缓存」，界面于是不显示「已缓存」角标、还允许用户
    /// 再点一次「转为永久」。
    #[must_use]
    pub fn stat_file(&self, place: &str, key: &str, total_blocks: u64) -> FileCacheStat {
        let pinned = self.is_pinned(place, key);
        let mut cached_blocks = 0u64;
        let mut cached_bytes = 0u64;
        for block in 0..total_blocks {
            // 与 get() 同序：先永久层、后临时层。转永久搬运中途失败时两层
            // 各有一部分，任取其一都会少算
            let hit = self
                .pinned_path_of(place, key, block)
                .filter(|p| p.is_file())
                .or_else(|| Some(self.path_of(place, key, block)).filter(|p| p.is_file()));
            if let Some(p) = hit {
                if let Ok(m) = std::fs::metadata(&p) {
                    cached_blocks += 1;
                    cached_bytes = cached_bytes.saturating_add(m.len());
                }
            }
        }
        FileCacheStat {
            cached_blocks,
            total_blocks,
            cached_bytes,
            // 空文件（0 块）谈不上「已完整缓存」，按未缓存处理
            fully_cached: total_blocks > 0 && cached_blocks == total_blocks,
            pinned,
        }
    }

    /// 删除单个远程文件的**临时层**缓存块，返回实际释放的字节数。
    ///
    /// 删的只是本地密文块、绝不碰云端文件，因此只读位置也允许这个操作。
    /// 单个块删除失败不计入释放量，也不中断其余块的清理。
    ///
    /// **刻意不动永久层**：「从缓存中移除」与「取消永久保留」是两个不同的用户
    /// 意图，前者顺手把后者的东西删了，就是把用户特意留下来的内容静默删掉——
    /// 取消永久要走 [`BlockCache::unpin_file`] / [`BlockCache::unpin_and_drop`]。
    #[must_use]
    pub fn remove_file_blocks(&self, place: &str, key: &str, total_blocks: u64) -> u64 {
        let mut freed = 0u64;
        for block in 0..total_blocks {
            let p = self.path_of(place, key, block);
            if let Ok(m) = std::fs::metadata(&p) {
                if m.is_file() && std::fs::remove_file(&p).is_ok() {
                    freed = freed.saturating_add(m.len());
                }
            }
        }
        freed
    }
}

/// 两层缓存的用量快照。
///
/// 临时层是「已用 / 上限」，永久层是**绝对值 + 文件计数、没有分母**。
/// 不给永久层任何形式的分母是刻意的：一旦界面画出进度条，用户就会去找
/// 「那上限是多少」，而答案是没有上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct CacheUsage {
    /// 临时层已用字节。
    pub temp_used: u64,
    /// 临时层上限字节；0 表示不限制。**只约束临时层。**
    pub temp_limit: u64,
    /// 永久层已用字节。不受 `temp_limit` 约束。
    pub pinned_used: u64,
    /// 被标记为永久的文件数。
    pub pinned_files: u64,
    /// 本机能否使用永久缓存。为假时界面不该给出「永久保留」。
    pub pinning_available: bool,
}

/// 一个被标记为永久的文件，供「管理永久缓存」列表使用。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PinnedFile {
    /// 位置标识。
    pub place: String,
    /// 条目逻辑键（含文件版本），与缓存键里用的是同一个值。
    pub key: String,
    /// 该文件密文载荷的总块数。
    pub total_blocks: u64,
    /// 当前在永久层实际占用的字节数。
    ///
    /// 与 `total_blocks * BLOCK_SIZE` 不一定相等：末块不足一整块，而且
    /// 下载中途的文件只有一部分块在本地。
    pub used_bytes: u64,
}

impl PinnedFile {
    fn new(place: &str, key: &str, total_blocks: u64) -> Self {
        Self {
            place: place.to_string(),
            key: key.to_string(),
            total_blocks,
            used_bytes: 0,
        }
    }

    /// 标记文件的内容格式。
    ///
    /// 自己拼三行文本而不是上 JSON / TOML：这份记录只有三个字段、由同一个
    /// 模块读写，引一套序列化库反而让「坏文件怎么处理」变成别人的语义。
    /// 用 `\n` 分隔即可，因为 place 与 key 都不含换行（前者是配置里的 id，
    /// 后者是远端条目 id 加十六进制版本号）。
    fn encode(&self) -> Vec<u8> {
        format!("{}\n{}\n{}\n", self.place, self.key, self.total_blocks).into_bytes()
    }

    /// 解析标记文件内容；**格式不对就返回 `None`，不要造一个默认值**。
    ///
    /// 这份文件在应用数据目录里，用户可以手动碰它，磁盘也可能写坏。造个
    /// 默认值会让「管理永久缓存」列表里出现一条 place 为空、谁也取消不掉的
    /// 僵尸记录。
    fn decode(raw: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(raw).ok()?;
        let mut lines = text.split('\n');
        let place = lines.next()?.to_string();
        let key = lines.next()?.to_string();
        let total_blocks = lines.next()?.trim().parse::<u64>().ok()?;
        if place.is_empty() || key.is_empty() {
            return None;
        }
        Some(Self {
            place,
            key,
            total_blocks,
            used_bytes: 0,
        })
    }
}

/// 单个远程文件的密文块缓存覆盖情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FileCacheStat {
    /// 已缓存的密文块数（两层合计）。
    pub cached_blocks: u64,
    /// 该文件密文载荷的总块数。
    pub total_blocks: u64,
    /// 已缓存块占用的字节数。
    pub cached_bytes: u64,
    /// 是否所有块都已缓存（可离线播放的判定依据）。
    pub fully_cached: bool,
    /// 该文件是否被标记为永久保留。
    ///
    /// 界面靠它区分「已缓存」与「永久保留」：前者随时可能被淘汰，后者不会。
    /// 把两者显示成一样，用户就无从判断哪些内容离线时真的还在。
    pub pinned: bool,
}

/// 把区间扩展到块边界，返回涉及的块号范围。
#[must_use]
pub fn blocks_for(offset: u64, len: u64) -> std::ops::RangeInclusive<u64> {
    let first = offset / BLOCK_SIZE;
    let last = offset.saturating_add(len).saturating_sub(1) / BLOCK_SIZE;
    first..=last
}

/// 某一块在给定根目录下的路径。两层共用这一处，只是根不同。
fn block_path(root: &Path, place: &str, id: &str, block: u64) -> PathBuf {
    let hex = digest_hex(&[
        place.as_bytes(),
        b"\0",
        id.as_bytes(),
        b"\0",
        &block.to_le_bytes(),
    ]);
    // 分两级目录：单目录几万个文件会让部分文件系统的列举变慢
    let (a, rest) = hex.split_at(2);
    root.join(a).join(rest)
}

/// 原子写一个缓存块，失败即放弃。
fn write_block(path: &Path, data: &[u8]) {
    if let Some(d) = path.parent() {
        if std::fs::create_dir_all(d).is_err() {
            return;
        }
    }
    // 原子写：半截缓存块会被当成完整数据读出来，解密时表现为
    // 认证失败——而文件本身其实是好的
    omy_core::fsatomic::write_atomic(path, data).ok();
}

/// 在两层之间搬一个块，返回是否搬成功。
///
/// 先 `rename`：同一文件系统内它不复制数据，搬一个 4 GB 文件的几千块才不会变成
/// 几千次整块读写。但缓存位置可以被用户改到另一个盘，那时 `rename` 会失败
/// （Windows 报 `ERROR_NOT_SAME_DEVICE`），所以必须有复制这条退路——只当复制
/// 确实成功才删源，否则宁可两边都留着也不能把唯一一份弄丢。
fn move_file(from: &Path, to: &Path) -> bool {
    if std::fs::rename(from, to).is_ok() {
        return true;
    }
    if std::fs::copy(from, to).is_ok() {
        std::fs::remove_file(from).ok();
        return true;
    }
    false
}

/// 把若干字节片拼起来算 16 字节 blake2，输出小写十六进制。
///
/// 单独抽出来，是为了让缓存键的哈希算法在本文件里只有一处：新增按同样
/// 规则算路径的调用点时，不必把 `update` 的顺序复制一遍。复制出的第二份
/// 一旦顺序或分隔符不同，症状是「明明缓存过却永远不命中」——不报错、
/// 只表现为流量偏高，极难被发现。
fn digest_hex(parts: &[&[u8]]) -> String {
    let mut h = blake2::Blake2bVar::new(16).unwrap_or_else(|_| {
        // 16 字节在合法范围内，这里不可能失败；给个退路只为避免 unwrap
        blake2::Blake2bVar::new(16).expect("blake2 16 字节输出合法")
    });
    for p in parts {
        h.update(p);
    }
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).ok();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// 列出缓存里的所有文件及其大小与访问时间。
fn walk(dir: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else if p.file_name().map(|n| !is_marker(n)).unwrap_or(true) {
            // 目录标记文件（desktop.ini / .nomedia 等）不是缓存块，不计入
            // 用量、也不参与 LRU
            if let Ok(m) = e.metadata() {
                let at = m.accessed().or_else(|_| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                out.push((p, m.len(), at));
            }
        }
    }
    out
}

/// 更新文件访问时间。
///
/// 标准库没有直接设置 atime 的接口，这里用「读一下」触发系统更新。
/// 部分系统挂载了 noatime，此时 LRU 会退化为按修改时间——仍然可用，
/// 只是精度差一些。不引入额外依赖来做这件事。
fn filetime_touch(path: &Path, _now: std::time::SystemTime) {
    use std::io::Read as _;
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut b = [0u8; 1];
        let _ = f.read(&mut b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omy_cache_{}_{name}", std::process::id()));
        std::fs::remove_dir_all(&d).ok();
        d
    }

    /// 造一对「临时层 / 永久层」并列的目录，与生产形态一致（同父、互不嵌套）。
    ///
    /// 不用 `BlockCache::new` 是因为它会把永久层落到真实的用户数据目录去，
    /// 测试不该写那儿；但结构必须跟生产一样，否则测的就不是真实形态。
    fn cache2(name: &str) -> (PathBuf, BlockCache) {
        let base = tmp(name);
        let root = base.join("remote");
        let pinned = base.join("pinned");
        let c = BlockCache::with_pinned_root(&root, 0, Some(pinned)).expect("建两层缓存");
        assert!(c.pinning_available(), "夹具本身要支持永久缓存，否则后面什么都没测");
        (base, c)
    }

    /// 同上但可指定上限。
    fn cache2_limit(name: &str, limit: u64) -> (PathBuf, BlockCache) {
        let base = tmp(name);
        let root = base.join("remote");
        let pinned = base.join("pinned");
        let c = BlockCache::with_pinned_root(&root, limit, Some(pinned)).expect("建两层缓存");
        assert!(c.pinning_available());
        (base, c)
    }

    /// 缓存键不能包含文件名。
    ///
    /// 不这样会怎样：真实文件名（要解密才能得到）会出现在文件系统路径里，
    /// 等于把最敏感的那部分信息明文写到了磁盘上。
    #[test]
    fn key_does_not_contain_filename() {
        let d = tmp("key");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let p = c.path_of("nas", "/影视/沙丘 2.mkv", 3);
        let s = p.to_string_lossy();
        assert!(!s.contains("沙丘"), "路径里不能出现文件名");
        assert!(!s.contains("mkv"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// 不同块、不同文件、不同位置必须得到不同的键。
    #[test]
    fn keys_are_distinct() {
        let d = tmp("distinct");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let a = c.path_of("nas", "/a", 0);
        assert_ne!(a, c.path_of("nas", "/a", 1), "块号要区分");
        assert_ne!(a, c.path_of("nas", "/b", 0), "文件要区分");
        assert_ne!(a, c.path_of("other", "/a", 0), "位置要区分");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 存取要能往返。
    #[test]
    fn put_then_get() {
        let d = tmp("roundtrip");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let data: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        c.put("nas", "/a", 0, &data);
        assert_eq!(c.get("nas", "/a", 0).as_deref(), Some(data.as_slice()));
        assert!(c.get("nas", "/a", 1).is_none(), "未写入的块应未命中");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 单文件缓存统计要正确反映部分 / 完整覆盖。
    ///
    /// 不这样会怎样：菜单上的「从缓存中移除」可能对根本没缓存的文件也出现，
    /// 或「可离线播放（完整缓存）」标错。
    #[test]
    fn stat_file_reports_partial_then_full() {
        let d = tmp("stat");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let chunk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        c.put("nas", "/a\u{1}v1", 0, &chunk);
        c.put("nas", "/a\u{1}v1", 1, &chunk);

        let partial = c.stat_file("nas", "/a\u{1}v1", 3);
        assert_eq!(partial.cached_blocks, 2);
        assert_eq!(partial.total_blocks, 3);
        assert_eq!(partial.cached_bytes, 2 * chunk.len() as u64);
        assert!(!partial.fully_cached, "还差一块不能算完整缓存");

        c.put("nas", "/a\u{1}v1", 2, &chunk);
        assert!(c.stat_file("nas", "/a\u{1}v1", 3).fully_cached);
        // 另一个文件（不同版本键）的缓存不能算到这个文件头上
        assert_eq!(c.stat_file("nas", "/a\u{1}v2", 3).cached_blocks, 0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// 移除单文件缓存只能删该文件（含版本键）的块，不能波及别的文件。
    #[test]
    fn remove_file_blocks_is_scoped_to_that_file() {
        let d = tmp("remove-file");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let a: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
        let b: Vec<u8> = (0..2000u32).map(|i| (i % 251) as u8).collect();
        c.put("nas", "/a\u{1}v1", 0, &a);
        c.put("nas", "/b\u{1}v9", 0, &b);

        let freed = c.remove_file_blocks("nas", "/a\u{1}v1", 1);
        assert_eq!(freed, a.len() as u64, "返回值应等于实际释放字节");
        assert!(c.get("nas", "/a\u{1}v1", 0).is_none(), "目标文件块应已删");
        assert!(c.get("nas", "/b\u{1}v9", 0).is_some(), "别的文件块不能被误删");
        // 再删一次幂等，返回 0
        assert_eq!(c.remove_file_blocks("nas", "/a\u{1}v1", 1), 0);
        std::fs::remove_dir_all(&d).ok();
    }

    /// 超限必须淘汰到上限以内。
    #[test]
    fn evict_respects_limit() {
        let d = tmp("evict");
        let limit = 4096u64;
        let c = BlockCache::new(&d, limit).expect("建缓存");
        let chunk: Vec<u8> = (0..1024u32).map(|i| (i % 253) as u8).collect();
        for i in 0..10u64 {
            c.put("nas", "/a", i, &chunk);
            // 让访问时间有先后之分，否则 LRU 无从排序
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(c.used() > limit, "先要确实超限，否则这个测试什么也没验证");
        c.evict(&[]);
        assert!(c.used() <= limit, "淘汰后必须回落到上限内，实际 {}", c.used());
        std::fs::remove_dir_all(&d).ok();
    }

    /// 正在使用的块必须豁免淘汰。
    ///
    /// 不这样会怎样：播放一个比缓存上限还大的视频时，会一边下载一边把
    /// 自己刚下的块删掉，表现为无限循环下载、进度永远不前进。
    #[test]
    fn evict_keeps_in_use_blocks() {
        let d = tmp("keep");
        let c = BlockCache::new(&d, 2048).expect("建缓存");
        let chunk: Vec<u8> = (0..1024u32).map(|i| (i % 249) as u8).collect();
        for i in 0..8u64 {
            c.put("nas", "/a", i, &chunk);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // 最早写入的两块正在使用——它们恰好是 LRU 最先要淘汰的
        let keep = vec![c.path_of("nas", "/a", 0), c.path_of("nas", "/a", 1)];
        c.evict(&keep);
        for p in &keep {
            assert!(p.exists(), "正在使用的块不能被淘汰: {}", p.display());
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// 上限为 0 表示不限制，不应删任何东西。
    #[test]
    fn zero_limit_never_evicts() {
        let d = tmp("nolimit");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        let chunk = vec![7u8; 4096];
        c.put("nas", "/a", 0, &chunk);
        c.evict(&[]);
        assert!(c.get("nas", "/a", 0).is_some(), "不限制时不应淘汰");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 块划分要覆盖请求区间，且跨块时要包含两端。
    #[test]
    fn block_range_covers_request() {
        assert_eq!(blocks_for(0, 100), 0..=0);
        assert_eq!(blocks_for(0, BLOCK_SIZE), 0..=0);
        assert_eq!(blocks_for(0, BLOCK_SIZE + 1), 0..=1);
        assert_eq!(blocks_for(BLOCK_SIZE - 1, 2), 0..=1, "跨边界要含两块");
        assert_eq!(blocks_for(BLOCK_SIZE * 3, 10), 3..=3);
    }

    /// 清空后应当一块不剩，但目录仍在。
    #[test]
    fn clear_empties_cache() {
        let d = tmp("clear");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        c.put("nas", "/a", 0, &[1, 2, 3]);
        c.clear().expect("清空");
        assert_eq!(c.used(), 0);
        assert!(c.root().exists(), "目录要保留，否则下次写入还要重建");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 目录标记文件既不计入用量，也不被清空删除。
    ///
    /// 不这样会怎样：Windows 的 `desktop.ini`（约 39 字节）会让设置页的缓存
    /// 用量永远清不到 0；而强删它又可能因资源管理器短暂占用导致整个
    /// remove_dir_all 失败，反而留下真正的数据块。
    #[test]
    fn marker_files_are_ignored_and_preserved() {
        let d = tmp("marker");
        let c = BlockCache::new(&d, 0).expect("建缓存");
        // new() 只写当前平台的标记；这里把三种都放上，模拟跨平台残留
        for m in MARKER_FILES {
            std::fs::write(d.join(m), b"[shell]").ok();
        }
        assert_eq!(c.used(), 0, "标记文件不能计入用量");

        c.put("nas", "/a", 0, &[0u8; 100]);
        assert!(c.used() >= 100, "数据块要计入用量");

        c.clear().expect("清空");
        assert_eq!(c.used(), 0, "清空后数据块应为 0");
        for m in MARKER_FILES {
            assert!(d.join(m).exists(), "标记文件 {m} 必须保留");
        }
        assert!(c.root().exists(), "根目录保留");
        std::fs::remove_dir_all(&d).ok();
    }

    // ---------------- 永久缓存 ----------------

    /// 永久层默认取临时层的兄弟目录，不能是它的子目录。
    ///
    /// 不这样会怎样：永久层落在临时层里面，`used()` 会把永久块算进上限、
    /// `evict()` 会淘汰它、`clear()` 会删掉它——「永久」三项承诺一次全废，
    /// 而且毫无征兆。
    #[test]
    fn pinned_root_is_sibling_not_child() {
        let d = tmp("sibling");
        let root = d.join("cache").join("remote");
        let p = sibling_pinned_root(&root).expect("应能算出兄弟目录");
        assert_eq!(p, d.join("cache").join("pinned"));
        assert!(!p.starts_with(&root), "永久层不能落在临时层里面");
        assert!(!root.starts_with(&p), "临时层也不能落在永久层里面");

        // 真建一个也要成立
        let c = BlockCache::new(&root, 0).expect("建缓存");
        if let Some(got) = c.pinned_root() {
            assert!(!got.starts_with(c.root()), "实际的永久层也不能嵌在临时层里");
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// 嵌套的永久层必须被拒绝（能力关掉），而不是将就用。
    ///
    /// 不这样会怎样：调用方传了个子目录进来，代码照常工作、`pin_file` 也返回成功，
    /// 但那些块会被 LRU 淘汰掉——一个假装生效的永久保留。
    #[test]
    fn nested_pinned_root_disables_pinning() {
        let d = tmp("nested");
        let root = d.join("remote");
        let inside = root.join("pinned");
        let c = BlockCache::with_pinned_root(&root, 0, Some(inside)).expect("建缓存");
        assert!(!c.pinning_available(), "嵌套的永久层必须被拒绝");
        assert!(c.pin_file("nas", "/a", 1).is_err(), "拒绝之后 pin 必须报错而不是假装成功");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 永久块不计入临时层用量，也不算进上限的分子。
    ///
    /// 不这样会怎样：设置页会出现「已用 3.9 GB / 2.0 GB」这种越界数字，
    /// 而用户点「清空临时缓存」也降不下去——因为那些字节根本不在临时层。
    #[test]
    fn pinned_bytes_are_not_counted_in_limit() {
        let (d, c) = cache2("usage");
        let chunk: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();

        c.put("nas", "/a", 0, &chunk);
        assert_eq!(c.used(), chunk.len() as u64, "临时块要计入临时用量");
        assert_eq!(c.pinned_used(), 0);

        c.pin_file("nas", "/a", 1).expect("标记永久");
        assert_eq!(c.used(), 0, "搬走之后临时层不应再算这些字节");
        assert_eq!(c.pinned_used(), chunk.len() as u64, "永久层要如实报出占用");

        let u = c.usage();
        assert_eq!(u.temp_used, 0);
        assert_eq!(u.pinned_used, chunk.len() as u64);
        assert_eq!(u.pinned_files, 1, "永久层没有分母，只有绝对值与文件计数");
        assert!(u.pinning_available);
        std::fs::remove_dir_all(&d).ok();
    }

    /// 写满上限后，永久块一个都不能少。
    ///
    /// 不这样会怎样：用户特意留着离线看的电影，会被「随便翻几个大目录」挤掉，
    /// 而且是静默没的——他只在下次点开、发现又要重新下载时才知道。
    #[test]
    fn eviction_never_touches_pinned_blocks() {
        // 上限只有 2 KiB，而下面会往临时层写 10 KiB，必然反复淘汰
        let (d, c) = cache2_limit("evict-pinned", 2048);
        let chunk: Vec<u8> = (0..1024u32).map(|i| (i % 253) as u8).collect();

        // 先放一个永久文件（两块）
        c.put("nas", "/keep", 0, &chunk);
        c.put("nas", "/keep", 1, &chunk);
        let moved = c.pin_file("nas", "/keep", 2).expect("标记永久");
        assert_eq!(moved, 2 * chunk.len() as u64, "两块都要搬进永久层");

        // 再用临时块把上限撑爆
        for i in 0..10u64 {
            c.put("nas", "/junk", i, &chunk);
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        assert!(c.used() > 2048, "先要确实超限，否则这个测试什么也没验证");
        c.evict(&[]);
        assert!(c.used() <= 2048, "临时层要被淘汰回上限内，实际 {}", c.used());

        // 关键断言：永久块还在，而且还能读出来
        assert_eq!(c.pinned_used(), 2 * chunk.len() as u64, "永久块一个都不能少");
        for b in 0..2u64 {
            assert_eq!(
                c.get("nas", "/keep", b).as_deref(),
                Some(chunk.as_slice()),
                "永久块 {b} 应当仍可读出"
            );
        }
        std::fs::remove_dir_all(&d).ok();
    }

    /// 「清空缓存」之后永久目录原封不动。
    ///
    /// 不这样会怎样：清空是一个危险按钮——用户以为在清可回收的缓存，
    /// 实际把自己特意留下来的内容也删了，而且不可恢复。
    #[test]
    fn clear_leaves_pinned_intact() {
        let (d, c) = cache2("clear-pinned");
        let chunk: Vec<u8> = (0..2048u32).map(|i| (i % 247) as u8).collect();
        c.put("nas", "/keep", 0, &chunk);
        c.pin_file("nas", "/keep", 1).expect("标记永久");
        c.put("nas", "/junk", 0, &chunk);

        c.clear().expect("清空临时层");
        assert_eq!(c.used(), 0, "临时层要清空");
        assert_eq!(c.pinned_used(), chunk.len() as u64, "永久层不能被清掉");
        assert!(c.is_pinned("nas", "/keep"), "永久标记也不能被清掉");
        assert_eq!(c.get("nas", "/keep", 0).as_deref(), Some(chunk.as_slice()));
        std::fs::remove_dir_all(&d).ok();
    }

    /// 已标记为永久的文件，之后新下载的块要**直接落进永久层**。
    ///
    /// 不这样会怎样：标记只影响「已经缓存的那部分」，用户先点永久再让它下载
    /// （这正是产品语义上的主路径——转永久是一次真实的下载任务），下到的块仍
    /// 落在临时层被淘汰。表现是「标记明明生效了，离线时却只有前面一小段」。
    #[test]
    fn blocks_written_after_pinning_go_to_pinned_layer() {
        let (d, c) = cache2("write-after-pin");
        let chunk: Vec<u8> = (0..1500u32).map(|i| (i % 241) as u8).collect();

        // 一块都还没下就先标记（total_blocks 是预期块数）
        c.pin_file("nas", "/later", 3).expect("先标记");
        assert_eq!(c.pinned_used(), 0, "此刻还没有任何块");

        // 之后才下载
        c.put("nas", "/later", 0, &chunk);
        c.put("nas", "/later", 1, &chunk);

        assert_eq!(c.used(), 0, "已是永久的文件不该再往临时层写");
        assert_eq!(c.pinned_used(), 2 * chunk.len() as u64, "新块必须落进永久层");
        let st = c.stat_file("nas", "/later", 3);
        assert_eq!(st.cached_blocks, 2, "统计要能看见永久层里的块");
        assert!(st.pinned, "统计要报出永久状态，界面靠它区分「已缓存」与「永久」");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 取消永久后，内容必须**真的回到 LRU 的管辖范围**，不是只改个标记。
    ///
    /// 不这样会怎样：取消之后那些字节仍在永久层、既不计入上限也不被淘汰，
    /// 于是「取消永久」变成一个只改显示的假动作，用户腾不出空间、
    /// 也无法理解为什么用量没变。
    #[test]
    fn unpinned_blocks_rejoin_lru() {
        let (d, c) = cache2_limit("unpin-lru", 1024);
        let chunk: Vec<u8> = (0..1600u32).map(|i| (i % 239) as u8).collect();
        c.put("nas", "/x", 0, &chunk);
        c.pin_file("nas", "/x", 1).expect("标记永久");
        assert_eq!(c.used(), 0);
        assert!(c.pinned_used() > 1024, "这块本身就比上限大，取消后必然会被淘汰");

        let moved = c.unpin_file("nas", "/x", 1).expect("取消永久");
        assert_eq!(moved, chunk.len() as u64, "应把块搬回临时层而不是丢掉");
        assert!(!c.is_pinned("nas", "/x"), "标记必须真的没了");
        assert_eq!(c.pinned_used(), 0, "永久层要空出来");
        assert_eq!(c.used(), chunk.len() as u64, "搬回后必须计入临时层用量");

        // 真正的证据：它现在会被淘汰
        c.evict(&[]);
        assert!(
            c.get("nas", "/x", 0).is_none(),
            "取消永久后这些内容必须进入 LRU、可被清理——否则只是改了个标记"
        );
        std::fs::remove_dir_all(&d).ok();
    }

    /// 取消并腾空间这条路径要真的把字节删掉。
    ///
    /// 不这样会怎样：用户取消永久的动机之一就是腾空间，若只搬回临时层，
    /// 磁盘一个字节都没少，而他明确要的是少。
    #[test]
    fn unpin_and_drop_frees_bytes() {
        let (d, c) = cache2("unpin-drop");
        let chunk: Vec<u8> = (0..900u32).map(|i| (i % 233) as u8).collect();
        c.put("nas", "/y", 0, &chunk);
        c.pin_file("nas", "/y", 1).expect("标记永久");

        let freed = c.unpin_and_drop("nas", "/y", 1).expect("取消并删除");
        assert_eq!(freed, chunk.len() as u64, "返回值应等于实际释放字节");
        assert_eq!(c.pinned_used(), 0);
        assert_eq!(c.used(), 0, "不能顺手搬回临时层，那就没腾出空间");
        assert!(c.get("nas", "/y", 0).is_none(), "块应当已不存在");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 永久清单必须可枚举，且不含文件名。
    ///
    /// 不这样会怎样：没有这份清单，「管理永久缓存」就做不出来，用户只能回到
    /// 原位置里一个个找回文件才能取消——Telegram 这类位置上那条消息可能
    /// 根本找不回来了。而清单里若写了文件名，等于把解密出来的信息落盘。
    #[test]
    fn pinned_list_is_enumerable_without_filenames() {
        let (d, c) = cache2("list");
        let chunk: Vec<u8> = (0..700u32).map(|i| (i % 229) as u8).collect();
        c.put("nas", "/影视/沙丘 2.mkv\u{1}v1", 0, &chunk);
        c.pin_file("nas", "/影视/沙丘 2.mkv\u{1}v1", 1).expect("标记 1");
        c.pin_file("tg", "/chat/42\u{1}v7", 2).expect("标记 2");

        let list = c.list_pinned();
        assert_eq!(list.len(), 2, "两个文件都要能列出来");
        assert_eq!(c.pinned_file_count(), 2);
        // 排序稳定：nas 在 tg 之前
        assert_eq!(list[0].place, "nas");
        assert_eq!(list[1].place, "tg");
        assert_eq!(list[0].total_blocks, 1);
        assert_eq!(list[1].total_blocks, 2);
        assert_eq!(list[0].used_bytes, chunk.len() as u64, "要报出实际占用");
        assert_eq!(list[1].used_bytes, 0, "只标记未下载的文件占 0 字节");

        // 标记文件的路径里不能出现条目名
        let mark = c.mark_path("nas", "/影视/沙丘 2.mkv\u{1}v1").expect("标记路径");
        let s = mark.to_string_lossy();
        assert!(!s.contains("沙丘"), "标记路径里不能出现条目名");
        assert!(!s.contains("mkv"));

        // 取消之后就不在清单里
        c.unpin_file("nas", "/影视/沙丘 2.mkv\u{1}v1", 1).expect("取消");
        assert_eq!(c.list_pinned().len(), 1);
        std::fs::remove_dir_all(&d).ok();
    }

    /// `list_pinned` 列出来的每一项，都必须能**原样**拿去取消并真的释放空间。
    ///
    /// 不这样会怎样：这是「管理永久缓存」能不能用的全部意义。列表给的是
    /// `(place, key, total_blocks)`，而 key 里含文件版本哈希、界面拼不回
    /// 原始路径，size 也无从得知——若取消那条路要求路径或大小，列表里每一项
    /// 就都点不动，用户还是只能回原位置一个个找，而 Telegram 上那条消息
    /// 可能根本找不回来了。
    ///
    /// 所以这条断言刻意**只用列表项里有的字段**去取消，并检查空间真的被释放。
    /// 只断言「列表非空」抓不到这个——那正是「断言停在差异出现之前」。
    #[test]
    fn every_listed_pin_can_be_cancelled_with_only_its_own_fields() {
        let (d, c) = cache2("actionable");
        let chunk: Vec<u8> = (0..900u32).map(|i| (i % 233) as u8).collect();
        c.put("tg", "tg:-100123:77\u{1}v3", 0, &chunk);
        c.put("tg", "tg:-100123:77\u{1}v3", 1, &chunk);
        c.pin_file("tg", "tg:-100123:77\u{1}v3", 2).expect("标记");

        let before = c.pinned_used();
        assert!(before > 0, "夹具本身要真的占了空间，否则后面什么都没测");

        let list = c.list_pinned();
        let [item] = list.as_slice() else {
            panic!("应当正好列出一项，实际 {list:?}");
        };

        // 关键：**只用列表项自己带的三个字段**，不借助任何外部信息
        let freed = c
            .unpin_and_drop(&item.place, &item.key, item.total_blocks)
            .expect("列出来的项必须能直接取消");

        assert!(freed > 0, "取消必须真的释放字节，而不是只摘掉标记");
        assert_eq!(freed, before, "释放的字节数要与它原本占的一致");
        assert_eq!(c.pinned_used(), 0, "永久层应当真的空了");
        assert!(c.list_pinned().is_empty(), "取消之后不该还在清单里");

        std::fs::remove_dir_all(&d).ok();
    }

    /// 坏掉的标记文件要被跳过，不能变成一条谁也取消不掉的僵尸记录。
    ///
    /// 不这样会怎样：这份文件在用户能碰到的目录里，写坏或被手动改过之后，
    /// 若解析失败时造个默认值，列表里会出现 place 为空、或块数为 0 的条目，
    /// 点取消也取消不掉（块数为 0 时循环一次都不走，块永远留着），
    /// 用户只能去手动删目录。
    ///
    /// **三种坏法都要测**：截断、块数不是数字、字段为空。它们走的是 `decode`
    /// 里三条不同的返回路径，只测一种会让另两条的缺陷溜过去——实测确认过，
    /// 只放「截断」那一种时，把块数解析改成 `unwrap_or(0)` 的变异能存活。
    #[test]
    fn corrupt_mark_is_skipped() {
        let (d, c) = cache2("corrupt");
        c.pin_file("nas", "/good\u{1}v1", 1).expect("好的标记");

        let marks = c.pinned_root().expect("永久层").join(PINNED_MARKS);
        let bad_cases: [(&str, &[u8]); 3] = [
            // 截断：只有一行，连条目键都没有
            ("truncated", b"only-one-line"),
            // 块数不是数字：前两行齐全，第三行是垃圾
            ("bad-count", b"nas\n/x\x01v1\nnot-a-number\n"),
            // 位置为空：格式对但内容没意义，取消时键对不上
            ("empty-place", b"\n/x\x01v1\n3\n"),
        ];
        for (name, body) in bad_cases {
            let p = marks.join("zz").join(name);
            std::fs::create_dir_all(p.parent().expect("父目录")).expect("建目录");
            std::fs::write(&p, body).expect("写坏标记");
        }

        let list = c.list_pinned();
        assert_eq!(list.len(), 1, "三种坏标记都要被跳过，只留下好的那条：{list:?}");
        assert_eq!(list[0].place, "nas");
        assert_eq!(list[0].total_blocks, 1);
        assert!(!list.iter().any(|r| r.place.is_empty()), "不能出现空 place 的僵尸记录");
        assert!(
            !list.iter().any(|r| r.total_blocks == 0),
            "块数为 0 的记录取消时一块都清不掉，等于清不干净"
        );
        assert_eq!(c.pinned_file_count(), 4, "计数按标记文件数算，坏文件也占位（好让用户看得见并能清理）");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 重复标记与重复取消都要幂等。
    ///
    /// 不这样会怎样：界面上的勾是异步刷新的，用户手快点两下很常见；
    /// 第二次若报错或把字节数重复累加，显示的用量就是错的。
    #[test]
    fn pin_and_unpin_are_idempotent() {
        let (d, c) = cache2("idempotent");
        let chunk: Vec<u8> = (0..500u32).map(|i| (i % 227) as u8).collect();
        c.put("nas", "/z", 0, &chunk);

        assert_eq!(c.pin_file("nas", "/z", 1).expect("首次"), chunk.len() as u64);
        assert_eq!(c.pin_file("nas", "/z", 1).expect("再次"), 0, "已经搬完了，第二次搬 0 字节");
        assert_eq!(c.pinned_file_count(), 1, "不能变成两条记录");

        assert_eq!(c.unpin_file("nas", "/z", 1).expect("首次取消"), chunk.len() as u64);
        assert_eq!(c.unpin_file("nas", "/z", 1).expect("再次取消"), 0);
        assert!(!c.is_pinned("nas", "/z"));
        std::fs::remove_dir_all(&d).ok();
    }

    /// 不支持永久缓存时，能力位为假且 `pin` 报错、其余照常工作。
    ///
    /// 不这样会怎样：界面会摆出一个点了没反应的「永久保留」——比没有这个
    /// 开关更糟，用户会以为已经保住了。
    #[test]
    fn without_pinned_root_everything_else_still_works() {
        let d = tmp("nopinned");
        let c = BlockCache::with_pinned_root(&d, 0, None).expect("建缓存");
        assert!(!c.pinning_available());
        assert_eq!(c.pinned_used(), 0);
        assert_eq!(c.pinned_file_count(), 0);
        assert!(c.list_pinned().is_empty());
        assert!(!c.is_pinned("nas", "/a"));
        assert!(c.pin_file("nas", "/a", 1).is_err(), "不能假装标记成功");
        // 取消是幂等的，没有标记时不该报错
        assert_eq!(c.unpin_file("nas", "/a", 1).expect("取消"), 0);

        // 普通缓存照常
        let chunk: Vec<u8> = (0..300u32).map(|i| (i % 223) as u8).collect();
        c.put("nas", "/a", 0, &chunk);
        assert_eq!(c.get("nas", "/a", 0).as_deref(), Some(chunk.as_slice()));
        assert!(!c.usage().pinning_available);
        std::fs::remove_dir_all(&d).ok();
    }

    /// 「从缓存中移除」不能顺手删掉永久块。
    ///
    /// 不这样会怎样：它与「取消永久保留」是两个不同的用户意图，前者把后者的
    /// 东西删了，就是把用户特意留下的内容静默删掉。
    #[test]
    fn remove_file_blocks_does_not_touch_pinned() {
        let (d, c) = cache2("remove-vs-pin");
        let chunk: Vec<u8> = (0..800u32).map(|i| (i % 211) as u8).collect();
        c.put("nas", "/p", 0, &chunk);
        c.pin_file("nas", "/p", 1).expect("标记永久");

        let freed = c.remove_file_blocks("nas", "/p", 1);
        assert_eq!(freed, 0, "永久块不在临时层，这里没有可释放的字节");
        assert_eq!(c.pinned_used(), chunk.len() as u64, "永久块必须还在");
        assert!(c.is_pinned("nas", "/p"), "标记也不能被顺手清掉");
        std::fs::remove_dir_all(&d).ok();
    }
}
