//! 设备库：本机身份与已配对设备。
//!
//! # 为什么 GUI 需要一个「设备库会话」，而 CLI 不需要
//!
//! CLI 里每条命令都是一个独立进程，问一次密码、做一件事、退出。
//! 所以 `omy share pair` 会问两次密码（开库一次、存库一次），
//! 这在命令行里可以接受。
//!
//! GUI 是长驻进程，用户会连续操作：看看有谁在线 → 配对 → 改个名字
//! → 再配一台。若沿用 CLI 的做法，这四步要输七八次密码，
//! 而且每次都是同一个密码——用户会很快学会把它设成 `1`。
//!
//! **让安全措施难用，结果是用户绕过它**，所以这里把解开的 `Store`
//! 留在会话里。
//!
//! # 留在内存里安全吗
//!
//! 这份数据的敏感度与文件密钥是**同一量级**（都在 `SessionKeys` 里），
//! 而后者本来就常驻会话。设备库里最敏感的是本机静态私钥，
//! 泄露的后果是「攻击者可冒充本设备」——不是「泄露用户文件内容」。
//!
//! 两个约束让它不至于失控：
//!
//! - `lock()` 时**一并清空**，与文件密钥同生共死。用户按 Ctrl+L
//!   锁定应用，不该只锁一半。
//! - 密码本身用 `Zeroizing` 持有，`Drop` 时抹掉。留着它是为了保存时
//!   不用再问一遍——保存发生在配对成功之后，那时再弹密码框，
//!   用户会以为配对失败了。

use omy_net::store::{DeviceRecord, Store};
use std::path::PathBuf;
use std::sync::Mutex;
use zeroize::Zeroizing;

/// 设备库会话：解开后的 `Store` 与用于保存的密码。
pub struct DeviceSession {
    inner: Mutex<Option<Opened>>,
}

/// 已打开的设备库。
struct Opened {
    store: Store,
    path: PathBuf,
    /// 保存时要用。`Zeroizing` 保证 drop 时抹掉。
    password: Zeroizing<Vec<u8>>,
}

/// 设备库的当前状态，给前端渲染用。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceStatus {
    /// 设备库文件是否已存在。
    pub exists: bool,
    /// 本次会话是否已打开它。
    pub opened: bool,
    /// 本机设备名，未打开时为 `None`。
    pub device_name: Option<String>,
    /// 本机指纹，未打开时为 `None`。
    ///
    /// 配对时两端要**人工核对**这个值，所以必须能显示出来。
    pub fingerprint: Option<String>,
    /// 已配对设备数量。
    pub paired_count: usize,
}

/// 一台已配对设备，给前端渲染用。
///
/// 不直接把 [`DeviceRecord`] 丢给前端：它含完整公钥，
/// 而界面只需要指纹。公钥进 WebView 没有用途，只会多一处泄露面。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PairedInfo {
    /// 16 位十六进制指纹，前端用它指代设备。
    pub fingerprint: String,
    /// 对方设备名。**来自对方，不可信**，仅作展示。
    pub name: String,
    /// 配对时间（Unix 秒）。
    pub paired_at: u64,
    /// 到期时间（Unix 秒），0 表示永久。
    pub expires_at: u64,
    /// 是否已过期。
    pub expired: bool,
}

impl From<&DeviceRecord> for PairedInfo {
    fn from(d: &DeviceRecord) -> Self {
        Self {
            fingerprint: hex8(&d.fingerprint()),
            name: d.name.clone(),
            paired_at: d.paired_at,
            expires_at: d.expires_at,
            expired: d.is_expired(),
        }
    }
}

impl DeviceSession {
    /// 创建一个空会话（未打开）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// 打开或创建设备库。
    ///
    /// 已经打开过就直接返回，不重复派生 Argon2——那要几十毫秒，
    /// 而这个函数会被状态查询频繁调用。
    ///
    /// # Errors
    ///
    /// 密码错误、文件损坏或写盘失败时返回。
    pub fn open(&self, password: &[u8], default_name: &str) -> Result<DeviceStatus, DeviceError> {
        let path = omy_net::store::default_path().ok_or(DeviceError::NoStorePath)?;

        let store = if path.exists() {
            Store::load(&path, password).map_err(|_| DeviceError::WrongPassword)?
        } else {
            // 首次使用：建一个新身份。设备名允许非法字符（用户的主机名
            // 可能含控制字符），先归一化再交给校验
            let name = sanitize_device_name(default_name);
            let s = Store::create(&name).map_err(|_| DeviceError::BadDeviceName)?;
            s.save(&path, password).map_err(|_| DeviceError::SaveFailed)?;
            s
        };

        let status = status_of(&store, true);
        if let Ok(mut g) = self.inner.lock() {
            *g = Some(Opened {
                store,
                path,
                password: Zeroizing::new(password.to_vec()),
            });
        }
        Ok(status)
    }

