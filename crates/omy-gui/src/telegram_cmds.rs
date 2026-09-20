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
use omy_remote::telegram::store::TelegramStore;
use omy_remote::telegram::{connect, proxy, session as tgsession, tdata};

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
        /// 供排查的具体原因（服务端错误名 + 代码）。
        ///
        /// 只在归不进上面那些具体分类时才有值。**不含服务端的 message**——
        /// 那里面可能回显请求参数，而扫码请求的参数里有 api_hash。
        ///
        /// 没有它的时候界面只能显示一句「登录失败」，排查时完全不知道服务端
        /// 到底回了什么，只能靠猜。
        detail: Option<String>,
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
    // 归不进具体分类的才带上原文。已分类的那些自己的文案就说清了出路，
    // 再附一串错误名只会让界面变吵
    let detail = match e {
        QrError::Invocation(d) => Some(d.clone()),
        _ => None,
    };
    LoginPhase::Failed {
        code: String::from(code),
        wait_secs: wait,
        detail,
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
    ///
    /// 类型是 **Tauri 的** `JoinHandle` 而不是 tokio 的：见 `spawn` 那处的注释，
    /// 用 `tokio::spawn` 会在没有 reactor 的线程上 panic 并带走整个进程。
    handle: tauri::async_runtime::JoinHandle<()>,
    /// 用户输入的云密码往这里送。
    ///
    /// 做成通道而不是让前端再调一个命令去碰 `QrSession`：`QrSession` 归后台
    /// 任务独占，两边都能拿到它就要加一把锁，而那把锁会在等更新时被一直持有。
    password_tx: tokio::sync::mpsc::UnboundedSender<String>,
    /// 后台任务是否已经结束。
    ///
    /// Tauri 的 `JoinHandle` 不提供 `is_finished`，而「还忙不忙」必须问得到
    /// ——问不到就只能一直当成忙，用户登录失败之后再也点不动「开始扫码」。
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl LoginTask {
    /// 新建一个空闲任务。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 有没有正在进行的登录。
    ///
    /// 看的是 `done` 这个标志而不是句柄——Tauri 的 `JoinHandle` 没有
    /// `is_finished`。标志由后台任务自己在退出前置位，所以「任务跑完了但
    /// 句柄还挂着」不会被误判成忙。
    #[must_use]
    pub fn is_busy(&self) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|r| !r.done.load(std::sync::atomic::Ordering::SeqCst))
            })
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
                // abort 只是请求取消，任务可能还没真的停下。立刻置位是对的：
                // 这个标志回答的是「还该不该拦下一次登录」，而取消之后显然
                // 不该再拦——否则用户取消完立刻重开会被拒绝
                r.done.store(true, std::sync::atomic::Ordering::SeqCst);
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
                old.done.store(true, std::sync::atomic::Ordering::SeqCst);
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

/// 某个**位置**有没有可用的登录态。
///
/// 「有存档」不等于「能免登录」：一份没有 auth key 的存档看着一切正常，载入后
/// 却仍是未登录。所以这里判的是 auth key 在不在，而不是文件在不在。
///
/// # 为什么必须带 place_id
///
/// 多账号之后「有没有登录态」不再是一个全局问题。不带位置去问的话，只要有
/// **任意一个**账号登录过就会答「有」——那正是用户报的「点添加它也展示已登录」：
/// 界面拿一个全局答案去决定新位置该显示什么，于是第二个账号永远走不到登录页。
#[tauri::command]
#[must_use]
pub fn telegram_has_session(place_id: String) -> bool {
    let app = AppId::builtin();
    matches!(tgsession::load(&app, &place_id), Ok(Some(s)) if tgsession::has_auth_key(&s))
}

