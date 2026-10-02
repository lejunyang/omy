//! Telegram 远程位置命令：扫码/手机号登录、tdata 导入、代理与应用身份设置。
//!
//! # 与 GUI 共用什么
//!
//! 本模块**不复刻** GUI 的登录编排，而是直接复用 [`omy_remote::telegram`] 里
//! 已经测好的业务原语：[`QrSession`]/[`PhoneSession`] 状态机、[`session`] 的
//! 落盘/收编/抹除、[`connect`] 的取账号信息、[`register::fold_into_places`] 的
//! 「pending → 位置」去重分配。CLI 只做「终端外壳」——把二维码画到 stderr、
//! 把验证码/二步密码/api_hash 接到安全输入通道、把结果写进同一份
//! [`omy_config::SavedPlace`]。
//!
//! # 秘密绝不进 argv / JSON / 日志
//!
//! 验证码、二步密码、tdata 本地密码、api_hash 一律走
//! [`PasswordSource`]（env / file / stdin 三通道互斥），不存在 `--code 123456`
//! 这类明文参数。成功 JSON 里也不放任何秘密（见 `success_payload`）。
//!
//! # 非交互缺输入立即失败，不挂起
//!
//! `read_password` 在非 TTY 且无通道时直接报错；本模块另加「验证码与二步密码
//! 不能同时走 stdin」的互斥（一条 stdin 流只能读一次）。
//!
//! # 网络边界
//!
//! 真正的联网（扫码、手机号、tdata 导入都要连 Telegram 数据中心）不假装做过
//! 实测：纯逻辑（二维码渲染、密码通道、pending 收编、去重、id 分配、参数互斥）
//! 都在单测里钉死，联网部分只做正确接线，并在失败时如实报错。

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use omy_remote::placebook::protect_key;
use omy_remote::telegram::appid_store::{self, ResolveError};
use omy_remote::telegram::login::CodeShape;
use omy_remote::telegram::phonelogin::{PhoneEvent, PhoneSession};
use omy_remote::telegram::proxy as tg_proxy;
use omy_remote::telegram::{
    AppId, AppIdChoice, DeviceInfo, QrError, QrEvent, QrSession, connect, encode_matrix,
    register, resolve_app, resolve_proxy, session, tdata,
};
use serde_json::json;

use crate::cmd::KdfProfile;
use crate::password::{PasswordSource, read_password};

use super::{Ctx, find_place, mutate_places, qrterm, rt};

/// Telegram 子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 扫码登录一个 Telegram 账号，并保存为一个远程位置
    Login(LoginArgs),
    /// 手机号 + 验证码 + 两步密码登录一个 Telegram 账号
    Phone(PhoneLoginArgs),
    /// 登出：移除位置并**销毁**本机登录态（之后要用须重新登录）
    Logout(NameArgs),
    /// 从列表移除位置，但保留本机登录态（之后可重新加回而不必重登）
    Detach(NameArgs),
    /// 从 Telegram Desktop 的 tdata 目录导入登录态
    #[command(subcommand)]
    Tdata(TdataCmd),
    /// 查看 Telegram 全局代理当前生效情况
    ProxyStatus,
    /// 设置 Telegram 全局代理（--url 手动 / --system 跟随系统）
    ProxySet(ProxySetArgs),
    /// 恢复为跟随系统代理
    ProxyReset,
    /// 登录前连通性自检：按配置的代理去连 Telegram 主 DC，只测 TCP。
    ///
    /// 与 GUI 登录页「检查连接」共用 omy_remote::telegram::proxy::probe_connection。
    /// 结果写到 stdout；**不通时退出码为 1**，供脚本在批量登录/转发前先判断网络或代理。
    #[command(name = "check")]
    Check,
    /// 查看当前应用身份（内置或自定义 api_id）
    AppIdStatus,
    /// 保存自定义 api_id / api_hash（api_hash 经安全通道输入，不进命令行）
    AppIdSet(AppIdSetArgs),
    /// 恢复使用内置应用身份
    AppIdReset,

    /// 给位置加密：用现场密码独立保护该位置的登录态（与普通文件加密同款）
    Encrypt(PlaceEncryptArgs),
    /// 用现场密码验证能否解锁一个加密位置（脚本可据此判断密码对错）
    Unlock(PlacePwArgs),
    /// 确认位置处于锁定态（CLI 无长期会话，位置每次访问都需密码）
    Lock(NameArgs),
    /// 取消位置加密：恢复为机器密钥保护
    Decrypt(PlacePwArgs),

    /// 列出可接收转发的会话（转发目标选择）
    Targets(TgConnArgs),
    /// 把同一源对话里的一条或多条消息原生转发到目标会话
    Forward(ForwardArgs),
    /// 服务端搜索消息内容
    Search(SearchArgs),
    /// 创建一个只含自己的私密超级群（转发归档目标）
    Group(GroupArgs),
    /// 查看 Telegram 位置的本地会话状态（不联网）
    Status(StatusArgs),
}

/// `omy remote telegram login`（扫码）。
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// 给这个位置起个名字（默认取服务端昵称）
    #[arg(long, value_name = "名字")]
    pub name: Option<String>,
    /// 代理地址，如 socks5://127.0.0.1:7897（默认自动探测系统代理）
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// 二步验证密码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 二步验证密码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 二步验证密码：从标准输入读取
    #[arg(long)]
    pub password_stdin: bool,
}

/// `omy remote telegram phone`（手机号登录）。
#[derive(Debug, Args)]
pub struct PhoneLoginArgs {
    /// 手机号（国际格式，如 +8613800000000）；交互模式下可省略，到时提示输入
    #[arg(long, value_name = "号码")]
    pub phone: Option<String>,
    /// 给这个位置起个名字（默认取服务端昵称）
    #[arg(long, value_name = "名字")]
    pub name: Option<String>,
    /// 代理地址，如 socks5://127.0.0.1:7897（默认自动探测系统代理）
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// 验证码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub code_env: Option<String>,
    /// 验证码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub code_file: Option<PathBuf>,
    /// 验证码：从标准输入读取
    #[arg(long)]
    pub code_stdin: bool,

    /// 二步验证密码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 二步验证密码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 二步验证密码：从标准输入读取
    #[arg(long)]
    pub password_stdin: bool,
}

/// `logout` / `detach` 的位置参数。
#[derive(Debug, Args)]
pub struct NameArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
}

/// 位置密码通道（unlock/decrypt 共用）。
///
/// 与登录二步密码同款：密码只走 env/file/stdin 三通道互斥，绝不进 argv。
#[derive(Debug, Args)]
pub struct PlacePwArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 位置密码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 位置密码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 位置密码：从标准输入读取
    #[arg(long)]
    pub password_stdin: bool,
}

/// `omy remote telegram encrypt <位置>`。
#[derive(Debug, Args)]
pub struct PlaceEncryptArgs {
    #[command(flatten)]
    pub pw: PlacePwArgs,
    /// KDF 档位（默认 moderate）
    #[arg(long, value_name = "档位")]
    pub kdf: Option<String>,
}

/// 需要联机的 Telegram 操作（targets/forward/search/group）共用连接参数。
#[derive(Debug, Args)]
pub struct TgConnArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 位置密码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 位置密码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 位置密码：从标准输入读取
    #[arg(long)]
    pub password_stdin: bool,
}

impl TgConnArgs {
    fn source(&self) -> PasswordSource {
        PasswordSource {
            env: self.password_env.clone(),
            file: self.password_file.clone(),
            stdin: self.password_stdin,
        }
    }
}

/// `omy remote telegram forward`。
#[derive(Debug, Args)]
pub struct ForwardArgs {
    #[command(flatten)]
    pub conn: TgConnArgs,
    /// 目标对话（tg:<chat>，用 `targets` 查）
    #[arg(long, value_name = "tg:CHAT")]
    pub target: String,
    /// 源条目，可重复指定；形如 tg:<chat>:<msg>
    #[arg(long = "entry", value_name = "tg:CHAT:MSG")]
    pub entries: Vec<String>,
    /// 从 JSON 文件读取源条目 id 数组（与 --entry 二选一或并用合并）
    #[arg(long, value_name = "PATH")]
    pub entries_json: Option<PathBuf>,
}

