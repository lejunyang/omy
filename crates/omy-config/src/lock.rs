//! 跨进程配置互斥锁。
//!
//! # 为什么需要它
//!
//! 配置文件的写路径是「读最新磁盘 → 改内存 → 原子写回」。GUI 常驻、CLI 短命，
//! 两个进程完全可能同时走到这条路径。各自读改写本身没错，错在**没有互斥**：
//! 两边都读到 [p1]，CLI 加了 p2 写回，GUI 又把自己手里那份 [p1] 写回——
//! p2 就被静默冲掉了。这就是数组（`remote.places`）的丢失更新。
//!
//! `mutate_places` 早先堵过一次「启动快照整体覆盖」：每次写前都重读磁盘。
//! 但「重读 → 写回」之间仍有一条 TOCTOU 窗口，两个进程会卡进同一次读里。
//! 本模块把整条「读-改-写」放进同一把跨进程锁里，窗口才真正消失。
//!
//! # 为什么用 OS 建议锁（flock / LockFileEx）而不是 O_EXCL 锁文件
//!
//! 跨进程锁的常见做法是 `create_new` 抢一个 `.lock` 文件，谁抢到谁写。它有两个
//! 真实的坑，这里都刻意避开：
//!
//! 1. **崩溃残留**：持锁进程被杀后，`.lock` 文件还躺在磁盘上，后续所有写者
//!    都会以为锁被占着。只能再靠「写 PID 进去、发现 PID 不在就偷锁」补救——
//!    而 PID 会复用，偷错的概率虽小后果却是互斥失效。
//! 2. 要在 Windows 上判断 PID 存活同样得 FFI，省不掉系统调用。
//!
//! OS 建议锁把锁挂在**打开的句柄**上：进程一旦退出（正常退出、崩溃、断电），
//! 内核会关闭它的一切句柄，锁随之自动释放。没有残留锁文件要清理，也没有 PID
//! 猜测。代价只是写者始终持有一个句柄——而配置写本来就是亚毫秒级的事。
//!
//! # 崩溃恢复
//!
//! 见上：锁随句柄释放，进程死了锁就没了。这是内核保证的行为，不需要本层做
//! 任何「检测残留锁」的动作。对应测试里直接 `taskkill` / 杀掉子进程后断言
//! 能立刻重新拿到锁。
//!
//! # 只读不被阻塞
//!
//! [`crate::Config::load`] 等读路径**完全不碰这把锁**：它们只读文件，而写路径
//! 走 `omy_core::fsatomic` 的 tmp + rename，读者要么看到旧文件要么看到新文件，
//! 永远不会读到半截。所以一把写锁不会让任何读永久等待。
//!
//! # 超时与错误语义
//!
//! 正常持锁是亚毫秒级（解析一个几 KB 的 TOML、改一个字段、写回）。拿不到锁
//! 超过 [`DEFAULT_ACQUIRE_TIMEOUT`] 就返回超时错误，而不是无限等待——
//! 死等会让「持锁者卡死」变成「用户整个应用卡死」，比报错更糟。

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 默认获取锁的超时。
///
/// 不这样会怎样：持锁者因为闭包写慢了（理论上不该发生——所有闭包都只改内存）
/// 卡住时，另一个写者无限等待，表现为「点了保存没反应」，用户无从判断。
/// 5 秒远超正常持锁时间，到点就报错，把问题暴露出来而不是冻住界面。
pub const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// 计算锁文件路径：配置文件旁的同名 `.lock`。
///
/// 与配置同目录，便携模式下跟着 `omy-data/` 一起走，不会散落到系统目录。
pub(crate) fn lock_path(config: &Path) -> PathBuf {
    let mut s = config.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// 获取锁失败。
#[derive(Debug)]
pub(crate) enum AcquireError {
    /// 打开/创建锁文件失败（目录不可写等）。
    Io {
        /// 锁文件路径。
        path: PathBuf,
        /// 底层错误。
        source: io::Error,
    },
    /// 等待超过超时仍没拿到锁。
    Timeout {
        /// 锁文件路径。
        path: PathBuf,
        /// 等了多久。
        waited: Duration,
    },
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "锁文件 {} 打开失败: {source}", path.display()),
            Self::Timeout { path, waited } => write!(
                f,
                "等待配置锁 {} 超时（{waited:?}）：另一进程持锁过久",
                path.display()
            ),
        }
    }
}

