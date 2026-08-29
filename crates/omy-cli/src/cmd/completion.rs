//! `omy completion`：生成 shell 补全脚本。

use super::Ctx;
use anyhow::Result;
use clap::{Args as ClapArgs, CommandFactory};
use clap_complete::Shell;

/// `completion` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 目标 shell
    #[arg(value_enum)]
    pub shell: Shell,
}

/// 执行 `completion`。
///
/// 补全脚本写到 stdout，由用户重定向到合适位置：
///
/// ```bash
/// omy completion bash > /etc/bash_completion.d/omy
/// omy completion powershell | Out-String | Invoke-Expression
/// ```
///
/// # Errors
///
/// 当前实现不会失败，返回 `Result` 是为了与其它子命令签名一致。
pub fn run<C: CommandFactory>(_ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let mut cmd = C::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(a.shell, &mut cmd, name, &mut std::io::stdout());
    Ok(())
}
