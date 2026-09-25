//! WebDAV 文件的外部只读/编辑会话。
//!
//! 外部打开必须把明文落进应用私有目录，这是一条显式例外：只有用户主动选择
//! “只读打开”或“可编辑打开”才创建；可编辑会额外持久化远端版本基线与本地指纹。Android 的
//! FileProvider 只暴露这个目录；FileObserver 只写脏标记，真正同步始终由 Rust
//! 定时器串行完成。

use crate::commands::{CmdError, CmdResult, Shared};
use crate::places::PlaceRegistry;
use omy_core::crypto::Kek;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::Emitter as _;

const STATE_VERSION: u32 = 1;
const SYNC_EVENT: &str = "external-edit://sync";

#[derive(Debug, Clone)]
struct Roots {
    files: PathBuf,
    state: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    version: u32,
    id: String,
    place_id: String,
    remote_path: String,
    display_name: String,
    mime: String,
    encrypted: bool,
    editable: bool,
    remote_size: u64,
    revision: String,
    local_file: String,
    source_file: Option<String>,
    #[serde(default)]
    pending_file: Option<String>,
    #[serde(default)]
    pending_fingerprint: Option<String>,
    synced_fingerprint: String,
    dirty: bool,
    conflict: bool,
    last_error: Option<String>,
    updated_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    #[serde(default)]
    sessions: Vec<Session>,
}

/// 外部编辑状态管理器。
pub struct ExternalEdits {
    roots: Mutex<Option<Roots>>,
    sessions: Mutex<BTreeMap<String, Session>>,
    syncing: AtomicBool,
}

impl ExternalEdits {
    #[must_use]
    pub fn new() -> Self {
        Self {
            roots: Mutex::new(None),
            sessions: Mutex::new(BTreeMap::new()),
            syncing: AtomicBool::new(false),
        }
    }

    /// 用 Tauri 解析出的应用私有目录初始化，并恢复上次会话。
    #[cfg(target_os = "android")]
    pub fn configure(&self, root: PathBuf) -> std::io::Result<()> {
        let roots = Roots {
            files: root.join("files"),
            state: root.join("state.json"),
        };
        std::fs::create_dir_all(&roots.files)?;
        if let Some(parent) = roots.state.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let loaded = load_state(&roots.state).unwrap_or_default();
        let map = loaded
            .sessions
            .into_iter()
            .filter(|s| s.version == STATE_VERSION)
            .map(|s| (s.id.clone(), s))
            .collect();
        if let Ok(mut g) = self.sessions.lock() {
            *g = map;
        }
        if let Ok(mut g) = self.roots.lock() {
            *g = Some(roots);
        }
        self.detect_changes();
        Ok(())
    }

    fn roots(&self) -> Result<Roots, CmdError> {
        self.roots
            .lock()
            .ok()
            .and_then(|g| g.clone())
            .ok_or_else(|| CmdError::code("external_edit_unavailable"))
    }