/// 「从列表移除」：只删位置配置，**保留 session 文件**。
///
/// # 为什么与「删除账号」分成两个命令
///
/// 两者的后果差得很远：这一条之后用户随时能把账号加回来、不必重新扫码；
/// 另一条则意味着下次要重新扫码或重新导入 tdata。做成一个带 flag 的命令，
/// 调用方少传一个参数就会静默走到破坏性更强的那一侧，而那个错误**不可撤销**。
///
/// 所以宁可多一个命令：命令名本身就说清了会发生什么。
///
/// # Errors
///
/// 不返回错误：位置本来就不在也算达成目的（用户要的是「它别在列表里」）。
#[tauri::command]
pub fn telegram_place_detach(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
    place_id: String,
) {
    reg.remove(&place_id);
    // 立刻落盘，否则删掉的位置重启后又回来了
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存远程位置失败：{e}");
    }
}

/// 「删除账号（含本地登录态）」：位置配置与 session 文件一起删掉。
///
/// session 文件按 `omy-secret` 的既有做法**先覆写再删**（见
/// `telegram::session::forget`），不是简单的 remove_file。
///
/// # 这一步不可撤销
///
/// 删完之后再想用这个账号，必须重新扫码或重新导入 tdata。界面上必须与
/// 「从列表移除」明确区分开，并且要让用户看得出差别——这也正是后端把它们
/// 分成两个命令的原因。
///
/// # 顺序：先摘位置，再删文件
///
/// 反过来的话，删文件与摘位置之间若出了岔子（进程被杀），会留下一个
/// 指向不存在 session 的位置——用户点进去只会看到一句「还没有登录」，
/// 而他明明记得自己登录过。
///
/// # Errors
///
/// session 文件删除失败时返回。位置**已经摘掉**，所以这个错误的含义是
/// 「列表里没有了，但本机可能还留着登录态」——必须如实说，不能吞掉。
#[tauri::command]
pub fn telegram_place_delete_account(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
    place_id: String,
) -> CmdResult<()> {
    reg.remove(&place_id);
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存远程位置失败：{e}");
    }
    tgsession::forget(&place_id)
        .map_err(|e| CmdError::with("tg_forget_failed", detail(&e.to_string())))
}

/// tdata 导入的前提探测结果。
///
/// 一次把四条前提的状态都给前端，而不是让它连问四个命令——
/// 分开问的话，界面会出现「一条一条陆续变绿」的闪烁，
/// 而用户根本不需要看到这个过程。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TdataProbe {
    /// Telegram Desktop 是否正在运行。
    ///
    /// 它运行时会占着 tdata。这条必须**指名道姓**地告诉用户去托盘退出，
    /// 而不是把一个文件锁错误原样抛出来——那会让人去查磁盘、查权限。
    pub client_running: bool,
    /// 自动找到的 tdata 候选路径。
    ///
    /// **空是常态而不是错误**：便携版跟着 exe 走、可以在任意盘，
    /// 自动发现必然落空。前端此时要**就地给路径选择器**，
    /// 而不是显示「未检测到 Telegram Desktop」——那等于把
    /// 「需要你补一个信息」显示成「不支持」，用户看到就走了。
    pub candidates: Vec<String>,
}

/// 探测 tdata 导入的前提。
///
/// 不读任何加密内容，只看进程与目录结构，所以很快、可以随时调。
#[tauri::command]
#[must_use]
pub fn telegram_tdata_probe() -> TdataProbe {
    TdataProbe {
        client_running: telegram_desktop_running(),
        candidates: tdata::common_locations()
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
    }
}

