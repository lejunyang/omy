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

use super::{Cipher, Ctx, KdfProfile, NameMode, OriginalAction};
use crate::i18n::t;
use crate::output::{human_bytes, parse_size};
use crate::progress::Progress;
use crate::password::{PasswordSource, read_password};
use anyhow::{Context as _, Result, bail};
use clap::Args as ClapArgs;
use omy_core::pack::SkipReason;
use omy_core::crypto::Kek;
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt_with_progress};
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

    /// 加入已有库：复用该文件（或该目录下任一文件）的 vault salt 与 KDF 参数
    ///
    /// 不指定时会生成新的随机 salt，产出的文件**自成一库**——
    /// 即使用同一个密码，也需要为它单独跑一次 Argon2 才能解开。
    /// 分批加密到同一个文件夹时必须指定这个参数，否则每批都是独立的库。
    #[arg(long, value_name = "PATH")]
    pub vault: Option<PathBuf>,

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

    /// 加密后如何处理原文件：keep | trash | delete
    ///
    /// 不指定时取配置文件的 `defaults.original_action`（默认 keep）。
    /// `trash` 与 `delete` 都需要 `--yes` 或交互确认。
    #[arg(long, value_enum, value_name = "ACTION")]
    pub original: Option<OriginalAction>,

    /// 缩略图模式：auto | none
    ///
    /// 契约见 `docs/research/09-cli-design.md`。`auto` 对图片缩放原图、
    /// 对视频抽帧；`none` 完全不生成。
    #[arg(long, value_name = "MODE", default_value = "auto")]
    pub thumbnail: String,

    /// 视频取帧时间点，如 `00:01:23` 或 `83` 或 `83.5`
    ///
    /// 只对视频有效。不指定则自动取时长 10% 处（避开片头黑帧）。
    #[arg(long, value_name = "T")]
    pub thumbnail_frame: Option<String>,

    /// 不写入媒体元信息（`TLV_MEDIA_META`）
    ///
    /// 默认写入。它让列表页无需解密探测就能判断能否内嵌播放。
    #[arg(long)]
    pub no_media_meta: bool,

    /// 不缓存 MP4 的 moov box（`TLV_MOOV_CACHE`）
    ///
    /// 默认缓存。缓存后起播可省掉两次 seek，局域网播放与缺片播放收益最大。
    #[arg(long)]
    pub no_moov_cache: bool,
}

/// 把原文件移到系统回收站。
///
/// 单独抽成一个函数是因为两处处置逻辑（单文件与容器）都要做同一件事，
/// 而它还带一个平台分支——两处各写一遍的话，将来只会改一处。
/// **改这里时注意两个调用点都受影响。**
///
/// # 为什么 Android 要单独一支
///
/// `trash` 5.2 在 Android 上不提供任何实现，所以那里连依赖都不声明
/// （见 Cargo.toml 的 `[target.'cfg(not(target_os = "android"))'.dependencies]`）。
/// 此时必须**明确报不支持**，绝不能退回 `remove_file`：用户选「回收站」
/// 要的就是能后悔，悄悄改成永久删除就是数据丢失。
///
/// # Errors
///
/// 移动失败，或当前平台没有回收站。
#[cfg(not(target_os = "android"))]
fn move_original_to_trash(orig: &Path) -> Result<()> {
    trash::delete(orig).map_err(|e| anyhow::anyhow!("移到回收站失败 {}: {e}", orig.display()))
}

/// Android 没有回收站，如实报错而不是降级成永久删除。
///
/// # Errors
///
/// 恒为错误。
#[cfg(target_os = "android")]
fn move_original_to_trash(orig: &Path) -> Result<()> {
    bail!(
        "当前平台没有回收站，无法移动 {}；请改用 --original keep 或 --original delete",
        orig.display()
    )
}

