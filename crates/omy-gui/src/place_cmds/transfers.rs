//! 传输任务列表的控制命令，以及 Telegram「全部缓存」的批量任务与失败重试。

use super::*;

/// 当前全部传输任务。
#[tauri::command]
#[must_use]
pub fn transfer_list(
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
) -> Vec<crate::transfers::Task> {
    xfer.list()
}

/// 取消一条任务。执行体会在下一个分片边界干净退出。
#[tauri::command]
pub fn transfer_cancel(
    app: tauri::AppHandle,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    pins: tauri::State<'_, Arc<PinRetryStore>>,
    copies: tauri::State<'_, Arc<crate::remote_copy::CopyRetryStore>>,
    batches: tauri::State<'_, Arc<CacheBatchStore>>,
    id: u64,
) {
    // 取消总任务时级联取消全部未结束子任务；子任务单独取消不影响兄弟任务。
    for child_id in xfer.child_ids(id) {
        xfer.cancel(&app, child_id);
        batches.forget(child_id);
    }
    xfer.cancel(&app, id);
    batches.forget(id);
    // 取消即不再是「可重试的失败」，两类原始请求登记都一并清掉。
    pins.forget(id);
    copies.forget(id);
}

/// 暂停或继续单条任务；总任务操作会级联到全部子任务。
#[tauri::command]
pub fn transfer_pause(
    app: tauri::AppHandle,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    id: u64,
    paused: bool,
) {
    let children = xfer.child_ids(id);
    if children.is_empty() {
        xfer.pause(&app, id, paused);
    } else {
        for child_id in children {
            xfer.pause(&app, child_id, paused);
        }
        xfer.pause(&app, id, paused);
    }
}

/// Telegram 群组全部缓存的类型选择与交集筛选。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TelegramCacheAllRequest {
    pub place_id: String,
    pub dir: String,
    pub dir_name: String,
    #[serde(default)]
    pub media_types: Vec<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    #[serde(default)]
    pub keyword: String,
}

#[derive(Debug, Clone)]
pub(super) struct BatchChild {
    task_id: u64,
    req: RemoteFileRef,
}

/// 批量缓存任务的运行态。队列与重试参数只驻留内存；进程重启后用户可重新发起，
/// 已完整缓存的文件会被跳过，不会重复下载。
#[derive(Debug, Default)]
pub struct CacheBatchStore {
    queue: Mutex<VecDeque<BatchChild>>,
    reqs: Mutex<HashMap<u64, BatchChild>>,
    running: std::sync::atomic::AtomicUsize,
}

impl CacheBatchStore {
    fn remember(&self, child: BatchChild) {
        if let Ok(mut reqs) = self.reqs.lock() {
            reqs.insert(child.task_id, child.clone());
        }
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(child);
        }
    }

    fn get(&self, id: u64) -> Option<BatchChild> {
        self.reqs
            .lock()
            .ok()
            .and_then(|reqs| reqs.get(&id).cloned())
    }

    fn forget(&self, id: u64) {
        if let Ok(mut reqs) = self.reqs.lock() {
            reqs.remove(&id);
        }
        if let Ok(mut queue) = self.queue.lock() {
            queue.retain(|child| child.task_id != id);
        }
    }

    fn requeue(&self, child: BatchChild) {
        if let Ok(mut queue) = self.queue.lock() {
            queue.push_back(child);
        }
    }
}

