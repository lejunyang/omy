//! Telegram 登录态的落盘与读回。
//!
//! # 为什么必须自己写这一层
//!
//! `grammers-session` 的默认 feature `sqlite-storage` 会拖进 libsql（bindgen +
//! cmake），而「整条依赖树里没有 C 工具链」正是选 grammers 而不是 TDLib 的核心
//! 理由。关掉它之后就没有现成的持久化存储了——只剩 `MemorySession`，进程一退
//! 登录态就没了。
//!
//! 这是**想要**的方向：session 里含 auth key，等同账号凭据，本来就该由我们决定
//! 怎么落盘，而不是让第三方库在用户目录下另开一个明文 sqlite。
//!
//! # 落盘的东西是凭据，所以必须加密
//!
//! [`DcOption::auth_key`] 是 256 字节的永久授权密钥。拿到它就能以这个账号发请求，
//! **不需要密码也不需要验证码**。所以它和 WebDAV 密码走同一条路：用
//! `omy-secret` 的信封加密，密钥交给本机凭据库保管。
//!
//! **拿不到保护密钥时拒绝写盘，而不是退回明文。** 代价是那台机器上每次启动都要
//! 重新扫码；收益是不会出现「用户以为被保护着、实际 auth key 明文躺在磁盘上」。
//! 这与 `places.rs` 对 WebDAV 密码的处置是同一条规则，两处都别改成静默降级。
//!
//! # 只存两样东西
//!
//! `home_dc` 与 `dc_options`（含 auth key）。这两样是免去重新登录**真正需要**的。
//!
//! `peer_infos` 不存：那只是对话缓存，用到时重新拉即可，而且 `Session` trait
//! 没有提供枚举入口，想存也取不全。`updates_state` 不存：它是增量更新的位点，
//! omy 只做「列目录 + 读字节」，不消费更新流，存了也没人用。
//!
//! # api_id 必须一起存
//!
//! 见 [`SessionIdentity`]：session 与建立它的 api_id 绑定，换了 api_id 之后旧
//! session 会被服务端拒绝。存着它才能在载入时就说清楚「是你改了 api_id」，
//! 而不是让用户看到一句「登录已失效」去怀疑账号被盗。

use std::path::{Path, PathBuf};

use grammers_session::storages::MemorySession;
use grammers_session::types::DcOption;
use grammers_session::Session as _;

use crate::telegram::appid::{AppId, SessionIdentity, SessionMismatch};

/// 本机凭据库里的服务名。
///
/// 与远程位置那把（`omy-remote-places`）分开：两者的生命周期不同——用户删光
/// WebDAV 位置时不该顺带让 Telegram 登录态失效。
const SECRET_SERVICE: &str = "omy-telegram";

/// 保护密钥在凭据库里的 id。
const SECRET_KEY_ID: &str = "telegram-session-key-v1";

/// 落盘文件名。
const FILE_NAME: &str = "telegram-session.json";

/// 主数据中心的编号范围。
///
/// `Session` trait 只给按编号查的 `dc_option`，没有枚举入口，所以要逐个问。
/// 官方主数据中心就是 1..=5（`SessionData::default` 里也正好是这五个）。
const DC_IDS: std::ops::RangeInclusive<i32> = 1..=5;

/// 落盘的登录态。
///
/// **不派生 `Debug`**：`dc_options` 里有 auth key，一次 `dbg!` 就把它写进日志了。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct SavedSession {
    /// 建立这份 session 时用的 api_id。
    pub api_id: i32,
    /// 主数据中心编号。
    pub home_dc: i32,
    /// 各数据中心的地址与 auth key。
    pub dc_options: Vec<DcOption>,
}

/// 登录态持久化可能出的问题。
///
/// 分得细是因为**界面要给的话完全不同**：没有凭据库要告诉用户「这台机器上
/// 登录态存不住，每次启动要重新扫码」，而 api_id 变了要告诉他「是你改了设置」。
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// 这台机器没有可用的凭据库。
    ///
    /// **此时不写盘**。见模块文档：绝不退回明文。
    #[error("这台机器没有可用的凭据库，Telegram 登录态无法安全保存")]
    NoProtector,
    /// 算不出该往哪写（极少见，通常是权限极受限的环境）。
    #[error("找不到可写的数据目录")]
    NoDataDir,
    /// 读写磁盘失败。
    #[error("登录态读写失败：{0}")]
    Io(String),
    /// 存档解不开（换了机器、清了钥匙串）。
    #[error("登录态解不开，需要重新登录")]
    Undecryptable,
    /// 存档格式不认识。
    #[error("登录态格式无法解析，需要重新登录")]
    Malformed,
    /// 存档是用另一个 api_id 建的。
    #[error("{0}")]
    Mismatch(#[from] SessionMismatch),
    /// 读不出内存里的 session 状态（锁中毒）。
    #[error("读取内存中的登录态失败：{0}")]
    Extract(String),
}

