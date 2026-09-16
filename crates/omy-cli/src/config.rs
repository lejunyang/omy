//! 配置文件。
//!
//! 定义与读写实现在 [`omy_config`]，CLI 与 GUI 共用同一份——两边各写一份
//! 的话，同一个键可以有不同默认值，而用户完全看不出为什么 CLI 和 GUI
//! 行为不一致。
//!
//! 本模块只保留 CLI 特有的两件事：
//!
//! 1. `--config` 显式指定路径时，读不到必须报错（而默认路径不存在则用
//!    内置默认值）——用户明确指定的文件被静默忽略，会让人以为配置生效了。
//! 2. 解析到未知键时给出提示。CLI 的配置多半是手写的，拼错一个键却毫无
//!    反馈，用户会一直以为设置生效了。GUI 那边不做这个提示：它的配置由
//!    界面写入，不会拼错，而未知键通常来自更高版本。

use anyhow::{Context, Result};
use std::path::Path;

pub use omy_config::Config;

/// 加载配置。
///
/// `explicit` 为 `--config` 指定的路径。
///
/// # Errors
///
/// 显式指定的文件不存在或解析失败，或默认路径的文件存在但解析失败。
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    if let Some(p) = explicit {
        return Config::load_from(p).with_context(|| format!("读取配置文件 {} 失败", p.display()));
    }

    let loaded = omy_config::Config::load_checked().context("读取配置失败")?;
    // 拼错的键不会让解析失败（否则高版本写的新键会让低版本完全读不了
    // 配置），所以必须在这里主动说出来，不然用户永远不知道自己写错了
    for k in &loaded.unknown_keys {
        eprintln!("警告: 配置中有无法识别的项 `{k}`，已忽略");
    }
    Ok(loaded.config)
}

/// 默认配置文件路径。
#[must_use]
pub fn default_path() -> Option<std::path::PathBuf> {
    omy_config::config_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 显式指定的文件不存在必须报错。
    ///
    /// 不这样会怎样：用户 `--config` 指了个打错字的路径，程序用默认值
    /// 跑完并报告成功，而他以为自己的配置生效了。
    #[test]
    fn explicit_missing_path_errors() {
        let p = std::env::temp_dir().join("omy_cli_no_such_config_xyz.toml");
        assert!(load(Some(&p)).is_err(), "显式指定的缺失文件应报错");
    }

    /// 默认值必须与共用实现一致。
    ///
    /// 不这样会怎样：CLI 这层若自己兜了一份默认值，与 GUI 不一致时
    /// 两边行为会分叉，而配置文件里根本看不到这一项。
    #[test]
    fn defaults_come_from_shared_crate() {
        let c = Config::default();
        assert_eq!(c.defaults.kdf_profile, "interactive");
        assert_eq!(c.defaults.chunk_size, "256K");
        assert_eq!(c.ui.language, "auto");
    }
}
