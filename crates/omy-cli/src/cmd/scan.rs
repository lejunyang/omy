//! `omy scan`：扫描目录，用给定密码匹配可解锁的文件。
//!
//! 这是「多浏览密码」功能的命令行入口（决策 D-19）。
//!
//! # 扫描结果只在内存
//!
//! 进程退出即消失，**不落盘任何索引数据库**。否则会出现
//! 「文件加密了但索引泄露了文件名」这种自相矛盾的情况。
//!
//! # 为什么要先探测一个文件
//!
//! KEK 由 `Argon2id(密码, vault_salt)` 派生，而 `vault_salt` 存在文件头里。
//! 因此必须先读到一个 `.omy` 文件才能知道该用哪个 salt。
//! 同一 vault 内所有文件共享 salt，故只需探测一次，之后成千上万个文件
//! 复用同一个 KEK——这正是两级 KDF 的意义（实测约 30 万倍差距）。
//!
//! 若目录内存在多个不同 salt 的文件（来自不同 vault），会分组分别派生。

use super::Ctx;
use crate::i18n::t;
use crate::output::human_bytes;
use crate::password::{PasswordSource, read_password};
use anyhow::Result;
use clap::Args as ClapArgs;
use omy_core::scan::{ScanOptions, UnlockOutcome, scan_dirs};
use omy_core::session::SessionKeys;
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

/// `scan` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 待扫描的目录
    pub dirs: Vec<PathBuf>,

    /// 递归子目录（默认开启，用 --no-recursive 关闭）
    #[arg(short, long, default_value_t = true)]
    pub recursive: bool,

    /// 不递归，只看目录本层
    #[arg(long, conflicts_with = "recursive")]
    pub no_recursive: bool,

    /// 最大递归深度
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// 从文件读取密码，可重复以提供多个密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Vec<PathBuf>,

    /// 从环境变量读取密码，可重复（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Vec<String>,

    /// 同时列出未匹配的文件
    #[arg(long)]
    pub show_locked: bool,

    /// 也检查后缀不是 .omy 的文件（伪装文件）
    #[arg(long)]
    pub any_extension: bool,

    /// 单次扫描的文件数上限
    #[arg(long, value_name = "N")]
    pub max_files: Option<usize>,
}

/// 执行 `scan`。
///
/// # Errors
///
/// 目录不可访问、密码读取失败，或目录内没有可用于取 salt 的 omy 文件时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let passwords = collect_passwords(a)?;

    let dirs: Vec<PathBuf> = if a.dirs.is_empty() {
        if ctx.cfg.scan.paths.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            ctx.cfg.scan.paths.iter().map(expand_tilde).collect()
        }
    } else {
        a.dirs.clone()
    };

    let defaults = ScanOptions::default();
    let default_max_files = defaults.max_files;
    let opts = ScanOptions {
        recursive: !a.no_recursive,
        max_depth: a.max_depth.unwrap_or(ctx.cfg.scan.max_depth),
        // 默认只看 .omy：伪装文件可能叫 .jpg，
        // 需要覆盖时用 --any-extension（代价是要读每个文件的头部）
        extension_filter: if a.any_extension {
            None
        } else {
            Some(String::from("omy"))
        },
        max_files: a.max_files.unwrap_or(default_max_files),
        ..defaults
    };

    let t0 = Instant::now();

    // 先收集目录内出现过的所有 (vault_salt, argon2 参数) 组合。
    //
    // salt 与 Argon2 参数**必须成对取自同一个文件头**：
    // 文件可能用任意档位加密，用当前默认档位去派生会得到完全不同的 KEK，
    // 结果是一个都解不开却看不出原因。
    let vaults = collect_vaults(&dirs, &opts);
    // 代价说明：这使目录被遍历两遍（一遍探 vault 配置、一遍实际解锁）。
    // 两遍都只读文件头部前缀，不读载荷，相比一次 Argon2（约 90 ms）
    // 可以忽略；但在网络文件系统上目录遍历本身可能很慢，
    // 届时应改为一次遍历、边收集边惰性派生。
    ctx.out.trace(&format!(
        "首轮探测耗时 {:.2} s（只读文件头，不做密钥运算）",
        t0.elapsed().as_secs_f64()
    ));
    if vaults.is_empty() {
        ctx.out.info(t("ok.no_files"));
        ctx.out.result(
            "",
            &json!({ "files": [], "stats": { "examined": 0, "omy_found": 0, "unlocked": 0 } }),
        );
        return Ok(());
    }
    ctx.out.detail(&format!(
        "发现 {} 个不同的 vault 配置，为每个配置派生 {} 个 KEK",
        vaults.len(),
        passwords.len()
    ));

    let mut session = SessionKeys::new();
    let mut derivations = 0usize;
    for (salt, params) in &vaults {
        for (label, pw) in &passwords {
            // Argon2 在此处运行：vault 配置数 × 密码数 次，与文件数无关。
            // 同一 salt 下成千上万个文件复用同一个 KEK。
            //
            // label 需带上参数指纹：SessionKeys 按 (salt, label) 缓存，
            // 若两个 vault 用了相同 salt 但不同参数（罕见但可能），
            // 不区分会导致后者复用前者的错误 KEK。
            let tagged = format!("{label}#m{}t{}", params.m_kib, params.t);
            if let Err(e) = session.unlock_password(&tagged, salt, pw, *params) {
                ctx.out.detail(&format!("凭据 {label} 派生失败: {e}"));
            } else {
                derivations += 1;
            }
        }
    }
    ctx.out
        .detail(&format!("完成 {derivations} 次 KEK 派生，耗时 {:.2} s", t0.elapsed().as_secs_f64()));

    let result = scan_dirs(&dirs, &session, &opts)?;
    let elapsed = t0.elapsed();

    let mut rows = Vec::new();
    for hit in &result.hits {
        let ok = hit.unlock.is_unlocked();
        if !ok && !a.show_locked {
            continue;
        }
        let (name, plain_size, cred) = match &hit.unlock {
            UnlockOutcome::Unlocked {
                filename,
                plaintext_size,
                credential,
                ..
            } => (filename.clone(), *plaintext_size, Some(credential.clone())),
            UnlockOutcome::Locked => (None, hit.header.plaintext_size, None),
        };

        if !ctx.out.is_json() {
            let mark = if ok { "✓" } else { "·" };
            ctx.out.result(
                &format!(
                    "{mark} {:<36} {:>10}  {}",
                    name.as_deref().unwrap_or("<文件名已加密>"),
                    human_bytes(plain_size),
                    hit.path.display()
                ),
                &json!(null),
            );
        }
        // -vv：逐个命中的细节，含匹配到哪个凭据
        ctx.out.trace(&format!(
            "    密文 {}，块大小 {}，{}",
            human_bytes(hit.file_size),
            human_bytes(u64::from(hit.header.chunk_size)),
            cred.as_deref()
                .map_or_else(|| String::from("未解锁"), |c| format!("匹配凭据 {c}"))
        ));
        rows.push(json!({
            "path": hit.path.display().to_string(),
            "unlocked": ok,
            "filename": name,
            "plaintext_size": plain_size,
            "file_size": hit.file_size,
            "matched_credential": cred,
        }));
    }

    if ctx.out.is_json() {
        ctx.out.result(
            "",
            &json!({
                "files": rows,
                "stats": {
                    "examined": result.stats.files_examined,
                    "omy_found": result.stats.omy_found,
                    "unlocked": result.stats.unlocked,
                    "io_errors": result.stats.io_errors,
                    "malformed": result.stats.malformed,
                },
                "elapsed_ms": elapsed.as_millis(),
            }),
        );
    } else {
        ctx.out.info(&format!(
            "\n{}: {}   {}: {}   {}: {}   {}: {}   {}: {:.2} s",
            t("scan.examined"),
            result.stats.files_examined,
            t("scan.found"),
            result.stats.omy_found,
            t("scan.unlocked"),
            result.stats.unlocked,
            t("scan.locked"),
            result.stats.omy_found.saturating_sub(result.stats.unlocked),
            t("scan.elapsed"),
            elapsed.as_secs_f64()
        ));
        if result.stats.io_errors > 0 || result.stats.malformed > 0 {
            ctx.out.detail(&format!(
                "跳过：{} 个 I/O 错误、{} 个格式异常",
                result.stats.io_errors, result.stats.malformed
            ));
        }
        ctx.out.detail(t("scan.memory_only"));
    }
    Ok(())
}

