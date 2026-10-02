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
        assert!(
            matches!(&c.command, Command::Remote(cmd::remote::Cmd::AddWebdav(a))
                if a.name == "NAS" && a.password_stdin && !a.read_only),
            "add-webdav 应解析出 AddWebdav 且字段符合预期"
        );

        // 密码绝不接受 --password 明文 argv
        assert!(
            Cli::try_parse_from(["omy", "remote", "add-webdav", "NAS", "--url", "https://dav/", "--password", "x"]).is_err(),
            "不得存在 --password 明文参数"
        );

        let c = Cli::try_parse_from(["omy", "remote", "cache", "status"]).unwrap();
        assert!(matches!(c.command, Command::Remote(cmd::remote::Cmd::Cache(_))));

        // Telegram：新增的连通性自检必须真的接上子命令（漏挂会静默不出现在 help 里）。
        let c = Cli::try_parse_from(["omy", "remote", "telegram", "check"]).unwrap();
        assert!(
            matches!(&c.command, Command::Remote(cmd::remote::Cmd::Telegram(
                cmd::remote::telegram::Cmd::Check
            ))),
            "telegram check 应解析为 Telegram::Check"
        );

        // virtual 的 unlock/lock 必须挂在子命令树上（与 GUI 命令树对齐）。
        let c = Cli::try_parse_from(["omy", "remote", "virtual", "lock", "v1"]).unwrap();
        assert!(matches!(
            &c.command,
            Command::Remote(cmd::remote::Cmd::Virtual(cmd::remote::virt::Cmd::Lock(_)))
        ));
    }

    #[test]
    fn quiet_and_verbose_conflict() {
        assert!(Cli::try_parse_from(["omy", "-q", "-v", "info", "a.omy"]).is_err());
    }

    /// `remote upload` 的 help 必须诚实宣称「文件或目录」语义。
    ///
    /// 不这样会怎样：clap 把 value_name 写成「本地文件」、about 写成「上传本地文件」，
    /// 用户照 help 以为只能传单文件；而实现里 `cmd/remote/files.rs` 已对 `meta.is_dir()`
    /// 走了递归上传。help 与实现不一致是加密工具里最容易被忽视的一类——
    /// 用户不知道能传目录，功能等于不存在。这个测试直接渲染 help 字符串并断言
    /// value_name 与描述都含目录语义，而不是只断言解析成功。
    #[test]
    fn upload_help_advertises_directory_input() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let upload = cmd
            .find_subcommand_mut("remote")
            .expect("remote 子命令存在")
            .find_subcommand_mut("upload")
            .expect("remote upload 子命令存在");
        let help = upload.render_help().to_string();

        // usage 里第二个位置参数的 value_name 必须是「本地路径」而非「本地文件」。
        // 若有人把 value_name 改回「本地文件」，这条立刻红。
        assert!(
            help.contains("<本地路径>"),
            "usage 应展示 <本地路径>（与文档站 cli.md 对齐），实际 help：\n{help}"
        );
        assert!(
            !help.contains("<本地文件>"),
            "value_name 不应再写死成「本地文件」，实际：\n{help}"
        );

        // about 首行必须提到目录，不能只说「上传本地文件」。
        let first_line = help.lines().next().unwrap_or("");
        assert!(
            first_line.contains("目录"),
            "help 首行 about 应包含目录语义，实际首行：{first_line:?}"
        );

        // 位置参数的描述必须让用户知道目录会递归上传。
        assert!(
            help.contains("目录则递归上传") || help.contains("递归"),
            "参数描述应说明目录会递归上传，实际 help：\n{help}"
        );
    }

    /// 单位置文件命令必须真的挂上位置密码三通道（--password-stdin/file/env），
    /// copy 必须挂上前缀化的 --source-password-*/--dest-password-*，
    /// decrypt 必须挂上 --place-password-*（与文件密码 --password-* 区分）。
    ///
    /// 不这样会怎样：加密的 Telegram 位置在脚本里打不开，而 help 里看不到任何
    /// 输入密码的口子，功能等于不存在；copy 两端若共用同名 --password-* 会歧义。
    #[test]
    fn remote_file_commands_expose_place_password_flags() {
        use clap::CommandFactory;
        let mut root = Cli::command();
        let remote = root.find_subcommand_mut("remote").expect("remote 存在");

        // 单位置命令：--password-stdin / --password-file / --password-env 都可达。
        for sub in ["ls", "upload", "download", "mkdir", "delete", "move"] {
            let cmd = remote.find_subcommand_mut(sub).expect("remote 文件子命令存在");
            let help = cmd.render_help().to_string();
            assert!(help.contains("--password-stdin"), "{sub} help 缺 --password-stdin:\n{help}");
            assert!(help.contains("--password-file"), "{sub} help 缺 --password-file");
            assert!(help.contains("--password-env"), "{sub} help 缺 --password-env");
            assert!(
                !help.contains("--password "),
                "{sub} 不应出现明文 --password <值>"
            );
        }
        // copy：两端独立、前缀无歧义，且不能出现裸 --password-stdin。
        let copy = remote
            .find_subcommand_mut("copy")
            .expect("remote copy 存在")
            .render_help()
            .to_string();
        assert!(copy.contains("--source-password-stdin"), "copy 缺 --source-password-stdin:\n{copy}");
        assert!(copy.contains("--source-password-file"));
        assert!(copy.contains("--source-password-env"));
        assert!(copy.contains("--dest-password-stdin"), "copy 缺 --dest-password-stdin");
        assert!(copy.contains("--dest-password-file"));
        assert!(copy.contains("--dest-password-env"));
        assert!(
            !copy.contains("--password-stdin\n") && !copy.contains("--password-file"),
            "copy 不应出现无前缀的 --password-file/stdin:\n{copy}"
        );

        // decrypt：位置密码用 --place-password-*，与文件密码 --password-* 并存不冲突。
        let decrypt = remote
            .find_subcommand_mut("decrypt")
            .expect("remote decrypt 存在")
            .render_help()
            .to_string();
        assert!(decrypt.contains("--place-password-stdin"), "decrypt 缺 --place-password-stdin:\n{decrypt}");
        assert!(decrypt.contains("--place-password-file"));
        assert!(decrypt.contains("--place-password-env"));
        assert!(decrypt.contains("--password-stdin"), "decrypt 仍应保留文件密码通道 --password-stdin");

        // cache pin/unpin 也要位置密码通道。
        let cache = remote.find_subcommand_mut("cache").expect("cache 存在");
        let pin = cache
            .find_subcommand_mut("pin")
            .expect("cache pin 存在")
            .render_help()
            .to_string();
        assert!(pin.contains("--password-stdin"), "cache pin 缺 --password-stdin:\n{pin}");
    }

    /// copy 的双端密码标志必须能被 clap 解析成独立字段。
    #[test]
    fn copy_parses_source_and_dest_password_channels() {
        let c = Cli::try_parse_from([
            "omy", "remote", "copy", "p1:/a.omy", "p2:/b.omy",
            "--source-password-env", "SRC_PW",
            "--dest-password-file", "dst.txt",
        ])
        .unwrap();
        let ok = match &c.command {
            Command::Remote(cmd::remote::Cmd::Copy(a)) => {
                a.source_password_env.as_deref() == Some("SRC_PW")
                    && a.dest_password_file.as_ref().is_some_and(|p| p.ends_with("dst.txt"))
                    && !a.source_password_stdin
            }
            _ => false,
        };
        assert!(ok, "copy 应解析出独立的源/目标密码字段");
    }

    #[test]
    fn subcommand_required() {
        assert!(Cli::try_parse_from(["omy"]).is_err());
    }
}