/// Telegram Desktop 在运行吗。
///
/// # 为什么不去试着锁文件
///
/// 「能不能读」与「客户端在不在跑」是两回事：客户端运行时我们**仍然读得到**
/// 那些文件（我们只读副本），但它可能正在写、拿到的是半截状态。
/// 所以判据是进程在不在，不是文件能不能打开。
fn telegram_desktop_running() -> bool {
    // tasklist 是 Windows 自带的，不需要额外依赖。
    // 失败时返回 false 而不是 true：探测不出来就别拦着用户，
    // 真有冲突后面解析会失败，那时的错误更具体
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Telegram.exe", "/NH"])
            .output();
        match out {
            Ok(o) => String::from_utf8_lossy(&o.stdout).contains("Telegram.exe"),
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 这个路径像 tdata 吗（给前端做即时反馈）。
///
/// 选错目录时当场说，而不是等用户填完密码、点了导入、跑完一轮解密才报错。
#[tauri::command]
#[must_use]
pub fn telegram_tdata_check(path: String) -> bool {
    tdata::looks_like_tdata(std::path::Path::new(&path))
}

/// 从 tdata 导入登录态，并注册成远程位置。
///
/// # 顺序：先验证服务端认不认，认了再落盘
///
/// 反过来的话，一份已经失效的 tdata 会把当前可用的 session 覆盖掉——
/// 用户为了省一次扫码，反而把已有的登录弄丢了。
///
/// # `NeedPasscode` 是正常分支，不是失败
///
/// 有没有设本地密码**事先无法预知**（`key_datas` 在不在都一样，
/// 没设密码时它也存在，只是用空密码加密）。所以只能先试空密码，
/// 解不开再向用户要——前端据这个错误码弹出输入框。
///
/// # Errors
///
/// 解析失败、需要/错误的本地密码、服务端不认这份登录态、网络不通时返回，
/// 各自有不同的错误码。**「解析失败」与「登录态已失效」必须分开**：
/// 前者要改用扫码，后者说明桌面端那边也已经登出了。
#[tauri::command]
pub async fn telegram_tdata_import(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
    path: String,
    passcode: Option<String>,
    proxy_url: Option<String>,
) -> CmdResult<String> {
    let dir = std::path::PathBuf::from(&path);
    let pass = passcode.unwrap_or_default();

    // 解析。注意这一步**不碰网络**，失败就是格式或密码问题
    let auth = tokio::task::spawn_blocking(move || tdata::read_tdata(&dir, &pass))
        .await
        .map_err(|e| CmdError::with("tg_tdata_failed", detail(&e.to_string())))?
        .map_err(|e| CmdError::with(tdata_code(&e), detail(&e.to_string())))?;

    let proxy = match proxy::normalize(proxy_url.as_deref().unwrap_or_default()) {
        Ok(p) => p.map(|p| p.to_string()),
        Err(e) => return Err(CmdError::with("tg_bad_proxy", detail(&e.to_string()))),
    };

    let app = AppId::builtin();
    let device = DeviceInfo::current();
    let saved = tdata::to_saved_session(&auth, app.id());

    // 真的问一句服务端认不认。
    //
    // 不问的话，一份几个月前、早已被注销的 tdata 也会「导入成功」，
    // 错误要等到用户点开某个对话才冒出来，而那时的报错指向那次操作、
    // 不是这次导入——诊断方向完全跑偏。
    let conn = connect::connect_with(&saved, &app, &device, proxy.as_deref())
        .await
        .map_err(|e| CmdError::with(e.code(), detail(&e.to_string())))?;

    // 默认位置名取服务端昵称；取不到就回落，不让它影响导入本身
    let label = connect::account_label(&conn.client).await;

    // **不再摘掉已有的 Telegram 位置。**
    //
    // 早先这里有一句「已有占位就先摘掉，否则侧栏会出现两个 Telegram」，
    // 那基于「只会有一个账号」的前提。多账号之后两个位置正是**想要**的结果，
    // 摘掉反而会让用户导入第二个账号时第一个凭空消失。
    let store = TelegramStore::from_connection(conn.client, conn.runner);
    let id = reg
        .add_telegram(label, store, proxy.clone())
        .map_err(|e| CmdError::with("tg_register_failed", detail(&e.to_string())))?;

    // 拿到位置 id 之后才落 session：文件名正是按它命名的。
    //
    // 顺序不能反——先落盘就得先编一个 id，那会让「session 文件叫什么」
    // 有两个来源，迟早对不上，而现象是「导入成功但重启后要重新登录」。
    //
    // 这一步也让导入的账号**独立持有自己的 session 副本**：此后它与
    // Telegram Desktop 再无关系，桌面端退出登录或删掉 tdata 都不影响它。
    if let Err(e) = tgsession::save_current(&saved, &id) {
        // 存不住不该让整件事失败——本次会话里它是好的。
        // 但要说出来，否则用户会以为下次还在
        eprintln!("[omy] 保存 Telegram 登录态失败：{e}");
    }

    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存 Telegram 位置失败：{e}");
    }
    Ok(id)
}


/// 把 tdata 的错误映射成前端能分支的错误码。
///
/// **每一种都要有自己的码**，因为界面要做的事完全不同：
/// 要密码的弹输入框、密码错的提示重输、解析失败的引导去扫码。
/// 合并成一个码的话，前端只能显示一句笼统的话，
/// 而用户不知道下一步该干什么。
fn tdata_code(e: &tdata::TdataError) -> &'static str {
    match e {
        tdata::TdataError::NeedPasscode => "tg_tdata_need_passcode",
        tdata::TdataError::WrongPasscode => "tg_tdata_wrong_passcode",
        tdata::TdataError::NotTdata | tdata::TdataError::NoKeyData => "tg_tdata_not_found",
        tdata::TdataError::NoAccount => "tg_tdata_no_account",
        tdata::TdataError::Corrupt(_) | tdata::TdataError::Unsupported(_) => "tg_tdata_unsupported",
        tdata::TdataError::Io(_) => "tg_tdata_io",
    }
}

