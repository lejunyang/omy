//! 设置：读写配置文件。
//!
//! # 为什么不用 localStorage
//!
//! 原先语言与主题存在 WebView 的 `localStorage` 里。那对这两项勉强够用，
//! 但设置页里多数项要由**后端**执行：缓存上限归 `omy-remote` 管，自动锁定
//! 归会话管，删除行为归 `fileops` 管。存在 WebView 里后端根本读不到。
//!
//! 而且 `localStorage` 会随「清除数据」丢失，用户会发现设置莫名回到默认。
//!
//! # 前端拿到的是整份配置
//!
//! 没有做成一项一个命令。设置页一次要显示十几项，逐项调用意味着十几次
//! IPC 往返；而保存时整份写回也更容易保证一致性——不会出现「改了两项，
//! 第一项成功第二项失败」这种半截状态。

use crate::commands::{CmdError, CmdResult};

/// 把配置错误转成前端可翻译的结构化错误。
///
/// 不直接把 `e.to_string()` 丢给前端：那是中文的，而用户可能把界面切成
/// 英文（见 `commands` 模块开头的约定）。这里只给错误码加上原始细节，
/// 由前端决定怎么措辞。
fn to_cmd_err(code: &str, e: &omy_config::Error) -> CmdError {
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

/// 配置文件与各目录的位置，供设置页显示。
///
/// 用户需要知道「我的设置到底存在哪」——尤其是便携模式下，这决定了
/// 拷贝哪个文件夹能带走全部状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigPaths {
    /// 配置文件绝对路径。
    pub config: Option<String>,
    /// 缓存目录。
    pub cache: Option<String>,
    /// 数据目录（设备库等）。
    pub data: Option<String>,
    /// 日志目录（排查用）。用户加日志就是为了排查，找不到等于没有，
    /// 所以设置页要能直接显示它在哪、并一键打开。
    pub log: Option<String>,
    /// 是否为便携模式（放在可执行文件旁）。
    pub portable: bool,
}

/// 读取当前配置。
///
/// # Errors
///
/// 配置文件存在但损坏时返回 `config_read_failed`。
#[tauri::command]
pub fn config_get() -> CmdResult<omy_config::Config> {
    omy_config::Config::load().map_err(|e| to_cmd_err("config_read_failed", &e))
}

/// 写回整份配置。
///
/// # Errors
///
/// 无法确定路径或写盘失败时返回 `config_write_failed`。
#[tauri::command]
pub fn config_set(config: omy_config::Config) -> CmdResult<()> {
    config.save().map_err(|e| to_cmd_err("config_write_failed", &e))
}

/// 前端上报一条用户操作日志（进对话、切账号、切视图、pin 等）。
///
/// 为什么要前端上报：后端日志只看得到「发了什么请求」，看不到「用户点了
/// 什么」。用户排查时想知道自己的操作序列，需要这条。前端只传一个已在代码里
/// 写死的简短动作标识（如 `enter-dialog` / `switch-account`），**不传任何
/// 用户内容**——保持日志不泄密的红线。
#[tauri::command]
pub fn ui_log(action: String, detail: Option<String>) {
    // detail 也只该是前端传的非敏感短串（如视图名 files/messages）；
    // 真要带对话/文件标识，调用方自己先 redact。这里原样记，责任在调用点。
    let msg = match detail {
        Some(d) if !d.is_empty() => format!("{action} {d}"),
        _ => action,
    };
    crate::applog::info("ui", &msg);
}

/// 查询配置与各目录位置。
#[tauri::command]
#[must_use]
pub fn config_paths() -> ConfigPaths {
    let to_s = |p: Option<std::path::PathBuf>| p.map(|x| x.display().to_string());
    ConfigPaths {
        config: to_s(omy_config::config_path()),
        cache: to_s(omy_config::cache_dir()),
        data: to_s(omy_config::data_dir()),
        // 优先给「实际在写的那个文件所在目录」；没在写文件（移动端/降级）时
        // 回落到「本该在哪」，让界面仍能说清位置
        log: to_s(crate::applog::current_path()
            .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
            .or_else(crate::applog::dir)),
        portable: omy_config::is_portable(),
    }
}

/// 在系统文件管理器里打开日志目录。
///
/// 复用回收站那套的思路——桌面端才有「文件管理器」这个概念，移动端没有。
/// 打不开（目录不存在、无桌面环境）时返回结构化错误，界面提示用户手动去
/// 那个路径找，而不是静默失败。
///
/// # Errors
///
/// 日志目录未知、或系统调用失败时返回。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[tauri::command]
pub fn open_log_dir() -> CmdResult<()> {
    let dir = crate::applog::current_path()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
        .or_else(crate::applog::dir)
        .ok_or_else(|| CmdError::code("log_dir_unknown"))?;
    // 目录可能还没建（还没写过日志）——先确保它在，否则打开会失败
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Err(CmdError::with("log_dir_open_failed", serde_json::json!({
            "detail": e.to_string()
        })));
    }
    opener_reveal(&dir)
}

/// 移动端没有文件管理器可打开，命令仍要存在（前端统一调用），直接返回不支持。
#[cfg(any(target_os = "android", target_os = "ios"))]
#[tauri::command]
pub fn open_log_dir() -> CmdResult<()> {
    Err(CmdError::code("log_dir_unsupported"))
}

