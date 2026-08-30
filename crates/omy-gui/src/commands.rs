//! Tauri 命令：前端唯一能调用后端的入口。
//!
//! # 错误返回的约定（文档 §7.2）
//!
//! 后端**不返回拼好的中文字符串**，而是返回错误码 + 参数，由前端翻译。
//! 理由很实际：用户可以把界面切成英文，但后端的 `SessionKeys` 不知道
//! 这件事；一旦后端拼了中文，英文界面里就会突然冒出中文错误。
//!
//! ```rust,ignore
//! // ❌ 后端拼字符串
//! Err("密码不正确".into())
//! // ✅ 结构化错误，前端翻译
//! Err(CmdError::code("wrong_password"))
//! ```
//!
//! # 为什么解锁要先选目录
//!
//! Argon2 的盐**和参数**都存在文件头里，不在应用配置中。
//! 这是格式设计的有意选择：换台机器、换个应用版本，只要有文件
//! 就能解开。代价是解锁流程必须「先有文件，才能派生密钥」——
//! 所以 UI 上「添加库」在「解锁」之前，而不是反过来。
//!
//! 曾经想过用固定参数派生再校验，但那等于把参数硬编码进应用，
//! 一旦用户用 `--argon2-m 1G` 加密过文件就再也打不开了。

use crate::state::{AppState, FileEntry};
use omy_core::scan::{ScanOptions, scan_dir};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::State;

/// 返回给前端的结构化错误。
///
/// `code` 是翻译键，`params` 供插值。前端用 `errors.<code>` 查文案。
#[derive(Debug, Clone, serde::Serialize)]
pub struct CmdError {
    /// 翻译键。
    pub code: String,
    /// 插值参数。
    pub params: serde_json::Value,
}

impl CmdError {
    /// 只有错误码的错误。
    #[must_use]
    pub fn code(c: &str) -> Self {
        Self {
            code: c.to_owned(),
            params: serde_json::Value::Null,
        }
    }

    /// 带参数的错误。
    #[must_use]
    pub fn with(c: &str, params: serde_json::Value) -> Self {
        Self {
            code: c.to_owned(),
            params,
        }
    }
}

/// 命令结果。
pub(crate) type CmdResult<T> = Result<T, CmdError>;

/// 共享状态句柄。
pub type Shared = Arc<AppState>;

/// 库的解锁参数，从任意一个 `.omy` 文件的头部读出。
///
/// 前端拿到后在解锁时原样回传——它不含秘密，
/// 盐和 Argon2 参数本来就是明文存在文件头里的。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VaultParams {
    /// 十六进制的 16 字节盐。
    pub salt: String,
    /// Argon2 内存开销（KiB）。
    pub m_kib: u32,
    /// Argon2 迭代次数。
    pub t: u32,
    /// Argon2 并行度。
    pub p: u32,
}

/// 解锁结果，供状态栏显示。
#[derive(Debug, serde::Serialize)]
pub struct UnlockResult {
    /// 当前已解锁的凭据总数。
    pub credentials: usize,
    /// 本次成功派生的 vault 数量。
    pub vaults_unlocked: usize,
}

