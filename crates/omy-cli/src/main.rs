//! omy 命令行工具。
//!
//! 与 GUI 共享 `omy-core`，行为一致——CLI 能做的 GUI 都能做，反之亦然
//! （除交互式浏览与媒体播放外）。设计契约见
//! [09 号文档](../../docs/research/09-cli-design.md)。

// 测试代码中 unwrap 与索引是合理的：失败即测试失败，且下标均为已知常量。
// 只对 cfg(test) 编译单元放宽，库与二进制代码仍受严格 lint 约束。
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::cast_possible_truncation
    )
)]

mod cmd;
mod config;
mod i18n;
mod output;
mod password;
mod progress;

use anyhow::Result;
use clap::{Parser, Subcommand};
use output::{Format, Out};
use std::path::PathBuf;

/// 顶层命令。
#[derive(Debug, Parser)]
#[command(
    name = "omy",
    version,
    about = "omy 加密工具：文件加解密、扫描、分片与密钥管理",
    long_about = None,
    // 拒绝 --password 需要能捕获未知参数并给出解释，
    // 因此不能让 clap 直接以通用错误退出
    disable_help_subcommand = true,
)]
struct Cli {
    /// 详细输出，可叠加（-vv）
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    /// 仅输出错误
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// 以 JSON 输出，便于脚本消费
    #[arg(long, global = true)]
    json: bool,

    /// 禁用彩色输出
    #[arg(long, global = true)]
    no_color: bool,

    /// 指定配置文件路径
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// 跳过所有确认（危险操作慎用）
    #[arg(long, global = true)]
    yes: bool,

    #[command(subcommand)]
    command: Command,
}

/// 子命令。
#[derive(Debug, Subcommand)]
enum Command {
    /// 加密文件或目录
    Encrypt(cmd::encrypt::Args),
    /// 解密文件
    Decrypt(cmd::decrypt::Args),
    /// 查看文件信息（无需密码即可看部分）
    Info(cmd::info::Args),
    /// 验证完整性
    Verify(cmd::verify::Args),
    /// 列出目录中的 .omy 文件
    List(cmd::list::Args),
    /// 扫描目录，匹配密码并列出可解锁文件
    Scan(cmd::scan::Args),
    /// 解密并输出到标准输出（管道友好）
    Cat(cmd::cat::Args),
    /// 密钥 slot 管理
    #[command(subcommand)]
    Key(cmd::key::Cmd),
    /// 分片切分与合并
    #[command(subcommand)]
    Shard(cmd::shard::Cmd),
    /// 局域网共享与访问
    #[command(subcommand)]
    Share(cmd::share::Cmd),
    /// KDF 与加解密性能基准
    Bench(cmd::bench::Args),
    /// 环境自检
    Doctor(cmd::doctor::Args),
    /// 远程位置（WebDAV / Telegram）：注册、浏览、上传下载、缓存管理
    #[command(subcommand)]
    Remote(cmd::remote::Cmd),
    /// 生成 shell 补全脚本
    Completion(cmd::completion::Args),
}

fn main() -> std::process::ExitCode {
    // 先拦截 --password：clap 报「未知参数」对用户毫无帮助，
    // 而这是最容易误用、后果最严重（旁路 L12）的一个。
    if std::env::args().any(|a| a == "--password" || a.starts_with("--password=")) {
        eprint!("{}", password::explain_password_flag());
        return std::process::ExitCode::from(2);
    }

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap 的 help/version 也走这里，那是正常退出
            let is_help = matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp
                    | clap::error::ErrorKind::DisplayVersion
                    | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            let _ = e.print();
            return std::process::ExitCode::from(if is_help { 0 } else { 2 });
        }
    };

    let cfg = match config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("错误: 读取配置失败: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    i18n::init(Some(cfg.ui.language.as_str()));

    let format = if cli.json { Format::Json } else { Format::Human };
    let out = Out::new(format, cli.quiet, cli.verbose, cli.no_color);

    let r = dispatch(&cli, &cfg, &out);
    match r {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            let code = output::report_error(&out, &e);
            std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
    }
}

/// 分发到子命令。
fn dispatch(cli: &Cli, cfg: &config::Config, out: &Out) -> Result<()> {
    let ctx = cmd::Ctx {
        out,
        cfg,
        assume_yes: cli.yes,
        config_path: cli.config.as_deref(),
    };
    match &cli.command {
        Command::Encrypt(a) => cmd::encrypt::run(&ctx, a),
        Command::Decrypt(a) => cmd::decrypt::run(&ctx, a),
        Command::Info(a) => cmd::info::run(&ctx, a),
        Command::Verify(a) => cmd::verify::run(&ctx, a),
        Command::List(a) => cmd::list::run(&ctx, a),
        Command::Scan(a) => cmd::scan::run(&ctx, a),
        Command::Cat(a) => cmd::cat::run(&ctx, a),
        Command::Key(c) => cmd::key::run(&ctx, c),
        Command::Shard(c) => cmd::shard::run(&ctx, c),
        Command::Share(c) => cmd::share::run(&ctx, c),
        Command::Bench(a) => cmd::bench::run(&ctx, a),
        Command::Doctor(a) => cmd::doctor::run(&ctx, a),
        Command::Remote(c) => cmd::remote::run(&ctx, c),
        Command::Completion(a) => cmd::completion::run::<Cli>(&ctx, a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap 会在此处校验参数定义自身的一致性（重名、冲突引用等）
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_basic_invocations() {
        let c = Cli::try_parse_from(["omy", "info", "a.omy"]).unwrap();
        assert!(matches!(c.command, Command::Info(_)));

        let c = Cli::try_parse_from(["omy", "--json", "scan", "."]).unwrap();
        assert!(c.json);

        let c = Cli::try_parse_from(["omy", "encrypt", "-o", "x.omy", "a.txt"]).unwrap();
        assert!(matches!(c.command, Command::Encrypt(_)));

        let c = Cli::try_parse_from(["omy", "key", "list", "a.omy"]).unwrap();
        assert!(matches!(c.command, Command::Key(_)));

        let c = Cli::try_parse_from(["omy", "shard", "split", "--size", "4M", "a.omy"]).unwrap();
        assert!(matches!(c.command, Command::Shard(_)));

        // remote 子命令：位置管理与文件操作
        let c = Cli::try_parse_from(["omy", "remote", "list"]).unwrap();
        assert!(matches!(c.command, Command::Remote(_)));

        let c = Cli::try_parse_from(["omy", "remote", "add-webdav", "NAS", "--url", "https://dav/", "--password-stdin"]).unwrap();
        match &c.command {
            Command::Remote(cmd::remote::Cmd::AddWebdav(a)) => {
                assert_eq!(a.name, "NAS");
                assert!(a.password_stdin);
                assert!(!a.read_only, "默认应可写");
            }
            _ => panic!("应解析为 add-webdav"),
        }

        // 密码绝不接受 --password 明文 argv
        assert!(
            Cli::try_parse_from(["omy", "remote", "add-webdav", "NAS", "--url", "https://dav/", "--password", "x"]).is_err(),
            "不得存在 --password 明文参数"
        );

        let c = Cli::try_parse_from(["omy", "remote", "cache", "status"]).unwrap();
        assert!(matches!(c.command, Command::Remote(cmd::remote::Cmd::Cache(_))));
    }

    #[test]
    fn quiet_and_verbose_conflict() {
        assert!(Cli::try_parse_from(["omy", "-q", "-v", "info", "a.omy"]).is_err());
    }

    #[test]
    fn subcommand_required() {
        assert!(Cli::try_parse_from(["omy"]).is_err());
    }
}
