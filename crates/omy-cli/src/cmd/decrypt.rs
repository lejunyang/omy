//! `omy decrypt`：解密文件。

use super::Ctx;
use crate::i18n::t;
use crate::output::human_bytes;
use crate::password::{PasswordSource, read_password};
use anyhow::{Context as _, Result, bail};
use clap::Args as ClapArgs;
use omy_core::crypto::Kek;
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};

/// `decrypt` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 待解密的 .omy 文件
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

    /// 输出路径（单个输入时有效）
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// 批量输出目录
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// 输出到标准输出
    #[arg(long, conflicts_with_all = ["output", "output_dir"])]
    pub stdout: bool,

    /// 只校验不写出
    #[arg(long)]
    pub verify_only: bool,

    /// 缺片时输出可用部分
    #[arg(long)]
    pub ignore_missing_shards: bool,

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

/// 执行 `decrypt`。
///
/// # Errors
///
/// 读取失败、密码不匹配、认证失败或写出失败时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    if a.output.is_some() && a.files.len() > 1 {
        bail!("-o/--output 只能用于单个输入；多个输入请用 --output-dir");
    }

    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let pw = read_password(&src, t("prompt.password"), false)?;

    let mut results = Vec::new();

    for f in &a.files {
        let data = read_possibly_sharded(ctx, f, a.ignore_missing_shards)?;
        let h = omy_core::file::peek_header(&data)?;

        // 每个文件的 vault_salt 可能不同，故按文件派生 KEK。
        // 同一 salt 的多个文件由 SessionKeys 缓存复用（见 scan 命令）。
        let kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;
        let opened = omy_core::file::open(&data, &[kek])?;

        if opened.is_container() {
            let idx = opened.folder_index()?;
            let plain = opened.decrypt_all(&data)?;
            if a.verify_only {
                ctx.out.success(&format!(
                    "{} {}（容器，{} 个条目）",
                    t("ok.verified"),
                    f.display(),
                    idx.entries.len()
                ));
                results.push(json!({
                    "input": f.display().to_string(),
                    "verified": true,
                    "container": true,
                    "entries": idx.entries.len(),
                }));
                continue;
            }
            let target = container_target(f, a)?;
            let report = extract_container(ctx, &idx, &plain, &target)?;
            results.push(report);
            continue;
        }

        let plain = opened.decrypt_all(&data)?;

        if a.verify_only {
            ctx.out.success(&format!(
                "{} {}（{}）",
                t("ok.verified"),
                f.display(),
                human_bytes(plain.len() as u64)
            ));
            results.push(json!({
                "input": f.display().to_string(),
                "verified": true,
                "plaintext_size": plain.len(),
            }));
            continue;
        }

        if a.stdout {
            std::io::stdout()
                .write_all(&plain)
                .context("写入标准输出失败")?;
            continue;
        }

        let name = opened.filename().unwrap_or_else(|_| {
            // 无文件名 TLV 时，去掉 .omy 后缀作为回退
            f.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| String::from("decrypted"))
        });
        let out_path = resolve_output(f, &name, a)?;
        omy_core::fsatomic::write_atomic(&out_path, &plain)?;

        ctx.out.success(&format!(
            "{} {} → {}（{}）",
            t("ok.decrypted"),
            f.display(),
            out_path.display(),
            human_bytes(plain.len() as u64)
        ));
        results.push(json!({
            "input": f.display().to_string(),
            "output": out_path.display().to_string(),
            "plaintext_size": plain.len(),
        }));
    }

    ctx.out.result("", &json!({ "files": results }));
    Ok(())
}

