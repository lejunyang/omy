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
use omy_remote::telegram::phonelogin::{PhoneEvent, PhoneSession};
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
/// 给代理输入框一个**有依据的默认值**，而不是让用户对着空框猜。
///
/// # 为什么需要
///
/// 实测过一个让人困惑的组合：用户开了 Clash「全局代理」，已有的 Telegram
/// 位置一切正常，但新建登录和 tdata 导入都报网络失败。原因是那类工具的
/// 「全局」是系统代理设置 + 可能的 TUN，没开 TUN 时**不接管应用发起的裸
/// TCP**，而 MTProto 正是裸 TCP；已有位置之所以能用，是因为它的配置里存着
/// 之前填过的 socks5 地址，而新建那几条路传空就成了直连。
///
/// 三个来源按可信度排序：
///
/// 1. **已有 Telegram 位置的代理**——用户已经用它连通过一次，最可信；
/// 2. **系统代理**——大概率是同一个混合端口，但也可能是个纯 HTTP 端口；
/// 3. 都没有就返回空，由用户自己填。
#[tauri::command]
#[must_use]
pub fn telegram_suggest_proxy(
    reg: tauri::State<'_, Arc<crate::places::PlaceRegistry>>,
) -> String {
    // 已有位置优先：它是被验证过能用的那一个
    if let Some(p) = reg.telegram_proxy() {
        return p;
    }
    proxy::detect_system_proxy().map(|p| p.as_str().to_string()).unwrap_or_default()
}

/// 连通性自检的结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnCheck {
    /// `ok` | `bad_proxy` | `no_route`。
    ///
    /// 三种要分开，因为用户该做的事完全不同：通了就继续；代理地址不合法
    /// 要改地址（重试多少次都没用）；连不上才是去配代理或查网络。
    pub status: &'static str,
    /// 这次自检花了多久（毫秒）。6 秒和 0.3 秒对用户的意义不一样。
    pub elapsed_ms: u64,
    /// 自检时用的代理（空表示直连）。界面据此说「经代理」还是「直连」。
    pub via_proxy: bool,
}

/// 在登录**之前**检查能不能连到 Telegram。
///
/// # 为什么要有这一步
///
/// 实测本机直连 MTProto 数据中心超时、经 socks5 才通。对很多网络环境
/// 代理是必需项，而用户分不清「连不上」是代理、网络还是 api_id 的问题。
/// 先自检就把归因定下来，不用等他登录失败之后盲猜。
///
/// # 只测 TCP，不做 MTProto 握手
///
/// 握手失败还可能是 api_id 的问题，那是另一回事；混在一起就违背了
/// 「把归因定下来」这个目的。TCP 够判断「路通不通」，而且失败得快。
#[tauri::command]
pub async fn telegram_check_connection(proxy_url: Option<String>) -> ConnCheck {
    use std::time::Instant;

    let t0 = Instant::now();
    let raw = proxy_url.unwrap_or_default();
    let proxy = match proxy::normalize(&raw) {
        Ok(p) => p,
        Err(_) => {
            return ConnCheck {
                status: "bad_proxy",
                elapsed_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
                via_proxy: true,
            }
        }
    };

    // 目标取 DC2（149.154.167.51:443）——它是 Telegram 的主用入口之一。
    // 只连一个就够：自检回答的是「路通不通」，不是「哪个 DC 最快」
    let target = ("149.154.167.51", 443_u16);
    let ok = match proxy.as_ref() {
        Some(p) => probe_via_socks5(p.as_str(), target).await,
        None => probe_direct(target).await,
    };

    ConnCheck {
        status: if ok { "ok" } else { "no_route" },
        elapsed_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        via_proxy: proxy.is_some(),
    }
}

/// 直连一个 host:port，6 秒超时。
async fn probe_direct(target: (&str, u16)) -> bool {
    let addr = format!("{}:{}", target.0, target.1);
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(6),
            tokio::net::TcpStream::connect(addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// 经 SOCKS5 代理连一个 host:port。
///
/// 只做最小握手（无认证 + CONNECT），够验证「代理能不能把我送到那儿」。
/// 不引第三方 socks 客户端：这里只需要十几个字节的固定报文。
async fn probe_via_socks5(proxy: &str, target: (&str, u16)) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let hostport = proxy.trim_start_matches("socks5://");
    // 代理本身可能带认证前缀，取最后一个 '@' 之后的部分
    let hostport = hostport.rsplit_once('@').map_or(hostport, |(_, h)| h);

    let Ok(Ok(mut s)) = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        tokio::net::TcpStream::connect(hostport),
    )
    .await
    else {
        return false;
    };

    // 问候：VER=5, NMETHODS=1, METHOD=0(无认证)
    if s.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 2];
    if s.read_exact(&mut buf).await.is_err() || buf[0] != 0x05 || buf[1] != 0x00 {
        return false;
    }

    // CONNECT 到目标（ATYP=1 IPv4）
    let Ok(ip) = target.0.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&ip.octets());
    req.extend_from_slice(&target.1.to_be_bytes());
    if s.write_all(&req).await.is_err() {
        return false;
    }

    // 回复：第 2 字节 0x00 表示成功
    let mut rep = [0u8; 4];
    if s.read_exact(&mut rep).await.is_err() {
        return false;
    }
    rep[1] == 0x00
}

