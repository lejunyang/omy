//! 便携式滚动日志：把用户操作、接口调用与错误写到 exe 旁，供事后排查。
//!
//! # 为什么自己写而不引第三方日志库
//!
//! `tracing-appender` / `flexi_logger` 都能做滚动，但：
//!
//! 1. **不引 C 工具链**是本项目的硬约束（选 grammers 的核心理由），日志这种
//!    辅助设施更不该为它破功。自己写只用 std，零新依赖、零工具链风险。
//! 2. `tracing-appender` 当前不在离线依赖缓存里，拉它要联网、还带进
//!    `tracing-subscriber` 一大片表面。
//! 3. **最难的需求是"绝不泄密"**——密码/验证码/auth key/手机号/解密正文
//!    一个都不能进日志。自己控制每一条写入点，比在第三方宏的格式化里
//!    夹一层脱敏可靠得多。
//!
//! 需求本身很小：追加写、按大小切分、超限丢最旧、分级。std 足够。
//!
//! # 落点
//!
//! 复用 [`omy_config::log_dir`]：便携写 `<exe>/omy-data/logs`，非便携写系统
//! 数据目录下的 `omy/logs`。移动端不落文件（见 `init`）。
//!
//! # 大小上限与丢弃
//!
//! 单文件写到 [`MAX_BYTES`] 就滚动：`omy.log` 改名为 `omy.log.1`，原来的
//! `omy.log.1` 变 `.2`……超过 [`KEEP_FILES`] 的最旧那个被删除。于是磁盘占用
//! 有硬上界 `MAX_BYTES * (KEEP_FILES + 1)`，老日志自动丢弃。
//!
//! # 初始化失败不拖垮应用
//!
//! 目录不可写（只读介质、权限不足）时**降级为只往 stderr 打、不写文件**，
//! 绝不 panic。日志是辅助设施，它坏了不该让主程序起不来。

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个日志文件的大小上限（字节）。
///
/// 5 MiB：一条日志约几十到二百字节，5 MiB 能存几万条，够覆盖一次完整的
/// 排查会话；再大则文本编辑器打开变慢，而排查通常只看最近这一段。
const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 除当前文件外，最多保留几个历史文件。
///
/// 保留 4 个历史 + 1 个当前 = 磁盘上界约 25 MiB。便携场景常在 U 盘上，
/// 占用要有明确上界；4 个历史足以回溯到上一两次运行。
const KEEP_FILES: u32 = 4;

/// 当前日志文件名。
const LOG_NAME: &str = "omy.log";

/// 日志级别。刻意只三档：排查时 info/warn/error 已经够分辨，
/// 再细分只会让人纠结该用哪一档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 正常操作与接口调用。
    Info,
    /// 可恢复的异常、降级、非预期但不致命的情况。
    Warn,
    /// 操作失败、错误路径。
    Error,
}

impl Level {
    const fn tag(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

/// 全局日志器。`None` 表示未初始化或已降级为不写文件。
static LOGGER: OnceLock<Option<Logger>> = OnceLock::new();

struct Logger {
    /// 当前日志文件的完整路径。
    path: PathBuf,
    /// 单文件上限与历史保留数。做成字段而非直接读常量，是为了让测试能用
    /// 很小的阈值快速逼出滚动，而不必真写 5 MiB。
    max_bytes: u64,
    keep: u32,
    /// 打开的文件句柄 + 当前已写字节数。放一把锁里：多线程并发写日志时，
    /// 既要序列化写入、又要让"写了多少→要不要滚动"这对判断保持一致。
    inner: Mutex<Sink>,
}

struct Sink {
    file: File,
    written: u64,
}

impl Logger {
    /// 在 `dir` 下打开日志器。目录不可建或文件打不开时返回 `None`（降级）。
    fn open(dir: &std::path::Path, max_bytes: u64, keep: u32) -> Option<Self> {
        if let Err(e) = fs::create_dir_all(dir) {
            eprintln!("[omy] 日志目录不可写，本次不写文件日志：{e}");
            return None;
        }
        let path = dir.join(LOG_NAME);
        let file = match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[omy] 打开日志文件失败，本次不写文件日志：{e}");
                return None;
            }
        };
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Self {
            path,
            max_bytes,
            keep,
            inner: Mutex::new(Sink { file, written }),
        })
    }

    /// 写一条已成型的行（含换行）。锁内完成"按需滚动 + 写 + 计数"。
    fn write_line(&self, line: &str) {
        if let Ok(mut sink) = self.inner.lock() {
            let add = line.len() as u64;
            // 先判滚动：写这条会不会超限。sink.written > 0 避免"单条就超上限"
            // 时反复空滚动
            if sink.written.saturating_add(add) > self.max_bytes && sink.written > 0 {
                rotate(&self.path, self.keep, &mut sink);
            }
            if sink.file.write_all(line.as_bytes()).is_ok() {
                sink.written = sink.written.saturating_add(add);
                // 每条都 flush：排查场景下应用可能随后就崩，缓冲区里的正是
                // 最要紧的最后几条。日志量不大，flush 的开销可接受
                let _ = sink.file.flush();
            }
        }
    }
}

