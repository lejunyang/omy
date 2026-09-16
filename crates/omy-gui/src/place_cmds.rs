//! 远程位置的 Tauri 命令。
//!
//! # 列目录时顺带识别加密文件
//!
//! 这是格式设计带来的便宜：识别一个 `.omy` 文件只需读**开头 480 字节**
//! （见 `omy_core::scan` 的模块文档），载荷完全不碰。放到远程就是一次
//! Range 请求——扫 100 个文件只下约 48 KB，而不是把文件拉下来。
//!
//! # 三种状态必须分开
//!
//! 已解锁 / 是 omy 但当前密码打不开 / **没能探测成功**（网络失败）。
//! 第三种混进第二种是最糟的结果：用户会以为自己记错密码而反复尝试，
//! 真正的问题却是网络。

use std::sync::Arc;

use tauri::Emitter;

use crate::commands::{CmdError, CmdResult, Shared};
use crate::decrypt::{DecryptProgress, DECRYPT_PROGRESS_EVENT};
use crate::place_files::{OpenPlaceFile, PlaceFiles, PlaceThumbs, RemoteCache};
use crate::places::{PlaceInfo, PlaceRegistry};
use omy_core::crypto::Kek;
use omy_remote::source::RemoteSource;
use omy_remote::webdav::{WebDavConfig, WebDavStore};
use omy_remote::{Error as RemoteError, RemoteStore};

/// 远程目录里的一项，已附带识别结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteEntry {
    /// 条目 id（在该位置内的路径）。
    pub id: String,
    /// 显示名（未解密的名字）。
    pub name: String,
    /// 是否为目录。
    pub is_dir: bool,
    /// 字节数。
    pub size: Option<u64>,
    /// 是否为 omy 加密文件。
    pub is_encrypted: bool,
    /// 是否已用当前会话的密钥解开。
    pub unlocked: bool,
    /// 解开后的真实文件名。
    pub real_name: Option<String>,
    /// 明文大小。
    pub plaintext_size: Option<u64>,
    /// 探测是否失败（网络原因）。
    ///
    /// 与 `unlocked == false` 是**两回事**：那个是「密码不对」，
    /// 这个是「根本没读到」。界面必须用不同的图标和文案，
    /// 否则用户会去反复试密码而不是检查网络。
    pub probe_failed: bool,
    /// 列表缩略图令牌：仅当已解锁且文件头里确实带缩略图时为 `Some`，
    /// 前端据此请求 `omystream://pthumb/<token>`；否则回退类型图标。
    pub thumb_token: Option<String>,
    /// 是否仍在后台识别中（边扫边出的骨架态）。
    ///
    /// 列目录（PROPFIND）很快、逐文件读头部识别较慢；前端先拿到一屏带
    /// `probing=true` 的骨架立即渲染，后台每识别完一个就用事件推一条最终
    /// 条目（`probing=false`）就地替换。该态与「未能读取」「锁定」都不同，
    /// 必须单独成态，不能让用户对着还没出结果的条目猜密码。
    pub probing: bool,
}

