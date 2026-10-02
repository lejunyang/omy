//! 远程缓存。
//!
//! # 临时层：哈希分块
//!
//! 临时缓存按 1 MiB 块保存，供播放器 Range 读取、seek、LRU 淘汰与断点续传。
//! 路径使用哈希，不泄露文件名；`.omy` 内容缓存的是远端原始密文字节。
//!
//! # 永久层：完整原文件
//!
//! 永久缓存不再复用内部块格式，而是保存远端**完整原文件**。这样用户可以直接
//! 查看、复制、备份、合并目录；另一台机器重新添加同一远程后也能直接复用。
//! `.omy` 文件仍是加密的 `.omy` 原文件；Telegram / WebDAV 上的普通文件则按
//! 服务端原格式保存，因此用户能用其它程序直接打开。
//!
//! 两层仍放在并列目录：临时层是调用方给的 `cache/remote/`，永久层是兄弟目录
//! `cache/pinned/`。清空与 LRU 只遍历临时层，从结构上保证不会误删永久文件。
//!
//! 永久目录按稳定远程身份和稳定条目身份组织，而不是本机的 `p1` / `p2`：
//!
//! - Telegram：`telegram/<user_id>/<chat_id>/<message_id>/<服务端文件名>`；
//! - WebDAV：`webdav/<主机名+源哈希>/<条目哈希>/<服务端文件名>`。
//!
//! 每个条目目录旁有 `.omy-pin.json`，只记录远端公开身份、原文件名和大小，供
//! 设置页枚举和安全删除。文件名若含当前文件系统不能表示的字符，会做确定性
//! 转义并保留原扩展名；未触发转义时与服务端文件名逐字一致。

use std::path::{Path, PathBuf};

use blake2::digest::{Update, VariableOutput};

