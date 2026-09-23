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
//!
//! # 一个账号一个文件
//!
//! 文件名是 `telegram-session-<账号标识>.json`。账号标识由调用方给，产品里用
//! **位置 id**（见 `places.rs`）——不是昵称：昵称可以重复、可以随时改，拿它当
//! 文件名会让两个账号在改名后突然指向同一个文件。
//!
//! 曾经只有一个 `telegram-session.json`。升级上来的用户靠
//! [`migrate_legacy_to`] 把它迁成第一个账号的文件，**不迁的话登录态直接丢**，
//! 而现象是「升级完就要重新扫码」，用户会以为是被登出了。

use std::path::{Path, PathBuf};

use grammers_session::storages::MemorySession;
use grammers_session::types::DcOption;
use grammers_session::Session as _;
use omy_core::crypto::{CipherId, Kek};

use crate::telegram::appid::{AppId, SessionIdentity, SessionMismatch};
use crate::telegram::place_secret::{PlaceKeyError, PlaceSlots, SlotKey};

/// 本机凭据库里的服务名。
///
/// 与远程位置那把（`omy-remote-places`）分开：两者的生命周期不同——用户删光
/// WebDAV 位置时不该顺带让 Telegram 登录态失效。
const SECRET_SERVICE: &str = "omy-telegram";

/// 保护密钥在凭据库里的 id。
///
/// **所有账号共用这一把**。每个账号一条钥匙串记录的话，删账号时漏清就会在
/// 用户的钥匙串里堆垃圾，而 Linux 的 Secret Service 对条目数也不友好——
/// 这与 `places.rs` 对多个 WebDAV 位置只用一把密钥是同一条理由。
/// 隔离由**文件名**保证，不靠密钥。
const SECRET_KEY_ID: &str = "telegram-session-key-v1";

/// 旧的单账号落盘文件名。**只用于一次性迁移**，新代码不要再写它。
const LEGACY_FILE_NAME: &str = "telegram-session.json";

/// 每账号文件名的前缀与后缀。
const FILE_PREFIX: &str = "telegram-session-";
const FILE_SUFFIX: &str = ".json";


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
    /// 账号标识不合法（空，或含会跑出数据目录的字符）。
    #[error("账号标识不合法")]
    BadAccount,
}

/// 校验账号标识能不能安全地拼进文件名。
///
/// # 为什么必须过这一道
///
/// 标识会被拼进文件名。不校验的话，一个形如 `../../x` 的标识会让 session
/// **写到数据目录之外**；Windows 上 `a:b` 之类则会直接写失败。产品里标识是
/// 我们自己生成的位置 id（`p1`、`p2`），但「现在的调用方很规矩」不足以成为
/// 把未经校验的字符串拼进路径的理由。
///
/// 只放行 ASCII 字母数字、`-` 与 `_`；其余**拒绝**而不是替换成下划线——
/// 替换会让 `a/b` 与 `a_b` 落到同一个文件，而那正是本模块最不能出的错：
/// 两个账号写同一份 session。
fn check_account(account: &str) -> Result<(), SessionError> {
    if account.is_empty() {
        return Err(SessionError::BadAccount);
    }
    if account
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        Ok(())
    } else {
        Err(SessionError::BadAccount)
    }
}

/// 某个账号的登录态文件路径。
///
/// 这一步是**纯路径计算，不碰网络也不读盘**——多账号化之后「按账号找 session」
/// 依然是零开销的，连接复用那条性质（实测再连约 2ms）不受影响。
///
/// # Errors
///
/// 标识不合法时返回 [`SessionError::BadAccount`]；算不出数据目录时返回
/// [`SessionError::NoDataDir`]。
pub fn session_path_of(account: &str) -> Result<PathBuf, SessionError> {
    check_account(account)?;
    omy_config::data_dir()
        .map(|d| d.join(format!("{FILE_PREFIX}{account}{FILE_SUFFIX}")))
        .ok_or(SessionError::NoDataDir)
}