/// 把远程错误映射为结构化错误码。
fn to_cmd_err(e: &RemoteError) -> CmdError {
    let code = match e {
        RemoteError::Unauthorized => "remote_unauthorized",
        RemoteError::Forbidden => "remote_forbidden",
        RemoteError::NotFound(_) => "remote_not_found",
        RemoteError::RateLimited => "remote_rate_limited",
        RemoteError::Unsupported(_) => "remote_unsupported",
        RemoteError::Network(_) => "remote_network",
        _ => "remote_failed",
    };
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

/// 添加一个 WebDAV 位置。
///
/// # Errors
///
/// URL 非法或客户端构造失败时返回。
#[tauri::command]
pub fn remote_place_add(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    name: String,
    url: String,
    username: String,
    password: String,
    vendor: String,
    writable: bool,
) -> CmdResult<String> {
    let cfg = WebDavConfig {
        base_url: url,
        username,
        password,
        vendor: crate::places::parse_vendor(&vendor),
        writable,
        ..WebDavConfig::default()
    };
    reg.add_webdav(name, cfg).map_err(|e| to_cmd_err(&e))
}

/// 列出已注册的远程位置。
#[tauri::command]
pub fn remote_place_list(reg: tauri::State<'_, Arc<PlaceRegistry>>) -> Vec<PlaceInfo> {
    reg.list()
}

/// 移除一个远程位置。
#[tauri::command]
pub fn remote_place_remove(reg: tauri::State<'_, Arc<PlaceRegistry>>, id: String) {
    reg.remove(&id);
}

/// 浏览远程目录，并尝试识别其中的加密文件。
///
/// # Errors
///
/// 位置不存在、网络失败或认证失败时返回。
#[tauri::command]
pub async fn remote_browse(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<RemoteEntry>> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    let items = place.store.list(&dir).await.map_err(|e| to_cmd_err(&e))?;

    // 扫描行为读配置：识别范围（仅 .omy / 所有文件）与并发上限。
    // 每次浏览读一次配置，改完设置无需重启即可生效。
    let scan_cfg = omy_config::Config::load().unwrap_or_default();
    let omy_only = scan_cfg.remote.scan_omy_only;
    let concurrency = scan_cfg.remote.scan_concurrency.clamp(1, 32);

    // 缩略图 token 只服务当前这一屏：开始探测前清掉上一屏登记的头部，
    // 避免反复进出目录让句柄表无限增长，也防止旧 token 串到新列表。
    thumbs.clear();

    // 边扫边出：PROPFIND 列目录很快、逐文件读头部识别较慢。先返回一屏骨架
    // （待探测文件标 probing=true）让界面立刻有内容，后台每识别完一个就 emit
    // 一条最终条目，前端按「当前位置+目录」过滤后就地替换骨架。
    //
    // 探测（含密钥校验与缩略图登记）收敛到 probe_remote_entry 一处，
    // 边扫边出与单条目重试走同一条识别路径，不允许出现两套结果。
    let mut entries: Vec<RemoteEntry> = items
        .iter()
        .map(|it| skeleton_entry(it.id.clone(), it.name.clone(), it.is_dir, it.size, false))
        .collect();

    // 信号量限流：不为每个文件都 spawn 一个不受控请求
    // （那正是设置里「并发请求数」要防的服务端限流）。
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let shared: Arc<crate::state::AppState> = state.inner().clone();
    let thumbs_arc: Arc<PlaceThumbs> = thumbs.inner().clone();

    for (idx, it) in items.iter().enumerate() {
        let size = it.size.unwrap_or(0);
        let too_small = size < omy_core::scan::MIN_FILE_SIZE as u64;
        // 「仅 .omy」模式下，扩展名不是 .omy 的文件连头部都不请求——
        // 远程每个文件都要一次往返，这一项省的就是这个。
        let ext_omy = it.name.to_ascii_lowercase().ends_with(".omy");
        if it.is_dir || too_small || (omy_only && !ext_omy) {
            continue;
        }
        // 该文件骨架进入「识别中」
        if let Some(slot) = entries.get_mut(idx) {
            slot.probing = true;
        }
        let permit = Arc::clone(&sem).acquire_owned();
        let store = Arc::clone(&place.store);
        let base = entries
            .get(idx)
            .cloned()
            .unwrap_or_else(|| skeleton_entry(it.id.clone(), it.name.clone(), it.is_dir, it.size, true));
        let shared = Arc::clone(&shared);
        let thumbs = Arc::clone(&thumbs_arc);
        let app = app.clone();
        let place_id = place_id.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            // 拿到许可才发请求；permit 在任务结束时释放
            let _permit = match permit.await {
                Ok(g) => g,
                Err(_) => return,
            };
            let entry = probe_remote_entry(store.as_ref(), &shared, &thumbs, base).await;
            // 切目录后晚到的事件由前端按 位置+目录 过滤丢弃；这里照常发即可，
            // 多跑的只是几个 480B 的头部请求。
            let _ = app.emit(
                REMOTE_ENTRY_EVENT,
                RemoteEntryEvent {
                    place_id,
                    dir,
                    entry,
                },
            );
        });
    }

    // 立即返回骨架，不等后台识别（结果走 remote-entry 事件）
    Ok(entries)
}

/// 后台识别完一个远程条目时推送的事件名。
pub const REMOTE_ENTRY_EVENT: &str = "remote-entry";

/// `remote-entry` 事件载荷：附带位置与目录，供前端判断是否属于当前这一屏。
#[derive(Debug, Clone, serde::Serialize)]
struct RemoteEntryEvent {
    place_id: String,
    dir: String,
    entry: RemoteEntry,
}