impl std::error::Error for AcquireError {}

/// 已获取的跨进程锁句柄。
///
/// Drop 时解锁；进程崩溃时内核自动释放（见模块文档）。
pub(crate) struct FileLock {
    #[allow(dead_code)] // 仅为持有句柄而存在：句柄一关，锁即释放
    file: std::fs::File,
    #[allow(dead_code)] // 仅用于错误信息与测试
    path: PathBuf,
}

impl FileLock {
    /// 阻塞式获取，直到拿到或超时。
    ///
    /// 轮询而非系统调用里阻塞：我们要在等待期间能按 deadline 退出，
    /// 且不能因为平台原生「锁等待」被信号打断后丢状态。20ms 一次的重试
    /// 对亚毫秒级的写操作足够快，CPU 占用可忽略。
    pub(crate) fn acquire(config_path: &Path, timeout: Duration) -> Result<Self, AcquireError> {
        let lock_path = lock_path(config_path);
        if let Some(dir) = lock_path.parent() {
            // 建目录失败不算致命：真正 open 时会给出更准确的错误
            let _ = std::fs::create_dir_all(dir);
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // 只对这个文件的字节范围加锁，从不往里写；不截断——truncate(false)
            // 表明「我要的是打开已存在的内容」，避免每次获锁把文件截成 0。
            .truncate(false)
            .open(&lock_path)
            .map_err(|source| AcquireError::Io { path: lock_path.clone(), source })?;

        let deadline = Instant::now() + timeout;
        loop {
            match try_lock_exclusive(&file) {
                Ok(()) => return Ok(Self { file, path: lock_path }),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(AcquireError::Timeout { path: lock_path, waited: timeout });
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(TryLockError::Fatal(source)) => {
                    return Err(AcquireError::Io { path: lock_path, source });
                }
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        unlock_exclusive(&self.file);
    }
}

/// 尝试独占锁的结果：拿到、被占、或真出错。
enum TryLockError {
    /// 锁被别的进程占着，稍后再试。
    WouldBlock,
    /// 真正的系统错误。
    Fatal(io::Error),
}

// ---- Windows：LockFileEx ----

#[cfg(windows)]
fn try_lock_exclusive(file: &std::fs::File) -> Result<(), TryLockError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    // 锁整个文件的第 0 字节。不做异步 I/O，OVERLAPPED 清零即可。
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        Err(TryLockError::WouldBlock)
    } else {
        Err(TryLockError::Fatal(err))
    }
}

#[cfg(windows)]
fn unlock_exclusive(file: &std::fs::File) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    unsafe {
        UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped);
    }
}

// ---- Unix / Android：flock ----

#[cfg(unix)]
fn try_lock_exclusive(file: &std::fs::File) -> Result<(), TryLockError> {
    use std::os::unix::io::AsRawFd;
    let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // EWOULDBLOCK 在多数平台等于 EAGAIN；两者都按「被占着」处理。
    // 不能写成 `Some(EWOULDBLOCK) | Some(EAGAIN)` 的 or-pattern：macOS 上
    // 这两个常量就是同一个值，clippy 会把后半判为不可达分支而 -D warnings 挂门禁。
    match err.raw_os_error() {
        Some(e) if e == libc::EWOULDBLOCK || e == libc::EAGAIN => {
            Err(TryLockError::WouldBlock)
        }
        _ => Err(TryLockError::Fatal(err)),
    }
}

#[cfg(unix)]
fn unlock_exclusive(file: &std::fs::File) {
    use std::os::unix::io::AsRawFd;
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
}

// 非 Windows 非 Unix（理论上本 crate 只跑这两类）：提供一个编译可过的空实现。
// 真跑到这里等于平台未支持，调用方应视为错误而非静默降级。
#[cfg(not(any(windows, unix)))]
fn try_lock_exclusive(_file: &std::fs::File) -> Result<(), TryLockError> {
    Err(TryLockError::Fatal(io::Error::other("omy-config 跨进程锁未支持本平台")))
}

#[cfg(not(any(windows, unix)))]
fn unlock_exclusive(_file: &std::fs::File) {}