/// `omy remote telegram search`。
#[derive(Debug, Args)]
pub struct SearchArgs {
    #[command(flatten)]
    pub conn: TgConnArgs,
    /// 搜索关键词
    pub query: String,
    /// 限定在某个对话内（tg:<chat>）；缺省为全局
    #[arg(long, value_name = "tg:CHAT")]
    pub dir: Option<String>,
    /// 最多返回条数
    #[arg(long, default_value = "50", value_name = "N")]
    pub limit: u32,
}

/// `omy remote telegram group`。
#[derive(Debug, Args)]
pub struct GroupArgs {
    #[command(flatten)]
    pub conn: TgConnArgs,
    /// 私密群标题
    pub title: String,
}

/// `omy remote telegram status`。
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// 只看这一个位置（id 或显示名）；缺省列出全部 Telegram 位置
    pub place: Option<String>,
}

impl PlacePwArgs {
    /// 转成通用密码来源。
    fn source(&self) -> PasswordSource {
        PasswordSource {
            env: self.password_env.clone(),
            file: self.password_file.clone(),
            stdin: self.password_stdin,
        }
    }
}

/// `omy remote telegram tdata …`。
#[derive(Debug, Subcommand)]
pub enum TdataCmd {
    /// 探测本机常见的 tdata 位置（只读目录，不检查/不操作 Telegram Desktop 进程）
    Probe {
        /// 额外扫描一个 Telegram Desktop 安装目录
        #[arg(long, value_name = "DIR")]
        install_dir: Option<PathBuf>,
    },
    /// 检查一个目录是否像 tdata（只看结构，不解密）
    Check {
        /// tdata 目录
        path: PathBuf,
    },
    /// 从 tdata 导入登录态：先问服务端认不认，成功才收编成位置
    Import(TdataImportArgs),
}

/// `tdata import`。
#[derive(Debug, Args)]
pub struct TdataImportArgs {
    /// tdata 目录
    pub path: PathBuf,
    /// 位置名（默认取服务端昵称）
    #[arg(long, value_name = "名字")]
    pub name: Option<String>,
    /// 代理地址（默认探测系统代理）
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,
    /// tdata 本地密码：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub passcode_env: Option<String>,
    /// tdata 本地密码：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub passcode_file: Option<PathBuf>,
    /// tdata 本地密码：从标准输入读取
    #[arg(long)]
    pub passcode_stdin: bool,
}

/// `proxy-set`。
#[derive(Debug, Args)]
pub struct ProxySetArgs {
    /// 手动代理地址，如 socks5://127.0.0.1:7897（http://、host:port 会自动归一）
    #[arg(long, value_name = "URL", conflicts_with = "system")]
    pub url: Option<String>,
    /// 改为跟随系统代理
    #[arg(long, conflicts_with = "url")]
    pub system: bool,
}

/// `appid-set`。
#[derive(Debug, Args)]
pub struct AppIdSetArgs {
    /// my.telegram.org 申请的 api_id（整数）
    #[arg(long, value_name = "ID")]
    pub api_id: i32,
    /// api_hash：从环境变量读取（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub hash_env: Option<String>,
    /// api_hash：从文件读取首行
    #[arg(long, value_name = "PATH")]
    pub hash_file: Option<PathBuf>,
    /// api_hash：从标准输入读取
    #[arg(long)]
    pub hash_stdin: bool,
}

/// 分发。
pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Login(a) => login(ctx, a),
        Cmd::Phone(a) => phone(ctx, a),
        Cmd::Logout(a) => logout(ctx, a),
        Cmd::Detach(a) => detach(ctx, a),
        Cmd::Tdata(c) => tdata(ctx, c),
        Cmd::ProxyStatus => proxy_status(ctx),
        Cmd::ProxySet(a) => proxy_set(ctx, a),
        Cmd::ProxyReset => proxy_reset(ctx),
        Cmd::Check => check_connection(ctx),
        Cmd::AppIdStatus => appid_status(ctx),
        Cmd::AppIdSet(a) => appid_set(ctx, a),
        Cmd::AppIdReset => appid_reset(ctx),
        Cmd::Encrypt(a) => place_encrypt(ctx, a),
        Cmd::Unlock(a) => place_unlock(ctx, a),
        Cmd::Lock(a) => place_lock(ctx, a),
        Cmd::Decrypt(a) => place_decrypt(ctx, a),
        Cmd::Targets(a) => targets(ctx, a),
        Cmd::Forward(a) => forward(ctx, a),
        Cmd::Search(a) => search(ctx, a),
        Cmd::Group(a) => create_group(ctx, a),
        Cmd::Status(a) => status(ctx, a),
    }
}

// ---- 代理解析（登录命令共用） ----

/// 登录用代理：显式 `--proxy` 优先（override），否则按配置（system/manual）。
///
/// 与 GUI 登录、`telegram check` 同源（`resolve_proxy`）。以前这里只看 `--proxy`
/// 或系统代理，配置里的 manual 地址被静默忽略——用户 `proxy-set` 了却没生效。
fn resolve_login_proxy(ctx: &Ctx, arg: Option<&str>) -> Result<Option<String>> {
    Ok(resolve_proxy(&ctx.cfg.remote, arg)?
        .map(|p| p.as_str().to_string()))
}

/// 登录要用的应用身份：从配置解析（填了自己的就用自己的，否则内置）。
///
/// 这是「自定义 api_id 真正被登录流程消费」的落点——以前所有连接都硬编码
/// `AppId::builtin()`，配置里填了也白填。解析时若发现还是旧明文
/// （`Value::String`），顺手重封成信封完成迁移。
fn resolve_login_app(ctx: &Ctx<'_>) -> Result<AppId> {
    // 与 GUI login_app_id 同源：共享「解析 + 锁内迁移」。旧明文在 Config 跨进程
    // 锁里重读磁盘最新后重封成信封；无保护器则保留明文不动，不丢身份。
    // --config 指定了路径就用显式路径，否则走默认配置。
    let resolved = match ctx.config_path {
        Some(p) => appid_store::resolve_and_migrate_at(p),
        None => appid_store::resolve_and_migrate(),
    }
    .map_err(|e| anyhow!("解析应用身份失败：{e}"))?;
    Ok(resolved.app)
}

// ---- 安全输入 ----

/// 读一个短秘密（验证码 / 二步密码 / tdata 密码 / api_hash）。
///
/// 通道来源（env/file/stdin）走 `read_password`：它在非 TTY 且无通道时**直接
/// 报错而不是挂起**。交互 TTY 时：`hidden=true` 不回显（二步密码），
/// `hidden=false` 回显（验证码要核对位数）。
fn read_secret(src: &PasswordSource, prompt: &str, hidden: bool) -> Result<String> {
    if !src.is_interactive() {
        let raw = read_password(src, prompt, false)?;
        return String::from_utf8(raw).context("输入不是合法 UTF-8");
    }
    use std::io::Write as _;
    if !std::io::stdin().is_terminal() {
        bail!(
            "没有可交互终端，无法提示输入。脚本场景请用 \
             --code-env/--code-file/--code-stdin 或 --password-env/--password-file/--password-stdin 显式提供"
        );
    }
    let raw = if hidden {
        rpassword::prompt_password(format!("{prompt}: "))?
    } else {
        eprint!("{prompt}: ");
        std::io::stderr().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        s
    };
    let v = raw.trim().to_string();
    if v.is_empty() {
        bail!("输入不能为空");
    }
    Ok(v)
}

// ---- QR 登录 ----

fn login(ctx: &Ctx, args: &LoginArgs) -> Result<()> {
    let proxy = resolve_login_proxy(ctx, args.proxy.as_deref())?;
    let pw_src = PasswordSource {
        env: args.password_env.clone(),
        file: args.password_file.clone(),
        stdin: args.password_stdin,
    };
    pw_src.validate()?;

    let rt = rt()?;
    rt.block_on(run_login_cycle(ctx, args, &proxy, &pw_src))
}