/// 用密码解锁一个或多个 vault。
///
/// Argon2 派生要几百毫秒，必须放在阻塞线程里，否则 UI 会卡住。
/// Tauri 的 `async` 命令跑在 async 运行时上，直接做 CPU 密集工作
/// 会占死执行器线程——所以用 `spawn_blocking`。
///
/// 对每个 vault 都派生一次。一个都没成功才算失败：部分成功是
/// 正常情况（目录里可能混着别人的、用其他密码加密的文件）。
#[tauri::command]
pub async fn unlock(
    state: State<'_, Shared>,
    label: String,
    password: String,
    vaults: Vec<VaultParams>,
) -> CmdResult<UnlockResult> {
    if password.is_empty() {
        return Err(CmdError::code("empty_password"));
    }
    if vaults.is_empty() {
        return Err(CmdError::code("no_vault_selected"));
    }

    // 先把参数解析完再进线程：解析失败要立刻报错，
    // 而不是跑完几百毫秒的 Argon2 才发现盐是坏的
    let mut parsed = Vec::with_capacity(vaults.len());
    for v in &vaults {
        let salt = parse_salt(&v.salt).ok_or_else(|| CmdError::code("bad_salt"))?;
        parsed.push((
            salt,
            omy_core::crypto::Argon2Params {
                m_kib: v.m_kib,
                t: v.t,
                p: v.p,
            },
        ));
    }

    let handle: Shared = Arc::clone(&state);
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        handle.with_session(|s| {
            let mut ok = 0usize;
            for (i, (salt, params)) in parsed.iter().enumerate() {
                // 多个 vault 时给每个 KEK 一个不同的 label，
                // 否则它们在会话缓存里会互相覆盖
                let l = if parsed.len() == 1 {
                    label.clone()
                } else {
                    format!("{label}#{i}")
                };
                if s.unlock_password(&l, salt, &password, *params).is_ok() {
                    ok = ok.saturating_add(1);
                }
            }
            (ok, s.len())
        })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    match outcome {
        // 派生成功不代表密码对——KEK 是否正确要等真去解文件才知道。
        // 这里只要有一个 vault 派生成功就返回，由扫描结果告诉用户
        // 到底解开了几个文件
        Some((ok, total)) if ok > 0 => Ok(UnlockResult {
            credentials: total,
            vaults_unlocked: ok,
        }),
        Some(_) => Err(CmdError::code("wrong_password")),
        None => Err(CmdError::code("internal")),
    }
}

/// 探测一个目录，取出它包含的所有 vault 的解锁参数。
///
/// # 为什么返回的是一组而不是一个
///
/// 同一个 vault 里所有文件共用一个 salt（文档 03 §2.3 的两级 KDF 就是
/// 建立在这个前提上的：一次 Argon2 解开整个库）。但一个**文件夹**里
/// 完全可能混着多个 vault——用户分几次加密、从不同来源拷进来的文件，
/// 每批都有自己的 salt。
///
/// 早先的实现只取第一个文件的 salt，结果是：目录里 5 个文件只解开 1 个，
/// 其余 4 个显示为「锁定」，而用户明明用了正确的密码。这个缺陷是
/// GUI 端到端验证抓出来的——单测和编译都发现不了，因为它们不会
/// 去造一个「多 vault 混合」的目录。
///
/// 所以这里返回所有不同的 (salt, 参数) 组合，解锁时逐个派生。
/// 代价是有 N 个 vault 就跑 N 次 Argon2，但这是正确性的必需开销，
/// 且 N 通常很小（用户不会有几十个独立库）。
#[tauri::command]
pub fn vault_params_of(dir: String) -> CmdResult<Vec<VaultParams>> {
    let d = PathBuf::from(&dir);
    if !d.is_dir() {
        return Err(CmdError::code("not_a_directory"));
    }
    let entries = std::fs::read_dir(&d).map_err(|_| CmdError::code("io_error"))?;

    let mut seen: Vec<VaultParams> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = read_prefix(&p, 4096) else {
            continue;
        };
        let Ok(h) = omy_core::file::peek_header(&bytes) else {
            continue;
        };
        let salt = hex_of(&h.vault_salt);
        // 同一个 salt 只保留一份——否则 1000 个文件的库
        // 会让我们跑 1000 次同样的 Argon2
        if seen.iter().any(|v| v.salt == salt) {
            continue;
        }
        seen.push(VaultParams {
            salt,
            m_kib: h.argon2_m_kib,
            t: h.argon2_t,
            p: h.argon2_p,
        });
    }

    if seen.is_empty() {
        // 找不到可解析的文件时，把目录带回去：
        // 用户可能选错了文件夹，光说「没找到」他不知道是哪个
        return Err(CmdError::with(
            "no_omy_file_found",
            serde_json::json!({ "dir": dir }),
        ));
    }
    Ok(seen)
}

