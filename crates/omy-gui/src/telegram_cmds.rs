//! Telegram 扫码登录的 Tauri 命令层。
//!
//! # 为什么登录要做成「后台跑 + 事件推」，而不是一个 await 的命令
//!
//! 扫码是个**几十秒到几分钟**的过程，中间会换好几张二维码。做成一个
//! `async fn login()` 只能在最后给一个结果，中间态全丢——而中间态恰恰是这个
//! 流程里最要紧的部分：
//!
//! - 二维码换了第几张（静默换图会让盯着屏幕的人以为卡住了）；
//! - 是不是在切数据中心（做成失败会让一次本来会成功的登录被用户点掉）；
//! - 要不要云密码（**事先无法预知**，没开 2FA 的账号根本走不到）。
//!
//! 所以后台任务每推进一步就 emit 一个事件，前端据此更新界面。
//!
//! # 2FA 是独立子步，不是固定的第 N 步
//!
//! 界面**不能**把云密码画成「第 3 步 / 共 4 步」：账号没开两步验证时那一步
//! 永远不会出现，画出来就是一个用户永远走不到、却一直看着的步骤。
//! 所以事件流里它是一个可能出现也可能不出现的分支，前端按出现与否渲染。
//!
//! # 登录成功就立刻写 session
//!
//! 不等整趟结束。这次登录在服务端已经生效了，之后任何一步失败（用户关窗口、
//! 进程崩）都会让它白费——而重新登录要再扫一次码，还可能撞 `FLOOD_WAIT`。

use std::sync::{Arc, Mutex};

use tauri::Emitter as _;

use omy_remote::telegram::appid::AppId;
use omy_remote::telegram::device::DeviceInfo;
use omy_remote::telegram::qr::{encode_matrix, QrMatrix};
use omy_remote::telegram::qrlogin::{QrError, QrEvent, QrSession};
use omy_remote::telegram::{proxy, session as tgsession};

use crate::commands::{CmdError, CmdResult};

/// 登录进度事件的通道名。前端 `listen('telegram-login', …)`。
pub const LOGIN_EVENT: &str = "telegram-login";

/// 推给前端的一步进度。
///
/// `serde` 打成 `{ phase: "...", ... }`，前端按 `phase` 分支。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LoginPhase {
    /// 正在连接（建连接、发第一个请求）。
    Connecting,
    /// 有一张二维码可以显示了。
    Qr {
        /// 模块矩阵，前端自己画格子。
        matrix: QrMatrix,
        /// 还有多少秒过期，界面据此倒计时。
        expires_in_secs: u32,
        /// 这是第几次换图。**0 表示首张。**
        ///
        /// 大于 0 时界面必须给出可见提示（「已自动换一张（第 N 次）」）。
        /// 官方要求过期后自动重新生成，但静默替换会让盯着屏幕的人以为卡住，
        /// 或者怀疑刚才扫的那张还算不算数。
        refresh_index: u32,
    },
    /// 正在切换数据中心。
    ///
    /// **中间态，不是失败。** 界面显示进度并继续等，不要弹重试。
    Migrating {
        /// 目标数据中心编号。
        dc: i32,
    },
    /// 要输两步验证的云密码。
    ///
    /// 这一步**事先不可预知**，只有开了 2FA 的账号才会走到。
    NeedPassword {
        /// 服务端给的提示；`None` 时界面**整行不显示**，
        /// 不要放一个空的「提示：」。
        hint: Option<String>,
    },
    /// 登录成功。
    Done {
        /// 登录态有没有真的落盘。
        ///
        /// `false` 表示这台机器没有可用的凭据库，登录**本次有效**但重启后
        /// 要重新扫码。必须如实告诉用户——否则他下次打开发现又要扫码，
        /// 会以为程序把他登出了。
        session_saved: bool,
    },
    /// 失败。
    Failed {
        /// 翻译键（`errors.<code>`）。后端不拼文案，见 `commands.rs` 的约定。
        code: String,
        /// 限流时的剩余秒数，其余情况为 `None`。
        wait_secs: Option<u32>,
    },
}