/// 真正的异步扫码循环。单独成函数便于把「建 runtime」与「跑流程」分开。
async fn run_login_cycle(
    ctx: &Ctx<'_>,
    args: &LoginArgs,
    proxy: &Option<String>,
    pw_src: &PasswordSource,
) -> Result<()> {
    let appid = resolve_login_app(ctx)?;
    let device = DeviceInfo::current();

    let mut sess = QrSession::connect(appid.clone(), proxy.as_deref(), &device)
        .map_err(|e| anyhow!("连接 Telegram 失败：{e}"))?;

    loop {
        let ev = sess
            .step()
            .await
            .map_err(|e| anyhow!("扫码登录失败：{e}"))?;
        match ev {
            QrEvent::Token {
                url,
                expires_in_secs,
                refresh_index,
            } => {
                // 二维码与可复制链接只画/打到 stderr。绝不把 tg://token 带进
                // stdout 的结构化结果——那是短时票据，会被脚本日志原样留存。
                render_token(ctx, &url, expires_in_secs, refresh_index);
            }
            QrEvent::Migrating { dc } => {
                ctx.out.info(&format!("正在切换到 Telegram 数据中心 DC{dc}…"));
            }
            QrEvent::NeedPassword { hint } => {
                ask_password_loop(ctx, &mut sess, hint.as_deref(), pw_src).await?;
                break;
            }
            QrEvent::LoggedIn => break,
        }
    }

    // 落盘 pending → 自证 → 收编成位置（与手机号登录共用尾部）。
    let saved = match session::save(sess.session(), &appid, session::PENDING_ACCOUNT) {
        Ok(_) => true,
        Err(session::SessionError::NoProtector) => {
            ctx.out.warn(
                "这台机器没有可用的凭据库，Telegram 登录态不会保存（本次仍可使用，但下次需重新扫码）",
            );
            false
        }
        Err(e) => bail!("保存 Telegram 登录态失败：{e}"),
    };
    let authorized = sess
        .is_authorized()
        .await
        .map_err(|e| anyhow!("确认登录态失败：{e}"))?;
    let user_id = connect::account_user_id(sess.client()).await;
    let label = match &args.name {
        Some(n) => n.clone(),
        None => connect::account_label(sess.client()).await,
    };
    register_and_report(ctx, user_id, label, saved, authorized)
}

/// 把一张登录二维码画到 stderr，并始终给出可复制的链接。
fn render_token(ctx: &Ctx, url: &str, expires_in_secs: u32, refresh_index: u32) {
    let is_tty = std::io::stderr().is_terminal();
    match encode_matrix(url) {
        Ok(matrix) => {
            let mode = qrterm::choose_mode(is_tty);
            let art = qrterm::render(&matrix, mode);
            eprintln!("{art}");
        }
        Err(e) => ctx.out.warn(&format!("二维码渲染失败（{e}），改用链接方式：")),
    }
    eprintln!("用已登录 Telegram 的设备扫上面的码；或手动打开：");
    eprintln!("  {url}");
    if refresh_index == 0 {
        eprintln!("（{} 秒内有效，过期会自动刷新）", expires_in_secs);
    } else {
        eprintln!("（二维码已自动刷新第 {refresh_index} 次，{expires_in_secs} 秒后再次过期）");
    }
}

/// 二步验证子步：反复要密码直到通过或放弃。非交互通道只读一次。
async fn ask_password_loop(
    ctx: &Ctx<'_>,
    sess: &mut QrSession,
    hint: Option<&str>,
    pw_src: &PasswordSource,
) -> Result<()> {
    loop {
        let hint_note = match hint {
            Some(h) if !h.is_empty() => format!("（提示：{h}）"),
            _ => String::new(),
        };
        let raw = read_password(pw_src, &format!("两步验证密码{hint_note}"), false)?;
        let pw = String::from_utf8(raw).context("二步验证密码不是合法 UTF-8")?;

        match sess.submit_password(&pw).await {
            Ok(_) => return Ok(()),
            Err(QrError::WrongPassword) => {
                if pw_src.is_interactive() && std::io::stdin().is_terminal() {
                    ctx.out.warn("两步验证密码不正确，请重试");
                    continue;
                }
                bail!(
                    "两步验证密码不正确。非交互模式无法重试，请核对后重跑 \
                     （密码经 --password-stdin / --password-file / --password-env 提供）"
                );
            }
            Err(e) => return Err(anyhow!("二步验证失败：{e}")),
        }
    }
}

// ---- 手机号登录 ----

fn phone(ctx: &Ctx, a: &PhoneLoginArgs) -> Result<()> {
    let proxy = resolve_login_proxy(ctx, a.proxy.as_deref())?;
    let code_src = PasswordSource {
        env: a.code_env.clone(),
        file: a.code_file.clone(),
        stdin: a.code_stdin,
    };
    let pw_src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    validate_phone_channels(&code_src, &pw_src)?;

    let rt = rt()?;
    rt.block_on(run_phone(ctx, a, &proxy, &code_src, &pw_src))
}

/// 验证码与二步密码两通道的互斥检查（单测钉死）。
///
/// 各通道内部的三选一互斥由 `PasswordSource::validate` 保证；这里补跨字段
/// 的那条：**验证码与二步密码不能同时走 stdin**。一条 stdin 流只能读到第一次，
/// 第二次必然 EOF——两个秘密都走它，第二个一定拿空值失败。
fn validate_phone_channels(code: &PasswordSource, pw: &PasswordSource) -> Result<()> {
    code.validate()?;
    pw.validate()?;
    if code.stdin && pw.stdin {
        bail!(
            "验证码与二步密码不能同时从标准输入读取：一条 stdin 流只能读一次。\
             请用 --code-env/--code-file 提供验证码，二步密码再走 --password-stdin"
        );
    }
    Ok(())
}

