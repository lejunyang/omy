//! 原子文件写入与临时明文文件的生命周期管理。
//!
//! # 为什么需要这个模块
//!
//! 加密写入若在中途崩溃或断电，可能留下一个**看似完整实则损坏**的 `.omy` 文件——
//! 头部已落盘、载荷只写了一半，用户以为加密成功并删除了原文件，数据就永久丢失了。
//!
//! 本模块实现规范 §10 的五步流程：
//!
//! ```text
//! 1. 写入 <target>.tmp
//! 2. fsync(文件)
//! 3. fsync(父目录)          ← 容易遗漏，但在部分文件系统上必需
//! 4. rename(tmp → target)   ← 同文件系统内原子
//! 5. fsync(父目录)
//! ```
//!
//! # 关于父目录 fsync
//!
//! 只 fsync 文件本身是**不够的**：文件数据虽已落盘，但「目录里存在这个文件名」
//! 这条元数据可能仍在缓存中。断电后可能出现文件内容完好但目录项丢失的情况。
//! 在 ext4 的 `data=ordered` 模式、XFS、Btrfs 上这都是真实风险。
//!
//! Windows 无法直接 fsync 目录，NTFS 的 rename 本身是有日志的元数据事务，
//! 因此该步在 Windows 上跳过（见 [`sync_dir`]）。
//!
//! # 临时明文文件（旁路 L2/L3/L5）
//!
//! [`TempPlaintext`] 用于「用外部应用打开」场景。它必须处理三个旁路泄露：
//! 系统缩略图缓存、搜索索引、以及文件本身的残留。见该类型的文档。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// 原子写入使用的临时文件后缀。
pub const TMP_SUFFIX: &str = ".tmp";

/// 断点续传的进度文件后缀（移动端后台挂起场景，规范 §10）。
pub const PROGRESS_SUFFIX: &str = ".progress";

/// 把 [`io::Error`] 包装为带路径上下文的 [`Error::Io`]。
///
/// `Error::Io` 是 `#[from] io::Error` 的元组变体，本身不携带路径。
/// 而「哪个文件的哪一步失败了」对排查至关重要，因此这里把上下文拼进
/// 一个新的 `io::Error` 中，同时用 `kind()` 保留原始错误类别
/// （调用方仍可判断 `PermissionDenied`、`NotFound` 等）。
fn io_err(path: &Path, op: &'static str, e: &io::Error) -> Error {
    Error::Io(io::Error::new(e.kind(), format!("{op} failed on {}: {e}", path.display())))
}

/// 构造一个带说明的 [`Error::Io`]（无底层 io 错误时使用）。
fn io_msg(msg: String) -> Error {
    Error::Io(io::Error::other(msg))
}

/// fsync 一个目录，确保其中的目录项变更已落盘。
///
/// # 平台差异
///
/// - Unix：打开目录并 fsync。这是保证 rename 持久化的必要步骤。
/// - Windows：无法用 `File::open` 打开目录（会得到 `PermissionDenied`）。
///   NTFS 的 rename 是有日志的元数据事务，本身即持久，故跳过。
///
/// # Errors
///
/// Unix 下打开或 fsync 目录失败时返回 [`Error::Io`]。
pub fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        let _ = dir; // NTFS rename 自带日志，无需也无法 fsync 目录
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let f = File::open(dir).map_err(|e| io_err(dir, "open dir", &e))?;
        f.sync_all().map_err(|e| io_err(dir, "fsync dir", &e))?;
        Ok(())
    }
}