/// 用系统默认方式在文件管理器里显示某个目录。
///
/// 不引第三方 opener crate：各平台就一条命令，std::process 够了，且能避免
/// 又拉进一串依赖。Android 已被外层 cfg 排除，这里只处理三个桌面平台。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn opener_reveal(dir: &std::path::Path) -> CmdResult<()> {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(dir).spawn();
    // Linux 等：xdg-open。Android 走不到这（上面 cfg 已排除），
    // 否则会去调一个不存在的命令
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(dir).spawn();
    match r {
        // explorer 打开目录时返回码可能非 0，但目录确实开了，不据此判失败
        Ok(_) => Ok(()),
        Err(e) => Err(CmdError::with("log_dir_open_failed", serde_json::json!({
            "detail": e.to_string()
        }))),
    }
}

/// 应用与文件格式版本信息，供设置页「关于」显示。
///
/// 版本不能在前端写死：前端打包进二进制后，随 Cargo.toml 一起发版，
/// 写死的字符串很容易在发版时漏掉更新。格式版本直接取 omy-core 的常量，
/// 保证「关于」页声称的格式与实际加解密用的是同一个来源。
#[derive(Debug, Clone, serde::Serialize)]
pub struct AppAbout {
    /// 应用版本（来自本 crate 的 Cargo.toml）。
    pub app_version: String,
    /// OMYFILE 格式主版本。
    pub format_major: u16,
    /// OMYFILE 格式次版本。
    pub format_minor: u16,
    /// 构建时的 git 短 hash（由 build.rs 注入），用于自证「这是哪一版」。
    pub build_git: String,
    /// 构建时间，`YYYY-MM-DD HH:MM UTC`。
    pub build_time: String,
}

#[tauri::command]
#[must_use]
pub fn app_about() -> AppAbout {
    let (major, minor) = omy_core::FORMAT_VERSION;
    AppAbout {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        format_major: major,
        format_minor: minor,
        // 由 build.rs 注入。option_env! 而非 env!：万一构建脚本没跑（极少见），
        // 也不至于编译不过，回落到 unknown
        build_git: option_env!("OMY_BUILD_GIT").unwrap_or("unknown").to_string(),
        build_time: build_time_string(),
    }
}

/// 把 build.rs 注入的构建 UNIX 秒转成 `YYYY-MM-DD HH:MM UTC`。
///
/// 不引 chrono：用与 applog 同源的 civil-from-unix 纯整数算法。这里只到分钟，
/// 够用户和 `git log` 对上是哪一版。
fn build_time_string() -> String {
    let secs: u64 = option_env!("OMY_BUILD_UNIX")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if secs == 0 {
        return String::from("unknown");
    }
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let (h, mi) = ((rem / 3600) as u32, ((rem % 3600) / 60) as u32);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 路径查询必须给出便携标志。
    ///
    /// 不这样会怎样：设置页无法告诉用户设置存在哪，而便携模式下
    /// 「拷走哪个目录能带走全部状态」正是用户最需要知道的一件事。
    #[test]
    fn paths_report_portable_flag() {
        let p = config_paths();
        // 三个目录要么都能确定，要么都不能——它们同源
        if p.config.is_some() {
            assert!(p.cache.is_some(), "配置能定位时缓存目录也应能定位");
            assert!(p.data.is_some());
        }
        // portable 是 bool，这里只确认调用不 panic 且字段存在
        let j = serde_json::to_value(&p).expect("应可序列化");
        assert!(j.get("portable").is_some(), "前端要靠这个字段决定是否显示便携提示");
        // 日志路径字段必须在——设置页要显示「日志在哪」，缺了它这条排查
        // 入口就等于没做，而序列化不会因为少个字段报错
        assert!(j.get("log").is_some(), "设置页要靠 log 字段显示日志目录");
    }

    /// 配置能完整地序列化给前端。
    ///
    /// 不这样会怎样：某个新增分组忘了派生 Serialize，前端读到的是
    /// 残缺对象，设置页会把缺失项显示成空白并在保存时写回默认值——
    /// 用户的设置就被悄悄重置了。
    #[test]
    fn config_serializes_all_groups() {
        let c = omy_config::Config::default();
        let j = serde_json::to_value(&c).expect("应可序列化");
        for g in ["defaults", "compress", "scan", "serve", "ui", "security", "remote"] {
            assert!(j.get(g).is_some(), "分组 {g} 缺失，前端会读到 undefined");
        }
        // 设置页直接读这些键，改名等于改协议
        assert!(j["ui"].get("language").is_some());
        assert!(j["ui"].get("theme").is_some());
        assert!(j["security"].get("auto_lock_secs").is_some());
        assert!(j["remote"].get("cache_limit").is_some());
    }

    /// 错误要带 code 而不是拼好的中文。
    ///
    /// 不这样会怎样：用户把界面切成英文后，错误提示里会突然冒出中文。
    #[test]
    fn errors_are_structured() {
        let e = to_cmd_err(
            "config_write_failed",
            &omy_config::Error::NoPath,
        );
        assert_eq!(e.code, "config_write_failed");
        assert!(e.params.get("detail").is_some(), "细节要单独放，供排查用");
    }
}