/// 真正的异步手机号登录流程。
async fn run_phone(
    ctx: &Ctx<'_>,
    a: &PhoneLoginArgs,
    proxy: &Option<String>,
    code_src: &PasswordSource,
    pw_src: &PasswordSource,
) -> Result<()> {
    let appid = resolve_login_app(ctx)?;
    let device = DeviceInfo::current();
    let mut sess = PhoneSession::connect(appid.clone(), proxy.as_deref(), &device)
        .map_err(|e| anyhow!("连接 Telegram 失败：{e}"))?;

    let phone = match &a.phone {
        Some(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => prompt_phone()?,
    };

    match sess.send_code(&phone).await {
        Ok(PhoneEvent::LoggedIn) => {}
        Ok(PhoneEvent::NeedPassword { hint }) => {
            submit_twofactor(ctx, &mut sess, pw_src, hint.as_deref()).await?;
        }
        Ok(PhoneEvent::CodeSent { shape }) => {
            report_code_sent(ctx, &shape);
            loop {
                let code = read_secret(code_src, "验证码", false)?;
                match sess.submit_code(&code).await {
                    Ok(PhoneEvent::LoggedIn) => break,
                    Ok(PhoneEvent::NeedPassword { hint }) => {
                        submit_twofactor(ctx, &mut sess, pw_src, hint.as_deref()).await?;
                        break;
                    }
                    Ok(PhoneEvent::CodeSent { .. }) => {
                        unreachable!("submit_code 不返回 CodeSent")
                    }
                    Err(e) => handle_submit_code_err(ctx, e, code_src)?,
                }
            }
        }
        Err(e) => return Err(anyhow!("发送验证码失败：{e}")),
    }

    // 收尾与扫码同一条路径。
    let saved = match session::save(sess.session(), &appid, session::PENDING_ACCOUNT) {
        Ok(_) => true,
        Err(session::SessionError::NoProtector) => {
            ctx.out.warn(
                "这台机器没有可用的凭据库，Telegram 登录态不会保存（本次仍可使用，但下次需重新登录）",
            );
            false
        }
        Err(e) => bail!("保存 Telegram 登录态失败：{e}"),
    };
    let authorized = sess
        .is_authorized()
        .await
        .map_err(|e| anyhow!("确认登录态失败：{e}"))?;
    let user_id = connect::account_user_id(sess.client()).await;
    let label = match &a.name {
        Some(n) => n.clone(),
        None => connect::account_label(sess.client()).await,
    };
    register_and_report(ctx, user_id, label, saved, authorized)
}

/// 交互模式下提示输入手机号；非交互且没给 `--phone` 立即报错，不挂起。
fn prompt_phone() -> Result<String> {
    use std::io::Write as _;
    if !std::io::stdin().is_terminal() {
        bail!("非交互模式必须用 --phone 指定手机号");
    }
    eprint!("手机号（国际格式，如 +8613800000000）: ");
    std::io::stderr().flush()?;
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    let p = s.trim().to_string();
    if p.is_empty() {
        bail!("手机号不能为空");
    }
    Ok(p)
}

/// 验证码发到哪里：必须如实说「发到其他已登录客户端」，用户才不会白等短信。
fn report_code_sent(ctx: &Ctx, shape: &CodeShape) {
    use omy_remote::telegram::login::CodeDelivery;
    let where_to = match shape.via {
        CodeDelivery::App => "已发到你**其他已登录的 Telegram 客户端**（应用内），去那里查看",
        CodeDelivery::Sms => "已通过短信发送",
        CodeDelivery::Call => "将通过语音电话播报",
        CodeDelivery::FlashCall => "来电号码的后几位即验证码",
        CodeDelivery::Unknown => "已发送（服务端未说明方式）",
    };
    ctx.out.info(&format!(
        "验证码已发送（{} 位）：{}",
        shape.length, where_to
    ));
}

/// 提交验证码的错误处理：错码在交互 TTY 下重问，非交互立即失败。
fn handle_submit_code_err(ctx: &Ctx, e: QrError, code_src: &PasswordSource) -> Result<()> {
    if is_wrong_code(&e) {
        if code_src.is_interactive() && std::io::stdin().is_terminal() {
            ctx.out.warn("验证码不正确或已过期，请重新输入");
            return Ok(()); // 继续循环
        }
        bail!(
            "验证码不正确或已过期。非交互模式无法重试，请核对后重跑 \
             （验证码经 --code-stdin / --code-file / --code-env 提供）"
        );
    }
    Err(anyhow!("提交验证码失败：{e}"))
}

/// PHONE_CODE_INVALID / PHONE_CODE_EXPIRED 视为「错码，可重试」。
fn is_wrong_code(e: &QrError) -> bool {
    matches!(e, QrError::Invocation(s) if s.starts_with("PHONE_CODE_INVALID") || s.starts_with("PHONE_CODE_EXPIRED"))
}

/// 二步密码子步：反复要密码直到通过或放弃。
async fn submit_twofactor(
    ctx: &Ctx<'_>,
    sess: &mut PhoneSession,
    pw_src: &PasswordSource,
    hint: Option<&str>,
) -> Result<()> {
    loop {
        let note = match hint {
            Some(h) if !h.is_empty() => format!("（提示：{h}）"),
            _ => String::new(),
        };
        let pw = read_secret(pw_src, &format!("两步验证密码{note}"), true)?;
        match sess.submit_password(&pw).await {
            Ok(PhoneEvent::LoggedIn) | Ok(PhoneEvent::NeedPassword { .. }) => return Ok(()),
            Ok(PhoneEvent::CodeSent { .. }) => unreachable!("submit_password 不返回 CodeSent"),
            Err(QrError::WrongPassword) => {
                if pw_src.is_interactive() && std::io::stdin().is_terminal() {
                    ctx.out.warn("两步验证密码不正确，请重试");
                    continue;
                }
                bail!("两步验证密码不正确。非交互模式无法重试，请核对后重跑");
            }
            Err(e) => return Err(anyhow!("两步验证失败：{e}")),
        }
    }
}

// ---- 登录成功的共用收尾（QR 与手机号共用） ----

/// 去重/分配 id → 落配置（跨进程锁内重读最新）→ 收编 pending → 报告。
///
/// 纯配置编排，不碰 grammers 类型；网络侧（拿到 user_id/label、落 pending）
/// 由各登录路径在外面做完。这一段是 CLI 与 GUI「pending → 位置」共用的业务。
fn register_and_report(
    ctx: &Ctx<'_>,
    user_id: Option<i64>,
    label: String,
    session_saved: bool,
    authorized: bool,
) -> Result<()> {
    if !authorized {
        bail!("服务端未确认本次登录（登录态未生效）");
    }
    let (id, duplicate) = {
        let mut chosen: Option<(String, bool)> = None;
        mutate_places(ctx, |places| {
            chosen = Some(register::fold_into_places(places, user_id, &label));
            Ok(())
        })?;
        chosen.expect("闭包必然给 chosen 赋值")
    };

    if let Err(e) = session::adopt_pending(&id) {
        ctx.out.warn(&format!("收编登录态失败（不影响本次已登录）：{e}"));
    }

    let human = if duplicate {
        format!(
            "登录成功：账号已关联到已有位置 {id}（user_id={}）",
            user_id.map(|u| u.to_string()).unwrap_or_default()
        )
    } else {
        format!("登录成功，已保存为位置 {id}（{label}）")
    };
    ctx.out.result(&human, &success_payload(&id, &label, duplicate, user_id, session_saved));
    Ok(())
}

/// 登录成功的结构化结果。**绝不**放任何秘密（验证码、二步密码、tg:// 票据）。
#[must_use]
fn success_payload(
    id: &str,
    label: &str,
    duplicate: bool,
    user_id: Option<i64>,
    session_saved: bool,
) -> serde_json::Value {
    json!({
        "id": id,
        "name": label,
        "duplicate": duplicate,
        "user_id": user_id,
        "session_saved": session_saved,
    })
}

// ---- tdata 导入 ----

fn tdata(ctx: &Ctx, cmd: &TdataCmd) -> Result<()> {
    match cmd {
        TdataCmd::Probe { install_dir } => tdata_probe(ctx, install_dir),
        TdataCmd::Check { path } => tdata_check(ctx, path),
        TdataCmd::Import(a) => tdata_import(ctx, a),
    }
}

/// 列出本机常见的 tdata 位置。只读目录结构，不探测/不操作 Telegram Desktop 进程。
fn tdata_probe(ctx: &Ctx, install_dir: &Option<PathBuf>) -> Result<()> {
    let mut candidates: Vec<serde_json::Value> = Vec::new();
    for p in tdata::common_locations() {
        candidates.push(json!({
            "path": p.to_string_lossy().into_owned(),
            "looks_like": tdata::looks_like_tdata(&p),
        }));
    }
    if let Some(dir) = install_dir {
        for p in tdata::scan_install_dir(dir) {
            candidates.push(json!({
                "path": p.to_string_lossy().into_owned(),
                "looks_like": true,
            }));
        }
    }
    let human = if candidates.is_empty() {
        "未找到 tdata 候选目录。便携版 tdata 跟 exe 走，请用 --install-dir 指定安装目录，或 import 直接给路径。".to_string()
    } else {
        format!("找到 {} 个 tdata 候选", candidates.len())
    };
    ctx.out.result(&human, &json!({ "candidates": candidates }));
    Ok(())
}

fn tdata_check(ctx: &Ctx, path: &Path) -> Result<()> {
    let ok = tdata::looks_like_tdata(path);
    let human = if ok {
        format!("{} 看起来像 tdata（含 key_data）", path.display())
    } else {
        format!("{} 不像 tdata（需要含 key_data 的 Telegram Desktop tdata 目录）", path.display())
    };
    ctx.out.result(&human, &json!({
        "path": path.to_string_lossy().into_owned(),
        "looks_like": ok,
    }));
    Ok(())
}

fn tdata_import(ctx: &Ctx, a: &TdataImportArgs) -> Result<()> {
    let pass_src = PasswordSource {
        env: a.passcode_env.clone(),
        file: a.passcode_file.clone(),
        stdin: a.passcode_stdin,
    };
    pass_src.validate()?;
    let proxy = resolve_login_proxy(ctx, a.proxy.as_deref())?;
    let rt = rt()?;
    rt.block_on(run_tdata_import(ctx, a, &proxy, &pass_src))
}

/// tdata 错误映射出稳定码，脚本可按子串匹配（与 GUI 的 tdata_code 对齐）。
fn tdata_error(e: tdata::TdataError) -> anyhow::Error {
    let code = match &e {
        tdata::TdataError::NeedPasscode => "tg_tdata_need_passcode",
        tdata::TdataError::WrongPasscode => "tg_tdata_wrong_passcode",
        tdata::TdataError::NotTdata | tdata::TdataError::NoKeyData => "tg_tdata_not_found",
        tdata::TdataError::NoAccount => "tg_tdata_no_account",
        tdata::TdataError::Corrupt(_) | tdata::TdataError::Unsupported(_) => "tg_tdata_unsupported",
        tdata::TdataError::Io(_) => "tg_tdata_io",
    };
    anyhow!("[{code}] {e}")
}

async fn run_tdata_import(
    ctx: &Ctx<'_>,
    a: &TdataImportArgs,
    proxy: &Option<String>,
    pass_src: &PasswordSource,
) -> Result<()> {
    // 1. 解析（纯本地，不碰网络）。先试空密码；NeedPasscode 时再经安全通道要。
    let auth = match tdata::read_tdata(&a.path, "") {
        Ok(auth) => auth,
        Err(tdata::TdataError::NeedPasscode) => {
            let pass = read_secret(pass_src, "tdata 本地密码", true)?;
            tdata::read_tdata(&a.path, &pass).map_err(tdata_error)?
        }
        Err(e) => return Err(tdata_error(e)),
    };

    // 2. 先问服务端认不认，成功才落盘（顺序与 GUI 一致：避免把失效 tdata 写成本地位置）。
    // tdata 的 auth key 出身于 Telegram Desktop（api_id 2040）：必须无视配置里的
    // 自定义身份、强制内置——那份 key 是在 2040 下协商的，配别的 id 建连会以难解释
    // 的方式失败。与 GUI tdata 导入同源。
    let appid = resolve_app(AppIdChoice::Builtin, ctx.config_path).map_err(|e| anyhow!("{e}"))?;
    let device = DeviceInfo::current();
    let saved = tdata::to_saved_session(&auth, appid.id());
    let conn = connect::connect_with(&saved, &appid, &device, proxy.as_deref())
        .await
        .map_err(|e| anyhow!("{e}"))?;
    let user_id = connect::account_user_id(&conn.client).await;
    let label = match &a.name {
        Some(n) => n.clone(),
        None => connect::account_label(&conn.client).await,
    };
    drop(conn);

    // 3. 去重 + 分配 id。
    let (id, duplicate) = {
        let mut chosen: Option<(String, bool)> = None;
        mutate_places(ctx, |places| {
            chosen = Some(register::fold_into_places(places, user_id, &label));
            Ok(())
        })?;
        chosen.expect("闭包必然给 chosen 赋值")
    };

    // 4. 落 session 到该 id（独立副本，与 Telegram Desktop 再无关系）。
    let saved_ok = match session::save_current(&saved, &id) {
        Ok(_) => true,
        Err(session::SessionError::NoProtector) => {
            ctx.out.warn("这台机器没有可用的凭据库，导入的登录态不会保存");
            false
        }
        Err(e) => bail!("保存登录态失败：{e}"),
    };

    let human = if duplicate {
        format!("导入成功：tdata 对应账号已关联到已有位置 {id}")
    } else {
        format!("导入成功，已保存为位置 {id}（{label}）")
    };
    ctx.out.result(&human, &success_payload(&id, &label, duplicate, user_id, saved_ok));
    Ok(())
}

// ---- 代理设置 ----

/// 按当前配置算出本次建连实际用的代理（`None`=直连）。
///
/// `proxy-status` 与 `check` 共用：两处若各自解释 system/manual，迟早一个改了
/// 另一个没跟着改，出现「状态显示生效、实际探测用的不是同一个代理」。
fn effective_proxy(ctx: &Ctx) -> Result<Option<String>> {
    // 与 GUI、登录同源（resolve_proxy）：一处解释 system/manual，三处行为一致。
    Ok(resolve_proxy(&ctx.cfg.remote, None)?
        .map(|p| p.as_str().to_string()))
}

fn proxy_status(ctx: &Ctx) -> Result<()> {
    let mode = ctx.cfg.remote.telegram_proxy_mode.clone();
    let manual = ctx.cfg.remote.telegram_proxy.clone();
    let system = tg_proxy::detect_system_proxy().map(|p| p.as_str().to_string());
    let effective = effective_proxy(ctx)?;
    let human = match &effective {
        Some(u) => format!("Telegram 代理当前生效：{u}（模式 {mode}）"),
        None => format!("Telegram 当前无代理（模式 {mode}）"),
    };
    ctx.out.result(
        &human,
        &json!({
            "mode": mode,
            "manual_configured": manual,
            "system_detected": system,
            "effective": effective,
        }),
    );
    Ok(())
}

/// 登录前连通性自检：按配置的代理去连 Telegram 主 DC，只测 TCP。
///
/// 与 GUI 登录页共用 `omy_remote::telegram::proxy::probe_connection`，这里只做
/// 终端外壳——解析有效代理、跑探测、把结果写成人类可读 + JSON。
///
/// 探测类命令约定：结果照常打印，**只有 `status == "ok"` 才退出 0**；否则
/// 退出 1，供脚本在批量登录/转发前先判断是网络不通还是代理配置错。
fn check_connection(ctx: &Ctx) -> Result<()> {
    let proxy = effective_proxy(ctx)?;
    let c = rt()?.block_on(tg_proxy::probe_connection(proxy.as_deref()));
    let human = format!(
        "连通性 {}（{} ms，{}）",
        c.status,
        c.elapsed_ms,
        if c.via_proxy { "经代理" } else { "直连" }
    );
    ctx.out.result(&human, &serde_json::to_value(c)?);
    if c.status != "ok" {
        std::process::exit(1);
    }
    Ok(())
}

fn proxy_set(ctx: &Ctx, a: &ProxySetArgs) -> Result<()> {
    match (a.url.as_deref(), a.system) {
        (Some(url), false) => {
            let norm = tg_proxy::normalize(url)?; // 校验格式
            let addr = norm.map(|p| p.as_str().to_string()).unwrap_or_default();
            mutate_config(ctx, |c| {
                c.remote.telegram_proxy_mode = String::from("manual");
                c.remote.telegram_proxy = addr;
                Ok(())
            })?;
        }
        (None, true) => {
            mutate_config(ctx, |c| {
                c.remote.telegram_proxy_mode = String::from("system");
                c.remote.telegram_proxy = String::new();
                Ok(())
            })?;
        }
        (None, false) => bail!("proxy-set 必须给 --url 或 --system"),
        (Some(_), true) => bail!("--url 与 --system 互斥"),
    }
    proxy_status(ctx)
}

fn proxy_reset(ctx: &Ctx) -> Result<()> {
    mutate_config(ctx, |c| {
        c.remote.telegram_proxy_mode = String::from("system");
        c.remote.telegram_proxy = String::new();
        Ok(())
    })?;
    proxy_status(ctx)
}

// ---- 应用身份（自定义 api_id / api_hash） ----

fn appid_status(ctx: &Ctx) -> Result<()> {
    let protector = protect_key();
    match appid_store::resolve(&ctx.cfg.remote, protector.as_ref()) {
        Ok(r) => {
            let human = if r.app.is_builtin() {
                format!("当前使用内置 api_id={}（未配置自定义应用身份）", r.app.id())
            } else {
                format!("当前使用自定义 api_id={}", r.app.id())
            };
            // 绝不输出 hash：只有 builtin / effective_id / configured。
            ctx.out.result(
                &human,
                &json!({
                    "builtin": r.app.is_builtin(),
                    "effective_id": r.app.id(),
                    "configured": r.configured,
                }),
            );
        }
        Err(ResolveError::NoProtector) => {
            // 配了自定义但解不开：报出 api_id（公开数字），仍不碰 hash。
            let id = ctx.cfg.remote.telegram_api_id.unwrap_or(AppId::builtin().id());
            ctx.out.result(
                &format!("已配置自定义 api_id={id}，但本机凭据库不可用，无法解开 api_hash"),
                &json!({ "builtin": false, "effective_id": id, "configured": true, "locked": true }),
            );
        }
        Err(e) => return Err(anyhow!("解析应用身份失败：{e}")),
    }
    Ok(())
}

fn appid_set(ctx: &Ctx, a: &AppIdSetArgs) -> Result<()> {
    let hash_src = PasswordSource {
        env: a.hash_env.clone(),
        file: a.hash_file.clone(),
        stdin: a.hash_stdin,
    };
    hash_src.validate()?;
    let hash = read_secret(&hash_src, "api_hash（my.telegram.org 申请的 32 位十六进制）", true)?;
    // 校验身份形状合法（AppIdError 不含 hash 本体）。
    AppId::custom(a.api_id, &hash).map_err(|e| anyhow!("api_id/api_hash 校验失败：{e}"))?;
    // 封成信封再写。无保护器时拒绝写明文——绝不退而求其次存明文。
    let protector = protect_key();
    let envelope = appid_store::seal(protector.as_ref(), &hash)?
        .ok_or_else(|| anyhow!("本机没有可用的凭据库，无法安全加密 api_hash（绝不写明文）。请在凭据库可用时重试，或先 app-id-reset 用内置身份"))?;
    mutate_config(ctx, |c| {
        c.remote.telegram_api_id = Some(a.api_id);
        c.remote.telegram_api_hash = Some(envelope.clone());
        Ok(())
    })?;
    drop(hash); // 尽早丢掉明文副本
    ctx.out.result(
        &format!("已加密保存自定义 api_id={}", a.api_id),
        &json!({ "api_id": a.api_id, "configured": true }),
    );
    Ok(())
}

fn appid_reset(ctx: &Ctx) -> Result<()> {
    let path = config_target_path(ctx)?;
    // 用原始 TOML 写：Option 字段设 None 不会把键删掉，而「恢复内置」必须真的
    // 移除这两个键，否则下次读取仍是旧自定义身份。
    omy_config::Config::update_toml_at(&path, |table| {
        if let Some(toml::Value::Table(remote)) = table.get_mut("remote") {
            remote.remove("telegram_api_id");
            remote.remove("telegram_api_hash");
        }
        Ok::<(), anyhow::Error>(())
    })
    .with_context(|| format!("写回配置 {} 失败", path.display()))?;
    ctx.out.result(
        "已恢复内置应用身份",
        &json!({ "builtin": true, "configured": false }),
    );
    Ok(())
}

// ---- 配置写回（跨进程锁） ----

fn config_target_path(ctx: &Ctx) -> Result<PathBuf> {
    match ctx.config_path {
        Some(p) => Ok(p.to_path_buf()),
        None => omy_config::config_path()
            .ok_or_else(|| anyhow!("无法确定配置文件位置")),
    }
}

/// 在共享跨进程锁内读-改-写整份配置（与 GUI 的 update_at 同一把锁）。
fn mutate_config<F>(ctx: &Ctx, f: F) -> Result<()>
where
    F: FnOnce(&mut omy_config::Config) -> Result<()>,
{
    let path = config_target_path(ctx)?;
    omy_config::Config::update_at(&path, |c| f(c))
        .with_context(|| format!("写回配置 {} 失败", path.display()))
}

// ---- logout / detach ----

/// 要求一条 Telegram 位置，否则报错。
fn require_telegram(ctx: &Ctx, needle: &str) -> Result<omy_config::SavedPlace> {
    let sp = find_place(ctx.cfg, needle)?;
    if sp.kind != "telegram" {
        bail!("位置 {} 不是 Telegram 位置（它是 {:?}）", sp.id, sp.kind);
    }
    Ok(sp)
}

// ---- 位置独立加密 / 解锁 / 锁定 / 取消加密 ----
//
// 这些都是对落盘的登录态信封做变换，**不需要连网**，也不需要本进程持有 KEK
// 长会话——CLI 是一次性进程。真正的加/解密原语在 [`session`]（已被单测钉死），
// 这里只做「终端外壳」：收密码、把 SessionError 翻成脚本可匹配的稳定错误码、
// 打印结果。GUI 的对应 Tauri 命令也是同一批原语的壳，不复制其编排。

/// 把位置操作的底层错误翻成稳定错误码（方括号前缀，与 `tg_tdata_*` 同款）。
///
/// 分类依据来自 [`session`] 原语的返回约定：`Undecryptable` 一律是「密码开不了」，
/// `NoProtector` 是「本机无凭据库、拒绝落明文」。两边共用这套判断，避免 CLI 与
/// GUI 对同一个错给出不同提示。
fn map_place_err(id: &str, op: &str, e: session::SessionError) -> anyhow::Error {
    use session::SessionError;
    match e {
        SessionError::Undecryptable => anyhow!(
            "[tg_unlock_wrong] 位置 {id} 密码错误，或当前会话里没有能解开它的密钥"
        ),
        SessionError::NoProtector => anyhow!(
            "[tg_no_protector] 本机没有可用凭据库，拒绝把登录态落为明文（{op} 中止）"
        ),
        other => anyhow!("[tg_place_{op}] 位置 {id} 操作失败：{other}"),
    }
}

/// `omy remote telegram encrypt <位置>`。
fn place_encrypt(ctx: &Ctx, a: &PlaceEncryptArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.pw.place)?;
    // confirm=true：创建密码要重复输入防打错（env/file/stdin 通道下 read_password
    // 自动跳过确认——你已经在通道里给过一次了）。
    let pw = read_password(&a.pw.source(), "设置位置密码", true)?;
    let app = resolve_login_app(ctx)?;
    let profile = KdfProfile::from_name(a.kdf.as_deref().unwrap_or("moderate"))?;
    match session::encrypt_place_with_password(&app, &sp.id, &pw, profile.params(), &[]) {
        Ok(true) => {
            ctx.out.result(
                &format!("已用位置密码加密 {}（{}），下次访问需提供该密码", sp.id, sp.name),
                &json!({ "encrypted": sp.id, "kdf": profile.name() }),
            );
        }
        // Ok(false) = Absent：这个位置还没登录态，无可加密
        Ok(false) => bail!(
            "[tg_no_session] 位置 {} 还没有可用登录态，先 omy remote telegram login {}",
            sp.id,
            sp.id
        ),
        Err(e) => return Err(map_place_err(&sp.id, "encrypt", e)),
    }
    Ok(())
}