/// 取出父目录；没有父目录时视为当前目录。
fn parent_of(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// 原子地把 `data` 写入 `target`。
///
/// 严格执行规范 §10 的五步流程。若中途失败，会尽力清理临时文件，
/// 且 `target` 保持原样（要么是旧内容，要么不存在）——**绝不会出现半截文件**。
///
/// # Errors
///
/// 任一步 I/O 失败时返回 [`Error::Io`]，错误信息含出错的路径与操作名。
pub fn write_atomic(target: &Path, data: &[u8]) -> Result<()> {
    let mut w = AtomicWriter::create(target)?;
    w.write_all(data)?;
    w.commit()
}

/// 流式原子写入器。
///
/// 适用于加密大文件——载荷逐块产生，不必先在内存里拼出完整文件。
/// 未调用 [`AtomicWriter::commit`] 就 drop 时，临时文件会被自动删除。
///
/// # 示例
///
/// ```no_run
/// use omy_core::fsatomic::AtomicWriter;
/// # fn main() -> omy_core::Result<()> {
/// let mut w = AtomicWriter::create(std::path::Path::new("out.omy"))?;
/// w.write_all(b"header")?;
/// w.write_all(b"chunk-0")?;
/// w.commit()?;   // 不调用 commit 则临时文件被丢弃
/// # Ok(())
/// # }
/// ```
pub struct AtomicWriter {
    /// 最终目标路径。
    target: PathBuf,
    /// 临时文件路径。
    tmp: PathBuf,
    /// 打开的临时文件；`commit`/`drop` 后为 `None`。
    file: Option<File>,
    /// 已写入的字节数。
    written: u64,
}

impl std::fmt::Debug for AtomicWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtomicWriter")
            .field("target", &self.target)
            .field("written", &self.written)
            .field("open", &self.file.is_some())
            .finish()
    }
}

impl AtomicWriter {
    /// 创建临时文件并准备写入。
    ///
    /// 临时文件名为 `<target><TMP_SUFFIX>`，与目标**同目录**——这是 rename
    /// 原子性的前提（跨文件系统的 rename 不是原子操作，且多数平台直接失败）。
    ///
    /// # Errors
    ///
    /// 创建目录或临时文件失败时返回 [`Error::Io`]。
    pub fn create(target: &Path) -> Result<Self> {
        let dir = parent_of(target);
        if !dir.exists() {
            fs::create_dir_all(&dir).map_err(|e| io_err(&dir, "create_dir_all", &e))?;
        }

        let mut tmp = target.as_os_str().to_os_string();
        tmp.push(TMP_SUFFIX);
        let tmp = PathBuf::from(tmp);

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| io_err(&tmp, "create temp file", &e))?;

        Ok(Self { target: target.to_path_buf(), tmp, file: Option::from(file), written: 0 })
    }

    /// 追加写入一段数据。
    ///
    /// # Errors
    ///
    /// 写入失败或写入器已关闭时返回 [`Error::Io`]。
    pub fn write_all(&mut self, buf: &[u8]) -> Result<()> {
        let f = self.file.as_mut().ok_or_else(|| {
            io_msg("writer already committed or aborted".to_owned())
        })?;
        f.write_all(buf).map_err(|e| io_err(&self.tmp, "write", &e))?;
        self.written = self.written.saturating_add(buf.len() as u64);
        Ok(())
    }

    /// 已写入的字节数。
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// 目标路径。
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// 提交：fsync → fsync 父目录 → rename → fsync 父目录。
    ///
    /// # Errors
    ///
    /// 任一步失败时返回 [`Error::Io`]；此时会尽力删除临时文件，目标保持原样。
    pub fn commit(mut self) -> Result<()> {
        let Some(file) = self.file.take() else {
            return Err(io_msg("writer already committed or aborted".to_owned()));
        };

        // 步骤 2：文件内容落盘
        if let Err(e) = file.sync_all() {
            let err = io_err(&self.tmp, "fsync", &e);
            drop(file);
            let _ = fs::remove_file(&self.tmp);
            return Err(err);
        }
        drop(file); // Windows 上 rename 要求先关闭句柄

        let dir = parent_of(&self.target);

        // 步骤 3：目录项落盘（Unix）
        if let Err(e) = sync_dir(&dir) {
            let _ = fs::remove_file(&self.tmp);
            return Err(e);
        }

        // 步骤 4：原子替换。
        // fs::rename 在 Unix 上直接覆盖；在 Windows 上，std 已使用
        // MoveFileEx(MOVEFILE_REPLACE_EXISTING)，同样可覆盖已存在的目标。
        if let Err(e) = fs::rename(&self.tmp, &self.target) {
            let err = io_err(&self.tmp, "rename to target", &e);
            let _ = fs::remove_file(&self.tmp);
            return Err(err);
        }

        // 步骤 5：让 rename 本身持久化
        sync_dir(&dir)
    }

    /// 主动放弃写入并删除临时文件。
    pub fn abort(mut self) {
        self.cleanup();
    }

    /// 关闭句柄并删除临时文件。
    fn cleanup(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.tmp);
    }
}

