//! 共享业务层：GUI 与 CLI 复用的远程文件操作编排。
//!
//! # 这里放什么、不放什么
//!
//! - **放**：跨界面一致的编排——路径安全校验、按有效能力做门禁、临时名提交与失败
//!   清理、远程密文流式解密到本地的机械过程（`.part` + 原子改名 + 二次检查）。
//! - **不放**：字节搬运（那是 driver 的事）、界面/CLI 的展示与错误码映射、
//!   Tauri 会话里取 KEK 那一类宿主相关逻辑。
//!
//! # 为什么必须只有一份
//!
//! 早先 GUI 在 `remote_copy.rs` 里写了「临时名 + rename + 清理」，CLI 在
//! `files.rs` 里又写了一遍「直接 write_stream」。两套的能力判断、失败清理迟早
//! 走岔——而 AGENTS.md 的「同一逻辑不允许两处实现」正是要拦这个。本模块把编排
//! 收成一份，driver 之上的所有调用都走这里。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use omy_core::crypto::Kek;
use omy_core::source::read_source_range;

use crate::cache::BlockCache;
use crate::source::RemoteSource;
use crate::store::{Entry, RemoteStore, UploadMediaHint};
use crate::{Capabilities, Error, Result};

// ---------------------------------------------------------------------------
// 路径安全
// ---------------------------------------------------------------------------

