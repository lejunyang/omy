//! 高级搜索命令与批量任务。查询只驻留内存，失败重试沿用游标和已成功子任务。
use super::*;
use crate::transfers::{TaskHandle, TaskKind, TaskState, Transfers};
use crate::virtual_place::VirtualRegistry;
use omy_remote::telegram::advanced::{Query, SearchHit, SearchPage, Selection};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedSearchRequest {
    place_id: String,
    dir: String,
    query: Query,
    cursor: Option<String>,
    limit: usize,
}

#[tauri::command]
pub async fn telegram_advanced_search(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    req: AdvancedSearchRequest,
) -> CmdResult<SearchPage> {
    req.query
        .validate()
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    let chat = omy_remote::telegram::Conversation::parse_dir_id(&req.dir)
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    if !(1..=100).contains(&req.limit) {
        return Err(CmdError::code("tg_advanced_invalid"));
    }
    omy_remote::telegram::advanced::Cursor::start(chat, &req.query, req.cursor.as_deref())
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    crate::telegram_cmds::ensure_connected(
        &reg,
        &req.place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let tg = place
        .store
        .as_telegram()
        .ok_or_else(|| CmdError::code("remote_unsupported"))?;
    let mut page = tg
        .advanced_search(&req.dir, &req.query, req.cursor.as_deref(), req.limit)
        .await
        .map_err(|e| to_cmd_err(&e))?;
    for hit in &mut page.hits {
        hit.thumb_token = hit.thumb.take().and_then(|b| thumbs.insert_image(b));
    }
    Ok(page)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchAction {
    Pin,
    Forward,
    AddVirtual,
    UploadTo,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BatchTarget {
    Forward {
        dir: String,
    },
    Virtual {
        place_id: String,
        folder: String,
    },
    Remote {
        place_id: String,
        dir: String,
        mode: crate::remote_copy::CopyMode,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdvancedBatchRequest {
    place_id: String,
    dir: String,
    query: Query,
    selection: Selection,
    action: BatchAction,
    target: Option<BatchTarget>,
}

#[derive(Debug, Clone)]
enum Work {
    File { hit: SearchHit, ref_id: String },
    Forward(Vec<i32>),
}

#[derive(Debug)]
struct BatchRun {
    req: AdvancedBatchRequest,
    cursor: Option<String>,
    ids_offset: usize,
    discovered: BTreeSet<String>,
    forward_ids: BTreeSet<i32>,
    children: BTreeMap<u64, Work>,
    scan_done: bool,
}

#[derive(Debug, Default)]
pub struct AdvancedBatchStore {
    runs: Mutex<HashMap<u64, Arc<Mutex<BatchRun>>>>,
}
impl AdvancedBatchStore {
    pub(super) fn forget(&self, id: u64) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&id);
            for run in runs.values() {
                if let Ok(mut run) = run.lock() {
                    run.children.remove(&id);
                }
            }
        }
    }
    fn get(&self, id: u64) -> Option<Arc<Mutex<BatchRun>>> {
        self.runs.lock().ok()?.get(&id).cloned()
    }
}

#[derive(Clone)]
pub(super) struct BatchContext {
    pub app: tauri::AppHandle,
    pub reg: Arc<PlaceRegistry>,
    pub cache: Arc<RemoteCache>,
    pub xfer: Arc<Transfers>,
    pub vreg: Arc<VirtualRegistry>,
    pub batches: Arc<AdvancedBatchStore>,
}

fn forward_chunks(ids: &BTreeSet<i32>) -> Vec<Vec<i32>> {
    ids.iter()
        .copied()
        .collect::<Vec<_>>()
        .chunks(100)
        .map(<[i32]>::to_vec)
        .collect()
}

fn target_valid(req: &AdvancedBatchRequest) -> bool {
    matches!(
        (&req.action, &req.target),
        (BatchAction::Pin, None)
            | (BatchAction::Forward, Some(BatchTarget::Forward { .. }))
            | (BatchAction::AddVirtual, Some(BatchTarget::Virtual { .. }))
            | (BatchAction::UploadTo, Some(BatchTarget::Remote { .. }))
    )
}

async fn validate(ctx: &BatchContext, req: &AdvancedBatchRequest) -> CmdResult<()> {
    req.query
        .validate()
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    let chat = omy_remote::telegram::Conversation::parse_dir_id(&req.dir)
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    req.selection
        .validate(chat)
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    if !target_valid(req) {
        return Err(CmdError::code("tg_advanced_target"));
    }
    let place = ctx
        .reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let tg = place
        .store
        .as_telegram()
        .ok_or_else(|| CmdError::code("remote_unsupported"))?;
    match &req.target {
        Some(BatchTarget::Forward { dir }) => {
            tg.validate_forward(&req.dir, dir).await.map_err(|e| {
                if matches!(
                    e,
                    RemoteError::Unsupported(omy_remote::telegram::store::FORWARD_PROTECTED)
                ) {
                    CmdError::code("tg_forward_protected")
                } else {
                    to_cmd_err(&e)
                }
            })?
        }
        Some(BatchTarget::Virtual { place_id, folder }) => {
            if ctx.vreg.is_encrypted(place_id) && !ctx.vreg.is_unlocked(place_id) {
                return Err(CmdError::code("virtual_locked"));
            }
            if !ctx
                .vreg
                .with_place(place_id, |v| v.root.find(folder).is_some())
                .unwrap_or(false)
            {
                return Err(CmdError::code("virtual_no_target"));
            }
            if PlaceRegistry::place_source(&place).is_none() {
                return Err(CmdError::code("virtual_source_unidentified"));
            }
        }
        Some(BatchTarget::Remote { place_id, dir, .. }) => {
            let dest = ctx
                .reg
                .get(place_id)
                .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
            if !dest
                .store
                .effective_capabilities(dir)
                .await
                .map_err(|e| to_cmd_err(&e))?
                .write
            {
                return Err(CmdError::code("remote_readonly"));
            }
        }
        None => {}
    }
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn telegram_advanced_batch(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    xfer: tauri::State<'_, Arc<Transfers>>,
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    batches: tauri::State<'_, Arc<AdvancedBatchStore>>,
    req: AdvancedBatchRequest,
) -> CmdResult<serde_json::Value> {
    let keks = crate::place_keys::unlock_keks(&state);
    // 先做纯参数验证，畸形请求不应触发建连。
    req.query
        .validate()
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    let chat = omy_remote::telegram::Conversation::parse_dir_id(&req.dir)
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    req.selection
        .validate(chat)
        .map_err(|_| CmdError::code("tg_advanced_invalid"))?;
    if !target_valid(&req) {
        return Err(CmdError::code("tg_advanced_target"));
    }
    crate::telegram_cmds::ensure_connected(&reg, &req.place_id, &keks).await?;
    if let Some(BatchTarget::Remote { place_id, .. }) = &req.target {
        crate::telegram_cmds::ensure_connected(&reg, place_id, &keks).await?;
    }
    let ctx = BatchContext {
        app,
        reg: Arc::clone(&reg),
        cache: Arc::clone(&cache),
        xfer: Arc::clone(&xfer),
        vreg: Arc::clone(&vreg),
        batches: Arc::clone(&batches),
    };
    validate(&ctx, &req).await?;
    let source = ctx
        .reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let handle = ctx.xfer.start_group(
        &ctx.app,
        source
            .store
            .as_telegram()
            .and_then(|tg| {
                omy_remote::telegram::Conversation::parse_dir_id(&req.dir)
                    .ok()
                    .and_then(|chat| tg.conversation(chat))
            })
            .map_or_else(|| req.dir.clone(), |c| c.title),
        source.name.clone(),
    );
    let id = handle.id();
    ctx.batches
        .runs
        .lock()
        .map_err(|_| CmdError::code("remote_failed"))?
        .insert(
            id,
            Arc::new(Mutex::new(BatchRun {
                req,
                cursor: None,
                ids_offset: 0,
                discovered: BTreeSet::new(),
                forward_ids: BTreeSet::new(),
                children: BTreeMap::new(),
                scan_done: false,
            })),
        );
    spawn(ctx, id, handle);
    Ok(serde_json::json!({ "task_id": id.to_string() }))
}

async fn ready(handle: &TaskHandle) -> bool {
    while handle.is_paused() && !handle.is_canceled() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    !handle.is_canceled()
}

fn spawn(ctx: BatchContext, id: u64, handle: Arc<TaskHandle>) {
    tauri::async_runtime::spawn(async move {
        let result = run(&ctx, id, &handle).await;
        if handle.is_canceled() {
            return;
        }
        match result {
            Ok(()) => ctx.xfer.finish_group_scan(&ctx.app, id),
            Err(e) => ctx.xfer.finish(&ctx.app, id, TaskState::failed(e.code)),
        }
    });
}

async fn run(ctx: &BatchContext, id: u64, handle: &Arc<TaskHandle>) -> CmdResult<()> {
    let run = ctx
        .batches
        .get(id)
        .ok_or_else(|| CmdError::code("remote_retry_expired"))?;
    let req = run
        .lock()
        .map_err(|_| CmdError::code("remote_failed"))?
        .req
        .clone();
    let source = ctx
        .reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let tg = source
        .store
        .as_telegram()
        .ok_or_else(|| CmdError::code("remote_unsupported"))?;
    let selected_ids = req.selection.message_ids();
    loop {
        if !ready(handle).await {
            return Ok(());
        }
        let (done, cursor, offset) = {
            let r = run.lock().map_err(|_| CmdError::code("remote_failed"))?;
            (r.scan_done, r.cursor.clone(), r.ids_offset)
        };
        if done {
            break;
        }
        let (hits, next, scanned_done, next_offset) =
            if matches!(req.selection, Selection::Ids { .. }) {
                let chunk = selected_ids
                    .iter()
                    .skip(offset)
                    .take(100)
                    .copied()
                    .collect::<Vec<_>>();
                let n = offset + chunk.len();
                (
                    tg.advanced_selected(&req.dir, &req.query, &chunk)
                        .await
                        .map_err(|e| to_cmd_err(&e))?,
                    None,
                    n >= selected_ids.len(),
                    n,
                )
            } else {
                let page = tg
                    .advanced_search(&req.dir, &req.query, cursor.as_deref(), 100)
                    .await
                    .map_err(|e| to_cmd_err(&e))?;
                let end = page.cursor.is_none();
                (page.hits, page.cursor, end, offset)
            };
        if !ready(handle).await {
            return Ok(());
        }
        // 页成功入队后才推进游标；失败重试不会跳过未登记的页。
        let mut r = run.lock().map_err(|_| CmdError::code("remote_failed"))?;
        for mut hit in hits {
            if handle.is_canceled() { return Ok(()); }
            hit.thumb = None;
            hit.text.clear();
            if !req.selection.includes(&hit.id) || !r.discovered.insert(hit.id.clone()) {
                continue;
            }
            if req.action == BatchAction::Forward {
                r.forward_ids.insert(hit.message);
                continue;
            }
            if hit.file_name.is_none() || hit.file_size.is_none() {
                continue;
            }
            let kind = if req.action == BatchAction::Pin {
                TaskKind::Pin
            } else {
                TaskKind::Upload
            };
            let child = ctx.xfer.start_child_kind(
                &ctx.app,
                id,
                kind,
                hit.file_name.clone().unwrap_or_default(),
                req.dir.clone(),
                hit.file_size.unwrap_or(0),
            );
            ctx.xfer.set_waiting(&ctx.app, child.id(), true);
            r.children.insert(child.id(), Work::File { hit, ref_id: format!("vr{}", crate::virtual_place::random_id()) });
        }
        r.cursor = next;
        r.ids_offset = next_offset;
        r.scan_done = scanned_done;
        if scanned_done && req.action == BatchAction::Forward {
            // 全局旧→新再分批；只反转每一页会使批次间仍是新→旧。
            for chunk in forward_chunks(&r.forward_ids) {
                let child = ctx.xfer.start_child_kind(
                    &ctx.app,
                    id,
                    TaskKind::Upload,
                    req.dir.clone(),
                    req.dir.clone(),
                    chunk.len() as u64,
                );
                ctx.xfer.set_waiting(&ctx.app, child.id(), true);
                r.children.insert(child.id(), Work::Forward(chunk.to_vec()));
            }
        }
    }
    let work = run
        .lock()
        .map_err(|_| CmdError::code("remote_failed"))?
        .children
        .clone();
    // 顺序执行也使转发批次顺序确定；每个子任务都有独立失败和重试，不重放成功件。
    for (child_id, work) in work {
        if !ready(handle).await {
            return Ok(());
        }
        let Some(child) = ctx.xfer.handle(child_id) else {
            continue;
        };
        if !ready(&child).await {
            continue;
        }
        ctx.xfer.set_waiting(&ctx.app, child_id, false);
        let result = execute(ctx, &req, work, &child).await;
        if child.is_canceled() {
            continue;
        }
        match result {
            Ok(()) => ctx.xfer.finish(&ctx.app, child_id, TaskState::Done),
            Err(e) => {
                ctx.xfer
                    .finish(&ctx.app, child_id, TaskState::failed(e.code.clone()));
                // 前一批尚未确认成功时不能继续发后一批，否则重试会打乱全局时间顺序。
                if req.action == BatchAction::Forward {
                    for task in ctx
                        .xfer
                        .list()
                        .into_iter()
                        .filter(|task| task.parent_id == Some(id) && task.id > child_id)
                    {
                        if ctx.xfer.handle(task.id).is_some() {
                            ctx.xfer
                                .finish(&ctx.app, task.id, TaskState::failed(&e.code));
                        }
                    }
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

async fn execute(
    ctx: &BatchContext,
    req: &AdvancedBatchRequest,
    work: Work,
    handle: &Arc<TaskHandle>,
) -> CmdResult<()> {
    match (work, &req.target) {
        (Work::Forward(ids), Some(BatchTarget::Forward { dir })) => {
            let p = ctx
                .reg
                .get(&req.place_id)
                .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
            let tg = p
                .store
                .as_telegram()
                .ok_or_else(|| CmdError::code("remote_unsupported"))?;
            let count = tg
                .forward_messages(&req.dir, dir, &ids)
                .await
                .map_err(|e| {
                    // 断网时可能已发成功，旧接口重试会生成新的 random_id；不能盲目重复发送。
                    if matches!(e, RemoteError::Network(_)) {
                        CmdError::code("tg_forward_uncertain")
                    } else {
                        to_cmd_err(&e)
                    }
                })?;
            if count != ids.len() {
                return Err(CmdError::code("tg_forward_uncertain"));
            }
            Ok(())
        }
        (Work::File { hit, ref_id }, target) => {
            let file = RemoteFileRef {
                place_id: req.place_id.clone(),
                path: hit.id.clone(),
                size: hit.file_size.unwrap_or(0),
                name: hit.file_name.clone(),
                server_name: hit.file_name.clone(),
            };
            match (req.action, target) {
                (BatchAction::Pin, None) => {
                    let source = build_remote_source(&ctx.reg, &ctx.cache, &file).await?;
                    run_pin(&ctx.app, &source, &ctx.xfer, &file, handle).map(|_| ())
                }
                (BatchAction::AddVirtual, Some(BatchTarget::Virtual { place_id, folder })) => {
                    crate::virtual_cmds::add_ref_with_id(
                        &ctx.vreg,
                        &ctx.reg,
                        crate::virtual_cmds::AddRefReq {
                            place_id: place_id.clone(),
                            folder: folder.clone(),
                            source_place_id: req.place_id.clone(),
                            dir_id: req.dir.clone(),
                            file_id: hit.id,
                            snapshot_name: hit.file_name.unwrap_or_default(),
                            snapshot_size: hit.file_size,
                            source_media_tab: hit.media.map(|m| m.tab().to_owned()),
                        },
                        ref_id,
                    )?;
                    Ok(())
                }
                (
                    BatchAction::UploadTo,
                    Some(BatchTarget::Remote {
                        place_id,
                        dir,
                        mode,
                    }),
                ) => {
                    let mut copy = crate::remote_copy::RemoteCopyRequest {
                        source: file,
                        target_place_id: place_id.clone(),
                        target_dir: dir.clone(),
                        target_name: hit.file_name.unwrap_or_default(),
                        mode: *mode,
                        media_hint: None,
                    };
                    crate::remote_copy::validate_request(&ctx.reg, &copy).await?;
                    let source = ctx
                        .reg
                        .get(&req.place_id)
                        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
                    copy.media_hint = source
                        .store
                        .upload_media_hint(&copy.source.path)
                        .await
                        .map_err(|e| to_cmd_err(&e))?;
                    crate::remote_copy::execute_copy(
                        &ctx.app, &ctx.reg, &ctx.cache, &ctx.xfer, &copy, handle,
                    )
                    .await
                    .map_err(|code| CmdError::code(&code))
                }
                _ => Err(CmdError::code("tg_advanced_target")),
            }
        }
        _ => Err(CmdError::code("tg_advanced_target")),
    }
}

/// 返回 None 表示不是高级任务；父任务从失败扫描页续跑，已完成子任务不重放。
pub(super) async fn retry_advanced(
    ctx: BatchContext,
    id: u64,
    keks: &[Kek],
) -> Option<CmdResult<u64>> {
    let direct = ctx.batches.get(id);
    let parent = ctx
        .xfer
        .list()
        .iter()
        .find(|t| t.id == id)
        .and_then(|t| t.parent_id);
    let (root, run) = match direct {
        Some(r) => (id, r),
        None => {
            let p = parent?;
            (p, ctx.batches.get(p)?)
        }
    };
    let req = match run.lock() {
        Ok(r) => r.req.clone(),
        Err(_) => return Some(Err(CmdError::code("remote_failed"))),
    };
    if let Err(e) = crate::telegram_cmds::ensure_connected(&ctx.reg, &req.place_id, keks).await {
        return Some(Err(e));
    }
    if let Some(BatchTarget::Remote { place_id, .. }) = &req.target
        && let Err(e) = crate::telegram_cmds::ensure_connected(&ctx.reg, place_id, keks).await {
        return Some(Err(e));
    }
    if let Err(e) = validate(&ctx, &req).await {
        return Some(Err(e));
    }
    if root != id {
        if req.action == BatchAction::Forward {
            return Some(Err(CmdError::code("remote_retry_not_failed")));
        }
        if ctx.xfer.handle(root).is_some() {
            return Some(Err(CmdError::code("remote_retry_not_failed")));
        }
        let work = run.lock().ok().and_then(|r| r.children.get(&id).cloned())?;
        let Some(handle) = ctx.xfer.reset_running(&ctx.app, id) else {
            return Some(Err(CmdError::code("remote_retry_not_failed")));
        };
        tauri::async_runtime::spawn(async move {
            if !ready(&handle).await {
                return;
            }
            let result = execute(&ctx, &req, work, &handle).await;
            if handle.is_canceled() {
                return;
            }
            ctx.xfer.finish(
                &ctx.app,
                id,
                match result {
                    Ok(()) => TaskState::Done,
                    Err(e) => TaskState::failed(e.code),
                },
            );
        });
        return Some(Ok(id));
    }
    let Some(handle) = ctx.xfer.reset_running(&ctx.app, root) else {
        return Some(Err(CmdError::code("remote_retry_not_failed")));
    };
    for child in ctx.xfer.child_ids(root) {
        ctx.xfer.reset_running(&ctx.app, child);
    }
    ctx.xfer.begin_group_scan(root);
    spawn(ctx, root, handle);
    Some(Ok(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn paused_work_waits_and_cancel_unblocks_it() {
        let handle = Arc::new(TaskHandle::test_handle());
        handle.test_control(true, false);
        let paused =
            tokio::time::timeout(std::time::Duration::from_millis(20), ready(&handle)).await;
        assert!(paused.is_err(), "暂停时不能发出下一次扫描或动作请求");
        let resume = Arc::clone(&handle);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            resume.test_control(false, false);
        });
        assert!(ready(&handle).await, "继续后应允许后续动作");
        handle.test_control(true, true);
        assert!(!ready(&handle).await, "暂停期间取消不能永远卡在暂停循环");
    }
    #[test]
    fn forward_batches_are_globally_chronological() {
        let ids: BTreeSet<_> = (1..=230).rev().collect();
        let chunks = forward_chunks(&ids);
        assert_eq!(
            chunks.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![100, 100, 30],
            "超过 100 必须分批，尾批不能遗漏"
        );
        assert_eq!(
            chunks.into_iter().flatten().collect::<Vec<_>>(),
            (1..=230).collect::<Vec<_>>(),
            "必须先全局旧到新再分批，不能只反转单页"
        );
    }
    #[test]
    fn missing_and_mismatched_targets_are_rejected() {
        let q = Query {
            r#match: omy_remote::telegram::advanced::Match::All,
            conditions: vec![],
        };
        let mut req = AdvancedBatchRequest {
            place_id: String::from("p1"),
            dir: String::from("tg:1"),
            query: q,
            selection: Selection::All { except: vec![] },
            action: BatchAction::Forward,
            target: None,
        };
        assert!(!target_valid(&req), "无目标转发不能假装已启动");
        req.target = Some(BatchTarget::Virtual {
            place_id: String::from("v1"),
            folder: String::new(),
        });
        assert!(!target_valid(&req), "目标类型必须与动作一致");
        req.action = BatchAction::AddVirtual;
        assert!(target_valid(&req), "有效虚拟目标须可接线");
    }
}