/// 把**刚扫码登录成功**的那个账号注册成一个新的远程位置，返回位置 id。
///
/// # 为什么连接发生在这里而不是注册表里
///
/// 建连要走网络、可能几十秒才超时。放进 `PlaceRegistry::add_*` 会让一个
/// 看起来是「登记一条记录」的同步操作变成能卡住界面的操作；放在命令层，
/// 前端可以照常显示进行中状态。
///
/// # 每次调用都新建一个位置，**不再复用已有的**
///
/// 早先这里会先查「有没有 Telegram 位置」，有就直接返回那个 id。那基于
/// 「一个 omy 只持有一份登录态」的前提，而它正是用户报的
/// 「点添加它也展示已登录」的根因：第二个账号刚要添加就被换成了第一个。
///
/// 现在每次都建新位置。想避免把**同一个账号**加两遍，应当由调用方在登录
/// 之前判断，而不是在这里把所有 Telegram 位置当成同一个。
///
/// # Errors
///
/// 没有登录态、登录态已失效、网络不通时返回，各自有不同的错误码，
/// 前端据此决定是引导去扫码还是提示检查网络。
#[tauri::command]
pub async fn telegram_place_connect(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
    proxy_url: Option<String>,
) -> CmdResult<String> {
    let proxy = match proxy::normalize(proxy_url.as_deref().unwrap_or_default()) {
        Ok(p) => p.map(|p| p.to_string()),
        Err(e) => return Err(CmdError::with("tg_bad_proxy", detail(&e.to_string()))),
    };

    let app = AppId::builtin();
    let device = DeviceInfo::current();
    // 扫码那条路把 session 落在 PENDING_ACCOUNT 下（那时还没有位置 id），
    // 所以这里从它读回来
    let conn = connect::connect_saved(&app, &device, proxy.as_deref(), tgsession::PENDING_ACCOUNT)
        .await
        .map_err(|e| CmdError::with(e.code(), detail(&e.to_string())))?;

    // 默认位置名取服务端昵称，让两个账号在侧栏上一眼能分辨
    let label = connect::account_label(&conn.client).await;

    // 把 runner 一并交给 store 接管。
    //
    // 不能只取 client 然后让 conn 离开作用域：runner 被丢弃后所有请求都会排进
    // 队列、再也发不出去，而且不报错——表现是界面一直转圈。交给 store 之后
    // 这条约束由类型系统保证，不靠谁记得。
    let store = TelegramStore::from_connection(conn.client, conn.runner);

    let id = reg
        .add_telegram(label, store, proxy.clone())
        .map_err(|e| CmdError::with("tg_register_failed", detail(&e.to_string())))?;

    // 把待收编的那份 session 改名成这个位置自己的。
    //
    // 不做这一步的话，下次启动按位置 id 去找必然落空，用户要重新扫码——
    // 而 pending 那份还躺在磁盘上，成了谁也不认领的孤儿。
    if let Err(e) = tgsession::adopt_pending(&id) {
        eprintln!("[omy] 收编 Telegram 登录态失败：{e}");
    }

    // 落盘，否则「加过的位置重启就没了」而且不报错。
    //
    // 失败只记日志不报错：位置在本次会话里是好的，为了一个「下次还在不在」
    // 让本次直接失败不划算，而用户此刻要的是先能用。
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存 Telegram 位置失败：{e}");
    }
    Ok(id)
}

