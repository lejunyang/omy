//! 未加密文件的预览与外部打开。
//!
//! # 为什么未加密文件也要走自定义协议
//!
//! 直觉上「文件就在磁盘上，让 WebView 直接读不就行了」。但那需要
//! 启用 Tauri 的 `protocol-asset`，而它会把**整个文件系统**暴露给
//! WebView 里的 JS——只要拼出 `asset://` URL 就能读任意文件。
//! 对一个加密工具来说这个攻击面不划算（见 `Cargo.toml` 里的说明）。
//!
//! 所以这里复用同一条 `omystream://` 协议，加一个 `/plain/` 前缀。
//! 代价是要自己做 Range，收益是**入口只有一个**，且能强制路径白名单。
//!
//! # 与 `/file/` 的区别
//!
//! | | `/file/<id>` | `/plain/<token>` |
//! |---|---|---|
//! | 内容 | 加密文件，按需解密 | 磁盘上的明文，直接读 |
//! | 寻址 | 会话内的 id | 路径的哈希 token |
//! | 需要密钥 | 是 | 否 |
//! | 泄露风险 | 明文只在内存 | 文件本来就是明文 |
//!
//! 未加密文件本来就以明文躺在磁盘上，读它不产生任何**新的**明文——
//! 这与文档 §3 L2 说的「临时解密文件」是两回事，那条针对的是
//! 把加密内容解出来写到磁盘。
//!
//! # 为什么用 token 而不直接传路径
//!
//! 若 URL 里直接放路径，WebView 里的任何脚本都能构造
//! `omystream://localhost/plain/C:\Users\x\.ssh\id_rsa` 来读任意文件。
//! token 是「路径 → 随机 id」的单向映射，且只有**被浏览过的目录里
//! 真实存在的文件**才会拿到 token。脚本猜不出 token，也就读不到
//! 没被列出来的文件。

use crate::commands::{CmdError, CmdResult, Shared};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 明文文件的访问登记表。
///
/// 只有经由 `browse_directory` 列出过的文件才会在这里注册，
/// 协议层据此拒绝一切未登记的路径。
#[derive(Default)]
pub struct PlainRegistry {
    inner: Mutex<HashMap<String, PathBuf>>,
}

impl PlainRegistry {
    /// 新建空登记表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个路径，返回访问 token。
    ///
    /// 同一路径重复注册会得到**同一个** token——否则每次刷新目录
    /// 都会产生新 token，旧的永远留在表里，浏览一天下来能堆几万条。
    pub fn register(&self, path: &Path) -> Option<String> {
        let key = token_for(path);
        let mut g = self.inner.lock().ok()?;
        g.entry(key.clone()).or_insert_with(|| path.to_path_buf());
        Some(key)
    }

    /// 按 token 取回路径。
    pub fn resolve(&self, token: &str) -> Option<PathBuf> {
        self.inner.lock().ok()?.get(token).cloned()
    }

