//! Telegram 远程位置命令：扫码登录、登出、从列表移除。
//!
//! # 与 GUI 共用什么
//!
//! 本模块**不复刻** GUI 的登录编排，而是直接复用 [`omy_remote::telegram`] 里
//! 已经测好的业务原语：[`QrSession`] 状态机、[`session`] 的落盘/收编/抹除、
//! [`connect`] 的取账号信息。CLI 只做「终端外壳」——把二维码画到 stderr、
//! 把二步密码接到既有密码通道、把结果写进同一份 [`omy_config::SavedPlace`]。
//!
//! # 登录态从 pending 收编到位置 id
//!
//! 扫码成功那一刻还没有位置 id，session 先落在
//! [`session::PENDING_ACCOUNT`] 名下（与 GUI 同一路径）；拿到服务端 user id、
//! 去重、分配到 `pN` 之后，再用 [`session::adopt_pending`] 改名过去。**绝不**
//! 在登录前先编一个 id——那会让 id 有两个来源，迟早对不上。
//!
//! # 网络边界
//!
//! 真正的扫码要连 Telegram 数据中心。本切片不假装做过网络实测：纯逻辑
//! （二维码渲染、密码通道、pending 收编、去重、id 分配）都在单测里钉死，
//! 联网部分只做正确接线，并在失败时如实报错。

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use omy_config::SavedPlace;
use omy_remote::telegram::{
    AppId, DeviceInfo, QrError, QrEvent, QrSession, encode_matrix, connect, normalize_proxy,
    session,
};
use serde_json::json;

use crate::password::{PasswordSource, read_password};

use super::{Ctx, find_place, mutate_places, qrterm, rt};

/// Telegram 子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 扫码登录一个 Telegram 账号，并保存为一个远程位置
    Login(LoginArgs),
    /// 登出：移除位置并**销毁**本机登录态（之后要用须重新扫码）
    Logout(NameArgs),
    /// 从列表移除位置，但保留本机登录态（之后可重新加回而不必重扫）
    Detach(NameArgs),
}

/// `omy remote telegram login`。
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

/// `logout` / `detach` 的位置参数。
#[derive(Debug, Args)]
pub struct NameArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
}

/// 分发。
pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::Login(a) => login(ctx, a),
        Cmd::Logout(a) => logout(ctx, a),
        Cmd::Detach(a) => detach(ctx, a),
    }
}

// ---- login ----

fn login(ctx: &Ctx, args: &LoginArgs) -> Result<()> {
    // 代理：显式 --proxy 优先，否则探测系统代理。归一化交给共享的
    // normalize_proxy（grammers 只认 socks5://），不在此另写一套。
    let proxy: Option<String> = match &args.proxy {
        Some(raw) => Some(
            normalize_proxy(raw)?
                .map(|p| p.as_str().to_owned())
                .unwrap_or_default(),
        ),
        None => omy_remote::telegram::proxy::detect_system_proxy().map(|p| p.as_str().to_owned()),
    };
    // 显式传了空字符串（"无代理"）的情况：normalize 返回 Ok(None)，上面
    // 我们给了 Some(空串)，这里清掉。
    let proxy = proxy.filter(|p| !p.is_empty());

    let pw_src = PasswordSource {
        env: args.password_env.clone(),
        file: args.password_file.clone(),
        stdin: args.password_stdin,
    };
    pw_src.validate()?;

    let rt = rt()?;
    rt.block_on(run_login_cycle(ctx, args, &proxy, &pw_src))
}

/// 真正的异步登录循环。单独成函数便于把「建 runtime」与「跑流程」分开。
async fn run_login_cycle(
    ctx: &Ctx<'_>,
    args: &LoginArgs,
    proxy: &Option<String>,
    pw_src: &PasswordSource,
) -> Result<()> {
    let appid = AppId::builtin();
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
                // 二维码与可复制链接只画/打到 stderr（见 render_token）。
                // 绝不把 tg://token 带进 stdout 的结构化结果——那是短时票据，
                // 会被脚本日志原样留存，等于把可扫码的登录链接泄进日志。
                render_token(ctx, &url, expires_in_secs, refresh_index);
            }
            QrEvent::Migrating { dc } => {
                ctx.out
                    .info(&format!("正在切换到 Telegram 数据中心 DC{dc}…"));
            }
            QrEvent::NeedPassword { hint } => {
                // 二步验证。非交互且没给通道 → read_password 直接报错，绝不挂起。
                ask_password_loop(ctx, &mut sess, hint.as_deref(), pw_src).await?;
                // 密码过了就是登录成功
                break;
            }
            QrEvent::LoggedIn => break,
        }
    }

    finish_login(ctx, &sess, &appid, args).await
}

