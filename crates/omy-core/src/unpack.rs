//! 把容器载荷展开到磁盘。
//!
//! # 为什么这一层在 core
//!
//! [`crate::pack`] 的模块文档写过同一件事：CLI 先实现了遍历逻辑，GUI 要做
//! 文件夹加密时面临「复制一份还是提上来」，复制的代价不是多写几行，而是
//! **两份实现迟早分歧**。
//!
//! 解包这边的分歧代价更高，因为它包含两类安全相关的判断：
//!
//! - **路径逃逸防护**（`..`、绝对路径、盘符）。复制一份意味着以后修补一个
//!   逃逸漏洞时，另一个入口仍然可被利用，而且没人会想起来它。
//! - **文件名兼容处理**（Windows 非法字符与保留名）。两份实现给出不同的
//!   替换结果，同一个容器在 CLI 和 GUI 解出来会得到不同的文件名。
//!
//! # 元数据还原为什么在这里面调用，而不是交给调用方
//!
//! [`crate::restore::restore_metadata`] 有一个顺序约束：必须在**全部条目
//! 落盘之后**才能调用（新建目录项会更新父目录的 mtime）。把它放在本函数
//! 内部的末尾，这个约束就由结构保证，任何调用方都不可能弄错；交给调用方
//! 则意味着每个入口都要记住这件事，而忘记的后果是「文件时间对了、目录
//! 时间还是现在」——只在非空目录上出现，很容易被当成偶发问题。

use crate::container::{ContainerIndex, EntryKind};
use crate::error::{Error, Result};
use crate::restore::RestoreReport;
use std::path::{Path, PathBuf};

/// 未能还原的条目及原因。
#[derive(Debug, Clone)]
pub struct SkippedEntry {
    /// 条目路径（相对容器根，用 `/` 连接）。
    pub path: String,
    /// 稳定代号，同时是 JSON 字段值与翻译键后缀。
    pub reason: &'static str,
}

/// 解包结果。
#[derive(Debug)]
pub struct ExtractReport {
    /// 容器根目录在磁盘上的实际位置。
    pub root: PathBuf,
    /// 写出的文件数。
    pub files: usize,
    /// 创建的目录数。
    pub dirs: usize,
    /// 成功创建的符号链接数。
    pub links: usize,
    /// 因平台限制被改名的条目（原路径）。
    ///
    /// 必须报告：文件名被悄悄改掉的话，用户按原名找不到文件，
    /// 只会以为解包漏了东西（决策 N3，文档 05 §4.3）。
    pub adjusted: Vec<String>,
    /// 未能还原的条目。
    pub skipped: Vec<SkippedEntry>,
    /// 元数据还原报告。
    pub metadata: RestoreReport,
}

