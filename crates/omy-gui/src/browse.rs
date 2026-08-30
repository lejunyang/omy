//! 文件浏览：不需要密码就能用的那一半。
//!
//! # 为什么要有这个模块
//!
//! 原来的 GUI 只有 `scan_directory`，它依赖会话密钥，只列 `.omy` 文件。
//! 结果是**第一次打开应用必须先输密码**——可新用户还没有密码，
//! 他要做的第一件事恰恰是「挑几个文件加密」。
//!
//! 所以这里提供一条不需要密钥的路径：像普通文件管理器那样列目录，
//! 顺带标出哪些是本应用的加密文件。密码只在真正要看加密内容时才问。
//!
//! # 与 `scan_directory` 的分工
//!
//! | | `browse_directory`（本模块） | `scan_directory` |
//! |---|---|---|
//! | 需要密钥 | 否 | 是 |
//! | 列出的东西 | 目录 + 所有文件 | 只有能识别的 `.omy` |
//! | 递归 | 否，一次一层 | 是 |
//! | 用途 | 日常浏览、挑文件加密 | 「把整个库都找出来」 |
//!
//! 两者并存，不是替代关系。

use crate::commands::{CmdError, CmdResult, Shared};
use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

/// 目录里的一项。
///
/// 与 [`crate::state::FileEntry`] 分开是有意的：那个描述「一个已识别的
/// 加密文件」，字段全都围绕解密后的元信息；这个描述「磁盘上的一个东西」，
/// 可能是目录、普通文件或加密文件。硬塞进一个结构会让两边都是一堆
/// `Option` 且语义含混。
#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    /// 完整路径。
    pub path: String,
    /// 显示名（文件名部分）。
    pub name: String,
    /// 是否是目录。
    pub is_dir: bool,
    /// 字节数；目录为 `None`。
    pub size: Option<u64>,
    /// 是否是本应用的加密文件（读文件头判断，不看后缀）。
    pub is_encrypted: bool,
    /// 加密文件是否已被当前会话解锁。
    ///
    /// `is_encrypted && !unlocked` 就是「需要密码」的状态，
    /// 前端据此显示锁图案。
    pub unlocked: bool,
    /// 解锁后的真实文件名。锁定或非加密文件为 `None`。
    pub real_name: Option<String>,
    /// 已解锁加密文件对应的 `FileEntry` id，用于预览。
    pub entry_id: Option<String>,
    /// 扩展名（小写，不含点），用于选图标。
    pub ext: Option<String>,
}

/// 浏览一个目录。
///
/// 不递归：文件管理器就该一次一层，递归会让大目录卡住而且没人想看
/// 一万个文件铺平在一起。
///
/// # Errors
///
/// - `not_a_directory`：路径不是目录
/// - `read_failed`：没有权限或路径消失
#[tauri::command]
pub async fn browse_directory(state: State<'_, Shared>, dir: String) -> CmdResult<Vec<DirEntry>> {
    let root = PathBuf::from(&dir);
    if !root.is_dir() {
        return Err(CmdError::code("not_a_directory"));
    }

    let handle: Shared = std::sync::Arc::clone(&state);

    // 读目录 + 探测文件头都是阻塞 IO，不能占着异步执行器
    tauri::async_runtime::spawn_blocking(move || list_dir(&root, &handle))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

/// 实际的目录读取。
fn list_dir(root: &Path, state: &Shared) -> CmdResult<Vec<DirEntry>> {
    let rd = std::fs::read_dir(root).map_err(|_| CmdError::code("read_failed"))?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for item in rd.flatten() {
        let path = item.path();
        let Ok(md) = item.metadata() else {
            // 取不到元数据的项直接跳过：可能是权限不足或符号链接指向了
            // 不存在的目标。为一个列不出来的项报错会让整个目录打不开
            continue;
        };

        let name = item.file_name().to_string_lossy().into_owned();

        if md.is_dir() {
            dirs.push(DirEntry {
                path: path.to_string_lossy().into_owned(),
                name,
                is_dir: true,
                size: None,
                is_encrypted: false,
                unlocked: false,
                real_name: None,
                entry_id: None,
                ext: None,
            });
            continue;
        }

        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase());

        // 判断是不是加密文件：读文件头，不看后缀。
        // 用户可能把 .omy 改名成 .jpg 做伪装，也可能有别的 .omy 文件
        // 其实不是我们的格式
        let encrypted = probe_encrypted(&path);

        files.push(DirEntry {
            path: path.to_string_lossy().into_owned(),
            name,
            is_dir: false,
            size: Some(md.len()),
            is_encrypted: encrypted,
            unlocked: false,
            real_name: None,
            entry_id: None,
            ext,
        });
    }

    // 目录在前，各自按名称排序。这是文件管理器的通用约定
    dirs.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    files.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    dirs.append(&mut files);

    // 有会话时顺带标出哪些加密文件已解锁
    annotate_unlocked(&mut dirs, state);

    Ok(dirs)
}