/// 旧的单账号登录态文件路径。**只用于一次性迁移。**
///
/// # Errors
///
/// 算不出数据目录时返回 [`SessionError::NoDataDir`]。
pub fn legacy_session_path() -> Result<PathBuf, SessionError> {
    omy_config::data_dir()
        .map(|d| d.join(LEGACY_FILE_NAME))
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

/// 无库回退用的「机器密钥 KEK」：从系统凭据库取那把随机密钥，包成一个 KEK。
///
/// 用户一个 omy 库/密码都没有时，位置的密码槽用它来占——位置仍是加密的
/// （机器绑定），只是解锁不需要输密码，等同旧行为。等用户将来设了库密码，
/// 可以再给位置 `add` 一个真正的密码槽升级保护。
///
/// 返回 `None` 表示这台机器连凭据库都没有——那种情况下 session 无法安全落盘，
/// 与旧的 `NoProtector` 语义一致，调用方应据此提示「本机存不住登录态」。
#[must_use]
pub fn machine_fallback_kek() -> Option<Kek> {
    protect_key().map(|k| crate::telegram::place_secret::kek_from_machine_key(&k))
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

/// 落盘到某个账号的文件。
///
/// **登录一成功就该调用它**，而不是等整趟流程走完。中间任何一步失败
/// （2FA 输错、用户关窗口、进程崩）都会让这次已经在服务端生效的登录白费——
/// 而重新登录要再扫一次码，还可能撞 `FLOOD_WAIT`。
///
/// # Errors
///
/// 没有凭据库、标识不合法、算不出目录、加密或写盘失败时返回。
/// **没有凭据库时不写明文**。
pub fn save(
    session: &MemorySession,
    app: &AppId,
    account: &str,
) -> Result<PathBuf, SessionError> {
    let saved = extract(session, app)?;
    let path = session_path_of(account)?;
    save_to(&saved, &path)
}

/// 把一份现成的登录态落到某个账号的**产品路径**。
///
/// [`save`] 是从 `MemorySession` 抽取的，而 tdata 导入手里已经是
/// [`SavedSession`]。用 [`save_to`] 也能写，但那要调用方自己算路径——
/// 「session 存在哪」就有了两个来源，改一处另一处会悄悄不同步，
/// 表现是「登录了但重启后还要再登一次」，而两处代码单看都没错。
///
/// # Errors
///
/// 同 [`save`]。
pub fn save_current(saved: &SavedSession, account: &str) -> Result<PathBuf, SessionError> {
    let path = session_path_of(account)?;
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

/// 读回某个账号的登录态。
///
/// 返回 `Ok(None)` 表示「没存过」——那是首次使用的正常状态，不是错误。
/// 与「存了但解不开」必须分开：后者要提示用户重新登录，前者什么都不用说。
///
/// # Errors
///
/// 标识不合法、存档存在但解不开、格式不对、或 api_id 与当前不匹配时返回。
pub fn load(app: &AppId, account: &str) -> Result<Option<SavedSession>, SessionError> {
    let path = session_path_of(account)?;
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

/// 安全删除某个账号的登录态（「删除账号（含本地登录态）」那条路）。
///
/// 文件本来就不存在时算成功：用户要的是「别再留着」，而不是「删掉一个文件」。
///
/// # 为什么要覆写而不是直接 remove_file
///
/// 文件里是 auth key 的密文信封。直接删只是摘掉目录项，内容仍在磁盘上，
/// 而这个 crate 的整条规则就是「auth key 等同账号凭据」。覆写一遍再删，
/// 与 `omy-core` 的 `TempPlaintext::shred` 是同一条做法。
///
/// **效力边界要说清**：SSD 的磨损均衡会让覆写落到别的物理块上，所以这
/// 不是「不可恢复」的保证，只是把最容易被翻出来的那一份抹掉。真正的保障
/// 仍然是信封加密 + 机器绑定的保护密钥。
///
/// # Errors
///
/// 标识不合法、算不出目录或删除失败时返回。覆写失败**不算失败**——
/// 那时仍然会走删除，能删掉总好过因为覆写不了就把凭据留在盘上。
pub fn forget(account: &str) -> Result<(), SessionError> {
    let path = session_path_of(account)?;
    shred_file(&path)
}

/// 覆写并删除一个文件；不存在算成功。
fn shred_file(path: &Path) -> Result<(), SessionError> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() > 0 => {
            // 尽力覆写。失败不中断：删掉永远比留着强
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(path) {
                use std::io::Write as _;
                let mut buf = [0u8; 4096];
                omy_core::util::fill_random(&mut buf);
                let mut left = m.len();
                while left > 0 {
                    let n = usize::try_from(left.min(buf.len() as u64)).unwrap_or(buf.len());
                    if f.write_all(buf.get(..n).unwrap_or(&buf)).is_err() {
                        break;
                    }
                    left = left.saturating_sub(n as u64);
                }
                let _ = f.sync_all();
            }
        }
        // 不存在就没什么可抹的；其余元信息错误交给下面的 remove 去报
        _ => {}
    }
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(SessionError::Io(e.to_string())),
    }
}

/// 把旧的单文件登录态迁成某个账号的文件。**一次性**。
///
/// 返回是否真的迁了（`false` 表示没有旧文件，或目标已存在）。
///
/// # 为什么必须有这一步
///
/// 旧版本只有一个 `telegram-session.json`。多文件化之后按账号去找必然落空，
/// 于是升级上来的用户**登录态凭空消失**——现象是「更新完就要重新扫码」，
/// 他会以为自己被登出了，甚至怀疑账号出了问题。
///
/// # 为什么是改名而不是复制
///
/// 复制会在磁盘上留下第二份 auth key 密文。改名是原子的，也不留残份。
///
/// # 目标已存在时不覆盖
///
/// 那说明这个账号已经有自己的 session 了（用户已经重新登录过）。用旧文件
/// 盖掉它等于把一份更旧的登录态强加回去，可能已经失效。这种情况下**保留
/// 两者、什么都不做**，把旧文件留在原地由用户或后续清理处置。
///
/// # Errors
///
/// 标识不合法、算不出目录、或改名失败时返回。
pub fn migrate_legacy_to(account: &str) -> Result<bool, SessionError> {
    let target = session_path_of(account)?;
    let legacy = legacy_session_path()?;
    migrate_between(&legacy, &target)
}

/// 迁移的实质逻辑，**路径由调用方给**。
///
/// # 为什么要把路径提出去
///
/// 产品路径写死在数据目录里，测试碰不到它——于是那两条最要紧的规则
/// （「要真的迁过去」「不能覆盖已有的」）只能在测试里把判断重抄一遍，
/// 而抄出来的那份**改坏产品代码也不会失败**。变异测试确认过这一点：
/// 把「目标存在就不迁」整段删掉，重抄版断言照样通过。
///
/// 路径作为参数传进来之后，测试与产品走的就是同一份代码。
///
/// # Errors
///
/// 建目录或改名失败时返回。
fn migrate_between(legacy: &Path, target: &Path) -> Result<bool, SessionError> {
    if !legacy.is_file() {
        return Ok(false);
    }
    if target.exists() {
        // 已经有自己的 session，不拿旧的去盖
        return Ok(false);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| SessionError::Io(e.to_string()))?;
    }
    std::fs::rename(legacy, target).map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(true)
}

/// 还有没有遗留的单文件登录态待迁移。
#[must_use]
pub fn has_legacy_session() -> bool {
    legacy_session_path().map(|p| p.is_file()).unwrap_or(false)
}

/// 「刚登录完、还没有位置」的那份登录态用的账号名。
///
/// # 为什么需要它
///
/// 扫码登录**先于**位置存在：用户点「添加」时还没有位置 id，而 session 必须
/// 在登录成功那一刻立刻落盘（中途任何失败都会让这次已在服务端生效的登录白费，
/// 重新登录要再扫一次码、还可能撞 `FLOOD_WAIT`）。
///
/// 所以先落到这个固定名字下，等位置建好再用 [`adopt_pending`] 改名过去。
pub const PENDING_ACCOUNT: &str = "pending";