/// 解析取帧时间点。
///
/// 接受三种写法，因为用户习惯不一：
///
/// - `83` / `83.5`：直接是秒
/// - `01:23` / `01:23.5`：分:秒
/// - `00:01:23`：时:分:秒
///
/// # Errors
///
/// 格式无法识别、含非数字字段或数值为负时返回错误。
fn parse_timecode(s: &str) -> Result<f64> {
    let t = s.trim();
    if t.is_empty() {
        bail!("取帧时间点不能为空");
    }
    let parts: Vec<&str> = t.split(':').collect();
    // 逐段解析，任何一段非法都要报错而不是当成 0——
    // 静默取 0 秒会让用户以为设置生效了，实际拿到的是首帧
    let nums: Result<Vec<f64>> = parts
        .iter()
        .map(|p| {
            p.parse::<f64>()
                .map_err(|_| anyhow::anyhow!("时间点 `{s}` 含无法解析的字段 `{p}`"))
        })
        .collect();
    let nums = nums?;

    let secs = match nums.as_slice() {
        [s] => *s,
        [m, s] => m * 60.0 + s,
        [h, m, s] => h * 3600.0 + m * 60.0 + s,
        _ => bail!("时间点 `{s}` 格式不支持，请用 秒 / 分:秒 / 时:分:秒"),
    };
    if !secs.is_finite() || secs < 0.0 {
        bail!("时间点 `{s}` 必须是非负数");
    }
    Ok(secs)
}