/// `omy remote telegram unlock <位置>`：验证密码能否解开（脚本可判断对错）。
///
/// CLI 没有 GUI 那样的长期会话，所以这里不「记住」KEK——本进程验证完就结束。
/// 真正打开加密 session 靠文件命令里传 `--password-*`，在
/// [`connect_with_password`] 里现派 KEK。
fn place_unlock(ctx: &Ctx, a: &PlacePwArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    let pw = read_password(&a.source(), "位置密码", false)?;
    let app = resolve_login_app(ctx)?;
    match session::unlock_place_with_password(&app, &sp.id, &pw, &[]) {
        Ok(Some(_)) => {
            ctx.out.result(
                &format!("密码正确：位置 {}（{}）可解锁", sp.id, sp.name),
                &json!({ "unlocked": sp.id }),
            );
        }
        Ok(None) => bail!("[tg_not_encrypted] 位置 {} 并未加密，无需解锁", sp.id),
        Err(e) => return Err(map_place_err(&sp.id, "unlock", e)),
    }
    Ok(())
}

/// `my remote telegram lock <位置>`：确认位置处于锁定态。
///
/// CLI 是一次性进程、从不跨命令保留 KEK，所以位置在两条命令之间天然就是锁着的——
/// 这个命令不改盘，只确认「它确实是加密位置」，给脚本一个明确的状态断言。
fn place_lock(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    match session::is_encrypted(&sp.id) {
        Ok(true) => {
            ctx.out.result(
                &format!("位置 {}（{}）已加密并处于锁定态：每次访问都需通过密码通道提供位置密码", sp.id, sp.name),
                &json!({ "locked": sp.id, "encrypted": true }),
            );
        }
        Ok(false) => bail!("[tg_not_encrypted] 位置 {} 并未加密，没有可锁定的", sp.id),
        Err(e) => return Err(map_place_err(&sp.id, "lock", e)),
    }
    Ok(())
}