use crate::pinned::{self, PinnedTarget};

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
/// `clear()` 会把永久文件一并算上、一并删掉，「永久」当场作废且毫无征兆。
///
/// 这样定义还让「更改缓存位置」自动带着永久层一起走——两处各自解析路径迟早
/// 漂移，症状是改完位置后永久文件还留在旧盘上、界面却显示 0 字节。
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
    /// 完整永久文件的根目录；`None` 表示这台机器上拿不到应用数据目录，
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
        // `used()` / `evict()` / `clear()` 会把永久文件一并算上、一并删掉，
        // 「永久」这个承诺当场作废，而且失效得毫无征兆。宁可关掉这个能力，
        // 也不要留一个会静默失效的实现。
        let pinned_root = pinned_root
            .filter(|p| !p.starts_with(&root) && !root.starts_with(p))
            .and_then(|p| {
                if std::fs::create_dir_all(&p).is_err() {
                    return None;
                }
                omy_core::fsatomic::mark_dir_no_index(&p).ok();
                Some(p)
            });
        Ok(Self {
            root,
            limit,
            pinned_root,
        })
    }

    /// 缓存根目录（临时层）。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 完整永久文件的根目录；`None` 表示本机不支持永久缓存。
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
    /// **不含永久文件**：上限约束的是「可以随时被回收的那部分」，把永久文件算
    /// 进来会让「已用 3.9 / 2.0 GB」这种越界数字出现在设置页上，而用户点
    /// 「清空缓存」也降不下去。永久层的占用由 [`BlockCache::pinned_used`]
    /// 单独给出，两个数字分开展示。
    #[must_use]
    pub fn used(&self) -> u64 {
        walk(&self.root).iter().map(|(_, sz, _)| *sz).sum()
    }

    /// 永久层完整原文件的总字节数；不含 `.omy-pin.json` 元数据。
    #[must_use]
    pub fn pinned_used(&self) -> u64 {
        self.pinned_root.as_deref().map(pinned::used).unwrap_or(0)
    }

    /// 永久层完整原文件数；只统计元数据与文件均完整匹配的条目。
    #[must_use]
    pub fn pinned_file_count(&self) -> u64 {
        self.pinned_root
            .as_deref()
            .map(|r| pinned::list(r).len() as u64)
            .unwrap_or(0)
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

    /// 完整永久文件是否存在且版本、大小与当前远端条目一致。
    #[must_use]
    pub fn is_pinned_target(&self, target: &PinnedTarget) -> bool {
        self.pinned_root
            .as_deref()
            .and_then(|r| pinned::complete_path(r, target))
            .is_some()
    }

    /// 从完整永久文件读取远端原始字节区间。
    #[must_use]
    pub fn get_pinned_range(
        &self,
        target: &PinnedTarget,
        offset: u64,
        len: u64,
    ) -> Option<Vec<u8>> {
        let root = self.pinned_root.as_deref()?;
        pinned::read_range(root, target, offset, len)
    }

    /// 在版本键未知时，只按稳定来源、条目和大小确认完整永久文件存在。
    ///
    /// GUI 用它决定是否可以跳过远端连接；这里会同时校验元数据与真实文件长度，
    /// 不能把残留 `.omy-pin.json` 当成可离线打开的成品。
    #[must_use]
    pub fn has_pinned_candidate(
        &self,
        source: &crate::virtuals::SourceRef,
        item_id: &str,
        total_size: u64,
    ) -> bool {
        self.pinned_root
            .as_deref()
            .and_then(|root| pinned::candidate_path(root, source, item_id, total_size))
            .is_some()
    }

    /// 在 `.omy` 头部尚未读到、版本键还未知时，按稳定来源与条目读取候选区间。
    /// 调用方拿到头部后仍会构造正式目标并校验版本。
    #[must_use]
    pub fn get_pinned_candidate_range(
        &self,
        source: &crate::virtuals::SourceRef,
        item_id: &str,
        total_size: u64,
        offset: u64,
        len: u64,
    ) -> Option<Vec<u8>> {
        let root = self.pinned_root.as_deref()?;
        pinned::candidate_range(root, source, item_id, total_size, offset, len)
    }

    /// 准备完整永久文件的临时写入路径。
    pub fn prepare_pinned(&self, target: &PinnedTarget) -> crate::Result<PathBuf> {
        let root = self
            .pinned_root
            .as_deref()
            .ok_or(crate::Error::Unsupported(
                "本机没有可写的永久缓存目录，无法永久保留",
            ))?;
        pinned::prepare(root, target)
            .map(|p| p.part)
            .map_err(crate::Error::Io)
    }

    /// 将已写完的 `.part` 原子提交为用户可见的完整原文件。
    pub fn commit_pinned(&self, target: &PinnedTarget) -> crate::Result<u64> {
        let root = self
            .pinned_root
            .as_deref()
            .ok_or(crate::Error::Unsupported(
                "本机没有可写的永久缓存目录，无法永久保留",
            ))?;
        pinned::commit(root, target).map_err(crate::Error::Io)
    }

    /// 删除一个完整永久文件。临时分块缓存保持不动。
    pub fn remove_pinned(&self, target: &PinnedTarget) -> crate::Result<u64> {
        let Some(root) = self.pinned_root.as_deref() else {
            return Ok(0);
        };
        pinned::remove(root, target).map_err(crate::Error::Io)
        }

    /// 列出全部完整永久文件。
    #[must_use]
    pub fn list_pinned(&self) -> Vec<crate::pinned::PinnedFile> {
        self.pinned_root
            .as_deref()
            .map(pinned::list)
            .unwrap_or_default()
    }

    /// 按设置页返回的安全相对目录删除一项，不需要联网或重新定位远程文件。
    pub fn remove_pinned_relative(&self, relative: &str) -> crate::Result<u64> {
        let Some(root) = self.pinned_root.as_deref() else {
            return Ok(0);
        };
        pinned::remove_relative(root, relative).map_err(crate::Error::Io)
    }

    /// 读取一个临时缓存块；未命中返回 `None`。
    #[must_use]
    pub fn get(&self, place: &str, id: &str, block: u64) -> Option<Vec<u8>> {
        let p = self.path_of(place, id, block);
        let data = std::fs::read(&p).ok()?;
        // 更新访问时间供 LRU 使用。
        let now = std::time::SystemTime::now();
        filetime_touch(&p, now);
        Some(data)
    }

    /// 写入一个临时缓存块。完整永久文件由 [`Self::prepare_pinned`] 与
    /// [`Self::commit_pinned`] 独立管理，不把内部块格式暴露给用户。
    pub fn put(&self, place: &str, id: &str, block: u64, data: &[u8]) {
        write_block(&self.path_of(place, id, block), data);
    }

    /// 元数据命名空间前缀。对话列表/头像/列表快照等**不是文件内容块**，但同样
    /// 值得跨重启缓存。把它们当作 `place = "meta:<kind>"` 的整块 blob 存进同一
    /// 临时层：**天然共用 LRU、同一 `cache_limit`、同一淘汰**，除永久层外一切都
    /// 在 LRU 里——这正是「不再另造一套缓存设施」的做法。
    ///
    /// 元数据永远走临时层（不 pin）：它会变、也不值得永久占坑。
    fn meta_place(kind: &str) -> String {
        format!("meta:{kind}")
    }

    /// 存一份元数据 blob（整块，block=0）。`kind` 分命名空间（dialogs/avatar/
    /// list/...），`key` 建议带新鲜度因子或用内容 hash，见各调用点。
    ///
    /// 写完顺手 `evict` 一次，让元数据也受 `cache_limit` 约束、不无限堆积。
    pub fn put_meta(&self, kind: &str, key: &str, data: &[u8]) {
        let place = Self::meta_place(kind);
        write_block(&self.path_of(&place, key, 0), data);
        // 元数据通常很小，但海量小对象累积也可能超限；写后淘汰一次。
        // keep 为空：元数据没有「正在使用中不能删」的块（读到就整体拿走了）。
        self.evict(&[]);
    }

    /// 读一份元数据 blob。未命中或读失败返回 `None`（降级为走网络重取）。
    #[must_use]
    pub fn get_meta(&self, kind: &str, key: &str) -> Option<Vec<u8>> {
        let place = Self::meta_place(kind);
        let p = self.path_of(&place, key, 0);
        let data = std::fs::read(&p).ok()?;
        // 命中即更新访问时间，供 LRU 判「最近用过」
        filetime_touch(&p, std::time::SystemTime::now());
        Some(data)
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
    pub fn clear(&self) -> std::io::Result<()> {
        if !self.root.exists() {
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

    /// 统计单个远程文件在临时分块缓存里的覆盖情况。
    ///
    /// 完整永久文件由 `RemoteSource::cache_stat` 在此结果上覆盖为完整命中；这里
    /// 只负责临时块，避免底层块缓存认识 provider 的稳定身份。
    #[must_use]
    pub fn stat_file(&self, place: &str, key: &str, total_blocks: u64) -> FileCacheStat {
        let mut cached_blocks = 0u64;
        let mut cached_bytes = 0u64;
        for block in 0..total_blocks {
            let p = self.path_of(place, key, block);
            if let Ok(m) = std::fs::metadata(&p)
                && m.is_file()
            {
                    cached_blocks += 1;
                    cached_bytes = cached_bytes.saturating_add(m.len());
                }
        }
        FileCacheStat {
            cached_blocks,
            total_blocks,
            cached_bytes,
            fully_cached: total_blocks > 0 && cached_blocks == total_blocks,
            pinned: false,
        }
    }

    /// 删除单个远程文件的**临时层**缓存块，返回实际释放的字节数。
    ///
    /// 删的只是本地密文块、绝不碰云端文件，因此只读位置也允许这个操作。
    /// 单个块删除失败不计入释放量，也不中断其余块的清理。
    ///
    /// **刻意不动永久层**：「从缓存中移除」与「取消永久保留」是两个不同的用户
    /// 意图，前者顺手把后者的东西删了，就是把用户特意留下来的内容静默删掉——
    /// 取消永久要走 [`BlockCache::remove_pinned`]。
    #[must_use]
    pub fn remove_file_blocks(&self, place: &str, key: &str, total_blocks: u64) -> u64 {
        let mut freed = 0u64;
        for block in 0..total_blocks {
            let p = self.path_of(place, key, block);
            if let Ok(m) = std::fs::metadata(&p)
                && m.is_file()
                && std::fs::remove_file(&p).is_ok()
            {
                    freed = freed.saturating_add(m.len());
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

/// 单个远程文件的缓存覆盖情况。
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
    if let Some(d) = path.parent()
        && std::fs::create_dir_all(d).is_err()
    {
            return;
        }
    // 原子写：半截缓存块会被当成完整数据读出来，解密时表现为
    // 认证失败——而文件本身其实是好的
    omy_core::fsatomic::write_atomic(path, data).ok();
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
                let at = m
                    .accessed()
                    .or_else(|_| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
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
        assert!(
            c.pinning_available(),
            "夹具本身要支持永久缓存，否则后面什么都没测"
        );
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

    /// 元数据 blob 落盘：写盘 → **换新实例（模拟重启）** → 仍能读回；
    /// 且它落在临时层、受 LRU 淘汰、超限时被清；不同 (kind,key) 不串。
    ///
    /// 不这样会怎样：元数据只在内存里的话，重启后对话列表/列表快照全丢、每次
    /// 开应用都重新走网络——正是用户报的「重启全部重新加载」。用独立实例读回
    /// 才能排除「只是内存 HashMap 还在」的假通过。
    #[test]
    fn meta_blob_survives_restart_and_obeys_lru() {
        let d = tmp("meta-blob");
        let payload = b"[{\"name\":\"a.jpg\",\"size\":123}]".to_vec();
        // 会话1：写盘（上限很大，不触发淘汰）
        {
            let c = BlockCache::new(&d, 100 * 1024 * 1024).expect("建缓存");
            assert!(
                c.get_meta("list", "p3\0tg:1\0media").is_none(),
                "空目录读不到"
            );
            c.put_meta("list", "p3\0tg:1\0media", &payload);
            assert_eq!(
                c.get_meta("list", "p3\0tg:1\0media").as_deref(),
                Some(&payload[..]),
                "同实例写完能读回"
            );
        }
        // 会话2（新实例=重启）：仍能读回
        {
            let c = BlockCache::new(&d, 100 * 1024 * 1024).expect("重开缓存");
            assert_eq!(
                c.get_meta("list", "p3\0tg:1\0media").as_deref(),
                Some(&payload[..]),
                "重启后（新实例）必须仍能读回，否则等于没落盘"
            );
            // 不同 kind / key 不串
            assert!(
                c.get_meta("dialogs", "p3\0tg:1\0media").is_none(),
                "kind 不同不该命中"
            );
            assert!(
                c.get_meta("list", "p3\0tg:2\0media").is_none(),
                "key 不同不该命中"
            );
        }
        // LRU：把上限压到极小、灌一堆元数据，超限的被淘汰（永久层不参与、这里也没 pin）
        {
            let c = BlockCache::new(&d, 4096).expect("小上限缓存");
            let big: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
            for i in 0..10 {
                c.put_meta("list", &format!("k{i}"), &big);
            }
            // put_meta 每次写后 evict，总量应被压到上限附近，不会 10*3000 无限堆
            assert!(
                c.used() <= 4096 + big.len() as u64,
                "元数据也必须受 cache_limit LRU 约束，实际 used={}",
                c.used()
            );
        }
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
        assert!(
            c.get("nas", "/b\u{1}v9", 0).is_some(),
            "别的文件块不能被误删"
        );
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
        assert!(
            c.used() <= limit,
            "淘汰后必须回落到上限内，实际 {}",
            c.used()
        );
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

    // ---------------- 完整文件永久缓存 ----------------

    fn telegram_target(size: u64) -> crate::pinned::PinnedTarget {
        crate::pinned::PinnedTarget::new(
            crate::virtuals::SourceRef::telegram(42),
            "tg:-10088:19",
            "video.mp4",
            format!("plain{size}"),
            size,
        )
    }

    /// 永久层默认取临时层的兄弟目录，不能是它的子目录。
    #[test]
    fn pinned_root_is_sibling_not_child() {
        let d = tmp("sibling");
        let root = d.join("cache").join("remote");
        let p = sibling_pinned_root(&root).expect("应能算出兄弟目录");
        assert_eq!(p, d.join("cache").join("pinned"));
        assert!(!p.starts_with(&root), "永久层不能落在临时层里面");
        assert!(!root.starts_with(&p), "临时层也不能落在永久层里面");
        std::fs::remove_dir_all(&d).ok();
    }

    /// 嵌套永久层必须禁用，避免临时清理误删用户保留的完整文件。
    #[test]
    fn nested_pinned_root_disables_pinning() {
        let d = tmp("nested");
        let root = d.join("remote");
        let c = BlockCache::with_pinned_root(&root, 0, Some(root.join("pinned"))).expect("建缓存");
        assert!(!c.pinning_available());
        assert!(c.prepare_pinned(&telegram_target(3)).is_err());
        std::fs::remove_dir_all(&d).ok();
    }

    /// 完整永久文件不计入临时上限，清空临时缓存也不能碰它。
    #[test]
    fn complete_pinned_file_is_separate_from_temp_cache() {
        let (d, c) = cache2("complete-file");
        let data: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        let target = telegram_target(data.len() as u64);
        let part = c.prepare_pinned(&target).expect("准备永久文件");
        std::fs::write(&part, &data).expect("写完整原文件");
        c.commit_pinned(&target).expect("提交永久文件");
        c.put("temp", "/x", 0, &data);

        assert_eq!(c.pinned_used(), data.len() as u64);
        assert_eq!(c.pinned_file_count(), 1);
            assert_eq!(
            c.get_pinned_range(&target, 10, 20).as_deref(),
            Some(&data[10..30])
            );
        assert_eq!(c.used(), data.len() as u64);

        c.clear().expect("清空临时缓存");
        assert_eq!(c.used(), 0);
        assert_eq!(
            c.get_pinned_range(&target, 10, 20).as_deref(),
            Some(&data[10..30])
        );
        assert!(c.is_pinned_target(&target));
        std::fs::remove_dir_all(&d).ok();
    }

    /// 设置页列出的相对目录必须可以在无网络时安全删除。
    #[test]
    fn listed_complete_file_can_be_removed_offline() {
        let (d, c) = cache2("list-remove");
        let target = telegram_target(5);
        let part = c.prepare_pinned(&target).expect("准备");
        std::fs::write(&part, b"hello").expect("写文件");
        c.commit_pinned(&target).expect("提交");

        let list = c.list_pinned();
        let [item] = list.as_slice() else {
            panic!("应当正好列出一项，实际 {list:?}");
        };
        assert_eq!(item.name, "video.mp4");
        assert_eq!(item.item_id, "tg:-10088:19");
        assert_eq!(item.used_bytes, 5);
        let freed = c
            .remove_pinned_relative(&item.relative_dir)
            .expect("离线删除");
        assert_eq!(freed, 5);
        assert!(c.list_pinned().is_empty());
        std::fs::remove_dir_all(&d).ok();
    }

    /// 不支持永久缓存时，临时分块功能仍正常。
    #[test]
    fn without_pinned_root_temp_cache_still_works() {
        let d = tmp("nopinned");
        let c = BlockCache::with_pinned_root(&d, 0, None).expect("建缓存");
        assert!(!c.pinning_available());
        assert!(c.prepare_pinned(&telegram_target(3)).is_err());
        let data = [1u8, 2, 3];
        c.put("nas", "/a", 0, &data);
        assert_eq!(c.get("nas", "/a", 0).as_deref(), Some(&data[..]));
        std::fs::remove_dir_all(&d).ok();
    }
}
