//! 应用身份（自定义 `api_id` / `api_hash`）的**配置存取**：GUI 与 CLI 同源。
//!
//! # 为什么单独一处
//!
//! 「配置里读哪一对、hash 是信封还是旧明文、没凭据库时怎么办、保存时怎么封」
//! 这套规则 GUI 和 CLI 都要做。各写一份会漂移：一边按信封读、一边按明文存，
//! 就会出现「命令行填的对，界面读出来解不开」这种不报错的不一致。
//!
//! # api_hash 永远不明文落盘
//!
//! 它是凭据：拿到它加一次登录就能冒充这个应用。所以：
//! - 保存走 [`seal`]，用 [`ProtectKey`] 封成 omy-secret 信封（表）；
//! - **没有保护器时拒绝写明文**（返回 `Ok(None)`），而不是退而求其次存明文；
//! - 旧版本写过的明文（`Value::String`）读兼容，由 [`migrate`] 在下一次
//!   保存/登录时重封成信封。
//!
//! # 绝不回显 hash
//!
//! 本模块返回的 [`AppId`] 本身 `Debug` 就不打 hash；上层 status/JSON 只报
//! `id` 与 `builtin/configured`，没有任何路径把 hash 塞进输出。

use omy_config::Remote;
use omy_secret::{Envelope, ProtectKey};
use toml::Value;

use super::appid::{AppId, AppIdError};

/// 解析出的应用身份。
#[derive(Debug, Clone)]
pub struct ResolvedApp {
    /// 真正要拿去建连的那一对。
    pub app: AppId,
    /// 配置里是否填了自定义身份（`false` = 用内置那一份）。
    pub configured: bool,
    /// 存的还是旧明文（`Value::String`），建议尽快迁移成信封。
    pub legacy_plaintext: bool,
}

/// 解析失败。
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("{0}")]
    BadApp(#[from] AppIdError),
    /// 有加密信封，但本机凭据库不可用（无后端/被锁），解不开。
    #[error("api_hash 已加密，但本机凭据库不可用，无法解开；请在凭据库可用时重试，或重置为内置身份")]
    NoProtector,
    /// 信封损坏或密钥对不上。
    #[error("api_hash 信封无法解开（凭据库已变更或配置损坏）")]
    UnsealFailed,
}

/// 保存失败。
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("加密 api_hash 失败")]
    SealFailed,
    #[error("编码 api_hash 信封失败")]
    EncodeFailed,
}

/// 从配置解析当前要用的应用身份。
///
/// - 两个字段都没有 → 内置那一份；
/// - 填全了 → 用自己的（旧明文直接用，信封用 `protector` 解开）；
/// - 只填一半 → 报错，**不静默回落内置**（那会让用户以为已生效）。
pub fn resolve(
    remote: &Remote,
    protector: Option<&ProtectKey>,
) -> Result<ResolvedApp, ResolveError> {
    let id = remote.telegram_api_id;
    match (id, remote.telegram_api_hash.as_ref()) {
        (None, None) => Ok(ResolvedApp {
            app: AppId::builtin(),
            configured: false,
            legacy_plaintext: false,
        }),
        (Some(id), Some(Value::String(plain))) => {
            // 旧明文：兼容读取。真正用于登录后由调用方触发 migrate。
            let app = AppId::custom(id, plain)?;
            Ok(ResolvedApp {
                app,
                configured: true,
                legacy_plaintext: true,
            })
        }
        (Some(id), Some(v)) => {
            // 新信封（表）。解不开时绝不用内置顶替——那会让「自己的对」
            // 静默变成「内置的对」，排查时最迷惑。
            let env: Envelope = v
                .clone()
                .try_into()
                .map_err(|_| ResolveError::UnsealFailed)?;
            let key = protector.ok_or(ResolveError::NoProtector)?;
            let pt = omy_secret::unseal(key, &env).map_err(|_| ResolveError::UnsealFailed)?;
            let hash = String::from_utf8_lossy(&pt);
            let app = AppId::custom(id, hash.as_ref())?;
            Ok(ResolvedApp {
                app,
                configured: true,
                legacy_plaintext: false,
            })
        }
        // 只填一半：与 AppId::from_config 同语义
        (Some(_), None) => Err(AppIdError::InvalidHash.into()),
        (None, Some(_)) => Err(AppIdError::InvalidId.into()),
    }
}

/// 把 api_hash 封成信封，准备写进配置。
///
/// 返回 `Ok(None)` 表示**没有保护器**——调用方必须据此报错，而不是退回去存
/// 明文。这是「无凭据库不写明文」这条硬约束的落点。
pub fn seal(
    protector: Option<&ProtectKey>,
    hash: &str,
) -> Result<Option<Value>, SealError> {
    let Some(key) = protector else {
        return Ok(None);
    };
    let env = omy_secret::seal(key, hash.as_bytes()).map_err(|_| SealError::SealFailed)?;
    let v = Value::try_from(env).map_err(|_| SealError::EncodeFailed)?;
    Ok(Some(v))
}

