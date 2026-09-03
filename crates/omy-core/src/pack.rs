//! 把一个目录打包成容器载荷。
//!
//! # 为什么这一层在 core 而不在各前端里
//!
//! CLI 早先自己实现了这套遍历逻辑，GUI 要做文件夹加密时面临选择：
//! 复制一份，还是提上来共享。复制的代价不是多写几行，而是**两份实现
//! 迟早分歧**——比如哪天给 CLI 修了「跳过符号链接」的处理，GUI 那份
//! 还在按老样子打包，用户在两个入口得到不同的容器。
//!
//! core 里已经有 [`crate::fsatomic`] 在做文件系统操作，所以放这里
//! 与既有边界一致。
//!
//! # 为什么不用 walkdir
//!
//! core 的依赖目前只有密码学与格式相关的几个 crate，保持这份克制是
//! 有价值的：它让 core 能被任意项目复用，也让审计面更小。
//! 目录遍历用 `std::fs` 手写一个显式栈就够了，不值得为此引入新依赖。
//!
//! 手写还有个额外好处：**递归深度可控**。`walkdir` 默认无限深，
//! 遇到深层嵌套目录时 CLI 那份实现是靠运气不炸；这里显式设了上限。
//!
//! # 顺序必须稳定
//!
//! 条目按路径排序后写入。这不是美观问题——索引里记的是每个文件在
//! 载荷中的**区间**，遍历顺序变了，同一个目录两次打包会得到不同的
//! 布局。稳定顺序让「同样的输入产出同样的容器」成立，便于校验。

use crate::container::{ContainerBuilder, ContainerIndex, EntryMeta};
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

/// 目录递归深度上限。
///
/// 超过就拒绝，而不是一路走到栈溢出。真实目录很少超过 20 层，
/// 到 64 层基本可以断定是符号链接环或者恶意构造。
pub const MAX_DEPTH: usize = 64;

/// 打包过程中跳过的条目。
///
/// 必须如实报告给用户：静默丢弃会让人以为整个目录都进去了，
/// 等到解密时才发现少东西，那时原目录可能已经删了。
#[derive(Debug, Clone)]
pub struct SkippedEntry {
    /// 被跳过的路径（相对根目录）。
    pub path: String,
    /// 跳过原因的稳定标识，供上层翻译。
    pub reason: SkipReason,
}

/// 跳过原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 符号链接。跨平台还原语义不一致，首期不支持（文档 05 §4.2）。
    Symlink,
    /// 既不是普通文件也不是目录（设备文件、FIFO、socket 等）。
    NotRegular,
    /// 读取失败：权限不足、文件正被独占打开、或读到一半被删了。
    Unreadable,
    /// 超过递归深度上限。
    TooDeep,
}

impl SkipReason {
    /// 稳定的英文标识，用作翻译键与 JSON 字段。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Symlink => "symlink",
            Self::NotRegular => "not_regular",
            Self::Unreadable => "unreadable",
            Self::TooDeep => "too_deep",
        }
    }
}

/// 打包结果。
#[derive(Debug)]
pub struct PackedFolder {
    /// 拼接后的明文载荷。
    pub payload: Vec<u8>,
    /// 容器索引。
    pub index: ContainerIndex,
    /// 根目录名，用作加密后的显示名。
    pub root_name: String,
    /// 文件数。
    pub file_count: usize,
    /// 目录数。
    pub dir_count: usize,
    /// 被跳过的条目，需如实展示给用户。
    pub skipped: Vec<SkippedEntry>,
}

/// 遍历时可选的进度回调。
///
/// 大目录打包要花不少时间，GUI 需要据此更新界面。
/// 用 `&mut dyn FnMut` 而不是泛型参数：这个函数已经够长，
/// 再加个类型参数会让调用方的错误信息变得难读。
pub type ProgressFn<'a> = &'a mut dyn FnMut(&str, u64);

/// 遍历时交给回调的一个条目。
///
/// 抽出这个类型是为了让容器模式与树形模式**共用同一套遍历判定**：
/// 按名字稳定排序、用 `symlink_metadata` 而不是 `metadata`、跳过符号链接
/// 与非普通文件、深度上限。各写一份的话，哪天给容器模式修了「跳过 socket
/// 文件」，树形模式还在按老样子处理，同一个目录用两种模式加密会得到不同的
/// 内容集合。
#[derive(Debug)]
pub struct WalkItem {
    /// 绝对路径。
    pub path: PathBuf,
    /// 相对根目录的路径组件。
    pub comps: Vec<String>,
    /// 是否是目录。
    pub is_dir: bool,
    /// 文件系统元数据，供调用方提取 mtime 等。
    pub meta: EntryMeta,
}