/// 远程路径里禁止出现 `..` 段。
///
/// WebDAV 服务端未必做路径规范化，放任用户传 `/backup/../其它` 可能越级写到
/// 预料之外的目录；这类「路径穿越」必须在客户端这一层直接拒绝，而不是等服务端
/// 规范化后写到意外位置、命令还「成功」。
///
/// # Errors
///
/// 任一段为 `..` 时返回 [`Error::Protocol`]。
pub fn ensure_safe_remote(path: &str) -> Result<()> {
    if path.split('/').any(|seg| seg == "..") {
        return Err(Error::Protocol(format!(
            "远程路径不得包含 '..'：{path}"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 能力门禁
// ---------------------------------------------------------------------------

/// 按「有效能力」要求某个写能力，不满足就报 [`Error::Unsupported`]。
///
/// 调用方应先用 [`RemoteStore::effective_capabilities`] 取目录级能力（而不是
/// 位置级上界），再传入对应位。在只读对话上调用 `delete` 必须在这里就被拦住，
/// 而不是把字节发出去再被服务端拒。
///
/// # Errors
///
/// `flag` 为假时返回 [`Error::Unsupported`]，操作名写进错误信息，方便用户判断
/// 「是位置不支持，还是权限不够」。
pub fn require_capability(want: &'static str, flag: bool) -> Result<()> {
    if !flag {
        return Err(Error::Unsupported(want));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 临时名提交（commit_upload）
// ---------------------------------------------------------------------------

/// 一次上传提交失败：包装底层错误，并说明是否可能残留未清理的远程对象。
///
/// # 为什么要单独包一层
///
/// 上传中途失败，用户最该知道的是「服务端上是不是还留了个半截文件」。这个信息
/// 不在任何底层错误里——底层只知道「网络断了」。把它显式带出来，界面与命令行
/// 才能统一提示「去手动删一下」，而不是静默假定清理成功。
#[derive(Debug)]
pub struct CommitError {
    /// 底层错误。
    pub inner: Error,
    /// 是否可能残留一个未清理的远程对象。
    ///
    /// `true` 时调用方必须在用户可见的信息里提示：服务端上可能留有一个半成品，
    /// 需要手动删除。无法预知 child_id 的 provider（Telegram）失败时必为 `true`。
    pub residue: bool,
}

impl std::fmt::Display for CommitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.inner)?;
        if self.residue {
            write!(f, "（服务端可能残留未清理的半成品对象，请手动检查）")?;
        }
        Ok(())
    }
}

impl std::error::Error for CommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

/// 提交策略：写临时对象再改名，还是直接写最终名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommitPolicy {
    /// 先写到临时名，确认完整后 rename 成最终名；失败能精确删除临时对象。
    TemporaryThenRename,
    /// 直接写最终名；失败时若 id 可预知且支持删除，则尽力清理。
    Direct,
}

/// 由有效能力决定提交策略。
///
/// 只有 `write + rename + delete` 同时成立才走临时名。仅能改名、不能删除时，
/// 一旦上传或改名失败会留下一个永远清不掉的临时对象，反而比直接写最终名更糟。
#[must_use]
fn commit_policy(caps: Capabilities) -> CommitPolicy {
    if caps.write && caps.rename && caps.delete {
        CommitPolicy::TemporaryThenRename
    } else {
        CommitPolicy::Direct
    }
}

/// 进程内递增计数，用于生成不与并发上传撞名的临时对象名。
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// 生成一个临时对象名：`.omy-upload-<随机>-<最终名>`。
///
/// 不引入 uuid 依赖；用纳秒 + 进程 id + 单调计数足以避免同进程并发上传撞名，
/// 跨进程撞名的概率也极低（且即便撞名，临时名多写一次也无害）。
fn temp_name(final_name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    format!(".omy-upload-{nanos:x}-{pid:x}-{seq:x}-{final_name}")
}

/// 尽力清理一个远程对象；返回是否清理成功。
///
/// 只在「有效能力允许删除」且「对象 id 可预知」时才有机会清理。Telegram 这类
/// 发送前无法预知 id 的 provider，清理直接落空——调用方据此报残留风险。
pub async fn cleanup_remote<S: RemoteStore + ?Sized>(
    store: &S,
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

/// 提交一次上传：按能力选择「临时名 + rename」或「直接写」，任一步失败尽力清理。
///
/// # 为什么需要它
///
/// 直接把字节写到最终文件名，中途失败（网络断、进程死、管道破）会留下一个半截
/// 文件，而它**顶着最终名**——下次用户以为文件已经传好了。能改名的位置先写到一个
/// 临时名、确认完整后再 MOVE 成最终名，失败时删掉临时对象即可，绝不污染最终名。
///
/// # 参数
///
/// - `reader` 是待上传字节的读端（通常来自一条 duplex 管道：调用方在另一头喂数据）。
/// - `hint` 是媒体提示；`None` 即普通上传（默认实现会落到 `write_stream`）。
///
/// # 失败语义
///
/// - 写入失败：若能定位 id 且可删除，尽力删除；`residue` 表明是否清干净了。
/// - 临时名改名失败：删除已上传的临时对象，再返回底层错误。
/// - 直接写 provider（无 rename）：写入失败后若 `child_id` 可定位则尽力删，
///   否则错误里明确残留风险。
pub async fn commit_upload<S>(
    store: &S,
    caps: Capabilities,
    dir_id: &str,
    final_name: &str,
    size: u64,
    reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    hint: Option<&UploadMediaHint>,
) -> std::result::Result<Entry, CommitError>
where
    S: RemoteStore + ?Sized,
{
    let policy = commit_policy(caps);
    let upload_name = match policy {
        CommitPolicy::TemporaryThenRename => temp_name(final_name),
        CommitPolicy::Direct => final_name.to_owned(),
    };
    let cleanup_id = store.child_id(dir_id, &upload_name);

    let entry = match store
        .write_stream_with_hint(dir_id, &upload_name, size, reader, hint)
        .await
    {
        Ok(e) => e,
        Err(inner) => {
            let cleaned = cleanup_remote(store, caps, cleanup_id.as_deref()).await;
            return Err(CommitError {
                inner,
                residue: !cleaned,
            });
        }
    };

    // 临时名提交：确认完整后改名到最终名。改名失败必须删掉临时对象，
    // 否则会留下一个永远是临时名的残留。
    if policy == CommitPolicy::TemporaryThenRename {
        if let Err(inner) = store.rename(&entry.id, final_name).await {
            let cleaned = cleanup_remote(store, caps, Some(&entry.id)).await;
            return Err(CommitError {
                inner,
                residue: !cleaned,
            });
        }
        // 返回给调用方的是「最终名」那一份，而不是写时用的临时名。
        // id 也换成按最终名可预知的路径（WebDAV MOVE 后资源 URL 变了）。
        return Ok(Entry {
            name: final_name.to_owned(),
            id: store
                .child_id(dir_id, final_name)
                .unwrap_or_else(|| entry.id.clone()),
            ..entry
        });
    }
    Ok(entry)
}

// ---------------------------------------------------------------------------
// 远程文件头读取
// ---------------------------------------------------------------------------

/// 头部在缓存里占用的「块号」。
///
/// 与正文块**共用同一个 `BlockCache`**，但键空间互不相交：正文键是
/// `<id>\u{1}<版本哈希>`，头部键是下面的 [`header_cache_key`]。
pub const HEADER_BLOCK: u64 = 0;

/// 头部读取的缓存键。
///
/// 键里含 size：远程同名文件被覆盖更新后长度多半会变，旧头部随之失效；
/// 恰好等长的覆盖是已知取舍（见 `RemoteSource::new_plain` 的同类说明）。
#[must_use]
pub fn header_cache_key(id: &str, size: u64) -> String {
    // 用控制字符分隔，正常 id（WebDAV 路径或 `tg:<对话>:<消息>`）里不会出现，
    // 与 RemoteSource::cache_key 的分隔符保持一致
    format!("{id}\u{1}hdr{size}")
}

/// 读到足以 `open` 的完整文件头，优先走密文块缓存。
///
/// 识别窗口（前 `MIN_PROBE_SIZE` 字节）通常已覆盖头部；带缩略图 / 压缩索引的
/// TLV 区可能更长，故解析出 `header_len` 后按需补读。补读之后才写缓存——
/// 存半截头部会让下次命中得到一段不完整字节，而 `open` 报成「文件损坏」，
/// 那个症状完全指不到缓存。
///
/// `cache` 为 `None` 时退化成每次都走网络——那是缓存目录建不起来时的既有降级，
/// 不该让识别本身失败。
pub async fn fetch_header<S: RemoteStore + ?Sized>(
    store: &S,
    id: &str,
    size: u64,
    cache: Option<&BlockCache>,
    place_id: &str,
) -> Result<Vec<u8>> {
    let key = header_cache_key(id, size);

    if size > 0
        && let Some(c) = cache
            && let Some(hit) = c.get(place_id, &key, HEADER_BLOCK)
    {
        return Ok(hit);
    }

    let probe_len = size.min(omy_core::scan::MIN_PROBE_SIZE as u64);
    let mut buf = if probe_len == 0 {
        Vec::new()
    } else {
        store.read_range(id, 0, probe_len).await?
    };

    if let Ok(h) = omy_core::file::peek_header(&buf) {
        let need = u64::from(h.header_len);
        if need <= size && (buf.len() as u64) < need {
            let extra = store
                .read_range(id, buf.len() as u64, need - buf.len() as u64)
                .await?;
            buf.extend_from_slice(&extra);
        }
    }

    if size > 0
        && let Some(c) = cache
    {
        c.put(place_id, &key, HEADER_BLOCK, &buf);
    }
    Ok(buf)
}

// ---------------------------------------------------------------------------
// 远程密文流式解密到本地
// ---------------------------------------------------------------------------

/// 流式解密到本地时每次写出的块大小。
const LOCAL_WRITE_CHUNK: u64 = 1024 * 1024;

/// 远程 .omy 流式解密到本地目录的结果。
#[derive(Debug, Clone)]
pub struct DecryptLocalOutcome {
    /// 最终写出的明文文件路径。
    pub saved_path: PathBuf,
    /// 明文文件名（取自头部 TLV，已做路径清洗）。
    pub name: String,
    /// 写出的明文字节数。
    pub bytes: u64,
}

/// 把远程 .omy **流式解密到本地目录**。
///
/// 调用方须已先用 [`fetch_header`] 取到头部、用 `omy_core::file::open` 确认可解
/// 锁且非容器，并从头部 TLV 取出并清洗好 `safe_name`、`plaintext_size`。
/// 本函数只负责机械地把明文流写到本地：
///
/// - 先写 `<safe>.part`，中途失败/取消不会留下一个看起来完整的半成品；
/// - 全程走 `RemoteSource`（块对齐 + 密文缓存），不另写一套解密读取；
/// - 改名前再查一次目标存在性，堵住并发下的覆盖；
/// - 任一步失败都尽力删除 `.part`。
///
/// `force` 为 `false` 时，目标已存在直接返回 [`Error::Conflict`]。
///
/// # Errors
///
/// 写出失败、解密流不完整、并发覆盖冲突时返回。
pub async fn decrypt_stream_to_local<S>(
    store: Arc<S>,
    place_id: &str,
    id: &str,
    total_ct_size: u64,
    header: Vec<u8>,
    keks: Vec<Kek>,
    cache: Option<BlockCache>,
    dest_dir: &Path,
    safe_name: &str,
    plaintext_size: u64,
    force: bool,
    rt: tokio::runtime::Handle,
) -> Result<DecryptLocalOutcome>
where
    S: RemoteStore + 'static,
{
    let final_path = dest_dir.join(safe_name);
    if !force && final_path.exists() {
        return Err(Error::Conflict);
    }
    let part_path = dest_dir.join(format!("{safe_name}.part"));

    let store2 = Arc::clone(&store);
    let place_id2 = place_id.to_owned();
    let id2 = id.to_owned();
    let header2 = header.clone();
    let safe_name_owned = safe_name.to_owned();
    let part_cleanup = part_path.clone();
    let final_check = final_path.clone();

    // RemoteSource 的读方法内部用保存的 Handle block_on 网络请求，
    // 必须在阻塞线程里调（async 线程里 block_on 当前 runtime 会 panic）。
    let join = tokio::task::spawn_blocking(move || -> Result<DecryptLocalOutcome> {
        let source = RemoteSource::new(
            store2,
            place_id2,
            id2,
            &header2,
            total_ct_size,
            cache,
            rt,
        )
        .map_err(|e| Error::Protocol(e.to_string()))?;
        let opened = omy_core::file::open(&header2, &keks)
            .map_err(|_| Error::Protocol("密码打不开或文件损坏".into()))?;

        use std::io::Write as _;
        let _ = std::fs::remove_file(&part_path);
        let mut out = std::fs::File::create(&part_path).map_err(Error::Io)?;

        let mut done: u64 = 0;
        while done < plaintext_size {
            let want = (plaintext_size - done).min(LOCAL_WRITE_CHUNK);
            let chunk = read_source_range(&source, &opened, done, want)
                .map_err(|e| Error::Protocol(e.to_string()))?;
            if chunk.is_empty() {
                return Err(Error::Protocol("解密流不完整".into()));
            }
            out.write_all(&chunk).map_err(Error::Io)?;
            done = done.saturating_add(chunk.len() as u64);
        }
        out.sync_all().map_err(Error::Io)?;
        drop(out);

        // rename 前再查一次存在性，堵住并发下的覆盖
        if !force && final_check.exists() {
            let _ = std::fs::remove_file(&part_path);
            return Err(Error::Conflict);
        }
        std::fs::rename(&part_path, &final_check).map_err(Error::Io)?;
        Ok(DecryptLocalOutcome {
            saved_path: final_check,
            name: safe_name_owned,
            bytes: done,
        })
    })
    .await
    .map_err(|_| Error::Protocol("解密任务异常".to_string()))?;

    match join {
        Ok(r) => Ok(r),
        Err(e) => {
            let _ = std::fs::remove_file(&part_cleanup);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Entry;

    /// 拼接目录与名字，与 WebDAV 驱动同语义。
    fn join(dir: &str, name: &str) -> String {
        let d = dir.trim_end_matches('/');
        if d.is_empty() {
            format!("/{name}")
        } else {
            format!("{d}/{name}")
        }
    }

    /// 记录写入名字的假存储，用于验证临时名提交的策略与清理。
    #[derive(Default)]
    struct RecStore {
        deleted: std::sync::Mutex<Vec<String>>,
        renamed: std::sync::Mutex<Vec<(String, String)>>,
        /// 实际上传用的名字（临时名或最终名）。
        uploaded: std::sync::Mutex<Vec<String>>,
        /// 注入一次写入失败。
        fail_write: std::sync::atomic::AtomicBool,
        caps: std::sync::Mutex<Capabilities>,
    }

    impl RecStore {
        fn writable() -> Self {
            Self {
                caps: std::sync::Mutex::new(Capabilities::cloud_writable()),
                ..Default::default()
            }
        }
    }

    impl RemoteStore for RecStore {
        fn capabilities(&self) -> Capabilities {
            *self.caps.lock().unwrap()
        }
        fn describe(&self) -> String {
            String::from("rec")
        }
        async fn list(&self, _dir: &str) -> Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        async fn read_range(&self, _id: &str, _o: u64, _l: u64) -> Result<Vec<u8>> {
            Ok(Vec::new())
        }
        fn child_id(&self, dir_id: &str, name: &str) -> Option<String> {
            Some(join(dir_id, name))
        }
        async fn delete(&self, id: &str) -> Result<()> {
            self.deleted.lock().unwrap().push(id.to_owned());
            Ok(())
        }
        async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
            self.renamed
                .lock()
                .unwrap()
                .push((id.to_owned(), new_name.to_owned()));
            Ok(())
        }
        async fn write_stream_with_hint(
            &self,
            dir_id: &str,
            name: &str,
            _size: u64,
            _reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            _hint: Option<&UploadMediaHint>,
        ) -> Result<Entry> {
            self.uploaded.lock().unwrap().push(name.to_owned());
            if self
                .fail_write
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(Error::Network("boom".into()));
            }
            Ok(Entry {
                id: join(dir_id, name),
                name: name.to_owned(),
                is_dir: false,
                size: Some(3),
                mtime: None,
                etag: None,
                thumb: None,
                media_tab: None,
            })
        }
    }

    /// 临时名策略需要 write+rename+delete 三者齐全；缺任一退化成直接写。
    ///
    /// 不这样会怎样：只能改名不能删除时，改名失败会留下永远清不掉的临时对象，
    /// 比直接写最终名更糟。
    #[test]
    fn commit_policy_requires_rename_and_delete() {
        let full = Capabilities::cloud_writable();
        assert_eq!(
            commit_policy(full),
            CommitPolicy::TemporaryThenRename,
            "write+rename+delete 齐全才走临时名"
        );

        let no_delete = Capabilities {
            delete: false,
            ..full
        };
        assert_eq!(commit_policy(no_delete), CommitPolicy::Direct);
        let no_rename = Capabilities {
            rename: false,
            ..full
        };
        assert_eq!(commit_policy(no_rename), CommitPolicy::Direct);
    }

    /// 临时名提交成功：先写临时对象，再 rename 到最终名。
    #[tokio::test]
    async fn temp_name_commit_renames_to_final() {
        let s = RecStore::writable();
        let e = commit_upload(
            &s,
            s.capabilities(),
            "/d",
            "a.omy",
            3,
            Box::new(&b"abc"[..]),
            None,
        )
        .await
        .expect("提交成功");

        assert_eq!(e.name, "a.omy");
        let up = s.uploaded.lock().unwrap();
        assert_eq!(up.len(), 1);
        assert!(up[0].starts_with(".omy-upload-"), "应先写临时名：{}", up[0]);
        assert_ne!(up[0], "a.omy");
        let renamed = s.renamed.lock().unwrap();
        assert_eq!(renamed.len(), 1, "临时名必须 rename 成最终名");
        assert_eq!(renamed[0].1, "a.omy");
        assert!(s.deleted.lock().unwrap().is_empty(), "rename 成功后不应删除");
    }

    /// 写入失败且可定位 id 时必须尽力清理，residue 为 false。
    #[tokio::test]
    async fn failed_write_is_cleaned_up() {
        let s = RecStore::writable();
        s.fail_write
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let err = commit_upload(
            &s,
            s.capabilities(),
            "/d",
            "a.omy",
            3,
            Box::new(&b"abc"[..]),
            None,
        )
        .await
        .expect_err("写入应失败");
        assert!(
            !err.residue,
            "可定位 id 且可删除时应清理干净，residue 应为 false"
        );
        assert_eq!(
            s.deleted.lock().unwrap().len(),
            1,
            "失败写入的临时对象应被删除"
        );
    }

    /// 不支持 rename 的 provider 走直接写：写入用最终名，不改名。
    #[tokio::test]
    async fn direct_policy_writes_final_name() {
        let s = RecStore::writable();
        *s.caps.lock().unwrap() = Capabilities {
            rename: false,
            delete: false,
            ..Capabilities::cloud_writable()
        };
        let caps = s.capabilities();
        let e = commit_upload(&s, caps, "/d", "a.omy", 3, Box::new(&b"abc"[..]), None)
            .await
            .expect("直接写成功");
        assert_eq!(e.name, "a.omy");
        let up = s.uploaded.lock().unwrap();
        assert_eq!(up[0], "a.omy", "无 rename 能力应直接写最终名");
        assert!(s.renamed.lock().unwrap().is_empty(), "无 rename 不应改名");
    }

    /// 直接写 provider 失败且不可删除时，错误要明确残留风险（residue=true）。
    #[tokio::test]
    async fn direct_write_failure_reports_residue() {
        let s = RecStore::writable();
        *s.caps.lock().unwrap() = Capabilities {
            rename: false,
            delete: false,
            ..Capabilities::cloud_writable()
        };
        s.fail_write
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let caps = s.capabilities();
        let err = commit_upload(&s, caps, "/d", "a.omy", 3, Box::new(&b"abc"[..]), None)
            .await
            .expect_err("写入应失败");
        assert!(
            err.residue,
            "无 delete 能力、无法清理时必须报残留风险"
        );
        assert!(s.deleted.lock().unwrap().is_empty(), "无 delete 不应尝试删除");
    }

    /// 临时名改名失败：必须删除已上传的临时对象。
    #[tokio::test]
    async fn rename_failure_deletes_temp_object() {
        struct RenameFail(RecStore);
        impl RemoteStore for RenameFail {
            fn capabilities(&self) -> Capabilities {
                self.0.capabilities()
            }
            fn describe(&self) -> String {
                self.0.describe()
            }
            async fn list(&self, d: &str) -> Result<Vec<Entry>> {
                self.0.list(d).await
            }
            async fn read_range(&self, id: &str, o: u64, l: u64) -> Result<Vec<u8>> {
                self.0.read_range(id, o, l).await
            }
            fn child_id(&self, d: &str, n: &str) -> Option<String> {
                self.0.child_id(d, n)
            }
            async fn delete(&self, id: &str) -> Result<()> {
                self.0.delete(id).await
            }
            async fn write_stream_with_hint(
                &self,
                dir_id: &str,
                name: &str,
                size: u64,
                reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>,
                hint: Option<&UploadMediaHint>,
            ) -> Result<Entry> {
                self.0
                    .write_stream_with_hint(dir_id, name, size, reader, hint)
                    .await
            }
            async fn rename(&self, id: &str, _n: &str) -> Result<()> {
                self.0.rename(id, "_").await.ok();
                Err(Error::Network("rename boom".into()))
            }
        }
        let s = RenameFail(RecStore::writable());
        let err = commit_upload(
            &s,
            s.capabilities(),
            "/d",
            "a.omy",
            3,
            Box::new(&b"abc"[..]),
            None,
        )
        .await
        .expect_err("rename 应失败");
        assert!(!err.residue, "改名失败后临时对象应被删除");
        assert_eq!(s.0.deleted.lock().unwrap().len(), 1);
    }

    /// 头部缓存键要带 hdr 前缀与 size，且与正文键不撞。
    #[test]
    fn header_cache_key_is_namespaced() {
        let k = header_cache_key("/a.omy", 4096);
        assert!(k.contains("\u{1}hdr"), "头部键要有自己的前缀");
        assert_ne!(
            k,
            header_cache_key("/a.omy", 8192),
            "size 变了键必须变，否则覆盖更新后命中旧头部"
        );
        assert_ne!(k, "/a.omy\u{1}plain4096", "正文键与头部键不能撞");
    }

    /// `..` 段必须被拒绝。
    #[test]
    fn dotdot_is_rejected() {
        assert!(ensure_safe_remote("/a/b").is_ok());
        assert!(ensure_safe_remote("/backup/../etc").is_err());
        assert!(ensure_safe_remote("/a/../b").is_err());
    }
}