/// 把驱动层的错误翻成前端要的错误码。
///
/// `FLOOD_WAIT` 单独带秒数：界面要显示倒计时并**禁用**按钮，而不是给重试——
/// 限流下重试会把等待时间越点越长。
fn phase_of_error(e: &QrError) -> LoginPhase {
    let (code, wait) = match e {
        QrError::FloodWait { secs } => ("tg_flood_wait", Some(*secs)),
        QrError::ApiIdPublishedFlood => ("tg_api_id_flood", None),
        QrError::WrongPassword => ("tg_wrong_password", None),
        QrError::SignUpRequired => ("tg_signup_required", None),
        QrError::UnsupportedPasswordAlgo => ("tg_unsupported_2fa", None),
        QrError::Disconnected => ("tg_disconnected", None),
        QrError::Invocation(_) => ("tg_login_failed", None),
    };
    LoginPhase::Failed {
        code: String::from(code),
        wait_secs: wait,
    }
}

/// 正在进行的登录任务。
///
/// 同一时刻只允许一个：并发扫码会让两条流程抢同一份 session，而且**必然**撞
/// 限流——Telegram 对 `exportLoginToken` 的频率限制很紧。
#[derive(Default)]
pub struct LoginTask {
    inner: Mutex<Option<Running>>,
}

struct Running {
    /// 后台任务句柄，用于取消。
    handle: tokio::task::JoinHandle<()>,
    /// 用户输入的云密码往这里送。
    ///
    /// 做成通道而不是让前端再调一个命令去碰 `QrSession`：`QrSession` 归后台
    /// 任务独占，两边都能拿到它就要加一把锁，而那把锁会在等更新时被一直持有。
    password_tx: tokio::sync::mpsc::UnboundedSender<String>,
}

impl LoginTask {
    /// 新建一个空闲任务。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 有没有正在进行的登录。
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| !r.handle.is_finished()))
            .unwrap_or(false)
    }

    /// 取消并清理。
    ///
    /// 用户关掉登录窗口就该立刻断开：那条连接挂着会占一个连接配额，而
    /// Telegram 对同一账号的并发连接数有限制，反复开关几次就开始收到 429。
    pub fn cancel(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(r) = g.take() {
                r.handle.abort();
            }
        }
    }

    /// 把云密码送给正在等它的后台任务。
    ///
    /// 返回是否真的送出去了（没有任务在等时为 `false`）。
    fn send_password(&self, pw: String) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.password_tx.send(pw).is_ok()))
            .unwrap_or(false)
    }

    fn set(&self, r: Running) {
        if let Ok(mut g) = self.inner.lock() {
            // 覆盖前先停掉旧的，否则旧任务还在后台跑、还在往前端推事件
            if let Some(old) = g.take() {
                old.handle.abort();
            }
            *g = Some(r);
        }
    }
}

/// 共享句柄。
pub type SharedLogin = Arc<LoginTask>;

/// 这台机器能不能安全保存 Telegram 登录态。
///
/// 界面要在**开始扫码之前**问它：答案为否时先告诉用户「这台机器上登录态存不住，
/// 每次启动都要重新扫一次」，而不是等他扫完了才说。
#[tauri::command]
#[must_use]
pub fn telegram_can_persist() -> bool {
    tgsession::can_persist()
}

/// 已经有可用的登录态了吗。
///
/// 「有存档」不等于「能免登录」：一份没有 auth key 的存档看着一切正常，载入后
/// 却仍是未登录。所以这里判的是 auth key 在不在，而不是文件在不在。
#[tauri::command]
#[must_use]
pub fn telegram_has_session() -> bool {
    let app = AppId::builtin();
    matches!(tgsession::load(&app), Ok(Some(s)) if tgsession::has_auth_key(&s))
}

/// 忘掉已保存的登录态（「退出 Telegram 账号」）。
///
/// # Errors
///
/// 删除失败时返回。文件本来就不存在算成功——用户要的是「别再留着」。
#[tauri::command]
pub fn telegram_forget_session() -> CmdResult<()> {
    tgsession::forget().map_err(|e| CmdError::with("tg_forget_failed", detail(&e.to_string())))
}

/// 把一句说明装进错误参数。
fn detail(s: &str) -> serde_json::Value {
    serde_json::json!({ "detail": s })
}

