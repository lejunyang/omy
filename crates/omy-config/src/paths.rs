//! 配置、缓存与数据目录的位置。
//!
//! # 便携优先：放在可执行文件旁边
//!
//! 桌面端默认把配置与缓存放在 **exe 所在目录**，而不是
//! `%APPDATA%` / `~/.config`。这与多数应用相反，是刻意的：
//!
//! - omy 是加密工具，用户常把它连同加密文件一起放在移动硬盘或 U 盘上。
//!   配置散落在系统目录里，换台机器就全没了。
//! - 「我的东西都在这一个文件夹里」对这类工具是重要属性：想彻底清理时
//!   删掉目录即可，不用去翻系统目录里的残留。
//!
//! # 但不能盲目相信可执行文件目录可写
//!
//! 装在 `C:\Program Files` 或 `/usr/bin` 下时那里是只读的。此时静默写
//! 失败会让「设置保存了但重启就没了」，用户完全无从判断原因。所以
//! [`config_path`] 会**实测可写性**，不可写则回落到系统配置目录。
//!
//! 判定用真实写入而不是看权限位：Windows 的 ACL、Unix 的挂载选项
//! （只读挂载、noexec）都不是单看权限位能判断的，而这里判断错的代价
//! 是用户的设置悄悄丢失。
//!
//! # 移动端不适用
//!
//! Android/iOS 上应用目录不可写，也没有「可执行文件旁边」这个概念，
//! 一律用系统给的 app data 目录。

use std::path::{Path, PathBuf};

/// 配置文件名。
const CONFIG_NAME: &str = "config.toml";
/// 便携模式下，数据统一放在 exe 旁的这个子目录里。
///
/// 不直接把 config.toml 丢在 exe 同级：应用还会写缓存、设备库等，
/// 散在安装目录里会和程序文件混成一团，用户想备份也不知道该拷哪些。
const PORTABLE_DIR: &str = "omy-data";

/// 当前是否处于便携模式（配置放在可执行文件旁边）。
#[must_use]
pub fn is_portable() -> bool {
    portable_root().is_some()
}

/// 配置文件路径。
///
/// 顺序：可执行文件旁的 `omy-data/config.toml` → 系统配置目录。
/// 返回 `None` 表示两者都不可用（极少见，通常是嵌入式或权限极受限环境）。
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.join(CONFIG_NAME));
    }
    dirs::config_dir().map(|d| d.join("omy").join(CONFIG_NAME))
}

/// 密文缓存目录。
///
/// 与配置同源：便携模式下在 exe 旁，否则用系统缓存目录。
/// 移动端走 `dirs::cache_dir()`，那里返回的是 app 私有目录。
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.join("cache"));
    }
    dirs::cache_dir().map(|d| d.join("omy"))
}

/// 应用数据目录（设备库、令牌等）。
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.join("data"));
    }
    dirs::data_dir().map(|d| d.join("omy"))
}

/// 日志目录。
///
/// 与配置同源：便携模式下在 exe 旁的 `omy-data/logs`，否则用系统数据目录下的
/// `omy/logs`。移动端 `portable_root()` 恒为 `None`（见其定义），会走后一支
/// ——但移动端**不落文件日志**（应用目录不可写，见模块顶注），由日志初始化
/// 那边按平台决定不启用文件写入，这里只负责给出「假如要写，写哪」。
///
/// 复用 [`portable_root`]，不自己再定位一次 exe：同一逻辑两处实现，改了一处
/// 忘了另一处，会出现「配置在 A、日志在 B」这种最难排查的分裂。
#[must_use]
pub fn log_dir() -> Option<PathBuf> {
    if let Some(root) = portable_root() {
        return Some(root.join("logs"));
    }
    dirs::data_dir().map(|d| d.join("omy").join("logs"))
}

/// 可执行文件旁的数据目录，仅在**确认可写**时返回。
///
/// 移动端直接返回 `None`：那里没有便携这个概念，应用目录也不可写。
#[cfg(any(target_os = "android", target_os = "ios"))]
fn portable_root() -> Option<PathBuf> {
    None
}

/// 可执行文件旁的数据目录，仅在**确认可写**时返回。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn portable_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let root = dir.join(PORTABLE_DIR);

    // 目录可能还不存在（首次运行），创建失败即说明装在只读位置
    if std::fs::create_dir_all(&root).is_err() {
        return None;
    }
    if is_writable(&root) { Some(root) } else { None }
}

/// 实测目录可写性。
///
/// 为什么不看权限位：Windows 的 ACL 与 Unix 的只读挂载都不是单看
/// 权限位能判断的，而判断错的后果是用户的设置悄悄丢失。真写一个
/// 临时文件再删掉，是唯一可靠的判定。
#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".omy-write-probe");
    match std::fs::write(&probe, b"1") {
        Ok(()) => {
            // 删不掉不影响可写判定，忽略结果
            std::fs::remove_file(&probe).ok();
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个目录必须落在同一个根下，否则「便携」这个承诺就是假的。
    ///
    /// 不这样会怎样：用户拷走整个文件夹，却发现缓存或设备库留在了
    /// 系统目录里，换机后状态不完整，而现象是「有些设置带过来了、
    /// 有些没有」，极难归因。
    #[test]
    fn portable_paths_share_one_root() {
        let Some(cfg) = config_path() else {
            return; // 环境不支持，跳过
        };
        if !is_portable() {
            return; // 非便携环境下本断言不适用
        }
        let cache = cache_dir().expect("便携模式下应有缓存目录");
        let data = data_dir().expect("便携模式下应有数据目录");
        let root = cfg.parent().expect("配置应有父目录");
        assert!(cache.starts_with(root), "缓存应与配置同根");
        assert!(data.starts_with(root), "数据应与配置同根");
    }

    /// 配置文件名固定，路径必须以它结尾。
    #[test]
    fn config_path_ends_with_filename() {
        if let Some(p) = config_path() {
            assert_eq!(p.file_name().and_then(|s| s.to_str()), Some(CONFIG_NAME));
        }
    }

    /// 可写性判定要能识别出不存在的目录。
    ///
    /// 不这样会怎样：把不可写误判为可写，配置写入会在每次保存时失败，
    /// 而用户只看到「设置没保存住」。
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn unwritable_dir_is_detected() {
        let missing = std::env::temp_dir().join("omy_no_such_dir_for_probe_xyz");
        std::fs::remove_dir_all(&missing).ok();
        assert!(!is_writable(&missing), "不存在的目录不应判为可写");

        let ok = std::env::temp_dir();
        assert!(is_writable(&ok), "临时目录应可写");
    }

    /// 移动端必须不走便携分支。
    #[cfg(any(target_os = "android", target_os = "ios"))]
    #[test]
    fn mobile_is_never_portable() {
        assert!(!is_portable(), "移动端没有便携模式");
    }
}