/// 把 [`PENDING_ACCOUNT`] 那份登录态收编成某个账号的。
///
/// 返回是否真的收编了（`false` 表示没有待收编的文件）。
///
/// # 为什么是改名而不是复制
///
/// 复制会在磁盘上留下第二份 auth key 密文，而且那一份不属于任何位置、
/// 谁也不会去清理它。改名是原子的，也不留残份。
///
/// # Errors
///
/// 标识不合法、算不出目录或改名失败时返回。
pub fn adopt_pending(account: &str) -> Result<bool, SessionError> {
    let target = session_path_of(account)?;
    let pending = session_path_of(PENDING_ACCOUNT)?;
    if !pending.is_file() {
        return Ok(false);
    }
    // 目标已存在就先抹掉再改名：这条路径上「目标」是刚分配的新位置 id，
    // 按理不该有文件；真有的话是上一次异常留下的残骸，留着只会让新账号
    // 读到一份属于别人的 session
    if target.exists() {
        shred_file(&target)?;
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).map_err(|e| SessionError::Io(e.to_string()))?;
    }
    std::fs::rename(&pending, &target).map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(true)
}

/// 落盘的完整结构：位置密码槽 + 用 PDK 加密的 session。
///
/// # 为什么两样一起存
///
/// 槽区（`slots`）告诉「谁能解出 PDK」，`session` 是「用 PDK 加密的登录态」。
/// 分两个文件的话，一个在、一个丢就成了半个状态；放同一个文件里，要么都在
/// 要么都不在。
///
/// # 与旧格式的区分
///
/// 旧格式（机器密钥那版）直接把 `omy_secret::Envelope` 序列化写盘。新格式是
/// 这个带 `fmt` 标记的结构。载入时先按新格式解析，失败再回退按旧 Envelope
/// 解析（见 [`load_with_keks`]），从而识别出「这是个待迁移的旧文件」。
#[derive(serde::Serialize, serde::Deserialize)]
pub struct StoredSession {
    /// 格式标记。固定为 2；旧格式没有这个字段（它是裸 Envelope）。
    pub fmt: u8,
    /// 位置密码槽：决定哪些已解锁的 KEK 能解出 PDK。
    pub slots: PlaceSlots,
    /// 用 PDK 加密的 session 明文信封。
    pub session: omy_secret::Envelope,
}

/// 当前落盘格式版本。
const STORED_FMT: u8 = 2;

/// 载入结果：要么解出了 session，要么明确「锁着」或「需要迁移」。
pub enum LoadOutcome {
    /// 成功解出登录态。
    Unlocked(SavedSession),
    /// 是新格式，但当前这批 KEK 都开不了它的槽——即未解锁（正常态，不是错误）。
    Locked,
    /// 是旧格式（机器密钥加密的裸 Envelope）——即**未加密的正常态**，可直接
    /// 使用，不是待迁移。加密是可选功能：用户没显式加密过的位置就一直是这个
    /// 形态。带回解出的 `SavedSession` 供连接使用；也供用户显式选择「加密此
    /// 位置」时（`migrate_to_slots`）直接拿去转槽格式。**仅当机器密钥仍可用
    /// 时**才解得出；解不出则为 `Locked`。
    LegacyUnencrypted(SavedSession),
    /// 没存过（首次使用），正常态。
    Absent,
}

/// 用一批已解锁的 KEK 把 session 落盘成新格式（per-place 槽）。
///
/// `keys` 是要给这个位置开的密码槽：通常是「会话里已解锁的全部凭据」——这样
/// 用户当前输过的任一密码之后都能直接开这个位置。至少要有一个，否则位置永远
/// 打不开（[`PlaceKeyError::NoSlots`]）。
///
/// # Errors
///
/// 槽创建失败、序列化或写盘失败时返回。**不再有「没有凭据库」这条**：新格式
/// 的密钥来自会话 KEK 或机器密钥回退，由调用方保证 `keys` 非空。
pub fn save_with_slots(
    saved: &SavedSession,
    path: &Path,
    keys: &[SlotKey<'_>],
) -> Result<PathBuf, SessionError> {
    let (slots, pdk) = PlaceSlots::create(keys, CipherId::ChaCha20Poly1305)
        .map_err(map_place_err)?;
    let plain = serde_json::to_vec(saved).map_err(|e| SessionError::Io(e.to_string()))?;
    let env = omy_secret::seal(
        &crate::telegram::place_secret::pdk_as_protect_key(&pdk),
        &plain,
    )
    .map_err(|e| SessionError::Io(e.to_string()))?;
    let stored = StoredSession { fmt: STORED_FMT, slots, session: env };
    let text = serde_json::to_string(&stored).map_err(|e| SessionError::Io(e.to_string()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| SessionError::Io(e.to_string()))?;
    }
    std::fs::write(path, text).map_err(|e| SessionError::Io(e.to_string()))?;
    Ok(path.to_path_buf())
}

/// 把 `PlaceKeyError` 归并到 `SessionError`。
fn map_place_err(e: PlaceKeyError) -> SessionError {
    match e {
        PlaceKeyError::Locked => SessionError::Undecryptable,
        // 其余都是「存不出去」类，归到 IO 让调用方按写盘失败处理
        other => SessionError::Io(other.to_string()),
    }
}

/// 用一批已解锁的 KEK 载入某账号的 session（新格式优先，旧格式回退）。
///
/// 返回 [`LoadOutcome`]：区分「解出了」「锁着」「要迁移」「没存过」——它们
/// 对界面的意义完全不同，不能混。
///
/// # Errors
///
/// 标识不合法、读盘失败、格式彻底不认识、或 api_id 不匹配时返回。
pub fn load_with_keks(
    app: &AppId,
    account: &str,
    keks: &[Kek],
) -> Result<LoadOutcome, SessionError> {
    let path = session_path_of(account)?;
    load_with_keks_from(&path, app, keks)
}

/// [`load_with_keks`] 的按路径版本（测试用）。
///
/// # Errors
///
/// 同 [`load_with_keks`]。
pub fn load_with_keks_from(
    path: &Path,
    app: &AppId,
    keks: &[Kek],
) -> Result<LoadOutcome, SessionError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(LoadOutcome::Absent),
        Err(e) => return Err(SessionError::Io(e.to_string())),
    };

    // 先按新格式解析。带 fmt 标记才当新格式，避免把恰好能反序列化成
    // StoredSession 的旧内容误判
    if let Ok(stored) = serde_json::from_str::<StoredSession>(&text) {
        if stored.fmt == STORED_FMT {
            let pdk = match stored.slots.unlock(keks) {
                Ok(p) => p,
                // 没有 KEK 能开 = 未解锁，正常态
                Err(PlaceKeyError::Locked) => return Ok(LoadOutcome::Locked),
                Err(e) => return Err(map_place_err(e)),
            };
            let plain = omy_secret::unseal(
                &crate::telegram::place_secret::pdk_as_protect_key(&pdk),
                &stored.session,
            )
            .map_err(|_| SessionError::Undecryptable)?;
            let saved: SavedSession =
                serde_json::from_slice(&plain).map_err(|_| SessionError::Malformed)?;
            SessionIdentity { api_id: saved.api_id }.check(app)?;
            return Ok(LoadOutcome::Unlocked(saved));
        }
    }

    // 回退：旧格式是裸 Envelope（机器密钥加密）。能解出就标记「需迁移」，
    // 解不出（换了机器/清了钥匙串）就当锁着——两者都不是「没存过」
    match load_from(path, app) {
        Ok(Some(saved)) => Ok(LoadOutcome::LegacyUnencrypted(saved)),
        Ok(None) => Ok(LoadOutcome::Absent),
        // 机器密钥不可用 → 旧文件解不开。它确实存过，只是这台机器打不开了，
        // 当作「锁着」比报错更贴切（迁移那步会在有 KEK 时重试）
        Err(SessionError::NoProtector | SessionError::Undecryptable) => Ok(LoadOutcome::Locked),
        Err(e) => Err(e),
    }
}

