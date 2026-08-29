//! 子命令实现。
//!
//! 每个子命令一个模块，都遵循同样的形状：`Args` 结构 + `run` 函数。
//! 共享的参数解析（KDF 档位、算法、文件名模式）集中在本文件，
//! 避免各命令各自解析导致取值范围不一致。

pub mod bench;
pub mod cat;
pub mod completion;
pub mod decrypt;
pub mod doctor;
pub mod encrypt;
pub mod info;
pub mod key;
pub mod list;
pub mod scan;
pub mod shard;
pub mod verify;

use crate::config::Config;
use crate::output::Out;
use anyhow::{Result, bail};
use clap::ValueEnum;
use omy_core::crypto::{Argon2Params, CipherId};

/// 子命令共享的上下文。
pub struct Ctx<'a> {
    /// 输出通道。
    pub out: &'a Out,
    /// 已加载的配置。
    pub cfg: &'a Config,
    /// 是否跳过确认。
    pub assume_yes: bool,
}

/// KDF 档位。取值与设计文档 03 号 §4 的表格一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum KdfProfile {
    /// m=32 MiB, t=4 —— 移动端
    Mobile,
    /// m=64 MiB, t=3 —— 桌面默认（OWASP 基线）
    Interactive,
    /// m=256 MiB, t=4
    Moderate,
    /// m=1 GiB, t=4
    Sensitive,
}

impl KdfProfile {
    /// 转为 core 的参数。
    #[must_use]
    pub const fn params(self) -> Argon2Params {
        match self {
            Self::Mobile => Argon2Params::MOBILE,
            Self::Interactive => Argon2Params::INTERACTIVE,
            Self::Moderate => Argon2Params::MODERATE,
            Self::Sensitive => Argon2Params::SENSITIVE,
        }
    }

    /// 档位名。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Mobile => "mobile",
            Self::Interactive => "interactive",
            Self::Moderate => "moderate",
            Self::Sensitive => "sensitive",
        }
    }

    /// 按名称解析，用于读取配置文件。
    ///
    /// # Errors
    ///
    /// 名称不在四个档位内时返回错误。
    pub fn from_name(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "mobile" => Ok(Self::Mobile),
            "interactive" => Ok(Self::Interactive),
            "moderate" => Ok(Self::Moderate),
            "sensitive" => Ok(Self::Sensitive),
            other => bail!(
                "未知的 KDF 档位 {other:?}，可选：mobile / interactive / moderate / sensitive"
            ),
        }
    }
}

/// AEAD 算法选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Cipher {
    /// XChaCha20-Poly1305，默认。无 AES 硬件加速时也快。
    Xchacha20,
    /// AES-256-GCM。有 AES-NI 时更快。
    Aes256gcm,
}

impl Cipher {
    /// 转为 core 的算法 id。
    #[must_use]
    pub const fn id(self) -> CipherId {
        match self {
            Self::Xchacha20 => CipherId::ChaCha20Poly1305,
            Self::Aes256gcm => CipherId::Aes256Gcm,
        }
    }

    /// 按名称解析，用于读取配置文件。
    ///
    /// # Errors
    ///
    /// 名称无法识别时返回错误。
    pub fn from_name(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "xchacha20" | "xchacha20poly1305" | "chacha20poly1305" => Ok(Self::Xchacha20),
            "aes256gcm" | "aesgcm" | "aes" => Ok(Self::Aes256gcm),
            other => bail!("未知的加密算法 {other:?}，可选：xchacha20 / aes256gcm"),
        }
    }
}

/// 文件名处理模式，对应决策 D-01。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NameMode {
    /// 完全加密文件名，含后缀。
    Encrypt,
    /// 加密文件名但明文保留后缀，便于按类型筛选。
    KeepExt,
    /// 不加密文件名。
    Plain,
}

impl NameMode {
    /// 按名称解析，用于读取配置文件。
    ///
    /// # Errors
    ///
    /// 名称无法识别时返回错误。
    pub fn from_name(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().replace('_', "-").as_str() {
            "encrypt" => Ok(Self::Encrypt),
            "keep-ext" | "keepext" => Ok(Self::KeepExt),
            "plain" => Ok(Self::Plain),
            other => bail!("未知的文件名模式 {other:?}，可选：encrypt / keep-ext / plain"),
        }
    }
}

/// 判断路径是否像 `.omy` 文件。
///
/// 只看后缀，用于快速筛选；真正的识别靠文件头（见 `omy_core::is_omy_file`）。
#[must_use]
pub fn has_omy_ext(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("omy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kdf_profile_names_roundtrip() {
        for p in [
            KdfProfile::Mobile,
            KdfProfile::Interactive,
            KdfProfile::Moderate,
            KdfProfile::Sensitive,
        ] {
            assert_eq!(KdfProfile::from_name(p.name()).unwrap(), p);
            // 大小写不敏感
            assert_eq!(
                KdfProfile::from_name(&p.name().to_uppercase()).unwrap(),
                p
            );
        }
        assert!(KdfProfile::from_name("nope").is_err());
    }

    #[test]
    fn kdf_params_match_documented_table() {
        // 与设计文档 03 号 §4 对齐
        assert_eq!(KdfProfile::Mobile.params().m_kib, 32 * 1024);
        assert_eq!(KdfProfile::Interactive.params().m_kib, 64 * 1024);
        assert_eq!(KdfProfile::Moderate.params().m_kib, 256 * 1024);
        assert_eq!(KdfProfile::Sensitive.params().m_kib, 1024 * 1024);
        assert_eq!(KdfProfile::Interactive.params().t, 3);
        assert_eq!(KdfProfile::Mobile.params().t, 4);
    }

    #[test]
    fn cipher_aliases_accepted() {
        assert_eq!(Cipher::from_name("xchacha20").unwrap(), Cipher::Xchacha20);
        assert_eq!(
            Cipher::from_name("XChaCha20-Poly1305").unwrap(),
            Cipher::Xchacha20
        );
        assert_eq!(Cipher::from_name("aes256gcm").unwrap(), Cipher::Aes256gcm);
        assert_eq!(Cipher::from_name("AES-256-GCM").unwrap(), Cipher::Aes256gcm);
        assert!(Cipher::from_name("blowfish").is_err());
    }

    #[test]
    fn name_mode_aliases() {
        assert_eq!(NameMode::from_name("encrypt").unwrap(), NameMode::Encrypt);
        assert_eq!(NameMode::from_name("keep-ext").unwrap(), NameMode::KeepExt);
        assert_eq!(NameMode::from_name("keep_ext").unwrap(), NameMode::KeepExt);
        assert_eq!(NameMode::from_name("plain").unwrap(), NameMode::Plain);
        assert!(NameMode::from_name("hidden").is_err());
    }

    #[test]
    fn omy_ext_detection() {
        use std::path::Path;
        assert!(has_omy_ext(Path::new("a.omy")));
        assert!(has_omy_ext(Path::new("a.OMY")));
        assert!(has_omy_ext(Path::new("dir/b.Omy")));
        assert!(!has_omy_ext(Path::new("a.txt")));
        assert!(!has_omy_ext(Path::new("a.omy.001")));
        assert!(!has_omy_ext(Path::new("omy")));
    }
}
