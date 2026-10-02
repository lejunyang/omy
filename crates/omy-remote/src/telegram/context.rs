//! 一次 Telegram 建连所需的全部构造参数：应用身份（api_id / api_hash）+ 设备 + 代理。
//!
//! # 为什么要有这一层
//!
//! 建连参数散在连接、登录、设置多处时，调整必然漏一处，而漏掉的那处会让一部分
//! 用户用错 api_id 或走不同代理——表现是「有人能登录有人不能」或「明明配了代理
//! 却没走」，极难归因。这里把「从配置读出实际用的 api_id / api_hash / 代理」
//! 收成一处，QR、手机号、tdata、重连、打开已有 session 共用同一份。
//!
//! 已落盘的 session 把建连时的 `api_id` 记在信封里（见 [`super::session`]），加载
//! 时会校验。所以「打开/重连」用的 api_id 必须与「当初登录」时同源——否则换了
//! 配置就会撞 `SessionMismatch`。这正是本层要钉死的一致性。
//!
//! # tdata 是唯一的例外
//!
//! tdata 里的 auth key 出身于 Telegram Desktop（api_id 2040），导入时必须强制
//! [`AppIdChoice::Builtin`]，无视用户在配置里填的自定义身份——那条 auth key 是在
//! 2040 下协商的，配别的 id 建连会以难解释的方式失败。代理仍与其它入口同源。
//!
//! # 不泄密
//!
//! 本结构持有 [`AppId`]（含 api_hash 凭据）与可能带认证的代理地址，故**不派生
//! `Debug`**；手写实现只报公开的 id 与代理有无。

use std::path::Path;

use omy_config::Remote;

use super::appid::AppId;
use super::appid_store;
use super::device::DeviceInfo;
use super::proxy::{self, ProxyError, ProxyUrl};

/// 建连用哪一份应用身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppIdChoice {
    /// 按配置解析：填了自定义就用自定义，否则内置。
    ///
    /// 登录、重连、打开已有 session、tdata 以外的一切建连都走这条。
    FromConfig,
    /// 强制内置 2040。**仅 tdata 导入用**：那里的 auth key 出身于 Telegram Desktop。
    Builtin,
}

/// 构造连接上下文时出错（解析应用身份或代理）。
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error(transparent)]
    AppId(#[from] appid_store::LoadError),
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error(transparent)]
    Config(#[from] omy_config::Error),
    #[error("代理模式非法：{0:?}（应为 system 或 manual）")]
    BadProxyMode(String),
}

/// 按配置与可选显式 override 解析最终代理（已归一化为 grammers 能吃的 `socks5://`）。
///
/// 这是**纯函数**，不碰网络（system 模式下读系统代理是本地注册表/环境读取），
/// 便于把 override / manual / system 三态矩阵钉在单测里。
///
/// 规则：
/// - `override_url` 非空时优先（仅 CLI `--proxy` 显式指定），无视配置；
/// - 否则按 `remote.telegram_proxy_mode`：
///   - `system`：读取当前系统代理（探测失败 / 没开则直连）；
///   - `manual`：用 `remote.telegram_proxy`，非法当场报错。
///
/// 返回 `Ok(None)` 表示直连。
pub fn resolve_proxy(
    remote: &Remote,
    override_url: Option<&str>,
) -> Result<Option<ProxyUrl>, ContextError> {
    resolve_proxy_with_system(remote, override_url, proxy::detect_system_proxy())
}

/// [`resolve_proxy`] 的可注入版本：系统代理值由调用方给。
///
/// 生产代码用 [`resolve_proxy`]（它内部调 `detect_system_proxy`）；测试用本函数
/// 注入一个固定的系统代理值，把三态矩阵钉住而不依赖本机注册表。
pub fn resolve_proxy_with_system(
    remote: &Remote,
    override_url: Option<&str>,
    system: Option<ProxyUrl>,
) -> Result<Option<ProxyUrl>, ContextError> {
    if let Some(raw) = override_url {
        return proxy::normalize(raw).map_err(ContextError::from);
    }
    match remote.telegram_proxy_mode.as_str() {
        "system" => Ok(system),
        "manual" => proxy::normalize(&remote.telegram_proxy).map_err(ContextError::from),
        other => Err(ContextError::BadProxyMode(other.to_string())),
    }
}

