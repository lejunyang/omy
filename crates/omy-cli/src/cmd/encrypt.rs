//! `omy encrypt`：加密文件或目录。
//!
//! # 批量加密复用 KEK
//!
//! 对多个文件加密时**只跑一次 Argon2**，之后每个文件用同一个 KEK 走 HKDF
//! 派生各自的 slot 密钥。实测差距约 300,000 倍（见验证报告 3.2.1），
//! 这不是优化而是可用性前提。
//!
//! # 原子写入
//!
//! 每个输出文件都经 `tmp → fsync → rename → fsync 父目录`，
//! 中途崩溃不会留下看似正常实则损坏的文件（风险登记册 R6）。

use super::{Cipher, Ctx, KdfProfile, NameMode};
use crate::i18n::t;
use crate::output::{human_bytes, parse_size};
use crate::password::{PasswordSource, read_password};
use anyhow::{Context as _, Result, bail};
use clap::Args as ClapArgs;
use omy_core::container::{ContainerBuilder, EntryMeta};
use omy_core::crypto::Kek;
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt as core_encrypt};
use serde_json::json;
use std::path::{Path, PathBuf};

/// `encrypt` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 待加密的文件或目录
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    /// 输出路径（单个输入时有效）
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// 批量输出目录
    #[arg(long, value_name = "DIR")]
    pub output_dir: Option<PathBuf>,

    /// 文件名处理模式
    #[arg(long, value_enum, value_name = "MODE")]
    pub name_mode: Option<NameMode>,

    /// 从环境变量读取密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,

    /// 追加更多密码 slot（可重复，最多 8 个）。仅交互式输入。
    #[arg(long, action = clap::ArgAction::Count)]
    pub add_password: u8,

    /// KDF 档位
    #[arg(long, value_enum, value_name = "PROFILE")]
    pub kdf_profile: Option<KdfProfile>,

    /// AEAD 算法
    #[arg(long, value_enum, value_name = "CIPHER")]
    pub cipher: Option<Cipher>,

    /// 分块大小，如 256K / 1M / 4M
    #[arg(long, value_name = "SIZE")]
    pub chunk_size: Option<String>,

    /// 启用 zstd 压缩
    #[arg(long)]
    pub compress: bool,

    /// zstd 压缩级别（1–19）
    #[arg(long, value_name = "N")]
    pub compress_level: Option<i32>,

    /// 目录加密模式：container 打包成单文件，tree 逐个加密
    #[arg(long, value_enum, value_name = "MODE", default_value = "container")]
    pub mode: DirMode,

    /// 保留原文件（默认）
    #[arg(long, conflicts_with = "delete_original")]
    pub keep_original: bool,

    /// 删除原文件（需 --yes 或交互确认）
    #[arg(long)]
    pub delete_original: bool,
}

/// 目录加密模式，对应决策 D-05。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DirMode {
    /// 打包成单个 .omy，完全隐藏目录结构
    Container,
    /// 逐个加密，保持目录结构，支持增量同步
    Tree,
}