/// 遍历目录树，对每个条目调用 `visit`。
///
/// **父目录保证先于其子项被访问**——树形模式依赖这一点：子项要写进父目录
/// 对应的密文目录，那个目录必须已经建好。
///
/// 跳过的条目记进 `skipped`，不中断遍历：一个打不开的文件不该让整个目录
/// 加密失败。
///
/// # Errors
///
/// 只有 `visit` 自己返回错误时才会中断。
pub fn walk_dir(
    root: &Path,
    skipped: &mut Vec<SkippedEntry>,
    visit: &mut dyn FnMut(&WalkItem) -> Result<()>,
) -> Result<()> {
    // 显式栈而不是递归：递归在深目录上会爆栈，而爆栈是不可恢复的。
    // 栈里存 (绝对路径, 相对组件, 深度)
    let mut stack: Vec<(PathBuf, Vec<String>, usize)> = vec![(root.to_path_buf(), Vec::new(), 0)];

    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            skipped.push(SkippedEntry {
                path: rel.join("/"),
                reason: SkipReason::TooDeep,
            });
            continue;
        }

        let Ok(rd) = std::fs::read_dir(&dir) else {
            skipped.push(SkippedEntry {
                path: rel.join("/"),
                reason: SkipReason::Unreadable,
            });
            continue;
        };

        // 先收集再排序：read_dir 的顺序由文件系统决定，
        // 不排序的话同一目录两次打包会得到不同布局
        let mut items: Vec<_> = rd.flatten().collect();
        items.sort_by_key(std::fs::DirEntry::file_name);

        // 目录压栈时要反着来。栈是后进先出，正序压入会让遍历
        // 变成倒序——虽然索引里已排序，但 payload 的拼接顺序
        // 会跟着变，同样破坏可复现性
        let mut subdirs = Vec::new();

        for item in items {
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            let mut comps = rel.clone();
            comps.push(name);

            // 用 symlink_metadata 而不是 metadata：后者会跟随链接，
            // 于是一个指向目录的符号链接会被当成真目录递归进去，
            // 环状链接直接把我们送进死循环
            let Ok(md) = path.symlink_metadata() else {
                skipped.push(SkippedEntry {
                    path: comps.join("/"),
                    reason: SkipReason::Unreadable,
                });
                continue;
            };

            if md.file_type().is_symlink() {
                skipped.push(SkippedEntry {
                    path: comps.join("/"),
                    reason: SkipReason::Symlink,
                });
                continue;
            }

            let meta = meta_from_fs(&md);

            if md.is_dir() {
                visit(&WalkItem {
                    path: path.clone(),
                    comps: comps.clone(),
                    is_dir: true,
                    meta,
                })?;
                subdirs.push((path, comps, depth.saturating_add(1)));
            } else if md.is_file() {
                visit(&WalkItem { path, comps, is_dir: false, meta })?;
            } else {
                // 设备文件、FIFO、socket 等
                skipped.push(SkippedEntry {
                    path: comps.join("/"),
                    reason: SkipReason::NotRegular,
                });
            }
        }

        subdirs.reverse();
        stack.extend(subdirs);
    }
    Ok(())
}

/// 把 `root` 目录打包成容器载荷。
///
/// # 内存
///
/// 载荷是**整个目录的内容拼接**，会全部读进内存。这与 core 其他部分
/// 按块处理的风格不一致，但容器格式本身要求先知道每个文件的区间才能
/// 写索引，而区间要等前面所有文件都读完才确定。
///
/// 调用方应对大目录做预检——`ContainerIndex` 里有
/// [`crate::container::RECOMMENDED_MAX_ENTRIES`] 可参考。
///
/// # Errors
///
/// - 根路径不是目录
/// - 索引构建失败（路径组件非法等，由 `ContainerBuilder` 校验）
///
/// 单个文件读不出来**不算错误**，会记进 `skipped` 继续——
/// 一个打不开的文件不该让整个目录加密失败。
pub fn pack_folder(root: &Path, mut progress: Option<ProgressFn<'_>>) -> Result<PackedFolder> {
    if !root.is_dir() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "pack_folder expects a directory",
        )));
    }

    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        // 根目录（如 `C:\`）没有 file_name。用固定名而不是空串：
        // 空名会让解密时创建不出目录
        .unwrap_or_else(|| String::from("folder"));

    let mut builder = ContainerBuilder::new(root_name.clone());
    let mut payload = Vec::new();
    let mut skipped = Vec::new();
    let mut file_count = 0usize;
    let mut dir_count = 0usize;
    // 闭包里读不出来的文件单独收集：`skipped` 已被 walk_dir 可变借走，
    // 闭包不能同时碰它。遍历结束后合并
    let mut unreadable: Vec<String> = Vec::new();

    walk_dir(root, &mut skipped, &mut |item| {
        if item.is_dir {
            builder.add_dir(item.comps.clone(), item.meta.clone())?;
            dir_count = dir_count.saturating_add(1);
        } else {
            let Ok(data) = std::fs::read(&item.path) else {
                // 读不出来就跳过并记录，不让整个目录加密失败。
                // 常见原因：文件正被别的程序独占打开
                //
                // 注意这里不能 push 到 skipped——它已被 walk_dir 借走。
                // 交给下面的 unreadable 收集
                unreadable.push(item.comps.join("/"));
                return Ok(());
            };
            let size = data.len() as u64;
            let hash = crate::util::blake2b_256(&data);
            builder.add_file(item.comps.clone(), size, Some(hash), item.meta.clone())?;
            payload.extend_from_slice(&data);
            file_count = file_count.saturating_add(1);
            if let Some(cb) = progress.as_deref_mut() {
                cb(&item.comps.join("/"), size);
            }
        }
        Ok(())
    })?;

    let index = builder.finish()?;

    // 合并闭包里收集的读失败项
    skipped.extend(unreadable.into_iter().map(|path| SkippedEntry {
        path,
        reason: SkipReason::Unreadable,
    }));

    Ok(PackedFolder {
        payload,
        index,
        root_name,
        file_count,
        dir_count,
        skipped,
    })
}