/// 扫描一个 Telegram 对话，并逐步建立“总任务 → 文件子任务”。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn telegram_cache_all(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    batches: tauri::State<'_, Arc<CacheBatchStore>>,
    req: TelegramCacheAllRequest,
) -> CmdResult<u64> {
    if req.to.zip(req.from).is_some_and(|(to, from)| to < from) {
        return Err(CmdError::code("remote_cache_bad_range"));
    }
    crate::telegram_cmds::ensure_connected(
        &reg,
        &req.place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    if place.store.as_telegram().is_none() {
        return Err(CmdError::code("remote_unsupported"));
    }
    let tabs = req
        .media_types
        .iter()
        .filter(|key| key.as_str() != "all")
        .map(|key| omy_remote::telegram::store::MediaTab::from_key(key))
        .collect();
    let filter = omy_remote::telegram::store::CacheFilter {
        media_tabs: tabs,
        from: req.from,
        to: req.to,
        keyword: req.keyword.clone(),
    };
    let group = xfer.start_group(&app, req.dir_name.clone(), place.name.clone());
    let parent_id = group.id();
    let place_id = req.place_id.clone();
    let dir = req.dir.clone();
    let target = format!("{} › {}", place.name, req.dir_name);
    let store_for_scan = Arc::clone(&place.store);
    let reg_for_scan = Arc::clone(&reg);
    let cache_for_scan = Arc::clone(&cache);
    let xfer_for_scan = Arc::clone(&xfer);
    let batches_for_scan = Arc::clone(&batches);
    let app_for_scan = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(telegram) = store_for_scan.as_telegram() else {
            xfer_for_scan.finish(
                &app_for_scan,
                parent_id,
                crate::transfers::TaskState::failed("remote_unsupported"),
            );
            return;
        };
        let scan = telegram
            .scan_cache_candidates(&dir, &filter, &mut |candidate| {
                if group.is_canceled() {
                    return false;
                }
                let file_req = RemoteFileRef {
                    place_id: place_id.clone(),
                    path: candidate.id,
                    size: candidate.size,
                    name: Some(candidate.name.clone()),
                    server_name: Some(candidate.name),
                };
                let child = xfer_for_scan.start_child(
                    &app_for_scan,
                    parent_id,
                    file_req
                        .name
                        .clone()
                        .unwrap_or_else(|| file_req.path.clone()),
                    target.clone(),
                    file_req.size,
                );
                xfer_for_scan.set_waiting(&app_for_scan, child.id(), true);
                batches_for_scan.remember(BatchChild {
                    task_id: child.id(),
                    req: file_req,
                });
                true
            })
            .await;
        if let Err(error) = scan {
            xfer_for_scan.finish(
                &app_for_scan,
                parent_id,
                crate::transfers::TaskState::failed(to_cmd_err(&error).code),
            );
            return;
        }
        // 扫描结束后若一个候选都没有，总任务直接完成；否则由子任务汇总驱动。
        if xfer_for_scan.child_ids(parent_id).is_empty() {
            xfer_for_scan.finish(
                &app_for_scan,
                parent_id,
                crate::transfers::TaskState::Done,
            );
        } else {
            xfer_for_scan.finish_group_scan(&app_for_scan, parent_id);
        }
        pump_cache_batches(
            app_for_scan,
            reg_for_scan,
            cache_for_scan,
            xfer_for_scan,
            batches_for_scan,
        );
    });
    Ok(parent_id)
}

// Rust 1.99 将 fetch_update 改名为 try_update，但后者到 1.95 才稳定；
// 本仓库 MSRV 是 1.88，因此只能在这个兼容点继续使用旧名。
#[allow(deprecated)]
pub(super) fn pump_cache_batches(
    app: tauri::AppHandle,
    reg: Arc<PlaceRegistry>,
    cache: Arc<RemoteCache>,
    xfer: Arc<crate::transfers::Transfers>,
    batches: Arc<CacheBatchStore>,
) {
    let limit = omy_config::Config::load()
        .unwrap_or_default()
        .remote
        .transfer_concurrency
        .clamp(1, 16);
    loop {
        let reserved = batches.running.fetch_update(
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
            |running| (running < limit).then_some(running.saturating_add(1)),
        );
        if reserved.is_err() {
            break;
        }
        let child = batches
            .queue
            .lock()
            .ok()
            .and_then(|mut queue| queue.pop_front());
        let Some(child) = child else {
            batches
                .running
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            break;
        };
        xfer.set_waiting(&app, child.task_id, false);
        let app2 = app.clone();
        let reg2 = Arc::clone(&reg);
        let cache2 = Arc::clone(&cache);
        let xfer2 = Arc::clone(&xfer);
        let batches2 = Arc::clone(&batches);
        tauri::async_runtime::spawn(async move {
            run_cache_child(&app2, &reg2, &cache2, &xfer2, &child).await;
            batches2
                .running
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            pump_cache_batches(app2, reg2, cache2, xfer2, batches2);
        });
    }
}