/// 收集全部密码，每个附一个便于识别的标签。
fn collect_passwords(a: &Args) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = Vec::new();

    for (i, p) in a.password_file.iter().enumerate() {
        let src = PasswordSource {
            file: Some(p.clone()),
            ..PasswordSource::default()
        };
        let pw = read_password(&src, &format!("密码 {}", i + 1), false)?;
        out.push((
            format!("file:{}", p.display()),
            String::from_utf8_lossy(&pw).into_owned(),
        ));
    }
    for v in &a.password_env {
        let src = PasswordSource {
            env: Some(v.clone()),
            ..PasswordSource::default()
        };
        let pw = read_password(&src, v, false)?;
        out.push((format!("env:{v}"), String::from_utf8_lossy(&pw).into_owned()));
    }
    if out.is_empty() {
        let pw = read_password(&PasswordSource::default(), t("prompt.password"), false)?;
        out.push((
            String::from("interactive"),
            String::from_utf8_lossy(&pw).into_owned(),
        ));
    }
    Ok(out)
}

/// 扫一遍目录，收集出现过的所有 `(vault_salt, Argon2 参数)` 组合。
///
/// 只读每个文件的头部前缀，不读载荷，也不做任何密钥运算。
///
/// 返回的是组合而非单独的 salt：KEK = Argon2id(密码, salt, 参数)，
/// 三者缺一不可，分开取会配错。
fn collect_vaults(
    dirs: &[PathBuf],
    opts: &ScanOptions,
) -> Vec<([u8; 16], omy_core::Argon2Params)> {
    // BTreeSet 需要 Ord，Argon2Params 未必实现，故用元组键手工去重
    let mut seen: BTreeSet<([u8; 16], u32, u32, u32)> = BTreeSet::new();
    let mut out = Vec::new();
    let empty = SessionKeys::new();
    // 用空 session 扫描：此时不会尝试解锁，但 hits 里含解析出的 header
    if let Ok(r) = scan_dirs(dirs, &empty, opts) {
        for hit in &r.hits {
            let h = &hit.header;
            let key = (h.vault_salt, h.argon2_m_kib, h.argon2_t, h.argon2_p);
            if seen.insert(key) {
                out.push((h.vault_salt, h.argon2_params()));
            }
        }
    }
    out
}

/// 展开路径开头的 `~`。
fn expand_tilde(s: &String) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = dirs::home_dir() {
            return h.join(rest);
        }
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expansion() {
        let p = expand_tilde(&String::from("~/Documents"));
        assert!(!p.to_string_lossy().starts_with("~/"));
        // 无 ~ 的路径原样返回
        assert_eq!(
            expand_tilde(&String::from("/abs/path")),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_tilde(&String::from("relative")),
            PathBuf::from("relative")
        );
    }
}