/// 从文件系统元数据提取可移植的元数据。
fn meta_from_fs(md: &std::fs::Metadata) -> EntryMeta {
    let mut m = EntryMeta::default();
    if let Ok(t) = md.modified() {
        m.mtime_ns = system_time_to_ns(t);
    }
    if let Ok(t) = md.created() {
        m.btime_ns = system_time_to_ns(t);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        m.mode = Some(md.mode());
        m.uid = Some(md.uid());
        m.gid = Some(md.gid());
    }
    m
}

/// `SystemTime` 转 Unix 纳秒。
///
/// 1970 年之前的时间戳会得到 `Err`，此时返回 `None` 而不是负数或 0——
/// 假装知道一个不知道的值，比承认不知道更糟。
fn system_time_to_ns(t: std::time::SystemTime) -> Option<i128> {
    t.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i128::try_from(d.as_nanos()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一棵测试目录树，返回根路径。
    fn make_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("omy-pack-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
        std::fs::write(root.join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("b.txt"), b"bbbb").unwrap();
        std::fs::write(root.join("sub/c.txt"), b"cc").unwrap();
        std::fs::write(root.join("sub/deeper/d.txt"), b"d").unwrap();
        root
    }

    #[test]
    fn packs_all_files_and_dirs() {
        let root = make_tree("basic");
        let p = pack_folder(&root, None).unwrap();

        assert_eq!(p.file_count, 4, "四个文件都要打进去");
        assert_eq!(p.dir_count, 2, "sub 与 sub/deeper");
        assert_eq!(p.payload.len(), 3 + 4 + 2 + 1, "载荷是所有文件内容拼接");
        assert_eq!(p.root_name, "omy-pack-basic");
        assert!(p.skipped.is_empty(), "正常目录不该有跳过项");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn payload_ranges_match_index() {
        // 这条守护容器格式的核心不变量：索引里记的区间必须真的
        // 对应载荷里那段字节。错了的话解出来的文件内容会串位——
        // 而且往往还是「能解开、内容是别的文件」这种最难查的形态
        let root = make_tree("ranges");
        let p = pack_folder(&root, None).unwrap();

        for e in &p.index.entries {
            let Some((start, len)) = e.range() else {
                continue; // 只有文件有区间
            };
            let s = usize::try_from(start).unwrap();
            let end = s + usize::try_from(len).unwrap();
            let slice = p.payload.get(s..end).expect("区间必须落在载荷内");
            assert_eq!(slice.len() as u64, len);

            // 内容要与磁盘上的原文件一致
            let disk = std::fs::read(root.join(e.path.join("/"))).unwrap();
            assert_eq!(slice, disk.as_slice(), "{} 的内容对不上", e.display_path());
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ordering_is_byte_order_not_filesystem_order() {
        // 变异测试抓出来的缺口：`ordering_is_stable` 只比较两次打包的结果，
        // 而**两次都会拿到同一个文件系统顺序**——把 sort 整行删掉，那条
        // 断言依然通过。它证明的是「确定性」，不是「按字节序排」。
        //
        // 这里改为断言一个与文件系统顺序**不同**的具体顺序：NTFS 大小写
        // 不敏感，返回 `a.txt, Z.txt`；而字节序里 `Z`(0x5A) < `a`(0x61)，
        // 应得到 `Z.txt, a.txt`。漏排序时这条立刻失败。
        //
        // 为什么必须按字节序：索引里记的是每个文件在载荷中的区间，跨平台
        // 打开同一个容器时若排序规则不同，区间就对不上。ext4 按字节序、
        // NTFS 按大小写不敏感——只有显式排序才能让两边一致。
        let root = std::env::temp_dir().join("omy-pack-order");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"aaa").unwrap();
        std::fs::write(root.join("Z.txt"), b"z").unwrap();

        let p = pack_folder(&root, None).unwrap();
        let names: Vec<String> = p
            .index
            .entries
            .iter()
            .filter(|e| e.range().is_some())
            .map(|e| e.display_path())
            .collect();
        assert_eq!(
            names,
            vec!["Z.txt", "a.txt"],
            "必须按字节序排列（大写 Z 在小写 a 之前），而不是沿用文件系统顺序"
        );
        // 载荷的拼接顺序也要跟着：Z 的内容在前
        assert_eq!(p.payload, b"zaaa", "载荷拼接顺序要与索引一致");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ordering_is_stable() {
        // 同一个目录打包两次必须得到完全相同的载荷。
        // read_dir 的顺序由文件系统决定，不排序的话这条不成立
        let root = make_tree("stable");
        let a = pack_folder(&root, None).unwrap();
        let b = pack_folder(&root, None).unwrap();
        assert_eq!(a.payload, b.payload, "两次打包的载荷必须一致");
        assert_eq!(a.index.encode(), b.index.encode(), "索引也必须一致");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_dir_is_packed_without_files() {
        let root = std::env::temp_dir().join("omy-pack-empty");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("nothing")).unwrap();

        let p = pack_folder(&root, None).unwrap();
        assert_eq!(p.file_count, 0);
        assert_eq!(p.dir_count, 1, "空目录本身也要记进索引");
        assert!(p.payload.is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_non_directory() {
        let f = std::env::temp_dir().join("omy-pack-not-a-dir.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(pack_folder(&f, None).is_err(), "对文件调用必须报错");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn progress_reports_every_file() {
        let root = make_tree("progress");
        let mut seen = Vec::new();
        let mut cb = |name: &str, size: u64| seen.push((name.to_owned(), size));
        let p = pack_folder(&root, Some(&mut cb)).unwrap();

        assert_eq!(seen.len(), p.file_count, "每个文件都要回调一次");
        let total: u64 = seen.iter().map(|(_, s)| s).sum();
        assert_eq!(total as usize, p.payload.len(), "回调报的大小之和要等于载荷");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_not_followed() {
        // 跟随符号链接会导致两个问题：环状链接死循环，
        // 以及把链接目标的内容重复打包进去
        let root = std::env::temp_dir().join("omy-pack-symlink");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("link.txt")).unwrap();

        let p = pack_folder(&root, None).unwrap();
        assert_eq!(p.file_count, 1, "只有真文件被打包");
        assert_eq!(p.skipped.len(), 1);
        assert_eq!(p.skipped[0].reason, SkipReason::Symlink);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn directory_junctions_are_skipped_not_followed() {
        // Windows 上的补位测试。原来的符号链接测试是 `#[cfg(unix)]`，
        // 于是「改用 metadata 会跟随链接」这个缺陷在 Windows 开发机上
        // **压根没有测试覆盖**——变异测试把这个盲区暴露了出来。
        //
        // 创建符号链接在 Windows 上需要特权（实测 WinError 1314），但
        // **目录联接（junction）不需要**，而 Rust 同样把它报成 symlink。
        // 于是不必提权就能覆盖这条路径。
        //
        // 为什么必须跳过：环状联接会让遍历死循环，且会把目标内容重复打包。
        let root = std::env::temp_dir().join("omy-pack-junction");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/inside.txt"), b"content").unwrap();

        let link = root.join("link");
        let st = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(root.join("real"))
            .output();
        // 建不出来就跳过：不同环境的策略可能不同，测试不该因此变红
        let Ok(o) = st else { return };
        if !o.status.success() || !link.exists() {
            return;
        }

        let p = pack_folder(&root, None).unwrap();
        assert_eq!(
            p.file_count, 1,
            "只有 real/inside.txt 该被打包；联接被跟随的话会变成 2"
        );
        assert!(
            p.skipped.iter().any(|s| s.reason == SkipReason::Symlink),
            "联接必须如实报告为符号链接，实际 {:?}",
            p.skipped
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skip_reason_codes_are_stable() {
        // 这些字符串是翻译键与 JSON 字段，改了会破坏前端与脚本
        assert_eq!(SkipReason::Symlink.code(), "symlink");
        assert_eq!(SkipReason::NotRegular.code(), "not_regular");
        assert_eq!(SkipReason::Unreadable.code(), "unreadable");
        assert_eq!(SkipReason::TooDeep.code(), "too_deep");
    }
}