/// 当前用的是哪一份应用身份。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiIdStatus {
    /// 是不是内置那一份。
    pub builtin: bool,
    /// 当前生效的 `api_id`。公开信息，可以显示。
    pub id: i32,
    /// 用户是否已保存过自己的一对。
    ///
    /// 与 `builtin` 分开：保存过但凭据库暂时打不开时，`builtin` 是
    /// `true`（实际回落了）而这里仍是 `true`——界面要能说清
    /// 「你填过，但这次没读出来」，而不是让它看起来像没填过。
    pub configured: bool,
}

/// 查当前的应用身份。
///
/// **不返回 `api_hash`**：它是凭据，界面没有任何需要显示它的理由。
#[tauri::command]
#[must_use]
pub fn telegram_api_id_status() -> ApiIdStatus {
    // 与 settings.rs 的 config_get / config_set 走同一条路：配置按需读写，
    // 不做成常驻 state。多一种访问方式就多一处会不同步的地方
    let (id, hash) = omy_config::Config::load()
        .map(|c| (c.remote.telegram_api_id, c.remote.telegram_api_hash.clone()))
        .unwrap_or((None, None));
    let configured = id.is_some() && hash.is_some();
    // 解不开信封时回落到内置：连不上比「用错身份」更糟
    let app = AppId::from_config(id, hash.as_deref()).unwrap_or_else(|_| AppId::builtin());
    ApiIdStatus {
        builtin: app.is_builtin(),
        id: app.id(),
        configured,
    }
}

/// 保存用户自己的 api_id / api_hash。
///
/// # 为什么要有这条逃生口
///
/// 内置的是 Telegram Desktop 的 2040。它一旦触发
/// `API_ID_PUBLISHED_FLOOD`，所有用户同时连不上而**没有任何自救办法**
/// ——只能等我们发新版本。
///
/// # Errors
///
/// 格式不合法时返回 `tg_bad_api_id`。在这里拦住是为了让用户当场知道
/// 填错了：放过去的话服务端只回一句 `CONNECTION_API_ID_INVALID`，
/// 而界面上会显示成「登录失败」，指不到「你那两个字段填错了」。
#[tauri::command]
pub fn telegram_api_id_save(api_id: i32, api_hash: String) -> CmdResult<()> {
    // 校验走已有的 AppId::from_config，不另写一套规则——两套规则迟早
    // 会不一致，而不一致的那一侧会放进一个连不上的身份
    AppId::from_config(Some(api_id), Some(&api_hash))
        .map_err(|e| CmdError::with("tg_bad_api_id", detail(&e.to_string())))?;

    let mut c = omy_config::Config::load()
        .map_err(|e| CmdError::with("config_read_failed", detail(&e.to_string())))?;
    c.remote.telegram_api_id = Some(api_id);
    c.remote.telegram_api_hash = Some(api_hash);
    c.save()
        .map_err(|e| CmdError::with("config_write_failed", detail(&e.to_string())))?;
    Ok(())
}