/// 初始化日志。返回实际落点（用于设置页展示）；返回 `None` 表示这次不写文件
/// （移动端、或目录不可写降级）。
///
/// 多次调用只有第一次生效——`OnceLock`。
#[must_use]
pub fn init() -> Option<PathBuf> {
    let slot = LOGGER.get_or_init(build);
    slot.as_ref().map(|l| l.path.clone())
}

/// 当前日志文件路径（已初始化且在写文件时）。供设置页与「打开日志目录」用。
#[must_use]
pub fn current_path() -> Option<PathBuf> {
    LOGGER.get().and_then(|o| o.as_ref()).map(|l| l.path.clone())
}

/// 日志所在目录（即便这次没在写文件，也返回它「本该在哪」，供设置页显示）。
#[must_use]
pub fn dir() -> Option<PathBuf> {
    omy_config::log_dir()
}

fn build() -> Option<Logger> {
    // 目录建不出来（只读介质/权限）就降级：不写文件，只往 stderr 打。
    // 绝不 panic——日志坏了不该拖垮主程序（在 Logger::open 里处理）
    let dir = omy_config::log_dir()?;
    Logger::open(&dir, MAX_BYTES, KEEP_FILES)
}

/// 写一条日志。`target` 是来源分类（如 `cmd` / `rpc` / `ui`），`msg` 是正文。
///
/// **调用方负责脱敏**：这里不做任何过滤，因为"哪些字段是敏感的"只有调用点
/// 知道。约定见模块顶注与 [`redact`]。
pub fn log(level: Level, target: &str, msg: &str) {
    // 未初始化时也别丢：至少 stderr 能看到（比如 init 之前的早期错误）
    let Some(Some(logger)) = LOGGER.get() else {
        if level >= Level::Warn {
            eprintln!("[omy][{}] {target}: {msg}", level.tag());
        }
        return;
    };
    let line = format!("{} [{}] {target}: {msg}\n", timestamp(), level.tag());
    logger.write_line(&line);
}

/// 滚动：`omy.log`→`omy.log.1`→…→`omy.log.KEEP`，最旧的被删。
///
/// 失败时尽量不影响后续写入：换不了就继续往老文件写（顶多超一点上限），
/// 也好过因为滚动失败而丢日志。
fn rotate(path: &std::path::Path, keep: u32, sink: &mut Sink) {
    // 先把最旧的删掉，腾出 .keep 这个名字
    let oldest = path.with_extension(format!("log.{keep}"));
    let _ = fs::remove_file(&oldest);
    // 从 keep-1 往下，逐个改名后移一位
    for i in (1..keep).rev() {
        let from = path.with_extension(format!("log.{i}"));
        let to = path.with_extension(format!("log.{}", i + 1));
        let _ = fs::rename(&from, &to);
    }
    // 当前文件改成 .1
    let first = path.with_extension("log.1");
    let _ = fs::rename(path, &first);
    // 重开一个新的当前文件
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(path) {
        sink.file = f;
        sink.written = 0;
    }
    // 重开失败就继续用老句柄写——老文件已改名，但句柄仍指向那个 inode，
    // 顶多是新日志进了刚改名的 .1，总比丢弃好
}

/// UTC 时间戳，`YYYY-MM-DD HH:MM:SS.mmm`。
///
/// 不引 chrono/time：只用 std 从 UNIX 秒算出年月日。日志时间戳用 UTC，
/// 避免跨时区拷贝日志时对不上；毫秒对排查接口耗时前后顺序有用。
fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let (y, mo, d, h, mi, s) = civil_from_unix(secs);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{millis:03}")
}

/// UNIX 秒 → (年,月,日,时,分,秒) UTC。
///
/// 用 Howard Hinnant 的 days_from_civil 逆算法，纯整数运算、不依赖任何库。
/// 自己算是因为不想为一个时间戳引入 chrono（它会拉进一串传递依赖）。
#[allow(clippy::many_single_char_names)]
fn civil_from_unix(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;
    // days 是从 1970-01-01 起的天数
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0,399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0,365]
    let mp = (5 * doy + 2) / 153; // [0,11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1,31]
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1,12]
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d, h, mi, s)
}

/// 把一段可能含敏感内容的字符串替换成一个不可逆的短标识。
///
/// 用途：偶尔要在日志里区分「是不是同一个值」（比如同一个 auth key 反复失败）
/// 又绝不能记原文时，记它的短哈希。**不是加密，只是让原文不可读**。
///
/// 对密码/验证码/token 这类，**首选压根不记**；只有确实需要「能对上是不是
/// 同一个」时才用这个。
#[must_use]
pub fn redact(secret: &str) -> String {
    // FNV-1a：够短、够快、无依赖。仅用于「同不同」的比对，不做安全用途
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in secret.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("#{:08x}", (hash & 0xffff_ffff) as u32)
}

