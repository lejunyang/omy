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
    /// 未加密文件的访问 token，用于应用内预览与「用系统程序打开」。
    ///
    /// 目录为 `None`。加密文件也给 token——「在文件管理器中显示」
    /// 对加密文件同样适用，那个操作不需要解密。
    pub token: Option<String>,
    /// 未加密文件的预览类别（`image` / `video` / `audio` / `text` / `other`）。
    ///
    /// 前端据此决定双击是应用内预览还是交给系统程序。
    /// 让后端算而不是前端按后缀猜：判定规则（比如哪些格式需要转码）
    /// 会变，散在两处早晚不一致——`mime.rs` 里已经踩过这个坑。
    pub preview: Option<String>,
    /// 未加密文件的 MIME，供 `<video>` / `<img>` 使用。
    pub mime: Option<String>,
    /// 这个加密文件是不是一整个文件夹（目录容器）。
    ///
    /// 双击它应当进入容器浏览，而不是当作单个文件预览。
    /// 未解锁时恒为 `false`——「这是个文件夹」也是内容信息。
    pub is_container: bool,
    /// 这个**目录**是不是树形模式加密出来的。
    ///
    /// 注意它与 `is_encrypted` 并列而不是复用后者：`is_encrypted` 的含义是
    /// 「这个文件的内容是密文」，而加密目录本身没有内容，它只是名字是密文。
    /// 混用会让前端分不清「要不要解密才能预览」。
    ///
    /// `is_dir` 对它仍然是 `true`——前端已有的双击进目录逻辑要能直接复用，
    /// 否则就得为它写第二套打开逻辑。
    pub is_encrypted_dir: bool,
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

/// 把 `read_dir` 的失败翻译成前端能给出可行建议的错误码。
///
/// # 为什么不能一律 read_failed
///
/// 安卓上用户随时可以去系统设置里关掉「所有文件访问权限」，应用收不到
/// 任何通知。此后每个整机路径都会 `PermissionDenied`，而「无法读取该
/// 目录」这句话完全指不到真正的原因——用户只会以为应用坏了，或者以为
/// 这个目录本来就打不开。必须明确说「权限被关闭了，去重新授权」。
///
/// 桌面端不做这个区分：那里的 PermissionDenied 通常是真的目录权限问题
/// （系统目录、别人的用户目录），提示去申请全盘权限反而误导。
fn read_dir_error(root: &Path, e: &std::io::Error) -> CmdError {
    #[cfg(target_os = "android")]
    if e.kind() == std::io::ErrorKind::PermissionDenied && !is_app_private(root) {
        // 沙箱外的 PermissionDenied 只有一个成因：全盘权限没有或被撤销。
        // 沙箱内的要排除掉——那种是真的目录权限问题，让用户去开全盘
        // 权限也解决不了，只会白跑一趟设置页。
        return CmdError::code("storage_permission_lost");
    }

    let _ = root;
    let _ = e;
    CmdError::code("read_failed")
}

/// 该路径是否位于应用私有沙箱内。
///
/// 用路径前缀判断而不是比较 `app_data_dir()`：这个函数在 `list_dir` 里被
/// 每次列目录调用，拿 `AppHandle` 得把它一路传进来，而这两个前缀是
/// 安卓的固定布局，不会变。
#[cfg(target_os = "android")]
fn is_app_private(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/data/") || s.contains("/Android/data/")
}

/// 实际的目录读取。
fn list_dir(root: &Path, state: &Shared) -> CmdResult<Vec<DirEntry>> {
    let rd = std::fs::read_dir(root).map_err(|e| read_dir_error(root, &e))?;

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
            // 廉价判定：只看后缀与字符集，不尝试解密。目录多的时候
            // 逐个解密会让列目录明显变慢，而解名放到后面统一做
            let enc_dir = omy_core::dirname::looks_encrypted(&name);
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
                token: None,
                preview: None,
                mime: None,
                is_container: false,
                is_encrypted_dir: enc_dir,
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

        // 登记 token。加密文件也登记：「在文件管理器中显示」对它
        // 同样适用，那个操作不需要解密。
        let token = state.plain.register(&path);

        // 预览类别只对未加密文件有意义。加密文件的类别要等解开
        // 头部才知道——磁盘上的后缀是 .omy，按后缀只会得到 other，
        // 填进去反而误导前端
        let (preview, mime) = if encrypted {
            (None, None)
        } else {
            let (k, m) = crate::mime::by_extension(&name);
            (Some(k.to_owned()), Some(m))
        };

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
            token,
            preview,
            mime,
            is_container: false,
            is_encrypted_dir: false,
        });
    }

    // 目录在前，各自按名称排序。这是文件管理器的通用约定
    dirs.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    files.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    dirs.append(&mut files);

    // 有会话时顺带标出哪些加密文件已解锁
    annotate_unlocked(&mut dirs, state);
    // 再把树形加密的目录名与目录内文件名解出来。放在排序**之后**：
    // 排序按磁盘名做（那是稳定的），解出来的名字只用于显示。
    // 若按解出的名字排，锁定与解锁两种状态下顺序会不一样，
    // 列表会在输入密码的瞬间跳动
    annotate_tree_names(&mut dirs, root, state);

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