/// 从配置解析本次建连用的应用身份（含 api_hash，只在建连处内部使用）。
///
/// `FromConfig` 走 `resolve_and_migrate[_at]`——在配置跨进程锁内把旧明文
/// api_hash 重封成信封；`Builtin` 无视配置，直接用内置 2040（tdata 专用）。
pub fn resolve_app(
    choice: AppIdChoice,
    config_path: Option<&Path>,
) -> Result<AppId, ContextError> {
    match choice {
        AppIdChoice::Builtin => Ok(AppId::builtin()),
        AppIdChoice::FromConfig => {
            let resolved = match config_path {
                Some(p) => appid_store::resolve_and_migrate_at(p)?,
                None => appid_store::resolve_and_migrate()?,
            };
            Ok(resolved.app)
        }
    }
}

/// 一次 Telegram 建连的全部构造参数：应用身份 + 设备 + 代理。
///
/// 拿到后即可喂给 [`super::connect`] / [`super::qrlogin::QrSession`] /
/// [`super::phonelogin::PhoneSession`]，调用方不必再各自拼装。
pub struct TelegramConnectionContext {
    app: AppId,
    device: DeviceInfo,
    proxy: Option<ProxyUrl>,
}

impl TelegramConnectionContext {
    /// 用默认配置路径构造（GUI 与绝大多数 CLI 命令）。
    pub fn load(choice: AppIdChoice, proxy_override: Option<&str>) -> Result<Self, ContextError> {
        Self::load_at(None, choice, proxy_override)
    }

    /// 显式指定配置路径（CLI `--config` 指定文件时用）。
    pub fn load_at(
        config_path: Option<&Path>,
        choice: AppIdChoice,
        proxy_override: Option<&str>,
    ) -> Result<Self, ContextError> {
        let app = resolve_app(choice, config_path)?;
        let remote = match config_path {
            Some(p) => omy_config::Config::load_from(p)?.remote,
            None => omy_config::Config::load()?.remote,
        };
        let proxy = resolve_proxy(&remote, proxy_override)?;
        Ok(Self {
            app,
            device: DeviceInfo::current(),
            proxy,
        })
    }

    /// 应用身份。
    pub fn app(&self) -> &AppId {
        &self.app
    }

    /// 设备标识。
    pub fn device(&self) -> &DeviceInfo {
        &self.device
    }

    /// 归一化后的代理地址；`None` 表示直连。
    pub fn proxy(&self) -> Option<&str> {
        self.proxy.as_ref().map(ProxyUrl::as_str)
    }
}

