//! Telegram 的应用身份（api_id / api_hash）与它对 session 的约束。
//!
//! # 为什么这是独立一个模块
//!
//! 内置默认值有失效风险（`API_ID_PUBLISHED_FLOOD`，见 [`AppIdError`]），届时要能
//! **一处替换**。把数值散在连接、登录、设置三处，替换时必然漏一个，而漏掉的那处
//! 会让一部分用户继续撞在旧值上——现象是「有人能登录有人不能」，极难归因。
//!
//! # api_id 与 api_hash 的处置**刻意不同**
//!
//! | | 进 session 记录 | 进日志 / 错误信息 | 进 `Debug` |
//! |---|---|---|---|
//! | `api_id` | **是**（见下） | 是（它是公开的数字） | 是 |
//! | `api_hash` | **否** | **否** | **否** |
//!
//! `api_hash` 是凭据：拿到它加上一次登录就能冒充这个应用。所以它不落进任何
//! 持久化记录、不进日志、不进错误信息，`Debug` 也要手写以免被顺手带出去。
//!
//! 而 `api_id` **必须**跟 session 一起存，理由见 [`SessionIdentity`]。

use std::fmt;

/// 内置的应用身份：Telegram Desktop 官方客户端那一份。
///
/// # 为什么内置而不是让用户自己申请
///
/// 让用户先去 my.telegram.org 申请一对 api_id 才能用，是一道绝大多数人会
/// 直接放弃的门槛——而竞品（tdl 等）都内置了一份，用户的预期是「装上就能用」。
///
/// # 单点定义
///
/// **只有这里一处**。内置值有被服务端限流的风险（[`AppIdError::PublishedFlood`]），
/// 届时替换只需改这两行。散在多处的话替换时必然漏一个，而漏掉的那处会让一部分
/// 用户继续撞在旧值上——现象是「有人能登录有人不能」。
pub const BUILTIN_API_ID: i32 = 2040;

/// 内置 api_hash，与 [`BUILTIN_API_ID`] 配对。
///
/// **不要把它写进文档、日志或错误信息。** 它是凭据，不是配置项。
pub const BUILTIN_API_HASH: &str = "b18441a1ff607e10a989891a5462e627";

/// 一对应用身份。
///
/// `Clone` 但**不派生 `Debug`**：派生会把 `api_hash` 打出来，而 `Debug` 极容易
/// 被顺手塞进日志或错误信息里。手写的实现只报 id 与来源。
#[derive(Clone, PartialEq, Eq)]
pub struct AppId {
    /// 应用编号。公开信息，可以进日志与 session 记录。
    id: i32,
    /// 应用哈希。**凭据**，不进任何持久化记录与日志。
    hash: String,
    /// 是不是内置那一份。
    ///
    /// 用来区分 `API_ID_PUBLISHED_FLOOD` 的两种处境：撞在内置值上要引导用户
    /// 去填自己的，而撞在用户自己的值上说明是他自己那对被限流了，引导他改填
    /// 只会让他换来换去。
    builtin: bool,
}

impl fmt::Debug for AppId {
    /// 只报 id 与来源，**绝不打印 hash**。
    ///
    /// 不这样会怎样：一次 `dbg!` 或一句 `format!("{:?}")` 进了日志文件，
    /// 应用凭据就留在磁盘上了——而这不会有任何征兆。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppId")
            .field("id", &self.id)
            .field("builtin", &self.builtin)
            .finish_non_exhaustive()
    }
}

impl Default for AppId {
    fn default() -> Self {
        Self::builtin()
    }
}

impl AppId {
    /// 内置的那一份。
    #[must_use]
    pub fn builtin() -> Self {
        Self {
            id: BUILTIN_API_ID,
            hash: String::from(BUILTIN_API_HASH),
            builtin: true,
        }
    }