/// 若当前存的是旧明文，用保护器重封成信封；否则返回 `None`。
///
/// 用于「下一次登录/保存时安全迁移」：不动配置形状、只把那个字符串换成信封表。
pub fn migrate(remote: &Remote, protector: Option<&ProtectKey>) -> Option<Value> {
    match remote.telegram_api_hash.as_ref() {
        Some(Value::String(plain)) => seal(protector, plain).ok()?,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omy_config::Remote;

    fn remote_of(id: Option<i32>, hash: Option<Value>) -> Remote {
        let mut r = Remote::default();
        r.telegram_api_id = id;
        r.telegram_api_hash = hash;
        r
    }

    /// 两个字段都没有 → 内置。
    #[test]
    fn empty_falls_back_to_builtin() {
        let r = remote_of(None, None);
        let got = resolve(&r, None).unwrap();
        assert!(got.app.is_builtin());
        assert!(!got.configured);
        assert!(!got.legacy_plaintext);
    }

    /// 旧明文（Value::String）能读出身份，并标记为待迁移。
    #[test]
    fn legacy_plaintext_is_read_and_flagged() {
        let r = remote_of(Some(777), Some(Value::String("0123456789abcdef0123456789abcdef".into())));
        let got = resolve(&r, None).unwrap();
        assert_eq!(got.app.id(), 777);
        assert!(!got.app.is_builtin());
        assert!(got.legacy_plaintext, "字符串形态即旧明文");
        // migrate 在有保护器时给出信封
        let key = omy_secret::random_key();
        let env = migrate(&r, Some(&key)).expect("旧明文应能重封");
        assert!(matches!(env, Value::Table(_)), "迁移结果是信封表，不是字符串");
    }

    /// 信封 roundtrip：封进去 → resolve 解出来是同一对。
    #[test]
    fn envelope_roundtrip_recovers_same_identity() {
        let key = omy_secret::random_key();
        let plain = "0123456789abcdef0123456789abcdef";
        let stored = seal(Some(&key), plain).unwrap().unwrap();
        let r = remote_of(Some(777), Some(stored));
        let got = resolve(&r, Some(&key)).unwrap();
        assert_eq!(got.app.id(), 777);
        assert!(!got.legacy_plaintext, "信封形态不是旧明文");
        assert_eq!(got.app.hash(), plain);
    }

    /// 信封但拿不到保护器 → 报错，不用内置顶替。
    #[test]
    fn envelope_without_protector_is_an_error_not_a_fallback() {
        let key = omy_secret::random_key();
        let stored = seal(Some(&key), "0123456789abcdef0123456789abcdef").unwrap().unwrap();
        let r = remote_of(Some(777), Some(stored));
        let err = resolve(&r, None).unwrap_err();
        assert!(matches!(err, ResolveError::NoProtector));
    }

    /// 无保护器时 seal 返回 None——即拒绝写明文。
    #[test]
    fn no_protector_refuses_plaintext_storage() {
        assert!(seal(None, "0123456789abcdef0123456789abcdef").unwrap().is_none());
    }

    /// 只填一半必须报错，不回落内置。
    #[test]
    fn half_filled_is_an_error() {
        let r = remote_of(Some(777), None);
        assert!(resolve(&r, None).is_err());
        let r = remote_of(None, Some(Value::String("0123456789abcdef0123456789abcdef".into())));
        assert!(resolve(&r, None).is_err());
    }

    /// 自定义 id 确实被传进建连身份（id 等于配置值，而不是内置 2040）。
    ///
    /// 这钉死「只存不用」的回归：resolve 出来的 app.id 必须是用户填的那个。
    #[test]
    fn configured_id_reaches_the_app_identity() {
        let key = omy_secret::random_key();
        let stored = seal(Some(&key), "0123456789abcdef0123456789abcdef").unwrap().unwrap();
        let r = remote_of(Some(424242), Some(stored));
        let got = resolve(&r, Some(&key)).unwrap();
        assert_eq!(got.app.id(), 424242, "登录驱动拿到的必须是自定义 id，不是 2040");
        assert_ne!(got.app.id(), super::super::appid::BUILTIN_API_ID);
    }

    /// status JSON 绝不带 hash。
    #[test]
    fn status_payload_never_contains_hash() {
        let key = omy_secret::random_key();
        let plain = "0123456789abcdef0123456789abcdef";
        let stored = seal(Some(&key), plain).unwrap().unwrap();
        let r = remote_of(Some(777), Some(stored));
        let got = resolve(&r, Some(&key)).unwrap();
        // 与 CLI/GUI status 用的同一组字段
        let v = serde_json::json!({
            "builtin": got.app.is_builtin(),
            "effective_id": got.app.id(),
            "configured": got.configured,
        });
        let s = serde_json::to_string(&v).unwrap();
        assert!(!s.contains(plain), "status JSON 不得含 api_hash：{s}");
        assert_eq!(v["effective_id"], 777);
    }
}
