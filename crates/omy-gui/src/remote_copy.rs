//! 远程位置之间的复制上传。
//!
//! 源始终按固定窗口读取，目标始终消费异步流。两者之间只有一个固定容量的
//! duplex 管道，因此下载快于上传时会自然背压，不会把整个文件堆进内存。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omy_remote::{Capabilities, Error as RemoteError, RemoteStore};
use tokio::io::AsyncWriteExt as _;

use crate::commands::{CmdError, CmdResult, Shared};
use crate::place_cmds::{build_remote_source_with_cache, RemoteFileRef};
use crate::place_files::RemoteCache;
use crate::places::PlaceRegistry;
use crate::transfers::{TaskHandle, TaskKind, TaskState, Transfers};

/// 每次从源端读取 8 MiB。一次请求可合并多个缓存块，减少高速链路上的调度与请求开销。
const COPY_CHUNK: u64 = 8 * 1024 * 1024;
/// 下载与上传之间最多积压 64 MiB，吸收短时速率抖动；达到上限后仍会背压下载。
const PIPE_CAPACITY: usize = 64 * 1024 * 1024;

/// 远程来源采用哪种中转方式。
#[derive(Debug, Clone, Copy, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CopyMode {
    /// 通过既有版本化块缓存读取，并在首块前标记为永久。
    PermanentCache,
    /// 只经过固定容量内存管道，不写本地缓存。
    Memory,
}

/// 发起一次远程到远程复制所需的完整描述。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RemoteCopyRequest {
    pub source: RemoteFileRef,
    pub target_place_id: String,
    pub target_dir: String,
    pub target_name: String,
    pub mode: CopyMode,
}

/// 目标端按能力选择的提交策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitPolicy {
    /// 写临时对象，成功后改为最终名；失败能精确删除临时对象。
    TemporaryThenRename,
    /// 直接写最终名；失败时若目标 ID 可预知且支持删除，则尽力清理。
    Direct,
}

fn commit_policy(caps: Capabilities) -> CommitPolicy {
    // 只有 rename+delete 同时成立才走临时名。仅能改名、不能删除时，一旦上传或
    // 改名失败会留下永远清不掉的临时对象，反而比直接写最终名更糟。
    if caps.write && caps.rename && caps.delete {
        CommitPolicy::TemporaryThenRename
    } else {
        CommitPolicy::Direct
    }
}

/// 复制任务重试登记。只驻留内存，不把文件名和远程路径写入额外日志或配置。
#[derive(Debug, Default)]
pub struct CopyRetryStore {
    reqs: Mutex<HashMap<u64, RemoteCopyRequest>>,
}

impl CopyRetryStore {
    pub fn remember(&self, id: u64, req: RemoteCopyRequest) {
        if let Ok(mut reqs) = self.reqs.lock() {
            reqs.insert(id, req);
        }
    }

    #[must_use]
    pub fn get(&self, id: u64) -> Option<RemoteCopyRequest> {
        self.reqs.lock().ok().and_then(|reqs| reqs.get(&id).cloned())
    }

    pub fn forget(&self, id: u64) {
        if let Ok(mut reqs) = self.reqs.lock() {
            reqs.remove(&id);
        }
    }
}

/// 启动一条远程到远程复制任务。
#[tauri::command]
pub async fn remote_copy(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    xfer: tauri::State<'_, Arc<Transfers>>,
    retries: tauri::State<'_, Arc<CopyRetryStore>>,
    req: RemoteCopyRequest,
) -> CmdResult<u64> {
    let keks = crate::place_keys::unlock_keks(&state);
    crate::telegram_cmds::ensure_connected(&reg, &req.source.place_id, &keks).await?;
    if req.target_place_id != req.source.place_id {
        crate::telegram_cmds::ensure_connected(&reg, &req.target_place_id, &keks).await?;
    }
    validate_request(&reg, &req).await?;

    let source_name = req
        .source
        .name
        .clone()
        .unwrap_or_else(|| req.source.path.clone());
    let target = reg
        .get(&req.target_place_id)
        .map_or_else(|| req.target_place_id.clone(), |place| place.name.clone());
    let handle = xfer.start(&app, TaskKind::Upload, source_name, target, req.source.size);
    let id = handle.id();
    retries.remember(id, req.clone());
    spawn_copy(
        app,
        Arc::clone(&reg),
        Arc::clone(&cache),
        Arc::clone(&xfer),
        req,
        handle,
    );
    Ok(id)
}