    /// 当前状态。未打开时也要能回答——界面要据此决定显示
    /// 「设置设备库密码」还是「输入设备库密码」。
    pub fn status(&self) -> DeviceStatus {
        let exists = omy_net::store::default_path().is_some_and(|p| p.exists());
        match self.inner.lock() {
            Ok(g) => match g.as_ref() {
                Some(o) => status_of(&o.store, exists),
                None => DeviceStatus {
                    exists,
                    opened: false,
                    device_name: None,
                    fingerprint: None,
                    paired_count: 0,
                },
            },
            Err(_) => DeviceStatus {
                exists,
                opened: false,
                device_name: None,
                fingerprint: None,
                paired_count: 0,
            },
        }
    }

    /// 是否已打开。
    pub fn is_open(&self) -> bool {
        self.inner.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// 已配对设备列表。
    pub fn devices(&self) -> Vec<PairedInfo> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|o| o.store.devices().iter().map(PairedInfo::from).collect())
            })
            .unwrap_or_default()
    }

    /// 在已打开的 store 上做一件事，然后存盘。
    ///
    /// 把「改动 + 保存」绑在一起是有意的：分开的话，很容易出现
    /// 改了内存却忘了存盘的情况——症状是重启应用后配对记录消失，
    /// 而这种缺陷在开发时几乎发现不了（进程不重启就一直是对的）。
    ///
    /// # Errors
    ///
    /// 未打开、闭包失败或存盘失败时返回。
    pub fn mutate<T>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<T, DeviceError>,
    ) -> Result<T, DeviceError> {
        let mut g = self.inner.lock().map_err(|_| DeviceError::Internal)?;
        let o = g.as_mut().ok_or(DeviceError::NotOpen)?;
        let out = f(&mut o.store)?;
        o.store
            .save(&o.path, &o.password)
            .map_err(|_| DeviceError::SaveFailed)?;
        Ok(out)
    }

    /// 只读地用一下 store（不存盘）。
    ///
    /// # Errors
    ///
    /// 未打开时返回。
    pub fn read<T>(&self, f: impl FnOnce(&Store) -> T) -> Result<T, DeviceError> {
        let g = self.inner.lock().map_err(|_| DeviceError::Internal)?;
        let o = g.as_ref().ok_or(DeviceError::NotOpen)?;
        Ok(f(&o.store))
    }

    /// 关闭：抹掉内存里的身份与密码。
    ///
    /// 与文件密钥的锁定同时发生。只锁一半会让用户以为都锁了。
    pub fn close(&self) {
        if let Ok(mut g) = self.inner.lock() {
            // Opened 里的 password 是 Zeroizing，drop 时自动抹除；
            // Store 里的私钥由 omy-net 自己负责
            *g = None;
        }
    }
}

impl Default for DeviceSession {
    fn default() -> Self {
        Self::new()
    }
}

/// 由 store 生成状态快照。
fn status_of(s: &Store, exists: bool) -> DeviceStatus {
    DeviceStatus {
        exists,
        opened: true,
        device_name: Some(s.device_name().to_owned()),
        fingerprint: Some(hex8(&s.fingerprint())),
        paired_count: s.devices().len(),
    }
}

/// 设备库相关错误。
///
/// 与 `CmdError` 分开定义，因为这一层不该知道前端翻译键长什么样；
/// 转换在 commands 层做。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceError {
    /// 无法确定设备库路径（拿不到配置目录）。
    NoStorePath,
    /// 密码不对，或文件已损坏。
    WrongPassword,
    /// 设备名不合法。
    BadDeviceName,
    /// 存盘失败。
    SaveFailed,
    /// 设备库尚未打开。
    NotOpen,
    /// 找不到指定设备。
    NoSuchDevice,
    /// 配对握手失败：配对码不对，或对方根本不是 omy。
    ///
    /// 与 [`Self::WrongPassword`] 分开是必须的——那个指的是**设备库
    /// 密码**错。两者都报同一个码的话，用户会去改设备库密码，
    /// 而真正该重试的是配对码。
    PairFailed,
    /// 连接对方失败。
    ConnectFailed,
    /// 用户主动取消。
    ///
    /// 不是错误，但要沿着同一条 `Result` 通道传回来，好让后台任务
    /// 知道「别把这次结束当成失败报给用户」。
    Cancelled,
    /// 与该设备的授权已过期，需要重新配对。
    ///
    /// 与 [`Self::NoSuchDevice`] 分开：前者要重新配对，
    /// 后者是从没配过。用户的处理方式不同。
    AuthExpired,
    /// 局域网里找不到这台设备。
    PeerNotFound,
    /// 对方协议版本不兼容。
    Incompatible,
    /// 加密握手被拒——通常是对方吊销了本机授权。
    Unauthorized,
    /// 尚未连接任何远端设备。
    NotConnected,
    /// 对方返回了错误响应。
    RemoteError,
    /// 锁中毒等内部错误。
    Internal,
}