/// 由命令行参数构造媒体准备选项。
///
/// # 为什么 `auto` 用 `VideoAuto` 而非按扩展名分派
///
/// `omy_media::prepare` 内部会先尝试图片解码、再走 ffprobe，
/// 自己就能分辨类型。按扩展名猜反而会错——用户完全可能把 PNG
/// 命名成 `.dat`。所以这里只表达"要自动处理"，具体分派交给下层。
///
/// # Errors
///
/// `--thumbnail` 取值非法或 `--thumbnail-frame` 无法解析时返回错误。
fn build_prepare_options(a: &Args) -> Result<omy_media::PrepareOptions> {
    let thumb = match a.thumbnail.trim().to_ascii_lowercase().as_str() {
        "none" => omy_media::ThumbSource::None,
        "auto" => match &a.thumbnail_frame {
            // 用户指定了帧：尊重它（需求确认项 B.2）
            Some(tc) => omy_media::ThumbSource::VideoAt(parse_timecode(tc)?),
            // 交给 prepare 按实际内容分派。
            //
            // 早先这里写的是 `VideoAuto`，于是**图片一律拿不到缩略图**：
            // 静态图会被当成单帧视频送去抽帧，最后在 webp 编码器里报
            // Cannot allocate memory。这里看不出问题，因为此刻还没读文件内容，
            // 无从知道它是图还是视频——所以分派只能放在 prepare 里。
            None => omy_media::ThumbSource::Auto,
        },
        other => bail!(
            "--thumbnail 只接受 auto | none，收到 `{other}`\n\
             （指定图片路径作为封面的功能尚未实现）"
        ),
    };

    // 指定了 --thumbnail-frame 却又 --thumbnail none：明确报错。
    // 静默忽略会让用户以为设置生效了。
    if a.thumbnail_frame.is_some() && thumb == omy_media::ThumbSource::None {
        bail!("--thumbnail-frame 与 --thumbnail none 冲突：不生成缩略图时指定帧无意义");
    }

    Ok(omy_media::PrepareOptions {
        media_meta: !a.no_media_meta,
        moov_cache: !a.no_moov_cache,
        thumbnail: thumb,
        ..omy_media::PrepareOptions::default()
    })
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
    // 与 name_mode 同构：命令行优先，否则回落到配置文件。
    // 解析放在读密码之前——配置里写错值应当立刻报错，
    // 而不是让用户白输一遍密码再失败
    let original = match a.original {
        Some(m) => m,
        None => OriginalAction::from_name(&ctx.cfg.defaults.original_action)?,
    };
    let compress = a.compress || ctx.cfg.compress.enabled;
    let level = a.compress_level.unwrap_or(ctx.cfg.compress.level);
    if compress && !(1..=19).contains(&level) {
        bail!("压缩级别必须在 1–19 之间，当前为 {level}");
    }

    // 媒体准备选项。在循环外算一次：解析时间点可能报错，
    // 应当在读密码**之前**就失败——让用户白输一遍密码再报参数错误很糟。
    let prep_opts = build_prepare_options(a)?;

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

    // vault salt：同一次调用内共享，使 KEK 可复用。
    //
    // 指定 --vault 时改为沿用已有库的 salt 与 KDF 参数。
    // 这两者必须一起沿用：salt 相同但参数不同，派生出的仍是
    // 不同的 KEK，文件照样打不开——而且症状是「密码正确却解不开」，
    // 极难排查。
    let (vault_salt, params) = match &a.vault {
        Some(p) => {
            let (salt, existing) = read_vault_params(p)?;
            if a.kdf_profile.is_some() && existing != profile.params() {
                // 用户同时指定了 --vault 和 --kdf-profile 且两者冲突。
                // 不能静默采用其中之一：沿用已有参数会让 --kdf-profile
                // 失效（用户以为改了强度），采用新参数则文件进不了那个库。
                bail!(
                    "--vault 指定的库使用 m={} t={} p={}，与 --kdf-profile 的 m={} t={} p={} 冲突。\n\
                     加入已有库时不能改 KDF 参数——同一库内参数必须一致，否则同一密码会派生出不同的 KEK。\n\
                     去掉 --kdf-profile 即可沿用该库的参数。",
                    existing.m_kib, existing.t, existing.p,
                    profile.params().m_kib, profile.params().t, profile.params().p
                );
            }
            ctx.out.detail(&format!(
                "加入已有库：沿用 {} 的 vault salt 与 KDF 参数",
                p.display()
            ));
            (salt, existing)
        }
        None => (omy_core::util::random_16(), profile.params()),
    };

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

        // 树形模式走完全不同的流程：它自己往磁盘写很多个文件，
        // 没有单一的「明文载荷 + 密文字节」可以交给下面那条流水线。
        // 硬塞进去的话，进度条、媒体探测、JSON 契约全都要加分支判断，
        // 而它们对树形模式没有一个是适用的
        if meta.is_dir() && a.mode == DirMode::Tree {
            let one = encrypt_as_tree(ctx, p, a, &keks, &vault_salt, &opts_base(
                name_mode, compress, level, chunk_u32, cipher, params,
            ), original)?;
            total_in += one.0;
            total_out += one.1;
            results.push(one.2);
            continue;
        }

        let (plaintext, folder_index, name) = if meta.is_dir() {
            match a.mode {
                DirMode::Container => {
                    let (p, idx, n) = build_container(ctx, p)?;
                    (p, Some(idx), n)
                }
                // 上面已经 continue 掉了，这里不可能到达。
                // 写 unreachable 而不是重复一遍逻辑：两处实现迟早分歧
                DirMode::Tree => unreachable!("tree 模式已在上面处理"),
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

        // 媒体准备：探测并产出 media_meta / moov_cache / thumbnail。
        //
        // 只对**单文件**做：容器模式的载荷是多个文件拼接，
        // 整体探测毫无意义（会把第一个文件的头当成整体格式）。
        //
        // prepare 不返回 Err——媒体附加信息全是优化项，
        // 任何一步失败都只降级并记警告，绝不阻断加密。
        let prepared = if folder_index.is_some() {
            omy_media::Prepared::default()
        } else {
            omy_media::prepare(&plaintext, &prep_opts)
        };
        // 警告要让用户看见，否则"为什么没有缩略图"无从排查
        for w in &prepared.warnings {
            ctx.out.warn(w);
        }

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
            thumbnail: prepared.thumbnail.clone(),
            media_meta: prepared.media_meta.clone(),
            moov_cache: prepared.moov_cache.clone(),
            folder_index: folder_index.clone(),
        };

        // 进度条按**明文**字节走。回调在加密线程内同步调用，
        // 所以里面只做一次 set_position——indicatif 自己会限流重绘，
        // 在这里再加判断反而会让进度条卡顿。
        let bar = Progress::new(
            ctx.out.wants_progress(),
            plaintext.len() as u64,
            &format!("加密 {}", name),
        );
        let enc = {
            let mut cb = |done: u64, _total: u64| bar.set(done);
            encrypt_with_progress(
                &plaintext,
                &keks,
                &vault_salt,
                &opts,
                &RandomMaterial::generate(),
                Some(&mut cb),
            )?
        };
        // 必须在打印结果之前清掉，否则进度条残留会和成功信息挤在一行
        bar.finish();

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

        // 处置原文件前先校验加密产物可解密——绝不能先删后验
        if original.needs_confirm() {
            verify_then_handle(ctx, p, &enc.bytes, &keks, original)?;
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

/// 构造与单文件路径一致的加密选项。
///
/// 抽出来是为了让树形模式与容器模式**用同一份选项**：算法、分块、KDF 参数
/// 若两条路径各写一遍，哪天改了 cipher 默认值只改一处，同一个命令的两种
/// 模式会产出不同格式的文件。
///
/// `filename` 与媒体附加信息不在这里——前者由每个文件自己决定，
/// 后者只对单文件有意义。
fn opts_base(
    name_mode: NameMode,
    compress: bool,
    level: i32,
    chunk_u32: u32,
    cipher: Cipher,
    params: omy_core::crypto::Argon2Params,
) -> EncryptOptions {
    EncryptOptions {
        filename: None,
        preserve_extension: name_mode == NameMode::KeepExt,
        compress,
        zstd_level: level,
        chunk_size: chunk_u32,
        cipher: cipher.id(),
        argon2: params,
        write_content_hash: true,
        ..EncryptOptions::default()
    }
}

/// 树形模式加密一个目录。
///
/// 返回 `(明文字节数, 密文字节数, JSON 结果)`。
fn encrypt_as_tree(
    ctx: &Ctx<'_>,
    root: &Path,
    a: &Args,
    keks: &[Kek],
    vault_salt: &[u8; 16],
    opts: &EncryptOptions,
    original: OriginalAction,
) -> Result<(u64, u64, serde_json::Value)> {
    // 泄露的元数据必须在**动手之前**告知，不是事后报告。
    // 用户看到「已加密完成，顺便说一下别人能数出你有多少文件」时，
    // 原目录可能已经删了
    ctx.out.warn(omy_core::tree::TreeReport::leak_notice());

    // 输出父目录：--output-dir 指定则用它，否则与原目录同级
    let out_parent = if let Some(d) = &a.output_dir {
        std::fs::create_dir_all(d)
            .with_context(|| format!("创建输出目录 {} 失败", d.display()))?;
        d.clone()
    } else if let Some(o) = &a.output {
        // -o 在树形模式下语义是「输出根目录的父目录」而不是文件名——
        // 树形模式产出的是目录，不是单个文件。明确说清而不是默默改语义
        std::fs::create_dir_all(o)
            .with_context(|| format!("创建输出目录 {} 失败", o.display()))?;
        o.clone()
    } else {
        root.parent().unwrap_or_else(|| Path::new(".")).to_path_buf()
    };

    let mut total_in = 0u64;
    let mut on_file = |name: &str, size: u64| {
        ctx.out.trace(&format!("  文件 {} （{}）", name, human_bytes(size)));
    };

    // 逐个文件生成媒体附加信息。选项由 `a` 现算，而不是从外面多传一个
    // 参数进来——那会让本函数的参数数量超出 clippy 上限，而加 allow
    // 等于把「参数太多」这个真实信号关掉。
    let prep_opts = build_prepare_options(a)?;
    // 警告只在 trace 级别输出：一棵树里非媒体文件是多数，逐个报
    // 「不是媒体」会把有用输出冲掉；而真正的探测故障仍然要能查到，
    // 所以不是直接丢弃。
    let mut on_media = |data: &[u8], name: &str| {
        let p = omy_media::prepare(data, &prep_opts);
        for w in &p.warnings {
            ctx.out.trace(&format!("  媒体 {name}：{w}"));
        }
        (p.thumbnail, p.media_meta, p.moov_cache)
    };

    let rep = omy_core::tree::encrypt_tree_with_media(
        root,
        &out_parent,
        keks,
        vault_salt,
        opts,
        Some(&mut on_file),
        Some(&mut on_media),
    )
    .with_context(|| format!("树形加密 {} 失败", root.display()))?;

    // 跳过的条目逐条报出来，与容器模式同样处理：静默丢弃会让用户
    // 以为整个目录都加密了
    for sk in &rep.skipped {
        ctx.out.warn(&format!("跳过 {}：{}", sk.path, skip_why(sk.reason)));
    }

    // 超长目录名有运维含义：这些目录一旦丢了边车文件就认不出原名
    if rep.long_names > 0 {
        ctx.out.warn(&format!(
            "{} 个目录名过长，已截断并在目录内写入名称文件。\n\
             删掉那个文件会导致目录名无法还原；如需避免，请缩短目录名。",
            rep.long_names
        ));
    }

    // 统计明文与密文体积。逐个文件加密，密文总量要遍历产物才知道
    for_each_file(root, &mut |sz| total_in = total_in.saturating_add(sz));
    let mut total_out = 0u64;
    for_each_file(&rep.root, &mut |sz| total_out = total_out.saturating_add(sz));
    let count = rep.files;

    ctx.out.success(&format!(
        "{} {} → {}（{} 个文件、{} 个目录，{} → {}）",
        t("ok.encrypted"),
        root.display(),
        rep.root.display(),
        count,
        rep.dirs,
        human_bytes(total_in),
        human_bytes(total_out)
    ));

    // 原文件处置：树形模式无法用「解密一次比对字节」那套校验——
    // 那是针对单个密文文件的。这里改为逐个文件校验整棵树
    if original.needs_confirm() {
        verify_tree_then_handle(ctx, root, &rep.root, keks, vault_salt, opts.cipher, original)?;
    }

    Ok((
        total_in,
        total_out,
        json!({
            "input": root.display().to_string(),
            "output": rep.root.display().to_string(),
            "mode": "tree",
            "files": count,
            "dirs": rep.dirs,
            "long_names": rep.long_names,
            "plaintext_size": total_in,
            "encrypted_size": total_out,
        }),
    ))
}

/// 树形模式的原件处置：先把整棵树解开比对，再动原件。
///
/// 不能复用 [`verify_then_handle`]：那个函数解一份密文字节与一个原文件比对，
/// 而树形模式是多对多。**校验必须逐个文件比对内容**——只数文件个数的话，
/// 内容串位（最难查的那种缺陷）会被放过，而此时原目录已经删了。
fn verify_tree_then_handle(
    ctx: &Ctx<'_>,
    orig: &Path,
    enc_root: &Path,
    keks: &[Kek],
    vault_salt: &[u8; 16],
    cipher: omy_core::crypto::CipherId,
    action: OriginalAction,
) -> Result<()> {
    let warn = match action {
        OriginalAction::Trash => t("warn.trash_original"),
        _ => t("warn.delete_original"),
    };
    if !ctx.out.confirm(&format!("{warn}\n{}", t("prompt.confirm")), ctx.assume_yes) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    // 解到临时目录里比对。用 target 旁边的临时目录而不是系统 temp：
    // 大目录可能几十 GB，系统 temp 常在小分区上
    let tmp = enc_root.with_file_name(format!(
        ".omy-verify-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)
        .with_context(|| format!("创建校验目录 {} 失败", tmp.display()))?;

    let verify = || -> Result<()> {
        let rep = omy_core::tree::decrypt_tree(enc_root, &tmp, keks, vault_salt, cipher, None)?;
        let mut mismatch = Vec::new();
        compare_trees(orig, &rep.root, &mut mismatch);
        if !mismatch.is_empty() {
            bail!(
                "校验失败：{} 项与原目录不一致（首个：{}），已跳过处置",
                mismatch.len(),
                mismatch.first().map_or("?", String::as_str)
            );
        }
        Ok(())
    };
    let outcome = verify();
    // 无论成败都要清掉临时明文——它是完整的明文副本，留着是安全问题
    let _ = std::fs::remove_dir_all(&tmp);
    outcome?;

    match action {
        OriginalAction::Trash => {
            move_original_to_trash(orig)?;
            ctx.out.info(&format!("已移到回收站 {}", orig.display()));
        }
        OriginalAction::Delete => {
            std::fs::remove_dir_all(orig)
                .with_context(|| format!("删除 {} 失败", orig.display()))?;
            ctx.out.info(&format!("已删除原目录 {}", orig.display()));
        }
        OriginalAction::Keep => {}
    }
    Ok(())
}

/// 递归比对两棵目录树的文件内容，把不一致的相对路径记进 `out`。
fn compare_trees(a: &Path, b: &Path, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(a) else {
        out.push(a.display().to_string());
        return;
    };
    for e in rd.flatten() {
        let pa = e.path();
        let pb = b.join(e.file_name());
        if pa.is_dir() {
            if pb.is_dir() {
                compare_trees(&pa, &pb, out);
            } else {
                out.push(pa.display().to_string());
            }
        } else if pa.is_file() {
            match (std::fs::read(&pa), std::fs::read(&pb)) {
                (Ok(x), Ok(y)) if x == y => {}
                _ => out.push(pa.display().to_string()),
            }
        }
    }
}

/// 遍历目录下所有文件，把每个文件的大小交给回调。
fn for_each_file(root: &Path, cb: &mut dyn FnMut(u64)) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            for_each_file(&p, cb);
        } else if let Ok(md) = p.metadata() {
            cb(md.len());
        }
    }
}

/// 跳过原因的中文说明。
///
/// 与容器模式共用：同一个原因在两种模式下说法不一致，用户会以为是两回事。
fn skip_why(reason: SkipReason) -> &'static str {
    match reason {
        // 只有树形模式会报这个了：容器模式把链接存进索引，不再跳过。
        // 说清是「这个模式」而不是「omy」不支持，否则用户不知道换个
        // 模式就能保留。
        SkipReason::Symlink => "树形模式存不下符号链接，改用 --mode container 可保留",
        SkipReason::NotRegular => "非常规文件（设备/管道/套接字）",
        SkipReason::Unreadable => "读取失败（权限不足，或文件正被占用）",
        SkipReason::TooDeep => "目录层级超过上限",
    }
}

/// 把目录打包成容器的明文载荷。
///
/// 遍历逻辑本身在 `omy_core::pack`——GUI 也要做文件夹加密，
/// 两个入口必须产出**完全一样**的容器。这里只负责把结果讲给用户听。
///
/// 返回 `(载荷字节, 索引编码, 根目录名)`。
fn build_container(ctx: &Ctx<'_>, root: &Path) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let mut on_file = |name: &str, size: u64| {
        ctx.out.trace(&format!("  文件 {} （{}）", name, human_bytes(size)));
    };

    let packed = omy_core::pack::pack_folder(root, Some(&mut on_file))
        .with_context(|| format!("打包 {} 失败", root.display()))?;

    // 跳过的条目必须逐条报出来。静默丢弃最坏的后果是：用户以为整个
    // 目录都加密了，删掉原件，解密时才发现少东西
    for sk in &packed.skipped {
        ctx.out.warn(&format!("跳过 {}：{}", sk.path, skip_why(sk.reason)));
    }

    if packed.index.entries.len() > omy_core::container::RECOMMENDED_MAX_ENTRIES {
        ctx.out.warn(&format!(
            "容器含 {} 个条目，超过建议上限 {}。\n\
             大容器修改任一文件都需重写整包，建议改用 --mode tree。",
            packed.index.entries.len(),
            omy_core::container::RECOMMENDED_MAX_ENTRIES
        ));
    }

    // 链接数只在真有链接时才说。绝大多数目录不含链接，无条件加一句
    // 「0 个符号链接」只是噪音，还会让人以为这里有什么要注意的
    let links = if packed.link_count > 0 {
        format!("、{} 个符号链接", packed.link_count)
    } else {
        String::new()
    };
    ctx.out.detail(&format!(
        "容器：{} 个文件、{} 个目录{}，载荷 {}",
        packed.file_count,
        packed.dir_count,
        links,
        human_bytes(packed.payload.len() as u64)
    ));

    Ok((packed.payload, packed.index.encode(), packed.root_name))
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

/// 校验加密产物可解密后再处置原文件。
///
/// 顺序至关重要：先验证再动原件。反之一旦加密有问题，原文件已经没了。
/// 回收站虽然可还原，也一样先验——不能指望用户去回收站里捞。
fn verify_then_handle(
    ctx: &Ctx<'_>,
    orig: &Path,
    enc: &[u8],
    keks: &[Kek],
    action: OriginalAction,
) -> Result<()> {
    // 两种处置的后果差别很大，提示语必须分开：
    // 回收站可还原，永久删除不可撤销，用同一句话会误导用户
    let warn = match action {
        OriginalAction::Trash => t("warn.trash_original"),
        _ => t("warn.delete_original"),
    };
    if !ctx.out.confirm(&format!("{warn}\n{}", t("prompt.confirm")), ctx.assume_yes) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    // 真解密一次并比对内容，而不是只检查文件存在
    let opened = omy_core::file::open(enc, keks)?;
    let plain = opened.decrypt_all(enc)?;
    let disk = std::fs::read(orig)?;
    if plain != disk {
        bail!(
            "校验失败：解密结果与原文件不一致，已跳过处置 {}",
            orig.display()
        );
    }

    match action {
        // 回收站对文件和目录是同一个入口，不必像永久删除那样自己分流
        OriginalAction::Trash => {
            move_original_to_trash(orig)?;
            ctx.out.info(&format!("已移到回收站 {}", orig.display()));
        }        // remove_file 对目录一律失败，而 container 模式加密的正是目录：
        // 不分流会让「加密文件夹后删原件」静默失效
        OriginalAction::Delete => {
            if orig.is_dir() {
                std::fs::remove_dir_all(orig)
            } else {
                std::fs::remove_file(orig)
            }
            .with_context(|| format!("删除 {} 失败", orig.display()))?;
            ctx.out.info(&format!("已删除原文件 {}", orig.display()));
        }
        OriginalAction::Keep => {}
    }
    Ok(())
}

/// 从已有的 omy 文件读出 vault salt 与 KDF 参数，用于 `--vault`。
///
/// 传目录时取其中第一个可解析的文件——同一库内这些参数本就一致，
/// 取哪个都一样。传文件时直接读它。
///
/// 只读文件开头一小段：头部就在最前面，为了取 16 字节的盐
/// 去读一个 800 MB 的视频没有道理。
fn read_vault_params(p: &Path) -> Result<([u8; 16], omy_core::crypto::Argon2Params)> {
    if p.is_file() {
        let bytes = read_header_prefix(p)?;
        let h = omy_core::file::peek_header(&bytes)
            .with_context(|| format!("{} 不是可识别的 omy 文件", p.display()))?;
        return Ok((h.vault_salt, h.argon2_params()));
    }

    if !p.is_dir() {
        bail!("{} 不存在", p.display());
    }

    // 目录：收集其中出现的所有不同的 vault，而不是取第一个。
    //
    // 一个文件夹里混着多个库是常态（分批加密、从别处拷进来的文件）。
    // 若静默取第一个，用户以为加入了 A 库、实际进了 B 库，
    // 而且直到换台机器用另一个密码打不开时才会发现。
    // 这种错误必须在加密前就拦下来。
    let mut found: Vec<([u8; 16], omy_core::crypto::Argon2Params)> = Vec::new();
    let mut sample: Vec<PathBuf> = Vec::new();

    let mut entries: Vec<PathBuf> = std::fs::read_dir(p)
        .with_context(|| format!("无法读取目录 {}", p.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|q| q.is_file())
        .collect();
    entries.sort();

    for c in &entries {
        let Ok(bytes) = read_header_prefix(c) else {
            continue;
        };
        let Ok(h) = omy_core::file::peek_header(&bytes) else {
            continue;
        };
        if !found.iter().any(|(s, _)| *s == h.vault_salt) {
            found.push((h.vault_salt, h.argon2_params()));
            sample.push(c.clone());
        }
    }

    match found.len() {
        0 => bail!(
            "{} 中没有可识别的 omy 文件，无法取得 vault salt",
            p.display()
        ),
        1 => Ok(found.swap_remove(0)),
        n => {
            // 列出每个库的一个代表文件，用户据此改成指定具体文件
            let list = sample
                .iter()
                .take(4)
                .map(|q| format!("  --vault {}", q.display()))
                .collect::<Vec<_>>()
                .join("\n");
            bail!(
                "{} 中存在 {n} 个不同的库，无法确定要加入哪一个。\n\
                 请改为指定具体文件，例如：\n{list}",
                p.display()
            )
        }
    }
}
/// 只读文件开头的头部区域。
fn read_header_prefix(p: &Path) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(p).with_context(|| format!("无法打开 {}", p.display()))?;
    // 4 KiB 足够覆盖固定头与 slot 区；TLV 区可能更长，
    // 但 peek_header 只需要固定头
    let mut buf = vec![0u8; 4096];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}
