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
use crate::place_files::{PlaceContainers, OpenPlaceFile, PlaceFiles, PlaceThumbs, RemoteCache};
use crate::places::{PlaceInfo, PlaceRegistry};
use omy_core::crypto::Kek;
use omy_remote::source::RemoteSource;
use omy_remote::webdav::WebDavConfig;
use omy_remote::{Error as RemoteError, PlaceStore, RemoteStore};

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
///
/// # 为什么 CDN 重定向要单拎一条
///
/// 它也是 `Unsupported`，落到通用的 `remote_unsupported` 上，用户看到的是
/// 「该位置不支持此操作」——既不说是哪个操作，也不说能怎么办，是个死胡同。
/// 而这件事对用户其实有明确的下一步（改用官方客户端下载），值得一句自己的话。
fn to_cmd_err(e: &RemoteError) -> CmdError {
    let code = match e {
        RemoteError::Unauthorized => "remote_unauthorized",
        RemoteError::Forbidden => "remote_forbidden",
        RemoteError::NotFound(_) => "remote_not_found",
        RemoteError::RateLimited => "remote_rate_limited",
        RemoteError::Unsupported(what) if *what == CDN_REDIRECT => "tg_cdn_unsupported",
        RemoteError::Unsupported(what) if *what == BROADCAST_NO_MESSAGES => {
            "tg_broadcast_no_messages"
        }
        RemoteError::Unsupported(_) => "remote_unsupported",
        RemoteError::Network(_) => "remote_network",
        _ => "remote_failed",
    };
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

/// `read_range` 撞到 CDN 重定向时给出的标记。
///
/// 与 `omy_remote::telegram::store` 里那个 `Unsupported` 的参数必须逐字相同。
/// 做成常量并由两边共用，是因为靠字面量对暗号的写法会在改动一侧时静默失效——
/// 而失效的表现只是「文案退回通用错误」，没有任何报错。
const CDN_REDIRECT: &str = omy_remote::telegram::store::CDN_REDIRECT;

/// 广播频道不提供消息视图时的标记。与 omy-remote 里那个共用同一个常量，
/// 理由同 [`CDN_REDIRECT`]：两边各写一份字面量会在改动一侧时静默失效。
const BROADCAST_NO_MESSAGES: &str = omy_remote::telegram::store::BROADCAST_NO_MESSAGES;

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
    let id = reg.add_webdav(name, cfg).map_err(|e| to_cmd_err(&e))?;
    // 立刻落盘：用户加完位置就可能直接关掉应用，等到退出再存会丢。
    //
    // 存不上不让整个添加失败——位置在本次会话里是可用的，只是重启后
    // 要重加。那比「加了半天说失败了、其实连接是好的」体验好。
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 远程位置未能保存：{e}");
    }
    Ok(id)
}

/// 列出已注册的远程位置。
#[tauri::command]
pub fn remote_place_list(reg: tauri::State<'_, Arc<PlaceRegistry>>) -> Vec<PlaceInfo> {
    reg.list()
}

/// 本机凭据保护是否可用。
///
/// 设置页据此显示「密码已加密保存在本机」还是「这台机器无法安全保存
/// 密码，每次启动需重新输入」。
#[tauri::command]
#[must_use]
pub fn remote_secret_status() -> crate::places::SecretStatus {
    PlaceRegistry::secret_status()
}

/// 移除一个远程位置。
#[tauri::command]
pub fn remote_place_remove(reg: tauri::State<'_, Arc<PlaceRegistry>>, id: String) {
    reg.remove(&id);
    // 同样立刻落盘，否则删掉的位置重启后又回来了
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 远程位置未能保存：{e}");
    }
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
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<RemoteEntry>> {
    // 未连接的 Telegram 占位在这里自动连上。
    //
    // 从配置恢复出来的位置是未连接占位（恢复流程刻意不碰网络，否则应用会卡在
    // 启动那一刻）。不在这里补连的话，用户点「进入」只会看到一句
    // 「找不到该远程位置」——而那句话是错的：位置就在那儿，只是还没连。
    crate::telegram_cmds::ensure_connected(&reg, &place_id).await?;

    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    let items = place.store.list(&dir).await.map_err(|e| to_cmd_err(&e))?;
    Ok(scan_entries(
        &app,
        &state,
        &thumbs,
        &place,
        &place_id,
        &dir,
        items,
        cache.snapshot(),
    ))
}