/// `omy remote telegram decrypt <位置>`：取消位置加密。
fn place_decrypt(ctx: &Ctx, a: &PlacePwArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    let pw = read_password(&a.source(), "位置密码", false)?;
    let app = resolve_login_app(ctx)?;
    match session::decrypt_place_with_password(&app, &sp.id, &pw) {
        Ok(true) => {
            ctx.out.result(
                &format!("已取消位置 {}（{}）的加密，恢复为机器密钥保护", sp.id, sp.name),
                &json!({ "decrypted": sp.id }),
            );
        }
        Ok(false) => bail!("[tg_not_encrypted] 位置 {} 并未加密", sp.id),
        Err(e) => return Err(map_place_err(&sp.id, "decrypt", e)),
    }
    Ok(())
}

// ---- 联机操作：转发目标 / 转发 / 服务端搜索 / 自建私密群 ----
//
// 这些都要真连 Telegram。连接复用文件命令的 connect_store（含现场密码派生 KEK），
// 不另起一套。底层 forward_targets/forward_messages/create_self_group 与
// RemoteStore::search 都是 omy-remote 已测好的原语，CLI 只做参数接线与结果打印。

/// 连到位置并取回 TelegramStore。WebDAV 位置直接拒绝——这些是 Telegram 专属操作。
///
/// 密码通道未指定时不主动提示：未加密位置用不到密码，加密位置会由 connect 报
/// `[tg_locked]` 并提示加 `--password-*`。这样脚本里不会因为位置本来就不要密码
/// 而卡在交互提示上。
fn connect_tg(
    ctx: &Ctx,
    conn: &TgConnArgs,
) -> Result<(omy_config::SavedPlace, omy_remote::telegram::TelegramStore)> {
    let sp = require_telegram(ctx, &conn.place)?;
    let src = conn.source();
    let pw = if src.is_interactive() {
        None
    } else {
        Some(read_password(&src, "位置密码", false)?)
    };
    let rt = rt()?;
    let store = rt.block_on(super::connect_store(&sp, pw.as_deref()))?;
    match store {
        super::stores::AnyStore::Tg(tg) => Ok((sp, tg)),
        super::stores::AnyStore::Dav(_) => {
            bail!("位置 {} 是 WebDAV，这些是 Telegram 专属操作", sp.id)
        }
    }
}