/// 读文件头判断是不是本应用的加密文件。
///
/// 只读前 64 字节：magic 在最前面，没必要为此把大文件读进来。
fn probe_encrypted(p: &Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = [0u8; 64];
    let Ok(n) = f.read(&mut buf) else {
        return false;
    };
    omy_core::file::is_omy_file(buf.get(..n).unwrap_or(&[]))
}

/// 给已解锁的加密文件补上真实文件名与 entry id。
fn annotate_unlocked(entries: &mut [DirEntry], state: &Shared) {
    if !state.is_unlocked() {
        return;
    }
    // 已扫描出来的文件按路径索引，避免对每个条目再解一次
    let known: std::collections::HashMap<String, crate::state::FileEntry> = state
        .files()
        .into_iter()
        .map(|e| (e.path.clone(), e))
        .collect();

    for e in entries.iter_mut().filter(|e| e.is_encrypted) {
        if let Some(f) = known.get(&e.path)
            && f.unlocked
        {
            e.unlocked = true;
            e.real_name = Some(f.name.clone());
            e.entry_id = Some(f.id.clone());
        }
    }
}

/// 文件名的自然序比较：数字按数值而不是字典序。
///
/// `第2章` 要排在 `第10章` 前面。字典序会把 `10` 排在 `2` 前，
/// 在文件列表里非常刺眼。
///
/// 前端也有一份 `Intl.Collator`，但那只作用于**已解锁**的显示名；
/// 这里排的是磁盘文件名，两者都需要。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.char_indices().peekable();
    let mut bi = b.char_indices().peekable();

    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some((apos, ac)), Some((bpos, bc))) => {
                if ac.is_ascii_digit() && bc.is_ascii_digit() {
                    let (an, alen) = take_number(a, apos);
                    let (bn, blen) = take_number(b, bpos);
                    if an != bn {
                        return an.cmp(&bn);
                    }
                    for _ in 0..alen {
                        ai.next();
                    }
                    for _ in 0..blen {
                        bi.next();
                    }
                } else {
                    let al = ac.to_lowercase().next().unwrap_or(ac);
                    let bl = bc.to_lowercase().next().unwrap_or(bc);
                    if al != bl {
                        return al.cmp(&bl);
                    }
                    ai.next();
                    bi.next();
                }
            }
        }
    }
}

/// 从 `pos` 起读一串数字，返回数值与消耗的字符数。
///
/// 超长数字串（比如 40 位）会溢出 u64，此时退回按长度比较——
/// 位数多的更大。这比 panic 或截断都合理。
fn take_number(s: &str, pos: usize) -> (u128, usize) {
    let mut v: u128 = 0;
    let mut n = 0usize;
    let mut overflow = false;
    for c in s.get(pos..).unwrap_or("").chars() {
        if !c.is_ascii_digit() {
            break;
        }
        n += 1;
        if !overflow {
            match v
                .checked_mul(10)
                .and_then(|x| x.checked_add(u128::from(c as u8 - b'0')))
            {
                Some(nv) => v = nv,
                None => overflow = true,
            }
        }
    }
    if overflow {
        // 溢出时用位数当权重，保证「更长的数更大」
        (u128::MAX, n)
    } else {
        (v, n)
    }
}