    /// 用户自己填的一份。
    ///
    /// # Errors
    ///
    /// id 非正、或 hash 为空 / 明显不是 32 位十六进制时返回 —— 在这里拦住是为了
    /// 让用户当场知道填错了。放过去的话，服务端会回一个
    /// `CONNECTION_API_ID_INVALID`，而界面上只能显示一句「登录失败」，
    /// 用户不会想到是自己填的那串数字有问题。
    pub fn custom(id: i32, hash: impl Into<String>) -> Result<Self, AppIdError> {
        let hash = hash.into();
        if id <= 0 {
            return Err(AppIdError::InvalidId);
        }
        // Telegram 的 api_hash 一律是 32 位小写十六进制。只校验形状，不校验内容
        // ——真假只有服务端知道，但形状错了本地就能拦。
        if hash.len() != 32 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(AppIdError::InvalidHash);
        }
        Ok(Self {
            id,
            hash,
            builtin: false,
        })
    }

    /// 按配置取用：填了自己的就用自己的，否则用内置那一份。
    ///
    /// 配置里存的 hash 为空视为「没填」而不是「填了空的」：用户可能只填了 id
    /// 就保存了，此时回落到内置值比报错更符合他的预期（他要的是能用）。
    ///
    /// # Errors
    ///
    /// 用户填了但填得不合法时返回 —— **不静默回落到内置值**。回落会让用户以为
    /// 自己填的那对生效了，而实际上没有；等他真的因为内置值被限流来查时，
    /// 会发现「明明填了却没用」。
    pub fn from_config(custom_id: Option<i32>, custom_hash: Option<&str>) -> Result<Self, AppIdError> {
        match (custom_id, custom_hash.map(str::trim).filter(|s| !s.is_empty())) {
            (Some(id), Some(h)) => Self::custom(id, h),
            // 只填了一半：明确报错而不是回落。这是用户填错了，
            // 而静默回落会让他以为已经生效
            (Some(_), None) => Err(AppIdError::InvalidHash),
            (None, Some(_)) => Err(AppIdError::InvalidId),
            (None, None) => Ok(Self::builtin()),
        }
    }

    /// 应用编号。
    #[must_use]
    pub const fn id(&self) -> i32 {
        self.id
    }

    /// 应用哈希。
    ///
    /// 只给需要它建连的那一处用。**不要把返回值写进日志、错误信息或任何
    /// 持久化记录。**
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// 是不是内置那一份。
    #[must_use]
    pub const fn is_builtin(&self) -> bool {
        self.builtin
    }
}

/// 应用身份相关的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppIdError {
    /// api_id 不是正整数。
    #[error("api_id 必须是正整数")]
    InvalidId,
    /// api_hash 不是 32 位十六进制。
    ///
    /// 错误信息里**不能**回显用户填的值：那串东西本身就是凭据，
    /// 而错误信息常被复制进工单或日志。
    #[error("api_hash 必须是 32 位十六进制字符")]
    InvalidHash,
    /// 服务端对这个 api_id 限流了（`API_ID_PUBLISHED_FLOOD`）。
    ///
    /// # 这一条必须单独成一类，不能混进通用重试
    ///
    /// 它长得像限流，但**重试永远不会成功**：服务端拒绝的是这个 api_id 本身，
    /// 因为它被公开使用得太多。混进通用重试逻辑的话，界面会无限转圈，
    /// 而出路就在旁边——让用户填一对自己的 api_id 就好了。
    ///
    /// 选内置官方 AppID 这个方案**降低的是它出现的概率，不是消掉它**，
    /// 所以这条自救路径必须存在。
    #[error("这个 api_id 被服务端限流，请在设置里填入你自己的 api_id")]
    PublishedFlood,
}

impl AppIdError {
    /// 界面该不该引导用户去填自己的 api_id。
    ///
    /// 只有「撞在**内置**值上被限流」才该引导。用户自己那对被限流时引导他再改，
    /// 只会让他在两对之间换来换去——那时该告诉他的是「等一等或换一对」。
    #[must_use]
    pub const fn suggests_custom_app_id(&self, current_is_builtin: bool) -> bool {
        matches!(self, Self::PublishedFlood) && current_is_builtin
    }

    /// 能不能靠重试解决。
    ///
    /// 三种都不能：前两种是用户填错了，第三种重试永远不会成功。
    /// 把它们当成可重试错误会让界面无限转圈。
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        false
    }
}

/// 一份已保存的登录态属于哪个应用身份。
///
/// # 为什么 api_id 必须跟 session 一起存
///
/// auth key 是在某个 api_id 下协商出来的，服务端把两者绑在一起。换了 api_id
/// 继续用同一份 auth key，请求会以难解释的方式失败（而不是干脆地报「凭据不
/// 匹配」）。tdl 从 tdata 导入登录态时必须同时把应用身份设成 2040，就是这个
/// 原因。
///
/// 所以持久化时把 api_id 一起存下来，加载时比对；不一致就要求重新登录。
///
/// **只存 api_id，不存 api_hash**：比对只需要 id，而 hash 是凭据，
/// 多存一处就多一处泄露面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionIdentity {
    /// 这份 session 是用哪个 api_id 建立的。
    pub api_id: i32,
}

impl SessionIdentity {
    /// 记下当前用的应用身份。
    #[must_use]
    pub const fn of(app: &AppId) -> Self {
        Self { api_id: app.id }
    }

    /// 这份 session 还能不能配着 `app` 用。
    #[must_use]
    pub const fn matches(&self, app: &AppId) -> bool {
        self.api_id == app.id
    }