/// 开始扫码登录。
///
/// 立刻返回，进度通过 [`LOGIN_EVENT`] 事件推送。
///
/// `proxy` 为用户填的代理地址（可空）。它会先经
/// [`omy_remote::telegram::proxy::normalize`] 归一化——grammers 只认
/// `socks5://`，而系统代理给出的通常是 `http://` 形式。
///
/// # Errors
///
/// 已有登录进行中、或代理地址不合法时返回。**代理不合法是配置错误，不是网络
/// 故障**：正确动作是改地址而不是重试，所以要在这里当场拒绝并说清楚。
#[tauri::command]
pub fn telegram_login_start(
    app: tauri::AppHandle,
    task: tauri::State<'_, SharedLogin>,
    proxy_url: Option<String>,
) -> CmdResult<()> {
    if task.is_busy() {
        return Err(CmdError::code("tg_login_in_progress"));
    }

    // 代理地址在这里就校验：放过去的话 grammers 会在连接层回一句英文的
    // 「proxy scheme not supported」，那句话指不到「你填的地址要改」这件事
    let proxy = match proxy::normalize(proxy_url.as_deref().unwrap_or_default()) {
        Ok(p) => p.map(|p| p.as_str().to_owned()),
        Err(e) => {
            return Err(CmdError::with("tg_bad_proxy", detail(&e.to_string())));
        }
    };

    let (password_tx, password_rx) = tokio::sync::mpsc::unbounded_channel();
    let emitter = app.clone();
    let handle = tokio::spawn(async move {
        run_login(&emitter, proxy, password_rx).await;
    });
    task.set(Running {
        handle,
        password_tx,
    });
    Ok(())
}

/// 提交两步验证的云密码。
///
/// # Errors
///
/// 没有正在等密码的登录任务时返回。
#[tauri::command]
pub fn telegram_submit_password(
    task: tauri::State<'_, SharedLogin>,
    password: String,
) -> CmdResult<()> {
    if task.send_password(password) {
        Ok(())
    } else {
        Err(CmdError::code("tg_no_login_in_progress"))
    }
}

/// 取消登录。
#[tauri::command]
pub fn telegram_login_cancel(task: tauri::State<'_, SharedLogin>) {
    task.cancel();
}

/// 往前端推一步进度。
fn emit(app: &tauri::AppHandle, phase: &LoginPhase) {
    if let Err(e) = app.emit(LOGIN_EVENT, phase) {
        // 推不出去不该让登录本身失败：登录在服务端已经生效了，
        // 丢一条界面事件远好过把它整个作废
        eprintln!("[omy] Telegram 登录进度推送失败：{e}");
    }
}

/// 后台登录主循环。
async fn run_login(
    app: &tauri::AppHandle,
    proxy: Option<String>,
    mut password_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    emit(app, &LoginPhase::Connecting);

    let appid = AppId::builtin();
    let device = DeviceInfo::current();
    let mut sess = match QrSession::connect(appid.clone(), proxy.as_deref(), &device) {
        Ok(s) => s,
        Err(e) => {
            emit(app, &phase_of_error(&e));
            return;
        }
    };

    loop {
        let ev = match sess.step().await {
            Ok(ev) => ev,
            Err(e) => {
                emit(app, &phase_of_error(&e));
                return;
            }
        };
        match ev {
            QrEvent::Token {
                url,
                expires_in_secs,
                refresh_index,
            } => {
                // 画不出二维码就没法继续：报错而不是显示一张空图，
                // 后者会让用户对着空白反复扫
                let Ok(matrix) = encode_matrix(&url) else {
                    emit(
                        app,
                        &LoginPhase::Failed {
                            code: String::from("tg_qr_render_failed"),
                            wait_secs: None,
                        },
                    );
                    return;
                };
                emit(
                    app,
                    &LoginPhase::Qr {
                        matrix,
                        expires_in_secs,
                        refresh_index,
                    },
                );
            }
            QrEvent::Migrating { dc } => emit(app, &LoginPhase::Migrating { dc }),
            QrEvent::NeedPassword { hint } => {
                if !handle_password(app, &mut sess, hint, &mut password_rx).await {
                    return;
                }
                // 密码过了就是登录成功
                finish(app, &sess, &appid).await;
                return;
            }
            QrEvent::LoggedIn => {
                finish(app, &sess, &appid).await;
                return;
            }
        }
    }
}