impl Drop for AtomicWriter {
    /// 未提交即析构 → 丢弃临时文件，避免留下垃圾。
    fn drop(&mut self) {
        if self.file.is_some() {
            self.cleanup();
        }
    }
}

/// 受控的临时明文文件，用于「以外部应用打开」。
///
/// # 这个类型在防什么
///
/// 把解密后的明文落盘给外部程序打开，会踩到三个旁路泄露
/// （见 `docs/research/07-platform-and-sidechannels.md` §3）：
///
/// | 编号 | 泄露 | 本类型的对策 |
/// |---|---|---|
/// | L2 | 明文文件残留 | [`Drop`] 时删除；先覆写再删除（见下） |
/// | L3 | 系统缩略图缓存 | 目录写入 `desktop.ini`（Windows） |
/// | L5 | 搜索索引收录明文内容 | 目录标记不索引 |
///
/// # ⚠️ 覆写删除的真实效力
///
/// [`TempPlaintext::shred`] 会在删除前用随机字节覆写文件。**在现代存储上这不保证
/// 数据真正消失**：SSD 的 wear leveling 会把写入重定向到新的物理块，日志式文件系统
/// （APFS/Btrfs/ZFS）可能保留旧版本，快照更是直接留存。
///
/// 它能挡住的是「文件被删除后用常规恢复工具捞回来」，挡不住物理取证。
/// 本项目威胁模型已排除物理取证（N2/N3），因此这是成本合适的措施——
/// 但**不能向用户宣称"安全擦除"**。
#[derive(Debug)]
pub struct TempPlaintext {
    /// 明文文件路径。
    path: PathBuf,
    /// drop 时是否覆写。
    shred_on_drop: bool,
}

impl TempPlaintext {
    /// 在指定目录下创建临时明文文件。
    ///
    /// `dir` 应当是应用私有的受控目录，**不要**用系统 `%TEMP%`——那里可能被
    /// 云盘同步（L14）或备份系统（L13）覆盖。调用方负责选定安全目录。
    ///
    /// `filename` 用于保留正确的扩展名，让外部应用能正确关联打开方式。
    ///
    /// # Errors
    ///
    /// 创建目录或写入文件失败时返回 [`Error::Io`]。
    pub fn create(dir: &Path, filename: &str, plaintext: &[u8]) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| io_err(dir, "create_dir_all", &e))?;
        mark_dir_no_index(dir)?;

        let path = dir.join(filename);
        // 明文文件同样走原子写入：避免外部应用读到半截内容
        write_atomic(&path, plaintext)?;
        restrict_permissions(&path)?;

        Ok(Self { path, shred_on_drop: true })
    }

    /// 明文文件路径，可交给外部应用。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 关闭 drop 时的覆写（仅用于测试或用户显式选择更快的清理）。
    pub const fn set_shred_on_drop(&mut self, shred: bool) {
        self.shred_on_drop = shred;
    }

    /// 覆写并删除文件。
    ///
    /// 关于效力边界见类型文档。
    ///
    /// # Errors
    ///
    /// 覆写或删除失败时返回 [`Error::Io`]。
    pub fn shred(&self) -> Result<()> {
        shred_file(&self.path, self.shred_on_drop)
    }
}