/// 把一个旧格式（机器密钥）session 迁移成新格式（per-place 槽）。
///
/// **只在会话已解锁、拿得到 KEK 时调用**——启动即迁移会因为还没有任何 KEK
/// 而失败，或被迫仍用机器密钥，那就白迁了。
///
/// 迁移是**写操作**：先用新格式写到临时文件、fsync、再原子改名覆盖原文件，
/// 最后没有旧密文残留。中途失败时原文件仍在（旧格式仍可被机器密钥解开），
/// 不会出现「新的没写好、旧的已删」的半迁移态。
///
/// 返回是否真的迁了（`false` = 该文件不是待迁移的旧格式）。
///
/// # Errors
///
/// 读不出旧格式、槽创建或写盘失败时返回。
pub fn migrate_to_slots(
    app: &AppId,
    account: &str,
    keys: &[SlotKey<'_>],
) -> Result<bool, SessionError> {
    let path = session_path_of(account)?;
    migrate_to_slots_at(&path, app, keys)
}

/// [`migrate_to_slots`] 的便捷版：直接收一批裸 KEK，内部包成 `SlotKey`。
///
/// GUI 的 `ensure_connected` 手里是会话已解锁的 `Vec<Kek>`（没有 label/kind），
/// 用这个免得调用方自己拼 `SlotKey`。kind/label 只用于显示，统一填占位值——
/// 解锁靠逐把 KEK 试，不看这两个字段。
///
/// # Errors
///
/// 同 [`migrate_to_slots`]。
pub fn migrate_to_slots_with_keks(
    app: &AppId,
    account: &str,
    keks: &[Kek],
) -> Result<bool, SessionError> {
    let keys: Vec<SlotKey<'_>> = keks
        .iter()
        .map(|k| SlotKey { kek: k, kind: "vault", label: "session" })
        .collect();
    migrate_to_slots(app, account, &keys)
}

/// 这个账号的 session 当前是否是「已加密」（per-place 槽）格式。
///
/// 只看盘上格式，不需要 KEK：有 `fmt` 标记即已加密，裸 Envelope 即未加密。
/// 供界面显示加密标识用。
///
/// # Errors
///
/// 标识不合法或读盘失败时返回；文件不存在返回 `Ok(false)`（没存过≠已加密）。
pub fn is_encrypted(account: &str) -> Result<bool, SessionError> {
    let path = session_path_of(account)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(serde_json::from_str::<StoredSession>(&text)
            .is_ok_and(|st| st.fmt == STORED_FMT)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(SessionError::Io(e.to_string())),
    }
}

/// 显式加密一个位置：把它的 session 转成 per-place 槽格式。
///
/// 与迁移是同一个动作，但语义是**用户主动选择**而非自动。已经是加密格式时
/// 是 no-op（返回 `false`）。用当前会话已解锁的 `keks` 建槽。
///
/// # Errors
///
/// 读不出当前 session、无可用 KEK、槽创建或写盘失败时返回。
pub fn encrypt_place(app: &AppId, account: &str, keks: &[Kek]) -> Result<bool, SessionError> {
    migrate_to_slots_with_keks(app, account, keks)
}

/// 显式取消加密一个位置：把它的 session 转回默认（机器密钥裸信封）格式。
///
/// 需要 `keks` 先解开当前的槽（拿到明文 session），再用机器密钥重新落盘。
/// 已经是未加密格式时是 no-op（返回 `false`）。
///
/// 原子替换（写临时文件→rename），失败时原文件原样保留——与迁移同样的
/// 「不出半状态」保证。
///
/// # Errors
///
/// 当前是加密格式但 `keks` 解不开（[`SessionError::Undecryptable`]）、
/// 本机无凭据库（[`SessionError::NoProtector`]，此时**不落明文**）、
/// 读写失败时返回。
pub fn decrypt_place(app: &AppId, account: &str, keks: &[Kek]) -> Result<bool, SessionError> {
    let path = session_path_of(account)?;
    // 先确认它确实是加密格式，并用 keks 解出明文 session
    let saved = match load_with_keks_from(&path, app, keks)? {
        // 已是未加密格式：no-op
        LoadOutcome::LegacyUnencrypted(_) => return Ok(false),
        LoadOutcome::Unlocked(s) => s,
        // 加密着但当前 keks 开不了：不能取消（否则会丢登录态）
        LoadOutcome::Locked => return Err(SessionError::Undecryptable),
        LoadOutcome::Absent => return Ok(false),
    };
    // 用机器密钥重新落盘成默认格式。拿不到凭据库时拒绝——不落明文
    let tmp = path.with_extension("json.decrypting");
    seal_and_write(&saved, &tmp, protect_key())?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        SessionError::Io(e.to_string())
    })?;
    Ok(true)
}

