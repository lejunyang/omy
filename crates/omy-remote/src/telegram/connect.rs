//! 用已保存的登录态建立连接。
//!
//! # 为什么单独一个模块
//!
//! 「连上去」这件事有两个入口：扫码登录（[`super::qrlogin`]）和复用已保存的
//! 登录态（这里）。两者建连的参数必须完全一致——device 标识、代理、api_id
//! 任何一项不同，服务端「已登录设备」列表里就会多出一条对不上的记录，
//! 而用户看到的是「我明明只登录了一次，为什么有两个设备」。
//!
//! 所以建连参数的组装收在这一处，由两个入口共用。

use std::sync::Arc;

use grammers_client::Client;
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::storages::MemorySession;

use super::appid::AppId;
use super::device::DeviceInfo;
use super::session::{self, SessionError};

/// 建连失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// 本地没有已保存的登录态。
    #[error("还没有登录 Telegram")]
    NoSession,
    /// 读取或解密登录态失败。
    #[error("读取登录态失败：{0}")]
    Session(#[from] SessionError),
    /// 网络层失败。
    #[error("连接失败：{0}")]
    Connect(String),
    /// 连不上，**而且没有配代理**。
    ///
    /// 与 [`Self::Connect`] 分开，因为用户要做的事完全不同：这一条是
    /// 「去填一个代理」，那一条是「你填的代理或网络有问题」。
    ///
    /// 为什么值得单独一个码：很多人以为开了系统级的「全局代理」就能直连。
    /// 但那类工具代理的是系统代理设置（HTTP 层）与可选的 TUN，没开 TUN
    /// 时**不接管应用自己发起的裸 TCP**，而 MTProto 正是裸 TCP。于是出现
    /// 「浏览器能上网，omy 却连不上 Telegram」，而一句笼统的「请检查网络
    /// 或代理设置」指不到这个真正的原因。
    #[error("直连 Telegram 数据中心失败，且未配置代理：{0}")]
    ConnectNoProxy(String),
    /// 服务端不认这份登录态（被撤销、过期、换了账号）。
    #[error("登录态已失效，需要重新登录")]
    Unauthorized,
    /// 位置的 session 被加密着，当前没有已解锁的密码能打开它。
    ///
    /// 这是**正常态、不是错误**：用户还没输能开这个位置的 omy 密码。界面据此
    /// 显示锁定态并提示先解锁，而不是引导重新登录（那会白扫一次码）。
    #[error("位置已加密，请先用 omy 密码解锁")]
    Locked,
}

impl ConnectError {
    /// 给界面用的稳定错误码。
    ///
    /// 用码而不是把 Display 直接丢给前端：前端要据此决定「引导去重新扫码」
    /// 还是「提示检查网络」，靠匹配中文串做这个判断会在改文案时悄悄失效。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NoSession => "tg_no_session",
            Self::Session(_) => "tg_session_unreadable",
            Self::Connect(_) => "tg_connect_failed",
            Self::ConnectNoProxy(_) => "tg_connect_no_proxy",
            Self::Unauthorized => "tg_session_expired",
            Self::Locked => "tg_locked",
        }
    }
}

/// 一条已建立的连接。
///
/// 持有 runner 的句柄：**丢掉它整条连接就停了**，所有请求会排进队列再也发不
/// 出去，而且不会有任何报错——表现是界面一直转圈。
pub struct Connection {
    /// 客户端。
    pub client: Client,
    /// 后台收发任务。
    ///
    /// **必须一直活着**，丢掉它所有请求都只会排队、再也发不出去，且不报错。
    /// 调用方应当把它交给 `TelegramStore::from_connection` 接管，
    /// 而不是自己想办法留着。
    pub runner: tokio::task::JoinHandle<()>,
}

/// 组装建连参数。扫码与复用两条路共用，避免两处各写一份。
#[must_use]
pub fn params(device: &DeviceInfo, proxy: Option<&str>) -> ConnectionParams {
    ConnectionParams {
        device_model: device.device_model.clone(),
        system_version: device.system_version.clone(),
        app_version: device.app_version.clone(),
        system_lang_code: device.system_lang_code.clone(),
        lang_code: device.lang_code.clone(),
        proxy_url: proxy.map(str::to_owned),
        ..ConnectionParams::default()
    }
}

