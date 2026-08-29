//! 配置文件。
//!
//! 路径遵循 XDG：`~/.config/omy/config.toml`（Windows 上为
//! `%APPDATA%\omy\config.toml`）。格式见设计文档 [09 号 §7](../../docs/research/09-cli-design.md)。
//!
//! 优先级：命令行参数 > 环境变量 > 配置文件 > 内置默认。
//! 本模块只负责最后两层；前两层由 clap 与各子命令处理。

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// 顶层配置。所有字段都可缺省。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// 默认参数。
    pub defaults: Defaults,
    /// 压缩相关。
    pub compress: Compress,
    /// 扫描相关。
    pub scan: Scan,
    /// 共享相关。
    pub serve: Serve,
    /// 界面相关。
    pub ui: Ui,
}

/// 默认加密参数。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Defaults {
    /// KDF 档位：mobile | interactive | moderate | sensitive
    pub kdf_profile: String,
    /// 加密算法：xchacha20 | aes256gcm
    pub cipher: String,
    /// 分块大小，如 `256K`
    pub chunk_size: String,
    /// 文件名模式：encrypt | keep-ext | plain
    pub name_mode: String,
    /// 原文件处理：keep | trash | delete
    pub original_action: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            kdf_profile: "interactive".into(),
            cipher: "xchacha20".into(),
            chunk_size: "256K".into(),
            name_mode: "encrypt".into(),
            original_action: "keep".into(),
        }
    }
}

/// 压缩配置。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Compress {
    /// 是否默认启用。
    pub enabled: bool,
    /// zstd 级别。
    pub level: i32,
}

impl Default for Compress {
    fn default() -> Self {
        Self { enabled: false, level: 3 }
    }
}

/// 扫描配置。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scan {
    /// 默认扫描路径。
    pub paths: Vec<String>,
    /// 最大递归深度。
    pub max_depth: usize,
}

impl Default for Scan {
    fn default() -> Self {
        Self { paths: Vec::new(), max_depth: 8 }
    }
}

/// 局域网共享配置。
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Serve {
    /// 本机显示名。
    pub device_name: Option<String>,
    /// 默认有效期，如 `24h`。
    pub default_expire: Option<String>,
}

/// 界面配置。
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// 语言：auto | zh-CN | en
    pub language: Option<String>,
}

impl Default for Ui {
    fn default() -> Self {
        Self { language: Some("auto".into()) }
    }
}

impl Config {
    /// 加载配置。
    ///
    /// `explicit` 为 `--config` 指定的路径：指定了却读不到必须报错，
    /// 而默认路径不存在则用内置默认值。这个区别很重要——
    /// 用户明确指定的文件被静默忽略会让人以为配置生效了。
    ///
    /// # Errors
    ///
    /// 显式指定的文件不存在或解析失败，或默认路径的文件存在但解析失败。
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        if let Some(p) = explicit {
            let text = std::fs::read_to_string(p)
                .with_context(|| format!("读取配置文件 {} 失败", p.display()))?;
            return toml::from_str(&text)
                .with_context(|| format!("解析配置文件 {} 失败", p.display()));
        }

        let Some(p) = default_path() else {
            return Ok(Self::default());
        };
        if !p.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("读取配置文件 {} 失败", p.display()))?;
        toml::from_str(&text).with_context(|| format!("解析配置文件 {} 失败", p.display()))
    }
}

/// 默认配置文件路径。
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("omy").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_documented_values() {
        let c = Config::default();
        assert_eq!(c.defaults.kdf_profile, "interactive");
        assert_eq!(c.defaults.cipher, "xchacha20");
        assert_eq!(c.defaults.chunk_size, "256K");
        assert_eq!(c.defaults.name_mode, "encrypt");
        assert_eq!(c.defaults.original_action, "keep");
        assert!(!c.compress.enabled);
        assert_eq!(c.compress.level, 3);
        assert_eq!(c.scan.max_depth, 8);
        assert_eq!(c.ui.language.as_deref(), Some("auto"));
    }

    #[test]
    fn parses_documented_example() {
        // 这段与设计文档 09 §7 的示例一致，若文档改了此测试应同步
        let text = r#"
[defaults]
kdf_profile = "moderate"
cipher = "aes256gcm"
chunk_size = "4M"
name_mode = "keep-ext"
original_action = "trash"

[compress]
enabled = true
level = 12

[scan]
paths = ["~/Documents", "~/Videos"]
max_depth = 5

[serve]
device_name = "书房台式机"
default_expire = "24h"

[ui]
language = "zh-CN"
"#;
        let c: Config = toml::from_str(text).unwrap();
        assert_eq!(c.defaults.kdf_profile, "moderate");
        assert_eq!(c.defaults.chunk_size, "4M");
        assert!(c.compress.enabled);
        assert_eq!(c.compress.level, 12);
        assert_eq!(c.scan.paths.len(), 2);
        assert_eq!(c.scan.max_depth, 5);
        assert_eq!(c.serve.device_name.as_deref(), Some("书房台式机"));
        assert_eq!(c.ui.language.as_deref(), Some("zh-CN"));
    }

    #[test]
    fn partial_config_uses_defaults_for_rest() {
        let c: Config = toml::from_str("[compress]\nenabled = true\n").unwrap();
        assert!(c.compress.enabled);
        assert_eq!(c.compress.level, 3, "未指定的项应回落到默认值");
        assert_eq!(c.defaults.cipher, "xchacha20");
    }

    #[test]
    fn unknown_field_is_rejected() {
        // 拼错的键必须报错。静默忽略会让用户以为配置生效了。
        let r: Result<Config, _> = toml::from_str("[defaults]\nkdf_profil = \"mobile\"\n");
        assert!(r.is_err(), "未知字段本应被拒绝");
    }

    #[test]
    fn empty_config_is_valid() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.defaults.cipher, "xchacha20");
    }

    #[test]
    fn explicit_missing_path_errors() {
        let p = std::env::temp_dir().join("omy_definitely_no_such_config_xyz.toml");
        assert!(Config::load(Some(&p)).is_err(), "显式指定的缺失文件应报错");
    }
}