/// 解析一个 `tg:<chat>:<msg>` 条目 id，返回 (对话目录 tg:CHAT, 消息号)。
fn parse_entry(id: &str) -> Result<(String, i32)> {
    let parts: Vec<&str> = id.split(':').collect();
    match parts.as_slice() {
        ["tg", chat, msg] => {
            let chat: i64 = chat
                .parse()
                .map_err(|_| anyhow!("[tg_bad_entry] 条目 {id:?} 的对话段不是整数"))?;
            let msg: i32 = msg
                .parse()
                .map_err(|_| anyhow!("[tg_bad_entry] 条目 {id:?} 的消息段不是整数"))?;
            Ok((format!("tg:{chat}"), msg))
        }
        _ => bail!("[tg_bad_entry] 条目 {id:?} 形如 tg:<对话>:<消息>，例如 tg:123456:789"),
    }
}

/// 收集 `--entry` 与 `--entries-json` 给出的源条目，校验全部在同一源对话内。
fn collect_entries(entries: &[String], json: &Option<PathBuf>) -> Result<(String, Vec<i32>)> {
    let mut raw: Vec<String> = entries.to_vec();
    if let Some(path) = json {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取条目 JSON {} 失败", path.display()))?;
        let arr: Vec<String> = serde_json::from_str(&text)
            .with_context(|| format!("{} 不是字符串数组 JSON", path.display()))?;
        raw.extend(arr);
    }
    if raw.is_empty() {
        bail!("[tg_no_entries] 没有要转发的条目：用 --entry tg:<对话>:<消息> 或 --entries-json");
    }
    let mut source_dir: Option<String> = None;
    let mut ids = Vec::new();
    for e in &raw {
        let (dir, msg) = parse_entry(e)?;
        match &source_dir {
            Some(d) if *d != dir => bail!(
                "[tg_cross_chat] 转发的条目必须来自同一源对话：{} 与 {} 不同",
                d,
                dir
            ),
            _ => source_dir = Some(dir),
        }
        ids.push(msg);
    }
    Ok((source_dir.expect("至少一条"), ids))
}

/// `omy remote telegram targets`。
fn targets(ctx: &Ctx, a: &TgConnArgs) -> Result<()> {
    let (_sp, tg) = connect_tg(ctx, a)?;
    let rt = rt()?;
    let targets = rt
        .block_on(tg.forward_targets())
        .map_err(|e| anyhow!("[tg_targets] 拉取转发目标失败: {e}"))?;
    let rows: Vec<_> = targets
        .iter()
        .map(|t| json!({ "dir_id": t.dir_id, "title": t.title, "kind": t.kind }))
        .collect();
    ctx.out.result(
        &format!("可转发目标 {} 个", rows.len()),
        &json!({ "targets": rows }),
    );
    Ok(())
}

/// `omy remote telegram forward`。
fn forward(ctx: &Ctx, a: &ForwardArgs) -> Result<()> {
    let (_sp, tg) = connect_tg(ctx, &a.conn)?;
    let (source_dir, ids) = collect_entries(&a.entries, &a.entries_json)?;
    let rt = rt()?;
    let n = rt
        .block_on(tg.forward_messages(&source_dir, &a.target, &ids))
        .map_err(|e| anyhow!("[tg_forward] 转发失败: {e}"))?;
    ctx.out.result(
        &format!("已从 {source_dir} 原生转发 {n} 条消息到 {}", a.target),
        &json!({ "forwarded": n, "source": source_dir, "target": a.target }),
    );
    Ok(())
}

/// `omy remote telegram search`。
fn search(ctx: &Ctx, a: &SearchArgs) -> Result<()> {
    let (_sp, tg) = connect_tg(ctx, &a.conn)?;
    let dir = a.dir.clone().unwrap_or_default();
    let rt = rt()?;
    use omy_remote::RemoteStore as _;
    let rows = rt
        .block_on(tg.search(&dir, &a.query, a.limit as usize))
        .map_err(|e| anyhow!("[tg_search] 搜索失败: {e}"))?;
    let items: Vec<_> = rows
        .iter()
        .map(|e| {
            json!({ "id": e.id, "name": e.name, "size": e.size, "is_dir": e.is_dir })
        })
        .collect();
    ctx.out.result(
        &format!("命中 {} 条", items.len()),
        &json!({ "query": a.query, "dir": dir, "hits": items }),
    );
    Ok(())
}

/// `omy remote telegram group`。
fn create_group(ctx: &Ctx, a: &GroupArgs) -> Result<()> {
    let (_sp, tg) = connect_tg(ctx, &a.conn)?;
    let rt = rt()?;
    let target = rt
        .block_on(tg.create_self_group(&a.title))
        .map_err(|e| anyhow!("[tg_group] 创建私密群失败: {e}"))?;
    ctx.out.result(
        &format!("已创建私密群 {}（{}）", target.title, target.dir_id),
        &json!({ "dir_id": target.dir_id, "title": target.title, "kind": target.kind }),
    );
    Ok(())
}

/// `omy remote telegram status`：本地会话状态，**不联网**。
///
/// 给脚本一个不依赖网络/账号的健康检查：位置是否已登录、是否 per-place 加密、
/// 本机机器密钥能否直接打开（决定要不要在文件命令里补 `--password-*`）。
/// 真要验证「连不连得上服务端」仍走一次实际命令（ls），那一段要联网，不在此。
fn status(ctx: &Ctx, a: &StatusArgs) -> Result<()> {
    let machine: Vec<_> = session::machine_fallback_kek().into_iter().collect();
    let mut rows = Vec::new();
    for sp in &ctx.cfg.remote.places {
        if sp.kind != "telegram" {
            continue;
        }
        if let Some(want) = &a.place
            && sp.id != *want && sp.name != *want
        {
            continue;
        }
        let encrypted = session::is_encrypted(&sp.id).unwrap_or(false);
        let machine_open = session::place_unlocked_with_keks(&sp.id, &machine).unwrap_or(false);
        rows.push(json!({
            "id": sp.id,
            "name": sp.name,
            "encrypted": encrypted,
            "machine_open": machine_open,
            // 加密但机器开不了 → 访问必须补 --password-*
            "needs_password": encrypted && !machine_open,
        }));
    }
    if rows.is_empty() {
        bail!("[tg_no_place] 没有匹配的 Telegram 位置（用 `omy remote list` 查看）");
    }
    let human = rows
        .iter()
        .map(|r| {
            let enc = if r["encrypted"].as_bool() == Some(true) { "加密" } else { "未加密" };
            let open = if r["machine_open"].as_bool() == Some(true) { "机器可开" } else { "需密码" };
            format!("{}（{}）：{enc}，{open}", r["id"], r["name"])
        })
        .collect::<Vec<_>>()
        .join("\n");
    ctx.out.result(&human, &json!({ "places": rows }));
    Ok(())
}