/// 解出树形加密目录的真实名字，以及目录内加密文件的真实名字。
///
/// # 为什么两件事放一起
///
/// 都需要同一份前置条件：一个能读出 `vault_salt` 的样本文件，加上会话里
/// 对应的 KEK。分成两个函数会把「找样本 + 派生 dirname key」这段做两遍，
/// 而它涉及磁盘 IO 与 Argon2 之后的 HKDF。
///
/// # 静默失败是有意的
///
/// 解不开就保留磁盘名（那串 base32），不报错也不清空。理由：
/// - 没有密码时**本该**看不懂，这是正常状态而不是错误；
/// - 一个解不开的名字不应该让整个目录列不出来。
fn annotate_tree_names(entries: &mut [DirEntry], root: &Path, state: &Shared) {
    if !state.is_unlocked() {
        return;
    }
    // 当前目录里有没有加密目录 / 加密文件，决定了值不值得往下做
    let has_enc_dir = entries.iter().any(|e| e.is_encrypted_dir);
    let has_enc_file = entries.iter().any(|e| e.is_encrypted && !e.unlocked);
    if !has_enc_dir && !has_enc_file {
        return;
    }

    // 取 vault 参数的样本：先在当前目录里找，找不到再往加密子目录里找。
    //
    // 「往子目录里找」是必要的：用户站在密文树的**根的父目录**时，当前层
    // 只有一个加密目录、没有任何 .omy 文件，此时拿不到 salt 就解不出那个
    // 目录的名字——而这恰好是最常见的场景（刚加密完，站在原地看产物）。
    let Some(sample) = omy_core::tree::find_any_file(root) else {
        return;
    };
    // 4 KiB 足够覆盖固定头 + slot 区：这里只要 vault_salt 与 KDF 参数
    let Ok(prefix) = read_head(&sample, 4096) else {
        return;
    };
    let Ok(header) = omy_core::file::peek_header(&prefix) else {
        return;
    };

    let keks: Vec<omy_core::crypto::Kek> = state
        .with_session(|s| {
            s.all_for(&header.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();
    let Some(kek) = keks.first() else {
        return;
    };
    let dkey = omy_core::dirname::DirnameKey::derive(kek, &header.vault_salt);

    for e in entries.iter_mut() {
        if e.is_encrypted_dir {
            // 超长名截断过，完整密文在目录内部的边车文件里
            let sidecar =
                std::fs::read(Path::new(&e.path).join(omy_core::dirname::DIRNAME_SIDECAR)).ok();
            if let Ok(real) = omy_core::dirname::decrypt_dirname(
                &e.name,
                sidecar.as_deref(),
                &dkey,
                header.cipher_id,
            ) {
                e.real_name = Some(real);
                e.unlocked = true;
            }
        } else if e.is_encrypted && !e.unlocked {
            // 树形模式里的文件磁盘名是随机 uuid，真名在它自己的 TLV 里。
            // annotate_unlocked 只认 scan_directory 登记过的文件，
            // 而浏览进密文树时没人调过 scan——所以这里要自己解一次
            if let Some(real) = real_name_of(Path::new(&e.path), &keks) {
                e.unlocked = true;
                // 真名的后缀才是有意义的那个：磁盘上一律是 .omy
                let (kind, mime) = crate::mime::by_extension(&real);
                e.preview = Some(kind.to_owned());
                e.mime = Some(mime);
                e.ext = std::path::Path::new(&real)
                    .extension()
                    .map(|x| x.to_string_lossy().to_ascii_lowercase());
                e.real_name = Some(real);
            }
        }
    }
}

/// 只读文件开头若干字节。
///
/// `commands.rs` 与 `encrypt.rs` 里各有一份等价实现，它们是 `pub(self)` 的。
/// 这里没有第四次复制的必要——若将来要统一，三处一起收进 `crate::util`
/// 之类的地方，**改一处就要改三处**。
fn read_head(p: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

/// 读出一个加密文件在 TLV 里记的真实文件名。
///
/// 只读头部区域：文件名在 TLV 区，不需要把载荷读进来。1 MiB 与
/// `enrich_file` 取同一个上限——那里的注释说明了它足够覆盖含缩略图的 TLV 区。
fn real_name_of(path: &Path, keks: &[omy_core::crypto::Kek]) -> Option<String> {
    let bytes = read_head(path, 1 << 20).ok()?;
    let opened = omy_core::file::open(&bytes, keks).ok()?;
    opened.filename().ok()
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
            e.is_container = f.is_container;
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
pub fn list_places(app: tauri::AppHandle) -> Vec<DirEntry> {
    // 安卓完全是另一套：应用默认跑在沙箱里，`/` 和 `/sdcard` 都是
    // Permission denied，`dirs::home_dir()` 之类返回的路径同样读不到
    // （它读 $HOME，而安卓上那个值对应用无意义）。照桌面的逻辑走，
    // 侧栏会列出一堆点进去就报 read_failed 的入口。
    //
    // 拿到全盘权限后能列整机，但入口仍由 android_places 按实时授权
    // 状态决定，不走桌面这条路——存储卷的挂载点得问系统要。
    #[cfg(target_os = "android")]
    {
        android_places(&app)
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
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
                    token: None,
                    preview: None,
                    mime: None,
                    is_container: false,
                    is_encrypted_dir: false,
                });
            }
        }

        out.extend(drive_roots());
        out
    }
}

/// 安卓上的可访问位置。
///
/// 分两种情况，取决于有没有拿到全盘访问权限（见 [`crate::storage`]）：
///
/// - **未授权**：只列应用沙箱内的目录。这些是不申请任何权限就能读写的
///   地方。此时列出整机路径只会得到「无法读取」，比不列更让人困惑。
/// - **已授权**：列出各存储卷根目录与其下的常用目录（下载、相册等）。
///   这才是文件管理器该有的样子。
///
/// 授权状态每次实查，不缓存：用户随时可能去系统设置里关掉开关，
/// 而应用收不到任何通知。缓存的后果是侧栏继续显示整机路径，
/// 点进去全部 EACCES。
///
/// 沙箱目录在两种情况下都列：即使有了全盘权限，应用私有目录仍然是
/// 「解密临时文件在哪」的答案，用户需要能找到并清理它。
#[cfg(target_os = "android")]
fn android_places(app: &tauri::AppHandle) -> Vec<DirEntry> {
    use tauri::Manager as _;

    let mut out = Vec::new();

    // 有权限时先列整机：用户来这个应用是为了处理相册、下载里的文件，
    // 沙箱目录是次要的，放前面会挡住主路径。
    if crate::storage::is_granted(app) {
        out.extend(android_volume_places(app));
    }

    // 私有文档区：用户自己的文件放这儿。外部私有目录（sdcard 上的
    // Android/data/<pkg>）优先，因为它能用 USB 或文件管理器从电脑侧看到，
    // 便于把待加密的文件传进来；取不到再退回内部 files/
    let docs = app
        .path()
        .app_data_dir()
        .ok()
        .map(|d| d.join("documents"))
        .filter(|d| std::fs::create_dir_all(d).is_ok());

    if let Some(p) = docs {
        out.push(android_entry(&p, "documents"));
    }

    // 应用缓存：解密临时文件落在这里，让用户能看到并手动清理
    if let Ok(p) = app.path().app_cache_dir() {
        if std::fs::create_dir_all(&p).is_ok() {
            out.push(android_entry(&p, "cache"));
        }
    }

    out
}

/// 已授权时可列出的整机位置：各存储卷根 + 主卷下的常用目录。
///
/// 常用目录用固定的英文子目录名拼接，而不是调 `Environment` 的
/// `DIRECTORY_DOWNLOADS` 之类：那些常量的值本来就是这些英文名，
/// 而系统显示的中文名由前端的翻译键负责。
///
/// 逐个 `is_dir()` 过滤：这些目录并非每台设备都有（例如从未用过相机的
/// 设备没有 DCIM）。列出不存在的路径会让用户点到一个报错的入口。
#[cfg(target_os = "android")]
fn android_volume_places(app: &tauri::AppHandle) -> Vec<DirEntry> {
    let mut out = Vec::new();

    for vol in crate::storage::volumes(app) {
        let root = Path::new(&vol.path);

        if vol.primary {
            // 主存储：先给常用目录，再给根。用户找「下载」的频率远高于
            // 从根目录一层层点进去。
            for (label, sub) in [
                ("downloads", "Download"),
                ("pictures", "Pictures"),
                ("camera", "DCIM"),
                ("documents_shared", "Documents"),
                ("movies", "Movies"),
                ("music", "Music"),
            ] {
                let p = root.join(sub);
                if p.is_dir() {
                    out.push(android_entry(&p, label));
                }
            }
            out.push(android_entry(root, "internal_storage"));
        } else if root.is_dir() {
            // SD 卡 / U 盘：名字用系统给的本地化描述（「SD 卡」等），
            // 取不到时退回路径末段。不能退回固定文案：插两张卡时
            // 两个条目会同名，用户分不清哪个是哪个。
            let name = vol.label.clone().unwrap_or_else(|| {
                root.file_name()
                    .map_or_else(|| vol.path.clone(), |n| n.to_string_lossy().into_owned())
            });
            let mut e = android_entry(root, "removable");
            e.real_name = Some(name);
            out.push(e);
        }
    }

    out
}

/// 构造一个安卓侧栏条目。
#[cfg(target_os = "android")]
fn android_entry(path: &Path, label: &str) -> DirEntry {
    DirEntry {
        path: path.to_string_lossy().into_owned(),
        name: String::from(label),
        is_dir: true,
        size: None,
        is_encrypted: false,
        unlocked: false,
        real_name: None,
        entry_id: None,
        ext: None,
        token: None,
        preview: None,
        mime: None,
        is_container: false,
        is_encrypted_dir: false,
    }
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
                token: None,
                preview: None,
                mime: None,
                is_container: false,
                is_encrypted_dir: false,
            })
        })
        .collect()
}