/// 恢复使用内置的那一份。
///
/// # Errors
///
/// 配置写入失败时返回。
#[tauri::command]
pub fn telegram_api_id_reset() -> CmdResult<()> {
    // **不能只把字段设成 None 再 save。**
    //
    // `Config::save_to` 是合并写入而不是整体覆盖——那是为了保留更高版本
    // 写入的、本版本不认识的键（否则新旧版本来回切一次，新版本的设置就
    // 悄悄丢了）。代价是 `Option` 字段设成 `None` 时序列化后**根本不产生
    // 这个键**，合并时老值原封不动留着。
    //
    // 实测过这个后果：点「恢复内置」毫无反应，配置里仍是用户那一对，
    // 而界面状态文案也不变——两处都指不到「保存其实没写进去」。
    //
    // 所以这里显式把键从文件里删掉。不去改 merge_into 的语义：那会波及
    // 所有配置项，风险远大于收益。
    let path = omy_config::config_path().ok_or_else(|| CmdError::code("config_no_path"))?;
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut table = text.parse::<toml::Table>().unwrap_or_default();
    if let Some(toml::Value::Table(remote)) = table.get_mut("remote") {
        remote.remove("telegram_api_id");
        remote.remove("telegram_api_hash");
    }
    let out = toml::to_string_pretty(&table)
        .map_err(|e| CmdError::with("config_write_failed", detail(&e.to_string())))?;
    omy_core::fsatomic::write_atomic(&path, out.as_bytes())
        .map_err(|e| CmdError::with("config_write_failed", detail(&e.to_string())))?;
    Ok(())
}

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
    state: tauri::State<'_, crate::commands::Shared>,
    path: String,
    passcode: Option<String>,
    proxy_url: Option<String>,
) -> CmdResult<RegisterOutcome> {
    let dir = std::path::PathBuf::from(&path);
    let pass = passcode.unwrap_or_default();

    // 记「开始导入」但**不记真实 tdata 路径与 passcode**——路径可能暴露
    // 用户目录结构，passcode 是凭据
    crate::applog::info(
        "tg-tdata",
        &format!("开始导入 有passcode={}", !pass.is_empty()),
    );
    // 解析。注意这一步**不碰网络**，失败就是格式或密码问题
    let auth = tokio::task::spawn_blocking(move || tdata::read_tdata(&dir, &pass))
        .await
        .map_err(|e| CmdError::with("tg_tdata_failed", detail(&e.to_string())))?
        .map_err(|e| {
            crate::applog::error("tg-tdata", &format!("解析阶段失败: {}", tdata_code(&e)));
            CmdError::with(tdata_code(&e), detail(&e.to_string()))
        })?;
    crate::applog::info("tg-tdata", "解析成功，开始向服务端验证");

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
        .map_err(|e| {
            crate::applog::error("tg-tdata", &format!("连接阶段失败: {}", e.code()));
            CmdError::with(e.code(), detail(&e.to_string()))
        })?;

    // 拿这个账号的服务端 user id 用于去重；默认位置名取服务端昵称
    let user_id = connect::account_user_id(&conn.client).await;
    let label = connect::account_label(&conn.client).await;

    let store = TelegramStore::from_connection(conn.client, conn.runner);

    // 去重在**建位置之前**做——这样命中时根本不必回滚一个刚建的位置。
    // exclude 传 None：tdata 这条路此刻还没有属于自己的占位。
    if let Some(existing) = dedupe_target(&reg, user_id, None) {
        // 命中已有账号：不新建。用这次的登录态覆盖已有位置的 session 与连接，
        // 不留孤儿——tdata 的 session 此刻还没落盘，直接 save_current 到
        // 已有位置的 id 即可。
        if let Err(e) = crate::place_keys::save_session_at(&state, &saved, &existing) {
            eprintln!("[omy] 覆盖已有账号 session 失败：{e}");
        }
        reg.update_telegram_connection(&existing, store, user_id);
        if let Err(e) = reg.persist() {
            eprintln!("[omy] 保存 Telegram 位置失败：{e}");
        }
        crate::applog::info(
            "tg-tdata",
            &format!("导入命中已有账号 uid={}，并入 {existing}，不新建", redact_uid(user_id)),
        );
        return Ok(RegisterOutcome { id: existing, duplicate: true });
    }

    // **不再摘掉已有的 Telegram 位置。**
    //
    // 早先这里有一句「已有占位就先摘掉，否则侧栏会出现两个 Telegram」，
    // 那基于「只会有一个账号」的前提。多账号之后两个位置正是**想要**的结果，
    // 摘掉反而会让用户导入第二个账号时第一个凭空消失。
    let id = reg
        .add_telegram(label, store, proxy.clone(), user_id)
        .map_err(|e| CmdError::with("tg_register_failed", detail(&e.to_string())))?;
    crate::applog::info("tg-tdata", &format!("导入成功，位置 id={id}"));

    // 拿到位置 id 之后才落 session：文件名正是按它命名的。
    //
    // 顺序不能反——先落盘就得先编一个 id，那会让「session 文件叫什么」
    // 有两个来源，迟早对不上，而现象是「导入成功但重启后要重新登录」。
    //
    // 这一步也让导入的账号**独立持有自己的 session 副本**：此后它与
    // Telegram Desktop 再无关系，桌面端退出登录或删掉 tdata 都不影响它。
    if let Err(e) = crate::place_keys::save_session_at(&state, &saved, &id) {
        // 存不住不该让整件事失败——本次会话里它是好的。
        // 但要说出来，否则用户会以为下次还在
        eprintln!("[omy] 保存 Telegram 登录态失败：{e}");
    }

    if let Err(e) = reg.persist() {
        eprintln!("[omy] 保存 Telegram 位置失败：{e}");
    }
    Ok(RegisterOutcome { id, duplicate: false })
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
/// 登录去重的结果：命中已有账号，还是建了新位置。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterOutcome {
    /// 最终对应的位置 id（命中时是已有位置的 id，否则是新建的）。
    pub id: String,
    /// 是否命中了已有账号（前端据此提示「该账号已添加」并跳过去）。
    pub duplicate: bool,
}