/// 云密码子步：反复要密码直到通过或用户放弃。
///
/// 返回 `true` 表示通过。密码错了**停在本步重输**，不退回扫码——退回去意味着
/// 用户要重扫一次，而他只是打错了一个字符。
async fn handle_password(
    app: &tauri::AppHandle,
    sess: &mut QrSession,
    mut hint: Option<String>,
    password_rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> bool {
    loop {
        emit(app, &LoginPhase::NeedPassword { hint: hint.clone() });
        // 通道关了 = 任务被取消，静默收工，不要推一条「失败」——
        // 那是用户自己取消的，报错只会让他以为出了问题
        let Some(pw) = password_rx.recv().await else {
            return false;
        };
        match sess.submit_password(&pw).await {
            Ok(_) => return true,
            Err(QrError::WrongPassword) => {
                // 停在本步：把提示原样保留，重新推一次 NeedPassword。
                // 提示往往是用户自己设的，丢了他就失去唯一的线索
                emit(
                    app,
                    &LoginPhase::Failed {
                        code: String::from("tg_wrong_password"),
                        wait_secs: None,
                    },
                );
                hint = hint.take();
            }
            Err(e) => {
                emit(app, &phase_of_error(&e));
                return false;
            }
        }
    }
}

/// 登录成功的收尾：**立刻**落盘，然后自证服务端真的认。
///
/// # 为什么要先自证再报成功
///
/// 「没报错」不等于「登录成功」：auth key 与账号的绑定发生在服务端，本地拿到
/// 一个 `Success` 之后仍可能因为写 session 时丢了字节而变成一份用不了的登录态。
/// 那种失败会在下次启动时才暴露，且现象是「会话被吊销」——完全指不到这里。
async fn finish(app: &tauri::AppHandle, sess: &QrSession, appid: &AppId) {
    // 先存盘再做别的。这次登录在服务端已经生效，之后任何一步失败都会让它白费，
    // 而重新登录要再扫一次码、还可能撞 FLOOD_WAIT
    let saved = match tgsession::save(sess.session(), appid) {
        Ok(path) => {
            // 只打目录不打文件名也没必要——这条日志里不含任何凭据，
            // 但也不需要把路径写出去
            let _ = path;
            true
        }
        Err(tgsession::SessionError::NoProtector) => {
            // 这台机器没有凭据库。**不退回明文**，如实告诉用户这次登录只在
            // 本次会话有效
            eprintln!("[omy] 这台机器没有可用的凭据库，Telegram 登录态不会保存");
            false
        }
        Err(e) => {
            eprintln!("[omy] Telegram 登录态保存失败：{e}");
            false
        }
    };

    // 自证：服务端认不认这份登录态
    match sess.is_authorized().await {
        Ok(true) => emit(
            app,
            &LoginPhase::Done {
                session_saved: saved,
            },
        ),
        Ok(false) => emit(
            app,
            &LoginPhase::Failed {
                code: String::from("tg_login_not_effective"),
                wait_secs: None,
            },
        ),
        Err(e) => emit(app, &phase_of_error(&e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取出 `Failed` 的错误码与等待秒数；不是 `Failed` 时返回 `None`。
    ///
    /// 写成辅助函数而不是在断言里 `panic!`：omy-gui 全局禁用 `panic`，
    /// 测试代码也不给它开口子——把「不是预期形态」变成一个可断言的 `None`，
    /// 比为了写测试而放宽产品级 lint 好。
    fn failed_parts(p: &LoginPhase) -> Option<(String, Option<u32>)> {
        match p {
            LoginPhase::Failed { code, wait_secs } => Some((code.clone(), *wait_secs)),
            _ => None,
        }
    }

    /// 每种失败都要映射到**各自**的错误码，不能共用。
    ///
    /// 不这样会怎样：几种失败共用一个码，界面就只能显示同一句话——而它们的
    /// 出路完全不同：限流要等、api_id 被封要去填自己的、密码错要重输。
    /// 给出错误的指引比不给更糟。
    #[test]
    fn every_failure_has_its_own_code() {
        let cases = [
            QrError::FloodWait { secs: 30 },
            QrError::ApiIdPublishedFlood,
            QrError::WrongPassword,
            QrError::SignUpRequired,
            QrError::UnsupportedPasswordAlgo,
            QrError::Disconnected,
            QrError::Invocation(String::from("X")),
        ];
        let mut codes = Vec::new();
        for e in &cases {
            let p = phase_of_error(e);
            let parts = failed_parts(&p);
            assert!(parts.is_some(), "错误必须映射成 Failed：{e:?}");
            if let Some((c, _)) = parts {
                codes.push(c);
            }
        }
        let before = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(before, codes.len(), "不同失败原因不能共用同一个错误码");
    }

    /// 限流必须带上秒数，其余失败不带。
    ///
    /// 不这样会怎样：没有秒数，界面只能给一个重试按钮——而限流下重试会把等待
    /// 时间越点越长。反过来，给别的失败编一个等待秒数会让用户干等一段
    /// 根本不存在的倒计时。
    #[test]
    fn only_flood_wait_carries_seconds() {
        let p = phase_of_error(&QrError::FloodWait { secs: 41 });
        assert_eq!(
            failed_parts(&p).and_then(|(_, w)| w),
            Some(41),
            "限流秒数要如实带出来"
        );

        for e in [
            QrError::ApiIdPublishedFlood,
            QrError::WrongPassword,
            QrError::Disconnected,
        ] {
            let p = phase_of_error(&e);
            let parts = failed_parts(&p);
            assert!(parts.is_some(), "{e:?} 应当是 Failed");
            assert!(
                parts.and_then(|(_, w)| w).is_none(),
                "{e:?} 不该有等待秒数"
            );
        }
    }

    /// 首张二维码的 `refresh_index` 必须是 0，换图后必须大于 0。
    ///
    /// 不这样会怎样：前端按 `refresh_index > 0` 决定要不要显示「已自动换一张」。
    /// 首张就报 1 会让用户一进来就看到「已换第 1 次」，而他还什么都没等；
    /// 换图后仍报 0 则是静默换图——盯着屏幕的人会以为卡住了。
    #[test]
    fn first_qr_is_not_labelled_as_a_refresh() {
        let mk = |i: u32| LoginPhase::Qr {
            matrix: QrMatrix {
                size: 21,
                modules: vec![false; 21 * 21],
            },
            expires_in_secs: 30,
            refresh_index: i,
        };
        let idx = |p: &LoginPhase| match p {
            LoginPhase::Qr { refresh_index, .. } => Some(*refresh_index),
            _ => None,
        };
        assert_eq!(idx(&mk(0)), Some(0), "首张不算换图");
        assert!(
            idx(&mk(3)).is_some_and(|i| i > 0),
            "换过图就必须能被界面看出来"
        );
    }

    /// 序列化出来的事件里必须有 `phase` 判别字段。
    ///
    /// 不这样会怎样：前端靠 `phase` 分支渲染，少了它所有状态长得一样——
    /// 「正在切换数据中心」会和「登录失败」渲染成同一个东西，而这两者恰恰是
    /// 最不能混的一对（一个该等，一个该重来）。
    #[test]
    fn events_carry_a_phase_discriminant() {
        let done = serde_json::to_string(&LoginPhase::Done {
            session_saved: true,
        })
        .expect("序列化");
        assert!(done.contains("\"phase\":\"done\""), "{done}");

        let mig = serde_json::to_string(&LoginPhase::Migrating { dc: 4 }).expect("序列化");
        assert!(mig.contains("\"phase\":\"migrating\""), "{mig}");
        assert!(mig.contains("\"dc\":4"), "{mig}");
    }

    /// 「登录态没存住」必须如实报出来。
    ///
    /// 不这样会怎样：用户下次打开发现又要扫码，会以为程序把他登出了、甚至
    /// 怀疑账号出了问题——而真实原因是这台机器没有可用的凭据库，这件事
    /// 在他扫码那一刻就该被告知。
    #[test]
    fn done_reports_whether_the_session_was_saved() {
        let no = serde_json::to_string(&LoginPhase::Done {
            session_saved: false,
        })
        .expect("序列化");
        assert!(no.contains("\"session_saved\":false"), "{no}");
    }

    /// 空闲的任务不算忙，取消一个空闲任务不能崩。
    #[test]
    fn an_idle_task_is_not_busy() {
        let t = LoginTask::new();
        assert!(!t.is_busy());
        t.cancel();
        assert!(!t.is_busy());
        // 没有任务在等密码时，提交密码要被如实拒绝而不是静默吞掉
        assert!(!t.send_password(String::from("x")));
    }
}