/// 重新探测远程目录里的**单个文件**，供「未能读取」条目就地重试。
///
/// 整屏浏览里某个文件因一次网络抖动被标成 `probe_failed` 时，不该逼用户
/// 重载整个目录（其余文件已识别好）；这个命令只重取该文件头部并重算识别
/// 结果，前端按 `id` 替换对应条目。路径与大小前端列表里都有。
///
/// # Errors
///
/// 位置不存在时返回；单文件读取失败不报错，而是落到 `probe_failed`，
/// 与整屏浏览的状态语义保持一致。
#[tauri::command]
pub async fn remote_probe_entry(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    place_id: String,
    id: String,
    size: u64,
) -> CmdResult<RemoteEntry> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    // 列表里的 name 是路径最后一段；重试只拿到 id（路径），现解一次。
    // rsplit 按字符 '/' 切，不会切坏多字节 UTF-8。
    let name = id
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());
    let base = skeleton_entry(id, name, false, Some(size), false);
    let out = probe_remote_entry(&place.store, state.inner(), thumbs.inner(), base).await;
    Ok(out)
}

/// 一个远程条目骨架：目录天然完整，文件默认全部「待定」。
/// `probing` 仅文件在后台识别中为 true。
fn skeleton_entry(
    id: String,
    name: String,
    is_dir: bool,
    size: Option<u64>,
    probing: bool,
) -> RemoteEntry {
    RemoteEntry {
        id,
        name,
        is_dir,
        size,
        is_encrypted: false,
        unlocked: false,
        real_name: None,
        plaintext_size: None,
        probe_failed: false,
        thumb_token: None,
        probing,
    }
}

/// 拉取单个远程**文件**的完整头部并探测加密状态，就地补全条目字段。
///
/// 这是整屏浏览与单条目重试共用的唯一识别路径：
/// - 读不到头部（网络/权限）→ `probe_failed = true`，**绝不**静默当成普通文件；
/// - 能读到但不是 omy → 保持非加密；
/// - 是 omy 且当前会话密钥能开 → `unlocked`，头部带缩略图则登记轻量句柄。
async fn probe_remote_entry(
    store: &WebDavStore,
    shared: &Shared,
    thumbs: &PlaceThumbs,
    mut e: RemoteEntry,
) -> RemoteEntry {
    let size = e.size.unwrap_or(0);
    match fetch_full_header(store, &e.id, size).await {
        Ok(bytes) => {
            // 必须读到完整头部再试解锁：文件名等 TLV 常使 header_len
            // 超过识别窗，只拿识别窗去 open 会把已解锁文件误判成锁定。
            if let Some((unlocked, real, psize, has_thumb)) = probe_omy(&bytes, shared) {
                e.is_encrypted = true;
                e.unlocked = unlocked;
                e.real_name = real;
                e.plaintext_size = psize;
                // 已解锁且头部带缩略图：头部此刻已在手里，登记一份
                // 轻量句柄即可让列表直接显示缩略图，不再多发一次请求。
                if unlocked && has_thumb {
                    e.thumb_token = thumbs.insert(bytes);
                }
            }
        }
        // 读不到就如实标记，不要静默当成普通文件——那会让用户
        // 以为文件不是加密的，而实际只是这次没读到
        Err(_) => e.probe_failed = true,
    }
    // 走到这里识别已经结束（成功/非加密/失败三态之一），骨架态必须清除，
    // 否则前端会永远停在「识别中」。
    e.probing = false;
    e
}

/// 打开远程 `.omy` 的结果。
///
/// 三种结局必须分开，前端据此给三种不同的界面：
/// - `unlocked=true, token=Some`：可播放，拿 token 去请求 `omystream://pfile/`；
/// - `unlocked=false, not_encrypted=false`：是 omy 但当前会话密码打不开（不是网络问题）；
/// - `not_encrypted=true`：根本不是 omy 文件（远程普通文件，本期只能下载/外部打开）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenPlaceResult {
    /// 播放令牌；仅解锁成功时为 `Some`。
    pub token: Option<String>,
    /// 是否成功解锁。
    pub unlocked: bool,
    /// 是否为非 omy 的普通文件。
    pub not_encrypted: bool,
    /// 解密后的真实文件名。
    pub name: Option<String>,
    /// 预览类别 video/audio/image/text/other。
    pub kind: Option<String>,
    /// 解密内容的 MIME。
    pub mime: Option<String>,
    /// 明文大小。
    pub size: Option<u64>,
    /// 密文大小（任何时候都可见）。
    pub encrypted_size: u64,
}