/// 把十六进制盐解析成 16 字节。
fn parse_salt(hex: &str) -> Option<[u8; 16]> {
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        let a = i.checked_mul(2)?;
        let s = hex.get(a..a.checked_add(2)?)?;
        *slot = u8::from_str_radix(s, 16).ok()?;
    }
    Some(out)
}

/// 锁定：抹掉所有密钥并清空文件列表。
///
/// # 为什么设备库也要一起关
///
/// 用户按 Ctrl+L 的心智是「把这个应用锁上」，不是「锁上文件但设备
/// 身份继续留在内存里」。只锁一半会让锁定成为**假象**：本机静态私钥
/// 仍在内存中，攻击者拿到进程内存后可以冒充这台设备去连别人。
///
/// 两者同生共死，就不会出现「以为锁了其实没锁」的状态差。
#[tauri::command]
pub fn lock(state: State<'_, Shared>, devices: State<'_, crate::device_cmds::SharedDevices>) {
    state.lock();
    devices.close();
}

/// 当前是否已解锁。
#[tauri::command]
pub fn is_unlocked(state: State<'_, Shared>) -> bool {
    state.is_unlocked()
}

/// 已解锁的凭据数量。
#[tauri::command]
pub fn credential_count(state: State<'_, Shared>) -> usize {
    state.credential_count()
}