/// 把容器载荷展开到 `target` 目录下。
///
/// 会在 `target` 里建一层以容器根名命名的目录，条目落在其中——与 CLI
/// 既有行为一致。容器根名来自索引，不是密文文件名。
///
/// # Errors
///
/// 目录创建失败、路径逃逸、载荷区间越界或逐文件哈希不匹配时返回错误。
/// 单个符号链接无法还原**不算错误**，记进 `skipped` 继续——为一个链接
/// 让整个解包失败，代价与收益不成比例。
pub fn extract_container(
    idx: &ContainerIndex,
    payload: &[u8],
    target: &Path,
) -> Result<ExtractReport> {
    let root = target.join(sanitize_filename(&idx.root));
    std::fs::create_dir_all(&root).map_err(Error::Io)?;

    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut links = 0usize;
    // 平台不允许建链接的条目数。累计计数而不是逐条记录：报告要说
    // 「3 个链接因平台限制未还原」，不是重复三行同样的话。
    let mut unsupported_links = 0usize;
    // 建链接时的真实失败（区别于平台不支持），带路径和原因。
    let mut link_failures: Vec<(String, String)> = Vec::new();
    let mut adjusted: Vec<String> = Vec::new();
    let mut skipped: Vec<SkippedEntry> = Vec::new();

    // 先建所有目录，含空目录——空目录必须显式还原，否则会丢失
    // （文档 05 §2.2 为此在索引里显式记录 dir 条目）
    for e in &idx.entries {
        if e.kind == EntryKind::Dir {
            let (p, was_adjusted) = safe_join(&root, &e.path)?;
            if was_adjusted {
                adjusted.push(e.display_path());
            }
            std::fs::create_dir_all(&p).map_err(Error::Io)?;
            dirs = dirs.saturating_add(1);
        }
    }

    for e in &idx.entries {
        match e.kind {
            EntryKind::File => {
                let (p, was_adjusted) = safe_join(&root, &e.path)?;
                if was_adjusted {
                    adjusted.push(e.display_path());
                }
                if let Some(parent) = p.parent() {
                    // 父目录可能因为改名而与上面建的那个不同名，补一次
                    std::fs::create_dir_all(parent).ok();
                }
                let start = usize::try_from(e.offset).unwrap_or(usize::MAX);
                let len = usize::try_from(e.size).unwrap_or(0);
                let end = start.saturating_add(len);
                // 归到 MalformedHeader 而不是载荷类错误：容器索引在头部
                // TLV 里，「区间超出载荷」说明索引本身不对
                let bytes = payload.get(start..end).ok_or(Error::MalformedHeader {
                    reason: "container entry payload range out of bounds",
                })?;

                // 逐文件哈希校验：容器里某个文件坏了要能精确指出是哪个。
                // 整体 AEAD 已经保证了完整性，但它只能说「这个容器坏了」
                if let Some(expect) = &e.hash {
                    let actual = crate::util::blake2b_256(bytes);
                    if &actual != expect {
                        return Err(Error::ContentHashMismatch);
                    }
                }
                std::fs::write(&p, bytes).map_err(Error::Io)?;
                files = files.saturating_add(1);
            }
            EntryKind::Symlink | EntryKind::Dir => {}
        }
    }

    // 链接单独走一遍，必须在所有文件和目录都落盘之后。
    //
    // 原因是目标类型：Windows 分 symlink_file 与 symlink_dir 两个不同的
    // 系统调用，选错会建出类型不符的链接。判断依据是目标当前是不是目录，
    // 而容器内的链接完全可能指向索引里排在它后面的条目——放在同一个循环里
    // 就会在目标还没落盘时去判断，把指向目录的链接建成文件型。
    //
    // 顺带也让「链接指向同容器内的文件」这种常见情况不再是悬空链接。
    for e in &idx.entries {
        if e.kind != EntryKind::Symlink {
            continue;
        }
        let (p, was_adjusted) = safe_join(&root, &e.path)?;
        if was_adjusted {
            adjusted.push(e.display_path());
        }
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        // 目标缺失的链接条目在 container 层就被拒了（见
        // `symlink_without_target_rejected`），这里再兜一次：
        // 拿不到目标就没法建，如实报告而不是建一个空链接。
        let Some(target) = &e.target else {
            skipped.push(SkippedEntry {
                path: e.display_path(),
                reason: "symlink_no_target",
            });
            continue;
        };

        // 指向容器之外的链接不还原。
        //
        // 这是一个真实的逃逸口：`safe_join` 只管住了链接**自身**要落在哪，
        // 完全没看它**指向**哪。一个恶意容器可以放 `link -> C:\Windows` 或
        // `link -> ../../../etc/passwd`，解开后用户在文件管理器里点进去，
        // 就在容器外面操作了——而他以为自己还在解出来的目录里。
        //
        // 拒绝而不是改写成安全路径：改写会得到一个指向别处的链接，看着正常
        // 却指错地方，比不建更难发现。如实报告，让用户知道容器里有这么一项。
        if !link_target_stays_inside(&root, &p, target) {
            skipped.push(SkippedEntry {
                path: e.display_path(),
                reason: "symlink_escapes_root",
            });
            continue;
        }

        match create_symlink(target, &p) {
            Ok(()) => links = links.saturating_add(1),
            Err(err) if is_privilege_error(&err) => {
                // 平台不允许建链接（Windows 未开开发者模式时必然如此）。
                // 归 unsupported 而不是 failure：目标仍在加密文件里，换
                // 环境能还原，报成失败会让人反复重试一件不可能成功的事。
                unsupported_links = unsupported_links.saturating_add(1);
            }
            Err(err) => {
                // 其他失败是这次操作的问题（同名文件已占位、磁盘问题），
                // 重试可能就好了，要如实报错。
                link_failures.push((e.display_path(), err.to_string()));
            }
        }
    }

    // 必须在这里——所有条目都已落盘。见模块文档
    let mut metadata = crate::restore::restore_metadata(&root, idx);

    // 链接的结果并进同一份报告，而不是在 ExtractReport 上另开两个字段：
    // 对用户来说「链接没能还原」和「权限位没能还原」是同一类信息——这次
    // 拿不到但值还在加密文件里，展示与翻译都该走同一条路径。
    metadata.note_unsupported_n(
        crate::restore::UnsupportedKind::Symlink,
        unsupported_links,
    );
    for (path, reason) in link_failures {
        metadata.push_failure(path, "symlink", reason);
    }

    Ok(ExtractReport {
        root,
        files,
        dirs,
        links,
        adjusted,
        skipped,
        metadata,
    })
}

