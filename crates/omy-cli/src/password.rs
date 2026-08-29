//! 密码输入通道。
//!
//! 对应设计文档 [09 号 §4](../../docs/research/09-cli-design.md) 与旁路清单 L12。
//!
//! # 为什么没有 `--password`
//!
//! 命令行参数对同机其它用户可见（`ps aux`、`/proc/*/cmdline`、Windows 的
//! WMI 与任务管理器命令行列），且会写入 shell 历史文件。这是**最高优先级**
//! 的旁路风险，因此本工具**不提供**明文密码参数，而不是提供后附加警告。
//!
//! 用户误用 `--password` 时给出明确解释与替代方案，见 [`explain_password_flag`]。

use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

/// 密码来源。互斥，最多指定一种。
#[derive(Debug, Clone, Default)]
pub struct PasswordSource {
    /// 从环境变量读取。传变量**名**而非值。
    pub env: Option<String>,
    /// 从文件读取首行。
    pub file: Option<PathBuf>,
    /// 从标准输入读取。
    pub stdin: bool,
}

impl PasswordSource {
    /// 是否未指定任何非交互通道。
    #[must_use]
    pub const fn is_interactive(&self) -> bool {
        self.env.is_none() && self.file.is_none() && !self.stdin
    }

    /// 校验只指定了一种通道。
    ///
    /// # Errors
    ///
    /// 指定了多种通道时返回错误——静默取其一会让用户误以为用的是另一个。
    pub fn validate(&self) -> Result<()> {
        let n = usize::from(self.env.is_some())
            + usize::from(self.file.is_some())
            + usize::from(self.stdin);
        if n > 1 {
            bail!(
                "只能指定一种密码来源，当前指定了 {n} 种\n\
                 \n  --password-stdin / --password-file / --password-env 互斥"
            );
        }
        Ok(())
    }
}

/// 读取密码。
///
/// `prompt` 用于交互式场景的提示语；`confirm` 为真时要求重复输入以防打错，
/// 仅在**创建**密码时使用（加密、改密码），解密时不应确认。
///
/// # Errors
///
/// 通道不可用、文件读取失败、环境变量未设置、输入为空、两次确认不一致时返回错误。
pub fn read_password(src: &PasswordSource, prompt: &str, confirm: bool) -> Result<Vec<u8>> {
    src.validate()?;

    if let Some(name) = &src.env {
        let v = std::env::var(name).with_context(|| {
            format!("环境变量 {name} 未设置或含非 UTF-8 字节")
        })?;
        if v.is_empty() {
            bail!("环境变量 {name} 为空");
        }
        return Ok(v.into_bytes());
    }

    if let Some(path) = &src.file {
        return read_password_file(path);
    }

    if src.stdin {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("从标准输入读取密码失败")?;
        let pw = first_line(&buf);
        if pw.is_empty() {
            bail!("标准输入未提供密码");
        }
        return Ok(pw.as_bytes().to_vec());
    }

    // 交互式：必须有终端。管道场景下应显式用 --password-stdin，
    // 否则脚本会挂在这里等输入而看不出原因。
    if !std::io::stdin().is_terminal() {
        bail!(
            "当前没有可交互的终端，无法提示输入密码\n\
             \n  管道或脚本场景请显式指定：\n\
             \n    --password-stdin        从管道读取\
             \n    --password-file <路径>  从文件读取\
             \n    --password-env <变量名> 从环境变量读取"
        );
    }

    let pw = rpassword::prompt_password(format!("{prompt}: "))
        .context("读取密码失败")?;
    if pw.is_empty() {
        bail!("密码不能为空");
    }
    if confirm {
        let again = rpassword::prompt_password("再次输入以确认: ")
            .context("读取确认密码失败")?;
        if again != pw {
            bail!("两次输入的密码不一致");
        }
    }
    Ok(pw.into_bytes())
}

/// 从文件读取密码首行。
fn read_password_file(path: &Path) -> Result<Vec<u8>> {
    let data = std::fs::read(path)
        .with_context(|| format!("读取密码文件 {} 失败", path.display()))?;
    let text = String::from_utf8(data)
        .with_context(|| format!("密码文件 {} 不是合法 UTF-8", path.display()))?;
    let pw = first_line(&text);
    if pw.is_empty() {
        bail!("密码文件 {} 首行为空", path.display());
    }
    warn_if_world_readable(path);
    Ok(pw.as_bytes().to_vec())
}