    /// 校验并给出「为什么要重新登录」。
    ///
    /// # Errors
    ///
    /// api_id 变了时返回 [`SessionMismatch`]。
    ///
    /// # 为什么要返回一个能解释原因的错误，而不是一个布尔
    ///
    /// 不匹配时界面要说的是「你改了 api_id，需要重新登录一次」，而**不是**
    /// 「登录已失效」。后者会让用户以为账号出了问题——去官方客户端查会话、
    /// 怀疑被盗号，而真正的原因是他刚在设置里改了一个数字。
    pub const fn check(&self, app: &AppId) -> Result<(), SessionMismatch> {
        if self.api_id == app.id {
            Ok(())
        } else {
            Err(SessionMismatch {
                saved_api_id: self.api_id,
                current_api_id: app.id,
            })
        }
    }
}

/// 已保存的 session 与当前应用身份不匹配。
///
/// 两个 api_id 都带上：它们是公开的数字，写进日志有助于排查
/// （「原来是从 2040 换成了自己的那个」）。**这里不会出现 api_hash。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "已保存的登录态是用 api_id {saved_api_id} 建立的，当前配置是 {current_api_id}；\
     应用凭据变更后登录态无法沿用，需要重新登录一次（与账号本身无关）"
)]
pub struct SessionMismatch {
    /// session 里记着的那个。
    pub saved_api_id: i32,
    /// 现在配置要用的那个。
    pub current_api_id: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置身份就是定稿的那一对。
    ///
    /// 不这样会怎样：内置值写错一个字符，所有不填自己 api_id 的用户都登录不了，
    /// 而错误是服务端回的 `CONNECTION_API_ID_INVALID`，界面上只是一句
    /// 「登录失败」——没人会想到是内置常量敲错了。
    #[test]
    fn builtin_identity_is_the_decided_pair() {
        let a = AppId::builtin();
        assert_eq!(a.id(), 2040);
        assert_eq!(a.hash().len(), 32, "api_hash 必须是 32 位");
        assert!(a.hash().chars().all(|c| c.is_ascii_hexdigit()));
        assert!(a.is_builtin());
    }

    /// `Debug` 绝不能带出 api_hash。
    ///
    /// 不这样会怎样：一次 `dbg!` 或一句 `format!("{:?}")` 进了日志文件，
    /// 应用凭据就留在磁盘上了，而这不会有任何征兆。
    #[test]
    fn debug_never_leaks_api_hash() {
        for a in [
            AppId::builtin(),
            AppId::custom(12345, "0123456789abcdef0123456789abcdef").expect("合法"),
        ] {
            let text = format!("{a:?}");
            assert!(
                !text.contains(a.hash()),
                "Debug 里出现了 api_hash：{text}"
            );
            // id 可以有：它是公开信息，排查时有用
            assert!(text.contains(&a.id().to_string()), "id 应当可见便于排查");
        }
    }

    /// 错误信息里不能回显用户填的 api_hash。
    ///
    /// 不这样会怎样：错误信息常被用户直接复制进工单或贴到论坛求助，
    /// 而那串东西是凭据。
    #[test]
    fn errors_never_echo_the_hash() {
        let bad = "ZZZZ567890abcdef0123456789abcdef";
        let err = AppId::custom(1, bad).expect_err("非十六进制应当被拒");
        let text = err.to_string();
        assert!(!text.contains(bad), "错误信息里出现了用户填的值：{text}");
        assert!(!text.contains("ZZZZ"));
    }

    /// 形状不对的自定义身份要当场拦住。
    ///
    /// 不这样会怎样：放过去之后服务端回 `CONNECTION_API_ID_INVALID`，
    /// 界面只能显示「登录失败」，用户不会想到是自己填的那串有问题。
    #[test]
    fn malformed_custom_identity_is_refused_locally() {
        assert_eq!(AppId::custom(0, "0123456789abcdef0123456789abcdef"), Err(AppIdError::InvalidId));
        assert_eq!(AppId::custom(-5, "0123456789abcdef0123456789abcdef"), Err(AppIdError::InvalidId));
        // 太短 / 太长 / 含非十六进制
        assert_eq!(AppId::custom(1, "abc"), Err(AppIdError::InvalidHash));
        assert_eq!(
            AppId::custom(1, "0123456789abcdef0123456789abcdef0"),
            Err(AppIdError::InvalidHash)
        );
        assert_eq!(
            AppId::custom(1, "g123456789abcdef0123456789abcdef"),
            Err(AppIdError::InvalidHash)
        );
        // 合法的要放过
        assert!(AppId::custom(2040, "b18441a1ff607e10a989891a5462e627").is_ok());
        // 大写十六进制也该接受：用户从网页复制时大小写不一定
        assert!(AppId::custom(1, "0123456789ABCDEF0123456789ABCDEF").is_ok());
    }

