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
    // 统一用「偏移 + 长度」表达，避免闭/开区间混淆。
    // 早期实现让 parse_range 返回 (start, end) 又把 end 当开区间用，
    // 而用户按 HTTP Range 惯例写 --range 0-999 期望的是闭区间，
    // 结果每次都少读一个字节——对视频这类格式是致命的。
    let (start, length) = match &a.range {
        Some(r) => parse_range(r, total)?,
        None => (0, total),
    };
    let end_exclusive = start.saturating_add(length).min(total);

    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::with_capacity(256 * 1024, stdout.lock());

    // 按块流式输出，内存恒定
    let step = u64::from(h.chunk_size);
    let mut pos = start;
    while pos < end_exclusive {
        let want = step.min(end_exclusive.saturating_sub(pos));
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

/// 解析 `起始-结束` 范围，返回 `(偏移, 长度)`。
///
/// **端点按 HTTP Range 惯例是闭区间**：`0-999` 表示第 0 到第 999 字节，
/// 共 1000 字节。返回长度而非终点，是为了让调用方不可能弄错开闭区间。
///
/// 支持省略端点：
/// - `-1000` 表示**最后** 1000 字节（与 HTTP `Range: bytes=-1000` 一致）
/// - `500-` 表示从 500 到结尾
///
/// # Errors
///
/// 格式非法、数值无法解析或起点大于终点时返回错误。
fn parse_range(s: &str, total: u64) -> Result<(u64, u64)> {
    let t = s.trim();
    let Some((a, b)) = t.split_once('-') else {
        anyhow::bail!("范围格式应为 起始-结束，如 0-999 / -1000 / 500-");
    };
    let a = a.trim();
    let b = b.trim();

    if total == 0 {
        return Ok((0, 0));
    }
    let last = total.saturating_sub(1);

    // suffix 形式：-N 表示最后 N 字节。
    // 这与 HTTP 语义一致；早期实现把它当成「前 N 字节」，
    // 与 Range 头行为不符，会让用惯 HTTP 的人踩坑。
    if a.is_empty() {
        if b.is_empty() {
            anyhow::bail!("范围不能同时省略起点与终点");
        }
        let n: u64 = b
            .parse()
            .map_err(|_| anyhow::anyhow!("无法解析后缀长度 {b:?}"))?;
        let n = n.min(total);
        return Ok((total.saturating_sub(n), n));
    }

    let start: u64 = a
        .parse()
        .map_err(|_| anyhow::anyhow!("无法解析范围起点 {a:?}"))?;
    if start > last {
        anyhow::bail!("范围起点 {start} 超出明文长度 {total}");
    }
    let end_inclusive = if b.is_empty() {
        last
    } else {
        b.parse::<u64>()
            .map_err(|_| anyhow::anyhow!("无法解析范围终点 {b:?}"))?
            .min(last)
    };
    if end_inclusive < start {
        anyhow::bail!("范围起点 {start} 大于终点 {end_inclusive}");
    }
    Ok((start, end_inclusive.saturating_sub(start).saturating_add(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing_is_inclusive() {
        // 闭区间：0-999 是 1000 字节。
        // 这个断言曾经写成 (0, 1000) 并「通过」了——
        // 因为当时的实现把终点当开区间，测试与实现同错，
        // 结果掩盖了每次少读一字节的缺陷。
        assert_eq!(parse_range("0-999", 5000).unwrap(), (0, 1000));
        assert_eq!(parse_range("0-0", 5000).unwrap(), (0, 1));
        assert_eq!(parse_range("100-199", 5000).unwrap(), (100, 100));
        assert_eq!(parse_range(" 100 - 200 ", 5000).unwrap(), (100, 101));
        // 开放结尾：500 到 4999，共 4500 字节
        assert_eq!(parse_range("500-", 5000).unwrap(), (500, 4500));
        // 超出总长的终点被裁剪
        assert_eq!(parse_range("0-99999", 5000).unwrap(), (0, 5000));
    }

    #[test]
    fn suffix_range_means_last_n_bytes() {
        // 与 HTTP Range: bytes=-1000 一致，取最后 1000 字节
        assert_eq!(parse_range("-1000", 5000).unwrap(), (4000, 1000));
        // N 超过总长时取全部
        assert_eq!(parse_range("-99999", 5000).unwrap(), (0, 5000));
    }

    #[test]
    fn full_file_range() {
        // 整个文件
        assert_eq!(parse_range("0-4999", 5000).unwrap(), (0, 5000));
        assert_eq!(parse_range("0-", 5000).unwrap(), (0, 5000));
    }

    #[test]
    fn range_rejects_bad_input() {
        assert!(parse_range("abc", 100).is_err());
        assert!(parse_range("100", 100).is_err());
        assert!(parse_range("200-100", 500).is_err());
        assert!(parse_range("x-y", 100).is_err());
        assert!(parse_range("-", 100).is_err());
        // 起点越界必须报错，而不是静默返回空
        assert!(parse_range("100-200", 100).is_err());
        assert!(parse_range("500-", 100).is_err());
    }

    #[test]
    fn empty_file_yields_empty_range() {
        assert_eq!(parse_range("0-100", 0).unwrap(), (0, 0));
    }
}