/// 把一批远程条目变成「骨架 + 后台识别」的结果。
///
/// `remote_browse` 与 `remote_search` 共用这一份。搜索另写一份的话，结果里就
/// 不会有 omy 识别、缩略图与锁定态——表现是「浏览时能看出哪些是加密文件，
/// 一搜索就全成了普通文件」，而这恰恰是用户最需要在搜索结果里看到的信息。
#[allow(clippy::too_many_arguments)]
fn scan_entries(
    app: &tauri::AppHandle,
    state: &tauri::State<'_, crate::commands::Shared>,
    thumbs: &tauri::State<'_, Arc<PlaceThumbs>>,
    place: &Arc<crate::places::Place>,
    place_id: &str,
    dir: &str,
    items: Vec<omy_remote::store::Entry>,
    // 头部缓存的一份快照，分发给每个后台识别任务。
    // `BlockCache` 的克隆共享同一根目录，所以并发任务之间是真共享
    cache: Option<omy_remote::cache::BlockCache>,
) -> Vec<RemoteEntry> {
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
        let place_id = place_id.to_owned();
        let dir = dir.to_owned();
        let cache = cache.clone();
        let place_for_probe = place_id.clone();
        tokio::spawn(async move {
            // 拿到许可才发请求；permit 在任务结束时释放
            let _permit = match permit.await {
                Ok(g) => g,
                Err(_) => return,
            };
            let entry = probe_remote_entry(
                store.as_ref(),
                &shared,
                &thumbs,
                base,
                cache.as_ref(),
                &place_for_probe,
            )
            .await;
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
    entries
}

/// 在服务端搜索。
///
/// # 这个命令会把搜索词发到服务端
///
/// 与 `remote_browse` 分开而不是加一个参数，正是为了让这件事在调用点上就
/// 看得见：前端调的是哪个命令，决定了要不要给用户那条「搜索词已发送」的提示。
/// 合成一个命令加 flag 的话，界面很容易在某条路径上忘了提示，
/// 而用户不会知道自己搜的词出去了。
///
/// # 只对声明了 `search` 能力的位置可用
///
/// 不支持的位置在这里就拒绝，而不是发出去等服务端报错——本地目录和网盘根本
/// 没有这个概念，界面上那个分段控件也不该出现。
///
/// # Errors
///
/// 位置不存在、不支持搜索、网络失败或被限流时返回。
#[tauri::command]
// 八个形参里有五个是 Tauri 注入的（AppHandle 与四个 State），前端只传
// place_id / dir / query 三个。这条 lint 数的是形参个数，在注入式框架里
// 量错了对象——真按它去收结构体的话，反而要动前端契约。
#[allow(clippy::too_many_arguments)]
pub async fn remote_search(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    dir: String,
    query: String,
) -> CmdResult<Vec<RemoteEntry>> {
    crate::telegram_cmds::ensure_connected(&reg, &place_id).await?;

    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    // 能力位图是唯一真相：不支持搜索就当场拒绝，不要发出去让服务端报错
    if !place.store.capabilities().search {
        return Err(CmdError::code("remote_no_search"));
    }

    // 条数由后端定，不开放给前端。
    //
    // 开放的话前端可以传一个很大的数，而每一条结果都要跟一次头部识别请求——
    // 既慢又容易撞服务端限流。用户在结果屏上也看不完那么多。
    let items = place
        .store
        .search(&dir, &query, SEARCH_LIMIT)
        .await
        .map_err(|e| to_cmd_err(&e))?;

    // 搜索结果同样要走识别：否则搜出来的加密文件看不出是加密文件。
    //
    // dir 用搜索时的 dir 原样传下去，让前端的「位置+目录」过滤仍然成立；
    // 跨对话搜索时 dir 为空，事件也就归到空目录那一屏，与发起搜索的那屏一致。
    Ok(scan_entries(
        &app,
        &state,
        &thumbs,
        &place,
        &place_id,
        &dir,
        items,
        cache.snapshot(),
    ))
}

/// 一次搜索最多返回多少条。
///
/// 不是「越多越好」：结果屏用户看不完，而每一条都要跟一次头部识别请求，
/// 条数上去之后既慢又容易撞服务端限流。
const SEARCH_LIMIT: usize = 100;

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
// 八个形参里有四个是 Tauri 注入的 State，前端一个都传不到（它只传
// place_id / id / size / name 四个）。这条 lint 数的是形参个数，
// 在注入式框架里量错了对象——真按它去收结构体的话，反而要动前端契约。
#[allow(clippy::too_many_arguments)]
pub async fn remote_probe_entry(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    id: String,
    size: u64,
    name: Option<String>,
) -> CmdResult<RemoteEntry> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    // 名字优先用调用方给的——它列表里本来就有。
    //
    // 早先这里一律从 id 按 '/' 切最后一段，那是**写死了 WebDAV 的路径形状**。
    // Telegram 的 id 形如 `tg:<对话>:<消息号>`，里面没有斜杠，于是整个 id 被
    // 当成文件名，界面上显示成「tg:-1003929965717:10」。加密文件平时看不出来
    // （显示的是解出来的真名），但 plain 模式的文件本来就没有加密文件名、
    // 要靠磁盘名显示，一走这条路径就露馅。
    //
    // 不在这里加 Telegram 特判：那会让「id 长什么样」这件事散落到各处，
    // 加第三个 provider 时又要改一遍。让调用方传名字才是正解。
    let name = name.unwrap_or_else(|| {
        id.rsplit('/')
            .find(|seg| !seg.is_empty())
            .map_or_else(|| id.clone(), str::to_string)
    });
    let base = skeleton_entry(id, name, false, Some(size), false);
    // 重试仍然走缓存：识别失败时 fetch_full_header 在报错路径上**不写缓存**，
    // 所以「未能读取」的条目重试时必然是真的重新请求，不会命中一份坏结果
    let out = probe_remote_entry(
        &place.store,
        state.inner(),
        thumbs.inner(),
        base,
        cache.snapshot().as_ref(),
        &place_id,
    )
    .await;
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
    store: &PlaceStore,
    shared: &Shared,
    thumbs: &PlaceThumbs,
    mut e: RemoteEntry,
    // 头部缓存。识别是整条浏览路径上最贵的一步（实测约 1.5 秒/个且第二次
    // 一样慢），不传缓存的话每次进目录都要把所有文件的头部重下一遍
    cache: Option<&omy_remote::cache::BlockCache>,
    place_id: &str,
) -> RemoteEntry {
    let size = e.size.unwrap_or(0);
    match fetch_full_header(store, &e.id, size, cache, place_id).await {
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

/// 上传一批本地文件到远程目录。
///
/// # 用的是磁盘上的文件名，不是解密后的真名
///
/// 这不是偷懒。用户开了文件名加密，磁盘上就是那串密文名；omy 若
/// 「贴心地」用解密后的真名上传，**等于把加密掉的文件名主动交给服务端**，
/// 前面的加密全白做。原名只显示给用户自己看。
///
/// # 逐个上传而不是并发
///
/// 并发会同时开多个到同一服务端的请求，在限流敏感的 Telegram 上很容易
/// 换来一次 `FLOOD_WAIT`，结果是整体更慢而不是更快。这与 `prefetch_all`
/// 的取舍一致。
///
/// # 一个失败不影响其余
///
/// 逐个记录成败后一起返回。中途 `?` 掉的话，前 N 个已经传上去了而调用方
/// 只看到一个错误，界面上会显示成「全部失败」——与事实不符，
/// 而用户再传一次就会产生重复文件。
///
/// # Errors
///
/// 位置不存在、或目标目录不可写时返回。单个文件的失败不算错误，
/// 在返回值里如实标注。
#[tauri::command]
pub async fn remote_upload(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    place_id: String,
    dir: String,
    paths: Vec<String>,
) -> CmdResult<Vec<UploadOutcome>> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    // 先问能力位图。不问的话，只读对话上会发一次注定失败的请求，
    // 错误还只有一句 provider 的原始报错
    let caps = store.effective_capabilities(&dir).await.map_err(|e| to_cmd_err(&e))?;
    if !caps.write {
        return Err(CmdError::code("remote_readonly"));
    }

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let path = std::path::PathBuf::from(&p);
        // 磁盘文件名 —— 见函数文档，绝不能换成解密后的真名
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("file"));

        let data = match tokio::fs::read(&path).await {
            Ok(d) => d,
            Err(e) => {
                out.push(UploadOutcome {
                    path: p,
                    name,
                    ok: false,
                    id: None,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };

        match store.write(&dir, &name, &data).await {
            Ok(entry) => out.push(UploadOutcome {
                path: p,
                name,
                ok: true,
                id: Some(entry.id),
                error: None,
            }),
            Err(e) => out.push(UploadOutcome {
                path: p,
                name,
                ok: false,
                id: None,
                error: Some(e.to_string()),
            }),
        }
    }
    Ok(out)
}

/// 单个文件的上传结果。
///
/// 成败逐个返回而不是整体一个布尔：部分成功是常态（网络抖动、个别文件
/// 超限），压成一个布尔的话界面只能说「失败了」，用户不知道哪些传上去了，
/// 再传一次就产生重复文件。
#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadOutcome {
    /// 本地路径（前端据此对应回它自己的列表）。
    pub path: String,
    /// 上传时用的名字（磁盘名）。
    pub name: String,
    /// 成功与否。
    pub ok: bool,
    /// 成功时的远程条目 id。
    pub id: Option<String>,
    /// 失败原因。
    pub error: Option<String>,
}

/// 取当前远程目录里所有加密文件的 vault 参数（用于解锁）。
///
/// # 为什么需要这条命令
///
/// 顶栏的密码入口走 `unlock_directory`，它要一个**本地目录**去探测 vault。
/// 远程位置没有本地目录，于是 `tryUnlock` 第一行的 `if (!state.cwd)` 直接
/// 返回——用户在远程位置里输密码**什么都不会发生，连错误都没有**，
/// 看到的是「密码输了没反应，文件一直锁着」。
///
/// 局域网那条线早就有对应的 `remote_vaults`，云盘这条漏了。
///
/// 去重按 salt：同一个 vault 里所有文件的 salt 相同，重复派生纯属浪费
/// （Argon2 每次都要几百毫秒）。
///
/// # Errors
///
/// 位置不存在时返回。读不到头部的条目直接跳过而不是报错——
/// 那些文件的失败已经由 `probe_failed` 表达过了。
#[tauri::command]
pub async fn remote_place_vaults(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<crate::commands::VaultParams>> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    let entries = store
        .list(&dir)
        .await
        .map_err(|e| to_cmd_err(&e))?;

    let cache_snap = cache.snapshot();
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for e in entries {
        if e.is_dir {
            continue;
        }
        let size = e.size.unwrap_or(0);
        // 只读头部，载荷一个字节都不碰。
        //
        // 这条路径与浏览用的是同一批头部：解锁时走到这里，多半刚刚才浏览过
        // 同一个目录，走缓存就不必把那十几个头部再下一遍
        let Ok(bytes) =
            fetch_full_header(store.as_ref(), &e.id, size, cache_snap.as_ref(), &place_id).await
        else {
            continue;
        };
        let Ok(h) = omy_core::file::peek_header(&bytes) else {
            continue;
        };
        let salt = crate::commands::hex_of(&h.vault_salt);
        if seen.insert(salt.clone()) {
            out.push(crate::commands::VaultParams {
                salt,
                m_kib: h.argon2_m_kib,
                t: h.argon2_t,
                p: h.argon2_p,
            });
        }
    }
    Ok(out)
}

/// 「要打开哪个远程条目」。
///
/// 四个字段描述的是同一件事，收成结构体而不是平铺成参数。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTarget {
    /// 远程位置 id。
    pub place_id: String,
    /// 条目 id（WebDAV 是路径，Telegram 是 `tg:<对话>:<消息>`）。
    pub path: String,
    /// 条目字节数。
    pub size: u64,
    /// 条目显示名。
    ///
    /// 普通文件靠它推 MIME——Telegram 的 id 里没有扩展名，
    /// 只看 id 的话每个文件都会被判成 `application/octet-stream`，
    /// 图片视频统统不预览。
    pub name: Option<String>,
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
    target: OpenTarget,
) -> CmdResult<OpenPlaceResult> {
    // 收成一个结构体而不是四个平铺参数：它们本来就是一组——
    // 都在描述「要打开哪个条目」。顺带让参数个数回到 clippy 的限制内，
    // 那个限制在这里提示的是真实问题，不该用 allow 盖过去。
    let OpenTarget {
        place_id,
        path,
        size,
        name,
    } = target;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    // 读完整头部（识别窗口 + 按需补读到 header_len），载荷一个字节都不碰。
    // 走缓存：打开一个文件之前基本都先浏览过它所在的目录，那时头部已经取过
    let header = fetch_full_header(
        store.as_ref(),
        &path,
        size,
        cache.snapshot().as_ref(),
        &place_id,
    )
    .await
    .map_err(|e| to_cmd_err(&e))?;

    let parsed = match omy_core::file::peek_header(&header) {
        Ok(h) => h,
        Err(_) => {
            // 不是 omy 文件 —— 在远程位置里这是**正常情况而不是错误**。
            //
            // Telegram 位置的全部意义就是「把频道里的文件、图片、视频当远程
            // 文件用」，普通文件才是主体内容。原先这里直接返回「不是加密文件」
            // 且不给 token，于是界面上所有普通文件都打不开、右键也没有菜单
            // （菜单要求条目可激活）。
            //
            // 普通文件不需要解密，按原始字节转发即可。
            // 用调用方给的名字推 MIME。不能只看 id：Telegram 的 id 形如
            // `tg:<对话>:<消息>`，里面根本没有扩展名，光靠它每个文件都会被判成
            // application/octet-stream，于是图片视频统统不预览。
            let label = name.clone().unwrap_or_else(|| path.clone());
            let (kind, mime) = crate::mime::by_extension(&label);
            let rt = tokio::runtime::Handle::current();
            let source = RemoteSource::new_plain(
                Arc::clone(&store),
                place_id.clone(),
                path.clone(),
                size,
                cache.snapshot(),
                rt,
            );
            let token = files.insert(OpenPlaceFile {
                header: Vec::new(),
                plain: Some(size),
                source,
                mime: mime.clone(),
            });
            return Ok(OpenPlaceResult {
                token,
                unlocked: true,
                not_encrypted: true,
                name: None,
                kind: Some(kind.to_owned()),
                mime: Some(mime),
                size: Some(size),
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
        plain: None,
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

    let header = fetch_full_header(
        store.as_ref(),
        &path,
        size,
        cache.snapshot().as_ref(),
        &place_id,
    )
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

/// 仅取头部、构造一个远程源，供只需要文件结构而不解密明文的缓存操作使用。
///
/// 与播放/解密不同，这里不需要 KEK：缓存里只有密文，统计覆盖情况、删除本地
/// 密文块都不触及明文，因此锁定（未解锁）的文件也允许查/移除它的缓存。
///
/// # 普通文件同样适用
///
/// 远程位置里普通文件是主体内容（Telegram 频道里的图片、视频、文档），
/// 它们走同一套块缓存，也同样值得永久保留——离线看视频正是最典型的场景。
/// 原先这里对非 omy 文件一律返回 `not_an_omy_file`，于是 pin / unpin /
/// 缓存统计 / 清除四条命令**全都拒绝普通文件**，界面上那几个菜单项
/// 对它们永远不出现。
async fn build_remote_source(
    reg: &tauri::State<'_, Arc<PlaceRegistry>>,
    cache: &tauri::State<'_, Arc<RemoteCache>>,
    req: &RemoteFileRef,
) -> CmdResult<RemoteSource<PlaceStore>> {
    let place = reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);
    let header = fetch_full_header(
        store.as_ref(),
        &req.path,
        req.size,
        cache.snapshot().as_ref(),
        &req.place_id,
    )
    .await
    .map_err(|e| to_cmd_err(&e))?;
    let rt = tokio::runtime::Handle::current();

    // 是 omy 就按 omy 建（载荷从 header_len 起算），否则按普通文件建
    // （整个文件都是载荷）。判据是头部能不能解析，与「解不解得开」无关——
    // 缓存操作本来就不需要密码。
    match RemoteSource::new(
        Arc::clone(&store),
        req.place_id.clone(),
        req.path.clone(),
        &header,
        req.size,
        cache.snapshot(),
        rt.clone(),
    ) {
        Ok(src) => Ok(src),
        Err(_) => Ok(RemoteSource::new_plain(
            store,
            req.place_id.clone(),
            req.path.clone(),
            req.size,
            cache.snapshot(),
            rt,
        )),
    }
}

/// 列出一个**远程**目录容器里的条目。
///
/// # 与本地 `list_container` 的分工
///
/// 索引解析、条目映射、MIME 推导全部复用 `commands::items_from_index`——
/// 那段逻辑与密文从哪来无关，写第二份迟早漂移。这里只负责把远程那份头部
/// 取过来、并按远程句柄登记 token。
///
/// # 只读头部，不下载载荷
///
/// 索引在 TLV 区，读头部就够。这正是远程容器能「进去看看」而不必先拉下
/// 几百 MB 的原因。
///
/// # 参数为什么是句柄而不是位置 id + 路径
///
/// 容器明文不在磁盘上、要按需解，而 `remote_place_open` 颁发的句柄背后正好
/// 挂着已经建好的远程源与密文缓存。顺带也让「句柄失效 → 容器 token 跟着
/// 失效」自动成立。
///
/// # Errors
///
/// 句柄不存在、当前会话打不开、或这个文件不是目录容器时返回。
#[tauri::command]
pub async fn remote_list_container(
    state: tauri::State<'_, Shared>,
    files: tauri::State<'_, Arc<PlaceFiles>>,
    containers: tauri::State<'_, Arc<PlaceContainers>>,
    token: String,
) -> CmdResult<Vec<crate::commands::ContainerItem>> {
    let Some(f) = files.get(&token) else {
        return Err(CmdError::code("remote_file_closed"));
    };
    let header = f.header.clone();
    let shared: Shared = Arc::clone(&state);
    let idx = tauri::async_runtime::spawn_blocking(move || {
        let h = omy_core::file::peek_header(&header).ok()?;
        let keks: Vec<omy_core::crypto::Kek> = shared.with_session(|s| {
            s.all_for(&h.vault_salt).into_iter().map(|c| c.kek).collect()
        })?;
        let opened = omy_core::file::open(&header, &keks).ok()?;
        opened.folder_index().ok()
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    let Some(idx) = idx else {
        return Err(CmdError::code("not_a_container"));
    };

    let mut items = crate::commands::items_from_index(&idx);
    for it in &mut items {
        if let (Some(off), Some(size)) = (it.offset, it.size) {
            it.token = containers.register(crate::place_files::PlaceContainerRef {
                file_token: token.clone(),
                inner_path: it.path.clone(),
                offset: off,
                size,
                mime: it.mime.clone(),
            });
        }
    }
    Ok(items)
}

/// 列一个对话里的消息（以文件为主线的消息视图）。
///
/// # 范围
///
/// 这**不是**聊天客户端：只读、不发消息、不做回复关系 / 转发链 / reactions /
/// 已读状态（文档 §1.3 把它们列在 out of scope）。它的用途是让用户按时间线
/// 找文件——「我记得上周发过一个东西」。
///
/// # 广播频道没有消息视图
///
/// 见 `TelegramStore::messages` 上关于 ToS 3.3 的说明。这里把那个标记映射成
/// 专门的错误码，好让界面给一句能看懂的话而不是笼统的「不支持此操作」。
///
/// # Errors
///
/// 位置不存在、不是 Telegram、是广播频道、网络失败或限流时返回。
#[tauri::command]
pub async fn remote_messages(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<omy_remote::telegram::store::MessageRow>> {
    crate::telegram_cmds::ensure_connected(&reg, &place_id).await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        // 只有 Telegram 有「消息」这个概念。WebDAV 上没有，
        // 界面也不该显示那个切换——这里是最后一道
        return Err(CmdError::code("remote_no_messages"));
    };
    tg.messages(&dir, MESSAGE_PAGE)
        .await
        .map_err(|e| to_cmd_err(&e))
}

/// 消息视图一次拉多少条。
///
/// 不是「全部」：活跃对话里可能有上万条，全拉既慢又白占限流配额，
/// 而用户在时间线上一次也看不完这么多。
const MESSAGE_PAGE: usize = 80;

/// 查询单个远程文件在本地密文块缓存里的覆盖情况（不下载载荷）。
#[tauri::command]
pub async fn remote_cache_file_stat(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<omy_remote::cache::FileCacheStat> {
    let source = build_remote_source(&reg, &cache, &req).await?;
    Ok(source.cache_stat())
}

/// 删除单个远程文件的本地密文块，返回释放字节数。
///
/// 只读位置也允许：它只清理本机缓存，绝不向云端发任何写请求。
#[tauri::command]
pub async fn remote_cache_remove_file(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<serde_json::Value> {
    let source = build_remote_source(&reg, &cache, &req).await?;
    let freed_bytes = source.remove_cached_blocks();
    Ok(serde_json::json!({ "freed_bytes": freed_bytes }))
}

/// 查询某个远程目录下的**有效能力**。
///
/// 与 `remote_place_list` 给出的位置级能力不是一回事：那是**上界**，这是「在这个
/// 目录里实际能做什么」。界面必须读这一个——位置级能力对「对话即目录」的位置
/// 只是上界，照它渲染会在只读对话里点亮必然失败的删除与上传。
///
/// # Errors
///
/// 位置不存在，或驱动为查权限发起的请求失败时返回。**前端收到错误时不要回落到
/// 位置级能力**：那恰好是把上界当成实际能力用，等于这层收窄没做。按只读处理。
#[tauri::command]
pub async fn remote_effective_caps(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    place_id: String,
    dir: String,
) -> CmdResult<omy_remote::Capabilities> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    place
        .store
        .effective_capabilities(&dir)
        .await
        .map_err(|e| to_cmd_err(&e))
}

/// 头部缓存键里的固定块号。
///
/// 正文按 1 MiB 分块、块号从 0 递增；头部永远只读文件开头那一小段
/// （至多 `MIN_PROBE_SIZE` = 480 B，再按 `header_len` 补一点），
/// 所以它在自己的键空间里只占 0 这一块。
const HEADER_BLOCK: u64 = 0;

/// 头部读取的缓存键。
///
/// 与正文块**共用同一个 `BlockCache`，但键空间互不相交**：正文键是
/// `<id>\u{1}<版本哈希>`（omy 文件）或 `<id>\u{1}plain<size>`（普通文件），
/// 头部键是 `<id>\u{1}hdr<size>`。
///
/// # 为什么键里要含 size
///
/// 同名文件在云端被**覆盖更新**后长度多半会变，键随之变化，旧头部自然失效，
/// 不会把上一版的识别结果一直显示下去。长度恰好不变的覆盖会命中旧头部，
/// 这是与 `RemoteSource::new_plain` 一致的**已知取舍**：为这种少数情况让
/// 每次进目录都重下全部头部，代价是每次十几秒，不划算。
fn header_cache_key(id: &str, size: u64) -> String {
    // 用控制字符分隔，正常 id（WebDAV 路径或 `tg:<对话>:<消息>`）里不会出现。
    // 与 RemoteSource::cache_key 的分隔符保持一致
    format!("{id}\u{1}hdr{size}")
}

/// 读到足以 `open` 的完整文件头，**优先走密文块缓存**。
///
/// 识别窗口（前 `MIN_PROBE_SIZE` 字节）通常已覆盖头部；带缩略图/压缩索引的
/// 文件头部更长，此时按 `peek_header` 给出的 `header_len` 补读，直到覆盖
/// 完整 TLV 与头部 MAC。载荷依旧一个字节都不下载。
///
/// # 为什么必须缓存
///
/// 实测识别一个文件头约 1.5 秒（一到两次网络往返），而且**第二次一样慢**
/// （1689ms → 1449ms）。十几个文件的对话每次进去都要重等十几秒——用户报的
/// 「每次进来都要重新加载」就是这条。
///
/// 原先这里直接调 `store.read_range`，**完全绕过** `BlockCache`：同一个文件
/// 的同一段字节，播放时走缓存、识别时不走。顺带排除过另外两个候选——连接是
/// 复用的（首次 1518ms、再连 2ms，没有重新握手），列目录约 400ms，都不是瓶颈。
///
/// # 为什么可以放进同一个缓存
///
/// 头部与正文块是**同一个安全模型**：缓存目录里只有密文，整个拷走也解不开。
/// 放进去不引入新的暴露面，而另开一套缓存要把 LRU、永久层、淘汰豁免那些
/// 踩过坑的逻辑重写一遍。
///
/// `cache` 为 `None` 时退化成每次都走网络——那是缓存目录建不起来时的既有
/// 降级行为，不该让识别本身失败。
///
/// 泛型而不是写死 `&PlaceStore`：这样单测能传一个记录请求次数的假存储，
/// 真的验证「第二次不再发请求」。写死具体类型的话，唯一能测的就只有
/// 键的拼法，而缓存有没有真的接上去测不到——那恰恰是本函数的全部意义。
async fn fetch_full_header<S: RemoteStore>(
    store: &S,
    path: &str,
    size: u64,
    cache: Option<&omy_remote::cache::BlockCache>,
    place_id: &str,
) -> Result<Vec<u8>, RemoteError> {
    let key = header_cache_key(path, size);

    // size 为 0 的条目没有任何字节可读，也没必要为它写一个空缓存块
    if size > 0 {
        if let Some(c) = cache {
            if let Some(hit) = c.get(place_id, &key, HEADER_BLOCK) {
                return Ok(hit);
            }
        }
    }

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

    // 补读之后才写缓存：存半截头部的话，下次命中会拿到一段不完整的字节，
    // 而 open 会把它报成「文件损坏」——那个症状完全指不到缓存
    if size > 0 {
        if let Some(c) = cache {
            c.put(place_id, &key, HEADER_BLOCK, &buf);
        }
    }
    Ok(buf)
}

/// 远程缓存用量，供设置页显示进度条。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteCacheUsage {
    /// 临时层已用字节。**不含永久层**。
    pub used: u64,
    /// 临时层上限字节，0 表示不限。
    pub limit: u64,
    /// 缓存根目录（「打开缓存目录」用）。
    pub root: Option<String>,
    /// 永久层已用字节。
    ///
    /// 与 `used` 分开报，因为它**不计入 `limit`、也不参与淘汰**——这正是
    /// 永久缓存存在的意义。合成一个数字的话，界面只能显示一个「已用」，
    /// 用户会以为清空缓存能把它降下去，而永久层清不掉。
    pub pinned_used: u64,
    /// 永久层里有多少个文件。
    ///
    /// 永久层没有分母（不受上限约束），所以界面只能显示绝对值与文件数，
    /// 不能像临时层那样画一个百分比条。
    pub pinned_files: u64,
}

/// 把一个远程文件转为**永久缓存**。
///
/// # 这是一次真实的下载任务
///
/// 产品上「转为永久」不是打个标记就完事：它要把整个文件的密文块都取到本地，
/// 否则「永久」只是承诺而不是事实——下次离线打开照样失败。所以这个命令会
/// 真的把所有块拉下来，耗时与文件大小成正比。
///
/// 返回搬进永久层的字节数。
///
/// # Errors
///
/// 位置不存在、不是 omy 文件、网络失败时返回。
#[tauri::command]
pub async fn remote_cache_pin(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<u64> {
    let source = build_remote_source(&reg, &cache, &req).await?;
    // 先确保内容真的在本地：pin 只搬运已有的块，没有的块搬不了。
    // 不预热的话，「转为永久」会变成「把已经缓存的那几块标成永久」，
    // 而用户以为整个文件都留下来了——直到离线时才发现不是。
    tokio::task::block_in_place(|| source.prefetch_all())
        .map_err(|_| CmdError::code("remote_prefetch_failed"))?;
    source.pin().map_err(|e| to_cmd_err(&e))
}

/// 取消永久缓存。
///
/// 内容**搬回临时层**而不是原地改个标记——回到临时层就意味着重新计入 `limit`、
/// 重新参与 LRU，可能很快被清掉。那正是用户点「取消永久」想要的语义；
/// 只改标记的话空间不会真的还回来。
///
/// # Errors
///
/// 位置不存在或不是 omy 文件时返回。
#[tauri::command]
pub async fn remote_cache_unpin(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<u64> {
    let source = build_remote_source(&reg, &cache, &req).await?;
    source.unpin().map_err(|e| to_cmd_err(&e))
}

/// 查询远程密文缓存用量。
#[tauri::command]
pub fn remote_cache_usage(cache: tauri::State<'_, Arc<RemoteCache>>) -> RemoteCacheUsage {
    let (pinned_used, pinned_files) = cache
        .snapshot()
        .map_or((0, 0), |c| (c.pinned_used(), c.pinned_file_count()));
    RemoteCacheUsage {
        pinned_used,
        pinned_files,
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

    /// CDN 重定向要有自己的错误码，不能落到通用的「不支持此操作」。
    ///
    /// 不这样会怎样：用户看到一句「该位置不支持此操作」——既不说是哪个操作，
    /// 也不说能怎么办，是个死胡同。而这件事其实有明确的下一步（改用官方客户端
    /// 下载），值得一句自己的话。
    ///
    /// 这条映射的失效方式很安静：两侧字面量只要有一处改动，CDN 错误就会退回
    /// 通用码，界面上只是文案变笼统，不会有任何报错。
    #[test]
    fn cdn_redirect_gets_its_own_error_code() {
        let e = to_cmd_err(&RemoteError::Unsupported(CDN_REDIRECT));
        assert_eq!(
            e.code, "tg_cdn_unsupported",
            "CDN 重定向应当有专门的错误码，实际是 {}",
            e.code
        );

        // 其他 Unsupported 仍然走通用码——否则这条特判就成了「所有不支持都
        // 说成 CDN」，那是另一种误导
        let other = to_cmd_err(&RemoteError::Unsupported("something else"));
        assert_eq!(other.code, "remote_unsupported");
    }

    /// 常量必须与 omy-remote 里那个逐字相同。
    ///
    /// 不这样会怎样：两边各写一份字面量，改一侧不会有编译错误，
    /// 上面那条断言却仍然通过（它用的是同一个常量）——测试全绿而功能已坏。
    /// 所以这里直接断言常量的**值**，让改动必须同时改到这里。
    #[test]
    fn cdn_marker_matches_the_remote_crate() {
        assert_eq!(CDN_REDIRECT, "telegram cdn redirect");
    }
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

    /// 记录每次 `read_range` 的假存储，用来数「到底发了几次请求」。
    ///
    /// 缓存这类改动只有数请求次数才验得出来：断言「返回的字节对不对」的话，
    /// 一个完全不接缓存的实现照样全绿。
    struct CountingStore {
        data: Vec<u8>,
        calls: std::sync::Mutex<u32>,
    }

    impl CountingStore {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                calls: std::sync::Mutex::new(0),
            }
        }
        fn calls(&self) -> u32 {
            self.calls.lock().map(|c| *c).unwrap_or(u32::MAX)
        }
    }

    impl omy_remote::RemoteStore for CountingStore {
        fn capabilities(&self) -> omy_remote::Capabilities {
            omy_remote::Capabilities::read_only()
        }
        fn describe(&self) -> String {
            String::from("counting")
        }
        async fn list(
            &self,
            _dir_id: &str,
        ) -> Result<Vec<omy_remote::store::Entry>, RemoteError> {
            Ok(Vec::new())
        }
        async fn read_range(
            &self,
            _id: &str,
            offset: u64,
            len: u64,
        ) -> Result<Vec<u8>, RemoteError> {
            if let Ok(mut c) = self.calls.lock() {
                *c = c.saturating_add(1);
            }
            let s = usize::try_from(offset).unwrap_or(usize::MAX);
            let e = usize::try_from(offset.saturating_add(len)).unwrap_or(usize::MAX);
            Ok(self
                .data
                .get(s..e.min(self.data.len()))
                .unwrap_or(&[])
                .to_vec())
        }
    }

    /// 造一个临时缓存目录，返回（缓存, 目录路径）。
    fn temp_cache(tag: &str) -> (omy_remote::cache::BlockCache, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "omy_hdrcache_{tag}_{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        let c = match omy_remote::cache::BlockCache::new(&dir, 0) {
            Ok(c) => c,
            Err(e) => unreachable!("建缓存失败：{e}"),
        };
        (c, dir)
    }

    fn rt() -> tokio::runtime::Runtime {
        match tokio::runtime::Builder::new_current_thread().build() {
            Ok(r) => r,
            Err(e) => unreachable!("建运行时失败：{e}"),
        }
    }

    /// 同一个文件头读第二次**不能**再发网络请求。
    ///
    /// 不这样会怎样：这正是用户报的「每次进来都要重新加载」。实测识别一个
    /// 头部约 1.5 秒且第二次一样慢，十几个文件就是十几秒，每次进对话重来
    /// 一遍。原先 `fetch_full_header` 直连 `store.read_range`，绕过了播放
    /// 路径在用的 `BlockCache`。
    ///
    /// 这条断言数的是**请求次数**而不是返回内容：只断言「读回来的字节对」
    /// 的话，一个把缓存整个摘掉的实现照样通过——变异测试确认过，那样的断言
    /// 等于没写。
    #[test]
    fn a_second_header_read_does_not_hit_the_network() {
        let store = CountingStore::new(vec![0x7Au8; 4096]);
        let (cache, dir) = temp_cache("hit");
        let r = rt();

        let first = r
            .block_on(fetch_full_header(&store, "/a.omy", 4096, Some(&cache), "p1"))
            .unwrap_or_default();
        let after_first = store.calls();
        assert!(after_first > 0, "首次必须真的发请求");
        assert!(!first.is_empty(), "首次要读到内容");

        let second = r
            .block_on(fetch_full_header(&store, "/a.omy", 4096, Some(&cache), "p1"))
            .unwrap_or_default();
        assert_eq!(
            store.calls(),
            after_first,
            "第二次读同一个头部不该再发请求——缓存没接上"
        );
        assert_eq!(second, first, "缓存命中返回的内容必须与首次一致");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 文件被覆盖更新（长度变了）后，**不能**返回旧头部。
    ///
    /// 不这样会怎样：缓存键若不含 size，云端换了文件之后界面仍然显示上一版的
    /// 识别结果——文件名、是否加密、明文大小全是旧的，而且因为一直命中缓存
    /// 永远不会自愈。
    #[test]
    fn a_changed_size_invalidates_the_cached_header() {
        let old = CountingStore::new(vec![0x11u8; 4096]);
        let new = CountingStore::new(vec![0x22u8; 8192]);
        let (cache, dir) = temp_cache("size");
        let r = rt();

        let a = r
            .block_on(fetch_full_header(&old, "/a.omy", 4096, Some(&cache), "p1"))
            .unwrap_or_default();
        // 同一个 id、同一个位置，但文件长度变了
        let b = r
            .block_on(fetch_full_header(&new, "/a.omy", 8192, Some(&cache), "p1"))
            .unwrap_or_default();

        assert!(new.calls() > 0, "长度变了必须重新请求，不能吃旧缓存");
        assert_ne!(a, b, "返回的必须是新文件的头部");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 不同位置的同名条目不能互相串。
    ///
    /// 不这样会怎样：两个 Telegram 账号里各有一条 `tg:123:456`，缓存键若不含
    /// 位置 id，A 账号会显示 B 账号那个文件的识别结果——而两边看起来都「正常」，
    /// 没有任何报错。
    #[test]
    fn different_places_do_not_share_header_cache() {
        let a_store = CountingStore::new(vec![0x33u8; 4096]);
        let b_store = CountingStore::new(vec![0x44u8; 4096]);
        let (cache, dir) = temp_cache("place");
        let r = rt();

        let a = r
            .block_on(fetch_full_header(&a_store, "tg:1:2", 4096, Some(&cache), "p1"))
            .unwrap_or_default();
        let b = r
            .block_on(fetch_full_header(&b_store, "tg:1:2", 4096, Some(&cache), "p2"))
            .unwrap_or_default();

        assert!(b_store.calls() > 0, "另一个位置必须自己去取，不能命中前一个的缓存");
        assert_ne!(a, b, "不同位置的同名条目内容不能串");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 头部键与正文块键不能撞。
    ///
    /// 不这样会怎样：头部只有几百字节、正文块是 1 MiB，两者共用一个键的话，
    /// 播放时会把一段 480 B 的头部当成第 0 个正文块拿去解密，得到认证失败——
    /// 而那个症状指向密钥，完全指不到缓存键。
    #[test]
    fn header_keys_never_collide_with_payload_keys() {
        let k = header_cache_key("/a.omy", 4096);
        // 正文键的两种形态（见 RemoteSource）：`<id>\u{1}<hex哈希>` 与
        // `<id>\u{1}plain<size>`。头部键必须与它们都不同
        assert_ne!(k, "/a.omy\u{1}plain4096");
        assert!(k.contains("\u{1}hdr"), "头部键要有自己的前缀：{k}");
        // size 必须真的进键里，否则覆盖更新后旧头部不会失效
        assert_ne!(
            header_cache_key("/a.omy", 4096),
            header_cache_key("/a.omy", 8192),
            "长度不同必须得到不同的键"
        );
    }

    /// 没有缓存时要照常工作（缓存目录建不起来是既有的降级路径）。
    ///
    /// 不这样会怎样：把「没有缓存」写成错误的话，一台缓存目录不可写的机器上
    /// 整个远程浏览都会瘫掉，而它本来只该慢一点。
    #[test]
    fn header_read_works_without_a_cache() {
        let store = CountingStore::new(vec![0x55u8; 4096]);
        let r = rt();
        let out = r
            .block_on(fetch_full_header(&store, "/a.omy", 4096, None, ""))
            .unwrap_or_default();
        assert!(!out.is_empty(), "没有缓存也要能读到头部");
        assert!(store.calls() > 0);
    }
}