/// 重跑一条失败的复制任务。Telegram 作为目标时会从头上传；永久缓存模式下，
/// 源端已经缓存的分块仍会直接命中，因此不重复下载。
pub fn retry_copy(
    app: tauri::AppHandle,
    reg: Arc<PlaceRegistry>,
    cache: Arc<RemoteCache>,
    xfer: Arc<Transfers>,
    retries: &CopyRetryStore,
    id: u64,
) -> CmdResult<u64> {
    let req = retries
        .get(id)
        .ok_or_else(|| CmdError::code("remote_retry_expired"))?;
    let handle = xfer
        .reset_running(&app, id)
        .ok_or_else(|| CmdError::code("remote_retry_not_failed"))?;
    spawn_copy(app, reg, cache, xfer, req, handle);
    Ok(id)
}

async fn validate_request(reg: &PlaceRegistry, req: &RemoteCopyRequest) -> CmdResult<()> {
    if req.source.size == 0 || req.target_name.trim().is_empty() {
        return Err(CmdError::code("remote_copy_invalid"));
    }
    let target = reg
        .get(&req.target_place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let caps = target
        .store
        .effective_capabilities(&req.target_dir)
        .await
        .map_err(remote_cmd_error)?;
    if !caps.write {
        return Err(CmdError::code("remote_readonly"));
    }
    if req.source.place_id == req.target_place_id
        && let Some(target_id) = target.store.child_id(&req.target_dir, &req.target_name)
        && target_id == req.source.path
    {
        return Err(CmdError::code("remote_copy_same_target"));
    }
    // 路径型目标不能静默覆盖同名对象。Telegram 允许同名消息，且列表分页无法
    // 证明“整个对话里不存在同名”，所以只对可预知 child_id 的存储做这项检查。
    if target.store.child_id(&req.target_dir, &req.target_name).is_some() {
        let entries = target
            .store
            .list(&req.target_dir)
            .await
            .map_err(remote_cmd_error)?;
        if entries.iter().any(|entry| entry.name == req.target_name) {
            return Err(CmdError::code("target_exists"));
        }
    }
    Ok(())
}

fn spawn_copy(
    app: tauri::AppHandle,
    reg: Arc<PlaceRegistry>,
    cache: Arc<RemoteCache>,
    xfer: Arc<Transfers>,
    req: RemoteCopyRequest,
    handle: Arc<TaskHandle>,
) {
    tauri::async_runtime::spawn(async move {
        let result = execute_copy(&app, &reg, &cache, &xfer, &req, &handle).await;
        if handle.is_canceled() {
            return;
        }
        match result {
            Ok(()) => xfer.finish(&app, handle.id(), TaskState::Done),
            Err(code) => xfer.finish(&app, handle.id(), TaskState::failed(code)),
        }
    });
}

async fn execute_copy(
    app: &tauri::AppHandle,
    reg: &PlaceRegistry,
    cache: &RemoteCache,
    xfer: &Arc<Transfers>,
    req: &RemoteCopyRequest,
    handle: &Arc<TaskHandle>,
) -> Result<(), String> {
    let target = reg
        .get(&req.target_place_id)
        .ok_or_else(|| String::from("remote_no_such_place"))?;
    let caps = target
        .store
        .effective_capabilities(&req.target_dir)
        .await
        .map_err(|e| remote_code(&e).to_owned())?;
    if !caps.write {
        return Err(String::from("remote_readonly"));
    }

    let cache_snapshot = match req.mode {
        CopyMode::PermanentCache => cache.snapshot(),
        CopyMode::Memory => None,
    };
    let source = build_remote_source_with_cache(reg, cache_snapshot, &req.source)
        .await
        .map_err(|e| e.code)?;
    if req.mode == CopyMode::PermanentCache {
        // 标记必须先于首块读取：之后 fetch_block 才会直接写永久层。
        source.pin().map_err(|e| remote_code(&e).to_owned())?;
    }

    let policy = commit_policy(caps);
    let upload_name = match policy {
        CommitPolicy::TemporaryThenRename => {
            format!(".omy-upload-{}-{}", uuid::Uuid::new_v4(), req.target_name)
        }
        CommitPolicy::Direct => req.target_name.clone(),
    };
    let cleanup_id = target.store.child_id(&req.target_dir, &upload_name);

    let (mut writer, reader) = tokio::io::duplex(PIPE_CAPACITY);
    let total = req.source.size;
    let producer_handle = Arc::clone(handle);
    let app_for_progress = app.clone();
    let xfer_for_progress = Arc::clone(xfer);

    // RemoteSource 在内部用 runtime Handle 驱动异步 provider，因此整段读取必须
    // 离开 async worker。写端进入有界 duplex；队列满时 write_all 阻塞，背压下载。
    let producer = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let runtime = tokio::runtime::Handle::current();
        let mut offset = 0u64;
        while offset < total {
            if producer_handle.is_canceled() {
                return Err(String::from("remote_canceled"));
            }
            while producer_handle.is_paused() {
                if producer_handle.is_canceled() {
                    return Err(String::from("remote_canceled"));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let want = (total - offset).min(COPY_CHUNK);
            let bytes = source
                .read_raw_range(offset, want)
                .map_err(|_| String::from("remote_network"))?;
            if bytes.is_empty() {
                return Err(String::from("remote_copy_short_read"));
            }
            runtime
                .block_on(writer.write_all(&bytes))
                .map_err(|_| String::from("remote_copy_target_closed"))?;
            offset = offset.saturating_add(bytes.len() as u64);
            producer_handle.set_done(offset);
            xfer_for_progress.tick(&app_for_progress, &producer_handle);
        }
        runtime
            .block_on(writer.shutdown())
            .map_err(|_| String::from("remote_copy_target_closed"))
    });

    let upload_result = target
        .store
        .write_stream(&req.target_dir, &upload_name, total, Box::new(reader))
        .await;
    let producer_result = producer
        .await
        .map_err(|_| String::from("remote_copy_worker_failed"))?;

    let entry = match upload_result {
        Ok(entry) => entry,
        Err(error) => {
            let cleaned = cleanup_partial(target.store.as_ref(), caps, cleanup_id.as_deref()).await;
            return Err(if cleaned || cleanup_id.is_none() {
                remote_code(&error).to_owned()
            } else {
                String::from("remote_copy_residue")
            });
        }
    };
    if let Err(code) = producer_result {
        let cleaned = cleanup_partial(target.store.as_ref(), caps, Some(&entry.id)).await;
        return Err(if cleaned { code } else { String::from("remote_copy_residue") });
    }
    xfer.tick(app, handle);

    if policy == CommitPolicy::TemporaryThenRename
        && let Err(e) = target.store.rename(&entry.id, &req.target_name).await
    {
        let cleaned = cleanup_partial(target.store.as_ref(), caps, Some(&entry.id)).await;
        return Err(if cleaned {
            remote_code(&e).to_owned()
        } else {
            String::from("remote_copy_residue")
        });
    }
    Ok(())
}

async fn cleanup_partial(
    store: &omy_remote::PlaceStore,
    caps: Capabilities,
    id: Option<&str>,
) -> bool {
    if !caps.delete {
        return false;
    }
    let Some(id) = id else {
        return false;
    };
    store.delete(id).await.is_ok()
}

fn remote_cmd_error(e: RemoteError) -> CmdError {
    CmdError::code(remote_code(&e))
}

fn remote_code(e: &RemoteError) -> &'static str {
    match e {
        RemoteError::Unauthorized => "remote_unauthorized",
        RemoteError::Forbidden => "remote_forbidden",
        RemoteError::NotFound(_) => "remote_not_found",
        RemoteError::RateLimited => "remote_rate_limited",
        RemoteError::Unsupported(_) => "remote_unsupported",
        RemoteError::Network(_) => "remote_network",
        RemoteError::Conflict => "remote_conflict",
        RemoteError::Protocol(_) | RemoteError::Io(_) => "remote_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_policy_requires_rename_and_delete() {
        let full = Capabilities::cloud_writable();
        assert_eq!(commit_policy(full), CommitPolicy::TemporaryThenRename);

        let no_delete = Capabilities { delete: false, ..full };
        assert_eq!(
            commit_policy(no_delete),
            CommitPolicy::Direct,
            "不能删除时不得制造可能永久残留的临时对象"
        );
        let no_rename = Capabilities { rename: false, ..full };
        assert_eq!(commit_policy(no_rename), CommitPolicy::Direct);
    }

    #[test]
    fn memory_queue_is_bounded() {
        assert_eq!(COPY_CHUNK, 8 * 1024 * 1024);
        assert_eq!(PIPE_CAPACITY, 64 * 1024 * 1024);
        assert!(PIPE_CAPACITY >= COPY_CHUNK as usize);
        assert_eq!(
            PIPE_CAPACITY % COPY_CHUNK as usize,
            0,
            "完整窗口能留在管道中，避免固定产生半窗口尾巴"
        );
    }
}