    /// 没填就用内置，填全了就用自己的。
    #[test]
    fn config_picks_custom_over_builtin() {
        let none = AppId::from_config(None, None).expect("不填应当回落内置");
        assert!(none.is_builtin());
        assert_eq!(none.id(), BUILTIN_API_ID);

        let mine = AppId::from_config(Some(777), Some("0123456789abcdef0123456789abcdef"))
            .expect("填全了应当用自己的");
        assert!(!mine.is_builtin());
        assert_eq!(mine.id(), 777);

        // 空白 hash 视为没填（用户可能只填了 id 就保存）
        assert!(AppId::from_config(None, Some("   ")).expect("空白视为没填").is_builtin());
    }

    /// 只填了一半必须报错，**不能静默回落到内置值**。
    ///
    /// 不这样会怎样：用户以为自己填的那对生效了，实际用的还是内置值；
    /// 等他真因为内置值被限流来排查时，会发现「明明填了却没用」——
    /// 而那时他最需要的恰恰是这条自救路径真的生效。
    #[test]
    fn half_filled_config_is_an_error_not_a_fallback() {
        assert_eq!(AppId::from_config(Some(777), None), Err(AppIdError::InvalidHash));
        assert_eq!(
            AppId::from_config(None, Some("0123456789abcdef0123456789abcdef")),
            Err(AppIdError::InvalidId)
        );
    }

    /// `API_ID_PUBLISHED_FLOOD` 不可重试，且只在用内置值时引导用户去填自己的。
    ///
    /// 不这样会怎样：当成普通限流去重试，界面会无限转圈，而出路（填一对自己的
    /// api_id）就在旁边；反过来，用户自己那对被限流时还引导他改填，
    /// 只会让他在两对之间换来换去。
    #[test]
    fn published_flood_is_not_retryable_and_guides_only_when_builtin() {
        let e = AppIdError::PublishedFlood;
        assert!(!e.is_retryable(), "重试永远不会成功");
        assert!(e.suggests_custom_app_id(true), "撞在内置值上要引导用户填自己的");
        assert!(
            !e.suggests_custom_app_id(false),
            "用户自己那对被限流时不该再引导他改填"
        );
        // 另两种错误任何时候都不触发这条引导
        assert!(!AppIdError::InvalidId.suggests_custom_app_id(true));
        assert!(!AppIdError::InvalidHash.suggests_custom_app_id(true));
    }

    /// session 记录只存 api_id，不存 api_hash。
    ///
    /// 不这样会怎样：应用凭据被写进一个长期留在磁盘上的文件，
    /// 而比对身份只需要那个公开的数字，根本不需要 hash。
    #[test]
    fn session_record_stores_only_the_public_id() {
        let app = AppId::builtin();
        let rec = SessionIdentity::of(&app);
        let json = serde_json::to_string(&rec).expect("应可序列化");
        assert!(json.contains("2040"));
        assert!(
            !json.contains(BUILTIN_API_HASH),
            "session 记录里绝不能出现 api_hash：{json}"
        );
        // 字段就只有一个，多出字段说明有人把别的东西也塞进来了
        let v: serde_json::Value = serde_json::from_str(&json).expect("应可解析");
        assert_eq!(
            v.as_object().map(serde_json::Map::len),
            Some(1),
            "这条记录只该有 api_id 一个字段：{json}"
        );
    }

    /// 换了 api_id 之后旧 session 必须被拒，且原因要说清楚。
    ///
    /// 不这样会怎样：auth key 是在某个 api_id 下协商的，换了 id 继续复用会以
    /// 难解释的方式失败。而若只报「登录已失效」，用户会以为账号出了问题——
    /// 去官方客户端查会话、怀疑被盗号，真正原因只是他刚改了一个数字。
    #[test]
    fn changed_api_id_invalidates_saved_session_with_a_clear_reason() {
        let builtin = AppId::builtin();
        let mine = AppId::custom(999, "0123456789abcdef0123456789abcdef").expect("合法");
        let saved = SessionIdentity::of(&builtin);

        assert!(saved.matches(&builtin), "同一个 api_id 应当沿用");
        assert!(saved.check(&builtin).is_ok());

        assert!(!saved.matches(&mine), "换了 api_id 不能沿用");
        let err = saved.check(&mine).expect_err("应当拒绝");
        assert_eq!(err.saved_api_id, 2040);
        assert_eq!(err.current_api_id, 999);

        let text = err.to_string();
        // 要点名「凭据变更」并澄清与账号无关，否则用户会往盗号方向想
        assert!(text.contains("2040") && text.contains("999"), "两个 id 都要写出来便于排查");
        assert!(text.contains("重新登录"), "要告诉用户该做什么");
        assert!(text.contains("与账号本身无关"), "必须澄清不是账号出了问题");
        // 凭据不能出现在这条信息里
        assert!(!text.contains(BUILTIN_API_HASH));
    }
}
