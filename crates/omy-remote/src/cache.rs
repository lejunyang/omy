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

/// 判断一个文件名是否为目录标记文件。
fn is_marker(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|n| MARKER_FILES.contains(&n))
        .unwrap_or(false)
}

/// 磁盘上的密文块缓存。
#[derive(Debug, Clone)]
pub struct BlockCache {
    root: PathBuf,
    /// 上限字节数；0 表示不限制。
    limit: u64,
}

impl BlockCache {
    /// 在给定目录下建立缓存。
    ///
    /// # Errors
    ///
    /// 目录无法创建时返回。
    pub fn new(root: impl Into<PathBuf>, limit: u64) -> std::io::Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        // 标记为不被系统索引：缓存是密文，进搜索索引没有意义，
        // 而索引器读取会造成额外的磁盘与耗电开销
        omy_core::fsatomic::mark_dir_no_index(&root).ok();
        Ok(Self { root, limit })
    }

    /// 缓存根目录。
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 配置的上限字节数；`0` 表示不限制。
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// 当前占用字节数。
    #[must_use]
    pub fn used(&self) -> u64 {
        walk(&self.root).iter().map(|(_, sz, _)| *sz).sum()
    }

    /// 某一块的缓存路径。
    ///
    /// 键里不含文件名：那需要解密才能得到，写进路径等于把它泄露到
    /// 文件系统里（而文件名往往比内容更能说明问题）。
    #[must_use]
    pub fn path_of(&self, place: &str, id: &str, block: u64) -> PathBuf {
        let mut h = blake2::Blake2bVar::new(16).unwrap_or_else(|_| {
            // 16 字节在合法范围内，这里不可能失败；给个退路只为避免 unwrap
            blake2::Blake2bVar::new(16).expect("blake2 16 字节输出合法")
        });
        h.update(place.as_bytes());
        h.update(b"\0");
        h.update(id.as_bytes());
        h.update(b"\0");
        h.update(&block.to_le_bytes());
        let mut out = [0u8; 16];
        h.finalize_variable(&mut out).ok();
        let hex: String = out.iter().map(|b| format!("{b:02x}")).collect();
        // 分两级目录：单目录几万个文件会让部分文件系统的列举变慢
        let (a, rest) = hex.split_at(2);
        self.root.join(a).join(rest)
    }

    /// 读取一块；未命中返回 `None`。
    #[must_use]
    pub fn get(&self, place: &str, id: &str, block: u64) -> Option<Vec<u8>> {
        let p = self.path_of(place, id, block);
        let data = std::fs::read(&p).ok()?;
        // 更新访问时间供 LRU 使用。失败不影响读取结果
        let now = std::time::SystemTime::now();
        filetime_touch(&p, now);
        Some(data)
    }

    /// 写入一块。
    ///
    /// 失败不返回错误：缓存写不进去只该降级为「每次都走网络」，
    /// 不该让用户的播放或浏览失败。
    pub fn put(&self, place: &str, id: &str, block: u64, data: &[u8]) {
        let p = self.path_of(place, id, block);
        if let Some(d) = p.parent() {
            if std::fs::create_dir_all(d).is_err() {
                return;
            }
        }
        // 原子写：半截缓存块会被当成完整数据读出来，解密时表现为
        // 认证失败——而文件本身其实是好的
        omy_core::fsatomic::write_atomic(&p, data).ok();
    }

    /// 按 LRU 淘汰到上限以内。
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
    #[must_use]
    pub fn stat_file(&self, place: &str, key: &str, total_blocks: u64) -> FileCacheStat {
        let mut cached_blocks = 0u64;
        let mut cached_bytes = 0u64;
        for block in 0..total_blocks {
            let p = self.path_of(place, key, block);
            if let Ok(m) = std::fs::metadata(&p) {
                if m.is_file() {
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
        }
    }

    /// 删除单个远程文件的全部缓存块，返回实际释放的字节数。
    ///
    /// 删的只是本地密文块、绝不碰云端文件，因此只读位置也允许这个操作。
    /// 单个块删除失败不计入释放量，也不中断其余块的清理。
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

/// 单个远程文件的密文块缓存覆盖情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FileCacheStat {
    /// 已缓存的密文块数。
    pub cached_blocks: u64,
    /// 该文件密文载荷的总块数。
    pub total_blocks: u64,
    /// 已缓存块占用的字节数。
    pub cached_bytes: u64,
    /// 是否所有块都已缓存（可离线播放的判定依据）。
    pub fully_cached: bool,
}

/// 把区间扩展到块边界，返回涉及的块号范围。
#[must_use]
pub fn blocks_for(offset: u64, len: u64) -> std::ops::RangeInclusive<u64> {
    let first = offset / BLOCK_SIZE;
    let last = offset.saturating_add(len).saturating_sub(1) / BLOCK_SIZE;
    first..=last
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
}