/// 打开一个远程 `.omy`，登记可播放来源并返回令牌。
///
/// # Errors
///
/// 位置不存在、头部读取失败或来源构造失败时返回结构化错误。
#[tauri::command]
pub async fn remote_place_open(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    files: tauri::State<'_, Arc<PlaceFiles>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    state: tauri::State<'_, Shared>,
    place_id: String,
    path: String,
    size: u64,
) -> CmdResult<OpenPlaceResult> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    // 读完整头部（识别窗口 + 按需补读到 header_len），载荷一个字节都不碰
    let header = fetch_full_header(&store, &path, size)
        .await
        .map_err(|e| to_cmd_err(&e))?;

    let parsed = match omy_core::file::peek_header(&header) {
        Ok(h) => h,
        Err(_) => {
            return Ok(OpenPlaceResult {
                token: None,
                unlocked: false,
                not_encrypted: true,
                name: None,
                kind: None,
                mime: None,
                size: None,
                encrypted_size: size,
            });
        }
    };

    let keks: Vec<Kek> = state
        .with_session(|s| {
            s.all_for(&parsed.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();

    let opened = if keks.is_empty() {
        None
    } else {
        omy_core::file::open(&header, &keks).ok()
    };

    let Some(opened) = opened else {
        // 是 omy 但当前密码集打不开——明确告诉前端是「未解锁」，
        // 不能混进 not_encrypted，否则用户会以为文件没加密
        return Ok(OpenPlaceResult {
            token: None,
            unlocked: false,
            not_encrypted: false,
            name: None,
            kind: None,
            mime: None,
            size: None,
            encrypted_size: size,
        });
    };

    let real_name = opened.filename().ok();
    let plaintext_size = opened.header.plaintext_size;
    let (kind, mime) = match &real_name {
        Some(n) => {
            let (k, m) = crate::mime::by_extension(n);
            (k.to_owned(), m)
        }
        None => (
            crate::mime::kind::OTHER.to_owned(),
            String::from("application/octet-stream"),
        ),
    };

    // RemoteSource 只在构造时 peek 一次头部，之后按 1 MiB 块按需拉载荷。
    // 缓存句柄取全局共享的一份，多个文件共用同一磁盘缓存与上限。
    //
    // 用 tokio 的原始 Handle 而非 tauri::async_runtime::handle()：后者是
    // tauri 自己的 RuntimeHandle 包装，而 RemoteSource 要在同步协议线程里
    // block_on，需要的是具体的 tokio 句柄。本命令本身就跑在 tokio 线程上。
    let rt = tokio::runtime::Handle::current();
    let source = RemoteSource::new(
        Arc::clone(&store),
        place_id.clone(),
        path.clone(),
        &header,
        size,
        cache.snapshot(),
        rt,
    )
    .map_err(|e| {
        CmdError::with("remote_open_failed", serde_json::json!({ "detail": e.to_string() }))
    })?;

    let holder = OpenPlaceFile {
        header: header.clone(),
        source,
        mime: mime.clone(),
    };
    let token = files
        .insert(holder)
        .ok_or_else(|| CmdError::code("remote_open_failed"))?;

    Ok(OpenPlaceResult {
        token: Some(token),
        unlocked: true,
        not_encrypted: false,
        name: real_name,
        kind: Some(kind),
        mime: Some(mime),
        size: Some(plaintext_size),
        encrypted_size: size,
    })
}

/// 关闭一个远程播放来源（播放结束时调用），释放句柄。
#[tauri::command]
pub fn remote_place_close(
    files: tauri::State<'_, Arc<PlaceFiles>>,
    token: String,
) {
    files.remove(&token);
}

/// 每次落盘的明文窗口：远程文件可能是好几 GB 的视频，绝不能像本地还原那样
/// 先把整份明文解进内存；按 1 MiB 窗口「读一段密文→解密→写盘」流式推进。
const LOCAL_WRITE_CHUNK: u64 = 1024 * 1024;

/// 定位一个远程文件所需的三元组：哪个位置、位置内路径、总字节数。
///
/// 多个远程命令都要这三样（打开、解密到本地、未来的写操作），合成一个嵌套
/// 请求结构，既避免 Tauri 命令参数过多，也让前端传参口径统一。嵌套结构走
/// serde 默认的 snake_case，与 `EncryptRequest` 等一致。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RemoteFileRef {
    pub place_id: String,
    pub path: String,
    pub size: u64,
}

/// 「解密到本地」结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteDecryptResult {
    /// 最终写出的明文文件绝对路径。
    pub saved_path: String,
    /// 明文文件名（取自头部 TLV，已做路径清洗）。
    pub name: String,
    /// 写出的明文字节数。
    pub bytes: u64,
}