impl std::fmt::Debug for TelegramConnectionContext {
    // 只报公开的 id 与代理有无，绝不带 api_hash 或代理认证串。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramConnectionContext")
            .field("app_id", &self.app().id())
            .field("builtin", &self.app().is_builtin())
            .field("via_proxy", &self.proxy.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::appid::BUILTIN_API_ID;

    fn remote_with(mode: &str, manual: &str) -> Remote {
        Remote {
            telegram_proxy_mode: String::from(mode),
            telegram_proxy: String::from(manual),
            ..Default::default()
        }
    }

    /// `--proxy` override 优先于一切配置。
    ///
    /// 不这样会怎样：CLI `--proxy` 与配置 manual 同时存在时，配置静默覆盖命令行
    /// 参数，用户以为临时指定生效了，实际没走。
    #[test]
    fn override_wins_over_config() {
        let r = remote_with("manual", "socks5://127.0.0.1:1111");
        let got = resolve_proxy(&r, Some("socks5://127.0.0.1:2222"))
            .unwrap()
            .unwrap();
        assert_eq!(got.as_str(), "socks5://127.0.0.1:2222");
    }

    /// manual 模式用配置里的地址，并归一化成 socks5://。
    #[test]
    fn manual_mode_uses_configured_address() {
        let r = remote_with("manual", "127.0.0.1:7897");
        let got = resolve_proxy(&r, None).unwrap().unwrap();
        assert_eq!(got.as_str(), "socks5://127.0.0.1:7897");
    }

    /// manual 模式下非法地址当场报错，不静默回落直连。
    #[test]
    fn manual_bad_address_errors() {
        let r = remote_with("manual", "socks4://127.0.0.1:1080");
        assert!(resolve_proxy(&r, None).is_err());
    }

    /// system 模式：探测到就用探测结果，没探测到就直连；都不该报错。
    #[test]
    fn system_mode_uses_detection_or_direct() {
        let r = remote_with("system", "");
        let got = resolve_proxy(&r, None).unwrap();
        if let Some(p) = &got {
            assert!(
                p.as_str().starts_with("socks5://"),
                "探测结果必须已归一化: {}",
                p.as_str()
            );
        }
    }

    /// 配置里出现未知代理模式必须报错，不能猜一个。
    #[test]
    fn unknown_mode_errors() {
        let r = remote_with("bogus", "");
        assert!(matches!(
            resolve_proxy(&r, None),
            Err(ContextError::BadProxyMode(_))
        ));
    }

    /// tdata 边界：Builtin 无视配置，恒为内置 2040。
    ///
    /// 不这样会怎样：tdata 导入误用用户配置的自定义 id，而那份 auth key 出身于
    /// 2040，建连会以难解释的方式失败。
    #[test]
    fn builtin_choice_is_always_2040() {
        let app = resolve_app(AppIdChoice::Builtin, None).unwrap();
        assert_eq!(app.id(), BUILTIN_API_ID);
        assert!(app.is_builtin());
    }

    /// 自定义 app id 能穿透到上下文（id 是公开数字，hash 被包住不外泄）。
    #[test]
    fn custom_app_id_reaches_context() {
        let app = AppId::custom(424242, "0123456789abcdef0123456789abcdef").unwrap();
        let ctx = TelegramConnectionContext {
            app,
            device: DeviceInfo::current(),
            proxy: None,
        };
        assert_eq!(ctx.app().id(), 424242);
        assert!(!ctx.app().is_builtin());
        assert!(ctx.proxy().is_none());
    }

    /// session identity 一致性：用自定义身份落的 session，必须用同一身份重开；
    /// 拿内置 2040 去开就不匹配。
    ///
    /// 这正是「重连/打开/加解密切到 FromConfig」的根因：以前这些路径硬编码
    /// builtin(2040)，而自定义身份登录的 session 记的是 777，`SessionMismatch`
    /// 直接拒用。
    #[test]
    fn session_identity_must_match_saving_app() {
        use super::super::appid::SessionIdentity;
        let saved = AppId::custom(777, "0123456789abcdef0123456789abcdef").unwrap();
        let id = SessionIdentity::of(&saved);
        assert!(id.matches(&saved), "同一身份必须匹配");
        assert!(
            !id.matches(&AppId::builtin()),
            "内置 2040 不该匹配自定义 777 的 session"
        );
    }

    /// Debug 绝不能带出 api_hash 或代理认证串。
    ///
    /// 不这样会怎样：一次 `dbg!` 或把上下文塞进错误信息，应用凭据与代理密码就
    /// 留在日志里了。
    #[test]
    fn debug_does_not_leak_secrets() {
        let app = AppId::custom(777, "0123456789abcdef0123456789abcdef").unwrap();
        let proxy = proxy::normalize("socks5://u:secret@127.0.0.1:1080")
            .unwrap()
            .unwrap();
        let ctx = TelegramConnectionContext {
            app,
            device: DeviceInfo::current(),
            proxy: Some(proxy),
        };
        let text = format!("{ctx:?}");
        assert!(
            !text.contains("0123456789abcdef"),
            "Debug 带出了 api_hash: {text}"
        );
        assert!(
            !text.contains("secret"),
            "Debug 带出了代理密码: {text}"
        );
        assert!(text.contains("777"), "app_id 应当可见便于排查");
        assert!(text.contains("via_proxy"));
    }
}