impl Drop for TempPlaintext {
    fn drop(&mut self) {
        // Drop 中无法传播错误，只能尽力而为
        let _ = shred_file(&self.path, self.shred_on_drop);
    }
}

/// 覆写（可选）并删除文件。
fn shred_file(path: &Path, overwrite: bool) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if overwrite {
        if let Ok(meta) = fs::metadata(path) {
            let len = meta.len();
            // 写成嵌套 if 而不是 `len > 0 && let Ok(..)` 的 let-chain：
            // 后者到 Rust 1.88 才稳定，而本仓库 MSRV 是 1.85。
            if len > 0 {
                if let Ok(mut f) = OpenOptions::new().write(true).open(path) {
                    // 分块覆写，避免为大文件一次性分配缓冲区
                    const BUF: usize = 64 * 1024;
                    let mut buf = [0u8; BUF];
                    crate::util::fill_random(&mut buf);
                    let mut left = len;
                    while left > 0 {
                        let n = usize::try_from(left.min(BUF as u64)).unwrap_or(BUF);
                        if f.write_all(buf.get(..n).unwrap_or(&buf)).is_err() {
                            break;
                        }
                        left = left.saturating_sub(n as u64);
                    }
                    let _ = f.sync_all();
                }
            }
        }
    }
    fs::remove_file(path).map_err(|e| io_err(path, "remove temp plaintext", &e))
}

/// 标记目录不被系统搜索索引与缩略图缓存收录（旁路 L3/L5）。
///
/// # 平台实现
///
/// - Windows：写入 `desktop.ini`，设 `NoThumbnailCache`。
///   完整的 `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED` 需要调用 Win32 API，
///   留待 GUI 层用 `windows` crate 实现——core 层保持无平台依赖。
/// - macOS：创建 `.metadata_never_index`，Spotlight 会跳过整个目录。
/// - Linux：无统一机制；主流索引器（tracker/baloo）遵循 `.nomedia`。
///
/// # Errors
///
/// 标记文件写入失败时返回 [`Error::Io`]。目录不存在不算错误。
pub fn mark_dir_no_index(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let marker = dir.join(".metadata_never_index");
        if !marker.exists() {
            fs::write(&marker, b"").map_err(|e| io_err(&marker, "write marker", &e))?;
        }
    }

    #[cfg(windows)]
    {
        let ini = dir.join("desktop.ini");
        if !ini.exists() {
            // 关闭该目录的缩略图缓存
            let content = "[.ShellClassInfo]\r\nNoThumbnailCache=1\r\n";
            fs::write(&ini, content).map_err(|e| io_err(&ini, "write desktop.ini", &e))?;
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let marker = dir.join(".nomedia");
        if !marker.exists() {
            fs::write(&marker, b"").map_err(|e| io_err(&marker, "write marker", &e))?;
        }
    }

    Ok(())
}

/// 把文件权限收紧到仅所有者可读写。
///
/// Unix 下设为 `0o600`。Windows 的 ACL 模型不同，需要 Win32 API，
/// 留待平台层处理；此处为 no-op。
///
/// # Errors
///
/// 设置权限失败时返回 [`Error::Io`]。
pub fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let perm = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perm).map_err(|e| io_err(path, "set_permissions", &e))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// 清理指定目录下遗留的 `.tmp` 与 `.progress` 文件。
///
/// 应在应用启动时调用，清除上次异常退出的残留。
/// 返回被删除的文件数。
///
/// # Errors
///
/// 读取目录失败时返回 [`Error::Io`]。单个文件删除失败会被跳过，不中断整体清理。
pub fn cleanup_stale(dir: &Path) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n: usize = 0;
    let rd = fs::read_dir(dir).map_err(|e| io_err(dir, "read_dir", &e))?;
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if (name.ends_with(TMP_SUFFIX) || name.ends_with(PROGRESS_SUFFIX))
            && fs::remove_file(&path).is_ok()
        {
            n = n.saturating_add(1);
        }
    }
    Ok(n)
}