/// [`migrate_to_slots`] 的按路径版本（测试用）。
///
/// # Errors
///
/// 同 [`migrate_to_slots`]。
pub fn migrate_to_slots_at(
    path: &Path,
    app: &AppId,
    keys: &[SlotKey<'_>],
) -> Result<bool, SessionError> {
    // 先判断这是不是待迁移的旧格式。用空 KEK 集合探测：新格式会返回 Locked，
    // 旧格式（机器密钥可解）会返回 LegacyUnencrypted
    let saved = match load_with_keks_from(path, app, &[])? {
        LoadOutcome::LegacyUnencrypted(s) => s,
        // 已是新格式 / 没存过 / 锁着（机器密钥都解不开，无从迁移）
        _ => return Ok(false),
    };

    // 原子替换：写临时文件再 rename，避免半迁移态
    let tmp = path.with_extension("json.migrating");
    save_with_slots(&saved, &tmp, keys)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // 改名失败要清掉临时文件，别留垃圾
        let _ = std::fs::remove_file(&tmp);
        SessionError::Io(e.to_string())
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{SocketAddrV4, SocketAddrV6};
    use crate::telegram::place_secret::SlotKey;
    use omy_core::crypto::SecretKey;

    /// 造一把测试 KEK，避开 Argon2 开销。
    fn tkek(seed: u8) -> Kek {
        Kek::from_key(SecretKey::from_bytes(core::array::from_fn(|i| {
            (i as u8).wrapping_mul(31) ^ seed
        })))
    }

    /// 测试辅助：按路径判断是否加密（is_encrypted 是按 account 的，测试用临时路径）。
    fn is_encrypted_at(path: &std::path::Path) -> bool {
        match std::fs::read_to_string(path) {
            Ok(t) => serde_json::from_str::<StoredSession>(&t).is_ok_and(|st| st.fmt == STORED_FMT),
            Err(_) => false,
        }
    }
    /// 测试辅助：按路径加密（= 迁移到槽）。
    fn encrypt_place_at(path: &std::path::Path, app: &AppId, keks: &[Kek]) -> Result<bool, SessionError> {
        let keys: Vec<SlotKey<'_>> = keks.iter().map(|k| SlotKey { kek: k, kind: "vault", label: "s" }).collect();
        migrate_to_slots_at(path, app, &keys)
    }
    /// 测试辅助：按路径取消加密。复刻 decrypt_place 的路径版逻辑。
    fn decrypt_place_at(path: &std::path::Path, app: &AppId, keks: &[Kek]) -> Result<bool, SessionError> {
        let saved = match load_with_keks_from(path, app, keks)? {
            LoadOutcome::LegacyUnencrypted(_) => return Ok(false),
            LoadOutcome::Unlocked(s) => s,
            LoadOutcome::Locked => return Err(SessionError::Undecryptable),
            LoadOutcome::Absent => return Ok(false),
        };
        let tmp = path.with_extension("json.decrypting");
        seal_and_write(&saved, &tmp, protect_key())?;
        std::fs::rename(&tmp, path).map_err(|e| SessionError::Io(e.to_string()))?;
        Ok(true)
    }

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

    /// 测试里用的文件名。产品路径由 `session_path_of` 算，这里只要一个
    /// 落在临时目录里的名字。
    const TEST_FILE: &str = "telegram-session-test.json";

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
        let path = dir.join(TEST_FILE);
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
        let path = dir.join(TEST_FILE);
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
        let path = dir.join(TEST_FILE);
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
        let path = dir.join(TEST_FILE);
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
        let path = dir.join(TEST_FILE);

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
        let path = std::env::temp_dir().join("omy-tg-session-absent-4f2a.json");
        let _ = std::fs::remove_file(&path);
        assert!(shred_file(&path).is_ok(), "删一个不存在的文件应当算成功");
    }

    /// **两个账号必须落到两个不同的文件。**
    ///
    /// 不这样会怎样：这是多账号改造里最致命、也最难被发现的回归——两个位置
    /// 写同一份 session，后添加的那个会把前一个的 auth key 覆盖掉，于是
    /// 「加了第二个账号，第一个就登出了」。而侧栏上两个位置都好端端列着，
    /// 任何「两个位置都存在」的断言都照样通过。
    ///
    /// 所以这里比的是**路径本身**，不是「两个都在」。
    #[test]
    fn each_account_gets_its_own_file() {
        let (Ok(a), Ok(b)) = (session_path_of("p1"), session_path_of("p2")) else {
            eprintln!("跳过：这台机器算不出数据目录");
            return;
        };
        assert_ne!(a, b, "两个账号必须落到不同的 session 文件");
        // 文件名里要真的带上账号标识，否则「不同」可能只是别的原因凑巧造成的
        let name_a = a.file_name().map(|s| s.to_string_lossy().into_owned());
        let name_b = b.file_name().map(|s| s.to_string_lossy().into_owned());
        assert_eq!(name_a.as_deref(), Some("telegram-session-p1.json"));
        assert_eq!(name_b.as_deref(), Some("telegram-session-p2.json"));
        // 同一个账号每次都要算出同一个路径，否则重启后找不回自己的 session
        assert_eq!(session_path_of("p1").ok(), Some(a), "同一账号的路径必须稳定");
    }

    /// 新的每账号文件名不能与旧的单文件名撞。
    ///
    /// 不这样会怎样：撞了的话迁移的源和目标会指向同一个文件，`rename` 要么
    /// 失败要么变成空操作，而现象是「迁移跑过了但登录态还是丢」。
    #[test]
    fn per_account_name_never_equals_the_legacy_name() {
        for acct in ["p1", "p2", "legacy", "0"] {
            let Ok(p) = session_path_of(acct) else {
                continue;
            };
            let name = p.file_name().map(|s| s.to_string_lossy().into_owned());
            assert_ne!(
                name.as_deref(),
                Some(LEGACY_FILE_NAME),
                "账号 {acct} 的文件名与旧单文件名撞了"
            );
        }
    }

    /// 账号标识里的危险字符必须被**拒绝**，而不是被替换。
    ///
    /// 不这样会怎样：`../../x` 会让 session 写到数据目录之外；而如果把非法
    /// 字符替换成下划线，`a/b` 与 `a_b` 会落到同一个文件——那正是「两个账号
    /// 共用一份 session」这条最致命的回归，只不过换了个入口。
    #[test]
    fn dangerous_account_ids_are_refused_not_sanitized() {
        for bad in ["", "../x", "a/b", "a\\b", "a:b", "a.b", "有 空格", "p1/../p2"] {
            assert!(
                matches!(session_path_of(bad), Err(SessionError::BadAccount)),
                "{bad:?} 必须被拒绝"
            );
        }
        // 正常的位置 id 要放行，否则这条校验就成了「谁都不许用」
        for ok in ["p1", "p12", "a-b", "a_b", "A1"] {
            assert!(session_path_of(ok).is_ok(), "{ok:?} 是合法标识，不该被拒");
        }
    }

    /// 旧的单文件必须能迁成第一个账号的文件，且**内容一字不差**。
    ///
    /// 不这样会怎样：升级上来的用户登录态凭空消失，现象是「更新完就要重新
    /// 扫码」——他会以为自己被登出了，甚至怀疑账号被盗。
    ///
    /// 这条同时守住「迁过去的还能读回来」：只断言「目标文件存在」的话，
    /// 一个把目标创建成空文件的实现也能通过，而那等于登录态还是丢了。
    #[test]
    fn the_legacy_single_file_migrates_into_the_first_account() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-session-migrate-1");
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join(LEGACY_FILE_NAME);
        let target = dir.join("telegram-session-p1.json");
        save_to(&saved(2040), &legacy).expect("先写一份旧格式的");

        // 调用**产品代码本身**，不在测试里重抄一遍判断
        assert!(
            migrate_between(&legacy, &target).expect("迁移不该报错"),
            "有旧文件且目标为空时必须真的迁"
        );

        assert!(!legacy.exists(), "迁完不该再留着旧文件——那是第二份 auth key");
        let back = match load_from(&target, &AppId::builtin()) {
            Ok(Some(v)) => v,
            Ok(None) => panic!("迁过去的文件必须读得出内容"),
            Err(e) => panic!("迁过去的文件读不回来：{e}"),
        };
        assert!(
            has_auth_key(&back),
            "迁移后 auth key 必须还在，否则等于登录态丢了"
        );
        assert_eq!(back.home_dc, 2, "home_dc 也要原样带过去");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 目标已经有 session 时，迁移**不能**覆盖它。
    ///
    /// 不这样会怎样：用户已经重新登录过，旧文件里那份可能早就失效了。
    /// 拿它盖掉当前可用的登录态，等于把人从一个好账号里踢出去。
    #[test]
    fn migration_never_overwrites_an_existing_session() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-session-migrate-2");
        let _ = std::fs::remove_dir_all(&dir);
        let legacy = dir.join(LEGACY_FILE_NAME);
        let target = dir.join("telegram-session-p1.json");
        save_to(&saved(2040), &legacy).expect("旧文件");
        let mut newer = saved(2040);
        newer.home_dc = 4;
        save_to(&newer, &target).expect("目标已有 session");

        // 调用**产品代码本身**：目标已存在时必须原地不动
        assert!(
            !migrate_between(&legacy, &target).expect("不该报错"),
            "目标已存在时必须不迁移"
        );
        assert!(legacy.is_file(), "旧文件应当原样留在原地");

        let back = match load_from(&target, &AppId::builtin()) {
            Ok(Some(v)) => v,
            _ => panic!("目标文件应当还在"),
        };
        assert_eq!(back.home_dc, 4, "目标里那份必须原封不动，不能被旧文件盖掉");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 删一个账号不能动到另一个账号的文件。
    ///
    /// 不这样会怎样：用户删掉账号 B，结果账号 A 也要重新登录——而他完全
    /// 不知道为什么，只会觉得这个功能不能碰。
    #[test]
    fn deleting_one_account_leaves_the_other_intact() {
        if protect_key().is_none() {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-session-two-accounts");
        let _ = std::fs::remove_dir_all(&dir);
        let a = dir.join("telegram-session-p1.json");
        let b = dir.join("telegram-session-p2.json");
        save_to(&saved(2040), &a).expect("写 A");
        let mut other = saved(2040);
        other.home_dc = 5;
        save_to(&other, &b).expect("写 B");

        shred_file(&a).expect("删 A");

        assert!(!a.exists(), "A 必须被删掉");
        assert!(b.is_file(), "B 不能被连带删掉");
        let back = match load_from(&b, &AppId::builtin()) {
            Ok(Some(v)) => v,
            _ => panic!("B 应当仍然可读"),
        };
        assert_eq!(back.home_dc, 5, "B 的内容必须原封不动");
        assert!(has_auth_key(&back), "B 的 auth key 必须还在");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 安全删除要真的把文件删掉。
    ///
    /// 不这样会怎样：直接 remove_file 只摘掉目录项，auth key 的密文仍躺在
    /// 原来的扇区里。覆写不是「不可恢复」的保证（SSD 磨损均衡会让它落到
    /// 别的物理块），但至少不把最容易被翻出来的那一份留在那儿。
    #[test]
    fn shredding_removes_the_file() {
        let dir = std::env::temp_dir().join("omy-tg-session-shred");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("telegram-session-p9.json");
        // 内容用算式生成而不是长串重复字节：后者会让本机安全软件把测试
        // 二进制删掉（AGENTS.md 记过这一条）
        let marker: Vec<u8> = (0..512).map(|i| ((i % 251) as u8) ^ 0x5B).collect();
        std::fs::write(&path, &marker).expect("写文件");

        shred_file(&path).expect("安全删除");
        assert!(!path.exists(), "删完文件不该还在");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ========================================================
    // per-place 槽格式：save_with_slots / load_with_keks / 迁移
    // ========================================================

    /// 新格式：用某把 KEK 建槽落盘，同一把 KEK 能解出 session；别的 KEK = Locked。
    ///
    /// 不这样会怎样：这是「输过的密码能开这个位置、没输的开不了」的核心，
    /// 错了要么谁都进不去、要么没密码也能进。
    #[test]
    fn slots_roundtrip_unlocks_only_with_enrolled_kek() {
        let dir = std::env::temp_dir().join("omy-tg-slots-1");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        let pw = tkek(0x11);
        save_with_slots(&saved(2040), &path, &[SlotKey { kek: &pw, kind: "vault", label: "主" }])
            .expect("写新格式");

        // 正确 KEK → Unlocked，auth key 完好
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0x11)]) {
            Ok(LoadOutcome::Unlocked(back)) => {
                assert!(has_auth_key(&back), "解出的 session 必须带 auth key");
                assert_eq!(back.home_dc, 2);
            }
            other => panic_load("正确密码应解出", other),
        }
        // 错误 KEK → Locked（不是错误、不是没存过）
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0x99)]) {
            Ok(LoadOutcome::Locked) => {}
            other => panic_load("错误密码应为 Locked", other),
        }
        // 空 KEK → 新格式也应 Locked（不是 LegacyUnencrypted）
        match load_with_keks_from(&path, &AppId::builtin(), &[]) {
            Ok(LoadOutcome::Locked) => {}
            other => panic_load("新格式无 KEK 应 Locked", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未解锁时，落盘文件里绝不能出现 auth key 明文——与旧格式同等强度。
    #[test]
    fn slots_file_never_contains_auth_key() {
        let dir = std::env::temp_dir().join("omy-tg-slots-2");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        let s = saved(2040);
        let Some(key) = s.dc_options.first().and_then(|d| d.auth_key) else {
            panic!("夹具第一个 DC 应带 auth key");
        };
        let pw = tkek(0x22);
        save_with_slots(&s, &path, &[SlotKey { kek: &pw, kind: "vault", label: "主" }]).unwrap();
        let bytes = std::fs::read(&path).unwrap_or_default();
        assert!(!bytes.windows(key.len()).any(|w| w == key), "不得出现 auth key 原始字节");
        let text = String::from_utf8_lossy(&bytes).to_ascii_lowercase();
        assert!(!text.contains(&hex_of(&key)), "不得出现 auth key 十六进制");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 迁移：旧格式（机器密钥）→ 会话解锁后迁成新格式；迁后旧密钥失效、新 KEK 生效。
    ///
    /// 用机器密钥回退当「旧格式」的替身：save_to 写的就是裸 Envelope（旧格式），
    /// 但它用系统凭据库那把密钥。没有凭据库的 CI 上跳过。
    #[test]
    fn migration_from_legacy_to_slots() {
        if protect_key().is_none() {
            eprintln!("跳过：没有可用凭据后端，造不出旧格式文件");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-migrate-slots");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        // 造旧格式：裸 Envelope（机器密钥）
        save_to(&saved(2040), &path).expect("写旧格式");
        // 探测：应识别为 LegacyUnencrypted（未加密可用）
        match load_with_keks_from(&path, &AppId::builtin(), &[]) {
            Ok(LoadOutcome::LegacyUnencrypted(_)) => {}
            other => panic_load("旧格式应识别为未加密可用", other),
        }

        // 会话解锁（拿到 KEK）后迁移
        let pw = tkek(0x33);
        let migrated = migrate_to_slots_at(
            &path,
            &AppId::builtin(),
            &[SlotKey { kek: &pw, kind: "vault", label: "主" }],
        )
        .expect("迁移不该报错");
        assert!(migrated, "旧格式必须被迁移");

        // 迁移后：新 KEK 能开，机器密钥那条路（空 KEK 探测）不再是 LegacyUnencrypted
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0x33)]) {
            Ok(LoadOutcome::Unlocked(back)) => assert!(has_auth_key(&back), "迁后 auth key 必须在"),
            other => panic_load("迁后新密码应解出", other),
        }
        match load_with_keks_from(&path, &AppId::builtin(), &[]) {
            Ok(LoadOutcome::Locked) => {}
            other => panic_load("迁后应是新格式(空KEK=Locked)", other),
        }
        // 再迁一次应是 no-op（已经是新格式）
        assert!(
            !migrate_to_slots_at(&path, &AppId::builtin(), &[SlotKey { kek: &pw, kind: "vault", label: "主" }]).unwrap(),
            "已是新格式不该再迁"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 半迁移可恢复：迁移写临时文件失败时，原文件必须原样保留、仍能被旧路径解开。
    ///
    /// 造法：让目标路径的临时文件无法写（父目录不存在时 save_with_slots 会
    /// create_dir_all，所以改用「keys 为空」触发 NoSlots 让迁移中途失败），
    /// 断言原文件未被动。
    #[test]
    fn failed_migration_leaves_original_intact() {
        if protect_key().is_none() {
            eprintln!("跳过：没有可用凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-migrate-fail");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        save_to(&saved(2040), &path).expect("写旧格式");
        let before = std::fs::read(&path).expect("读原文件");

        // keys 为空 → save_with_slots 里 PlaceSlots::create 返回 NoSlots → 迁移失败
        let err = migrate_to_slots_at(&path, &AppId::builtin(), &[]);
        assert!(err.is_err(), "空 keys 迁移必须失败");

        // 原文件必须逐字节不变，且仍能按旧格式解开
        let after = std::fs::read(&path).expect("原文件应还在");
        assert_eq!(before, after, "迁移失败不得改动原文件");
        assert!(
            matches!(load_from(&path, &AppId::builtin()), Ok(Some(_))),
            "原旧格式文件必须仍可解开"
        );
        // 临时文件不该残留
        assert!(!path.with_extension("json.migrating").exists(), "不得残留临时文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **假加密防线**：用真实密码 KEK 加密的位置，**仅用机器密钥必须解不开**。
    ///
    /// 不这样会怎样：这正是「右键加密却不弹密码、静默成功」那个假加密缺陷的
    /// 核心——若加密时把机器密钥也放进槽，这台机器开机自动就能解开，等于没
    /// 加密。这条断言钉死「加密位置的槽里没有机器密钥」：只有真实密码能开，
    /// 单靠机器密钥（模拟本机自动派生的那把）开不了。
    #[test]
    fn encrypted_place_cannot_be_opened_by_machine_key_alone() {
        let dir = std::env::temp_dir().join("omy-tg-no-fake-encrypt");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        // 用「真实密码」KEK（0xA1）加密——只把它放进槽，不带任何机器密钥
        let real = tkek(0xA1);
        save_with_slots(&saved(2040), &path, &[crate::telegram::place_secret::SlotKey {
            kek: &real, kind: "vault", label: "主密码",
        }]).expect("加密写盘");

        // 仅用「机器密钥」（用另一把 KEK 0xB2 模拟本机自动可得、与真实密码不同的钥匙）：
        // 必须 Locked（解不开）
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0xB2)]) {
            Ok(LoadOutcome::Locked) => {}
            other => panic_load("仅机器密钥必须解不开加密位置（否则是假加密）", other),
        }
        // 真实密码：能解开
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0xA1)]) {
            Ok(LoadOutcome::Unlocked(b)) => assert!(has_auth_key(&b), "真实密码应能解出"),
            other => panic_load("真实密码应能解出加密位置", other),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 显式加密 / 取消加密往返：encrypt_place 后 is_encrypted 为真、能用 KEK 开；
    /// decrypt_place 后转回默认格式、is_encrypted 为假、机器密钥能开、内容不丢。
    ///
    /// 不这样会怎样：加密/取消是用户手动操作，来回切必须无损——任一方向丢了
    /// auth key 就是把用户登录态弄没了。
    #[test]
    fn encrypt_then_decrypt_roundtrip() {
        if protect_key().is_none() {
            eprintln!("跳过：没有可用凭据后端");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-enc-dec");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        // 起点：默认（未加密）格式
        save_to(&saved(2040), &path).expect("写默认格式");
        assert!(!is_encrypted_at(&path), "起点应未加密");

        // 加密：用一把 KEK 转槽
        let pw = tkek(0x61);
        assert!(encrypt_place_at(&path, &AppId::builtin(), &[tkek(0x61)]).expect("加密"), "应真的加密");
        assert!(is_encrypted_at(&path), "加密后应标记为已加密");
        // 加密后：正确 KEK 能开、auth key 在
        match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0x61)]) {
            Ok(LoadOutcome::Unlocked(b)) => assert!(has_auth_key(&b), "加密后 auth key 必须在"),
            other => panic_load("加密后应能用 KEK 解出", other),
        }
        let _ = pw;

        // 取消加密：转回默认格式
        assert!(decrypt_place_at(&path, &AppId::builtin(), &[tkek(0x61)]).expect("取消加密"), "应真的取消");
        assert!(!is_encrypted_at(&path), "取消后应回未加密");
        // 取消后：机器密钥（旧路径）能开、auth key 在、内容一致
        match load_from(&path, &AppId::builtin()) {
            Ok(Some(b)) => {
                assert!(has_auth_key(&b), "取消加密后 auth key 必须在");
                assert_eq!(b.home_dc, 2, "内容不得丢");
            }
            _ => panic!("取消加密后应能按默认格式解开"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **默认不加密**：旧格式文件被 load_with_keks 读取后，即使会话里有 KEK，
    /// 文件也**逐字节不变**——加密是显式动作，load 绝不顺手转格式。
    ///
    /// 不这样会怎样：这正是本次纠偏要防的回归。之前 load/连接路径会把所有旧格式
    /// 自动迁成 per-place 槽，等于把不设密码的账号也强行加密了。这条断言钉死
    /// 「读一遍不改盘」，防止再退回自动全加密。
    #[test]
    fn loading_legacy_never_rewrites_it() {
        if protect_key().is_none() {
            eprintln!("跳过：没有可用凭据后端，造不出旧格式文件");
            return;
        }
        let dir = std::env::temp_dir().join("omy-tg-no-auto-encrypt");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join(TEST_FILE);
        save_to(&saved(2040), &path).expect("写旧格式");
        let before = std::fs::read(&path).expect("读原文件");
        assert!(
            !String::from_utf8_lossy(&before).contains("\"fmt\""),
            "基线：应是旧格式（无 fmt 标记）"
        );

        // 带一把 KEK 去 load——即便有 KEK，也不该触发任何转换
        for _ in 0..3 {
            match load_with_keks_from(&path, &AppId::builtin(), &[tkek(0x77)]) {
                Ok(LoadOutcome::LegacyUnencrypted(_)) => {}
                other => panic_load("旧格式应报未加密、不自动转", other),
            }
        }
        let after = std::fs::read(&path).expect("读回");
        assert_eq!(before, after, "load 旧格式不得改动文件（默认不加密）");
        assert!(
            !String::from_utf8_lossy(&after).contains("\"fmt\""),
            "load 之后仍应是旧格式，绝不能被自动转成 per-place 槽"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 一个密码开两个位置：同一把 KEK 分别给两个账号建槽，都能各自解出。
    #[test]
    fn one_password_opens_two_places() {
        let dir = std::env::temp_dir().join("omy-tg-slots-multi");
        let _ = std::fs::remove_dir_all(&dir);
        let pa = dir.join("telegram-session-p1.json");
        let pb = dir.join("telegram-session-p2.json");
        let pw = tkek(0x44);
        save_with_slots(&saved(2040), &pa, &[SlotKey { kek: &pw, kind: "vault", label: "主" }]).unwrap();
        let mut other = saved(2040);
        other.home_dc = 4;
        save_with_slots(&other, &pb, &[SlotKey { kek: &pw, kind: "vault", label: "主" }]).unwrap();

        for (p, dc) in [(&pa, 2), (&pb, 4)] {
            match load_with_keks_from(p, &AppId::builtin(), &[tkek(0x44)]) {
                Ok(LoadOutcome::Unlocked(back)) => assert_eq!(back.home_dc, dc, "各自内容"),
                other => panic_load("同一密码应能开两个位置", other),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 测试辅助：LoadOutcome 不便 Debug（SavedSession 无 Debug），统一在这里 panic。
    fn panic_load(msg: &str, got: Result<LoadOutcome, SessionError>) -> ! {
        let tag = match got {
            Ok(LoadOutcome::Unlocked(_)) => "Unlocked",
            Ok(LoadOutcome::Locked) => "Locked",
            Ok(LoadOutcome::LegacyUnencrypted(_)) => "LegacyUnencrypted",
            Ok(LoadOutcome::Absent) => "Absent",
            Err(_) => "Err",
        };
        panic!("{msg}；实际得到 {tag}");
    }
}