/// 创建一个符号链接，`target` 是链接内容，`link` 是要创建的路径。
///
/// # 为什么 Windows 要分两个调用
///
/// Windows 的 `CreateSymbolicLinkW` 要在创建时就声明目标是文件还是目录
/// （Unix 只有一个 `symlink`，不关心目标类型）。声明错了会得到一个类型
/// 不符的链接：资源管理器里点开报错，`std::fs::metadata` 跟随后也拿不到
/// 正确的类型——是个能建成功但不可用的产物，比直接失败更难查。
///
/// 判断依据是**目标当前是不是目录**。目标不存在（悬空链接）时只能猜，
/// 这里按文件处理：文件型是更常见的情况，而且猜错的代价对悬空链接来说
/// 暂时不显现（等目标出现后才可能暴露）。
///
/// # 实测结论（开发者模式下）
///
/// 这段判断**不是**多余的保险，类型选错的后果已经实测过：把两个调用对调
/// 之后，链接照样建得出来（`is_symlink` 仍为 true），但 `metadata` 跟随
/// 时 `dlink` 报 permission denied、`flink` 报 not a directory，读文件
/// 直接「拒绝访问」——正是那种「能创建但不可用」的产物。
///
/// 另一个实测得到的细节：Windows 沿链接**路径**穿越时并不校验这个类型
/// 标记，所以即便类型选错，「透过 dlink 读 sub/inside.txt」照样成功。
/// 判断链接是否健康只能对链接自身取 `metadata`，不能靠能不能穿过去读文件
/// （`link_to_dir_inside_container_gets_right_type` 的注释里记了这一点，
/// 因为拿后者当判据的测试是抓不到缺陷的）。
///
/// 悬空链接在有特权时也能正常创建，所以不需要为它推迟或跳过。
///
/// # 分隔符必须换成反斜杠
///
/// 容器里的路径一律用 `/` 分隔（跨平台的容器格式只能这样），但 Windows
/// **不接受目标里的正斜杠**。这个错误的形态很坏：链接照样**创建成功**，
/// `is_symlink` 为 true、`read_link` 也读得回来，只有跟随时才报
/// `winerror 123`（`InvalidFilename`）。也就是说不转换的话，`links` 会
/// 报成功，磁盘上却是一个点开就报错的坏链接。
///
/// 受影响的是任何目标带子目录的链接（`../lib/libfoo.so` 这类很常见），
/// 尤其是在 Unix 上打包、拿到 Windows 解开的容器。
#[cfg(windows)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    // 见上：正斜杠会让链接建得出来却跟随不了
    let target = target.replace('/', "\\");

    // 相对目标要相对链接所在目录解析，不是相对进程当前目录——
    // 用当前目录判断会在链接不在 cwd 下时得到错误的类型判断。
    let base = link.parent().unwrap_or(Path::new("."));
    let resolved = base.join(&target);

    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(&target, link)
    } else {
        std::os::windows::fs::symlink_file(&target, link)
    }
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> std::io::Result<()> {
    // Unix 不区分目标类型，一个调用搞定
    std::os::unix::fs::symlink(target, link)
}