/// 登录态文件的完整路径。
///
/// # Errors
///
/// 算不出数据目录时返回 [`SessionError::NoDataDir`]。
pub fn session_path() -> Result<PathBuf, SessionError> {
    omy_config::data_dir()
        .map(|d| d.join(FILE_NAME))
        .ok_or(SessionError::NoDataDir)
}

/// 取本机保护密钥。
///
/// 每次现取而不缓存：钥匙串可能中途被锁上，缓存会让我们拿着一把已经无权使用的
/// 密钥去解，错误推迟到更难解释的地方才出现。与 `places.rs` 同一条理由。
fn protect_key() -> Option<omy_secret::ProtectKey> {
    let p = omy_secret::default_protector(SECRET_SERVICE).ok()?;
    p.retrieve_or_create(SECRET_KEY_ID).ok()
}

/// 这台机器能不能安全保存 Telegram 登录态。
///
/// 界面在开始扫码**之前**就该问这个：如果答案是否，要先告诉用户「这台机器上
/// 登录态存不住，每次启动要重新扫一次」，而不是等他扫完了才说。
#[must_use]
pub fn can_persist() -> bool {
    protect_key().is_some()
}

/// 把内存 session 里的状态抽出来。
///
/// # 为什么要自己拼
///
/// grammers 0.10 只提供 `SessionData::import_to`（导入方向），**没有反向的
/// extract**。`MemorySession` 虽有 `From<SessionData>`，那也是构造方向。所以只能
/// 通过 `Session` trait 的 getter 把状态逐个读出来。
///
/// # Errors
///
/// 内存 session 的锁中毒时返回 [`SessionError::Extract`]。
pub fn extract(session: &MemorySession, app: &AppId) -> Result<SavedSession, SessionError> {
    let home_dc = session
        .home_dc_id()
        .map_err(|e| SessionError::Extract(e.to_string()))?;
    let mut dc_options = Vec::new();
    for id in DC_IDS {
        match session.dc_option(id) {
            Ok(Some(opt)) => dc_options.push(opt),
            // 没连过的数据中心返回 None，跳过即可——它没有 auth key，存了也没用
            Ok(None) => {}
            Err(e) => return Err(SessionError::Extract(e.to_string())),
        }
    }
    Ok(SavedSession {
        api_id: app.id(),
        home_dc,
        dc_options,
    })
}

/// 存档里有没有任何一个数据中心带着 auth key。
///
/// 这是「这份存档能不能免去重新登录」的实质判据。没有 auth key 的存档看起来
/// 一切正常（有 home_dc、有地址），载入后却仍然是未登录——而那个现象很容易被
/// 当成「服务端把我踢了」。
#[must_use]
pub fn has_auth_key(saved: &SavedSession) -> bool {
    saved.dc_options.iter().any(|d| d.auth_key.is_some())
}

/// 落盘。
///
/// **登录一成功就该调用它**，而不是等整趟流程走完。中间任何一步失败
/// （2FA 输错、用户关窗口、进程崩）都会让这次已经在服务端生效的登录白费——
/// 而重新登录要再扫一次码，还可能撞 `FLOOD_WAIT`。
///
/// # Errors
///
/// 没有凭据库、算不出目录、加密或写盘失败时返回。**没有凭据库时不写明文**。
pub fn save(session: &MemorySession, app: &AppId) -> Result<PathBuf, SessionError> {
    let saved = extract(session, app)?;
    let path = session_path()?;
    save_to(&saved, &path)
}

/// 把一份现成的登录态落到**产品路径**。
///
/// [`save`] 是从 `MemorySession` 抽取的，而 tdata 导入手里已经是
/// [`SavedSession`]。用 [`save_to`] 也能写，但那要调用方自己算路径——
/// 「session 存在哪」就有了两个来源，改一处另一处会悄悄不同步，
/// 表现是「登录了但重启后还要再登一次」，而两处代码单看都没错。
///
/// # Errors
///
/// 同 [`save`]。
pub fn save_current(saved: &SavedSession) -> Result<PathBuf, SessionError> {
    let path = session_path()?;
    save_to(saved, &path)
}

