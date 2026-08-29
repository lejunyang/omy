//! `omy shard`：分片切分与合并。
//!
//! 对应决策 D-18：手动指定大小、冗余 header、缺片可单独播放。

use super::Ctx;
use crate::output::{human_bytes, parse_size};
use anyhow::{Context as _, Result, bail};
use clap::{Args as ClapArgs, Subcommand};
use serde_json::json;
use std::path::{Path, PathBuf};

/// `shard` 的子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 将已加密文件切分为分片
    Split(SplitArgs),
    /// 合并分片
    Merge(MergeArgs),
    /// 检查分片完整性与缺口
    Check(CheckArgs),
}

/// `shard split` 的参数。
#[derive(Debug, ClapArgs)]
pub struct SplitArgs {
    /// 待切分的 .omy 文件
    pub file: PathBuf,

    /// 每片大小，如 4095M（FAT32 单文件上限）
    #[arg(long, value_name = "SIZE")]
    pub size: String,

    /// 输出目录，默认与源文件同目录
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// 每片包含冗余头（默认开启），使丢失首片仍可恢复元信息
    #[arg(long, default_value_t = true)]
    pub redundant_header: bool,

    /// 不写冗余头
    #[arg(long, conflicts_with = "redundant_header")]
    pub no_redundant_header: bool,

    /// 切分后删除原文件
    #[arg(long)]
    pub remove_source: bool,
}

/// `shard merge` 的参数。
#[derive(Debug, ClapArgs)]
pub struct MergeArgs {
    /// 任一分片的路径，或不带序号的基名
    pub input: PathBuf,

    /// 输出文件路径
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
}

/// `shard check` 的参数。
#[derive(Debug, ClapArgs)]
pub struct CheckArgs {
    /// 任一分片的路径，或不带序号的基名
    pub input: PathBuf,
}

/// 执行 `shard` 子命令。
///
/// # Errors
///
/// 读写失败、分片不完整或参数非法时返回错误。
pub fn run(ctx: &Ctx<'_>, c: &Cmd) -> Result<()> {
    match c {
        Cmd::Split(a) => split(ctx, a),
        Cmd::Merge(a) => merge(ctx, a),
        Cmd::Check(a) => check(ctx, a),
    }
}

fn split(ctx: &Ctx<'_>, a: &SplitArgs) -> Result<()> {
    let size = parse_size(&a.size)?;
    let size_usize = usize::try_from(size)
        .map_err(|_| anyhow::anyhow!("分片大小 {size} 超出本平台上限"))?;

    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;

    // 必须是 omy 文件：对任意文件分片没有意义，合并时也无法校验
    if !omy_core::file::is_omy_file(&data) {
        bail!(
            "{} 不是 omy 文件。shard split 只处理已加密的 .omy 文件。",
            a.file.display()
        );
    }

    let redundant = !a.no_redundant_header;
    // 冗余头就是主文件的头部字节：丢了首片时靠它恢复元信息
    let h = omy_core::file::peek_header(&data)?;
    let hlen = usize::try_from(h.header_len)
        .map_err(|_| anyhow::anyhow!("header_len 超出本平台上限"))?;
    let head = data
        .get(..hlen)
        .ok_or_else(|| anyhow::anyhow!("文件短于头部声明长度，可能已损坏"))?;
    let parts = omy_core::shard::split(
        &data,
        &h.file_uuid,
        size_usize,
        if redundant { Some(head) } else { None },
    )?;

    let dir = a
        .output_dir
        .clone()
        .unwrap_or_else(|| a.file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("创建 {} 失败", dir.display()))?;

    let base = a
        .file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let mut written = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let p = dir.join(format!("{base}.{i:03}"));
        omy_core::fsatomic::write_atomic(&p, part)?;
        written.push(json!({
            "index": i,
            "path": p.display().to_string(),
            "size": part.len(),
        }));
        ctx.out.detail(&format!(
            "第 {} 片 → {}（{}）",
            i,
            p.display(),
            human_bytes(part.len() as u64)
        ));
    }

    ctx.out.success(&format!(
        "已切分为 {} 片，每片上限 {}{}",
        parts.len(),
        human_bytes(size),
        if redundant { "，含冗余头" } else { "" }
    ));

    if a.remove_source {
        // 删源前先验证分片能合回原样——顺序不能颠倒
        let back = omy_core::shard::merge(&parts)?;
        if back != data {
            bail!("校验失败：分片合并结果与源文件不一致，已保留源文件");
        }
        std::fs::remove_file(&a.file)
            .with_context(|| format!("删除 {} 失败", a.file.display()))?;
        ctx.out.info(&format!("已删除源文件 {}", a.file.display()));
    }

    ctx.out.result(
        "",
        &json!({
            "shards": written,
            "count": parts.len(),
            "shard_size": size,
            "redundant_header": redundant,
        }),
    );
    Ok(())
}

