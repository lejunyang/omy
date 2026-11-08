//! 远程文件的本地缓存：统计、常驻（pin）、清理与缓存目录。

use super::*;

/// 查询单个远程文件在本地密文块缓存里的覆盖情况（不下载载荷）。
#[tauri::command]
pub async fn remote_cache_file_stat(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<omy_remote::cache::FileCacheStat> {
    if !has_complete_pinned_candidate(&reg, &cache, &req.place_id, &req.path, req.size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &req.place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
    let source = build_remote_source(&reg, &cache, &req).await?;
    Ok(source.cache_stat())
}

/// 批量查一批远程文件的缓存状态。
///
/// 返回与 `reqs` 等长的数组，顺序一一对应；某一项查不到时给全零的
/// 占位而不是整批失败——一个条目查不到不该让整屏的标识都消失。
///
/// # 为什么要批量版
///
/// 卡片右上角的缓存标识需要每个文件的状态。逐个调
/// [`remote_cache_file_stat`] 意味着一屏 N 个文件就 N 次 IPC 往返，
/// 而这些查询全在本地（读缓存索引、不碰网络），往返开销反而成了主要
/// 成本。
///
/// 也**刻意不并进 `remote_browse`**：browse 要走网络、可能几百毫秒，
/// 缓存状态是本地的、几乎瞬时。合在一起就意味着「想刷新缓存标识必须
/// 重新列一次目录」，而 pin 之后变的只是标识、目录没变。
///
/// # Errors
///
/// 整批都拿不到时返回；单项失败不算错误。
#[tauri::command]
pub async fn remote_cache_file_stats(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    reqs: Vec<RemoteFileRef>,
) -> CmdResult<Vec<omy_remote::cache::FileCacheStat>> {
    let keks = crate::place_keys::unlock_keks(&state);
    let mut connected = std::collections::BTreeSet::new();
    for req in &reqs {
        if !has_complete_pinned_candidate(&reg, &cache, &req.place_id, &req.path, req.size)
            && connected.insert(req.place_id.clone())
        {
            crate::telegram_cmds::ensure_connected(&reg, &req.place_id, &keks).await?;
        }
    }
    let mut out = Vec::with_capacity(reqs.len());
    for req in reqs {
        // 一项失败给零值占位，不让整批垮掉：这批数据只用来画标识，
        // 少一个标识远好过整屏标识消失
        let stat = match build_remote_source(&reg, &cache, &req).await {
            Ok(src) => src.cache_stat(),
            Err(_) => omy_remote::cache::FileCacheStat {
                cached_blocks: 0,
                total_blocks: 0,
                cached_bytes: 0,
                fully_cached: false,
                pinned: false,
            },
        };
        out.push(stat);
    }
    Ok(out)
}

/// 删除单个远程文件的本地密文块，返回释放字节数。
///
/// 只读位置也允许：它只清理本机缓存，绝不向云端发任何写请求。
#[tauri::command]
pub async fn remote_cache_remove_file(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<serde_json::Value> {
    if !has_complete_pinned_candidate(&reg, &cache, &req.place_id, &req.path, req.size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &req.place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
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
    state: tauri::State<'_, Shared>,
    place_id: String,
    dir: String,
) -> CmdResult<omy_remote::Capabilities> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    place
        .store
        .effective_capabilities(&dir)
        .await
        .map_err(|e| to_cmd_err(&e))
}

pub(super) fn has_complete_pinned_candidate(
    reg: &PlaceRegistry,
    cache: &RemoteCache,
    place_id: &str,
    path: &str,
    size: u64,
) -> bool {
    let Some(place) = reg.get(place_id) else {
        return false;
    };
    let Some(source) = PlaceRegistry::place_source(&place) else {
        return false;
    };
    cache
        .snapshot()
        .is_some_and(|c| c.has_pinned_candidate(&source, path, size))
}

/// 读到足以 `open` 的完整文件头，**优先走完整永久文件和临时分块缓存**。
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
pub(super) async fn fetch_full_header<S: RemoteStore>(
    store: &S,
    path: &str,
    size: u64,
    cache: Option<&omy_remote::cache::BlockCache>,
    place_id: &str,
    pinned_source: Option<&omy_remote::virtuals::SourceRef>,
) -> Result<Vec<u8>, RemoteError> {
    omy_remote::fetch_header_with_pinned(store, path, size, cache, place_id, pinned_source).await
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

/// 把一个远程文件转为**完整原文件永久缓存**。
///
/// # 这是一次真实的下载任务
///
/// 产品上「转为永久」不是打个标记就完事：它按顺序取得整个远端原文件，写入
/// `.part`，同步完成后再原子提交。这样离线打开和跨机器复制依赖的是已验证完整
/// 文件，而不是一组只有应用内部能理解的散块。
///
/// 返回提交到永久层的字节数。
///
/// # Errors
///
/// 位置不存在、不是 omy 文件、网络失败时返回。
#[tauri::command]
pub async fn remote_cache_pin(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    pins: tauri::State<'_, Arc<PinRetryStore>>,
    req: RemoteFileRef,
) -> CmdResult<u64> {
    use crate::transfers::TaskKind;

    if !has_complete_pinned_candidate(&reg, &cache, &req.place_id, &req.path, req.size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &req.place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
    let source = build_remote_source(&reg, &cache, &req).await?;

    // 进传输管理页。「转为永久」在产品上就是一次真实的下载任务——
    // 有进度、可取消、失败可重试——所以它和上传、下载同表。
    //
    // 不登记会怎样：用户点完「转为永久」界面上什么都没有，而预热一个
    // 几 MB 的文件要好几秒，看起来像没点上。
    // 总量用前端给的 size：RemoteSource 自己不知道明文大小，
    // 而这个值本来就是列表里显示的那个，两处一致
    let total = req.size;
    // 显示名由调用方给。
    //
    // 不在这里按分隔符切 id 去猜：Telegram 的条目 id 形如
    // `tg:<对话>:<消息号>`，按 ':' 取最后一段会得到「6」这样的消息号，
    // 用户在传输列表里看到一行「6」根本不知道是哪个文件（实测过）。
    // 这和当初 remote_probe_entry 按 '/' 猜名字是同一个错误——
    // **让调用方传名字，不要在后端猜 id 的形状。**
    //
    // 调用方给的是列表里显示的那个名字：未解锁时是磁盘名，已解锁时是
    // 解密后的真名。后者确实是明文，但它本来就已经显示在用户自己的
    // 屏幕上了，传输列表不构成新的泄露面。
    let name = req.name.clone().unwrap_or_else(|| req.path.clone());
    let target = reg
        .get(&req.place_id)
        .map_or_else(|| req.place_id.clone(), |p| p.name.clone());
    let h = xfer.start(&app, TaskKind::Pin, name, target, total);
    // 记下这条 pin 任务的原始请求：失败后用户点重试要能重跑同一个操作。
    // 只进内存重试登记表（不落盘、不进日志）——它含明文名，等同用户屏幕上
    // 已显示的信息，进程退出即清。
    pins.remember(h.id(), req.clone());

    run_pin(&app, &source, &xfer, &req, &h)
}

/// pin 任务的执行体：顺序写完整远端原文件 → 原子提交，全程更新进度与终态。
/// 首次 pin 与失败后重试共用它；已有完整文件时立即命中，不访问网络。
pub(super) fn run_pin(
    app: &tauri::AppHandle,
    source: &omy_remote::source::RemoteSource<PlaceStore>,
    xfer: &Arc<crate::transfers::Transfers>,
    req: &RemoteFileRef,
    h: &Arc<crate::transfers::TaskHandle>,
) -> CmdResult<u64> {
    use crate::transfers::TaskState;

    let h2 = Arc::clone(h);
    let app2 = app.clone();
    let xfer2 = Arc::clone(xfer);
    let r = tokio::task::block_in_place(|| {
        source.pin_with_progress(&mut |done| {
            h2.set_done(done);
            xfer2.tick(&app2, &h2);
            while h2.is_paused() && !h2.is_canceled() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            !h2.is_canceled()
        })
    })
    .map_err(|e| to_cmd_err(&e));
    if h.is_canceled() {
        return Err(CmdError::code("remote_canceled"));
    }
    match &r {
        Ok(freed) => {
            crate::applog::info(
                "pin",
                &format!(
                    "place={} file={} 已转永久，bytes={freed}",
                    req.place_id,
                    crate::applog::redact(&req.path)
                ),
            );
            xfer.finish(app, h.id(), TaskState::Done);
        }
        Err(e) => {
            crate::applog::error(
                "pin",
                &format!(
                    "place={} file={} 转永久失败: {}",
                    req.place_id,
                    crate::applog::redact(&req.path),
                    e.code
                ),
            );
            xfer.finish(app, h.id(), TaskState::failed(e.code.clone()));
        }
    }
    r
}

/// 取消永久缓存：删除完整远端原文件副本。
///
/// 临时分块缓存与永久层独立；如果此前预览留下了临时块，它们继续受 LRU 管理，
/// 不为取消永久而额外拆回或复制。
///
/// # Errors
///
/// 位置不存在或不是 omy 文件时返回。
#[tauri::command]
pub async fn remote_cache_unpin(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    req: RemoteFileRef,
) -> CmdResult<u64> {
    if !has_complete_pinned_candidate(&reg, &cache, &req.place_id, &req.path, req.size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &req.place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
    let source = build_remote_source(&reg, &cache, &req).await?;
    let r = source.unpin().map_err(|e| to_cmd_err(&e));
    match &r {
        Ok(freed) => crate::applog::info(
            "unpin",
            &format!(
                "place={} file={} 取消永久，释放 bytes={freed}",
                req.place_id,
                crate::applog::redact(&req.path)
            ),
        ),
        Err(e) => crate::applog::warn(
            "unpin",
            &format!(
                "place={} file={} 取消永久失败: {}",
                req.place_id,
                crate::applog::redact(&req.path),
                e.code
            ),
        ),
    }
    r
}

/// 列出所有**永久保留**的文件，供设置页的「管理永久缓存」。
///
/// # 为什么这条入口必须存在
///
/// 没有它，用户要取消某个文件的永久保留，只能回到那个文件原来所在的位置、
/// 在目录里把它找出来再右键——而 Telegram 这类位置上，那条消息可能早就
/// 翻不到了（对话里几千条消息，或者他根本不记得是在哪个频道）。
/// 结果就是永久层只进不出，占了空间也清不掉。
///
/// 后端的 `list_pinned` 早就实现了、也有单测，只是一直没有命令暴露出来。
///
/// # 返回的是键而不是路径
///
/// `key` 含文件版本哈希（见 `RemoteSource::cache_version`），它是缓存里的
/// 身份，**不是**能拿去浏览的路径。界面把它当成不透明标识，
/// 配合 [`remote_cache_unpin_by_key`] 使用即可。
#[tauri::command]
#[must_use]
pub fn remote_cache_list_pinned(
    cache: tauri::State<'_, Arc<RemoteCache>>,
) -> Vec<omy_remote::pinned::PinnedFile> {
    cache
        .snapshot()
        .map(|c| c.list_pinned())
        .unwrap_or_default()
}

/// 按永久清单返回的不透明相对目录删除完整原文件，不碰网络。
#[tauri::command]
pub fn remote_cache_unpin_by_key(
    cache: tauri::State<'_, Arc<RemoteCache>>,
    relative_dir: String,
) -> CmdResult<u64> {
    let Some(c) = cache.snapshot() else {
        return Err(CmdError::code("remote_cache_unavailable"));
    };
    c.remove_pinned_relative(&relative_dir)
        .map_err(|e| to_cmd_err(&e))
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
pub(super) fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            CmdError::with(
                "open_failed",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })
}

#[cfg(target_os = "macos")]
pub(super) fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            CmdError::with(
                "open_failed",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
pub(super) fn open_in_file_manager(path: &std::path::Path) -> CmdResult<()> {
    std::process::Command::new("xdg-open")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            CmdError::with(
                "open_failed",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })
}

#[cfg(target_os = "android")]
pub(super) fn open_in_file_manager(_path: &std::path::Path) -> CmdResult<()> {
    Err(CmdError::code("remote_unsupported"))
}