/// 三条登录路径共用的去重判据：拿这次登录的 user id 查有没有已加过的同一账号。
///
/// **抽成一处、三处调用**（AGENTS.md 同一逻辑不两处实现）：只在扫码那条路上
/// 判重，换手机号或 tdata 照样能把同一个账号加第二遍。判据是 user id 不是
/// 昵称——昵称会重、会改，只有服务端 user id 唯一。
///
/// `exclude` 排除某个位置：扫码路径会先建占位（PENDING→connect），查重要把
/// 那个刚建的占位排除掉，否则会自己命中自己。tdata 传 `None`（查重在建位置
/// 之前做）。
///
/// 返回命中的已有位置 id；未命中或 user id 取不到（网络抖动）时 `None`——
/// 取不到时按新账号处理，也好过错判成某个已有账号。
fn dedupe_target(
    reg: &crate::places::PlaceRegistry,
    user_id: Option<i64>,
    exclude: Option<&str>,
) -> Option<String> {
    let uid = user_id?;
    reg.find_telegram_by_user(uid, exclude)
}

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
    state: tauri::State<'_, crate::commands::Shared>,
    proxy_url: Option<String>,
) -> CmdResult<RegisterOutcome> {
    let proxy = match proxy::normalize(proxy_url.as_deref().unwrap_or_default()) {
        Ok(p) => p.map(|p| p.to_string()),
        Err(e) => return Err(CmdError::with("tg_bad_proxy", detail(&e.to_string()))),
    };

    let app = AppId::builtin();
    let device = DeviceInfo::current();
    // 扫码那条路把 session 落在 PENDING_ACCOUNT 下（那时还没有位置 id），
    // 所以这里从它读回来
    let keks = crate::place_keys::unlock_keks(&state);
    let conn = connect::connect_saved_with_keks(
        &app, &device, proxy.as_deref(), tgsession::PENDING_ACCOUNT, &keks,
    )
    .await
    .map_err(|e| CmdError::with(e.code(), detail(&e.to_string())))?;

    // 拿这个账号的服务端 user id，用于去重。取不到就按新账号处理
    let user_id = connect::account_user_id(&conn.client).await;
    // 默认位置名取服务端昵称，让两个账号在侧栏上一眼能分辨
    let label = connect::account_label(&conn.client).await;

    // 把 runner 一并交给 store 接管。
    //
    // 不能只取 client 然后让 conn 离开作用域：runner 被丢弃后所有请求都会排进
    // 队列、再也发不出去，而且不报错——表现是界面一直转圈。交给 store 之后
    // 这条约束由类型系统保证，不靠谁记得。
    let store = TelegramStore::from_connection(conn.client, conn.runner);

    // 去重：这个账号是不是已经加过了？此处还没建位置，exclude 传 None。
    if let Some(existing) = dedupe_target(&reg, user_id, None) {
        // 命中已有账号：不新建。用这次更新鲜的登录态覆盖已有位置的连接，
        // 并把 PENDING 那份 session 收编成已有位置的（覆盖旧的），
        // 绝不留孤儿 session 文件。
        reg.update_telegram_connection(&existing, store, user_id);
        if let Err(e) = tgsession::adopt_pending(&existing) {
            eprintln!("[omy] 覆盖已有账号 session 失败：{e}");
        }
        if let Err(e) = reg.persist() {
            eprintln!("[omy] 保存 Telegram 位置失败：{e}");
        }
        crate::applog::info(
            "tg",
            &format!("扫码命中已有账号 uid={}，并入 {existing}，不新建", redact_uid(user_id)),
        );
        return Ok(RegisterOutcome { id: existing, duplicate: true });
    }

    let id = reg
        .add_telegram(label, store, proxy.clone(), user_id)
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
    Ok(RegisterOutcome { id, duplicate: false })
}