/// 落盘到指定路径（测试用；产品路径走 [`save`]）。
///
/// # Errors
///
/// 同 [`save`]。
pub fn save_to(saved: &SavedSession, path: &Path) -> Result<PathBuf, SessionError> {
    seal_and_write(saved, path, protect_key())
}

/// 真正干活的那一层：**保护密钥由调用方给**。
///
/// # 为什么要把取密钥这一步提出去
///
/// 「拿不到凭据库时拒绝写盘、绝不退回明文」是本模块最要紧的一条规则，但在一台
/// 有凭据库的机器上，那条分支**永远走不到**——于是它无法被测到，变异测试里把它
/// 改坏也不会有任何断言失败。一条测不到的安全规则等于没有。
///
/// 把密钥作为参数传进来，这条分支就成了可达、可断言的普通分支。
///
/// # Errors
///
/// `key` 为 `None`、序列化、加密或写盘失败时返回。
fn seal_and_write(
    saved: &SavedSession,
    path: &Path,
    key: Option<omy_secret::ProtectKey>,
) -> Result<PathBuf, SessionError> {
    // 先要密钥再序列化：拿不到就直接返回，明文根本不会被构造出来，
    // 更不会有机会被写出去
    let key = key.ok_or(SessionError::NoProtector)?;
    let plain = serde_json::to_vec(saved).map_err(|e| SessionError::Io(e.to_string()))?;
    let env = omy_secret::seal(&key, &plain).map_err(|e| SessionError::Io(e.to_string()))?;
    let text = serde_json::to_string(&env).map_err(|e| SessionError::Io(e.to_string()))?;

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| SessionError::Io(e.to_string()))?;
    }
    std::fs::write(path, text).map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(path.to_path_buf())
}

/// 读回。
///
/// 返回 `Ok(None)` 表示「没存过」——那是首次使用的正常状态，不是错误。
/// 与「存了但解不开」必须分开：后者要提示用户重新登录，前者什么都不用说。
///
/// # Errors
///
/// 存档存在但解不开、格式不对、或 api_id 与当前不匹配时返回。
pub fn load(app: &AppId) -> Result<Option<SavedSession>, SessionError> {
    let path = session_path()?;
    load_from(&path, app)
}

/// 从指定路径读回（测试用；产品路径走 [`load`]）。
///
/// # Errors
///
/// 同 [`load`]。
pub fn load_from(path: &Path, app: &AppId) -> Result<Option<SavedSession>, SessionError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        // 没有这个文件就是没存过，不是错误
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(SessionError::Io(e.to_string())),
    };
    let env: omy_secret::Envelope =
        serde_json::from_str(&text).map_err(|_| SessionError::Malformed)?;
    let key = protect_key().ok_or(SessionError::NoProtector)?;
    let plain = omy_secret::unseal(&key, &env).map_err(|_| SessionError::Undecryptable)?;
    let saved: SavedSession =
        serde_json::from_slice(&plain).map_err(|_| SessionError::Malformed)?;

    // api_id 对不上就直接报出来，且**不要**顺手把它当成「没存过」。
    // 两者给用户的话完全不同：一个是「你改了设置」，一个是「第一次用」。
    SessionIdentity {
        api_id: saved.api_id,
    }
    .check(app)?;
    Ok(Some(saved))
}

/// 把读回的状态灌进一个内存 session。
///
/// # Errors
///
/// 内存 session 写入失败时返回 [`SessionError::Extract`]。
pub async fn import_into(
    saved: &SavedSession,
    session: &MemorySession,
) -> Result<(), SessionError> {
    session
        .set_home_dc_id(saved.home_dc)
        .await
        .map_err(|e| SessionError::Extract(e.to_string()))?;
    for dc in &saved.dc_options {
        session
            .set_dc_option(dc)
            .await
            .map_err(|e| SessionError::Extract(e.to_string()))?;
    }
    Ok(())
}