fn merge(ctx: &Ctx<'_>, a: &MergeArgs) -> Result<()> {
    let (base, parts) = load_parts(&a.input)?;
    if parts.is_empty() {
        bail!("未找到 {} 的任何分片", a.input.display());
    }
    ctx.out.detail(&format!("找到 {} 个分片", parts.len()));

    let merged = omy_core::shard::merge(&parts)?;
    let out = a.output.clone().unwrap_or_else(|| base.clone());
    omy_core::fsatomic::write_atomic(&out, &merged)?;

    ctx.out.success(&format!(
        "已合并 {} 片 → {}（{}）",
        parts.len(),
        out.display(),
        human_bytes(merged.len() as u64)
    ));
    ctx.out.result(
        "",
        &json!({
            "output": out.display().to_string(),
            "shards": parts.len(),
            "size": merged.len(),
        }),
    );
    Ok(())
}

fn check(ctx: &Ctx<'_>, a: &CheckArgs) -> Result<()> {
    let (_, parts) = load_parts(&a.input)?;
    if parts.is_empty() {
        bail!("未找到 {} 的任何分片", a.input.display());
    }

    let cov = omy_core::shard::analyze_coverage(&parts)?;
    let complete = cov.missing.is_empty();

    let mut human = format!("分片数量    {} / {}\n", parts.len(), cov.total);
    human.push_str(&format!(
        "完整性      {}\n",
        if complete { "完整" } else { "有缺口" }
    ));
    if !cov.missing.is_empty() {
        human.push_str(&format!(
            "缺失片号    {}\n",
            cov.missing
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for (i, (off, len)) in cov.holes.iter().enumerate() {
        human.push_str(&format!(
            "  缺口 {}     偏移 {} 起 {}\n",
            i + 1,
            off,
            human_bytes(*len)
        ));
    }

    // 冗余头恢复能力：丢了首片是否还能读到元信息
    let mut recoverable = false;
    for p in &parts {
        if omy_core::shard::recover_header_from_shard(p).is_ok() {
            recoverable = true;
            break;
        }
    }
    human.push_str(&format!(
        "冗余头      {}\n",
        if recoverable {
            "存在，丢失首片仍可恢复元信息"
        } else {
            "不存在，丢失首片将无法解析"
        }
    ));

    ctx.out.result(
        &human,
        &json!({
            "shards": parts.len(),
            "total": cov.total,
            "complete": complete,
            "missing": cov.missing,
            "holes": cov.holes.iter().map(|(o, l)| json!({"offset": o, "length": l})).collect::<Vec<_>>(),
            "redundant_header": recoverable,
        }),
    );

    if !complete {
        return Err(anyhow::Error::new(omy_core::Error::MissingShards {
            missing: cov.missing.clone(),
            total: cov.total,
        }));
    }
    Ok(())
}

/// 载入某个基名对应的全部分片。
///
/// `input` 可以是 `a.omy`、`a.omy.000` 中任一形式。
fn load_parts(input: &Path) -> Result<(PathBuf, Vec<Vec<u8>>)> {
    let name = input.file_name().unwrap_or_default().to_string_lossy().to_string();
    let dir = input.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();

    // 若传入的是 xxx.omy.000，去掉序号得到基名
    let base_name = match name.rsplit_once('.') {
        Some((stem, suffix)) if suffix.len() == 3 && suffix.chars().all(|c| c.is_ascii_digit()) => {
            stem.to_string()
        }
        _ => name.clone(),
    };
    let base = dir.join(&base_name);

    let mut parts = Vec::new();
    // 关键：遇到缺失的序号**不能**立刻 break。
    // 缺片正是需要检测的情况——若在缺口处停下，只会读到缺口之前的片，
    // 于此 analyze_coverage 看到的 shard_total 与片数矛盾，
    // 报出的会是格式错误而不是「缺片」，退出码也就不是约定的 6。
    //
    // 策略：连续 miss 达到阈值才认为真的到头了。
    let mut consecutive_miss = 0u32;
    const MISS_TOLERANCE: u32 = 64;
    for i in 0..10_000u32 {
        let p = dir.join(format!("{base_name}.{i:03}"));
        if p.exists() {
            consecutive_miss = 0;
            parts.push(std::fs::read(&p).with_context(|| format!("读取 {} 失败", p.display()))?);
        } else {
            // 一片都还没找到就连续缺失，说明这个基名没有分片
            if parts.is_empty() && i > 0 {
                break;
            }
            consecutive_miss = consecutive_miss.saturating_add(1);
            if consecutive_miss >= MISS_TOLERANCE {
                break;
            }
        }
    }
    Ok((base, parts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_name_extraction() {
        // 传入分片路径时应能推出基名
        let (b, _) = load_parts(Path::new("/tmp/nonexistent/a.omy.000")).unwrap();
        assert!(b.to_string_lossy().ends_with("a.omy"));

        // 传入基名时保持不变
        let (b2, _) = load_parts(Path::new("/tmp/nonexistent/a.omy")).unwrap();
        assert!(b2.to_string_lossy().ends_with("a.omy"));
    }

    #[test]
    fn non_numeric_suffix_is_not_shard_index() {
        // .omy 不该被当成序号剥掉
        let (b, _) = load_parts(Path::new("/tmp/nonexistent/movie.omy")).unwrap();
        assert!(b.to_string_lossy().ends_with("movie.omy"));
    }
}
