//! 常规文件操作：删除、重命名、新建文件夹。
//!
//! # 为什么单独一个模块
//!
//! 这些操作与加密无关——它们是文件管理器的基本功能，而这个应用首先是个
//! 文件管理器。放进 `encrypt.rs` 会让那个模块同时负责「加密」和「管理」
//! 两件事；放进 `browse.rs` 也不合适，那个模块的定位是**只读**浏览。
//!
//! # 对加密内容没有特殊照顾
//!
//! 删掉一个 `.omy` 文件与删掉一张 jpg 走同一条路径。树形加密目录下的
//! 增删改之所以"能用"，正是因为这里不做区分——每个密文文件都是磁盘上
//! 一个独立文件，删它不需要密码，也不需要通知任何索引。
//!
//! 唯一的例外是重命名：改密文**目录**的名字会让它的名字再也解不开
//! （名字本身就是密文），所以那种情况要拦住。

use crate::commands::{CmdError, CmdResult, Shared};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;

/// 删除请求。
#[derive(Debug, Clone, Deserialize)]
pub struct DeleteRequest {
    /// 要删的路径。
    pub paths: Vec<String>,
    /// `true` 进系统回收站，`false` 永久删除。
    ///
    /// 默认进回收站：这是文件管理器的通用预期，而永久删除不可逆。
    #[serde(default = "default_true")]
    pub to_trash: bool,
}

fn default_true() -> bool {
    true
}

/// 删除结果。
#[derive(Debug, Clone, Serialize)]
pub struct DeleteOutcome {
    /// 成功删掉的路径。
    pub deleted: Vec<String>,
    /// 失败的：`(路径, 错误码)`。
    ///
    /// 单个失败不该让整批回滚——用户选了 20 个文件，其中一个正被别的程序
    /// 占用，没有理由让另外 19 个也留着。
    pub failed: Vec<(String, String)>,
}