    /// 清空登记表（锁定时调用）。
    ///
    /// 锁定的语义是「把这台机器上的痕迹收起来」。虽然明文文件本来
    /// 就在磁盘上、清不清都能被别人直接打开，但留着这张表意味着
    /// WebView 里还残留着一批可用 token，与「锁定后什么都看不到」
    /// 的预期不符。
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }

    /// 当前登记数量。
    ///
    /// 只有测试用得上——生产代码不该关心表里有多少条。
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// 是否为空。
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 由路径算出稳定 token。
///
/// 用 BLAKE3 而不是自增计数：同一路径在多次浏览之间得到同一个
/// token，前端可以安全地缓存 URL。用哈希也意味着 token 里
/// **不含路径信息**——即使泄露到日志里也不暴露用户的目录结构。
fn token_for(path: &Path) -> String {
    let h = omy_core::util::blake2b_256(path.to_string_lossy().as_bytes());
    // 取前 16 字节做十六进制，碰撞概率可以忽略
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// 用系统默认程序打开一个文件。
///
/// # 为什么不用 `tauri-plugin-opener`
///
/// 那个插件会把「打开任意路径」的能力暴露给前端 JS。这里自己实现，
/// 只在 Rust 侧执行，且**只接受登记过的路径**——前端拿不到任意
/// 路径的打开能力。
///
/// # 安全考虑
///
/// 路径必须来自登记表，不能直接用前端传来的字符串。否则一段恶意
/// 脚本可以让我们去「打开」任意可执行文件，等于任意代码执行。
///
/// # Errors
///
/// - `unknown_file`：token 没登记
/// - `open_failed`：系统调用失败（没有关联程序、文件已删除等）
#[tauri::command]
pub async fn open_external(state: tauri::State<'_, Shared>, token: String) -> CmdResult<()> {
    let Some(path) = state.plain.resolve(&token) else {
        return Err(CmdError::code("unknown_file"));
    };
    // 再确认一次文件还在：登记之后用户可能已经把它删了，
    // 此时报「打不开」比让系统弹一个陌生的错误框友好
    if !path.exists() {
        return Err(CmdError::code("unknown_file"));
    }
    launch(&path).map_err(|_| CmdError::code("open_failed"))
}

/// 在系统文件管理器中定位一个文件。
///
/// 与「打开」不同，这个不会启动关联程序，只是把资源管理器 /
/// Finder 打开到该文件所在位置并选中它。加密文件也适用——
/// 定位不需要解密。
///
/// # Errors
///
/// 同 [`open_external`]。
#[tauri::command]
pub async fn reveal_in_folder(state: tauri::State<'_, Shared>, token: String) -> CmdResult<()> {
    let Some(path) = state.plain.resolve(&token) else {
        return Err(CmdError::code("unknown_file"));
    };
    if !path.exists() {
        return Err(CmdError::code("unknown_file"));
    }
    reveal(&path).map_err(|_| CmdError::code("open_failed"))
}

/// 平台相关的「用默认程序打开」。
#[cfg(windows)]
fn launch(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // 用 explorer 而不是 `cmd /c start`：后者要处理引号转义，
    // 路径里有 & 或 ^ 时会被 cmd 解释成语法。explorer 直接收
    // 一个参数，不经过 shell 解析。
    //
    // CREATE_NO_WINDOW 防止闪一个黑框
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("explorer.exe")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// 平台相关的「用默认程序打开」。
#[cfg(target_os = "macos")]
fn launch(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open").arg(path).spawn().map(|_| ())
}

/// 平台相关的「用默认程序打开」。
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn launch(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(path).spawn().map(|_| ())
}

/// Android 上没有 xdg-open，交给系统应用要走 Intent。
///
/// 必须单独分一支：Android 也满足 `all(unix, not(macos))`，不排除
/// 的话会去 spawn 一个不存在的命令，得到一个含糊的 io error，
/// 而真正的原因是分支选错了。
#[cfg(target_os = "android")]
fn launch(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Android 需要通过 Intent 打开外部应用，尚未实现",
    ))
}

/// 平台相关的「在文件管理器中显示」。
#[cfg(windows)]
fn reveal(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // /select, 后面不能有空格，且路径要作为同一个参数传
    std::process::Command::new("explorer.exe")
        .arg("/select,")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

/// 平台相关的「在文件管理器中显示」。
#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()
        .map(|_| ())
}

/// 平台相关的「在文件管理器中显示」。
///
/// Linux 没有统一的「选中文件」接口，退而求其次打开父目录。
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn reveal(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(path);
    std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ())
}

/// Android 没有「文件管理器中显示」这个概念，也没有 xdg-open。
#[cfg(target_os = "android")]
fn reveal(_path: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Android 没有「在文件管理器中显示」",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_stable_for_same_path() {
        // 前端会缓存 URL，同一路径必须始终得到同一个 token
        let p = Path::new("/x/y/z.png");
        assert_eq!(token_for(p), token_for(p));
    }

    #[test]
    fn token_differs_for_different_paths() {
        assert_ne!(token_for(Path::new("/a.png")), token_for(Path::new("/b.png")));
    }

    #[test]
    fn token_does_not_leak_path() {
        // token 可能出现在日志或 DevTools 里，不能反推出用户的目录结构
        let t = token_for(Path::new("/home/alice/secret-project/plan.png"));
        assert!(!t.contains("alice"));
        assert!(!t.contains("secret"));
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn registry_roundtrip() {
        let r = PlainRegistry::new();
        let p = PathBuf::from("/tmp/a.png");
        let t = r.register(&p).unwrap_or_default();
        assert_eq!(r.resolve(&t), Some(p));
    }

    #[test]
    fn registry_rejects_unknown_token() {
        // 这条守护的是任意文件读取：未登记的 token 必须解析失败，
        // 否则 WebView 里的脚本可以构造 token 去读任意文件
        let r = PlainRegistry::new();
        assert_eq!(r.resolve("deadbeef"), None);
        assert_eq!(r.resolve(""), None);
    }

    #[test]
    fn re_registering_does_not_grow_table() {
        // 每次刷新目录都重新注册，若不去重，浏览一天能堆几万条
        let r = PlainRegistry::new();
        let p = PathBuf::from("/tmp/a.png");
        for _ in 0..100 {
            let _ = r.register(&p);
        }
        assert_eq!(r.len(), 1, "同一路径不应重复占用条目");
    }

    #[test]
    fn clear_empties_registry() {
        let r = PlainRegistry::new();
        let _ = r.register(Path::new("/tmp/a.png"));
        assert!(!r.is_empty());
        r.clear();
        assert!(r.is_empty(), "锁定后不应残留可用 token");
    }
}