/// 用**某个账号**已保存的登录态连上去。
///
/// `account` 是账号标识（产品里是位置 id，见 `places.rs`）。多账号之后这个
/// 参数不能省：省了就只能去猜「哪一份 session」，而猜错的表现是用户点开
/// A 账号却看到 B 账号的内容——两边都不报错。
///
/// 按账号找 session 只是一次**纯路径计算**，不额外碰网络，所以连接复用
/// 那条性质（实测再连约 2ms）不受影响。
///
/// # Errors
///
/// 没有存档、解密失败、网络不通、或服务端不认这份登录态时返回。
/// 最后一种要与前几种分开——它意味着「需要重新扫码」，而其余几种不是。
pub async fn connect_saved(
    app: &AppId,
    device: &DeviceInfo,
    proxy: Option<&str>,
    account: &str,
) -> Result<Connection, ConnectError> {
    let saved = session::load(app, account)?.ok_or(ConnectError::NoSession)?;
    connect_with(&saved, app, device, proxy).await
}

/// 用一批已解锁的 KEK 解开加密的 session 再连（per-place 槽格式）。
///
/// 与 [`connect_saved`] 的区别：那个走旧的 `session::load`（机器密钥）；这个
/// 走 `session::load_with_keks`，用会话里已解锁的 KEK 去开位置的密码槽。
///
/// # LoadOutcome 到 ConnectError 的映射
///
/// - `Unlocked` → 正常连接。
/// - `Locked` → [`ConnectError::Locked`]（未解锁，正常态，界面提示先解锁）。
/// - `NeedsMigration` → 用旧格式解出的登录态直接连（**能连上说明它有效**），
///   迁移到新格式的动作交给上层在合适时机做，不在建连路径里顺手改盘。
/// - `Absent` → [`ConnectError::NoSession`]。
///
/// # Errors
///
/// 读盘/解密/网络/服务端不认时返回；未解锁返回 [`ConnectError::Locked`]。
pub async fn connect_saved_with_keks(
    app: &AppId,
    device: &DeviceInfo,
    proxy: Option<&str>,
    account: &str,
    keks: &[omy_core::crypto::Kek],
) -> Result<Connection, ConnectError> {
    let saved = match session::load_with_keks(app, account, keks)? {
        session::LoadOutcome::Unlocked(s) | session::LoadOutcome::NeedsMigration(s) => s,
        session::LoadOutcome::Locked => return Err(ConnectError::Locked),
        session::LoadOutcome::Absent => return Err(ConnectError::NoSession),
    };
    connect_with(&saved, app, device, proxy).await
}

/// 用一份**给定的**登录态连上去（不读磁盘）。
///
/// tdata 导入要用它：导入拿到的登录态还没落盘，而正确的顺序是
/// **先问服务端认不认、认了再落盘**。反过来的话，一份已经失效的 tdata
/// 会把当前可用的那份 session 覆盖掉——用户为了省一次扫码，
/// 反而把已有的登录弄丢了。
///
/// # Errors
///
/// 同 [`connect_saved`]，但不会返回 [`ConnectError::NoSession`]。
pub async fn connect_with(
    saved: &session::SavedSession,
    app: &AppId,
    device: &DeviceInfo,
    proxy: Option<&str>,
) -> Result<Connection, ConnectError> {
    let mem = Arc::new(MemorySession::default());
    session::import_into(saved, &mem)
        .await
        .map_err(|e| ConnectError::Connect(e.to_string()))?;

    let SenderPool { runner, handle, .. } =
        SenderPool::with_configuration(Arc::clone(&mem), app.id(), params(device, proxy));
    let client = Client::new(handle);
    let runner = tokio::spawn(async move {
        runner.run().await;
    });

    // 必须真的问一句服务端认不认。
    //
    // 不问的话，一份被撤销的登录态也能「连接成功」，错误要等到用户点开某个
    // 对话才冒出来，而那时的报错指向的是那次操作，不是登录态——
    // 诊断方向完全跑偏。
    match client.is_authorized().await {
        Ok(true) => Ok(Connection { client, runner }),
        Ok(false) => Err(ConnectError::Unauthorized),
        // 没配代理时单独报。这台机器很可能直连不通，而那恰好是用户能
        // 动手解决的事；混进一句「请检查网络或代理设置」等于没说
        Err(e) if proxy.is_none() => Err(ConnectError::ConnectNoProxy(e.to_string())),
        Err(e) => Err(ConnectError::Connect(e.to_string())),
    }
}