/// 删除文件或目录。
///
/// # Errors
///
/// - `empty_selection`：没给任何路径
#[tauri::command]
pub async fn delete_paths(
    state: State<'_, Shared>,
    req: DeleteRequest,
) -> CmdResult<DeleteOutcome> {
    if req.paths.is_empty() {
        return Err(CmdError::code("empty_selection"));
    }
    let handle: Shared = std::sync::Arc::clone(&state);

    // 删大目录是阻塞 IO，回收站还要走系统 API
    tauri::async_runtime::spawn_blocking(move || {
        let mut deleted = Vec::new();
        let mut failed = Vec::new();
        for raw in &req.paths {
            let p = PathBuf::from(raw);
            if !p.exists() {
                // 已经不在了：报出来而不是当成成功。用户可能选中了一批，
                // 中间有一个被别的程序删了，静默通过会让他以为自己删的
                failed.push((raw.clone(), String::from("not_found")));
                continue;
            }
            let r = if req.to_trash {
                crate::encrypt::move_to_trash(&p).map_err(String::from)
            } else {
                crate::encrypt::delete_permanently(&p)
                    .map_err(|_| String::from("delete_failed"))
            };
            match r {
                Ok(()) => deleted.push(raw.clone()),
                Err(code) => failed.push((raw.clone(), code)),
            }
        }
        // 删掉的文件可能在 token 表里，留着会让「在文件管理器中显示」
        // 指向一个不存在的路径
        if !deleted.is_empty() {
            handle.plain.clear();
        }
        Ok(DeleteOutcome { deleted, failed })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 重命名请求。
#[derive(Debug, Clone, Deserialize)]
pub struct RenameRequest {
    /// 原路径。
    pub path: String,
    /// 新名字（只是文件名，不含目录）。
    pub name: String,
}

/// 重命名文件或目录。
///
/// # Errors
///
/// - `empty_name`：新名字为空或只有空白
/// - `invalid_name`：名字里有路径分隔符或非法字符
/// - `not_found`：原路径不存在
/// - `already_exists`：目标名已被占用
/// - `encrypted_dir_rename`：这是密文目录，改名会让名字再也解不开
/// - `io_error`：改名失败
#[tauri::command]
pub async fn rename_path(state: State<'_, Shared>, req: RenameRequest) -> CmdResult<String> {
    let src = PathBuf::from(&req.path);
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return Err(CmdError::code("empty_name"));
    }
    if !is_valid_filename(&name) {
        return Err(CmdError::code("invalid_name"));
    }
    if !src.exists() {
        return Err(CmdError::code("not_found"));
    }

    // 密文目录的名字**就是**密文本身，改掉它等于把那段密文扔了，
    // 里面的文件全都还在但目录名永远解不开。必须拦住而不是让用户
    // 事后才发现——那时已经没有任何办法恢复原名
    if src.is_dir() {
        let disk = src
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if omy_core::dirname::looks_encrypted(&disk) {
            return Err(CmdError::code("encrypted_dir_rename"));
        }
    }

    let parent = src
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| CmdError::code("io_error"))?;
    let dst = parent.join(&name);

    // 目标已存在就拒绝，不覆盖。
    //
    // 大小写不敏感的文件系统上要放过「只改大小写」这种改名：
    // Windows 上 `readme.txt` → `README.txt` 时 dst.exists() 为真，
    // 一律拒绝会让这个正常操作做不到
    if dst.exists() && !same_path(&src, &dst) {
        return Err(CmdError::code("already_exists"));
    }

    std::fs::rename(&src, &dst).map_err(|_| CmdError::code("io_error"))?;
    // 路径变了，token 表里的旧映射作废
    state.plain.clear();
    Ok(dst.to_string_lossy().into_owned())
}

/// 新建文件夹请求。
#[derive(Debug, Clone, Deserialize)]
pub struct NewFolderRequest {
    /// 在哪个目录里建。
    pub parent: String,
    /// 文件夹名。
    pub name: String,
}

/// 新建文件夹。
///
/// # Errors
///
/// - `empty_name` / `invalid_name`：名字不合法
/// - `not_a_directory`：父目录不存在或不是目录
/// - `already_exists`：同名已存在
/// - `io_error`：创建失败
#[tauri::command]
pub async fn create_folder(req: NewFolderRequest) -> CmdResult<String> {
    let parent = PathBuf::from(&req.parent);
    let name = req.name.trim().to_owned();
    if name.is_empty() {
        return Err(CmdError::code("empty_name"));
    }
    if !is_valid_filename(&name) {
        return Err(CmdError::code("invalid_name"));
    }
    if !parent.is_dir() {
        return Err(CmdError::code("not_a_directory"));
    }
    let dst = parent.join(&name);
    if dst.exists() {
        return Err(CmdError::code("already_exists"));
    }
    // create_dir 而不是 create_dir_all：名字已经校验过不含分隔符，
    // 所以这里只该建一层。用 all 会让「误传了带斜杠的名字」静默建出
    // 一串嵌套目录
    std::fs::create_dir(&dst).map_err(|_| CmdError::code("io_error"))?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 判断两个路径是否指向同一个东西（用于放过只改大小写的重命名）。
fn same_path(a: &Path, b: &Path) -> bool {
    // canonicalize 会解析符号链接并规范大小写，是唯一可靠的判断方式。
    // 失败时退回字符串比较：至少不会把正常改名误判成冲突
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// 校验用户输入的文件名。
///
/// 只允许「一个名字」，不允许任何路径成分——否则用户在重命名框里输
/// `../../evil` 就能把文件写到任意位置。这与 `unpack::sanitize_filename`
/// 的目标不同：那个是**净化**不可信输入（来自密文内部，必须容错地清洗），
/// 这个是**拒绝**用户的非法输入（应当明确报错让他改）。
fn is_valid_filename(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    // `.` 与 `..` 是目录自身与父目录，不是合法文件名
    if name == "." || name == ".." {
        return false;
    }
    // 路径分隔符一律拒绝，两种都要查：Windows 认 `\`，而 `/` 在
    // Windows API 里同样被当作分隔符
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    // Windows 保留字符。即使在 Unix 上也一并拒绝：跨平台同步时
    // 这些名字会在 Windows 端变成打不开的文件
    if name.contains([':', '*', '?', '"', '<', '>', '|']) {
        return false;
    }
    // 控制字符
    if name.chars().any(|c| c.is_control()) {
        return false;
    }
    // Windows 上以点或空格结尾的名字会被系统静默截断，
    // 于是「改名成功了但名字不是我输的那个」
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    // Windows 设备名。带扩展名也一样保留（`CON.txt` 同样不可用）
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_rejects_path_traversal() {
        // 重命名框里输 `../../evil` 就能把文件挪到任意位置。
        // 不拦的话这是一个任意路径写入
        assert!(!is_valid_filename("../evil"));
        assert!(!is_valid_filename("..\\evil"));
        assert!(!is_valid_filename("a/b"));
        assert!(!is_valid_filename("a\\b"));
        assert!(!is_valid_filename(".."));
        assert!(!is_valid_filename("."));
        // 正常名字要放过，包括中文和空格
        assert!(is_valid_filename("报告.md"));
        assert!(is_valid_filename("my file.txt"));
        assert!(is_valid_filename("..hidden"), "两个点开头但不是 .. 本身");
    }

    #[test]
    fn filename_rejects_windows_traps() {
        // 这些在 Windows 上会被系统静默处理掉，导致「改名成功但名字不对」
        assert!(!is_valid_filename("name."), "以点结尾会被截断");
        assert!(!is_valid_filename("name "), "以空格结尾会被截断");
        assert!(!is_valid_filename("CON"), "设备名");
        assert!(!is_valid_filename("con.txt"), "带扩展名的设备名同样不可用");
        assert!(!is_valid_filename("LPT1"));
        assert!(!is_valid_filename("a:b"), "冒号");
        assert!(!is_valid_filename("a?b"));
        assert!(!is_valid_filename("a\u{0}b"), "控制字符");
        // 不该误伤：包含设备名的正常文件名
        assert!(is_valid_filename("CONTRACT.pdf"), "CON 只是前缀不算设备名");
        assert!(is_valid_filename("console.log"));
    }

    #[test]
    fn filename_rejects_empty_and_overlong() {
        assert!(!is_valid_filename(""));
        assert!(!is_valid_filename("   "), "全空白 trim 后为空");
        let long: String = core::iter::repeat_n('a', 256).collect();
        assert!(!is_valid_filename(&long), "超过 255 字节");
        let ok: String = core::iter::repeat_n('a', 255).collect();
        assert!(is_valid_filename(&ok), "正好 255 应当放过");
    }

    #[test]
    fn same_path_allows_case_only_rename() {
        // Windows 与 APFS 默认大小写不敏感，`readme.txt` → `README.txt`
        // 时目标"已存在"。一律拒绝会让这个正常操作做不到
        let root = std::env::temp_dir().join("omy-fileops-case");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("readme.txt");
        std::fs::write(&a, b"x").unwrap();
        let b = root.join("README.txt");

        if b.exists() {
            // 大小写不敏感的文件系统：必须判定为同一个路径
            assert!(
                same_path(&a, &b),
                "大小写不敏感时必须认出这是同一个文件，否则改大小写会被误判成冲突"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