/// 读取文件，自动合并同名分片。
fn read_possibly_sharded(ctx: &Ctx<'_>, f: &Path, ignore_missing: bool) -> Result<Vec<u8>> {
    // 先看是否存在 .000 分片
    let first = shard_path(f, 0);
    if !first.exists() {
        return std::fs::read(f).with_context(|| format!("读取 {} 失败", f.display()));
    }

    let mut parts = Vec::new();
    // 与 shard 命令一致：缺片时不能在缺口处停下，
    // 否则会把「缺片」误报成格式错误，退出码也就不是约定的 6。
    let mut consecutive_miss = 0u32;
    const MISS_TOLERANCE: u32 = 64;
    for i in 0..10_000u32 {
        let p = shard_path(f, i);
        if p.exists() {
            consecutive_miss = 0;
            parts.push(std::fs::read(&p).with_context(|| format!("读取分片 {} 失败", p.display()))?);
        } else {
            if parts.is_empty() && i > 0 {
                break;
            }
            consecutive_miss = consecutive_miss.saturating_add(1);
            if consecutive_miss >= MISS_TOLERANCE {
                break;
            }
        }
    }
    ctx.out.detail(&format!("找到 {} 个分片，正在合并", parts.len()));

    match omy_core::shard::merge(&parts) {
        Ok(v) => Ok(v),
        Err(e) if ignore_missing => {
            ctx.out.warn(&format!("分片不完整：{e}。按 --ignore-missing-shards 继续。"));
            let cov = omy_core::shard::analyze_coverage(&parts)?;
            ctx.out.info(&format!(
                "共 {} 片，缺失 {} 片，形成 {} 个不可读区间",
                cov.total,
                cov.missing.len(),
                cov.holes.len()
            ));
            // 缺片时无法产出完整明文：落在缺口的块必然认证失败。
            // 与其写出一个静默残缺的文件，不如明确失败——
            // 部分播放属于 omy-media 的能力（缺片降级播放），不是 decrypt 的职责。
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

fn shard_path(f: &Path, i: u32) -> PathBuf {
    let name = f.file_name().unwrap_or_default().to_string_lossy();
    f.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{name}.{i:03}"))
}

/// 解密单文件时的输出路径。
fn resolve_output(input: &Path, name: &str, a: &Args) -> Result<PathBuf> {
    if let Some(o) = &a.output {
        return Ok(o.clone());
    }
    let dir = if let Some(d) = &a.output_dir {
        std::fs::create_dir_all(d)
            .with_context(|| format!("创建输出目录 {} 失败", d.display()))?;
        d.clone()
    } else {
        input.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
    };
    Ok(dir.join(sanitize_filename(name)))
}

/// 容器解开到哪个目录。
fn container_target(input: &Path, a: &Args) -> Result<PathBuf> {
    if let Some(o) = &a.output {
        return Ok(o.clone());
    }
    if let Some(d) = &a.output_dir {
        return Ok(d.clone());
    }
    Ok(input.parent().unwrap_or_else(|| Path::new(".")).to_path_buf())
}

/// 把容器载荷展开到磁盘。
///
/// 每个条目的路径组件在 `ContainerIndex::parse` 阶段已校验安全性，
/// 此处再做一次落盘前的最终确认——纵深防御，防止后续改动引入回归。
fn extract_container(
    ctx: &Ctx<'_>,
    idx: &omy_core::container::ContainerIndex,
    payload: &[u8],
    target: &Path,
) -> Result<serde_json::Value> {
    use omy_core::container::EntryKind;

    let root = target.join(sanitize_filename(&idx.root));
    std::fs::create_dir_all(&root)
        .with_context(|| format!("创建 {} 失败", root.display()))?;

    let mut n_files = 0usize;
    let mut n_dirs = 0usize;
    let mut adjusted: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    // 先建所有目录，含空目录——空目录必须显式还原，否则会丢失
    for e in &idx.entries {
        if e.kind == EntryKind::Dir {
            let p = safe_join(&root, &e.path)?;
            std::fs::create_dir_all(&p)
                .with_context(|| format!("创建目录 {} 失败", p.display()))?;
            n_dirs += 1;
        }
    }

    for e in &idx.entries {
        match e.kind {
            EntryKind::File => {
                let (p, was_adjusted) = safe_join_reporting(&root, &e.path)?;
                if was_adjusted {
                    adjusted.push(e.display_path());
                }
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let start = usize::try_from(e.offset).unwrap_or(usize::MAX);
                let len = usize::try_from(e.size).unwrap_or(0);
                let end = start.saturating_add(len);
                let bytes = payload.get(start..end).ok_or_else(|| {
                    anyhow::anyhow!(
                        "条目 {} 的区间 {}..{} 超出载荷长度 {}",
                        e.display_path(),
                        start,
                        end,
                        payload.len()
                    )
                })?;

                // 逐文件哈希校验：容器里某个文件坏了要能精确指出是哪个
                if let Some(expect) = &e.hash {
                    let actual = omy_core::util::blake2b_256(bytes);
                    if &actual != expect {
                        bail!("条目 {} 的内容哈希不匹配", e.display_path());
                    }
                }
                std::fs::write(&p, bytes)
                    .with_context(|| format!("写入 {} 失败", p.display()))?;
                n_files += 1;
            }
            EntryKind::Symlink => {
                // 符号链接跨平台差异大，按 N3 决策：不静默丢弃，明确报告
                skipped.push(format!("{}（符号链接）", e.display_path()));
            }
            EntryKind::Dir => {}
        }
    }

    // 元数据还原必须在此处——所有条目都已落盘之后。
    // 新建目录项会更新父目录的 mtime，边写边设会被随后的写入冲掉，
    // 症状是「文件时间对了、目录时间还是现在」，只在非空目录上出现
    let meta_rep = omy_core::restore::restore_metadata(&root, idx);

    ctx.out.success(&format!(
        "已解开容器到 {}：{n_files} 个文件、{n_dirs} 个目录",
        root.display()
    ));

    // 元数据还原报告：不支持的项必须明确报告（决策 N3）
    if !adjusted.is_empty() {
        ctx.out.warn(&format!(
            "{} 个文件名因当前平台限制被调整",
            adjusted.len()
        ));
        for a in adjusted.iter().take(10) {
            ctx.out.detail(&format!("  {a}"));
        }
    }
    if !skipped.is_empty() {
        ctx.out.warn(&format!("{} 个条目未还原：", skipped.len()));
        for s in skipped.iter().take(10) {
            ctx.out.info(&format!("  {s}"));
        }
    }
    report_metadata(ctx, &meta_rep);

    Ok(json!({
        "container": true,
        "root": root.display().to_string(),
        "files": n_files,
        "dirs": n_dirs,
        "adjusted_names": adjusted,
        "skipped": skipped,
        "metadata": metadata_json(&meta_rep),
    }))
}

/// 展示元数据还原报告（文档 05 §4.4）。
///
/// 「平台不支持」和「尝试了但失败」分开报，因为对用户的含义不同：
/// 前者换台机器解开就能拿到，后者是这次的问题、重试可能就好。
/// 合并会让人要么以为文件坏了，要么放弃重试白丢元数据。
fn report_metadata(ctx: &Ctx<'_>, rep: &omy_core::restore::RestoreReport) {
    if !rep.unsupported.is_empty() {
        ctx.out.warn("部分元数据未能还原（当前平台限制）：");
        for (kind, n) in &rep.unsupported {
            // 必须是 info 而不是 detail：detail 只在 -v 下显示，
            // 而上一行的冒号已经承诺要列举。默认只看到标题的话，
            // 用户想知道的「未能还原什么」得加 -v 重跑一次才拿到——
            // 可这正是决策 N3 要求明确报告的那件事。
            // 列举也不会刷屏：项数上限就四个
            ctx.out
                .info(&format!("  {n} 个条目的{}", meta_item_label(*kind)));
        }
        // 这句不能省。少了它，用户以为元数据已经丢了，
        // 而实际上值一直在加密文件里，换个平台解开就能完整还原
        ctx.out
            .info("  完整元数据仍保存在加密文件中，在其他平台解开可还原更多项");
    }
    if !rep.failures.is_empty() {
        ctx.out
            .warn(&format!("{} 项元数据设置失败：", rep.failures.len()));
        // 同样用 info：这是需要用户自己处理的问题（文件被占用、权限
        // 不足），藏在 -v 后面等于没报
        for f in rep.failures.iter().take(10) {
            ctx.out
                .info(&format!("  {} 的 {}：{}", f.path, f.item, f.reason));
        }
        // 被截断时必须说清还剩多少：只显示前 10 条而不提总数，
        // 会让人以为问题就这么多
        if rep.failures.len() > 10 {
            ctx.out.info(&format!(
                "  （还有 {} 项，用 --json 查看完整列表）",
                rep.failures.len() - 10
            ));
        }
    }
}

/// 不支持项的中文说明。
///
/// 不直接用 `code()`：那是给 JSON 与翻译键用的稳定标识，
/// 直接显示给用户就成了「mode」「btime」这种看不懂的词。
fn meta_item_label(kind: omy_core::restore::UnsupportedKind) -> &'static str {
    use omy_core::restore::UnsupportedKind as U;
    match kind {
        U::Mode => "POSIX 权限位",
        U::Btime => "创建时间",
        U::Owner => "属主（uid/gid）",
        U::Xattr => "扩展属性",
    }
}

/// 元数据报告的 JSON 形态。
///
/// `unsupported` 用对象数组而不是把 code 直接当键：条目数是数据不是键名，
/// 前者结构稳定、便于脚本遍历，后者每多一项就多一个字段。
fn metadata_json(rep: &omy_core::restore::RestoreReport) -> serde_json::Value {
    json!({
        "mtime_restored": rep.mtime_restored,
        "mode_restored": rep.mode_restored,
        "unsupported": rep.unsupported.iter().map(|(k, n)| json!({
            "item": k.code(),
            "count": n,
        })).collect::<Vec<_>>(),
        "failures": rep.failures.iter().map(|f| json!({
            "path": f.path,
            "item": f.item,
            "reason": f.reason,
        })).collect::<Vec<_>>(),
    })
}

/// 安全拼接路径，拒绝任何逃出 root 的结果。
fn safe_join(root: &Path, comps: &[String]) -> Result<PathBuf> {
    Ok(safe_join_reporting(root, comps)?.0)
}

/// 安全拼接并报告是否调整过文件名。
fn safe_join_reporting(root: &Path, comps: &[String]) -> Result<(PathBuf, bool)> {
    let mut p = root.to_path_buf();
    let mut adjusted = false;
    for c in comps {
        // 二次校验：解析阶段已查，此处防止后续改动引入回归
        omy_core::container::validate_component(c)?;
        let s = sanitize_filename(c);
        if s != *c {
            adjusted = true;
        }
        p.push(s);
    }
    // 最终确认结果仍在 root 之内
    if !p.starts_with(root) {
        bail!("路径 {} 逃出目标目录", p.display());
    }
    Ok((p, adjusted))
}

/// 把文件名调整为当前平台可用形式。
///
/// Windows 禁止 `\ / : * ? " < > |` 与一批保留名。绝不静默丢弃，
/// 调用方会把调整过的项报告给用户（决策 N3）。
fn sanitize_filename(name: &str) -> String {
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
        // 结尾的点与空格在 Windows 上会被吞掉
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

    #[test]
    fn shard_path_naming() {
        let p = Path::new("/tmp/a.omy");
        assert!(shard_path(p, 0).to_string_lossy().ends_with("a.omy.000"));
        assert!(shard_path(p, 12).to_string_lossy().ends_with("a.omy.012"));
        assert!(shard_path(p, 999).to_string_lossy().ends_with("a.omy.999"));
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
        let root = Path::new("/tmp/root");
        assert!(safe_join(root, &["..".to_string()]).is_err());
        assert!(safe_join(root, &["a".to_string(), "..".to_string()]).is_err());
        let ok = safe_join(root, &["a".to_string(), "b.txt".to_string()]).unwrap();
        assert!(ok.starts_with(root));
    }
}