/// 给一个远程位置改名。
///
/// **只改本地显示名，不碰服务端。** Telegram 上的昵称是账号自己的属性；
/// 用户在 omy 里给位置起的名字只是本地标签，真去改服务端 profile
/// 远超「给这个位置起个名」的预期。
///
/// # Errors
///
/// 位置不存在时返回——静默失败会让用户以为改好了，刷新后发现没变、
/// 然后反复去试。
#[tauri::command]
pub fn telegram_place_rename(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
    place_id: String,
    name: String,
) -> CmdResult<()> {
    // 空名字会让侧栏出现一个点不到、也看不出是什么的条目
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CmdError::code("remote_name_empty"));
    }
    if !reg.rename(&place_id, String::from(trimmed)) {
        return Err(CmdError::code("remote_no_such_place"));
    }
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存远程位置失败：{e}");
    }
    Ok(())
}

/// 确保某个 Telegram 位置已连上；已经连上就什么都不做。
///
/// 供 `remote_browse` 之类在真正用它之前调用——用户点「进入」是想看里面的
/// 东西，不是想诊断连接状态。让他先看到一句「还没连上」再自己去找哪里能连，
/// 是把实现细节摊给用户。
///
/// # Errors
///
/// 登录态失效或网络不通时返回，错误码与 `telegram_place_connect` 一致。
pub async fn ensure_connected(
    reg: &Arc<crate::places::PlaceRegistry>,
    place_id: &str,
) -> CmdResult<()> {
    let Some(p) = reg.get(place_id) else {
        return Ok(()); // 位置不存在由调用方自己报，这里不越俎代庖
    };
    if p.kind != "telegram" || telegram_store_connected(&p.store) {
        return Ok(());
    }
    // 用这个位置自己存着的代理重连。本机直连 Telegram 数据中心是超时的，
    // 丢了代理就只能等超时——而超时很久，用户只看到界面卡住
    let proxy = p.proxy.clone();
    let app = AppId::builtin();
    let device = DeviceInfo::current();
    // **按这个位置自己的 session 重连。** 多账号下这一步不能含糊：
    // 读错文件的表现是用户点开 A 账号却看到 B 账号的对话列表，
    // 而两边都不会报错
    let conn = connect::connect_saved(&app, &device, proxy.as_deref(), place_id)
        .await
        .map_err(|e| CmdError::with(e.code(), detail(&e.to_string())))?;
    let store = TelegramStore::from_connection(conn.client, conn.runner);
    // 换掉占位：先摘再加，否则会并存两条
    reg.remove(place_id);
    reg.add_telegram_with_id(
        String::from(place_id),
        p.name.clone(),
        store,
        proxy,
    );
    Ok(())
}