/// 取首行并去掉行尾换行与回车。
///
/// 只去行尾空白，**不** trim 前导空白——密码可能以空格开头。
fn first_line(s: &str) -> &str {
    let line = s.split('\n').next().unwrap_or("");
    line.strip_suffix('\r').unwrap_or(line)
}

/// 密码文件权限过宽时提示。
///
/// 仅提示不阻断：用户可能在受控环境中刻意如此。
#[cfg(unix)]
fn warn_if_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(path) {
        let mode = m.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "警告: 密码文件 {} 的权限为 {:o}，其它用户可读\n      建议执行 chmod 600 {}",
                path.display(),
                mode & 0o777,
                path.display()
            );
        }
    }
}

/// Windows 上没有等价的简单权限位判断，跳过。
#[cfg(not(unix))]
fn warn_if_world_readable(_path: &Path) {}

/// `--password` 被误用时的解释文案。
///
/// 单独成函数便于测试，也保证 CLI 各处用词一致。
#[must_use]
pub fn explain_password_flag() -> String {
    String::from(
        "不支持 --password 参数\n\
         \n  命令行参数对同机其它用户可见（ps aux / 任务管理器），\
         \n  且会被记入 shell 历史文件。\n\
         \n  请改用：\n\
         \n    --password-stdin        从管道读取\
         \n    --password-file <路径>  从文件读取\
         \n    --password-env <变量名> 从环境变量读取（传变量名，不是值）\
         \n    （不带任何参数）        交互式输入，不回显\n",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_handles_crlf_and_empty() {
        assert_eq!(first_line("abc\r\ndef"), "abc");
        assert_eq!(first_line("abc\ndef"), "abc");
        assert_eq!(first_line("abc"), "abc");
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("\n"), "");
        // 前导空格必须保留：密码可以以空格开头
        assert_eq!(first_line("  pw  \n"), "  pw  ");
    }

    #[test]
    fn mutually_exclusive_sources_rejected() {
        let s = PasswordSource {
            env: Some("A".into()),
            stdin: true,
            ..PasswordSource::default()
        };
        assert!(s.validate().is_err());

        let ok = PasswordSource {
            stdin: true,
            ..PasswordSource::default()
        };
        assert!(ok.validate().is_ok());
        assert!(!ok.is_interactive());
        assert!(PasswordSource::default().is_interactive());
    }

    #[test]
    fn env_source_reads_existing_var() {
        // 不用 set_var：Rust 2024 中它是 unsafe，而本 crate 设了
        // unsafe_code = "forbid"，不该为测试破例。
        // 改为读一个所有平台都存在的变量，验证读取路径本身。
        let name = "PATH";
        let s = PasswordSource {
            env: Some(name.into()),
            ..PasswordSource::default()
        };
        let pw = read_password(&s, "x", false).unwrap();
        assert!(!pw.is_empty(), "PATH 应当非空");
        // 读到的应与 std::env 一致
        assert_eq!(pw, std::env::var(name).unwrap().into_bytes());
    }

    #[test]
    fn missing_env_var_errors() {
        let s = PasswordSource {
            env: Some("OMY_TEST_DEFINITELY_UNSET_VAR".into()),
            ..PasswordSource::default()
        };
        assert!(read_password(&s, "x", false).is_err());
    }

    #[test]
    fn file_source_reads_first_line() {
        let dir = std::env::temp_dir().join(format!("omy_pw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("pw.txt");
        std::fs::write(&p, "line1\nline2\n").unwrap();

        let s = PasswordSource {
            file: Some(p.clone()),
            ..PasswordSource::default()
        };
        assert_eq!(read_password(&s, "x", false).unwrap(), b"line1");

        // 空文件必须报错而非返回空密码
        std::fs::write(&p, "\n").unwrap();
        assert!(read_password(&s, "x", false).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explanation_mentions_all_alternatives() {
        let t = explain_password_flag();
        for k in ["--password-stdin", "--password-file", "--password-env", "ps aux"] {
            assert!(t.contains(k), "解释文案缺少 {k}");
        }
    }
}