/// 记一条 info。
pub fn info(target: &str, msg: &str) {
    log(Level::Info, target, msg);
}
/// 记一条 warn。
pub fn warn(target: &str, msg: &str) {
    log(Level::Warn, target, msg);
}
/// 记一条 error。
pub fn error(target: &str, msg: &str) {
    log(Level::Error, target, msg);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 时间戳换算对几个已知点。
    ///
    /// 不这样会怎样：自己写的 civil_from_unix 若算错，所有日志时间都错，
    /// 而排查时正是靠时间对上下游——错的时间比没有时间更误导。
    #[test]
    fn timestamp_known_points() {
        // 2021-01-01 00:00:00 UTC = 1609459200
        assert_eq!(civil_from_unix(1_609_459_200), (2021, 1, 1, 0, 0, 0));
        // 2000-02-29 12:34:56 UTC = 951827696（闰年 2 月 29 日）
        assert_eq!(civil_from_unix(951_827_696), (2000, 2, 29, 12, 34, 56));
        // 1970-01-01 00:00:00
        assert_eq!(civil_from_unix(0), (1970, 1, 1, 0, 0, 0));
    }

    /// redact 对同一输入稳定、对不同输入不同，且不回显原文。
    #[test]
    fn redact_is_stable_and_opaque() {
        let a = redact("hunter2");
        assert_eq!(a, redact("hunter2"), "同一输入必须得到同一标识");
        assert_ne!(a, redact("hunter3"), "不同输入应得到不同标识");
        assert!(!a.contains("hunter"), "标识里绝不能出现原文");
    }

    /// 独立临时目录，避免并发测试互相踩。
    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir()
            .join(format!("omy_applog_test_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&p).ok();
        p
    }

    /// 灌到超上限，确认旧日志真被丢、磁盘占用有硬上界。
    ///
    /// 不这样会怎样：用户要的正是"大小受控、超限丢弃"。只写 rotate 代码不测，
    /// 一旦滚动条件写错（比较差一位、或从不触发），日志会无限增长或从不滚动，
    /// 而这在正常几分钟使用里根本暴露不出来——要跑很久或专门灌才现形。
    #[test]
    fn rotation_caps_total_and_drops_oldest() {
        let dir = tmp("rot");
        let keep = 2u32;
        let cap = 1000u64; // 小阈值快速逼出滚动
        let logger = Logger::open(&dir, cap, keep).expect("应能在临时目录建日志");
        for i in 0..500 {
            logger.write_line(&format!(
                "line {i:04} some padding to make each line long enough xx\n"
            ));
        }
        let cur = dir.join(LOG_NAME);
        assert!(cur.exists(), "当前日志文件应在");
        assert!(dir.join("omy.log.1").exists(), "应有 .1");
        assert!(dir.join("omy.log.2").exists(), "应有 .2");
        assert!(
            !dir.join("omy.log.3").exists(),
            "超过 keep 的历史必须被丢弃，.3 不该存在"
        );
        let total: u64 = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum();
        assert!(
            total <= cap * u64::from(keep + 1) + 100,
            "总占用 {total} 应受 {} 上界约束",
            cap * u64::from(keep + 1)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 泄密扫描：把假密码/假 token 交给 redact 后写日志，断言原文搜不到。
    ///
    /// 这是加日志最大的风险面——凭据一旦进日志就等于明文落盘。人眼 review
    /// 挡不住"某天有人 log 了整个请求参数"，必须有断言把这条守住。
    #[test]
    fn secrets_never_land_in_log_text() {
        let dir = tmp("sec");
        let logger = Logger::open(&dir, MAX_BYTES, KEEP_FILES).expect("建日志");
        let password = "SuperSecretPw!42";
        let token = "1a2b3c4d5e6f-authkey-DEADBEEF";
        let phone = "+8613800138000";
        logger.write_line(&format!(
            "{} [INFO] rpc: signIn ok user={} pw={}\n",
            timestamp(),
            redact(phone),
            redact(password)
        ));
        logger.write_line(&format!(
            "{} [ERROR] rpc: auth failed key={}\n",
            timestamp(),
            redact(token)
        ));
        let body = std::fs::read_to_string(dir.join(LOG_NAME)).expect("读回日志");
        for secret in [password, token, phone, "138001", "DEADBEEF", "SecretPw"] {
            assert!(
                !body.contains(secret),
                "日志里绝不能出现敏感原文，命中了：{secret}"
            );
        }
        assert!(body.contains(&redact(password)), "应记下脱敏后的标识");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 目录不可写时降级为 None，不 panic。
    ///
    /// 不这样会怎样：只读介质/权限不足时若 panic，omy-gui 禁 panic、release 下
    /// 直接 abort——日志这种辅助设施反倒把主程序带崩了。
    #[test]
    fn unwritable_degrades_to_none() {
        let file = tmp("deg_file");
        std::fs::create_dir_all(file.parent().unwrap()).ok();
        std::fs::write(&file, b"x").expect("写占位文件");
        // 在一个"是文件"的路径下建目录，create_dir_all 注定失败
        let under_file = file.join("logs");
        let r = Logger::open(&under_file, MAX_BYTES, KEEP_FILES);
        assert!(r.is_none(), "目录不可建时应降级为 None 而非 panic");
        std::fs::remove_file(&file).ok();
    }
}
