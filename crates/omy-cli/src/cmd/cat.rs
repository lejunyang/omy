//! `omy cat`：解密并输出到标准输出。
//!
//! 这是 CLI 相对 GUI 的独特价值：
//!
//! ```bash
//! omy cat movie.omy --password-file pw | mpv -
//! omy cat app.log.omy --password-stdin | grep ERROR
//! ```
//!
//! # 流式解密
//!
//! 按块读取并立即写出，内存占用恒定（约 2 个块大小），不受文件大小影响。
//! 播放大视频时这一点是必需的——若整体载入，4 GB 视频会吃掉 4 GB 内存。

use super::Ctx;
use crate::i18n::t;
use crate::password::{PasswordSource, read_password};
use anyhow::{Context as _, Result};
use clap::Args as ClapArgs;
use omy_core::crypto::Kek;
use omy_core::source::{BlockSource, LocalFileSource, read_source_range};
use std::io::Write;
use std::path::PathBuf;

/// `cat` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 待输出的 .omy 文件
    pub file: PathBuf,

    /// 只输出指定范围，格式 `起始-结束`（字节，含起始不含结束）
    #[arg(long, value_name = "RANGE")]
    pub range: Option<String>,

    /// 从环境变量读取密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// 执行 `cat`。
///
/// # Errors
///
/// 读取失败、密码不匹配、认证失败或写出失败时返回错误。
pub fn run(_ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let pw = read_password(&src, t("prompt.password"), false)?;

    let source = LocalFileSource::open(&a.file)?;
    let h = source.header();

    let kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;

    // open 需要完整头部字节。只读头部那一段，不读载荷。
    let head = read_head(&a.file, h.header_len as usize)?;
    let opened = omy_core::file::open(&head, &[kek])?;

    let total = h.plaintext_size;
    let (start, end) = match &a.range {
        Some(r) => parse_range(r, total)?,
        None => (0, total),
    };

    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());

    // 按块流式输出，内存恒定
    let step = u64::from(h.chunk_size);
    let mut pos = start;
    while pos < end {
        let want = step.min(end.saturating_sub(pos));
        let buf = read_source_range(&source, &opened, pos, want)?;
        if buf.is_empty() {
            break;
        }
        w.write_all(&buf).context("写入标准输出失败")?;
        pos = pos.saturating_add(buf.len() as u64);
    }
    w.flush().context("刷新标准输出失败")?;
    Ok(())
}

/// 只读文件的头部若干字节。
fn read_head(p: &std::path::Path, n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)
        .with_context(|| format!("打开 {} 失败", p.display()))?;
    let mut buf = vec![0u8; n];
    f.read_exact(&mut buf)
        .with_context(|| format!("读取 {} 的头部失败", p.display()))?;
    Ok(buf)
}

/// 解析 `起始-结束` 范围。
///
/// 支持省略端点：`-1000` 表示前 1000 字节，`500-` 表示从 500 到结尾。
fn parse_range(s: &str, total: u64) -> Result<(u64, u64)> {
    let t = s.trim();
    let Some((a, b)) = t.split_once('-') else {
        anyhow::bail!("范围格式应为 起始-结束，如 0-1000 / -1000 / 500-");
    };
    let start = if a.trim().is_empty() {
        0
    } else {
        a.trim()
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("无法解析范围起点 {a:?}"))?
    };
    let end = if b.trim().is_empty() {
        total
    } else {
        b.trim()
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("无法解析范围终点 {b:?}"))?
    };
    if start > end {
        anyhow::bail!("范围起点 {start} 大于终点 {end}");
    }
    Ok((start.min(total), end.min(total)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("0-1000", 5000).unwrap(), (0, 1000));
        assert_eq!(parse_range("-1000", 5000).unwrap(), (0, 1000));
        assert_eq!(parse_range("500-", 5000).unwrap(), (500, 5000));
        assert_eq!(parse_range(" 100 - 200 ", 5000).unwrap(), (100, 200));
        // 超出总长必须裁剪而非报错
        assert_eq!(parse_range("0-99999", 5000).unwrap(), (0, 5000));
    }

    #[test]
    fn range_rejects_bad_input() {
        assert!(parse_range("abc", 100).is_err());
        assert!(parse_range("100", 100).is_err());
        assert!(parse_range("200-100", 500).is_err());
        assert!(parse_range("x-y", 100).is_err());
    }
}