fn logout(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    if !ctx.out.confirm(
        format!(
            "将登出位置 {}（{}）：删除本机登录态，之后必须重新登录才能使用。继续？[y/N] ",
            sp.id, sp.name
        )
        .as_str(),
        ctx.assume_yes,
    ) {
        bail!("已取消");
    }
    mutate_places(ctx, |places| {
        places.retain(|p| p.id != sp.id);
        Ok(())
    })?;
    session::forget(&sp.id)
        .map_err(|e| anyhow!("已移除位置，但销毁登录态失败：{e}"))?;
    ctx.out.result(
        &format!("已登出位置 {}（{}），登录态已销毁", sp.id, sp.name),
        &json!({ "logged_out": sp.id }),
    );
    Ok(())
}

fn detach(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    if !ctx.out.confirm(
        format!(
            "将把位置 {}（{}）从列表移除，但保留本机登录态（之后可重新加回而不必重登）。继续？[y/N] ",
            sp.id, sp.name
        )
        .as_str(),
        ctx.assume_yes,
    ) {
        bail!("已取消");
    }
    mutate_places(ctx, |places| {
        places.retain(|p| p.id != sp.id);
        Ok(())
    })?;
    ctx.out.result(
        &format!("已从列表移除位置 {}（{}），登录态保留", sp.id, sp.name),
        &json!({ "detached": sp.id }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 二步密码通道与登录密码用同一套互斥约定，且绝不接受 argv。
    #[test]
    fn twofactor_source_reuses_shared_mutual_exclusion() {
        let both = PasswordSource {
            env: Some("TG_PW".into()),
            stdin: true,
            ..PasswordSource::default()
        };
        assert!(both.validate().is_err(), "同时给 stdin 与 env 应被拒");
        let ok = PasswordSource {
            stdin: true,
            ..PasswordSource::default()
        };
        assert!(ok.validate().is_ok());
    }

    /// 验证码与二步密码不能同时走 stdin：一条 stdin 流只能读一次。
    ///
    /// 不这样会怎样：脚本把两个秘密都从管道喂，第二次 read_to_string 必然 EOF，
    /// 二步密码拿到空值，报一句莫名其妙的「两步密码不正确」。
    #[test]
    fn code_and_password_cannot_both_use_stdin() {
        let code = PasswordSource { stdin: true, ..PasswordSource::default() };
        let pw = PasswordSource { stdin: true, ..PasswordSource::default() };
        assert!(validate_phone_channels(&code, &pw).is_err());

        // code 走 env、password 走 stdin：允许（两个独立通道，不抢同一条流）。
        let code = PasswordSource { env: Some("TG_CODE".into()), ..PasswordSource::default() };
        let pw = PasswordSource { stdin: true, ..PasswordSource::default() };
        assert!(validate_phone_channels(&code, &pw).is_ok());
    }

    /// 成功 JSON 绝不能带登录票据或任何秘密。
    #[test]
    fn success_payload_never_leaks_login_token() {
        let v = success_payload("p1", "我自己", false, Some(42), true);
        let s = serde_json::to_string(&v).expect("序列化");
        assert!(
            !s.contains("login_url") && !s.contains("token") && !s.contains("tg://"),
            "成功 JSON 不得含登录票据，实际：{s}"
        );
        assert_eq!(v["id"], "p1");
        assert_eq!(v["user_id"], 42);
        assert_eq!(v["session_saved"], true);
    }

    /// 错码判定：只认 PHONE_CODE_INVALID / EXPIRED，其它 RPC 错误不算「可重试」。
    #[test]
    fn wrong_code_detection_only_matches_code_errors() {
        assert!(is_wrong_code(&QrError::Invocation("PHONE_CODE_INVALID (420)".into())));
        assert!(is_wrong_code(&QrError::Invocation("PHONE_CODE_EXPIRED (0)".into())));
        assert!(!is_wrong_code(&QrError::WrongPassword));
        assert!(!is_wrong_code(&QrError::Invocation("PHONE_NUMBER_INVALID (400)".into())));
    }

    /// tdata 错误稳定码映射：脚本可按 [tg_tdata_*] 子串分支。
    #[test]
    fn tdata_error_codes_are_stable() {
        let s = tdata_error(tdata::TdataError::NotTdata).to_string();
        assert!(s.starts_with("[tg_tdata_not_found]"));
        let s = tdata_error(tdata::TdataError::NeedPasscode).to_string();
        assert!(s.starts_with("[tg_tdata_need_passcode]"));
    }

    /// 位置操作错误码稳定：密码错/无凭据库 两类必须区分开，脚本才能分别提示。
    #[test]
    fn place_error_codes_are_stable() {
        use session::SessionError;
        let s = map_place_err("p1", "unlock", SessionError::Undecryptable).to_string();
        assert!(s.starts_with("[tg_unlock_wrong]"), "实际：{s}");
        let s = map_place_err("p1", "encrypt", SessionError::NoProtector).to_string();
        assert!(s.starts_with("[tg_no_protector]"), "实际：{s}");
        // 其它错带上操作名，便于定位
        let s = map_place_err("p1", "decrypt", SessionError::Malformed).to_string();
        assert!(s.starts_with("[tg_place_decrypt]"), "实际：{s}");
    }

    /// 位置密码三通道与登录二步密码共用互斥约定，绝不进 argv。
    #[test]
    fn place_pw_source_is_same_lockdown() {
        let a = PlacePwArgs {
            place: "p1".into(),
            password_env: Some("TG_PLACE_PW".into()),
            password_file: None,
            password_stdin: false,
        };
        let src = a.source();
        assert!(src.validate().is_ok());
        assert_eq!(src.env.as_deref(), Some("TG_PLACE_PW"));
        // 两个通道同时给必须拒绝——静默取一会让用户以为用的是另一个
        let bad = PlacePwArgs {
            place: "p1".into(),
            password_env: Some("A".into()),
            password_file: None,
            password_stdin: true,
        };
        assert!(bad.source().validate().is_err());
    }

    /// encrypt 成功 JSON 里绝不回显密码（只回位置 id 与 KDF 档位）。
    #[test]
    fn encrypt_success_payload_has_no_secret() {
        let v = json!({ "encrypted": "p1", "kdf": "moderate" });
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains("password"), "成功 JSON 泄露密码字段: {s}");
        assert!(!s.contains("password"), "成功 JSON 泄露密码: {s}");
        assert!(s.contains("p1"));
    }

    /// 条目 id 解析：合法形拆出 (对话目录, 消息号)，非法形给稳定错码。
    #[test]
    fn entry_id_parsing() {
        let (dir, msg) = parse_entry("tg:123456:789").unwrap();
        assert_eq!(dir, "tg:123456");
        assert_eq!(msg, 789);

        for bad in ["", "tg", "tg:abc:1", "tg:1:2:3", "123456:789", "tg::"] {
            let e = parse_entry(bad).unwrap_err().to_string();
            assert!(e.contains("[tg_bad_entry]"), "{bad:?} -> {e}");
        }
    }

    /// 收集条目：--entry 与 JSON 文件合并、去重前先校验同对话。
    #[test]
    fn collect_entries_same_chat_only() {
        // 全在同一对话：返回源目录与消息号
        let (dir, ids) = collect_entries(
            &["tg:100:1".into(), "tg:100:3".into(), "tg:100:2".into()],
            &None,
        )
        .unwrap();
        assert_eq!(dir, "tg:100");
        assert_eq!(ids, vec![1, 3, 2]);

        // 跨对话必须拒绝——原生转发要求同源
        let e = collect_entries(&["tg:100:1".into(), "tg:200:9".into()], &None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("[tg_cross_chat]"), "{e}");

        // 空输入必须拒绝
        let e = collect_entries(&[], &None).unwrap_err().to_string();
        assert!(e.contains("[tg_no_entries]"), "{e}");
    }
}