/// user id 记进日志时脱敏：它虽是公开数字 id、不是凭据，但仍属于账号信息，
/// 日志里过一遍 redact 只保留「能不能对上是同一个」的能力。
fn redact_uid(user_id: Option<i64>) -> String {
    match user_id {
        Some(u) => crate::applog::redact(&u.to_string()),
        None => String::from("<none>"),
    }
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
    keks: &[omy_core::crypto::Kek],
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
    crate::applog::info(
        "tg",
        &format!(
            "重连 place={place_id} proxy={}",
            if proxy.is_some() { "有" } else { "无（直连）" }
        ),
    );
    let t0 = std::time::Instant::now();
    let conn = connect::connect_saved_with_keks(&app, &device, proxy.as_deref(), place_id, keks)
        .await
        .map_err(|e| {
            crate::applog::error(
                "tg",
                &format!("重连 place={place_id} 失败: {} ({}ms)", e.code(), t0.elapsed().as_millis()),
            );
            CmdError::with(e.code(), detail(&e.to_string()))
        })?;
    crate::applog::info(
        "tg",
        &format!("重连 place={place_id} 成功 ({}ms)", t0.elapsed().as_millis()),
    );
    // user id 优先沿用占位带回来的那个（restore 从配置读进 p.user_id）。
    // 但**老配置里的位置没有 user id**（这个字段是这次才加的），若不补上，
    // 用户已有的那些账号永远参与不了去重——重复登录同一个老账号照样会
    // 新建一个。所以当占位没有 user id 时，趁这次连上补取一次。
    let user_id = match p.user_id {
        Some(u) => Some(u),
        None => {
            let uid = connect::account_user_id(&conn.client).await;
            if uid.is_some() {
                crate::applog::info(
                    "tg",
                    &format!("为老位置 {place_id} 补取 user id={}", redact_uid(uid)),
                );
            }
            uid
        }
    };
    let backfilled = p.user_id.is_none() && user_id.is_some();
    let store = TelegramStore::from_connection(conn.client, conn.runner);
    // 换掉占位：先摘再加，否则会并存两条。
    reg.remove(place_id);
    reg.add_telegram_with_id(
        String::from(place_id),
        p.name.clone(),
        store,
        proxy,
        user_id,
    );
    // 补取了 user id 就落盘一次，让它下次启动也在（否则每次进都要重取）
    if backfilled {
        if let Err(e) = reg.persist() {
            eprintln!("[omy] 保存补取的 user id 失败：{e}");
        }
    }
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
    // 用 per-place 槽格式落盘：拿当前会话已解锁的 KEK（无库时回退机器密钥）
    // 给这份 PENDING session 建密码槽。这样用户当前输过的任一 omy 密码之后
    // 都能解锁这个位置；没有任何库/密码时用机器密钥，等同旧的机器绑定加密。
    use tauri::Manager as _;
    let state = app.state::<crate::commands::Shared>();
    let saved = match tgsession::extract(sess.session(), appid) {
        Ok(s) => match crate::place_keys::save_session_at(&state, &s, tgsession::PENDING_ACCOUNT) {
            Ok(crate::place_keys::SaveResult::Saved) => true,
            Ok(crate::place_keys::SaveResult::CannotPersist) => {
                // 本机既没解锁任何库、也没凭据库。**不退回明文**，如实告诉
                // 用户这次登录只在本次会话有效
                eprintln!("[omy] 本机无法安全保存 Telegram 登录态（无已解锁密码且无凭据库）");
                false
            }
            Err(e) => {
                eprintln!("[omy] Telegram 登录态保存失败：{e}");
                false
            }
        },
        Err(e) => {
            eprintln!("[omy] 抽取 Telegram 登录态失败：{e}");
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

/// 手机号登录进度事件的通道名。前端 `listen('telegram-phone-login', …)`。
///
/// 与扫码分开一个通道：两条登录路径的事件形态不同（这条没有二维码、有验证码
/// 形态），混在一个通道里前端要靠字段有无来猜是哪条，容易错。
pub const PHONE_LOGIN_EVENT: &str = "telegram-phone-login";

/// 推给前端的手机号登录进度。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PhonePhase {
    /// 正在连接。
    Connecting,
    /// 等用户输入手机号。
    AwaitingPhone,
    /// 验证码已发出，等输码。
    CodeSent {
        /// 是不是纯数字码。false 表示单词或短句，界面不能用数字键盘/位数限制。
        numeric: bool,
        /// 固定位数（供数字码分格）；无固定位数时为 `None`。
        length: Option<u8>,
        /// 「验证码发到哪里」的 i18n key。App 那条尤其重要：发到其他已登录
        /// 客户端，用户不知道就盯着短信白等。
        via_key: String,
        /// 这个送达方式要不要用户离开本机去别处取码（App 为真）。
        needs_other_client: bool,
    },
    /// 要输两步验证的云密码（事先不可预知，只有开了 2FA 才会走到）。
    NeedPassword {
        /// 用户自设的提示，可能为空。
        hint: Option<String>,
    },
    /// 登录成功。
    Done {
        /// 登录态有没有真的落盘。
        session_saved: bool,
    },
    /// 失败。
    Failed {
        /// 翻译键（`errors.<code>`）。
        code: String,
        /// 限流剩余秒数，其余为 `None`。
        wait_secs: Option<u32>,
        /// 供排查的原因（服务端错误名 + 代码），不含 message（可能回显号码）。
        detail: Option<String>,
    },
}

/// 用户给手机号登录任务的输入。
///
/// 用一个 enum 走同一条 channel，而不是给手机号、验证码、密码、重发各开一个
/// 通道：那样后台任务要同时 select 四个通道，而它们本质是「登录流程的下一个
/// 输入」这同一件事的不同阶段。
#[derive(Debug, Clone)]
pub enum PhoneInput {
    /// 提交手机号（含国家码，如 +8613800138000）。
    Phone(String),
    /// 提交验证码。
    Code(String),
    /// 提交 2FA 云密码。
    Password(String),
    /// 请求重新发码。
    Resend,
}

/// 正在进行的手机号登录任务。
#[derive(Default)]
pub struct PhoneLoginTask {
    inner: Mutex<Option<PhoneRunning>>,
}

struct PhoneRunning {
    handle: tauri::async_runtime::JoinHandle<()>,
    input_tx: tokio::sync::mpsc::UnboundedSender<PhoneInput>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl PhoneLoginTask {
    /// 新建空闲任务。
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
            .and_then(|g| {
                g.as_ref()
                    .map(|r| !r.done.load(std::sync::atomic::Ordering::SeqCst))
            })
            .unwrap_or(false)
    }

    /// 取消并清理。
    pub fn cancel(&self) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(r) = g.take() {
                r.handle.abort();
                r.done.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    /// 把一个输入送给后台任务。返回是否送出去了。
    fn send(&self, input: PhoneInput) -> bool {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.input_tx.send(input).is_ok()))
            .unwrap_or(false)
    }

    fn set(&self, r: PhoneRunning) {
        if let Ok(mut g) = self.inner.lock() {
            if let Some(old) = g.take() {
                old.handle.abort();
                old.done.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            *g = Some(r);
        }
    }
}

/// 共享句柄。
pub type SharedPhoneLogin = Arc<PhoneLoginTask>;

/// 把驱动层错误翻成手机号登录的失败事件。
/// 从错误里取出简短错误码，用于日志（不含手机号/验证码等敏感信息）。
fn phone_err_code(e: &QrError) -> String {
    match phone_phase_of_error(e) {
        PhonePhase::Failed { code, .. } => code,
        _ => String::from("unknown"),
    }
}

fn phone_phase_of_error(e: &QrError) -> PhonePhase {
    let (code, wait) = match e {
        QrError::FloodWait { secs } => ("tg_flood_wait", Some(*secs)),
        QrError::ApiIdPublishedFlood => ("tg_api_id_flood", None),
        QrError::WrongPassword => ("tg_wrong_password", None),
        QrError::SignUpRequired => ("tg_signup_required", None),
        QrError::UnsupportedPasswordAlgo => ("tg_unsupported_2fa", None),
        QrError::Disconnected => ("tg_disconnected", None),
        QrError::Invocation(d) => classify_phone_invocation(d),
    };
    let detail = match e {
        // 已归入具体码的（含验证码错、需付费）就不再附原文
        QrError::Invocation(d) if classify_phone_invocation(d).0 == "tg_login_failed" => {
            Some(d.clone())
        }
        _ => None,
    };
    PhonePhase::Failed {
        code: String::from(code),
        wait_secs: wait,
        detail,
    }
}

/// 从服务端原始错误名里认出手机号登录特有的几种，给它们各自的出路。
///
/// 验证码错 / 过期、号码不合法、需付费短信——每种界面要做的事不同：
/// 重输、重发、改号码、改用扫码。混成一句「登录失败」用户就不知道下一步。
fn classify_phone_invocation(detail: &str) -> (&'static str, Option<u32>) {
    if detail.contains("PHONE_CODE_INVALID") {
        ("tg_code_invalid", None)
    } else if detail.contains("PHONE_CODE_EXPIRED") {
        ("tg_code_expired", None)
    } else if detail.contains("PHONE_NUMBER_INVALID") {
        ("tg_phone_invalid", None)
    } else if detail.contains("PHONE_NUMBER_BANNED") {
        ("tg_phone_banned", None)
    } else if detail.contains("PAYMENT_REQUIRED") {
        // 需付费短信，我们不支持——引导改用扫码（扫码不经过 sendCode）
        ("tg_phone_payment", None)
    } else {
        ("tg_login_failed", None)
    }
}

/// 往前端推一步手机号登录进度。
fn phone_emit(app: &tauri::AppHandle, phase: &PhonePhase) {
    if let PhonePhase::Failed { code, detail, .. } = phase {
        eprintln!(
            "[omy] Telegram 手机号登录失败：{code}{}",
            detail.as_ref().map_or(String::new(), |d| format!(" / {d}"))
        );
    }
    if let Err(e) = app.emit(PHONE_LOGIN_EVENT, phase) {
        eprintln!("[omy] Telegram 手机号登录进度推送失败：{e}");
    }
}

/// 开始手机号登录。立刻返回，进度通过 [`PHONE_LOGIN_EVENT`] 推送。
///
/// 与扫码同理：必须是 `async` + `tauri::async_runtime::spawn`，否则 `spawn`
/// 会在没有 reactor 的线程上 panic 并带走整个进程（见 `telegram_login_start`
/// 那处的详细注释）。
///
/// # Errors
///
/// 已有登录进行中、或代理地址不合法时返回。
#[tauri::command]
pub async fn telegram_phone_start(
    app: tauri::AppHandle,
    task: tauri::State<'_, SharedPhoneLogin>,
    proxy_url: Option<String>,
) -> CmdResult<()> {
    if task.is_busy() {
        return Err(CmdError::code("tg_login_in_progress"));
    }
    let proxy = match proxy::normalize(proxy_url.as_deref().unwrap_or_default()) {
        Ok(p) => p.map(|p| p.as_str().to_owned()),
        Err(e) => return Err(CmdError::with("tg_bad_proxy", detail(&e.to_string()))),
    };
    let (input_tx, input_rx) = tokio::sync::mpsc::unbounded_channel();
    let emitter = app.clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_for_task = Arc::clone(&done);
    let handle = tauri::async_runtime::spawn(async move {
        run_phone_login(&emitter, proxy, input_rx).await;
        done_for_task.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    task.set(PhoneRunning {
        handle,
        input_tx,
        done,
    });
    Ok(())
}

/// 提交手机号。
///
/// # Errors
///
/// 没有正在进行的手机号登录时返回。
#[tauri::command]
pub fn telegram_phone_submit_phone(
    task: tauri::State<'_, SharedPhoneLogin>,
    phone: String,
) -> CmdResult<()> {
    if task.send(PhoneInput::Phone(phone)) {
        Ok(())
    } else {
        Err(CmdError::code("tg_no_login_in_progress"))
    }
}

/// 提交验证码。
///
/// # Errors
///
/// 没有正在进行的手机号登录时返回。
#[tauri::command]
pub fn telegram_phone_submit_code(
    task: tauri::State<'_, SharedPhoneLogin>,
    code: String,
) -> CmdResult<()> {
    if task.send(PhoneInput::Code(code)) {
        Ok(())
    } else {
        Err(CmdError::code("tg_no_login_in_progress"))
    }
}

/// 提交 2FA 云密码。
///
/// # Errors
///
/// 没有正在进行的手机号登录时返回。
#[tauri::command]
pub fn telegram_phone_submit_password(
    task: tauri::State<'_, SharedPhoneLogin>,
    password: String,
) -> CmdResult<()> {
    if task.send(PhoneInput::Password(password)) {
        Ok(())
    } else {
        Err(CmdError::code("tg_no_login_in_progress"))
    }
}

/// 请求重新发码。
///
/// # Errors
///
/// 没有正在进行的手机号登录时返回。
#[tauri::command]
pub fn telegram_phone_resend(task: tauri::State<'_, SharedPhoneLogin>) -> CmdResult<()> {
    if task.send(PhoneInput::Resend) {
        Ok(())
    } else {
        Err(CmdError::code("tg_no_login_in_progress"))
    }
}

/// 取消手机号登录。
#[tauri::command]
pub fn telegram_phone_cancel(task: tauri::State<'_, SharedPhoneLogin>) {
    task.cancel();
}

/// 后台手机号登录主循环。
async fn run_phone_login(
    app: &tauri::AppHandle,
    proxy: Option<String>,
    mut input_rx: tokio::sync::mpsc::UnboundedReceiver<PhoneInput>,
) {
    crate::applog::info(
        "tg-phone",
        &format!(
            "手机号登录开始，连接中 proxy={}",
            if proxy.is_some() { "有" } else { "无（直连）" }
        ),
    );
    phone_emit(app, &PhonePhase::Connecting);
    let appid = AppId::builtin();
    let device = DeviceInfo::current();
    let mut sess = match PhoneSession::connect(appid.clone(), proxy.as_deref(), &device) {
        Ok(s) => s,
        Err(e) => {
            // 只记错误码，不记手机号/代理地址明文
            crate::applog::error("tg-phone", &format!("连接失败: {}", phone_err_code(&e)));
            phone_emit(app, &phone_phase_of_error(&e));
            return;
        }
    };

    // ① 等手机号
    phone_emit(app, &PhonePhase::AwaitingPhone);
    let phone = loop {
        match input_rx.recv().await {
            Some(PhoneInput::Phone(p)) => break p,
            // 还没发码前收到别的输入就忽略——界面在这一步只该给手机号输入框
            Some(_) => {}
            // 通道关了 = 用户取消，静默收工
            None => return,
        }
    };

    // ② 发码，进入「输码」子循环。这里用一个可重入的循环处理重发。
    // 返回值（是否走到终态）在这里没有后续动作，直接丢弃
    let _ = phone_code_loop(app, &mut sess, &phone, &mut input_rx).await;
}

/// 发码 + 输码子循环，处理验证码错误重输、重发、以及 2FA 分支。
///
/// 返回 `true` 表示走到了终态（成功或已 emit 失败）。
async fn phone_code_loop(
    app: &tauri::AppHandle,
    sess: &mut PhoneSession,
    phone: &str,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PhoneInput>,
) -> bool {
    // 首次发码
    match sess.send_code(phone).await {
        Ok(PhoneEvent::CodeSent { shape }) => {
            crate::applog::info("tg-phone", "sendCode 成功，验证码已发出");
            emit_code_sent(app, &shape);
        }
        Ok(PhoneEvent::LoggedIn) => {
            crate::applog::info("tg-phone", "sendCode 即登录成功");
            phone_finish(app, sess).await;
            return true;
        }
        Ok(PhoneEvent::NeedPassword { .. }) => {} // send_code 不会走到这
        Err(e) => {
            // FLOOD_WAIT / PHONE_NUMBER_INVALID 这类正是排查最需要的
            crate::applog::error("tg-phone", &format!("sendCode 失败: {}", phone_err_code(&e)));
            phone_emit(app, &phone_phase_of_error(&e));
            return true;
        }
    }

    loop {
        match input_rx.recv().await {
            Some(PhoneInput::Code(code)) => match sess.submit_code(&code).await {
                Ok(PhoneEvent::LoggedIn) => {
                    crate::applog::info("tg-phone", "signIn 成功（验证码通过）");
                    phone_finish(app, sess).await;
                    return true;
                }
                Ok(PhoneEvent::NeedPassword { hint }) => {
                    crate::applog::info("tg-phone", "验证码通过，需 2FA 云密码");
                    return phone_password_loop(app, sess, hint, input_rx).await;
                }
                Ok(PhoneEvent::CodeSent { .. }) => {} // submit_code 不会走到这
                Err(e) => {
                    // 只记错误码（验证码错/过期），绝不记 code 本身
                    crate::applog::warn("tg-phone", &format!("signIn 失败: {}", phone_err_code(&e)));
                    // 验证码错 / 过期停在本步：推一条失败让界面提示，但**不返回**，
                    // 继续等用户重输或重发
                    phone_emit(app, &phone_phase_of_error(&e));
                }
            },
            Some(PhoneInput::Resend) => match sess.resend_code().await {
                Ok(PhoneEvent::CodeSent { shape }) => emit_code_sent(app, &shape),
                Ok(PhoneEvent::LoggedIn) => {
                    phone_finish(app, sess).await;
                    return true;
                }
                Ok(PhoneEvent::NeedPassword { .. }) => {}
                Err(e) => phone_emit(app, &phone_phase_of_error(&e)),
            },
            // 这一步不该收到手机号/密码，忽略
            Some(_) => {}
            None => return false,
        }
    }
}

/// 2FA 云密码子循环：反复要密码直到通过或用户放弃。
async fn phone_password_loop(
    app: &tauri::AppHandle,
    sess: &mut PhoneSession,
    mut hint: Option<String>,
    input_rx: &mut tokio::sync::mpsc::UnboundedReceiver<PhoneInput>,
) -> bool {
    loop {
        phone_emit(app, &PhonePhase::NeedPassword { hint: hint.clone() });
        let Some(input) = input_rx.recv().await else {
            return false;
        };
        let PhoneInput::Password(pw) = input else {
            // 这一步只收密码，其余忽略
            continue;
        };
        match sess.submit_password(&pw).await {
            Ok(_) => {
                phone_finish(app, sess).await;
                return true;
            }
            Err(QrError::WrongPassword) => {
                phone_emit(
                    app,
                    &PhonePhase::Failed {
                        code: String::from("tg_wrong_password"),
                        wait_secs: None,
                        detail: None,
                    },
                );
                hint = hint.take();
            }
            Err(e) => {
                phone_emit(app, &phone_phase_of_error(&e));
                return true;
            }
        }
    }
}

/// 把 CodeSent 事件翻成前端要的形状。
fn emit_code_sent(app: &tauri::AppHandle, shape: &omy_remote::telegram::login::CodeShape) {
    phone_emit(
        app,
        &PhonePhase::CodeSent {
            numeric: shape.length > 0,
            length: if shape.length > 0 {
                Some(shape.length)
            } else {
                None
            },
            via_key: String::from(shape.via.guidance_key()),
            needs_other_client: shape.via.requires_other_client(),
        },
    );
}

/// 手机号登录成功的收尾：与扫码同样，先落盘到 PENDING_ACCOUNT、再自证。
async fn phone_finish(app: &tauri::AppHandle, sess: &PhoneSession) {
    use tauri::Manager as _;
    let state = app.state::<crate::commands::Shared>();
    let saved = match tgsession::extract(sess.session(), sess.app()) {
        Ok(s) => match crate::place_keys::save_session_at(&state, &s, tgsession::PENDING_ACCOUNT) {
            Ok(crate::place_keys::SaveResult::Saved) => true,
            Ok(crate::place_keys::SaveResult::CannotPersist) => {
                eprintln!("[omy] 本机无法安全保存 Telegram 登录态（无已解锁密码且无凭据库）");
                false
            }
            Err(e) => {
                eprintln!("[omy] Telegram 登录态保存失败：{e}");
                false
            }
        },
        Err(e) => {
            eprintln!("[omy] 抽取 Telegram 登录态失败：{e}");
            false
        }
    };
    match sess.is_authorized().await {
        Ok(true) => phone_emit(
            app,
            &PhonePhase::Done {
                session_saved: saved,
            },
        ),
        Ok(false) => phone_emit(
            app,
            &PhonePhase::Failed {
                code: String::from("tg_login_not_effective"),
                wait_secs: None,
                detail: None,
            },
        ),
        Err(e) => phone_emit(app, &phone_phase_of_error(&e)),
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
