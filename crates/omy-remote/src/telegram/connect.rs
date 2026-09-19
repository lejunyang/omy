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
    /// 服务端不认这份登录态（被撤销、过期、换了账号）。
    #[error("登录态已失效，需要重新登录")]
    Unauthorized,
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
            Self::Unauthorized => "tg_session_expired",
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

/// 用已保存的登录态连上去。
///
/// # Errors
///
/// 没有存档、解密失败、网络不通、或服务端不认这份登录态时返回。
/// 最后一种要与前几种分开——它意味着「需要重新扫码」，而其余几种不是。
pub async fn connect_saved(
    app: &AppId,
    device: &DeviceInfo,
    proxy: Option<&str>,
) -> Result<Connection, ConnectError> {
    let saved = session::load(app)?.ok_or(ConnectError::NoSession)?;
    let mem = Arc::new(MemorySession::default());
    session::import_into(&saved, &mem)
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
        Err(e) => Err(ConnectError::Connect(e.to_string())),
    }
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