/// 执行 `encrypt`。
///
/// # Errors
///
/// 输入不存在、密码读取失败、加密失败或写入失败时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    if a.output.is_some() && a.paths.len() > 1 {
        bail!("-o/--output 只能用于单个输入；多个输入请用 --output-dir");
    }

    // 参数优先级：命令行 > 配置文件 > 内置默认
    let profile = match a.kdf_profile {
        Some(p) => p,
        None => KdfProfile::from_name(&ctx.cfg.defaults.kdf_profile)?,
    };
    let cipher = match a.cipher {
        Some(c) => c,
        None => Cipher::from_name(&ctx.cfg.defaults.cipher)?,
    };
    let chunk = match &a.chunk_size {
        Some(s) => parse_size(s)?,
        None => parse_size(&ctx.cfg.defaults.chunk_size)?,
    };
    let chunk_u32 = u32::try_from(chunk)
        .map_err(|_| anyhow::anyhow!("分块大小 {chunk} 超出 u32 范围"))?;
    let name_mode = match a.name_mode {
        Some(m) => m,
        None => NameMode::from_name(&ctx.cfg.defaults.name_mode)?,
    };
    let compress = a.compress || ctx.cfg.compress.enabled;
    let level = a.compress_level.unwrap_or(ctx.cfg.compress.level);
    if compress && !(1..=19).contains(&level) {
        bail!("压缩级别必须在 1–19 之间，当前为 {level}");
    }

    // 读密码。多个 slot 时逐个提示。
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let mut passwords: Vec<Vec<u8>> = Vec::new();
    passwords.push(read_password(&src, t("prompt.password"), src.is_interactive())?);
    for i in 0..a.add_password {
        let p = read_password(
            &PasswordSource::default(),
            &format!("追加密码 {}", i + 2),
            true,
        )?;
        passwords.push(p);
    }
    if passwords.len() > 8 {
        bail!("最多 8 个密码 slot，当前 {}", passwords.len());
    }

    // vault salt：同一次调用内共享，使 KEK 可复用
    let vault_salt = omy_core::util::random_16();
    let params = profile.params();

    ctx.out.detail(&format!(
        "派生 KEK：Argon2id m={} t={} p={}（{} 个密码，只跑一次）",
        human_bytes(u64::from(params.m_kib) * 1024),
        params.t,
        params.p,
        passwords.len()
    ));

    let mut keks = Vec::with_capacity(passwords.len());
    for pw in &passwords {
        keks.push(Kek::from_password(pw, &vault_salt, params)?);
    }

    let mut results = Vec::new();
    let mut total_in = 0u64;
    let mut total_out = 0u64;

    for p in &a.paths {
        let meta = std::fs::metadata(p)
            .with_context(|| format!("无法访问 {}", p.display()))?;

        let (plaintext, folder_index, name) = if meta.is_dir() {
            match a.mode {
                DirMode::Container => {
                    let (p, idx, n) = build_container(ctx, p)?;
                    (p, Some(idx), n)
                }
                DirMode::Tree => {
                    bail!(
                        "tree 模式尚未实现。当前可用 --mode container 打包成单文件。\n\
                         （tree 模式需要目录名加密与 base32 编码，见设计文档 05 号 §3）"
                    )
                }
            }
        } else {
            let data = std::fs::read(p)
                .with_context(|| format!("读取 {} 失败", p.display()))?;
            let n = p
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| String::from("unnamed"));
            (data, None, n)
        };

        let out_path = resolve_output(p, a, meta.is_dir())?;

        let opts = EncryptOptions {
            filename: match name_mode {
                NameMode::Plain => None,
                NameMode::Encrypt | NameMode::KeepExt => Some(name.clone()),
            },
            preserve_extension: name_mode == NameMode::KeepExt,
            compress,
            zstd_level: level,
            chunk_size: chunk_u32,
            cipher: cipher.id(),
            argon2: params,
            write_content_hash: true,
            thumbnail: None,
            media_meta: None,
            folder_index: folder_index.clone(),
        };

        let enc = core_encrypt(
            &plaintext,
            &keks,
            &vault_salt,
            &opts,
            &RandomMaterial::generate(),
        )?;

        omy_core::fsatomic::write_atomic(&out_path, &enc.bytes)?;

        total_in += plaintext.len() as u64;
        total_out += enc.bytes.len() as u64;

        ctx.out.success(&format!(
            "{} {} → {}（{} → {}）",
            t("ok.encrypted"),
            p.display(),
            out_path.display(),
            human_bytes(plaintext.len() as u64),
            human_bytes(enc.bytes.len() as u64)
        ));

        results.push(json!({
            "input": p.display().to_string(),
            "output": out_path.display().to_string(),
            "plaintext_size": plaintext.len(),
            "encrypted_size": enc.bytes.len(),
        }));

        // 删除原文件前先校验加密产物可解密——绝不能先删后验
        if a.delete_original {
            verify_then_delete(ctx, p, &enc.bytes, &keks)?;
        }
    }

    ctx.out.result(
        "",
        &json!({
            "files": results,
            "total_plaintext": total_in,
            "total_encrypted": total_out,
            "kdf_profile": profile.name(),
        }),
    );
    Ok(())
}

