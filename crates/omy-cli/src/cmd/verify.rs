//! `omy verify`：验证完整性。
//!
//! 与 `decrypt --verify-only` 的区别：`verify` 不需要密码也能做
//! **部分**校验（头部 magic、版本、结构自洽、分片 CRC），
//! 提供密码后才能做 AEAD 与内容哈希校验。

use super::Ctx;
use crate::i18n::t;
use crate::output::human_bytes;
use crate::password::{PasswordSource, read_password};
use anyhow::Result;
use clap::Args as ClapArgs;
use omy_core::crypto::Kek;
use serde_json::json;
use std::path::PathBuf;

/// `verify` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 待验证的文件
    #[arg(required = true)]
    pub files: Vec<PathBuf>,

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

/// 执行 `verify`。
///
/// # Errors
///
/// 任一文件校验失败时返回错误（并已先输出其余文件的结果）。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let with_password = !src.is_interactive() || std::io::IsTerminal::is_terminal(&std::io::stdin());
    let pw = if with_password {
        Some(read_password(&src, t("prompt.password"), false)?)
    } else {
        None
    };

    let mut results = Vec::new();
    let mut failed = 0usize;

    for f in &a.files {
        let data = match std::fs::read(f) {
            Ok(d) => d,
            Err(e) => {
                ctx.out.warn(&format!("{}: 读取失败 {e}", f.display()));
                failed += 1;
                results.push(json!({
                    "file": f.display().to_string(),
                    "ok": false,
                    "stage": "read",
                    "error": e.to_string(),
                }));
                continue;
            }
        };

        // 阶段 1：结构校验，无需密码
        let h = match omy_core::file::peek_header(&data) {
            Ok(h) => h,
            Err(e) => {
                ctx.out.warn(&format!("{}: 结构校验失败 {e}", f.display()));
                failed += 1;
                results.push(json!({
                    "file": f.display().to_string(),
                    "ok": false,
                    "stage": "header",
                    "error": e.to_string(),
                }));
                continue;
            }
        };

        // 阶段 2：完整解密并校验内容哈希，需要密码
        if let Some(pw) = &pw {
            let kek = Kek::from_password(pw, &h.vault_salt, h.argon2_params())?;
            match omy_core::file::open(&data, &[kek]).and_then(|o| o.decrypt_all(&data)) {
                Ok(plain) => {
                    ctx.out.success(&format!(
                        "{} {}（{}，{} 块）",
                        t("ok.verified"),
                        f.display(),
                        human_bytes(plain.len() as u64),
                        h.n_chunks()
                    ));
                    results.push(json!({
                        "file": f.display().to_string(),
                        "ok": true,
                        "stage": "full",
                        "plaintext_size": plain.len(),
                        "chunks": h.n_chunks(),
                    }));
                }
                Err(e) => {
                    ctx.out.warn(&format!("{}: {e}", f.display()));
                    failed += 1;
                    results.push(json!({
                        "file": f.display().to_string(),
                        "ok": false,
                        "stage": "decrypt",
                        "error": e.to_string(),
                        "code": e.code(),
                    }));
                }
            }
        } else {
            ctx.out.info(&format!(
                "{}: 结构有效（未提供密码，仅做结构校验）",
                f.display()
            ));
            results.push(json!({
                "file": f.display().to_string(),
                "ok": true,
                "stage": "header",
            }));
        }
    }

    ctx.out.result(
        "",
        &json!({
            "results": results,
            "total": a.files.len(),
            "failed": failed,
        }),
    );

    if failed > 0 {
        // 用 core 的错误类型以取得正确的退出码
        return Err(anyhow::Error::new(omy_core::Error::ContentHashMismatch)
            .context(format!("{failed} 个文件校验失败")));
    }
    Ok(())
}
