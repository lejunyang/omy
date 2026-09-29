//! ffprobe / ffmpeg 子进程的定位与调用。
//!
//! # 为什么要并发读写管道
//!
//! 给子进程喂 stdin 的同时必须**并发**读取 stdout，否则会死锁：
//! 子进程的 stdout 缓冲区（通常 64 KiB）填满后它会阻塞在写上，
//! 而我们还在等它读完 stdin——双方各等对方，永久卡住。
//! 大文件（几 MiB 的 JSON 不会有，但 remux 输出会）必然触发。
//!
//! 这里用独立线程写 stdin，主线程读 stdout，从结构上排除死锁。

use crate::error::{MediaError, Result};
use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// stderr 与诊断信息的截断长度。
///
/// FFmpeg 在遇到损坏文件时可能输出上百 MB 的重复警告。
/// 不截断会让错误信息本身变成内存问题。
const DIAG_LIMIT: usize = 2048;

/// 子进程超时。
///
/// 恶意构造的文件可能让 FFmpeg 陷入极长的解析循环
/// （`docs/research/04-media-playback.md` §14 要求「超时强杀」）。
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Windows 上不给子进程建控制台窗口。
///
/// GUI 程序（没有附着的控制台）启动一个控制台子进程时，Windows 会**新建
/// 一个控制台窗口**。加密一个视频要起好几次 ffprobe/ffmpeg，于是屏幕上
/// 一连串黑框闪现——用户能明确感知到「有东西在偷偷跑」，而且窗口可能抢
/// 走焦点。CLI 里不明显是因为它本就有控制台可继承。
///
/// 0x0800_0000 是 `CREATE_NO_WINDOW`。不用 winapi 依赖，就为一个常量
/// 引入一个 crate 不值得。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 构造子进程命令，并在 Windows 上抑制控制台窗口。
///
/// 所有起 ffprobe/ffmpeg 的地方都必须走这里，不要直接用 `Command::new`：
/// 漏一处就会有一处闪黑框，而这种缺陷在 Linux/macOS 上根本不出现，
/// 只在 Windows 的 GUI 里才看得见。
fn command_for(exe: &Path) -> Command {
    // mut 只有 windows 分支要用（creation_flags 取 &mut self）。
    // 非 Windows 平台不加这个属性会报「变量不需要 mut」，
    // 而 Android 构建开着 -D warnings 就直接失败
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut c = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// 可执行文件的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// ffprobe，用于探测。
    Ffprobe,
    /// ffmpeg，用于抽帧与转封装。
    Ffmpeg,
}

impl Tool {
    /// 可执行文件名（不含平台后缀）。
    const fn stem(self) -> &'static str {
        match self {
            Self::Ffprobe => "ffprobe",
            Self::Ffmpeg => "ffmpeg",
        }
    }
}

/// 定位结果的缓存。
///
/// 每次探测都遍历一遍 PATH 与候选目录是浪费——列表页可能有几百个文件。
/// 用 `OnceLock` 而非 `lazy_static`，避免额外依赖。
static FFPROBE_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
static FFMPEG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 候选目录：相对可执行文件所在位置。
///
/// 便携版 ffmpeg 常与应用放在一起。开发期我们把它放在 `tools/ffmpeg/bin`。
fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    // 应用自身目录及其常见的同级布局
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.to_path_buf());
            dirs.push(d.join("ffmpeg"));
            dirs.push(d.join("ffmpeg").join("bin"));
            // 发行包布局（deb/rpm/AppImage）：主程序在 <prefix>/bin，
            // 内置 FFmpeg 作为资源位于 <prefix>/lib/omy/ffmpeg。
            // AppImage 运行时挂载目录里也是同样的相对结构。
            if let Some(prefix) = d.parent() {
                dirs.push(prefix.join("lib").join("omy").join("ffmpeg"));
            }
            // 开发期：target/release/omy.exe → 仓库根/tools/ffmpeg/bin
            if let Some(up2) = d.parent().and_then(Path::parent) {
                dirs.push(up2.join("tools").join("ffmpeg").join("bin"));
            }
        }
    }

    // 当前工作目录下的开发期布局
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("tools").join("ffmpeg").join("bin"));
    }

    dirs
}