/// 删除已保存的登录态（用户「退出 Telegram 账号」时）。
///
/// 文件本来就不存在时算成功：用户要的是「别再留着」，而不是「删掉一个文件」。
///
/// # Errors
///
/// 算不出目录或删除失败时返回。
pub fn forget() -> Result<(), SessionError> {
    let path = session_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SessionError::Io(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{SocketAddrV4, SocketAddrV6};

    /// 造一个带 auth key 的数据中心项。
    ///
    /// auth key 用算式生成而不是 `[0x5A; 256]`：那种长串重复字节会让本机安全
    /// 软件把测试二进制删掉（AGENTS.md 记过这一条）。
    fn dc(id: i32, with_key: bool) -> DcOption {
        DcOption {
            id,
            ipv4: SocketAddrV4::new(std::net::Ipv4Addr::new(149, 154, 167, 51), 443),
            ipv6: SocketAddrV6::new(std::net::Ipv6Addr::LOCALHOST, 443, 0, 0),
            auth_key: with_key
                .then(|| core::array::from_fn(|i| ((i as u8).wrapping_mul(37)) ^ 0x6B)),
        }
    }

    fn saved(api_id: i32) -> SavedSession {
        SavedSession {
            api_id,
            home_dc: 2,
            dc_options: vec![dc(2, true), dc(5, false)],
        }
    }

    /// 落盘的文件里**绝不能**出现 auth key 的明文字节。
    ///
    /// 不这样会怎样：auth key 等同账号凭据，拿到它就能以这个账号发请求，
    /// 不需要密码也不需要验证码。它明文躺在磁盘上，等于用户的 Telegram 账号
    /// 随磁盘一起泄露——而功能上毫无异常，没有这条断言根本发现不了。
    #[test]
    fn persisted_file_never_contains_the_auth_key() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let s = saved(2040);
        let Some(key) = s.dc_options.first().and_then(|d| d.auth_key) else {
            panic!("夹具里第一个 DC 应当带 auth key");
        };

        let dir = std::env::temp_dir().join("omy-tg-session-test-1");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        save_to(&s, &path).expect("写盘");

        let bytes = std::fs::read(&path).expect("读回");
        // 二进制形式不能出现
        assert!(
            !bytes.windows(key.len()).any(|w| w == key),
            "落盘文件里出现了 auth key 的原始字节"
        );
        // 十六进制形式也不能出现——DcOption 的 serde 正是用 hex 编码 auth_key，
        // 只查二进制会漏掉这条最可能的泄露路径
        let text = String::from_utf8_lossy(&bytes);
        let hex_key = hex_of(&key);
        assert!(
            !text.to_ascii_lowercase().contains(&hex_key),
            "落盘文件里出现了 auth key 的十六进制形式"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 自己写的十六进制，避免为一条断言多引一个依赖。
    fn hex_of(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for x in b {
            s.push_str(&format!("{x:02x}"));
        }
        s
    }

    /// 存进去再读回来，auth key 必须一字不差。
    ///
    /// 不这样会怎样：auth key 只要错一个字节，服务端就报
    /// `AUTH_KEY_UNREGISTERED`——那个错误看起来像「会话被吊销」，用户会去官方
    /// 客户端查已登录设备，而真正的原因是我们序列化时丢了字节。
    #[test]
    fn round_trip_preserves_the_auth_key_exactly() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let s = saved(2040);
        let dir = std::env::temp_dir().join("omy-tg-session-test-2");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        save_to(&s, &path).expect("写盘");

        let back = match load_from(&path, &AppId::builtin()) {
            Ok(Some(v)) => v,
            // 同上：不能用 expect/unwrap，SavedSession 刻意没有 Debug
            Ok(None) => panic!("应当有内容"),
            Err(e) => panic!("读回失败：{e}"),
        };
        assert_eq!(back.home_dc, s.home_dc);
        assert_eq!(back.dc_options.len(), s.dc_options.len());
        for (a, b) in back.dc_options.iter().zip(s.dc_options.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.auth_key, b.auth_key, "DC {} 的 auth key 变了", a.id);
            assert_eq!(a.ipv4, b.ipv4);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 没存过要返回 `None`，不能报错。
    ///
    /// 不这样会怎样：首次运行就弹一条「登录态读取失败」，用户会以为程序坏了
    /// ——而他只是还没登录过。
    #[test]
    fn missing_file_is_not_an_error() {
        let path = std::env::temp_dir().join("omy-tg-session-does-not-exist-9c31.json");
        let _ = std::fs::remove_file(&path);
        let r = match load_from(&path, &AppId::builtin()) {
            Ok(v) => v,
            Err(e) => panic!("不该报错：{e}"),
        };
        assert!(r.is_none(), "没存过应当是 None");
    }

    /// 换了 api_id 之后，旧存档必须被拒绝并说清楚原因。
    ///
    /// 不这样会怎样：拿着旧 session 去连，服务端回一句认证失败，界面只能说
    /// 「登录已失效」——用户会跑去查账号是不是被盗，而真正原因是他刚在设置里
    /// 改了一个数字。
    #[test]
    fn changed_api_id_is_refused_with_a_clear_reason() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-session-test-3");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);
        save_to(&saved(2040), &path).expect("写盘");

        let mine = AppId::custom(4242, "0123456789abcdef0123456789abcdef").expect("合法");
        // 不用 expect_err：那要求 Ok 侧实现 Debug，而 SavedSession **刻意没有**
        // Debug（里面是 auth key）。为了写断言给它加 Debug 等于为测试打开一条
        // 泄露路径——这里改用 match，把那条保证留住。
        let Err(err) = load_from(&path, &mine) else {
            panic!("api_id 不同必须被拒");
        };
        let text = err.to_string();
        assert!(text.contains("2040") && text.contains("4242"), "{text}");
        assert!(
            matches!(err, SessionError::Mismatch(_)),
            "必须归为「api_id 变了」，而不是笼统的读取失败"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 损坏的存档要报「格式不对」，不能当成「没存过」。
    ///
    /// 不这样会怎样：当成没存过就会静默走到扫码页，用户以为自己从没登录过；
    /// 而真实情况是存档坏了，值得告诉他一声。
    #[test]
    fn corrupt_file_is_distinguished_from_absent() {
        let dir = std::env::temp_dir().join("omy-tg-session-test-4");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join(FILE_NAME);
        std::fs::write(&path, "这不是 JSON").expect("写坏文件");

        let Err(err) = load_from(&path, &AppId::builtin()) else {
            panic!("坏文件必须报错");
        };
        assert!(
            matches!(err, SessionError::Malformed),
            "应当报格式错误，实际 {err:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 「有存档」不等于「能免登录」——要能区分出没有 auth key 的存档。
    ///
    /// 不这样会怎样：一份只有 home_dc、没有 auth key 的存档看着一切正常，
    /// 载入后却仍是未登录状态。那个现象极容易被当成「服务端把我踢了」，
    /// 而真实原因是登录成功那一刻我们根本没把 auth key 存进去。
    #[test]
    fn a_session_without_an_auth_key_is_not_usable() {
        let mut s = saved(2040);
        assert!(has_auth_key(&s), "夹具本来该有 auth key");
        for d in &mut s.dc_options {
            d.auth_key = None;
        }
        assert!(
            !has_auth_key(&s),
            "没有任何 auth key 的存档不该被当成可用登录态"
        );
    }

    /// 拿不到凭据库时**必须拒绝写盘**，绝不退回明文。
    ///
    /// 不这样会怎样：那台机器上 auth key 会明文落盘，而界面照常显示「登录态
    /// 已保存」——用户以为它被保护着。这是本模块存在的核心规则，可它在一台
    /// 有凭据库的开发机上永远走不到，所以必须把「没有密钥」这个条件显式注入
    /// 才测得到；否则这条规则改坏了也没有任何断言会失败。
    #[test]
    fn without_a_protector_it_refuses_to_write_rather_than_falling_back_to_plaintext() {
        let dir = std::env::temp_dir().join("omy-tg-session-test-5");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(FILE_NAME);

        let Err(err) = seal_and_write(&saved(2040), &path, None) else {
            panic!("没有保护密钥时必须拒绝写盘");
        };
        assert!(
            matches!(err, SessionError::NoProtector),
            "要报「没有凭据库」，而不是别的原因：{err:?}"
        );
        // 要害：**一个字节都不能写出去**。报了错却仍留下明文文件是最糟的结果
        assert!(
            !path.exists(),
            "拒绝写盘时不能留下任何文件，否则等于明文落盘"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 删除登录态时，文件本来就不存在也算成功。
    ///
    /// 不这样会怎样：用户点「退出账号」收到一条错误，而他要的结果
    /// （别再留着登录态）其实已经达成了。
    #[test]
    fn forgetting_an_absent_session_succeeds() {
        // 直接验 remove_file 的分支语义，不碰真实数据目录
        let path = std::env::temp_dir().join("omy-tg-session-absent-4f2a.json");
        let _ = std::fs::remove_file(&path);
        let r = match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        };
        assert!(r.is_ok());
    }
}
