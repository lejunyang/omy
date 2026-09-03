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
            EntryKind::Symlink => {
                // 符号链接跨平台差异大（Windows 需要管理员权限或开发者
                // 模式），按决策 N3：不静默丢弃，明确报告
                skipped.push(SkippedEntry {
                    path: e.display_path(),
                    reason: "symlink",
                });
            }
            EntryKind::Dir => {}
        }
    }

    // 必须在这里——所有条目都已落盘。见模块文档
    let metadata = crate::restore::restore_metadata(&root, idx);

    Ok(ExtractReport {
        root,
        files,
        dirs,
        adjusted,
        skipped,
        metadata,
    })
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
    fn symlinks_are_reported_not_silently_dropped() {
        // 「跳过了」和「没有这一项」对用户是两件事。静默丢弃会让人以为
        // 容器里本来就没有这个链接
        let root = tmp("symlink");
        let mut b = ContainerBuilder::new("f");
        b.add_symlink(
            vec!["link".into()],
            "target.txt".into(),
            EntryMeta::default(),
        )
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = extract_container(&idx, b"", &root).unwrap();
        assert_eq!(rep.skipped.len(), 1, "必须报告");
        assert_eq!(rep.skipped[0].reason, "symlink");
        assert_eq!(rep.skipped[0].path, "link");

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