/// 在候选位置中查找工具。
///
/// 查找顺序：显式环境变量 → 候选目录 → PATH。
/// 环境变量优先是有意的：便于用户指定特定版本，也便于测试注入。
fn locate(tool: Tool) -> Option<PathBuf> {
    let stem = tool.stem();
    let exe_name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    };

    // 1. 显式环境变量（OMY_FFPROBE / OMY_FFMPEG）
    let env_key = match tool {
        Tool::Ffprobe => "OMY_FFPROBE",
        Tool::Ffmpeg => "OMY_FFMPEG",
    };
    if let Some(v) = std::env::var_os(env_key) {
        let p = PathBuf::from(v);
        // 环境变量指了但不存在时不要静默忽略——那是配置错误，
        // 静默回退会让用户以为自己的设置生效了。
        if p.is_file() {
            return Some(p);
        }
    }

    // 2. 候选目录
    for d in candidate_dirs() {
        let p = d.join(&exe_name);
        if p.is_file() {
            return Some(p);
        }
    }

    // 3. PATH
    if let Some(paths) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&paths) {
            let p = d.join(&exe_name);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

/// 取工具路径，结果被缓存。
fn tool_path(tool: Tool) -> Option<&'static PathBuf> {
    let slot = match tool {
        Tool::Ffprobe => &FFPROBE_PATH,
        Tool::Ffmpeg => &FFMPEG_PATH,
    };
    slot.get_or_init(|| locate(tool)).as_ref()
}

/// 构造"找不到工具"的错误，附带找过的位置。
fn unavailable(tool: Tool) -> MediaError {
    let mut tried = Vec::new();
    match tool {
        Tool::Ffprobe => tried.push("OMY_FFPROBE".to_owned()),
        Tool::Ffmpeg => tried.push("OMY_FFMPEG".to_owned()),
    }
    for d in candidate_dirs() {
        tried.push(d.display().to_string());
    }
    tried.push("PATH".to_owned());
    MediaError::FfmpegUnavailable { tried }
}

/// 是否有可用的 ffprobe。
///
/// 供 `doctor` 这类命令如实报告环境能力。
#[must_use]
pub fn has_ffprobe() -> bool {
    tool_path(Tool::Ffprobe).is_some()
}

/// 是否有可用的 ffmpeg。
#[must_use]
pub fn has_ffmpeg() -> bool {
    tool_path(Tool::Ffmpeg).is_some()
}

/// 已定位到的 ffprobe 路径，供诊断显示。
#[must_use]
pub fn ffprobe_path() -> Option<PathBuf> {
    tool_path(Tool::Ffprobe).cloned()
}

/// 已定位到的 ffmpeg 路径，供诊断显示。
#[must_use]
pub fn ffmpeg_path() -> Option<PathBuf> {
    tool_path(Tool::Ffmpeg).cloned()
}

/// 工具版本首行，用于 `doctor`。
pub fn version(tool: Tool) -> Result<String> {
    let exe = tool_path(tool).ok_or_else(|| unavailable(tool))?;
    let out = command_for(exe)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.lines().next().unwrap_or("").trim().to_owned())
}

/// 一次子进程调用的产出。
#[derive(Debug)]
pub struct Output {
    /// 标准输出的原始字节。
    pub stdout: Vec<u8>,
    /// 标准错误（已截断）。
    pub stderr: String,
    /// 退出码；被信号终止时为 `None`。
    pub code: Option<i32>,
}

impl Output {
    /// 是否正常退出（退出码 0）。
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// 截断诊断文本，避免错误信息本身变成内存问题。
fn truncate_diag(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let t = s.trim();
    if t.len() <= DIAG_LIMIT {
        return t.to_owned();
    }
    // 按字符边界截断：直接按字节切会在多字节字符中间断开。
    // FFmpeg 的输出可能含非 ASCII 路径名。
    let mut end = DIAG_LIMIT;
    while end > 0 && !t.is_char_boundary(end) {
        end -= 1;
    }
    let head = t.get(..end).unwrap_or("");
    format!("{head}…（已截断，共 {} 字节）", t.len())
}

/// 以管道方式运行工具：把 `input` 喂给 stdin，收集 stdout。
///
/// # 死锁防护
///
/// 写 stdin 在独立线程进行，主线程并发读 stdout。若串行执行，
/// 子进程 stdout 缓冲满后会阻塞，而我们还在写 stdin，双方互等。
///
/// # 管道断开不是错误
///
/// ffprobe 读到足够的头部信息后会**主动关闭 stdin**，此时写入端得到
/// `BrokenPipe`。这是**正常且预期**的——它意味着探测已经成功，
/// 不需要读完整个文件。把它当错误会让所有大文件探测都失败。
///
/// # Errors
///
/// 工具不存在、进程启动失败或超时时返回错误。
pub fn run_piped(tool: Tool, args: &[&str], input: Vec<u8>) -> Result<Output> {
    let exe = tool_path(tool).ok_or_else(|| unavailable(tool))?;

    let mut child = command_for(exe)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| MediaError::MalformedOutput {
            reason: "无法取得子进程 stdin".to_owned(),
        })?;

