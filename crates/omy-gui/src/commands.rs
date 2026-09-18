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
    /// 本次输入的密码是不是一个**新**密码。
    ///
    /// `false` 表示它与会话里已有的某个密码相同，本次没有新增凭据。
    /// 前端据此把提示从「已添加密码」改成「这个密码已经在用了」——
    /// 不区分的话，用户重复输入同一个密码会看到「已添加」却发现计数
    /// 没变，像是操作失败了。
    pub added: bool,
}

/// 用密码解锁一个或多个 vault。
///
/// Argon2 派生要几百毫秒，必须放在阻塞线程里，否则 UI 会卡住。
/// Tauri 的 `async` 命令跑在 async 运行时上，直接做 CPU 密集工作
/// 会占死执行器线程——所以用 `spawn_blocking`。
///
/// 对每个 vault 都派生一次。一个都没成功才算失败：部分成功是
/// 正常情况（目录里可能混着别人的、用其他密码加密的文件）。
///
/// # 累加，不是替换
///
/// 走 [`omy_core::session::SessionKeys::add_password`]，所以再输一个
/// **不同**的密码不会挤掉已装入的——真实密码与诱饵密码可以同时生效，
/// 一遍扫描各自命中自己的文件。同一个密码重复输入则只算一条。
///
/// 早先这里用的是 `unlock_password`（替换语义）加一个写死的
/// `label = "main"`，两者叠加的效果是「会话里永远只能有一个密码」。
/// 那个写死本身是为修另一个缺陷加的，根因见 core 侧 `add_password`
/// 的文档。
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
            let mut added = false;
            for (salt, params) in &parsed {
                // 每个 vault 各派生一次：同一个密码在不同 salt 下是不同的
                // KEK，必须各占一条（这与「多个密码」是两件事，见
                // `vault_params_of` 的文档）。
                //
                // label 不再需要手工加 #i 后缀去避让——add_password 自己
                // 会在重名时让开，而身份判断走的是指纹
                // 派生失败（Argon2 参数非法）只影响这一个 vault，
                // 其它的照常试——目录里混着多个库时不该一个坏头部
                // 就让整次解锁失败
                if let Ok(is_new) = s.add_password(&label, salt, &password, *params) {
                    ok = ok.saturating_add(1);
                    added |= is_new;
                }
            }
            (ok, s.len(), added)
        })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    match outcome {
        // 派生成功不代表密码对——KEK 是否正确要等真去解文件才知道。
        // 这里只要有一个 vault 派生成功就返回，由扫描结果告诉用户
        // 到底解开了几个文件
        Some((ok, total, added)) if ok > 0 => Ok(UnlockResult {
            credentials: total,
            vaults_unlocked: ok,
            added,
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
        // 树形加密的目录要往里取一个样本文件。
        //
        // 早先这里是 `if !p.is_file() { continue }`，于是站在密文树的
        // **父目录**输密码会报 no_omy_file_found——「这个目录里没有加密
        // 文件」，可用户明明看到一个带锁的加密文件夹就在眼前。而这恰好是
        // 最常见的位置：刚加密完，站在原地看产物。
        //
        // 只对**看起来是密文目录**的做这件事，不对所有目录递归：
        // 否则浏览一个有几万个文件的普通目录时，每次解锁都要深度遍历。
        let sample = if p.is_dir() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !omy_core::dirname::looks_encrypted(&name) {
                continue;
            }
            match omy_core::tree::find_any_file(&p) {
                Some(s) => s,
                None => continue,
            }
        } else {
            p
        };
        let Ok(bytes) = read_prefix(&sample, 4096) else {
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
pub(crate) fn parse_salt(hex: &str) -> Option<[u8; 16]> {
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
pub async fn lock(
    state: State<'_, Shared>,
    devices: State<'_, crate::device_cmds::SharedDevices>,
    remote: State<'_, std::sync::Arc<crate::remote::RemoteSession>>,
    place_files: State<'_, std::sync::Arc<crate::place_files::PlaceFiles>>,
    place_thumbs: State<'_, std::sync::Arc<crate::place_files::PlaceThumbs>>,
) -> CmdResult<()> {
    state.lock();
    devices.close();
    // 远端连接也必须断开，而且必须断在**后端**。
    //
    // 一开始只在前端的 doLock() 里调了 disconnect，实测发现锁定后
    // `remote_status` 仍然报 connected、协议里还能取到明文——因为
    // 任何绕过那段前端代码的调用（别的页面脚本、CDP、将来新加的
    // 快捷键分支）都会留下一条**活着且已认证**的信道。
    //
    // 锁定的语义是「从现在起什么都看不到」。把这条保证寄托在
    // 「前端记得多调一个函数」上，等于没有这条保证。
    remote.disconnect().await;
    // 远程云盘的播放句柄同理：token 还在就能凭 omystream://pfile/ 继续
    // Range 取明文，必须在后端随锁定一并关闭。密文磁盘缓存不删——
    // 它本就是密文，删不删不影响「锁定后读不到明文」这条保证。
    place_files.clear();
    // 列表缩略图句柄（文件头）同样随锁定失效，否则锁定后 pthumb token 仍在。
    place_thumbs.clear();
    Ok(())
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
    let f = std::fs::File::open(p)?;
    // 用 take + read_to_end 而不是单次 read：`Read::read` **不保证**填满
    // 缓冲区，它可以只返回一部分。单次 read 对小缓冲区几乎总能读满，
    // 所以这个问题在 64 字节的探测里从来不显现；但按 header_len 读几 KB
    // 时一旦短读，open() 就会因为数据不全而失败——表现为「同一个文件
    // 有时有缩略图有时没有」，极难复现。
    let mut buf = Vec::new();
    f.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// 读出足够覆盖整个头部区域（含 TLV）的字节，且**一个字节都不多读**。
///
/// # 为什么要两段读，而不是取一个够大的固定前缀
///
/// 固定前缀是在「够不够」和「多读多少」之间赌一个常量。实测数据
/// （10000 个文件，4 MiB 一个）说明这个赌注很贵：
///
/// | 前缀 | 每文件 | 10000 个文件 |
/// |---|---|---|
/// | 32 KiB | 0.047 ms | 0.47 s |
/// | 1 MiB | 0.549 ms | **5.49 s** |
///
/// 1 MiB 是 `enrich_file` 原来的取值——单个文件预览时无所谓，但扫描
/// 整个目录就是 10 GB 的读，直接冲破文档 §9 的「10000 文件 < 5s」。
/// 瓶颈完全在 IO：实际头部只有约 7 KB。
///
/// 格式本来就为这个场景留了 [`omy_core::header::HEADER_LEN_FIELD_OFFSET`]，
/// 让扫描器能先取出 `header_len` 再决定读多少。两段读之后耗时与文件
/// 大小**彻底无关**（实测小文件与 4 MiB 文件都是 0.09 ms/文件），
/// 不必再赌某个常量对将来的缩略图尺寸够不够大。
fn read_header_area(p: &Path) -> std::io::Result<Vec<u8>> {
    let off = omy_core::header::HEADER_LEN_FIELD_OFFSET;
    let head = read_prefix(p, off.saturating_add(4))?;
    let Some(field) = head
        .get(off..off.saturating_add(4))
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
    else {
        // 比固定头还短，不可能是 omy 文件。把已读到的还回去，
        // 让上层用统一的 peek_header 去报错，而不是在这里另造一种错误
        return Ok(head);
    };
    let claimed = u32::from_le_bytes(field) as usize;
    // header_len 来自**尚未验证**的文件内容，不能直接拿去分配内存：
    // 一个声称 4 GiB 的坏文件会让扫描进程被 OOM 杀掉。用文件实际长度
    // 兜住——头部不可能比文件本身长。
    let actual = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let cap = claimed.min(usize::try_from(actual).unwrap_or(usize::MAX));
    read_prefix(p, cap)
}

/// 从磁盘上的一个加密文件读出媒体附加信息。
///
/// 返回 `(媒体元信息, 有无缩略图, 是否是目录容器)`。
///
/// 提出来是因为**扫描和按需补齐都要做同一件事**：扫描时批量填，
/// 预览时对单个文件再确认一次。两边各写一遍的话，新增字段时漏掉
/// 一边就会出现「网格里有缩略图、预览里没有」这类只在特定操作顺序下
/// 才显现的不一致。
fn read_media_extras(
    path: &str,
    handle: &Shared,
) -> Option<(Option<omy_media::MediaMeta>, bool, bool)> {
    let bytes = read_header_area(Path::new(path)).ok()?;
    let h = omy_core::file::peek_header(&bytes).ok()?;
    let keks: Vec<omy_core::crypto::Kek> =
        handle.with_session(|s| s.all_for(&h.vault_salt).into_iter().map(|c| c.kek).collect())?;
    let opened = omy_core::file::open(&bytes, &keks).ok()?;
    let has_thumb = opened.thumbnail().is_ok();
    // 目录容器的载荷是多个文件拼接。不认出来的话，前端会把整包
    // 当成单个文件送去预览，得到一堆拼在一起的字节
    let is_container = opened.folder_index().is_ok();
    let meta = opened
        .media_meta()
        .ok()
        .and_then(|raw| omy_media::MediaMeta::from_json_bytes(&raw).ok());
    Some((meta, has_thumb, is_container))
}

/// 把读到的媒体附加信息写进一个条目。
///
/// 与 [`read_media_extras`] 分开：那个碰磁盘和密钥，这个是纯字段映射，
/// 可以单独测。
fn apply_media_extras(
    e: &mut FileEntry,
    name: &str,
    meta: Option<&omy_media::MediaMeta>,
    has_thumb: bool,
    is_container: bool,
) {
    e.has_thumbnail = has_thumb;
    e.is_container = is_container;
    e.needs_transcode = crate::mime::image_needs_transcode(&crate::mime::extension_of(name));
    if let Some(m) = meta {
        // 文件名的后缀参与 MIME 推导：容器名不足以区分
        // （比如 ffprobe 对 mp4 报的是 "mov,mp4,m4a,3gp,3g2,mj2"）
        let (kind, mime) = crate::mime::classify(name, m);
        e.kind = Some(kind.to_owned());
        e.mime = Some(mime);
        // 播放分级只对音视频有意义。图片走 ffprobe 会被识别成
        // 「单帧视频」，从而带上 P3（全解码）分级——界面上就成了
        // 一张 PNG 标着「🐌 需要重新编码」，纯属误导。
        if kind == crate::mime::kind::VIDEO || kind == crate::mime::kind::AUDIO {
            e.tier = Some(m.playback_tier.default.to_ascii_lowercase());
            // 理由是这个角标唯一有用的部分：光看 🐌 用户猜不出含义，
            // 实测就有人问「蜗牛是什么意思」。界面把它做成悬停提示。
            e.tier_reason = Some(m.playback_tier.reason.clone());
            e.duration_ms = m.duration_ms;
        }
        if let Some((w, h)) = m.resolution() {
            e.width = Some(w);
            e.height = Some(h);
        }
    } else {
        // 没有媒体元信息：按文件名后缀兜底判断，
        // 文本和 SVG 这类不走 ffprobe 的类型全靠它
        let (kind, mime) = crate::mime::by_extension(name);
        e.kind = Some(kind.to_owned());
        e.mime = Some(mime);
    }
}

/// 字节转十六进制。
pub fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 扫描目录并返回文件列表。
///
/// 用 `spawn_blocking`：扫描要读大量文件头，是阻塞 IO。
///
/// # 为什么顺带把媒体附加信息也填上
///
/// 缩略图、`kind`、时长这些字段原先只由 `enrich_file` 填，而前端只在
/// **打开预览时**才调它。于是网格视图永远拿不到 `has_thumbnail`，
/// 加密文件一律显示成锁图标——缩略图明明写进文件了，界面上就是不出现。
///
/// 当初分开是担心扫描变慢（文档 §9 要求 10000 文件 < 5s）。实测这个
/// 担心不成立：按 `header_len` 精确读取后是 **0.09 ms/文件**，10000 个
/// 文件约 0.9 s，且与文件大小无关（见 [`read_header_area`]）。
/// 所以直接在扫描时填全，不必让前端为可见范围另做一套按需加载。
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
            let mut entry = FileEntry::from_hit(id, hit);
            // 只对解开了的文件补：锁定文件的媒体信息正是要隐藏的内容，
            // 而且没有密钥也读不出来
            if entry.unlocked {
                let name = entry.name.clone();
                if let Some((meta, has_thumb, is_container)) =
                    read_media_extras(&entry.path, &handle)
                {
                    apply_media_extras(
                        &mut entry,
                        &name,
                        meta.as_ref(),
                        has_thumb,
                        is_container,
                    );
                }
            }
            out.push(entry);
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
/// 扫描时已经填过一轮（见 [`scan_directory`]），这里是给「扫描之后
/// 文件被改动」和「单个文件直接打开、没经过扫描」两种情况兜底。
/// 两条路径共用 [`read_media_extras`] 与 [`apply_media_extras`]，
/// 避免出现「网格里有缩略图、预览里没有」这类不一致。
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

    let found = tauri::async_runtime::spawn_blocking(move || read_media_extras(&path, &handle))
        .await
        .map_err(|_| CmdError::code("internal"))?;

    if let Some((meta, has_thumb, is_container)) = found {
        let name = entry.name.clone();
        state.update_file(&id, |e| {
            apply_media_extras(e, &name, meta.as_ref(), has_thumb, is_container);
        });
    }
    Ok(state.file(&id))
}

/// 容器内的一个条目。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ContainerItem {
    /// 相对容器根的路径组件，用 `/` 连接后展示。
    pub path: String,
    /// 末级名称。
    pub name: String,
    /// 是否是目录。
    pub is_dir: bool,
    /// 字节数；目录为 `None`。
    pub size: Option<u64>,
    /// 在明文载荷中的起始偏移。目录为 `None`。
    ///
    /// 后端据此定位，`read_range` 按这个偏移取数据。
    pub offset: Option<u64>,
    /// 预览类别，按文件名后缀判断。
    pub kind: String,
    /// MIME。
    pub mime: String,
    /// 预览用的访问 token。目录为 `None`。
    ///
    /// 前端拼成 `omystream://…/citem/<token>` 请求内容。**不能**让前端
    /// 直接传偏移和长度：那等于把「读这个容器任意位置」的能力交给
    /// WebView 里的任何脚本，越过了索引这层约束（见 [`crate::citem`]）。
    pub token: Option<String>,
}

/// 列出一个目录容器里的条目。
///
/// 只在文件已解锁时可用——容器里有什么本身就是要保护的内容。
///
/// # Errors
///
/// - `file_not_found`：id 不存在
/// - `locked`：文件未解锁
/// - `not_a_container`：这个文件不是目录容器
#[tauri::command]
pub async fn list_container(
    state: State<'_, Shared>,
    id: String,
) -> CmdResult<Vec<ContainerItem>> {
    let Some(entry) = state.file(&id) else {
        return Err(CmdError::code("file_not_found"));
    };
    if !entry.unlocked {
        return Err(CmdError::code("locked"));
    }

    let handle: Shared = Arc::clone(&state);
    let path = entry.path.clone();

    let idx = tauri::async_runtime::spawn_blocking(move || {
        // 索引在 TLV 区，读头部就够，不必把整个容器载荷读进来。
        // 大容器的索引可达 MB 量级，给 4 MiB 上限
        let bytes = read_prefix(Path::new(&path), 4 << 20).ok()?;
        let h = omy_core::file::peek_header(&bytes).ok()?;
        let keks: Vec<omy_core::crypto::Kek> = handle.with_session(|s| {
            s.all_for(&h.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })?;
        let opened = omy_core::file::open(&bytes, &keks).ok()?;
        opened.folder_index().ok()
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    let Some(idx) = idx else {
        return Err(CmdError::code("not_a_container"));
    };

    let mut items = items_from_index(&idx);
    // 给每个文件登记 token。放在这里而不是 `items_from_index` 里，是为了
    // 让那个函数保持纯映射、可单独测；登记要碰全局状态
    for it in &mut items {
        if let (Some(off), Some(size)) = (it.offset, it.size) {
            it.token = state.citem.register(crate::citem::ContainerRef {
                entry_id: id.clone(),
                inner_path: it.path.clone(),
                offset: off,
                size,
                mime: it.mime.clone(),
            });
        }
    }

    Ok(items)
}

/// 把容器索引转成前端条目。
///
/// 单独提出来是为了能测：这段映射（偏移、目录判定、MIME 推导）才是
/// 会出错的地方，而 `list_container` 的其余部分是取状态和解密，
/// 那些在别处已有覆盖。
fn items_from_index(idx: &omy_core::container::ContainerIndex) -> Vec<ContainerItem> {
    idx.entries
        .iter()
        .map(|e| {
            let name = e.path.last().cloned().unwrap_or_default();
            // 用 range() 判断而不是比较 kind：range() 是「有没有载荷区间」
            // 的权威答案，符号链接之类的非文件条目也会正确地落到 None
            let range = e.range();
            let is_dir = range.is_none();
            // 目录不需要 MIME，但给个稳定值比留空更好处理
            let (kind, mime) = if is_dir {
                (String::from("folder"), String::new())
            } else {
                let (k, m) = crate::mime::by_extension(&name);
                (k.to_owned(), m)
            };
            ContainerItem {
                path: e.path.join("/"),
                name,
                is_dir,
                size: range.map(|(_, len)| len),
                offset: range.map(|(off, _)| off),
                kind,
                mime,
                // token 由 `list_container` 登记后填入：那里才有全局状态
                token: None,
            }
        })
        .collect()
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
    // 安卓没有原生文件夹选择器：tauri-plugin-dialog 在移动端不提供
    // pick_folder，而 SAF 的 ACTION_OPEN_DOCUMENT_TREE 返回 content:// URI，
    // 与本项目基于路径的 std::fs 核心接不上（详见 crate::storage 的说明）。
    //
    // 安卓上选目录走另一条路：拿到全盘权限后直接在侧栏和列表里浏览，
    // 目标目录本身就是当前 cwd，不需要额外的选择器。
    // 明确报不支持而不是静默返回 None——前端靠错误码区分「取消了」和「用不了」。
    #[cfg(target_os = "android")]
    {
        let _ = (app, title);
        Err(CmdError::code("unsupported"))
    }

    #[cfg(not(target_os = "android"))]
    {
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
    password: String,
) -> CmdResult<UnlockResult> {
    if password.is_empty() {
        return Err(CmdError::code("empty_password"));
    }

    let vaults = vault_params_of(dir.clone())?;
    if vaults.is_empty() {
        return Err(CmdError::code("no_vault_found"));
    }

    // label 只是显示名，不再承担「这是哪个密码」的判断。
    //
    // 它曾经是会话缓存键 (vault_salt, kind, label) 的一部分，于是同一个
    // 密码配不同 label 会被算成两条凭据；当时的对策是统一写死成 "main"，
    // 代价是第二个**不同**的密码会被静默挤掉。现在身份判断走 KEK 指纹
    // （见 core 的 `add_password`），两个方向都对了，这里给个固定名字
    // 纯粹是因为 GUI 没有界面让用户命名。
    let label = String::from("main");
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

    /// 一个干净的条目，供媒体字段映射的测试用。
    ///
    /// 所有媒体字段都是空的，这样断言「某个字段被填上了」时，
    /// 通过一定是因为映射真的写了它，而不是初值恰好相同。
    fn sample_entry() -> FileEntry {
        FileEntry {
            id: String::from("0000deadbeef"),
            path: String::from("/x/a.png"),
            name: String::from("a.png"),
            unlocked: true,
            size: Some(1),
            encrypted_size: 1,
            kind: None,
            mime: None,
            tier: None,
            tier_reason: None,
            duration_ms: None,
            width: None,
            height: None,
            has_thumbnail: false,
            needs_transcode: false,
            credential: None,
            is_container: false,
        }
    }

    /// 造一个真容器索引：两个文件 + 一个目录。
    fn sample_index() -> omy_core::container::ContainerIndex {
        use omy_core::container::{ContainerBuilder, EntryMeta};
        let mut b = ContainerBuilder::new(String::from("root"));
        b.add_dir(vec![String::from("pics")], EntryMeta::default())
            .unwrap();
        b.add_file(
            vec![String::from("pics"), String::from("a.png")],
            100,
            None,
            EntryMeta::default(),
        )
        .unwrap();
        b.add_file(
            vec![String::from("notes.txt")],
            42,
            None,
            EntryMeta::default(),
        )
        .unwrap();
        b.finish().unwrap()
    }

    #[test]
    fn container_items_carry_offsets_and_kinds() {
        let items = items_from_index(&sample_index());
        assert_eq!(items.len(), 3);

        let dir = items.iter().find(|i| i.name == "pics").unwrap();
        assert!(dir.is_dir, "目录必须标成目录");
        assert!(dir.offset.is_none(), "目录没有载荷区间");
        assert!(dir.size.is_none());

        let png = items.iter().find(|i| i.name == "a.png").unwrap();
        assert!(!png.is_dir);
        assert_eq!(png.size, Some(100));
        assert_eq!(png.kind, "image", "按后缀推出图片类别");
        assert_eq!(png.path, "pics/a.png", "路径要能显示层级");

        let txt = items.iter().find(|i| i.name == "notes.txt").unwrap();
        assert_eq!(txt.kind, "text");
        // 偏移必须真的不同，否则预览会全部指向同一段数据
        assert_ne!(png.offset, txt.offset, "两个文件的偏移不能相同");
    }

    #[test]
    fn container_offsets_are_contiguous_and_ordered() {
        // 容器载荷是拼接的，所有文件区间必须首尾相接、不重叠。
        // 错了的话预览会读到别的文件的字节——而且往往「能显示，
        // 只是内容不对」，最难发现
        let items = items_from_index(&sample_index());
        let mut spans: Vec<(u64, u64)> = items
            .iter()
            .filter_map(|i| Some((i.offset?, i.size?)))
            .collect();
        spans.sort_unstable();

        let mut cursor = 0u64;
        for (off, len) in spans {
            assert_eq!(off, cursor, "区间之间不能有空隙或重叠");
            cursor += len;
        }
        assert_eq!(cursor, 142, "总长度应为 100 + 42");
    }

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
            added: true,
        };
        let j = serde_json::to_string(&r).unwrap_or_default();
        assert!(j.contains("vaults_unlocked"));
        assert!(j.contains('2'));
    }

    /// `added` 必须出现在序列化结果里，且能表达 false。
    ///
    /// 不这样会怎样：前端靠它区分「加了一个新密码」和「这个密码已经在用」。
    /// 字段若被 skip 掉或恒为 true，用户重复输入同一个密码会看到
    /// 「已添加」，但凭据数不变——看起来像操作失败了。
    #[test]
    fn unlock_result_carries_added_flag() {
        let dup = UnlockResult {
            credentials: 1,
            vaults_unlocked: 1,
            added: false,
        };
        let j = serde_json::to_string(&dup).unwrap_or_default();
        assert!(j.contains("\"added\":false"), "重复密码必须如实报 false，实得 {j}");
    }

    #[test]
    fn media_extras_set_thumbnail_flag() {
        // 这个字段决定网格里显示缩略图还是锁图标。曾经它只由 enrich_file
        // 设置，而前端只在打开预览时才调那个命令——于是网格永远拿不到，
        // 缩略图明明在文件里却不显示。
        let mut e = sample_entry();
        assert!(!e.has_thumbnail, "初始应为 false");
        apply_media_extras(&mut e, "a.png", None, true, false);
        assert!(e.has_thumbnail, "有缩略图时必须置为 true");
    }

    #[test]
    fn media_extras_fall_back_to_extension_without_meta() {
        // 文本、SVG 这类不走 ffprobe，没有 MediaMeta。
        // 不兜底的话它们的 kind 会一直是 None，前端无法决定怎么预览
        let mut e = sample_entry();
        apply_media_extras(&mut e, "notes.txt", None, false, false);
        assert_eq!(e.kind.as_deref(), Some("text"));
        assert!(e.mime.is_some(), "MIME 也要有兜底值");
    }

    #[test]
    fn media_extras_do_not_mark_images_as_needing_transcode() {
        // 图片经 ffprobe 会被认成「单帧视频」，若把播放分级也一并套上，
        // 界面上就是一张 PNG 标着「需要重新编码」。tier 只对音视频有意义
        let mut e = sample_entry();
        apply_media_extras(&mut e, "a.png", None, true, false);
        assert!(e.tier.is_none(), "图片不该有播放分级，实得 {:?}", e.tier);
        assert!(e.duration_ms.is_none(), "图片不该有时长");
        // 理由与分级必须同进同退：只清 tier 而留下 reason，界面上会出现
        // 「没有角标却有悬停说明」，或反过来把视频的理由套到图片上
        assert!(
            e.tier_reason.is_none(),
            "图片不该有分级理由，实得 {:?}",
            e.tier_reason
        );
    }

    #[test]
    fn media_extras_flag_container() {
        // 认不出容器的话，前端会把整包当单个文件送去预览，
        // 得到一堆拼在一起的字节
        let mut e = sample_entry();
        apply_media_extras(&mut e, "folder.omy", None, false, true);
        assert!(e.is_container);
    }

    #[test]
    fn header_area_read_is_bounded_by_file_length() {
        // header_len 取自尚未验证的文件内容。一个声称 4 GiB 的坏文件
        // 若被直接拿去分配，扫描进程会被 OOM 杀掉——而这在正常文件上
        // 永远不会发生，所以只有显式构造坏文件才能测到。
        let dir = std::env::temp_dir().join("omy-hdr-test");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("evil.omy");
        // 前 12 字节随意，第 12..16 字节是 header_len：填一个巨大的值
        let mut bytes = vec![0u8; 12];
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(b"tail");
        let _ = std::fs::write(&p, &bytes);

        let got = read_header_area(&p).unwrap_or_default();
        // 关键：读回来的不能超过文件实际长度，也不能是 4 GiB 的缓冲区
        assert!(
            got.len() <= bytes.len(),
            "读取量 {} 超过文件实际长度 {}",
            got.len(),
            bytes.len()
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn header_area_handles_truncated_file() {
        // 比固定头还短的文件不能 panic，也不能读出越界数据
        let dir = std::env::temp_dir().join("omy-hdr-test2");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("short.omy");
        let _ = std::fs::write(&p, b"tiny");
        let got = read_header_area(&p).unwrap_or_default();
        assert_eq!(got.len(), 4, "应原样返回已读到的字节");
        let _ = std::fs::remove_file(&p);
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