/// 把一个已解锁的远程 `.omy` **流式解密到本地目录**。
///
/// 这是只读位置也保留的主要用途（见原型能力矩阵）：服务器上始终只有密文，
/// 明文只在本机生成。复用播放链路同一套 `RemoteSource`（块对齐 + 密文缓存）
/// 与 `read_source_range`（header 偏移 / 压缩索引 / 解密），不允许出现第二套
/// 解密读取实现。
///
/// 能力安全边界：前端只对「已解锁单文件」放出口子，但这里仍独立校验——
/// 未解锁、非加密、加密文件夹（容器）一律拒绝，devtools 直接 invoke 也绕不过。
#[tauri::command]
pub async fn remote_decrypt_to_local(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    state: tauri::State<'_, Shared>,
    req: RemoteFileRef,
    dest_dir: String,
) -> CmdResult<RemoteDecryptResult> {
    let RemoteFileRef { place_id, path, size } = req;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    let header = fetch_full_header(&store, &path, size)
        .await
        .map_err(|e| to_cmd_err(&e))?;
    let parsed = omy_core::file::peek_header(&header)
        .map_err(|_| CmdError::code("not_an_omy_file"))?;

    let keks: Vec<Kek> = state
        .with_session(|s| {
            s.all_for(&parsed.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();
    if keks.is_empty() {
        return Err(CmdError::code("locked"));
    }

    // 先 open 一次做能力判定与取名/大小；真正解密在阻塞线程里再 open 一次
    // （OpenedFile 不长期持有，与 omystream 每请求重开一致）。
    let probe = omy_core::file::open(&header, &keks).map_err(|_| CmdError::code("locked"))?;
    // 容器（加密文件夹）需要整份索引后解压，无法走单文件流式，本期明确拒绝，
    // 不静默产出一个打不开的东西
    if probe.is_container() {
        return Err(CmdError::code("remote_container_unsupported"));
    }
    let raw_name = probe
        .filename()
        .map_err(|_| CmdError::code("decrypt_failed"))?;
    // 文件名来自加密文件内部 TLV，是不可信输入，必须清洗，否则一个构造的
    // TLV（如 ../../x）就能把明文写到目标目录之外
    let safe = omy_core::unpack::sanitize_filename(&raw_name);
    let total = probe.header.plaintext_size;

    let dest = std::path::PathBuf::from(&dest_dir);
    let final_path = dest.join(&safe);
    // 默认不覆盖，与本地还原一致
    if final_path.exists() {
        return Err(CmdError::code("target_exists"));
    }
    // 先写 .part 再原子改名：中途失败/取消不会留下一个看起来完整的半成品
    let part_path = dest.join(format!("{safe}.part"));

    let cache_snap = cache.snapshot();
    let rt = tokio::runtime::Handle::current();
    let app2 = app.clone();
    let name_for_event = safe.clone();
    // 闭包会拿走 part_path 的所有权，错误清理另留一份
    let part_cleanup = part_path.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<RemoteDecryptResult, String> {
        // RemoteSource 的读方法内部用保存的 Handle block_on 网络请求，
        // 必须在阻塞线程里调（async 线程里 block_on 当前 runtime 会 panic）
        let source = RemoteSource::new(
            store,
            place_id,
            path,
            &header,
            size,
            cache_snap,
            rt,
        )
        .map_err(|e| e.to_string())?;
        let opened = omy_core::file::open(&header, &keks).map_err(|_| "locked".to_string())?;

        use std::io::Write;
        let _ = std::fs::remove_file(&part_path);
        let mut out = std::fs::File::create(&part_path).map_err(|_| "write_failed".to_string())?;

        let mut done: u64 = 0;
        let mut last_pct: Option<u8> = None;
        while done < total {
            let want = (total - done).min(LOCAL_WRITE_CHUNK);
            let chunk = omy_core::source::read_source_range(&source, &opened, done, want)
                .map_err(|_| "decrypt_failed".to_string())?;
            if chunk.is_empty() {
                return Err("decrypt_failed".to_string());
            }
            out.write_all(&chunk).map_err(|_| "write_failed".to_string())?;
            done = done.saturating_add(chunk.len() as u64);

            // 按整百分比节流，否则大文件发几千次事件，IPC 反而拖慢传输
            let pct = u8::try_from(done.saturating_mul(100).checked_div(total).unwrap_or(100))
                .unwrap_or(100);
            if last_pct != Some(pct) {
                last_pct = Some(pct);
                let _ = app2.emit(
                    DECRYPT_PROGRESS_EVENT,
                    DecryptProgress {
                        index: 1,
                        total_files: 1,
                        name: name_for_event.clone(),
                        done,
                        total,
                    },
                );
            }
        }
        out.sync_all().map_err(|_| "write_failed".to_string())?;
        drop(out);

        // rename 前再查一次存在性，堵住并发下的覆盖
        if final_path.exists() {
            let _ = std::fs::remove_file(&part_path);
            return Err("target_exists".to_string());
        }
        std::fs::rename(&part_path, &final_path).map_err(|_| "write_failed".to_string())?;
        Ok(RemoteDecryptResult {
            saved_path: final_path.to_string_lossy().into_owned(),
            name: name_for_event,
            bytes: done,
        })
    })
    .await;

    match join {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(code)) => {
            let _ = std::fs::remove_file(&part_cleanup);
            Err(CmdError::code(&code))
        }
        Err(_) => {
            let _ = std::fs::remove_file(&part_cleanup);
            Err(CmdError::code("decrypt_failed"))
        }
    }
}

/// 读到足以 `open` 的完整文件头。
///
/// 识别窗口（前 `MIN_PROBE_SIZE` 字节）通常已覆盖头部；带缩略图/压缩索引的
/// 文件头部更长，此时按 `peek_header` 给出的 `header_len` 补读，直到覆盖
/// 完整 TLV 与头部 MAC。载荷依旧一个字节都不下载。
async fn fetch_full_header(
    store: &WebDavStore,
    path: &str,
    size: u64,
) -> Result<Vec<u8>, RemoteError> {
    let probe_len = size.min(omy_core::scan::MIN_PROBE_SIZE as u64);
    let mut buf = if probe_len == 0 {
        Vec::new()
    } else {
        store.read_range(path, 0, probe_len).await?
    };

    if let Ok(h) = omy_core::file::peek_header(&buf) {
        let need = u64::from(h.header_len);
        if need <= size && (buf.len() as u64) < need {
            let extra = store
                .read_range(path, buf.len() as u64, need - buf.len() as u64)
                .await?;
            buf.extend_from_slice(&extra);
        }
    }
    Ok(buf)
}

/// 远程缓存用量，供设置页显示进度条。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteCacheUsage {
    /// 已用字节。
    pub used: u64,
    /// 上限字节，0 表示不限。
    pub limit: u64,
    /// 缓存根目录（「打开缓存目录」用）。
    pub root: Option<String>,
}