/// 链接目标解析后是否仍落在 `root` 内。
///
/// `link` 是链接自身的落盘路径，`target` 是要写进链接的内容。
///
/// # 为什么不用 `canonicalize`
///
/// 目标此刻可能还不存在（悬空链接是合法的），`canonicalize` 会直接报错，
/// 于是所有悬空链接都会被误判成逃逸。这里改用纯路径推演：不碰文件系统，
/// 只按组件消解 `.` 与 `..`。
///
/// 顺带的好处是不受符号链接自身影响——`canonicalize` 会跟随路径上已存在的
/// 链接，攻击者可以借此让「检查时」和「使用时」看到不同的结果。
fn link_target_stays_inside(root: &Path, link: &Path, target: &str) -> bool {
    let t = Path::new(target);

    // 绝对路径（含 Windows 的盘符与 UNC）一律拒绝。就算它字面上落在 root
    // 里，也是一条写死了本机位置的链接：换台机器解开必然指向不存在的地方，
    // 还把原机器的目录结构泄进了容器
    if t.is_absolute() || t.has_root() {
        return false;
    }

    // 相对目标以链接**所在目录**为基准解析，不是以进程当前目录
    let Some(base) = link.parent() else {
        return false;
    };

    // 按组件消解。用 `pop` 处理 `..` 而不是留给系统：留着 `..` 的话
    // `starts_with` 会把 `root/a/../../x` 判成在 root 内
    let mut acc = base.to_path_buf();
    for comp in t.components() {
        match comp {
            std::path::Component::ParentDir => {
                // 已经退到 root 就不能再退。这里必须先判断再 pop：
                // pop 到 root 之上再比较就已经晚了
                if acc == root {
                    return false;
                }
                if !acc.pop() {
                    return false;
                }
            }
            std::path::Component::CurDir => {}
            // 绝对路径成分在上面已经挡掉，剩下的只可能是普通名字
            other => acc.push(other.as_os_str()),
        }
    }

    acc.starts_with(root)
}

/// 这个错误是不是「平台/权限不允许建符号链接」。
///
/// 决定报 unsupported 还是 failure，两者对用户的含义完全不同（见
/// [`crate::restore::RestoreReport`] 的文档）。
///
/// Windows 上不能只看 [`std::io::ErrorKind`]：无特权时返回的是
/// `ERROR_PRIVILEGE_NOT_HELD`（1314），而它并**不**映射成
/// `ErrorKind::PermissionDenied`，只看 kind 会把这个必然失败的情况
/// 报成普通失败，用户于是反复重试。实测本机（未开发者模式、未提权）
/// 建任何链接都得到这个错误。
#[cfg(windows)]
fn is_privilege_error(e: &std::io::Error) -> bool {
    /// `ERROR_PRIVILEGE_NOT_HELD`：调用方没有 `SeCreateSymbolicLinkPrivilege`。
    const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

    e.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD)
        || e.kind() == std::io::ErrorKind::PermissionDenied
}