/// 取一个适合当默认位置名的账号昵称。
///
/// 优先全名（first_name + last_name），其次 `@username`，都取不到就回落到
/// 「Telegram」。
///
/// # 为什么回落而不是报错
///
/// 名字只是个本地标签。为了取不到昵称就让整次登录或 tdata 导入失败，
/// 代价与收益完全不成比例——而那次登录在服务端已经生效了。
///
/// # 这只是默认值
///
/// 用户随时可以改（见 GUI 的 `telegram_place_rename`），且改名**只动本地
/// 显示名、不碰服务端**。也正因为昵称可重复、可随时改，它只能当显示名，
/// **不能**当 session 文件的账号标识——那个必须用稳定的位置 id。
///
/// 取不到时不打日志带出账号信息：昵称与 user id 都属于账号数据。
pub async fn account_label(client: &Client) -> String {
    let Ok(me) = client.get_me().await else {
        return String::from("Telegram");
    };
    let full = me.full_name();
    if !full.trim().is_empty() {
        return full;
    }
    if let Some(u) = me.username().filter(|u| !u.is_empty()) {
        return format!("@{u}");
    }
    String::from("Telegram")
}

/// 取当前账号的服务端 user id，用于登录去重。
///
/// 这是**账号在服务端的唯一标识**：昵称会重、会改，只有 user id 唯一，
/// 判断「两个位置是不是同一个账号」只能靠它。取不到（网络抖动、
/// `get_me` 失败）时返回 `None`——调用方据此选择不去重、按新账号处理，
/// 也好过错判成某个已有账号。
pub async fn account_user_id(client: &Client) -> Option<i64> {
    // me.id() 是 PeerId；对普通 User 取 bare_id() 得到服务端的账号数字 id。
    // bare_id() 只有在「self_user 占位」时才 None，而 get_me() 返回的是
    // 真实用户、不是那个占位，所以正常都能取到
    client.get_me().await.ok().and_then(|me| me.id().bare_id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 错误码必须稳定：前端靠它决定引导用户做什么。
    ///
    /// 不这样会怎样：前端改成匹配中文文案，之后任何一次文案调整都会让
    /// 「登录态失效 → 引导重新扫码」这条路径悄悄失效，而且不会有编译错误。
    #[test]
    fn error_codes_are_stable() {
        assert_eq!(ConnectError::NoSession.code(), "tg_no_session");
        assert_eq!(ConnectError::Unauthorized.code(), "tg_session_expired");
        assert_eq!(ConnectError::Locked.code(), "tg_locked");
        assert_ne!(
            ConnectError::Connect(String::new()).code(),
            ConnectError::ConnectNoProxy(String::new()).code(),
            "「连不上」与「连不上且没配代理」必须是不同的码：\
             前者让用户检查已填的代理，后者让他去填一个。\
             合成一个码的话，界面只能给一句放之四海皆准的提示"
        );
        assert_eq!(
            ConnectError::ConnectNoProxy(String::new()).code(),
            "tg_connect_no_proxy"
        );
        assert_eq!(
            ConnectError::Connect(String::new()).code(),
            "tg_connect_failed"
        );
    }

    /// 「没登录」与「登录态失效」必须是两个不同的码。
    ///
    /// 不这样会怎样：两者合并成一个码，界面只能给一句通用提示。
    /// 可它们要引导的动作不同——前者是「去登录」，后者是「你之前登录过，
    /// 但那个授权被撤销了」，后者还需要提示用户检查是不是自己在别处撤的。
    #[test]
    fn no_session_differs_from_expired() {
        assert_ne!(
            ConnectError::NoSession.code(),
            ConnectError::Unauthorized.code()
        );
    }

    /// 建连参数必须如实带上 device 标识。
    ///
    /// 不这样会怎样：device_model 落空或被写成别的客户端名，用户在官方
    /// 客户端的「已登录设备」里就认不出这条是 omy——而认不出就不敢撤销，
    /// 这是我们刻意报 omy 而不伪装的全部理由。
    #[test]
    fn params_carry_device_identity() {
        let d = DeviceInfo::current();
        let p = params(&d, Some("socks5://127.0.0.1:1080"));
        assert_eq!(p.device_model, d.device_model);
        assert!(
            p.device_model.contains("omy"),
            "device_model 应当如实报 omy，实际是 {}",
            p.device_model
        );
        assert_eq!(p.proxy_url.as_deref(), Some("socks5://127.0.0.1:1080"));
    }

    /// 不填代理时不能凭空造一个。
    ///
    /// 不这样会怎样：默认塞一个本机代理地址，在没开代理的机器上每次连接都要
    /// 等到超时才失败，而错误信息里根本不会提到「代理」两个字。
    #[test]
    fn params_without_proxy_stay_empty() {
        let p = params(&DeviceInfo::current(), None);
        assert!(p.proxy_url.is_none());
    }
}