/// 查询远程密文缓存用量。
#[tauri::command]
pub fn remote_cache_usage(cache: tauri::State<'_, Arc<RemoteCache>>) -> RemoteCacheUsage {
    RemoteCacheUsage {
        used: cache.used(),
        limit: cache.limit(),
        root: cache.root().map(|p| p.display().to_string()),
    }
}

/// 立即清空远程缓存，返回清空后占用（应为 0）。
#[tauri::command]
pub fn remote_cache_clear(cache: tauri::State<'_, Arc<RemoteCache>>) -> u64 {
    cache.clear()
}

/// 缓存设置变更后重建缓存（改上限或自定义目录）。
#[tauri::command]
pub fn remote_cache_apply(
    cache: tauri::State<'_, Arc<RemoteCache>>,
    limit: u64,
    cache_dir: Option<String>,
) {
    cache.reload(RemoteCache::resolve_root(cache_dir), limit);
}

/// 在系统文件管理器里打开缓存目录。移动端没有可浏览的外部目录，明确不支持。
#[tauri::command]
pub fn remote_cache_open_dir(cache: tauri::State<'_, Arc<RemoteCache>>) -> CmdResult<()> {
    let Some(root) = cache.root() else {
        return Err(CmdError::code("remote_cache_unavailable"));
    };
    open_in_file_manager(&root)
}

