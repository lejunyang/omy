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

/// 查询配置与各目录位置。
#[tauri::command]
#[must_use]
pub fn config_paths() -> ConfigPaths {
    let to_s = |p: Option<std::path::PathBuf>| p.map(|x| x.display().to_string());
    ConfigPaths {
        config: to_s(omy_config::config_path()),
        cache: to_s(omy_config::cache_dir()),
        data: to_s(omy_config::data_dir()),
        portable: omy_config::is_portable(),
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
}

#[tauri::command]
#[must_use]
pub fn app_about() -> AppAbout {
    let (major, minor) = omy_core::FORMAT_VERSION;
    AppAbout {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        format_major: major,
        format_minor: minor,
    }
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