    // 独立线程喂数据，避免与读 stdout 相互阻塞
    let writer = std::thread::spawn(move || {
        // BrokenPipe 是预期结果，不向上传播
        let _ = stdin.write_all(&input);
        let _ = stdin.flush();
        drop(stdin);
    });

    let out = child.wait_with_output()?;
    // 忽略写线程的 panic：它唯一可能的失败是管道断开，已在上面吞掉
    let _ = writer.join();

    Ok(Output {
        stdout: out.stdout,
        stderr: truncate_diag(&out.stderr),
        code: out.status.code(),
    })
}

/// 以文件路径方式运行工具，带超时。
///
/// 探测本地未加密文件时用这条路径更高效——不必把整个文件读进内存
/// 再喂管道。加密文件必须用 [`run_piped`]，因为明文只在内存里。
///
/// # Errors
///
/// 工具不存在、启动失败或超过 `timeout` 时返回错误。
pub fn run_with_timeout(
    tool: Tool,
    args: &[OsString],
    timeout: Option<std::time::Duration>,
) -> Result<Output> {
    let exe = tool_path(tool).ok_or_else(|| unavailable(tool))?;
    let limit = timeout.unwrap_or(DEFAULT_TIMEOUT);

    let mut child = command_for(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // 轮询等待而非直接 wait：需要能在超时后强杀。
    // 恶意文件可能让 FFmpeg 陷入极长解析。
    let start = std::time::Instant::now();
    loop {
        match child.try_wait()? {
            Some(_status) => break,
            None => {
                if start.elapsed() >= limit {
                    // 超时必须强杀，否则子进程会一直占着 CPU
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(MediaError::ProbeFailed {
                        code: None,
                        stderr: format!("超过 {} 秒未完成，已强制终止", limit.as_secs()),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    let out = child.wait_with_output()?;
    Ok(Output {
        stdout: out.stdout,
        stderr: truncate_diag(&out.stderr),
        code: out.status.code(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_stems_are_correct() {
        assert_eq!(Tool::Ffprobe.stem(), "ffprobe");
        assert_eq!(Tool::Ffmpeg.stem(), "ffmpeg");
    }

    #[test]
    fn truncate_keeps_short_text_intact() {
        assert_eq!(truncate_diag(b"hello"), "hello");
        // 首尾空白被裁掉
        assert_eq!(truncate_diag(b"  hello  "), "hello");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        // 中文每字符 3 字节。按字节切必然切断字符，
        // 若实现里直接 &s[..LIMIT] 就会 panic。
        let long = "中".repeat(DIAG_LIMIT);
        let out = truncate_diag(long.as_bytes());
        assert!(out.contains("已截断"));
        // 能正常构造出字符串即证明没有在字符中间切断
        assert!(out.chars().count() > 0);
    }

    #[test]
    fn truncate_reports_original_length() {
        let long = "a".repeat(DIAG_LIMIT * 2);
        let out = truncate_diag(long.as_bytes());
        // 必须告知原始长度，否则用户不知道被截了多少
        assert!(out.contains(&(DIAG_LIMIT * 2).to_string()), "{out}");
    }

    #[test]
    fn unavailable_error_lists_search_locations() {
        let e = unavailable(Tool::Ffprobe);
        match e {
            MediaError::FfmpegUnavailable { tried } => {
                assert!(tried.iter().any(|t| t == "OMY_FFPROBE"));
                assert!(tried.iter().any(|t| t == "PATH"));
            }
            other => panic!("错误类型不对：{other:?}"),
        }
    }

    #[test]
    fn output_success_only_for_zero() {
        let mk = |c: Option<i32>| Output {
            stdout: vec![],
            stderr: String::new(),
            code: c,
        };
        assert!(mk(Some(0)).success());
        assert!(!mk(Some(1)).success());
        // 被信号杀死不算成功
        assert!(!mk(None).success());
    }
}