impl DeviceError {
    /// 前端翻译键。
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NoStorePath => "no_store_path",
            Self::WrongPassword => "store_wrong_password",
            Self::BadDeviceName => "bad_device_name",
            Self::SaveFailed => "store_save_failed",
            Self::NotOpen => "store_not_open",
            Self::NoSuchDevice => "no_such_device",
            Self::PairFailed => "pair_failed",
            Self::ConnectFailed => "connect_failed",
            Self::Cancelled => "cancelled",
            Self::AuthExpired => "auth_expired",
            Self::PeerNotFound => "peer_not_found",
            Self::Incompatible => "incompatible_version",
            Self::Unauthorized => "unauthorized",
            Self::NotConnected => "not_connected",
            Self::RemoteError => "remote_error",
            Self::Internal => "internal",
        }
    }
}

/// 指纹转十六进制。
///
/// 必须补零：`{:x}` 对 `0x0A` 会给出 `a`，指纹长度就不定了，
/// 与 CLI 显示的值也对不上。
#[must_use]
pub fn hex8(fp: &[u8; 8]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

/// 解析 16 位十六进制指纹。
///
/// 容忍常见分隔写法：用户可能从别处复制带冒号或空格的形式。
#[must_use]
pub fn parse_fingerprint(s: &str) -> Option<[u8; 8]> {
    let t = s.trim().replace([':', '-', ' '], "").to_ascii_lowercase();
    if t.len() != 16 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let a = i.checked_mul(2)?;
        let pair = t.get(a..a.checked_add(2)?)?;
        *slot = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// 把任意字符串收拾成合法设备名。
///
/// 主机名可能含控制字符或过长，直接交给 `Store::create` 会失败，
/// 而「首次使用就创建失败」是最糟糕的第一印象。
#[must_use]
pub fn sanitize_device_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    let t = cleaned.trim();
    if t.is_empty() { String::from("omy") } else { t.to_owned() }
}

/// 取本机默认设备名。
#[must_use]
pub fn default_device_name() -> String {
    let raw = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| String::from("omy"));
    sanitize_device_name(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex8_pads_every_byte() {
        assert_eq!(hex8(&[0, 1, 2, 3, 0xAB, 0xCD, 0xEF, 0xFF]), "00010203abcdefff");
        // 高位为 0 时必须补零，否则长度不定、与 CLI 显示的值对不上
        assert_eq!(hex8(&[0x0A; 8]), "0a0a0a0a0a0a0a0a");
        assert_eq!(hex8(&[0; 8]).len(), 16);
    }

    /// 指纹解析必须与 `hex8` 互为逆运算。
    ///
    /// 不一致的话，界面上显示的指纹没法用来指代设备——
    /// 吊销时会找不到，而错误信息只会说「没有这台设备」。
    #[test]
    fn fingerprint_roundtrips() {
        for seed in [0u8, 1, 0x0A, 0x7F, 0x80, 0xFF] {
            let fp = [seed; 8];
            assert_eq!(parse_fingerprint(&hex8(&fp)), Some(fp));
        }
        let mixed = [0x0A, 0x00, 0xFF, 0x10, 0x01, 0xF0, 0x5A, 0xA5];
        assert_eq!(parse_fingerprint(&hex8(&mixed)), Some(mixed));
    }

    #[test]
    fn fingerprint_accepts_separators() {
        let want = [0x00, 0x11, 0x22, 0x33, 0xAA, 0xBB, 0xCC, 0xDD];
        // 用户可能从 CLI 输出或别的界面复制，带各种分隔符
        assert_eq!(parse_fingerprint("00:11:22:33:aa:bb:cc:dd"), Some(want));
        assert_eq!(parse_fingerprint("00112233-aabbccdd"), Some(want));
        assert_eq!(parse_fingerprint("  00112233AABBCCDD  "), Some(want));
    }

    #[test]
    fn fingerprint_rejects_bad_input() {
        assert!(parse_fingerprint("").is_none());
        assert!(parse_fingerprint("00112233aabbccd").is_none(), "少一位");
        assert!(parse_fingerprint("00112233aabbccdde").is_none(), "多一位");
        assert!(parse_fingerprint("00112233aabbccgg").is_none(), "非十六进制");
        assert!(parse_fingerprint("../../etc/passwd").is_none());
    }

    #[test]
    fn device_name_sanitization() {
        assert_eq!(sanitize_device_name("我的电脑"), "我的电脑");
        assert_eq!(sanitize_device_name("  留白  "), "留白");
        assert_eq!(sanitize_device_name("带\u{7}响铃"), "带响铃");
        assert_eq!(sanitize_device_name(""), "omy");
        assert_eq!(sanitize_device_name("   "), "omy");
        assert_eq!(sanitize_device_name("\u{0}\u{1}"), "omy");
        // 超长要截断，否则 Store::create 会拒绝
        assert!(sanitize_device_name(&"x".repeat(200)).len() <= 32);
    }

    /// 收拾过的名字必须能通过 net 侧的校验。
    ///
    /// 否则首次使用就会创建失败，而用户完全不知道该怎么办——
    /// 他并没有输入过任何设备名。
    #[test]
    fn sanitized_names_pass_net_validation() {
        for raw in [
            "",
            "   ",
            "正常名字",
            "带\u{7}控制符",
            &"x".repeat(200),
            "DESKTOP-ABC123",
        ] {
            let n = sanitize_device_name(raw);
            assert!(
                omy_net::discovery::validate_device_name(&n).is_ok(),
                "{raw:?} 收拾成 {n:?} 后仍未通过校验"
            );
        }
    }

    #[test]
    fn default_device_name_is_valid() {
        let n = default_device_name();
        assert!(!n.is_empty());
        assert!(omy_net::discovery::validate_device_name(&n).is_ok());
    }

    #[test]
    fn new_session_is_closed() {
        let s = DeviceSession::new();
        assert!(!s.is_open());
        assert!(s.devices().is_empty());
        let st = s.status();
        assert!(!st.opened);
        assert!(st.device_name.is_none(), "未打开时不能泄露设备名");
        assert!(st.fingerprint.is_none());
    }

    /// 未打开时的操作必须报错，而不是静默成功。
    #[test]
    fn operations_require_open() {
        let s = DeviceSession::new();
        assert_eq!(s.read(|st| st.devices().len()).unwrap_err(), DeviceError::NotOpen);
        let r = s.mutate(|st| {
            st.revoke_all();
            Ok(())
        });
        assert_eq!(r.unwrap_err(), DeviceError::NotOpen);
    }

    #[test]
    fn error_codes_are_stable_keys() {
        // 这些字符串是前端的翻译键，改动会让界面显示原始键名
        assert_eq!(DeviceError::WrongPassword.code(), "store_wrong_password");
        assert_eq!(DeviceError::NotOpen.code(), "store_not_open");
        assert_eq!(DeviceError::NoSuchDevice.code(), "no_such_device");
        // 配对失败与设备库密码错必须是不同的码：报混了的话，
        // 用户会去改设备库密码，而真正该重试的是配对码
        assert_ne!(
            DeviceError::PairFailed.code(),
            DeviceError::WrongPassword.code()
        );
        assert_ne!(
            DeviceError::ConnectFailed.code(),
            DeviceError::PairFailed.code(),
            "连不上和配对码错是两回事，用户要据此判断改地址还是改码"
        );
        // 不能是拼好的句子
        for e in [
            DeviceError::NoStorePath,
            DeviceError::WrongPassword,
            DeviceError::BadDeviceName,
            DeviceError::SaveFailed,
            DeviceError::NotOpen,
            DeviceError::NoSuchDevice,
            DeviceError::PairFailed,
            DeviceError::ConnectFailed,
            DeviceError::Cancelled,
            DeviceError::Internal,
        ] {
            let c = e.code();
            assert!(!c.is_empty());
            assert!(
                c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "{c} 应是 snake_case 翻译键"
            );
        }
    }

    #[test]
    fn paired_info_omits_public_key() {
        let rec = DeviceRecord {
            public_key: vec![0xAB; 32],
            name: String::from("对方设备"),
            paired_at: 1000,
            expires_at: 0,
        };
        let info = PairedInfo::from(&rec);
        let j = serde_json::to_string(&info).unwrap();
        // 公钥进 WebView 没有用途，只会多一处泄露面
        assert!(!j.contains("public_key"), "不该把公钥送进前端");
        assert!(!j.contains("abababab"), "公钥内容不该出现");
        assert_eq!(info.fingerprint.len(), 16);
        assert!(!info.expired, "expires_at=0 表示永久");
    }

    #[test]
    fn paired_info_reports_expiry() {
        let rec = DeviceRecord {
            public_key: vec![1; 32],
            name: String::from("过期设备"),
            paired_at: 1,
            expires_at: 2, // 1970 年就过期了
        };
        assert!(PairedInfo::from(&rec).expired);
    }
}