/// 把目录打包成容器的明文载荷。
///
/// 返回 `(载荷字节, 索引编码, 根目录名)`。
fn build_container(ctx: &Ctx<'_>, root: &Path) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| String::from("folder"));

    let mut builder = ContainerBuilder::new(root_name.clone());
    let mut payload = Vec::new();
    let mut n_files = 0usize;
    let mut n_dirs = 0usize;

    for entry in walkdir::WalkDir::new(root).sort_by_file_name() {
        let entry = entry.with_context(|| format!("遍历 {} 失败", root.display()))?;
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path());
        if rel.as_os_str().is_empty() {
            continue; // 根自身
        }
        let comps: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();

        let md = entry.metadata().ok();
        let meta = md.as_ref().map_or_else(EntryMeta::default, meta_from_fs);

        if entry.file_type().is_dir() {
            builder.add_dir(comps.clone(), meta)?;
            n_dirs += 1;
            ctx.out.trace(&format!("  目录 {}", comps.join("/")));
        } else if entry.file_type().is_file() {
            let data = std::fs::read(entry.path())
                .with_context(|| format!("读取 {} 失败", entry.path().display()))?;
            let hash = blake2_256(&data);
            let size = data.len() as u64;
            builder.add_file(comps.clone(), size, Some(hash), meta)?;
            payload.extend_from_slice(&data);
            n_files += 1;
            ctx.out.trace(&format!(
                "  文件 {} （{}）",
                comps.join("/"),
                human_bytes(size)
            ));
        } else {
            // 符号链接等：WalkDir 默认不跟随。
            // 明确告知用户而非静默丢弃——用户需要知道有东西没被打包。
            ctx.out.warn(&format!(
                "跳过非常规文件 {}（符号链接等暂不支持，见设计文档 05 号 §4.2）",
                entry.path().display()
            ));
        }
    }

    let index = builder.finish()?;
    if index.entries.len() > omy_core::container::RECOMMENDED_MAX_ENTRIES {
        ctx.out.warn(&format!(
            "容器含 {} 个条目，超过建议上限 {}。\n\
             大容器修改任一文件都需重写整包，建议改用 --mode tree。",
            index.entries.len(),
            omy_core::container::RECOMMENDED_MAX_ENTRIES
        ));
    }

    ctx.out.detail(&format!(
        "容器：{n_files} 个文件、{n_dirs} 个目录，载荷 {}",
        human_bytes(payload.len() as u64)
    ));

    Ok((payload, index.encode(), root_name))
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

fn system_time_to_ns(t: std::time::SystemTime) -> Option<i128> {
    match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_nanos()).ok(),
        Err(e) => {
            // 1970 之前的时间
            i128::try_from(e.duration().as_nanos()).ok().map(|v| -v)
        }
    }
}

fn blake2_256(data: &[u8]) -> [u8; 32] {
    use omy_core::util::blake2b_256;
    blake2b_256(data)
}

/// 决定输出路径。
fn resolve_output(input: &Path, a: &Args, is_dir: bool) -> Result<PathBuf> {
    if let Some(o) = &a.output {
        return Ok(o.clone());
    }
    let base = if is_dir {
        input
            .file_name()
            .map(|s| format!("{}.omy", s.to_string_lossy()))
            .unwrap_or_else(|| String::from("folder.omy"))
    } else {
        format!(
            "{}.omy",
            input.file_name().unwrap_or_default().to_string_lossy()
        )
    };
    if let Some(d) = &a.output_dir {
        std::fs::create_dir_all(d)
            .with_context(|| format!("创建输出目录 {} 失败", d.display()))?;
        return Ok(d.join(base));
    }
    Ok(input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(base))
}

/// 校验加密产物可解密后再删除原文件。
///
/// 顺序至关重要：先验证再删除。反之一旦加密有问题，原文件已经没了。
fn verify_then_delete(ctx: &Ctx<'_>, orig: &Path, enc: &[u8], keks: &[Kek]) -> Result<()> {
    if !ctx.out.confirm(
        &format!(
            "{}\n{}",
            t("warn.delete_original"),
            t("prompt.confirm")
        ),
        ctx.assume_yes,
    ) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    // 真解密一次并比对内容哈希，而不是只检查文件存在
    let opened = omy_core::file::open(enc, keks)?;
    let plain = opened.decrypt_all(enc)?;
    let disk = std::fs::read(orig)?;
    if plain != disk {
        bail!(
            "校验失败：解密结果与原文件不一致，已跳过删除 {}",
            orig.display()
        );
    }
    std::fs::remove_file(orig)
        .with_context(|| format!("删除 {} 失败", orig.display()))?;
    ctx.out.info(&format!("已删除原文件 {}", orig.display()));
    Ok(())
}
