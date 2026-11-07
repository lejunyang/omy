//! 列目录、翻页、搜索与逐项探测：识别加密文件只读开头几百字节，不下载载荷。

use super::*;

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
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;

    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    let t0 = std::time::Instant::now();
    let items = match place.store.list(&dir).await {
        Ok(v) => v,
        Err(e) => {
            crate::applog::warn(
                "browse",
                &format!(
                    "list place={place_id} dir={} failed: {}",
                    crate::applog::redact(&dir),
                    to_cmd_err(&e).code
                ),
            );
            return Err(to_cmd_err(&e));
        }
    };
    crate::applog::info(
        "browse",
        &format!(
            "list place={place_id} dir={} -> {} entries in {}ms",
            crate::applog::redact(&dir),
            items.len(),
            t0.elapsed().as_millis()
        ),
    );
    let entries = scan_entries(
        &app,
        &state,
        &thumbs,
        &place,
        &place_id,
        &dir,
        items,
        cache.snapshot(),
    );

    // 对话列表（根目录）的头像后台补齐。
    //
    // list("") 已经把对话结构返回了（头像先空着），这里把待下的头像交给后台
    // 逐个下、下完一个就通过 remote-entry 事件把那张卡片的图标补上——对齐官方
    // 「列表先出、头像后补」。不在 list 里同步下是因为那会让列表等所有头像
    // 到手才返回（72 个约 11s）。
    //
    // 只在**根目录**（dir 为空）且是 Telegram 位置时做：对话内层是文件、
    // 走的是另一条缩略图路径。
    if dir.is_empty()
        && let Some(tg) = place.store.as_telegram()
    {
            let jobs = tg.take_pending_avatars();
            if !jobs.is_empty() {
                spawn_avatar_backfill(
                    app.clone(),
                    Arc::clone(&place.store),
                    thumbs.inner().clone(),
                    place_id.clone(),
                    jobs,
                );
            }
        }

    Ok(entries)
}

/// 后台把对话头像逐个下好，下完一个就发 remote-entry 事件让前端替换图标。
///
/// 限流到 8 路（与文件缩略图同一个理由：别为 72 个头像同时打满请求触发
/// 服务端限流）。用户离开该列表后晚到的事件由前端按「位置+目录」过滤丢弃，
/// 与文件缩略图完全同一条通道、同一套丢弃规则，不新造机制。
pub(super) fn spawn_avatar_backfill(
    app: tauri::AppHandle,
    store: Arc<omy_remote::PlaceStore>,
    thumbs: Arc<PlaceThumbs>,
    place_id: String,
    jobs: Vec<(i64, omy_remote::telegram::store::ChatPhoto)>,
) {
    tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(8));
        let mut set = tokio::task::JoinSet::new();
        for (chat, photo) in jobs {
            let sem = Arc::clone(&sem);
            let store = Arc::clone(&store);
            let thumbs = Arc::clone(&thumbs);
            let place_id = place_id.clone();
            set.spawn(async move {
                let disk_key = format!("avatar:{place_id}:{chat}");
                // 先查磁盘：头像上次落过盘就直接读盘、不发网络（重启后仍有头像）
                if let Some(cached) = thumbs.disk_image(&disk_key) {
                    let tg = store.as_telegram()?;
                    tg.set_conversation_avatar(chat, Some(cached.clone()));
                    return Some((chat, Some(cached)));
                }
                let _permit = sem.acquire_owned().await.ok()?;
                let tg = store.as_telegram()?;
                let bytes = tg.download_avatar(&photo).await;
                // 写回对话缓存（内存），让本次会话内 refresh 命中、不重下
                tg.set_conversation_avatar(chat, bytes.clone());
                // 头像是 Telegram 侧公开信息，落盘可接受；重启后从盘恢复
                if let Some(b) = &bytes {
                    thumbs.persist_image(&disk_key, b);
                }
                Some((chat, bytes))
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok(Some((chat, bytes))) = joined else {
                continue;
            };
            // 没下到就不发事件：卡片保持类型图标即可，不必推一条空的
            let Some(bytes) = bytes else { continue };
            let token = thumbs.insert_image(bytes);
            // 发**完整**的对话条目，不是只带 token——前端按 id 整体替换
            // （state.remoteItems[idx] = entry），只带 token 会把标题清空。
            // 从 store 取回这个对话的标题等，构成和 list 里一致的条目。
            let Some(tg) = store.as_telegram() else {
                continue;
            };
            let Some(conv) = tg.conversation(chat) else {
                continue;
            };
            let mut entry = skeleton_entry(
                format!("tg:{chat}"),
                conv.title.clone(),
                true,
                None,
                None,
                false,
                None,
            );
            entry.thumb_token = token;
            let _ = app.emit(
                REMOTE_ENTRY_EVENT,
                RemoteEntryEvent {
                    place_id: place_id.clone(),
                    // 根目录的 dir 就是空串，与前端 state.remoteDir 对得上
                    dir: String::new(),
                    entry,
                },
            );
        }
    });
}