pub(super) async fn run_cache_child(
    app: &tauri::AppHandle,
    reg: &PlaceRegistry,
    cache: &RemoteCache,
    xfer: &Arc<crate::transfers::Transfers>,
    child: &BatchChild,
) {
    use crate::transfers::TaskState;

    let source = match build_remote_source(reg, cache, &child.req).await {
        Ok(source) => source,
        Err(error) => {
            xfer.finish(app, child.task_id, TaskState::failed(error.code));
            return;
        }
    };
    if source.cache_stat().fully_cached {
        xfer.mark_skipped(app, child.task_id);
        return;
    }
    let Some(handle) = xfer.handle(child.task_id) else {
        xfer.finish(app, child.task_id, TaskState::failed("remote_failed"));
        return;
    };
    let h = Arc::clone(&handle);
    let app2 = app.clone();
    let xfer2 = Arc::clone(xfer);
    let result = tokio::task::block_in_place(|| {
        source.prefetch_all_with_progress(&mut |done| {
            h.set_done(done);
            xfer2.tick(&app2, &h);
            while h.is_paused() && !h.is_canceled() {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            !h.is_canceled()
        })
    });
    if h.is_canceled() {
        return;
    }
    match result {
        Ok(_) if source.cache_stat().fully_cached => {
            xfer.finish(app, child.task_id, TaskState::Done)
        }
        Ok(_) => xfer.finish(
            app,
            child.task_id,
            TaskState::failed("remote_prefetch_failed"),
        ),
        Err(_) => xfer.finish(app, child.task_id, TaskState::failed("remote_network")),
    }
}

/// pin 任务的重试登记表：任务 id -> 原始 `RemoteFileRef`。
///
/// 为什么要单独存：`Task` 只带显示名 / 去向，不带 `place_id + path + size`，
/// 而重跑 pin 需要这三样才能重建 `RemoteSource`。放在 GUI 层的内存表里
/// （`Transfers` 是通用进度表、不该知道 pin 的请求形状），进程退出即清、
/// 不落盘、不进日志——它含明文名，等同用户屏幕上已显示的信息。
#[derive(Debug, Default)]
pub struct PinRetryStore {
    reqs: std::sync::Mutex<std::collections::HashMap<u64, RemoteFileRef>>,
}

impl PinRetryStore {
    /// 记下一条 pin 任务的原始请求。
    pub fn remember(&self, id: u64, req: RemoteFileRef) {
        if let Ok(mut m) = self.reqs.lock() {
            m.insert(id, req);
        }
    }
    /// 取出（不移除）某任务的原始请求，供重试重跑。
    #[must_use]
    pub fn get(&self, id: u64) -> Option<RemoteFileRef> {
        self.reqs.lock().ok().and_then(|m| m.get(&id).cloned())
    }
    /// 任务彻底结束（取消 / 清除）后丢弃其登记，避免表无限增长。
    pub fn forget(&self, id: u64) {
        if let Ok(mut m) = self.reqs.lock() {
            m.remove(&id);
        }
    }
}

/// 重试一条失败的传输任务。
///
/// 只对**可重试**的失败生效（`reset_running` 内部只翻 `Failed` 态，配合
/// 前端只在 `retryable` 时给按钮）：不可重试的（磁盘满 / 认证失效）压根不
/// 该走到这里；即便前端漏判传了进来，`reset_running` 会翻回 Running、
/// `run_pin` 再跑一次也只是再失败一次同样的错，不会造成坏状态。
///
/// 目前只有 pin 会进传输表（上传 / 下载还没登记成任务），所以重试就是
/// 重跑 pin。找不到登记的原始请求（进程重启后内存表已空）时返回错误、
/// 让界面提示重新从文件那里发起。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn transfer_retry(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    pins: tauri::State<'_, Arc<PinRetryStore>>,
    copies: tauri::State<'_, Arc<crate::remote_copy::CopyRetryStore>>,
    batches: tauri::State<'_, Arc<CacheBatchStore>>,
    id: u64,
) -> CmdResult<u64> {
    let child_ids = xfer.child_ids(id);
    if !child_ids.is_empty() {
        let mut retried = 0usize;
        for child_id in child_ids {
            let Some(child) = batches.get(child_id) else { continue };
            if xfer.reset_running(&app, child_id).is_some() {
                xfer.set_waiting(&app, child_id, true);
                batches.requeue(child);
                retried = retried.saturating_add(1);
            }
        }
        if retried == 0 {
            return Err(CmdError::code("remote_retry_not_failed"));
        }
        xfer.set_waiting(&app, id, false);
        pump_cache_batches(
            app,
            Arc::clone(&reg),
            Arc::clone(&cache),
            Arc::clone(&xfer),
            Arc::clone(&batches),
        );
        return Ok(id);
    }
    if let Some(child) = batches.get(id) {
        let handle = xfer
            .reset_running(&app, id)
            .ok_or_else(|| CmdError::code("remote_retry_not_failed"))?;
        xfer.set_waiting(&app, handle.id(), true);
        batches.requeue(child);
        pump_cache_batches(
            app,
            Arc::clone(&reg),
            Arc::clone(&cache),
            Arc::clone(&xfer),
            Arc::clone(&batches),
        );
        return Ok(id);
    }
    if let Some(req) = copies.get(id) {
        let keks = crate::place_keys::unlock_keks(&state);
        crate::telegram_cmds::ensure_connected(&reg, &req.source.place_id, &keks).await?;
        if req.target_place_id != req.source.place_id {
            crate::telegram_cmds::ensure_connected(&reg, &req.target_place_id, &keks).await?;
        }
        return crate::remote_copy::retry_copy(
            app,
            Arc::clone(&reg),
            Arc::clone(&cache),
            Arc::clone(&xfer),
            &copies,
            id,
        );
    }
    let Some(req) = pins.get(id) else {
        // 登记丢了（多半是重启后内存表已空）：没法原地重试，让界面引导用户
        // 回文件那里重新「转为永久」。给明确码而不是静默失败。
        return Err(CmdError::code("remote_retry_expired"));
    };
    crate::telegram_cmds::ensure_connected(
        &reg,
        &req.place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    // 先重建 source（网络 / 位置不存在会在这里失败，直接返回，不动任务状态）
    let source = build_remote_source(&reg, &cache, &req).await?;
    // 翻回 Running、发新句柄；非失败任务 reset_running 返回 None，说明这条
    // 已经在跑或已完成，不该重试
    let Some(h) = xfer.reset_running(&app, id) else {
        return Err(CmdError::code("remote_retry_not_failed"));
    };
    run_pin(&app, &source, &xfer, &req, &h)
}

/// 全部暂停 / 全部继续。
#[tauri::command]
pub fn transfer_pause_all(
    app: tauri::AppHandle,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    paused: bool,
) {
    xfer.pause_all(&app, paused);
}

/// 清除已结束的任务（完成 / 失败 / 取消）。
#[tauri::command]
pub fn transfer_clear_done(
    app: tauri::AppHandle,
    xfer: tauri::State<'_, Arc<crate::transfers::Transfers>>,
    pins: tauri::State<'_, Arc<PinRetryStore>>,
    copies: tauri::State<'_, Arc<crate::remote_copy::CopyRetryStore>>,
    batches: tauri::State<'_, Arc<CacheBatchStore>>,
) {
    // 清掉的任务其原始请求登记也一并丢，避免内存表随清除次数无限增长。
    for id in xfer.clear_done(&app) {
        pins.forget(id);
        copies.forget(id);
        batches.forget(id);
    }
}