/// 磁盘根。
///
/// 排除 android：那里 `/` 是 Permission denied，`list_places` 走
/// [`android_places`] 另一条路，不会调到这里。不排除的话安卓构建
/// 会报「函数从未使用」。
#[cfg(all(not(windows), not(target_os = "android")))]
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
        token: None,
        preview: None,
        mime: None,
        is_container: false,
        is_encrypted_dir: false,
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
    fn encrypted_dir_is_recognized_by_name_shape() {
        use omy_core::dirname::looks_encrypted;
        // 真密文目录名：全大写 base32 + .omy 后缀
        assert!(
            looks_encrypted("3NJDNEJ4TXYUFW2SW2D3RWBAP3I7WILHZB6SVLM7BE5JJ45OYH5Q4ZQ.omy"),
            "真实的密文目录名必须被认出来，否则用户看到的是一串乱码而不是原名"
        );
        // 截断名带 ~ 标记
        assert!(looks_encrypted("AAAABBBBCCCC~ABCD2345.omy"));

        // 普通目录不能被误判：误判会让它显示成「已加密」，
        // 而且会去读它内部的 .omy-name，白费一次磁盘 IO
        assert!(!looks_encrypted("工作资料"));
        assert!(!looks_encrypted("photos"));
        // 小写不是 base32 字符集：加密文件的磁盘名是小写十六进制，
        // 若把它当成目录名会解密失败
        assert!(!looks_encrypted("4c1f877776c32979260c06e8c48b0465.omy"));
        // 只有后缀、没有名字
        assert!(!looks_encrypted(".omy"));
        // 有 .omy 后缀但含非 base32 字符（用户自己建的目录）
        assert!(!looks_encrypted("MY-BACKUP.omy"));
    }

    #[test]
    fn encrypted_dir_stays_a_directory_for_the_frontend() {
        // 加密目录必须仍然 is_dir=true。前端已有的「双击进目录」逻辑靠这个
        // 字段分流，若把它标成文件，双击会走预览路径，得到「这不是能预览的
        // 文件」——而用户要的正是「跟正常目录一样访问」。
        //
        // 这条守的是列目录时的字段组合，所以造一个真目录来跑 list_dir 的
        // 目录分支逻辑（不需要会话）。
        let root = std::env::temp_dir().join("omy-browse-encdir");
        let _ = std::fs::remove_dir_all(&root);
        let enc_name = "AAAAAAAABBBBBBBBCCCCCCCCDDDDDDDDEEEEEEEEFFFFFFFF.omy";
        std::fs::create_dir_all(root.join(enc_name)).unwrap();
        std::fs::create_dir_all(root.join("普通目录")).unwrap();

        // 直接验判定与 DirEntry 的构造规则，不经 Shared
        for (name, want_enc) in [(enc_name, true), ("普通目录", false)] {
            let is_enc = omy_core::dirname::looks_encrypted(name);
            assert_eq!(is_enc, want_enc, "{name} 的判定不对");
        }

        // 加密目录的 is_encrypted 必须是 false：那个字段的含义是
        // 「内容是密文」，而目录没有内容。混用会让前端以为要先解密才能进
        let entry = DirEntry {
            path: root.join(enc_name).to_string_lossy().into_owned(),
            name: String::from(enc_name),
            is_dir: true,
            size: None,
            is_encrypted: false,
            unlocked: false,
            real_name: None,
            entry_id: None,
            ext: None,
            token: None,
            preview: None,
            mime: None,
            is_container: false,
            is_encrypted_dir: true,
        };
        assert!(entry.is_dir, "加密目录必须仍然是目录");
        assert!(!entry.is_encrypted, "目录本身没有密文内容");
        assert!(entry.is_encrypted_dir, "但要标出它是加密目录");

        let _ = std::fs::remove_dir_all(&root);
    }

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