/// 只读文件开头若干字节——探测头部不需要读整个文件。
///
/// 对 800 MB 的视频，读全文件再解析头部会让「添加目录」卡好几秒。
fn read_prefix(p: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

/// 字节转十六进制。
fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 扫描目录并返回文件列表。
///
/// 用 `spawn_blocking`：扫描要读大量文件头，是阻塞 IO。
#[tauri::command]
pub async fn scan_directory(
    state: State<'_, Shared>,
    dir: String,
    recursive: bool,
) -> CmdResult<Vec<FileEntry>> {
    let root = PathBuf::from(&dir);
    if !root.is_dir() {
        return Err(CmdError::code("not_a_directory"));
    }

    let handle: Shared = Arc::clone(&state);
    let root2 = root.clone();

    let entries = tauri::async_runtime::spawn_blocking(move || {
        let opts = ScanOptions {
            recursive,
            ..ScanOptions::default()
        };
        let result = handle.with_session(|s| scan_dir(&root2, s, &opts))?;
        let result = result.ok()?;

        let mut out = Vec::with_capacity(result.hits.len());
        for (i, hit) in result.hits.iter().enumerate() {
            // id 用序号 + 路径短哈希：既稳定又不泄露路径。
            // 直接用路径当 id 会让路径出现在 WebView 的 URL 里，
            // 而 URL 可能被记录在开发者工具的网络面板中。
            let id = format!("{i:04x}{}", short_hash(&hit.path.to_string_lossy()));
            out.push(FileEntry::from_hit(id, hit));
        }
        Some(out)
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
    .ok_or_else(|| CmdError::code("scan_failed"))?;

    state.add_root(root);
    state.set_files(entries.clone());
    Ok(entries)
}

/// 路径的短哈希，用于生成稳定 id。
fn short_hash(s: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    format!("{:08x}", h.finish() & 0xFFFF_FFFF)
}

/// 当前文件列表。
#[tauri::command]
pub fn list_files(state: State<'_, Shared>) -> Vec<FileEntry> {
    state.files()
}

/// 补充某个文件的媒体元信息。
///
/// 与扫描分开是有意的：扫描要快（文档 §9 要求 10000 文件 < 5s），
/// 而解析媒体元信息要解密 TLV。列表先出来，元信息按需补。
#[tauri::command]
pub async fn enrich_file(state: State<'_, Shared>, id: String) -> CmdResult<Option<FileEntry>> {
    let Some(entry) = state.file(&id) else {
        return Err(CmdError::code("file_not_found"));
    };
    if !entry.unlocked {
        // 锁定的文件不补元信息——那正是要隐藏的内容
        return Ok(None);
    }

    let handle: Shared = Arc::clone(&state);
    let path = entry.path.clone();

    let found = tauri::async_runtime::spawn_blocking(move || {
        // 元信息在头部区域，1 MiB 足够覆盖含缩略图的 TLV 区
        let bytes = read_prefix(Path::new(&path), 1 << 20).ok()?;
        let h = omy_core::file::peek_header(&bytes).ok()?;
        let keks: Vec<omy_core::crypto::Kek> = handle.with_session(|s| {
            s.all_for(&h.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })?;
        let opened = omy_core::file::open(&bytes, &keks).ok()?;
        let has_thumb = opened.thumbnail().is_ok();
        let meta = opened
            .media_meta()
            .ok()
            .and_then(|raw| omy_media::MediaMeta::from_json_bytes(&raw).ok());
        Some((meta, has_thumb))
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    if let Some((meta, has_thumb)) = found {
        // 文件名的后缀参与 MIME 推导：容器名不足以区分
        // （比如 ffprobe 对 mp4 报的是 "mov,mp4,m4a,3gp,3g2,mj2"）
        let name = entry.name.clone();
        state.update_file(&id, |e| {
            e.has_thumbnail = has_thumb;
            e.needs_transcode = crate::mime::image_needs_transcode(&crate::mime::extension_of(&name));
            if let Some(m) = &meta {
                let (kind, mime) = crate::mime::classify(&name, m);
                e.kind = Some(kind.to_owned());
                e.mime = Some(mime);
                // 播放分级只对音视频有意义。图片走 ffprobe 会被识别成
                // 「单帧视频」，从而带上 P3（全解码）分级——界面上就成了
                // 一张 PNG 标着「🐌 需要重新编码」，纯属误导。
                if kind == crate::mime::kind::VIDEO || kind == crate::mime::kind::AUDIO {
                    e.tier = Some(m.playback_tier.default.to_ascii_lowercase());
                    e.duration_ms = m.duration_ms;
                }
                if let Some((w, h)) = m.resolution() {
                    e.width = Some(w);
                    e.height = Some(h);
                }
            } else {
                // 没有媒体元信息：按文件名后缀兜底判断，
                // 文本和 SVG 这类不走 ffprobe 的类型全靠它
                let (kind, mime) = crate::mime::by_extension(&name);
                e.kind = Some(kind.to_owned());
                e.mime = Some(mime);
            }
        });
    }
    Ok(state.file(&id))
}

/// 当前界面语言。
#[tauri::command]
pub fn get_language(state: State<'_, Shared>) -> String {
    state.lang()
}

/// 设置界面语言。
#[tauri::command]
pub fn set_language(state: State<'_, Shared>, lang: String) -> String {
    let normalized = crate::state::normalize_language(&lang);
    state.set_lang(normalized.clone());
    normalized
}

/// 已加入的浏览目录。
#[tauri::command]
pub fn list_roots(state: State<'_, Shared>) -> Vec<String> {
    state
        .roots()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// 自定义协议的 URL 前缀。
///
/// # 为什么不能让前端写死
///
/// 各平台的 WebView 对自定义 scheme 的支持不一样：
///
/// | 平台 | URL 形式 | 原因 |
/// |---|---|---|
/// | Windows / Android | `http://omystream.localhost/` | WebView2 无法直接处理自定义 scheme，Tauri 把它映射成子域名 |
/// | macOS / iOS / Linux | `omystream://localhost/` | WKWebView / WebKitGTK 原生支持 |
///
/// 这个差异是实测踩出来的：先用 `omystream://localhost/` 写死，
/// 在 Windows 上所有请求都报
/// `Fetch API cannot load ... URL scheme "omystream" is not supported`，
/// `<video>` 则是静默的 `ERR_UNKNOWN_URL_SCHEME`——
/// 前端看到的只是「加载失败」，完全无从判断是协议没注册还是 URL 形式不对。
///
/// 由后端下发而不是前端嗅探 `navigator.platform`：后端本来就知道
/// 自己编译到哪个目标，而 UA 嗅探在 WebView 里并不可靠。
#[tauri::command]
pub fn stream_base() -> String {
    stream_base_url()
}

/// 打开原生目录选择器，返回用户选中的目录。
///
/// 用户取消时返回 `None`——这不是错误，前端应当静默处理。
///
/// # 为什么不用 `blocking_pick_folder`
///
/// 阻塞版在主线程调用会与事件循环死锁（插件文档明确警告）。
/// Tauri 命令虽然跑在别的线程上，但对话框本身要回到主线程弹出，
/// 阻塞版仍有死锁风险。这里用回调版 + oneshot 通道等结果：
/// 对话框在它该在的线程上弹，我们只是异步等一个值。
///
/// # 为什么不让前端直接用 dialog 插件
///
/// 那需要在 capabilities 里开放 `dialog:default`，等于给前端
/// 任意路径的选择能力。包成自己的命令后，前端只有「选一个目录」
/// 这一个受控入口，将来要加审计或路径限制也只需改这里。
#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle, title: String) -> CmdResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt as _;

    // 自动化验证用的旁路：原生对话框是 OS 窗口，CDP 点不到它，
    // 不留入口的话 GUI 端到端测试会永久卡在这里等一个没人点的窗口。
    //
    // 为什么这样做是安全的：这个变量只与 OMY_GUI_CDP_PORT 同场景使用，
    // 而后者本身就意味着「远程调试端口已开放」——真要攻击，
    // 直接通过 CDP 接管 WebView 比设这个变量容易得多。
    // 换言之它没有扩大攻击面。
    //
    // 为什么不用 #[cfg(test)]：单元测试跑不起 Tauri 运行时，
    // 这条路径只有真实 GUI 进程里才走得到。
    if let Ok(forced) = std::env::var("OMY_GUI_PICK_FOLDER") {
        if !forced.is_empty() {
            return Ok(Some(forced));
        }
    }

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .file()
        .set_title(if title.is_empty() { "选择文件夹" } else { &title })
        .pick_folder(move |picked| {
            // 发送失败只意味着接收端已经走了（窗口关闭等），
            // 没有可做的补救，也不该让它 panic
            let _ = tx.send(picked);
        });

    // 在阻塞线程上等：命令本身是 async，直接 recv 会占死执行器线程
    let picked = tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(|_| CmdError::code("internal"))?;

    Ok(picked.map(|p| p.to_string()))
}

/// 打开原生文件选择器，可多选。
///
/// 只列出 `.omy` 与分片文件——选中一个非 omy 文件对本应用没有意义，
/// 让它出现在列表里只会让用户白试一次。
#[tauri::command]
pub async fn pick_files(app: tauri::AppHandle, title: String) -> CmdResult<Vec<String>> {
    use tauri_plugin_dialog::DialogExt as _;

    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .file()
        .set_title(if title.is_empty() { "选择文件" } else { &title })
        .add_filter("omy", &["omy"])
        .pick_files(move |picked| {
            let _ = tx.send(picked);
        });

    let picked = tauri::async_runtime::spawn_blocking(move || rx.recv().ok().flatten())
        .await
        .map_err(|_| CmdError::code("internal"))?;

    Ok(picked
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string())
        .collect())
}

/// 按编译目标返回协议前缀。
#[must_use]
pub fn stream_base_url() -> String {
    if cfg!(any(windows, target_os = "android")) {
        String::from("http://omystream.localhost")
    } else {
        String::from("omystream://localhost")
    }
}

/// 用一个密码尝试解锁指定目录，并立即扫描。
///
/// # 与 `unlock` + `scan_directory` 的区别
///
/// 那两个命令是给「先解锁再进门」的旧流程用的：前端得先调
/// `vault_params_of` 拿参数，再调 `unlock`，再调 `scan_directory`。
/// 三次往返，且中间任何一步失败都要前端自己处理。
///
/// 新交互里用户是在文件管理器里**顺手**输个密码试试，所以合成一个
/// 命令：探测 vault → 派生 → 扫描，一次返回结果。解不开就是解不开，
/// 不需要用户理解「vault 参数」是什么。
///
/// # Errors
///
/// - `empty_password`：密码为空
/// - `no_vault_found`：这个目录里没有本应用的加密文件
/// - `wrong_password`：所有 vault 都没派生成功
#[tauri::command]
pub async fn unlock_directory(
    state: State<'_, Shared>,
    dir: String,
    label: String,
    password: String,
) -> CmdResult<UnlockResult> {
    if password.is_empty() {
        return Err(CmdError::code("empty_password"));
    }

    let vaults = vault_params_of(dir.clone())?;
    if vaults.is_empty() {
        return Err(CmdError::code("no_vault_found"));
    }

    let label = if label.is_empty() {
        String::from("main")
    } else {
        label
    };
    unlock(state, label, password, vaults).await
}

/// 单个加密文件的探测结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeResult {
    /// 是否是本应用的加密文件。
    pub is_omy: bool,
    /// 当前会话能否打开它。
    pub unlocked: bool,
    /// 能打开时的真实文件名。
    pub name: Option<String>,
}

/// 探测单个文件。
///
/// 双击一个加密文件时用：先看看当前会话能不能直接打开，
/// 能就直接预览，不能才弹密码框。省掉「明明已经解锁却还要再输一次」。
///
/// 复用 core 的 `probe_file` 而不是自己拼 `open` + 解 TLV：
/// 文件名藏在加密的 TLV 里，取它要先 unwrap FEK 再解密再去 padding，
/// 这套流程 scan.rs 已经写好且有测试覆盖，重写一遍只会引入分歧。
#[tauri::command]
pub async fn probe_one(state: State<'_, Shared>, path: String) -> CmdResult<ProbeResult> {
    let p = PathBuf::from(&path);
    let handle: Shared = Arc::clone(&state);

    tauri::async_runtime::spawn_blocking(move || {
        // 先用文件头快速排除非 omy 文件，避免为每个普通文件
        // 都走一遍完整的 probe
        let Ok(prefix) = read_prefix(&p, 64) else {
            return ProbeResult {
                is_omy: false,
                unlocked: false,
                name: None,
            };
        };
        if !omy_core::file::is_omy_file(&prefix) {
            return ProbeResult {
                is_omy: false,
                unlocked: false,
                name: None,
            };
        }

        let hit = handle
            .with_session(|s| omy_core::scan::probe_file(&p, s).ok().flatten())
            .flatten();

        match hit.map(|h| h.unlock) {
            Some(omy_core::scan::UnlockOutcome::Unlocked { filename, .. }) => ProbeResult {
                is_omy: true,
                unlocked: true,
                name: filename,
            },
            _ => ProbeResult {
                is_omy: true,
                unlocked: false,
                name: None,
            },
        }
    })
    .await
    .map_err(|_| CmdError::code("internal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn salt_parsing() {
        let hex = "000102030405060708090a0b0c0d0e0f";
        let got = parse_salt(hex).unwrap_or([0xFF; 16]);
        assert_eq!(got, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(hex_of(&got), hex, "往返必须一致");
    }

    #[test]
    fn salt_rejects_bad_input() {
        // 长度不对必须拒绝，不能补零或截断——
        // 那会静默派生出错误的 KEK，用户会在密码正确时看到「密码错误」
        assert!(parse_salt("").is_none());
        assert!(parse_salt("00").is_none());
        assert!(parse_salt(&"0".repeat(31)).is_none());
        assert!(parse_salt(&"0".repeat(33)).is_none());
        assert!(parse_salt("zz0102030405060708090a0b0c0d0e0f").is_none());
    }

    #[test]
    fn short_hash_is_stable_and_differs() {
        let a = short_hash("/x/a.omy");
        assert_eq!(a, short_hash("/x/a.omy"), "同一路径必须得到同一 id");
        assert_ne!(a, short_hash("/x/b.omy"), "不同路径应得到不同 id");
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn error_carries_code_not_prose() {
        // 后端只给错误码，翻译交给前端——
        // 否则英文界面会突然冒出中文错误
        let e = CmdError::code("wrong_password");
        assert_eq!(e.code, "wrong_password");
        let j = serde_json::to_string(&e).unwrap_or_default();
        assert!(j.contains("wrong_password"));
        assert!(!j.contains("密码"), "错误里不能有拼好的中文");
    }

    #[test]
    fn error_with_params() {
        let e = CmdError::with("need_memory", serde_json::json!({ "required": 1024 }));
        let j = serde_json::to_string(&e).unwrap_or_default();
        assert!(j.contains("1024"));
    }

    #[test]
    fn vault_params_roundtrip() {
        // 前端会原样回传这个结构，字段名必须稳定
        let v = VaultParams {
            salt: String::from("000102030405060708090a0b0c0d0e0f"),
            m_kib: 65536,
            t: 3,
            p: 1,
        };
        let j = serde_json::to_string(&v).unwrap_or_default();
        let back: VaultParams = serde_json::from_str(&j).unwrap_or(VaultParams {
            salt: String::new(),
            m_kib: 0,
            t: 0,
            p: 0,
        });
        assert_eq!(back.salt, v.salt);
        assert_eq!(back.m_kib, 65536);
        assert_eq!(back.t, 3);
    }

    #[test]
    fn vault_list_serializes_as_array() {
        // 命令返回的是一组而非单个：一个文件夹里可能混着多个独立的库。
        // 这条断言锁定契约——若有人改回单个，前端会静默拿到错误结构
        let list = vec![
            VaultParams {
                salt: String::from("00").repeat(16),
                m_kib: 32768,
                t: 4,
                p: 1,
            },
            VaultParams {
                salt: String::from("ff").repeat(16),
                m_kib: 65536,
                t: 3,
                p: 1,
            },
        ];
        let j = serde_json::to_string(&list).unwrap_or_default();
        assert!(j.starts_with('['), "必须是数组，实得 {j}");
        let back: Vec<VaultParams> = serde_json::from_str(&j).unwrap_or_default();
        assert_eq!(back.len(), 2);
        // 两个 vault 的参数各自独立，不能被合并或覆盖
        assert_eq!(back.first().map(|v| v.m_kib), Some(32768));
        assert_eq!(back.get(1).map(|v| v.m_kib), Some(65536));
    }

    #[test]
    fn unlock_result_reports_vault_count() {
        // 部分成功是正常情况：目录里可能混着用其他密码加密的文件。
        // 前端需要知道「派生成功几个」才能给出准确提示
        let r = UnlockResult {
            credentials: 3,
            vaults_unlocked: 2,
        };
        let j = serde_json::to_string(&r).unwrap_or_default();
        assert!(j.contains("vaults_unlocked"));
        assert!(j.contains('2'));
    }

    #[test]
    fn stream_base_matches_platform_webview() {
        // Windows/Android 的 WebView2 不认自定义 scheme，
        // Tauri 把它映射成 http://<scheme>.localhost。
        // 写死任何一种都会让另一半平台完全无法加载内容
        let base = stream_base_url();
        if cfg!(any(windows, target_os = "android")) {
            assert_eq!(base, "http://omystream.localhost");
        } else {
            assert_eq!(base, "omystream://localhost");
        }
        // 无论哪个平台，都不能有结尾斜杠——前端会拼 /file/<id>
        assert!(!base.ends_with('/'), "前缀不应带结尾斜杠：{base}");
    }
}