/// 列出可用的磁盘根（Windows 的盘符 / Unix 的 `/`）与常用目录。
///
/// 侧栏需要一个「从哪开始浏览」的入口。没有这个，用户每次都得
/// 点「选择文件夹」走原生对话框，很笨重。
#[tauri::command]
pub fn list_places() -> Vec<DirEntry> {
    let mut out = Vec::new();

    // 常用目录优先——比盘符更常用
    for (label, dir) in [
        ("home", dirs::home_dir()),
        ("desktop", dirs::desktop_dir()),
        ("documents", dirs::document_dir()),
        ("downloads", dirs::download_dir()),
        ("pictures", dirs::picture_dir()),
        ("videos", dirs::video_dir()),
    ] {
        if let Some(p) = dir
            && p.is_dir()
        {
            out.push(DirEntry {
                path: p.to_string_lossy().into_owned(),
                // name 用固定标签而不是目录名：前端据此查翻译，
                // 这样中文系统显示「下载」而不是「Downloads」
                name: String::from(label),
                is_dir: true,
                size: None,
                is_encrypted: false,
                unlocked: false,
                real_name: None,
                entry_id: None,
                ext: None,
            });
        }
    }

    out.extend(drive_roots());
    out
}

/// 磁盘根。
#[cfg(windows)]
fn drive_roots() -> Vec<DirEntry> {
    // 逐个试 A: 到 Z:。Windows 没有便捷的枚举 API 而不引入 winapi，
    // 26 次 is_dir 的开销可以忽略
    (b'A'..=b'Z')
        .filter_map(|c| {
            let p = format!("{}:\\", c as char);
            let path = PathBuf::from(&p);
            path.is_dir().then(|| DirEntry {
                path: p.clone(),
                name: p.clone(),
                is_dir: true,
                size: None,
                is_encrypted: false,
                unlocked: false,
                real_name: None,
                entry_id: None,
                ext: None,
            })
        })
        .collect()
}

/// 磁盘根。
#[cfg(not(windows))]
fn drive_roots() -> Vec<DirEntry> {
    vec![DirEntry {
        path: String::from("/"),
        name: String::from("/"),
        is_dir: true,
        size: None,
        is_encrypted: false,
        unlocked: false,
        real_name: None,
        entry_id: None,
        ext: None,
    }]
}

/// 某个路径的父目录。用于「上一级」按钮。
///
/// 已经在根时返回 `None`，前端据此禁用按钮。
#[tauri::command]
pub fn parent_of(path: String) -> Option<String> {
    Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_puts_2_before_10() {
        // 字典序会把「第10章」排在「第2章」前面，这在文件列表里很刺眼
        let mut v = vec![
            String::from("第10章.txt"),
            String::from("第2章.txt"),
            String::from("第1章.txt"),
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["第1章.txt", "第2章.txt", "第10章.txt"]);
    }

    #[test]
    fn natural_order_is_case_insensitive() {
        let mut v = vec![String::from("Beta"), String::from("alpha")];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["alpha", "Beta"]);
    }

    #[test]
    fn natural_order_handles_huge_numbers() {
        // 40 位数字会溢出 u64，不能 panic 也不能截断成错误的顺序
        let a = format!("f{}.txt", "9".repeat(40));
        let b = String::from("f1.txt");
        assert_eq!(natural_cmp(&b, &a), std::cmp::Ordering::Less);
    }

    #[test]
    fn take_number_reports_overflow_as_max() {
        let s = "9".repeat(50);
        let (v, n) = take_number(&s, 0);
        assert_eq!(v, u128::MAX, "溢出时应返回最大值而不是截断");
        assert_eq!(n, 50);
    }

    #[test]
    fn parent_of_root_is_none() {
        // 根目录没有父级，前端据此禁用「上一级」
        #[cfg(windows)]
        let root = "C:\\";
        #[cfg(not(windows))]
        let root = "/";
        assert!(parent_of(String::from(root)).is_none());
    }

    #[test]
    fn probe_rejects_non_omy() {
        // 普通文件不能被认成加密文件
        let dir = std::env::temp_dir().join("omy-browse-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("plain.txt");
        let _ = std::fs::write(&f, b"hello world");
        assert!(!probe_encrypted(&f));
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn probe_rejects_missing_file() {
        assert!(!probe_encrypted(Path::new("/definitely/not/here.omy")));
    }
}