#[cfg(not(windows))]
fn is_privilege_error(e: &std::io::Error) -> bool {
    // Unix 上建链接不需要特权，权限不足是真的目录权限问题。
    // 仍归 unsupported：换个可写位置就能还原，和 Windows 一致。
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// 安全拼接路径，拒绝任何逃出 `root` 的结果，并报告是否改过名。
///
/// # Errors
///
/// 路径组件非法或拼接结果逃出 `root` 时返回错误。
///
/// # 为什么校验两遍
///
/// `ContainerIndex::parse` 已经校验过组件，这里再查一次是纵深防御：
/// 解析与落盘之间隔着好几层调用，将来任何一处改动引入回归，都会在
/// 真正写盘之前被这一道拦住。
pub fn safe_join(root: &Path, comps: &[String]) -> Result<(PathBuf, bool)> {
    let mut p = root.to_path_buf();
    let mut adjusted = false;
    for c in comps {
        crate::container::validate_component(c)?;
        let s = sanitize_filename(c);
        if s != *c {
            adjusted = true;
        }
        p.push(s);
    }
    if !p.starts_with(root) {
        return Err(Error::MalformedHeader {
            reason: "container entry path escapes target directory",
        });
    }
    Ok((p, adjusted))
}

/// 把文件名调整为当前平台可用形式。
///
/// Windows 禁止 `\ / : * ? " < > |`、控制字符，以及一批设备保留名。
/// **绝不静默丢弃**：调用方会把改过名的项报告给用户（决策 N3）。
#[must_use]
pub fn sanitize_filename(name: &str) -> String {
    #[cfg(windows)]
    {
        const BAD: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
        const RESERVED: &[&str] = &[
            "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
            "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
            "LPT9",
        ];
        let mut s: String = name
            .chars()
            .map(|c| {
                if BAD.contains(&c) || (c as u32) < 0x20 {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        // 结尾的点与空格在 Windows 上会被吞掉，留着会导致
        // 「写进去的名字和读出来的不一样」
        while s.ends_with('.') || s.ends_with(' ') {
            s.pop();
        }
        let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            s = format!("_{s}");
        }
        if s.is_empty() {
            s = String::from("_");
        }
        s
    }
    #[cfg(not(windows))]
    {
        // Unix 只需处理 / 与 NUL
        let s: String = name
            .chars()
            .map(|c| if c == '/' || c == '\0' { '_' } else { c })
            .collect();
        if s.is_empty() { String::from("_") } else { s }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerBuilder, EntryMeta};

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("omy-unpack-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn extracts_files_dirs_and_empty_dirs() {
        let root = tmp("basic");
        let mut b = ContainerBuilder::new("myfolder");
        b.add_dir(vec!["sub".into()], EntryMeta::default()).unwrap();
        b.add_dir(vec!["empty".into()], EntryMeta::default())
            .unwrap();
        b.add_file(vec!["a.txt".into()], 3, None, EntryMeta::default())
            .unwrap();
        b.add_file(
            vec!["sub".into(), "b.txt".into()],
            4,
            None,
            EntryMeta::default(),
        )
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"aaabbbb", &root).unwrap();
        assert_eq!(rep.files, 2);
        assert_eq!(rep.dirs, 2);
        assert!(rep.root.ends_with("myfolder"), "根名取自索引");
        assert_eq!(
            std::fs::read(rep.root.join("a.txt")).unwrap(),
            b"aaa",
            "区间必须对上：错了会解出别的文件的内容"
        );
        assert_eq!(std::fs::read(rep.root.join("sub/b.txt")).unwrap(), b"bbbb");
        // 空目录必须还原。不显式建的话它会静默消失，
        // 而用户是有意留着它的
        assert!(rep.root.join("empty").is_dir(), "空目录必须还原");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn metadata_is_restored_without_caller_doing_anything() {
        // 顺序约束由结构保证：调用方不需要（也不能）自己记得在落盘之后
        // 再调 restore_metadata。这条锁住「解包即还原元数据」。
        let root = tmp("meta");
        const T: i128 = 1_551_968_736_000_000_000;
        let mut b = ContainerBuilder::new("f");
        b.add_dir(vec!["sub".into()], EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        b.add_file(vec!["sub".into(), "a.txt".into()], 1, None, EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"x", &root).unwrap();
        assert_eq!(rep.metadata.mtime_restored, 2, "文件和目录都该设上");

        // 查磁盘上的真实值，不只看计数——计数是自己报的
        for rel in ["sub", "sub/a.txt"] {
            let got = std::fs::metadata(rep.root.join(rel))
                .unwrap()
                .modified()
                .unwrap();
            let want = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(T as u64);
            let d = got
                .duration_since(want)
                .unwrap_or_else(|e| e.duration())
                .as_secs();
            assert!(d < 2, "{rel} 的 mtime 应当已还原，实际 {got:?}");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hash_mismatch_is_caught_per_file() {
        // 整体 AEAD 只能说「这个容器坏了」，逐文件哈希才能指出是哪个。
        // 不校验的话坏内容会静默落盘
        let root = tmp("hash");
        let mut b = ContainerBuilder::new("f");
        b.add_file(vec!["a.txt".into()], 3, Some([0u8; 32]), EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();
        assert!(
            extract_container(&idx, b"aaa", &root).is_err(),
            "哈希不匹配必须报错，不能静默写出坏内容"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn payload_range_out_of_bounds_is_rejected() {
        // 索引声称的区间超出实际载荷。不检查就是越界读
        let root = tmp("oob");
        let mut b = ContainerBuilder::new("f");
        b.add_file(vec!["a.txt".into()], 999, None, EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();
        assert!(extract_container(&idx, b"aaa", &root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlinks_are_restored_or_reported_never_silently_dropped() {
        // 这条测试在两种环境下都必须有意义，所以按实际能力分支断言：
        // 能建链接（Unix，或开了开发者模式的 Windows）就要求真的建出来；
        // 不能建（本机实测：未提权的 Windows 必然失败）就要求归到
        // unsupported 而不是 failures。
        //
        // 无论哪种，都不允许「既没建出来又没报告」——那会让用户以为容器
        // 里本来就没有这个链接。
        let root = tmp("symlink");
        let mut b = ContainerBuilder::new("f");
        b.add_file(vec!["target.txt".into()], 3, None, EntryMeta::default())
            .unwrap();
        b.add_symlink(
            vec!["link".into()],
            "target.txt".into(),
            EntryMeta::default(),
        )
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"abc", &root).unwrap();
        let link = rep.root.join("link");

        let unsupported_links = rep
            .metadata
            .unsupported
            .iter()
            .find(|(k, _)| *k == crate::restore::UnsupportedKind::Symlink)
            .map(|(_, n)| *n)
            .unwrap_or(0);

        if rep.links == 1 {
            // 建成功了：查磁盘上的真实类型，不只看计数——计数是自己报的。
            // 用 symlink_metadata，metadata 会跟随链接看到目标的类型
            let md = std::fs::symlink_metadata(&link).expect("链接该在磁盘上");
            assert!(
                md.file_type().is_symlink(),
                "报告说建了链接，磁盘上却不是链接"
            );
            assert_eq!(
                std::fs::read_link(&link).unwrap().to_string_lossy(),
                "target.txt",
                "目标要和存进去的一致，指错地方等于没还原"
            );
            assert_eq!(unsupported_links, 0, "建成功了就不该报不支持");
            assert!(rep.metadata.failures.is_empty(), "建成功了不该有失败");
        } else {
            assert_eq!(rep.links, 0, "要么建成 1 个，要么 0 个");
            assert_eq!(
                unsupported_links, 1,
                "建不出来必须报成 unsupported（换环境能还原），\
                 而不是 failures（让人反复重试）或干脆不报；实际 {:?} / {:?}",
                rep.metadata.unsupported, rep.metadata.failures
            );
            assert!(
                rep.metadata.failures.is_empty(),
                "平台限制不该记进 failures，实际 {:?}",
                rep.metadata.failures
            );
        }

        // 不管链接建没建成，同容器里的普通文件都要正常落盘——
        // 链接失败不该拖累其他条目
        assert_eq!(rep.files, 1);
        assert_eq!(
            std::fs::read(rep.root.join("target.txt")).unwrap(),
            b"abc",
            "链接的成败不该影响文件内容"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn link_to_dir_inside_container_gets_right_type() {
        // 锁住两件事：链接类型选对了，以及「链接在所有文件目录落盘之后
        // 才创建」这个顺序约束。
        //
        // 索引里链接排在它的目标**之前**（add_symlink 先调用）。如果实现
        // 把链接和文件放在同一个循环里处理，创建链接时目标还不存在，
        // Windows 上 `is_dir()` 对不存在的路径返回 false，于是指向目录的
        // 链接被建成文件型——能建成功但不可用。
        //
        // # 判据为什么是「对链接自身取 metadata」
        //
        // 变异测试实测过（把 symlink_dir / symlink_file 对调）：
        //
        // - `metadata(dlink)` 报 permission denied、`metadata(flink)` 报
        //   not a directory——**能抓到**。
        // - 而「透过 dlink 读 sub/inside.txt」在变异后**照样成功**：Windows
        //   沿链接路径穿越时并不校验这个类型标记。拿它当判据等于没测。
        //
        // 所以查的是链接自身跟随后的类型，不是能不能穿过去读到文件。
        let root = tmp("linkorder");
        let mut b = ContainerBuilder::new("f");
        b.add_symlink(vec!["dlink".into()], "sub".into(), EntryMeta::default())
            .unwrap();
        b.add_symlink(
            vec!["flink".into()],
            "sub/inside.txt".into(),
            EntryMeta::default(),
        )
        .unwrap();
        b.add_dir(vec!["sub".into()], EntryMeta::default()).unwrap();
        b.add_file(
            vec!["sub".into(), "inside.txt".into()],
            2,
            None,
            EntryMeta::default(),
        )
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"hi", &root).unwrap();

        // 建不出链接的环境（未开开发者模式的 Windows）跳过后半段——那里由
        // symlinks_are_restored_or_reported_never_silently_dropped 覆盖，
        // 本条测的是「建出来之后类型对不对」
        if rep.links == 0 {
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        assert_eq!(rep.links, 2, "两个链接都该建出来");

        // 目录链接跟随后必须是目录。类型选错时这里直接报错而不是返回 false
        let dmd = std::fs::metadata(rep.root.join("dlink"))
            .expect("目录链接跟随失败，说明类型选错了");
        assert!(dmd.is_dir(), "dlink 跟随后该是目录");

        // 文件链接跟随后必须是文件，且内容读得出来
        let fmd = std::fs::metadata(rep.root.join("flink"))
            .expect("文件链接跟随失败，说明类型选错了");
        assert!(fmd.is_file(), "flink 跟随后该是文件");
        assert_eq!(
            std::fs::read(rep.root.join("flink")).expect("读文件链接失败"),
            b"hi",
            "透过文件链接该读到目标内容"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn symlink_entry_without_target_is_reported() {
        // 正常路径下 container 层就拒了没目标的链接条目，所以这里要手工
        // 构造。仍要测：损坏或手工拼的容器不该让我们建出一个空链接，
        // 也不该静默忽略
        let root = tmp("notarget");
        let mut b = ContainerBuilder::new("f");
        b.add_symlink(vec!["link".into()], "t".into(), EntryMeta::default())
            .unwrap();
        let mut idx = b.finish().unwrap();
        // 绕过 builder 校验，模拟坏容器
        for e in &mut idx.entries {
            if e.kind == EntryKind::Symlink {
                e.target = None;
            }
        }

        let rep = extract_container(&idx, b"", &root).unwrap();
        assert_eq!(rep.links, 0, "没目标建不出链接");
        assert!(
            rep.skipped.iter().any(|s| s.reason == "symlink_no_target"),
            "必须如实报告，实际 {:?}",
            rep.skipped
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn link_targets_pointing_outside_are_refused() {
        // 纯函数级别地把逃逸判断的边界钉死。`safe_join` 只管链接自身落在
        // 哪，管不到它指向哪，所以这道检查是唯一的防线。
        let root = Path::new("/vault/root");
        let link = root.join("sub").join("l");

        // 该放行的：容器内的相对目标
        for ok in ["a.txt", "./a.txt", "d/e.txt", "../sibling.txt", "../a/../b"] {
            assert!(
                link_target_stays_inside(root, &link, ok),
                "{ok} 落在容器内，该放行"
            );
        }

        // 该拒绝的：爬出去、绝对路径、盘符、UNC
        for bad in [
            "../../outside.txt",
            "../../../../../../etc/passwd",
            // 先进子目录再爬出去，抵消掉的层数刚好越界。
            // 只做字符串前缀匹配的实现会漏掉这个
            "d/../../../outside.txt",
        ] {
            assert!(
                !link_target_stays_inside(root, &link, bad),
                "{bad} 爬出了容器，必须拒绝"
            );
        }

        // 绝对路径一律拒绝，即便字面上指回容器内——它写死了本机位置
        for abs in ["/etc/passwd", "/vault/root/a.txt"] {
            assert!(
                !link_target_stays_inside(root, &link, abs),
                "{abs} 是绝对路径，必须拒绝"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_absolute_link_targets_are_refused() {
        // Windows 特有的几种绝对形式。`Path::is_absolute` 对 `\foo` 这种
        // 「有根但无盘符」的路径返回 false，所以实现里还查了 has_root——
        // 漏掉它就会放行一条指向当前盘根目录的链接
        let root = Path::new(r"C:\vault\root");
        let link = root.join("l");
        for bad in [
            r"C:\Windows\System32",
            r"\\server\share\x",
            r"\Windows",
            r"C:\vault\root\a.txt",
        ] {
            assert!(
                !link_target_stays_inside(root, &link, bad),
                "{bad} 必须拒绝"
            );
        }
    }

    #[test]
    fn escaping_link_is_reported_not_created() {
        // 端到端确认：恶意容器里指向外面的链接不落盘，且如实报告。
        // 静默忽略会让用户以为容器里本来就没这项
        let root = tmp("escape");
        let mut b = ContainerBuilder::new("f");
        b.add_symlink(
            vec!["evil".into()],
            "../../../../outside.txt".into(),
            EntryMeta::default(),
        )
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"", &root).unwrap();
        assert_eq!(rep.links, 0, "逃逸的链接不该被创建");
        assert!(
            rep.skipped
                .iter()
                .any(|s| s.reason == "symlink_escapes_root"),
            "必须如实报告，实际 {:?}",
            rep.skipped
        );
        assert!(
            !rep.root.join("evil").exists(),
            "磁盘上不该出现这个链接"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sanitize_keeps_normal_names() {
        assert_eq!(sanitize_filename("report.docx"), "report.docx");
        assert_eq!(sanitize_filename("中文文件名.txt"), "中文文件名.txt");
    }

    #[cfg(windows)]
    #[test]
    fn sanitize_handles_windows_restrictions() {
        assert_eq!(sanitize_filename("report:2026?.txt"), "report_2026_.txt");
        assert_eq!(sanitize_filename("a<b>c"), "a_b_c");
        assert_eq!(sanitize_filename("trailing."), "trailing");
        assert_eq!(sanitize_filename("CON"), "_CON");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename(""), "_");
    }

    #[test]
    fn safe_join_rejects_traversal() {
        // 路径逃逸是本模块最重要的一道防线：容器索引来自文件，
        // 而文件内容是不可信输入
        let root = Path::new("/tmp/root");
        assert!(safe_join(root, &["..".to_string()]).is_err());
        assert!(safe_join(root, &["a".to_string(), "..".to_string()]).is_err());
        let (ok, adjusted) =
            safe_join(root, &["a".to_string(), "b.txt".to_string()]).unwrap();
        assert!(ok.starts_with(root));
        assert!(!adjusted, "正常名字不该被标记为已调整");
    }

    #[cfg(windows)]
    #[test]
    fn adjusted_names_are_reported() {
        // 名字被悄悄改掉的话，用户按原名找不到文件，只会以为解包漏了东西
        let root = tmp("adjust");
        let mut b = ContainerBuilder::new("f");
        // 问号在 Windows 上非法，但不像盘符——**不能**用 `a:b.txt` 试：
        // validate_component 会先把它按「路径组件形如盘符」拒掉（那道
        // 检查是对的，`C:` 这类组件在 Windows 上会被当成盘符，是真实的
        // 逃逸手段），于是根本走不到 sanitize_filename 这一步
        b.add_file(vec!["a?b.txt".into()], 1, None, EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"x", &root).unwrap();
        assert_eq!(rep.adjusted, vec!["a?b.txt"], "改名必须报告");
        assert!(rep.root.join("a_b.txt").exists(), "落盘用的是调整后的名字");

        let _ = std::fs::remove_dir_all(&root);
    }
}