    fn persist(&self) -> Result<(), CmdError> {
        let roots = self.roots()?;
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| CmdError::code("internal"))?
            .values()
            .cloned()
            .collect();
        let bytes = serde_json::to_vec_pretty(&Stored { sessions })
            .map_err(|_| CmdError::code("external_edit_state_failed"))?;
        omy_core::fsatomic::write_atomic(&roots.state, &bytes)
            .map_err(|_| CmdError::code("external_edit_state_failed"))
    }

    /// 比对文件指纹与 FileObserver 脏标记。启动和回前台都会调用。
    pub fn detect_changes(&self) {
        let mut changed = false;
        if let Ok(mut sessions) = self.sessions.lock() {
            for s in sessions.values_mut() {
                if !s.editable {
                    continue;
                }
                let path = PathBuf::from(&s.local_file);
                let marker = dirty_marker(&path);
                let fingerprint = fingerprint_file(&path).ok();
                if marker.exists()
                    || fingerprint
                        .as_deref()
                        .is_some_and(|f| f != s.synced_fingerprint)
                {
                    s.dirty = true;
                    s.updated_at = now_secs();
                    changed = true;
                }
            }
        }
        if changed {
            let _ = self.persist();
        }
    }

    fn upsert(&self, session: Session) -> Result<(), CmdError> {
        let id = session.id.clone();
        let previous = self
            .sessions
            .lock()
            .map_err(|_| CmdError::code("internal"))?
            .insert(id.clone(), session);
        if let Err(error) = self.persist() {
            if let Ok(mut sessions) = self.sessions.lock() {
                if let Some(previous) = previous {
                    sessions.insert(id, previous);
                } else {
                    sessions.remove(&id);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn snapshot(&self) -> Vec<Session> {
        self.sessions
            .lock()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    fn update(&self, session: Session) -> Result<(), CmdError> {
        self.upsert(session)
    }
}

impl Default for ExternalEdits {
    fn default() -> Self {
        Self::new()
    }
}

/// 前端发起的外部打开请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub place_id: String,
    pub path: String,
    pub name: String,
    pub mime: String,
    pub editable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenResult {
    pub session_id: String,
    pub editable: bool,
    pub pending_sync: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncSummary {
    pub checked: usize,
    pub synced: usize,
    pub pending: usize,
    pub conflicts: usize,
    pub failed: usize,
}

/// 下载远端文件到稳定临时路径，并交给 Android FileProvider。
#[tauri::command]
pub async fn external_edit_open(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    edits: tauri::State<'_, Arc<ExternalEdits>>,
    req: OpenRequest,
) -> CmdResult<OpenResult> {
    let place = reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let webdav = place
        .store
        .as_webdav()
        .ok_or_else(|| CmdError::code("external_edit_webdav_only"))?;
    if req.editable && !webdav.config().writable {
        return Err(CmdError::code("remote_readonly"));
    }

    let session_id = stable_id(&req.place_id, &req.path);
    edits.detect_changes();
    let existing = edits.snapshot().into_iter().find(|s| s.id == session_id);
    let pending_sync = existing.as_ref().is_some_and(|s| s.dirty || s.conflict);

    let roots = edits.roots()?;
    let safe_name = omy_core::unpack::sanitize_filename(&req.name);
    let session_dir = roots.files.join(&session_id);
    let local = existing
        .as_ref()
        .map(|s| PathBuf::from(&s.local_file))
        .unwrap_or_else(|| session_dir.join(&safe_name));
    let source = session_dir.join("source.omy");
    std::fs::create_dir_all(&session_dir)
        .map_err(|_| CmdError::code("external_edit_write_failed"))?;

    // 有未同步内容时绝不能重下远端覆盖本地；这也是进程被杀后恢复的关键。
    if !pending_sync || !local.is_file() {
        let (remote, revision) = webdav
            .read_with_revision(&req.path)
            .await
            .map_err(|e| remote_error(&e))?;
        let encrypted = omy_core::file::is_omy_file(&remote);
        let plaintext = if encrypted {
            let header =
                omy_core::file::peek_header(&remote).map_err(|_| CmdError::code("corrupted"))?;
            let keks: Vec<Kek> = state
                .with_session(|s| {
                    s.all_for(&header.vault_salt)
                        .into_iter()
                        .map(|c| c.kek)
                        .collect()
                })
                .unwrap_or_default();
            let remote2 = remote.clone();
            tokio::task::spawn_blocking(move || {
                let opened =
                    omy_core::file::open(&remote2, &keks).map_err(|_| CmdError::code("locked"))?;
                if opened.is_container() {
                    return Err(CmdError::code("remote_container_unsupported"));
                }
                opened
                    .decrypt_all(&remote2)
                    .map_err(|_| CmdError::code("decrypt_failed"))
            })
            .await
            .map_err(|_| CmdError::code("internal"))??
        } else {
            remote.clone()
        };
        omy_core::fsatomic::write_atomic(&local, &plaintext)
            .map_err(|_| CmdError::code("external_edit_write_failed"))?;
        if encrypted {
            omy_core::fsatomic::write_atomic(&source, &remote)
                .map_err(|_| CmdError::code("external_edit_write_failed"))?;
        }
        if req.editable && revision.is_none() {
            return Err(CmdError::code("external_edit_revision_unavailable"));
        }
        let fp =
            fingerprint_file(&local).map_err(|_| CmdError::code("external_edit_write_failed"))?;
        edits.upsert(Session {
            version: STATE_VERSION,
            id: session_id.clone(),
            place_id: req.place_id.clone(),
            remote_path: req.path.clone(),
            display_name: safe_name,
            mime: req.mime.clone(),
            encrypted,
            editable: req.editable || existing.as_ref().is_some_and(|s| s.editable),
            remote_size: remote.len() as u64,
            revision: revision.unwrap_or_default(),
            local_file: local.to_string_lossy().into_owned(),
            source_file: encrypted.then(|| source.to_string_lossy().into_owned()),
            pending_file: None,
            pending_fingerprint: None,
            synced_fingerprint: fp,
            dirty: false,
            conflict: false,
            last_error: None,
            updated_at: now_secs(),
        })?;
    } else if let Some(mut session) = existing {
        session.editable |= req.editable;
        session.mime = req.mime.clone();
        session.updated_at = now_secs();
        edits.upsert(session)?;
    }

    open_native(&app, &local, &req.mime, req.editable)?;
    Ok(OpenResult {
        session_id,
        editable: req.editable,
        pending_sync,
    })
}

/// 立即检测并同步；前端回到可见态时调用，后台定时器也复用同一实现。
#[tauri::command]
pub async fn external_edit_sync_now(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    edits: tauri::State<'_, Arc<ExternalEdits>>,
) -> CmdResult<SyncSummary> {
    Ok(sync_all(&app, &reg, &state, &edits).await)
}

/// 定时器与生命周期钩子调用的同步入口。
pub async fn sync_all(
    app: &tauri::AppHandle,
    reg: &Arc<PlaceRegistry>,
    state: &Shared,
    edits: &Arc<ExternalEdits>,
) -> SyncSummary {
    if edits.syncing.swap(true, Ordering::AcqRel) {
        return SyncSummary {
            checked: 0,
            synced: 0,
            pending: 0,
            conflicts: 0,
            failed: 0,
        };
    }
    edits.detect_changes();
    let sessions = edits.snapshot();
    let mut summary = SyncSummary {
        checked: sessions.len(),
        synced: 0,
        pending: 0,
        conflicts: 0,
        failed: 0,
    };
    for mut session in sessions {
        if !session.editable || !session.dirty || session.conflict {
            if session.dirty {
                summary.pending += 1;
            }
            if session.conflict {
                summary.conflicts += 1;
            }
            continue;
        }
        match sync_one(reg, state, edits, &mut session).await {
            Ok((revision, uploaded, fingerprint)) => {
                let pending = session.pending_file.clone();
                let current_fingerprint = fingerprint_file(Path::new(&session.local_file)).ok();
                session.revision = revision.unwrap_or_default();
                session.remote_size = uploaded.len() as u64;
                session.synced_fingerprint = fingerprint.clone();
                session.dirty = current_fingerprint
                    .as_deref()
                    .is_some_and(|f| f != fingerprint);
                session.pending_file = None;
                session.pending_fingerprint = None;
                session.conflict = false;
                session.last_error = None;
                session.updated_at = now_secs();
                if session.encrypted {
                    if let Some(source) = session.source_file.as_deref() {
                        if omy_core::fsatomic::write_atomic(Path::new(source), &uploaded).is_err() {
                            session.dirty = true;
                            session.last_error = Some(String::from("external_edit_source_failed"));
                            let _ = edits.update(session);
                            summary.failed += 1;
                            continue;
                        }
                    }
                }
                if edits.update(session.clone()).is_err() {
                    summary.failed += 1;
                    continue;
                }
                if session.dirty {
                    let _ = std::fs::write(dirty_marker(Path::new(&session.local_file)), b"1");
                } else {
                    let _ = std::fs::remove_file(dirty_marker(Path::new(&session.local_file)));
                }
                if let Some(pending) = pending {
                    let _ = std::fs::remove_file(pending);
                }
                summary.synced += 1;
            }
            Err(SyncFailure::Unchanged(fingerprint)) => {
                let pending = session.pending_file.take();
                session.pending_fingerprint = None;
                session.synced_fingerprint = fingerprint;
                session.dirty = false;
                session.last_error = None;
                if edits.update(session.clone()).is_err() {
                    summary.failed += 1;
                    continue;
                }
                let _ = std::fs::remove_file(dirty_marker(Path::new(&session.local_file)));
                if let Some(pending) = pending {
                    let _ = std::fs::remove_file(pending);
                }
            }
            Err(SyncFailure::Conflict) => {
                session.conflict = true;
                session.last_error = Some(String::from("conflict"));
                if edits.update(session).is_err() {
                    summary.failed += 1;
                } else {
                    summary.conflicts += 1;
                }
            }
            Err(SyncFailure::Pending(code)) => {
                session.last_error = Some(code);
                if edits.update(session).is_err() {
                    summary.failed += 1;
                } else {
                    summary.pending += 1;
                }
            }
            Err(SyncFailure::Failed(code)) => {
                session.last_error = Some(code);
                let _ = edits.update(session);
                summary.failed += 1;
            }
        }
    }
    edits.syncing.store(false, Ordering::Release);
    let _ = app.emit(SYNC_EVENT, &summary);
    summary
}

#[derive(Debug)]
enum SyncFailure {
    Unchanged(String),
    Conflict,
    Pending(String),
    Failed(String),
}

async fn sync_one(
    reg: &PlaceRegistry,
    state: &Shared,
    edits: &ExternalEdits,
    session: &mut Session,
) -> Result<(Option<String>, Vec<u8>, String), SyncFailure> {
    let local = tokio::fs::read(&session.local_file)
        .await
        .map_err(|_| SyncFailure::Failed(String::from("read_failed")))?;
    let fingerprint = fingerprint_bytes(&local);
    let discovered_pending = if session.encrypted {
        session
            .pending_file
            .as_deref()
            .zip(session.pending_fingerprint.as_ref())
            .map(|(path, fingerprint)| (PathBuf::from(path), fingerprint.clone()))
            .filter(|(path, _)| path.is_file())
            .or_else(|| {
                session
                    .source_file
                    .as_deref()
                    .and_then(|source| find_pending(Path::new(source), &session.synced_fingerprint))
            })
    } else {
        None
    };
    if fingerprint == session.synced_fingerprint && discovered_pending.is_none() {
        return Err(SyncFailure::Unchanged(fingerprint));
    }
    let place = reg
        .get(&session.place_id)
        .ok_or_else(|| SyncFailure::Pending(String::from("remote_no_such_place")))?;
    let webdav = place
        .store
        .as_webdav()
        .ok_or_else(|| SyncFailure::Failed(String::from("external_edit_webdav_only")))?;

    let mut transaction_fingerprint = fingerprint.clone();
    let upload = if session.encrypted {
        let source_path = session
            .source_file
            .as_deref()
            .ok_or_else(|| SyncFailure::Failed(String::from("external_edit_source_missing")))?;
        if let Some((pending, pending_fingerprint)) = discovered_pending {
            transaction_fingerprint = pending_fingerprint;
            session.pending_file = Some(pending.to_string_lossy().into_owned());
            session.pending_fingerprint = Some(transaction_fingerprint.clone());
            edits
                .update(session.clone())
                .map_err(|_| SyncFailure::Failed(String::from("external_edit_state_failed")))?;
            tokio::fs::read(&pending)
                .await
                .map_err(|_| SyncFailure::Failed(String::from("external_edit_source_missing")))?
        } else {
            let source = tokio::fs::read(source_path)
                .await
                .map_err(|_| SyncFailure::Failed(String::from("external_edit_source_missing")))?;
            let header = omy_core::file::peek_header(&source)
                .map_err(|_| SyncFailure::Failed(String::from("corrupted")))?;
            let keks: Vec<Kek> = state
                .with_session(|s| {
                    s.all_for(&header.vault_salt)
                        .into_iter()
                        .map(|c| c.kek)
                        .collect()
                })
                .unwrap_or_default();
            if keks.is_empty() {
                return Err(SyncFailure::Pending(String::from("locked")));
            }
            let encrypted = tokio::task::spawn_blocking(move || {
                let prepared = omy_media::prepare(&local, &omy_media::PrepareOptions::default());
                let meta = omy_core::reencrypt::ReplacementMetadata {
                    thumbnail: prepared.thumbnail,
                    media_meta: prepared.media_meta,
                    moov_cache: prepared.moov_cache,
                };
                let mut nonce = [0u8; 7];
                omy_core::util::fill_random(&mut nonce);
                omy_core::reencrypt::replace_plaintext_preserving_slots(
                    &source, &keks, &local, &meta, nonce,
                )
                .map(|v| v.bytes)
                .map_err(|_| SyncFailure::Failed(String::from("encrypt_failed")))
            })
            .await
            .map_err(|_| SyncFailure::Failed(String::from("internal")))??;
            let pending = pending_path(Path::new(source_path), &fingerprint);
            omy_core::fsatomic::write_atomic(&pending, &encrypted)
                .map_err(|_| SyncFailure::Failed(String::from("external_edit_source_failed")))?;
            session.pending_file = Some(pending.to_string_lossy().into_owned());
            session.pending_fingerprint = Some(fingerprint.clone());
            edits
                .update(session.clone())
                .map_err(|_| SyncFailure::Failed(String::from("external_edit_state_failed")))?;
            encrypted
        }
    } else {
        local
    };
    let expected = (!session.revision.is_empty()).then_some(session.revision.as_str());
    let revision = match webdav
        .replace_if_revision(&session.remote_path, &upload, expected)
        .await
    {
        Ok(revision) => revision,
        Err(omy_remote::Error::Conflict) => {
            // 进程可能在上次 PUT 成功后、状态落盘前被系统杀掉。此时基线 revision
            // 仍旧，但远端已经是待上传内容；逐字节确认后把它当成功恢复。
            match webdav.content_matches(&session.remote_path, &upload).await {
                Ok(true) => webdav
                    .revision(&session.remote_path)
                    .await
                    .map_err(|_| SyncFailure::Pending(String::from("remote_network")))?,
                Ok(false) => return Err(SyncFailure::Conflict),
                Err(e) if e.is_retryable() => {
                    return Err(SyncFailure::Pending(String::from("remote_network")));
                }
                Err(_) => return Err(SyncFailure::Conflict),
            }
        }
        Err(e) if e.is_retryable() => {
            return Err(SyncFailure::Pending(String::from("remote_network")));
        }
        Err(_) => return Err(SyncFailure::Failed(String::from("remote_failed"))),
    };
    Ok((revision, upload, transaction_fingerprint))
}

fn stable_id(place: &str, path: &str) -> String {
    let mut data = Vec::with_capacity(place.len() + path.len() + 1);
    data.extend_from_slice(place.as_bytes());
    data.push(0);
    data.extend_from_slice(path.as_bytes());
    omy_core::util::blake2b_256(&data)
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fingerprint_bytes(data: &[u8]) -> String {
    omy_core::util::blake2b_256(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn fingerprint_file(path: &Path) -> std::io::Result<String> {
    std::fs::read(path).map(|b| fingerprint_bytes(&b))
}

fn dirty_marker(path: &Path) -> PathBuf {
    let mut os = path.as_os_str().to_owned();
    os.push(".dirty");
    PathBuf::from(os)
}

fn pending_path(source: &Path, fingerprint: &str) -> PathBuf {
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("pending-{fingerprint}.omy"))
}

fn find_pending(source: &Path, synced_fingerprint: &str) -> Option<(PathBuf, String)> {
    let parent = source.parent()?;
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(parent).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Some(fingerprint) = name
            .strip_prefix("pending-")
            .and_then(|name| name.strip_suffix(".omy"))
        else {
            continue;
        };
        if fingerprint.len() != 64
            || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || !path.is_file()
        {
            continue;
        }
        if fingerprint == synced_fingerprint {
            // 状态已经提交、只差删除文件时被杀：这是已完成事务的孤儿，不应重放。
            let _ = std::fs::remove_file(path);
            continue;
        }
        candidates.push((path, fingerprint.to_owned()));
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    candidates.into_iter().next()
}

#[cfg(target_os = "android")]
fn load_state(path: &Path) -> std::io::Result<Stored> {
    if !path.is_file() {
        return Ok(Stored::default());
    }
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn remote_error(e: &omy_remote::Error) -> CmdError {
    let code = match e {
        omy_remote::Error::Unauthorized => "remote_unauthorized",
        omy_remote::Error::Forbidden => "remote_forbidden",
        omy_remote::Error::NotFound(_) => "remote_not_found",
        omy_remote::Error::Conflict => "external_edit_conflict",
        omy_remote::Error::RateLimited => "remote_rate_limited",
        omy_remote::Error::Unsupported(_) => "remote_unsupported",
        omy_remote::Error::Network(_) => "remote_network",
        _ => "remote_failed",
    };
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

#[cfg(target_os = "android")]
mod mobile {
    use serde::Serialize;
    use tauri::plugin::PluginHandle;

    const IDENTIFIER: &str = "org.omy.app";
    const CLASS: &str = "ExternalEditPlugin";

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct OpenArgs<'a> {
        path: &'a str,
        mime: &'a str,
        writable: bool,
    }

    #[derive(Serialize)]
    struct WatchArgs<'a> {
        path: &'a str,
    }
    #[derive(serde::Deserialize)]
    struct RootResult {
        path: String,
    }

    pub struct Plugin<R: tauri::Runtime>(PluginHandle<R>);

    impl<R: tauri::Runtime> Plugin<R> {
        fn root(&self) -> Result<std::path::PathBuf, String> {
            self.0
                .run_mobile_plugin::<RootResult>("editRoot", ())
                .map(|r| std::path::PathBuf::from(r.path))
                .map_err(|e| e.to_string())
        }

        fn watch(&self, path: &str) -> Result<(), String> {
            self.0
                .run_mobile_plugin::<serde_json::Value>("watchFile", WatchArgs { path })
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
        fn open(&self, path: &str, mime: &str, writable: bool) -> Result<(), String> {
            self.0
                .run_mobile_plugin::<serde_json::Value>(
                    "openFile",
                    OpenArgs {
                        path,
                        mime,
                        writable,
                    },
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
        }
    }

    pub fn init<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
        tauri::plugin::Builder::new("omy-external-edit")
            .setup(|app, api| {
                let handle = api.register_android_plugin(IDENTIFIER, CLASS)?;
                tauri::Manager::manage(app, Plugin(handle));
                Ok(())
            })
            .build()
    }

    pub fn root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
        use tauri::Manager as _;
        app.try_state::<Plugin<tauri::Wry>>()
            .ok_or_else(|| String::from("外部编辑插件未注册"))?
            .root()
    }

    pub fn watch(app: &tauri::AppHandle, path: &std::path::Path) -> Result<(), String> {
        use tauri::Manager as _;
        app.try_state::<Plugin<tauri::Wry>>()
            .ok_or_else(|| String::from("外部编辑插件未注册"))?
            .watch(&path.to_string_lossy())
    }
    pub fn open(
        app: &tauri::AppHandle,
        path: &std::path::Path,
        mime: &str,
        writable: bool,
    ) -> Result<(), String> {
        use tauri::Manager as _;
        app.try_state::<Plugin<tauri::Wry>>()
            .ok_or_else(|| String::from("外部编辑插件未注册"))?
            .open(&path.to_string_lossy(), mime, writable)
    }
}

#[cfg(target_os = "android")]
pub fn native_root(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    mobile::root(app)
}

#[cfg(target_os = "android")]
pub fn restore_watchers(app: &tauri::AppHandle, edits: &ExternalEdits) {
    for session in edits.snapshot().into_iter().filter(|s| s.editable) {
        let path = PathBuf::from(session.local_file);
        if path.is_file() {
            let _ = mobile::watch(app, &path);
        }
    }
}

#[cfg(target_os = "android")]
pub use mobile::init;

fn open_native(app: &tauri::AppHandle, path: &Path, mime: &str, writable: bool) -> CmdResult<()> {
    #[cfg(target_os = "android")]
    {
        mobile::open(app, path, mime, writable)
            .map_err(|e| CmdError::with("open_failed", serde_json::json!({ "detail": e })))
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, path, mime, writable);
        Err(CmdError::code("external_edit_android_only"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_id_is_stable_and_does_not_leak_path() {
        let a = stable_id("p1", "/私人/报告.docx");
        assert_eq!(a, stable_id("p1", "/私人/报告.docx"));
        assert_ne!(a, stable_id("p2", "/私人/报告.docx"));
        assert!(!a.contains("报告"), "URI 路径不能泄露远端目录");
    }

    #[test]
    fn dirty_marker_is_next_to_file() {
        assert_eq!(
            dirty_marker(Path::new("C:/cache/a.docx")),
            PathBuf::from("C:/cache/a.docx.dirty")
        );
    }

    #[test]
    fn old_state_without_pending_transaction_still_loads() {
        let json = r#"{
            "version":1,"id":"s","place_id":"p","remote_path":"/a.omy",
            "display_name":"a.txt","mime":"text/plain","encrypted":true,
            "editable":true,"remote_size":12,"revision":"etag:x",
            "local_file":"C:/work/a.txt","source_file":"C:/work/source.omy",
            "synced_fingerprint":"abc","dirty":true,"conflict":false,
            "last_error":null,"updated_at":1
        }"#;
        let session: Session = serde_json::from_str(json).expect("旧状态应继续兼容");
        assert!(
            session.pending_file.is_none(),
            "旧状态不能凭空生成待上传文件"
        );
        assert!(
            session.pending_fingerprint.is_none(),
            "旧状态不能凭空生成事务指纹"
        );
    }

    #[test]
    fn pending_transaction_is_kept_inside_session_directory() {
        assert_eq!(
            pending_path(
                Path::new("C:/external-edit/files/id/source.omy"),
                &"ab".repeat(32),
            ),
            PathBuf::from(format!(
                "C:/external-edit/files/id/pending-{}.omy",
                "ab".repeat(32)
            ))
        );
    }

    #[test]
    fn pending_discovery_drops_committed_orphan_and_keeps_new_transaction() {
        let dir = std::env::temp_dir().join(format!(
            "omy-external-edit-pending-{}-{}",
            std::process::id(),
            now_secs()
        ));
        std::fs::create_dir_all(&dir).expect("创建测试目录");
        let source = dir.join("source.omy");
        std::fs::write(&source, b"source").expect("写源文件");
        let old = "11".repeat(32);
        let new = "22".repeat(32);
        let old_pending = pending_path(&source, &old);
        let new_pending = pending_path(&source, &new);
        std::fs::write(&old_pending, b"old").expect("写已提交孤儿事务");
        std::fs::write(&new_pending, b"new").expect("写真正待处理事务");

        let found = find_pending(&source, &old).expect("应发现未完成事务");
        assert_eq!(found, (new_pending.clone(), new));
        assert!(!old_pending.exists(), "已提交事务的孤儿文件必须清理");
        assert!(new_pending.exists(), "未完成事务必须保留供重启恢复");
        let _ = std::fs::remove_dir_all(dir);
    }
}