/// 两级缩略图的第二级：后台把媒体卡片的糊占位图升级为清晰图。
///
/// 列目录时每个媒体条目已带 stripped 占位（秒出但糊）。这里对每个条目
/// 后台取一次清晰档（thumb_full = get_messages_by_id + 下载可渲染尺寸），
/// 下好就发 remote-entry 让前端把那张卡片的图换成清晰的。
///
/// 限流 8 路、与头像/识别同一条事件通道、同一套「切走后前端按位置+目录
/// 丢弃」规则，不新造机制。取不到清晰档就不发事件——占位图留着即可，
/// 不倒退成类型图标。
pub(super) fn spawn_thumb_upgrade(
    app: tauri::AppHandle,
    store: Arc<omy_remote::PlaceStore>,
    thumbs: Arc<PlaceThumbs>,
    place_id: String,
    dir: String,
    // (文件 id, 名字, 大小)——发事件时要还原成完整条目
    jobs: Vec<(String, String, Option<u64>, Option<i64>)>,
) {
    tokio::spawn(async move {
        let sem = Arc::new(tokio::sync::Semaphore::new(8));
        let mut set = tokio::task::JoinSet::new();
        for (id, name, size, mtime) in jobs {
            let sem = Arc::clone(&sem);
            let store = Arc::clone(&store);
            let thumbs = Arc::clone(&thumbs);
            set.spawn(async move {
                // 先查磁盘：清晰缩略图上次落过盘的话，直接读盘、不发网络请求。
                // 这就是「重启后仍是清晰图、不重新拉」的关键——纯内存表重启即空，
                // 磁盘这一层跨会话保留。
                if let Some(cached) = thumbs.disk_image(&id) {
                    return Some((id, name, size, mtime, cached));
                }
                let _permit = sem.acquire_owned().await.ok()?;
                let tg = store.as_telegram()?;
                let bytes = tg.thumb_full(&id).await?;
                // 拉回来的清晰图写盘，供下次（含重启后）直接命中
                thumbs.persist_image(&id, &bytes);
                Some((id, name, size, mtime, bytes))
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok(Some((id, name, size, mtime, bytes))) = joined else {
                continue;
            };
            // insert_image 内容寻址：清晰图与占位图内容不同 → 新 token，
            // 前端换 src 即从糊变清；万一服务端给的清晰档和占位同字节（不会），
            // token 相同也无害
            // insert_image 可能返回 None（空字节/登记失败）：那种情况不发事件，
            // 占位图留着即可，不要清成类型图标
            let Some(token) = thumbs.insert_image(bytes) else {
                continue;
            };
            let mut entry = skeleton_entry(id, name, false, size, mtime, false, None);
            entry.thumb_token = Some(token);
            let _ = app.emit(
                REMOTE_ENTRY_EVENT,
                RemoteEntryEvent {
                    place_id: place_id.clone(),
                    dir: dir.clone(),
                    entry,
                },
            );
        }
    });
}

/// 把一批远程条目变成「骨架 + 后台识别」的结果。
///
/// `remote_browse` 与 `remote_search` 共用这一份。搜索另写一份的话，结果里就
/// 不会有 omy 识别、缩略图与锁定态——表现是「浏览时能看出哪些是加密文件，
/// 一搜索就全成了普通文件」，而这恰恰是用户最需要在搜索结果里看到的信息。
#[allow(clippy::too_many_arguments)]
pub(super) fn scan_entries(
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

    // 只清上一屏登记的 omy 文件头 token（每屏递增、必须清否则无限涨），
    // **保留图片 token**（头像/内嵌缩略图，已内容寻址）。
    // 全清会让当前显示的头像在新列表到达前那几百毫秒 404、闪成文件夹图标
    // ——这正是用户报的「刷新时其他群图标闪回文件夹」。图片内容寻址后，
    // 同一张头像刷新前后是同一 token，不清即不闪。
    thumbs.clear_headers();

    // 边扫边出：PROPFIND 列目录很快、逐文件读头部识别较慢。先返回一屏骨架
    // （待探测文件标 probing=true）让界面立刻有内容，后台每识别完一个就 emit
    // 一条最终条目，前端按「当前位置+目录」过滤后就地替换骨架。
    //
    // 探测（含密钥校验与缩略图登记）收敛到 probe_remote_entry 一处，
    // 边扫边出与单条目重试走同一条识别路径，不允许出现两套结果。
    let mut entries: Vec<RemoteEntry> = items
        .iter()
        .map(|it| {
            let mut e = skeleton_entry(
                it.id.clone(),
                it.name.clone(),
                it.is_dir,
                it.size,
                it.mtime,
                false,
                it.media_tab.map(str::to_string),
            );
            // 服务端随消息送来的内嵌缩略图：**列目录时就已经在手里**，
            // 不需要任何额外请求，也不必等后台识别。
            //
            // 后台那条识别路径是给 omy 加密文件用的（要读头部、要解密），
            // 普通图片视频根本不走它——早先只在那条路径上登记缩略图，
            // 于是普通文件的 thumb_token 恒为 null，界面上永远只有类型图标。
            if let Some(bytes) = it.thumb.clone() {
                e.thumb_token = thumbs.insert_image(bytes);
            }
            e
        })
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
        let base = entries.get(idx).cloned().unwrap_or_else(|| {
            skeleton_entry(
                it.id.clone(),
                it.name.clone(),
                it.is_dir,
                it.size,
                it.mtime,
                true,
                it.media_tab.map(str::to_string),
            )
        });
        let shared = Arc::clone(&shared);
        let thumbs = Arc::clone(&thumbs_arc);
        let app = app.clone();
        let place_id = place_id.to_owned();
        let dir = dir.to_owned();
        let cache = cache.clone();
        let stable_source = PlaceRegistry::place_source(place);
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
                stable_source.as_ref(),
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
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;

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
pub(super) const SEARCH_LIMIT: usize = 100;

/// 后台识别完一个远程条目时推送的事件名。
pub const REMOTE_ENTRY_EVENT: &str = "remote-entry";

/// `remote-entry` 事件载荷：附带位置与目录，供前端判断是否属于当前这一屏。
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct RemoteEntryEvent {
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
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let stable_source = PlaceRegistry::place_source(&place);

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
    let base = skeleton_entry(id, name, false, Some(size), None, false, None);
    // 重试仍然走缓存：识别失败时 fetch_full_header 在报错路径上**不写缓存**，
    // 所以「未能读取」的条目重试时必然是真的重新请求，不会命中一份坏结果
    let out = probe_remote_entry(
        &place.store,
        state.inner(),
        thumbs.inner(),
        base,
        cache.snapshot().as_ref(),
        &place_id,
        stable_source.as_ref(),
    )
    .await;
    Ok(out)
}

/// 一个远程条目骨架：目录天然完整，文件默认全部「待定」。
/// `probing` 仅文件在后台识别中为 true。
pub(super) fn skeleton_entry(
    id: String,
    name: String,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<i64>,
    probing: bool,
    // 所在分栏。由调用方从 omy_remote Entry 透传；对话骨架等非文件场景给 None。
    media_tab: Option<String>,
) -> RemoteEntry {
    RemoteEntry {
        id,
        name,
        is_dir,
        size,
        mtime,
        is_encrypted: false,
        unlocked: false,
        real_name: None,
        plaintext_size: None,
        probe_failed: false,
        thumb_token: None,
        probing,
        media_tab,
    }
}

/// 拉取单个远程**文件**的完整头部并探测加密状态，就地补全条目字段。
///
/// 这是整屏浏览与单条目重试共用的唯一识别路径：
/// - 读不到头部（网络/权限）→ `probe_failed = true`，**绝不**静默当成普通文件；
/// - 能读到但不是 omy → 保持非加密；
/// - 是 omy 且当前会话密钥能开 → `unlocked`，头部带缩略图则登记轻量句柄。
pub(super) async fn probe_remote_entry(
    store: &PlaceStore,
    shared: &Shared,
    thumbs: &PlaceThumbs,
    mut e: RemoteEntry,
    // 头部缓存。识别是整条浏览路径上最贵的一步（实测约 1.5 秒/个且第二次
    // 一样慢），不传缓存的话每次进目录都要把所有文件的头部重下一遍
    cache: Option<&omy_remote::cache::BlockCache>,
    place_id: &str,
    stable_source: Option<&omy_remote::virtuals::SourceRef>,
) -> RemoteEntry {
    let size = e.size.unwrap_or(0);
    match fetch_full_header(store, &e.id, size, cache, place_id, stable_source).await {
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

/// 列一个对话里的文件，支持翻页。
///
/// `before` 为 `None` 是首屏；给了消息号就是「加载更多」——从那条之前
/// 再取一页。
///
/// # 为什么要分页
///
/// 固定拉一页的话，活跃群里**第 N+1 个文件之后永远看不到，而且界面上
/// 没有任何迹象**，用户会以为那些文件不在这个群里。
///
/// # Errors
///
/// 位置不存在、不是 Telegram 位置、或列目录失败时返回。
#[tauri::command]
// Tauri 命令的参数就是它的接口：每个 State 都是一次依赖注入。
// 打包成结构体只会多一层壳，反而更难看出这个命令依赖什么。
// place_cmds 里另外三处（scan_entries 等）出于同样理由也这么标
#[allow(clippy::too_many_arguments)]
pub async fn remote_browse_more(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    dir: String,
    before: i32,
) -> CmdResult<Vec<RemoteEntry>> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        // 只有「对话即目录」的位置有这种分页。WebDAV 的 PROPFIND 一次列全
        return Err(CmdError::code("remote_no_messages"));
    };
    let items = tg
        .list_more(&dir, Some(before))
        .await
        .map_err(|e| to_cmd_err(&e))?;
    // 走与首屏**完全相同**的 scan_entries，不另写一份识别流程——
    // 两份迟早不一致，而表现会是「翻页之后 omy 文件突然不识别了」
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

/// 按**媒体分栏**列一个对话（媒体/文件/链接/音频/GIF），服务端 filter 分页。
///
/// 与 remote_browse_more 的区别：那条无过滤列「所有文件」，这条按栏用服务端
/// 类型索引，各栏独立分页。`tab` 是前端传的短标识（media/file/link/audio/gif），
/// `before`=0 表示首屏、否则是上一页最后一条消息号；`limit` 由前端按视口算。
///
/// # Errors
///
/// 位置不存在、不是 Telegram 位置、或列目录失败时返回。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn remote_browse_tab(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    place_id: String,
    dir: String,
    tab: String,
    before: i32,
    limit: usize,
) -> CmdResult<Vec<RemoteEntry>> {
    use omy_remote::telegram::store::MediaTab;
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        return Err(CmdError::code("remote_no_messages"));
    };
    let media_tab = MediaTab::from_key(&tab);
    // before=0 当作首屏（消息号从 1 起，0 是"无游标"的哨兵，前端传 0 即首屏）
    let cursor = if before > 0 { Some(before) } else { None };
    // limit 兜底：前端估错或传 0 时给个合理默认，别把 limit=0 发给服务端
    let lim = if limit == 0 { 30 } else { limit.min(100) };
    let t0 = std::time::Instant::now();
    let items = tg
        .list_tab(&dir, media_tab, cursor, lim)
        .await
        .map_err(|e| to_cmd_err(&e))?;
    crate::applog::info(
        "browse-tab",
        &format!(
            "place={place_id} dir={} tab={tab} before={before} -> {} entries in {}ms",
            crate::applog::redact(&dir),
            items.len(),
            t0.elapsed().as_millis()
        ),
    );
    // 媒体/GIF 栏的条目有真实缩略图，值得后台升级为清晰图（两级缩略图第二级）。
    // 文件/链接/音频栏多半没有可升级的缩略图，就不多发这批请求。
    if matches!(media_tab, MediaTab::Media | MediaTab::Gif) {
        let jobs: Vec<(String, String, Option<u64>, Option<i64>)> = items
            .iter()
            .filter(|it| !it.is_dir && it.thumb.is_some())
            .map(|it| (it.id.clone(), it.name.clone(), it.size, it.mtime))
            .collect();
        if !jobs.is_empty() {
            spawn_thumb_upgrade(
                app.clone(),
                Arc::clone(&place.store),
                thumbs.inner().clone(),
                place_id.clone(),
                dir.clone(),
                jobs,
            );
        }
    }
    // 走与首屏完全相同的 scan_entries：识别 .omy、登记缩略图、边扫边出的
    // 后台识别都复用同一条路，不另写一份（否则翻页/换栏后 omy 识别会不一致）
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

/// 一页多少条。界面据它判断还有没有更多。
#[tauri::command]
#[must_use]
pub const fn remote_page_size() -> usize {
    omy_remote::telegram::store::TelegramStore::page_size()
}

/// 当前目录（对话）是否开了「受保护内容」。
///
/// 界面据此显示一行低权重告知。**不是警告**：实测 `noforwards` 拦的是
/// 转发，不拦 `upload.getFile`，读取与播放都正常（DEC-20）。用户需要
/// 知道的是「这个群开了保护，但不影响你在这里打开文件」。
///
/// 非 Telegram 位置一律 `false`——那些位置没有这个概念。
#[tauri::command]
pub async fn remote_dir_protected(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    dir: String,
) -> CmdResult<bool> {
    // 先确保连上。冷启动时 session 恢复是惰性的，store 里还没有 client，
    // 于是对话表既查不到也刷新不了，只能答 false——而那正是告知最该出现
    // 的时候（用户从配置恢复后直接进了某个受保护的群）。
    //
    // 实测过这个差别：冷启动直接问受保护的群得到 false，
    // 先 browse 一次再问才得到 true。界面上碰巧总是先 browse，
    // 但命令本身要自洽，不能要求调用方记住这个顺序。
    if crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
        .await
        .is_err()
    {
        // 连不上就不显示告知。这只是一行提示，不该因为它报错
        return Ok(false);
    }
    let Some(place) = reg.get(&place_id) else {
        // 位置不在了就不显示告知，而不是报错：这只是一行提示，
        // 不该因为它挡住整个目录
        return Ok(false);
    };
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        return Ok(false);
    };
    Ok(tg.dir_protected(&dir).await)
}

/// 用会话里的密钥尝试识别并解开一段文件头。
///
/// 返回 `None` 表示不是 omy 文件。元组末项表示文件头里是否带缩略图
/// （仅在已解锁时才有意义）。
pub(super) fn probe_omy(
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