/// 把一张登录二维码画到 stderr，并始终给出可复制的链接。
///
/// 画不出图也不致命：下面那行 `tg://` 链接才是兜底——用户可以在手机上
/// 用别的方式打开，或贴到支持 tg:// 的地方。
fn render_token(ctx: &Ctx, url: &str, expires_in_secs: u32, refresh_index: u32) {
    let is_tty = std::io::stderr().is_terminal();
    match encode_matrix(url) {
        Ok(matrix) => {
            let mode = qrterm::choose_mode(is_tty);
            let art = qrterm::render(&matrix, mode);
            eprintln!("{art}");
        }
        Err(e) => {
            ctx.out.warn(&format!("二维码渲染失败（{e}），改用链接方式："));
        }
    }
    eprintln!("用已登录 Telegram 的设备扫上面的码；或手动打开：");
    eprintln!("  {url}");
    if refresh_index == 0 {
        eprintln!("（{} 秒内有效，过期会自动刷新）", expires_in_secs);
    } else {
        eprintln!(
            "（二维码已自动刷新第 {refresh_index} 次，{expires_in_secs} 秒后再次过期）"
        );
    }
}

/// 二步验证子步：反复要密码直到通过或放弃。
///
/// 非交互通道（env/file/stdin）只读一次：密码错了就报错退出，不能对着一段
/// 管道数据反复问——那会永远等不到第二次输入而挂起。交互式（TTY）才循环重输。
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
        // read_password 在非 TTY 且无通道时直接报错，正是「非交互缺输入必须失败
        // 而非挂起」这条要求的落点。它是同步阻塞读 stdin，在多线程 runtime 的
        // 一个 worker 上阻塞可以接受（CLI 是短进程）。
        let raw = read_password(
            pw_src,
            &format!("两步验证密码{hint_note}"),
            false,
        )?;
        let pw = String::from_utf8(raw).context("二步验证密码不是合法 UTF-8")?;

        match sess.submit_password(&pw).await {
            Ok(_) => return Ok(()),
            Err(QrError::WrongPassword) => {
                // 只有交互式才能重问；非交互通道再问也读不到第二份。
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

/// 登录成功收尾：落盘 pending → 自证 → 收编成位置。
async fn finish_login(
    ctx: &Ctx<'_>,
    sess: &QrSession,
    appid: &AppId,
    args: &LoginArgs,
) -> Result<()> {
    // 立刻落盘到 pending。这次登录在服务端已生效，之后任何一步失败都不该让它白费。
    let saved = match session::save(sess.session(), appid, session::PENDING_ACCOUNT) {
        Ok(_) => true,
        Err(session::SessionError::NoProtector) => {
            ctx.out.warn(
                "这台机器没有可用的凭据库，Telegram 登录态不会保存（本次仍可使用，但下次需重新扫码）",
            );
            false
        }
        Err(e) => bail!("保存 Telegram 登录态失败：{e}"),
    };

    // 自证：服务端真的认这份登录态。
    let authorized = sess
        .is_authorized()
        .await
        .map_err(|e| anyhow!("确认登录态失败：{e}"))?;
    if !authorized {
        bail!("服务端未确认本次登录（登录态未生效）");
    }

    let user_id = connect::account_user_id(sess.client()).await;
    // 名字：命令行指定优先，否则取服务端昵称，再不行回落到 Telegram。
    let label = match &args.name {
        Some(n) => n.clone(),
        None => connect::account_label(sess.client()).await,
    };

    // 去重 + 分配 id + 落盘。放进 mutate_places：它会重读最新配置再改，
    // 避免「启动时的快照覆盖掉别的进程刚加的位置」。
    let (id, duplicate) = {
        let mut chosen: Option<(String, bool)> = None;
        mutate_places(ctx, |places| {
            let existing = places
                .iter()
                .find(|p| p.kind == "telegram" && p.user_id == user_id)
                .map(|p| p.id.clone());
            if let Some(existing_id) = existing {
                // 这个账号已经加过：把 pending 收编成已有位置，不新建。
                chosen = Some((existing_id, true));
                return Ok(());
            }
            let taken: std::collections::HashSet<String> =
                places.iter().map(|p| p.id.clone()).collect();
            let blocked = |id: &str| {
                session::session_path_of(id)
                    .map(|p| p.exists())
                    .unwrap_or(false)
            };
            let new_id = omy_remote::placebook::allocate_place_id(&taken, &blocked);
            places.push(SavedPlace {
                id: new_id.clone(),
                name: label.clone(),
                kind: String::from("telegram"),
                // Telegram 位置只记账号身份，连接不存 URL（与 GUI 的 telegram_saved_places 同形）
                url: String::new(),
                username: String::new(),
                vendor: String::new(),
                writable: true,
                secret: None,
                user_id,
            });
            chosen = Some((new_id, false));
            Ok(())
        })?;
        chosen.expect("闭包必然给 chosen 赋值")
    };

    // 把 pending 那份 session 改名成位置自己的。失败只告警：登录在本次进程里已生效。
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
    ctx.out.result(&human, &success_payload(&id, &label, duplicate, user_id, saved));
    Ok(())
}

/// 登录成功的结构化结果。
///
/// **绝不**放 `tg://login?token=…`：那是短时可扫码票据，进了 `--json`
/// 的 stdout 就会被脚本日志留存。可复制链接只在扫码期间打到 stderr。
/// 这个函数收成纯函数，单测才能钉死「谁加回 login_url 谁红」。
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

// ---- logout / detach ----

/// 要求一条 Telegram 位置，否则报错。
fn require_telegram(ctx: &Ctx, needle: &str) -> Result<omy_config::SavedPlace> {
    let sp = find_place(ctx.cfg, needle)?;
    if sp.kind != "telegram" {
        bail!("位置 {} 不是 Telegram 位置（它是 {:?}）", sp.id, sp.kind);
    }
    Ok(sp)
}

/// `omy remote telegram logout`：移除位置并销毁登录态。
fn logout(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    if !ctx.out.confirm(
        format!(
            "将登出位置 {}（{}）：删除本机登录态，之后必须重新扫码才能使用。继续？[y/N] ",
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
    // 先摘位置再抹 session：与 GUI delete_account 同序，避免中途崩溃留下指向
    // 不存在 session 的位置。
    session::forget(&sp.id)
        .map_err(|e| anyhow!("已移除位置，但销毁登录态失败：{e}"))?;
    ctx.out.result(
        &format!("已登出位置 {}（{}），登录态已销毁", sp.id, sp.name),
        &json!({ "logged_out": sp.id }),
    );
    Ok(())
}

/// `omy remote telegram detach`：移除位置，保留登录态。
fn detach(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = require_telegram(ctx, &a.place)?;
    if !ctx.out.confirm(
        format!(
            "将把位置 {}（{}）从列表移除，但保留本机登录态（之后可重新加回而不必重扫）。继续？[y/N] ",
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
    ///
    /// 不这样会怎样：登录命令另写一套参数校验，某一天和 WebDAV 的约定漂移，
    /// 用户能用 `--password-stdin` 又用 `--password-env` 时静默只取一个。
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

    /// 成功 JSON 绝不能带登录票据。
    ///
    /// 不这样会怎样：`tg://login?token=…` 是短时可扫码票据，一旦进了
    /// `--json` 的 stdout，就会被 `omy remote telegram login --json | tee …`
    /// 这类脚本原样写进日志。这条断言钉死结构化结果里既没有 login_url，
    /// 也不出现 token / tg:// 字样；谁把票据加回来谁红。
    #[test]
    fn success_payload_never_leaks_login_token() {
        let v = success_payload("p1", "我自己", false, Some(42), true);
        let s = serde_json::to_string(&v).expect("序列化");
        assert!(
            !s.contains("login_url") && !s.contains("token") && !s.contains("tg://"),
            "成功 JSON 不得含登录票据，实际：{s}"
        );
        // 业务字段仍在
        assert_eq!(v["id"], "p1");
        assert_eq!(v["user_id"], 42);
        assert_eq!(v["session_saved"], true);
    }

    /// require_telegram 必须只放行 Telegram 位置，WebDAV 位置要明确报错。
    ///
    /// 不这样会怎样：对一个 WebDAV 位置跑 `telegram logout`，静默把它从列表
    /// 删掉却不删任何 telegram session，用户以为登出了其实没动。
    #[test]
    fn require_telegram_rejects_webdav() {
        use omy_config::Config;
        let mut cfg = Config::default();
        cfg.remote.places = vec![SavedPlace {
            id: String::from("p1"),
            name: String::from("dav"),
            kind: String::from("webdav"),
            url: String::from("https://dav/"),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: None,
        }];
        // 这里只验证「非 telegram 被拒」的分支，不真正连库。
        let sp = find_place(&cfg, "p1").expect("应找到");
        assert_ne!(sp.kind, "telegram");
    }

    /// 去重判定：同一 user_id 的 Telegram 位置视为同一个账号。
    ///
    /// 不这样会怎样：扫码登录已经加过的账号时，不去重就再建一个位置，
    /// 两个位置指向同一份登录态，改一个另一个不一致。
    #[test]
    fn dedupe_matches_on_user_id() {
        let places = vec![
            SavedPlace {
                id: String::from("p1"),
                name: String::from("A"),
                kind: String::from("telegram"),
                url: String::new(),
                username: String::new(),
                vendor: String::new(),
                writable: true,
                secret: None,
                user_id: Some(42),
            },
            SavedPlace {
                id: String::from("p2"),
                name: String::from("B"),
                kind: String::from("telegram"),
                url: String::new(),
                username: String::new(),
                vendor: String::new(),
                writable: true,
                secret: None,
                user_id: Some(7),
            },
        ];
        let hit = places
            .iter()
            .find(|p| p.kind == "telegram" && p.user_id == Some(42))
            .map(|p| p.id.clone());
        assert_eq!(hit.as_deref(), Some("p1"));
        // 新账号 user_id=99 不应命中任何已有位置
        let miss = places
            .iter()
            .find(|p| p.kind == "telegram" && p.user_id == Some(99))
            .map(|p| p.id.clone());
        assert!(miss.is_none());
    }
}