/// 这个位置是不是一个**已连接**的 Telegram。
///
/// 从配置恢复出来的 Telegram 位置是未连接占位（restore 不碰网络），
/// 要能和真连上的区分开。
fn telegram_store_connected(store: &omy_remote::PlaceStore) -> bool {
    match store {
        omy_remote::PlaceStore::Telegram(t) => t.is_connected(),
        omy_remote::PlaceStore::WebDav(_) => false,
    }
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
/// # 为什么这个命令必须是 `async`
///
/// 它内部要 `tokio::spawn` 起后台登录任务。**同步的 Tauri 命令跑在主线程上，
/// 那里没有 tokio reactor**，`spawn` 会 panic「there is no reactor running」。
///
/// 而这个 panic 是在 WebView 的 C++ 回调里冒出来的（`extern "C"` 边界不能
/// unwind），于是立刻升级成 `panic in a function that cannot unwind` 并带走
/// 整个进程——现象是点了「开始扫码」之后应用直接消失，前端只看到一句
/// 「连接断开」。这是端到端实测撞到的，不是理论风险。
///
/// 标成 `async` 之后 Tauri 会把它派到自己的 tokio 运行时上跑，`spawn` 就有
/// reactor 了。**不要因为「反正函数体里没有 await」把它改回同步。**
///
/// # Errors
///
/// 已有登录进行中、或代理地址不合法时返回。**代理不合法是配置错误，不是网络
/// 故障**：正确动作是改地址而不是重试，所以要在这里当场拒绝并说清楚。
#[tauri::command]
pub async fn telegram_login_start(
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
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_for_task = Arc::clone(&done);
    // **必须用 tauri::async_runtime::spawn，不能用 tokio::spawn。**
    //
    // tokio::spawn 要求调用线程上有 reactor。Tauri 命令并不保证这一点，
    // 而撞上时不是返回一个错误，是 panic——且那个 panic 发生在 WebView 的
    // C++ 回调栈里，`extern "C"` 不能 unwind，于是升级成
    // 「panic in a function that cannot unwind」直接带走进程。
    // 实测现象：点「开始扫码」后应用消失，前端只看到「连接断开」。
    //
    // Tauri 的 spawn 自己持有运行时句柄，从哪个线程调都行。
    let handle = tauri::async_runtime::spawn(async move {
        run_login(&emitter, proxy, password_rx).await;
        // 在这里置位而不是靠句柄查：Tauri 的 JoinHandle 没有 is_finished，
        // 不置位的话这个任务会被永远当成「还在跑」，用户登录失败后
        // 再也点不动「开始扫码」
        done_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    task.set(Running {
        handle,
        password_tx,
        done,
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
    // 失败额外打一条日志：界面上那句话是给用户看的，而排查需要错误名。
    // 这里不含凭据——detail 只有服务端错误名与代码，见 map_err
    if let LoginPhase::Failed { code, detail, .. } = phase {
        eprintln!(
            "[omy] Telegram 登录失败：{code}{}",
            detail.as_ref().map_or(String::new(), |d| format!(" / {d}"))
        );
    }
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
                            detail: None,
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
                        detail: None,
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
    // 而重新登录要再扫一次码、还可能撞 FLOOD_WAIT。
    //
    // 落在 PENDING_ACCOUNT 下而不是某个位置 id 下：此刻位置还不存在
    // （建位置要先建连，而建连正是下一步 telegram_place_connect 做的事）。
    // 等位置建好后由 adopt_pending 改名过去。
    //
    // 不能为了「有个 id」就在这里先编一个：那会让位置 id 有两个来源，
    // 迟早对不上，而现象是「登录成功但重启后还要再登一次」。
    let saved = match tgsession::save(sess.session(), appid, tgsession::PENDING_ACCOUNT) {
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
                detail: None,
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
            LoginPhase::Failed {
                code, wait_secs, ..
            } => Some((code.clone(), *wait_secs)),
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
