//! `omy list`：列出目录中的 .omy 文件。
//!
//! 与 `scan` 的区别：`list` 不尝试任何密码，只按文件头识别并显示明文可见信息。
//! 适合快速查看「这个目录里有哪些加密文件」。

use super::{Ctx, has_omy_ext};
use crate::output::{human_bytes, thousands};
use anyhow::Result;
use clap::Args as ClapArgs;
use omy_core::header::flags;
use serde_json::json;
use std::path::PathBuf;

/// `list` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 目标目录，默认当前目录
    #[arg(default_value = ".")]
    pub dirs: Vec<PathBuf>,

    /// 递归子目录
    #[arg(short, long)]
    pub recursive: bool,

    /// 最大递归深度
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// 也列出后缀不是 .omy 但文件头匹配的文件
    #[arg(long)]
    pub any_extension: bool,
}

/// 执行 `list`。
///
/// # Errors
///
/// 目录无法遍历时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let depth = if a.recursive {
        a.max_depth.unwrap_or(usize::MAX)
    } else {
        1
    };

    let mut rows = Vec::new();
    let mut total_size = 0u64;

    for dir in &a.dirs {
        let walker = walkdir::WalkDir::new(dir)
            .max_depth(depth)
            .sort_by_file_name();
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    ctx.out.detail(&format!("跳过: {e}"));
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let p = entry.path();
            if !a.any_extension && !has_omy_ext(p) {
                continue;
            }

            // 读头部足够的前缀即可判断，不读整个文件
            let Ok(head) = read_prefix(p, omy_core::scan::MIN_PROBE_SIZE) else {
                continue;
            };
            if !omy_core::file::is_omy_file(&head) {
                continue;
            }
            let Ok(h) = omy_core::file::peek_header(&head) else {
                continue;
            };

            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            total_size += size;

            let mut marks = Vec::new();
            if h.has_flag(flags::COMPRESSED) {
                marks.push("压缩");
            }
            if h.has_flag(flags::CONTAINER) {
                marks.push("容器");
            }
            if h.has_flag(flags::SHARDED) {
                marks.push("分片");
            }
            if h.has_flag(flags::HAS_THUMBNAIL) {
                marks.push("缩略图");
            }
            if h.has_flag(flags::TRANSCODED) {
                marks.push("已转码");
            }

            ctx.out.line(&format!(
                "{:<44} {:>10}  {:>10} 块  {}",
                truncate(&p.display().to_string(), 44),
                human_bytes(size),
                thousands(h.n_chunks()),
                marks.join(" ")
            ));

            rows.push(json!({
                "path": p.display().to_string(),
                "size": size,
                "plaintext_size": h.plaintext_size,
                "chunks": h.n_chunks(),
                "chunk_size": h.chunk_size,
                "compressed": h.has_flag(flags::COMPRESSED),
                "container": h.has_flag(flags::CONTAINER),
                "sharded": h.has_flag(flags::SHARDED),
                "has_thumbnail": h.has_flag(flags::HAS_THUMBNAIL),
                "filename_encrypted": h.has_flag(flags::FILENAME_ENCRYPTED),
            }));
        }
    }

    if ctx.out.is_json() {
        ctx.out.result(
            "",
            &json!({ "files": rows, "count": rows.len(), "total_size": total_size }),
        );
    } else if rows.is_empty() {
        ctx.out.info(crate::i18n::t("ok.no_files"));
    } else {
        ctx.out.info(&format!(
            "\n共 {} 个文件，合计 {}",
            rows.len(),
            human_bytes(total_size)
        ));
    }
    Ok(())
}

/// 读取文件前若干字节。
fn read_prefix(p: &std::path::Path, n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; n];
    let mut got = 0usize;
    while got < n {
        let Some(s) = buf.get_mut(got..) else { break };
        match f.read(s) {
            Ok(0) => break,
            Ok(k) => got = got.saturating_add(k),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// 过长字符串截断，保留尾部（路径的尾部信息量更大）。
fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let skip = n.saturating_sub(max.saturating_sub(1));
    let tail: String = s.chars().skip(skip).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_tail() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abcdefghij", 10), "abcdefghij");
        let t = truncate("abcdefghijk", 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.starts_with('…'));
        assert!(t.ends_with('k'));
    }

    #[test]
    fn truncate_handles_multibyte() {
        // 按字符而非字节截断，中文不能被切坏
        let s = "非常长的中文路径名称测试字符串";
        let t = truncate(s, 5);
        assert_eq!(t.chars().count(), 5);
    }
}
