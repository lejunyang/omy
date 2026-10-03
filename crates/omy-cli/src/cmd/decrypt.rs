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

    /// 用这台机器上的设备密钥解锁，不输密码（需要 Windows Hello 确认）
    ///
    /// 要求先用 `omy key device <文件> add` 挂过。
    #[arg(long, conflicts_with_all = ["password_env", "password_file", "password_stdin"])]
    pub device: bool,
}

/// 执行 `decrypt`。
///
/// # Errors
///
/// 读取失败、密码不匹配、认证失败或写出失败时返回错误。
/// 从这台机器的安全硬件取出设备密钥，派生成 KEK。
///
/// 会弹 Windows Hello / Touch ID。失败原因要分清：没挂过、这台机器不支持、
/// 用户取消——三者的处置方式完全不同，混成一句「解锁失败」会让用户无从下手。
fn device_kek(vault_salt: &[u8; 16]) -> Result<Kek> {
    let p = omy_secret::device_protector("omy").map_err(|e| {
        anyhow::anyhow!(
            "这台机器上用不了设备密钥：{e}\n\n\
             Windows 需要 TPM 2.0 与 Windows Hello；macOS 需要已签名应用与 Touch ID。"
        )
    })?;
    let id = omy_core::devicekey::slot_id(vault_salt);
    let bio = p.name().to_owned();
    let secret = match omy_secret::Protector::retrieve(&p, &id) {
        Ok(k) => k,
        Err(omy_secret::Error::NotFound) => {
            bail!(
                "这台机器上没有为该文件所属的库保管设备密钥。\n\n\
                 先用 `omy key device <文件> add` 挂上，或改用密码解锁。"
            )
        }
        Err(omy_secret::Error::UserCancelled) => bail!("已取消{bio}确认"),
        Err(e) => bail!("取出设备密钥失败：{e}"),
    };
    Ok(omy_core::devicekey::kek_from_secret(&secret, vault_salt))
}
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    if a.output.is_some() && a.files.len() > 1 {
        bail!("-o/--output 只能用于单个输入；多个输入请用 --output-dir");
    }

    // 设备密钥路径没有「密码」这个东西，所以密码留空，
    // 后面按文件取 KEK 时再分流
    let pw = if a.device {
        Vec::new()
    } else {
        let src = PasswordSource {
            env: a.password_env.clone(),
            file: a.password_file.clone(),
            stdin: a.password_stdin,
        };
        read_password(&src, t("prompt.password"), false)?
    };

    let mut results = Vec::new();

    for f in &a.files {
        // 树形模式的产物是**目录**，不是单个 .omy 文件。不先分流的话
        // read_possibly_sharded 会拿目录去读，得到一个「拒绝访问」之类的
        // IO 错误——用户完全无法从中看出「这是树形加密的目录」
        if f.is_dir() {
            results.push(decrypt_as_tree(ctx, f, a, &pw)?);
            continue;
        }

        let data = read_possibly_sharded(ctx, f, a.ignore_missing_shards)?;
        let h = omy_core::file::peek_header(&data)?;

        // 每个文件的 vault_salt 可能不同，故按文件派生 KEK。
        // 同一 salt 的多个文件由 SessionKeys 缓存复用（见 scan 命令）。
        let kek = if a.device {
            device_kek(&h.vault_salt)?
        } else {
            Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?
        };
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
    Ok(dir.join(omy_core::unpack::sanitize_filename(name)))
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

/// 解开一棵树形加密的目录。
///
/// # 为什么要先去树里找一个文件
///
/// 单个 `.omy` 文件的头部自带 `vault_salt` 与 KDF 参数，可以直接派生 KEK。
/// 但树形模式的根是一个**目录**——目录没有头部。而目录名的解密又必须先有
/// KEK，于是形成了鸡生蛋问题。
///
/// 解法是从树里任意一个 `.omy` 文件的头部取参数：同一个 vault 内这些参数
/// 本就一致（`--vault` 的语义就是复用它们），取哪个都一样。
fn decrypt_as_tree(
    ctx: &Ctx<'_>,
    root: &Path,
    a: &Args,
    pw: &[u8],
) -> Result<serde_json::Value> {
    let Some(sample) = omy_core::tree::find_any_file(root) else {
        bail!(
            "{} 看起来不是树形加密的目录（里面找不到任何 .omy 文件）",
            root.display()
        );
    };
    // 只读头部那一小段：为了取 16 字节的盐去读一个几百 MB 的文件没有道理
    let head = read_head(&sample, 4096)?;
    let h = omy_core::file::peek_header(&head)?;
    let kek = Kek::from_password(pw, &h.vault_salt, h.argon2_params())?;

    if a.verify_only {
        // 校验模式不落盘，解到临时目录再删。放在密文旁边而不是系统 temp：
        // 大目录可能几十 GB，系统 temp 常在小分区上
        let tmp = root.with_file_name(format!(".omy-verify-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        let out = omy_core::tree::decrypt_tree(
            root,
            &tmp,
            &[kek],
            &h.vault_salt,
            h.cipher_id,
            None,
        );
        // 无论成败都清掉临时明文——它是完整的明文副本，留着是安全问题
        let _ = std::fs::remove_dir_all(&tmp);
        let rep = out?;
        ctx.out.success(&format!(
            "{} {}（树形，{} 个文件、{} 个目录）",
            t("ok.verified"),
            root.display(),
            rep.files,
            rep.dirs
        ));
        return Ok(json!({
            "input": root.display().to_string(),
            "verified": true,
            "mode": "tree",
            "files": rep.files,
            "dirs": rep.dirs,
        }));
    }

    let target = container_target(root, a)?;
    std::fs::create_dir_all(&target)
        .with_context(|| format!("创建输出目录 {} 失败", target.display()))?;

    let mut on_file = |name: &str, size: u64| {
        ctx.out.trace(&format!("  文件 {} （{}）", name, human_bytes(size)));
    };
    let rep = omy_core::tree::decrypt_tree(
        root,
        &target,
        &[kek],
        &h.vault_salt,
        h.cipher_id,
        Some(&mut on_file),
    )
    .with_context(|| format!("解开 {} 失败", root.display()))?;

    for sk in &rep.skipped {
        ctx.out.warn(&format!("跳过 {}", sk.path));
    }

    ctx.out.success(&format!(
        "已解开到 {}：{} 个文件、{} 个目录",
        rep.root.display(),
        rep.files,
        rep.dirs
    ));

    Ok(json!({
        "input": root.display().to_string(),
        "output": rep.root.display().to_string(),
        "mode": "tree",
        "files": rep.files,
        "dirs": rep.dirs,
    }))
}

/// 只读文件开头若干字节。
fn read_head(p: &Path, n: usize) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(p)
        .with_context(|| format!("打开 {} 失败", p.display()))?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

/// 把容器载荷展开到磁盘，并把结果展示给用户。
///
/// 落盘逻辑在 [`omy_core::unpack`]——路径逃逸防护与文件名兼容处理不能有
/// 两份实现（GUI 也要解容器）。这里只负责展示：人类可读的报告与 JSON 契约
/// 都是 CLI 的职责，不该进 core。
fn extract_container(
    ctx: &Ctx<'_>,
    idx: &omy_core::container::ContainerIndex,
    payload: &[u8],
    target: &Path,
) -> Result<serde_json::Value> {
    let rep = omy_core::unpack::extract_container(idx, payload, target)?;

    // 同 encrypt：没有链接就不提，免得每次都多一句「0 个符号链接」
    let links = if rep.links > 0 {
        format!("、{} 个符号链接", rep.links)
    } else {
        String::new()
    };
    ctx.out.success(&format!(
        "已解开容器到 {}：{} 个文件、{} 个目录{}",
        rep.root.display(),
        rep.files,
        rep.dirs,
        links
    ));

    // 元数据还原报告：不支持的项必须明确报告（决策 N3）
    if !rep.adjusted.is_empty() {
        ctx.out.warn(&format!(
            "{} 个文件名因当前平台限制被调整",
            rep.adjusted.len()
        ));
        for a in rep.adjusted.iter().take(10) {
            ctx.out.detail(&format!("  {a}"));
        }
    }
    if !rep.skipped.is_empty() {
        ctx.out.warn(&format!("{} 个条目未还原：", rep.skipped.len()));
        for s in rep.skipped.iter().take(10) {
            ctx.out.info(&format!("  {}（{}）", s.path, skip_label(s.reason)));
        }
    }
    report_metadata(ctx, &rep.metadata);

    Ok(json!({
        "container": true,
        "root": rep.root.display().to_string(),
        "files": rep.files,
        "dirs": rep.dirs,
        "links": rep.links,
        "adjusted_names": rep.adjusted,
        "skipped": rep.skipped.iter().map(|s| json!({
            "path": s.path,
            "reason": s.reason,
        })).collect::<Vec<_>>(),
        "metadata": metadata_json(&rep.metadata),
    }))
}

/// 未还原原因的中文说明。
///
/// 与 `meta_item_label` 同一个道理：`reason` 是给 JSON 与翻译键用的稳定
/// 标识，直接显示给用户就成了「symlink」这种半英文。
fn skip_label(reason: &str) -> &'static str {
    match reason {
        // 容器里的链接条目没记目标。正常不会出现（container 层会拒收），
        // 只可能来自手工构造或损坏的容器
        "symlink_no_target" => "符号链接缺少目标",
        // 说清「指向容器外」而不只是「已拒绝」：用户要能判断这是不是
        // 自己当初有意建的链接，还是容器来路不正
        "symlink_escapes_root" => "符号链接指向容器外，已拒绝还原",
        _ => "未知原因",
    }
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
        // 说清「这次不行」而不只是「符号链接」：用户看到光秃秃的
        // 「符号链接」不知道该做什么，而这一项恰好是开了开发者模式
        // 就能解决的
        U::Symlink => "符号链接（本机不允许创建）",
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
    fn skip_labels_cover_known_reasons() {
        // reason 是 core 给的稳定代号，这里保证每个已知代号都有中文说明。
        // 漏一个的话界面上会显示「未知原因」，而原因其实是知道的
        assert_eq!(skip_label("symlink_no_target"), "符号链接缺少目标");
        assert_eq!(
            skip_label("symlink_escapes_root"),
            "符号链接指向容器外，已拒绝还原"
        );
    }

    #[test]
    fn every_unsupported_kind_has_a_label() {
        use omy_core::restore::UnsupportedKind as U;
        // 新增 UnsupportedKind 时 match 是穷尽的，编译器会拦住漏写；
        // 但「写了却写成 code() 那种英文代号」编译器管不了，所以这里
        // 额外确认每一项都不是代号原文——直接显示 "symlink" 给用户，
        // 和显示「未知原因」一样没用
        for k in [U::Mode, U::Btime, U::Owner, U::Xattr, U::Symlink] {
            let label = meta_item_label(k);
            assert_ne!(label, k.code(), "{:?} 的说明不能直接用英文代号", k);
            assert!(!label.is_empty(), "{:?} 缺中文说明", k);
        }
    }

    // sanitize_filename / safe_join 的测试跟着实现一起搬到了
    // omy_core::unpack——留在这里会变成「测一个自己不再拥有的行为」，
    // 而且两处断言迟早分歧
}