/// 调起系统文件管理器并定位到目录。只 spawn 不等退出码——
/// explorer.exe 即使成功也常返回非零，wait 会误判成失败。
#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| CmdError::with("open_failed", serde_json::json!({ "detail": e.to_string() })))
}

#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| CmdError::with("open_failed", serde_json::json!({ "detail": e.to_string() })))
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| CmdError::with("open_failed", serde_json::json!({ "detail": e.to_string() })))
}

#[cfg(target_os = "android")]
fn open_in_file_manager(_path: &std::path::Path) -> CmdResult<()> {
    Err(CmdError::code("remote_unsupported"))
}

/// 用会话里的密钥尝试识别并解开一段文件头。
///
/// 返回 `None` 表示不是 omy 文件。元组末项表示文件头里是否带缩略图
/// （仅在已解锁时才有意义）。
fn probe_omy(
    bytes: &[u8],
    state: &crate::commands::Shared,
) -> Option<(bool, Option<String>, Option<u64>, bool)> {
    let header = omy_core::file::peek_header(bytes).ok()?;

    // 能解析出头部就说明是 omy 文件，即便打不开
    let keks = state.with_session(|s| {
        s.all_for(&header.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect::<Vec<_>>()
    });

    let Some(keks) = keks else {
        return Some((false, None, None, false));
    };
    if keks.is_empty() {
        return Some((false, None, None, false));
    }

    // open 只访问 data[..header_len]，所以传头部字节就够——
    // 这正是远程不必下载整个文件也能显示真实文件名的原因
    match omy_core::file::open(bytes, &keks) {
        Ok(opened) => {
            let name = opened.filename().ok();
            let has_thumb = opened.thumbnail().is_ok();
            Some((true, name, Some(opened.header.plaintext_size), has_thumb))
        }
        // 解不开是正常情况：文件可能属于另一个密码集
        Err(_) => Some((false, None, None, false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 错误码要能区分认证、限流与网络，而不是一律「失败」。
    ///
    /// 不这样会怎样：界面只能显示「操作失败」，而这三种情况用户
    /// 该做的事完全不同——重新登录、等一会、检查网络。
    #[test]
    fn error_codes_are_distinguishable() {
        assert_eq!(to_cmd_err(&RemoteError::Unauthorized).code, "remote_unauthorized");
        assert_eq!(to_cmd_err(&RemoteError::RateLimited).code, "remote_rate_limited");
        assert_eq!(
            to_cmd_err(&RemoteError::Network(String::from("x"))).code,
            "remote_network"
        );
        assert_eq!(
            to_cmd_err(&RemoteError::NotFound(String::from("a"))).code,
            "remote_not_found"
        );
    }

    /// 序列化字段名是前端契约，改名等于改协议。
    ///
    /// 不这样会怎样：前端读 `probe_failed` 得到 undefined，
    /// 网络失败的条目会被当成「密码不对」显示，用户去反复试密码。
    #[test]
    fn entry_fields_are_stable() {
        let e = RemoteEntry {
            id: String::from("/a.omy"),
            name: String::from("a.omy"),
            is_dir: false,
            size: Some(1000),
            is_encrypted: true,
            unlocked: false,
            real_name: None,
            plaintext_size: None,
            probe_failed: true,
            thumb_token: None,
            probing: false,
        };
        let j = serde_json::to_value(&e).expect("序列化");
        for k in [
            "id", "name", "is_dir", "size", "is_encrypted", "unlocked", "real_name",
            "plaintext_size", "probe_failed", "thumb_token", "probing",
        ] {
            assert!(j.get(k).is_some(), "字段 {k} 不能改名或缺失");
        }
    }

    /// 非 omy 数据必须返回 None，而不是误判成加密文件。
    ///
    /// 不这样会怎样：普通文件被标成加密文件，界面给出解密入口，
    /// 点下去必然失败。
    #[test]
    fn non_omy_bytes_are_not_encrypted() {
        let state: crate::commands::Shared =
            std::sync::Arc::new(crate::state::AppState::new());
        let junk = vec![0x41u8; 600];
        assert!(probe_omy(&junk, &state).is_none(), "随机数据不该被当成 omy");
        // 太短的数据同样不该误判
        assert!(probe_omy(b"OMY", &state).is_none());
    }
}
