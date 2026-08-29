//! 错误类型。

use std::fmt;

/// 本 crate 的结果别名。
pub type Result<T> = std::result::Result<T, MediaError>;

/// 媒体处理错误。
///
/// 变体的划分依据是**调用方需要做出的不同反应**，而不是错误发生的位置：
/// 「FFmpeg 没装」要降级、「文件不是媒体」要按普通文件处理、
/// 「FFmpeg 崩了」要警惕恶意文件——三者反应完全不同，不能混为一谈。
#[derive(Debug)]
pub enum MediaError {
    /// 找不到可用的 FFmpeg/ffprobe。
    ///
    /// **这不是致命错误**。调用方应降级为按扩展名保守分级，
    /// 并在 UI 上提示"安装 FFmpeg 可获得更准确的格式识别"。
    FfmpegUnavailable {
        /// 已尝试过的查找位置，用于生成有用的提示而非空泛的"找不到"。
        tried: Vec<String>,
    },

    /// ffprobe 判定输入不是可识别的媒体文件。
    ///
    /// 对文本、压缩包等非媒体文件而言这是**预期结果**，
    /// 调用方应按普通文件处理，不要向用户报错。
    NotMedia {
        /// ffprobe 的诊断输出（已截断），便于排查误判。
        detail: String,
    },

    /// ffprobe 进程异常退出或超时。
    ///
    /// 与 [`Self::NotMedia`] 的区别很重要：这里意味着**进程本身出了问题**
    /// （崩溃、被杀、超时），可能是恶意构造的文件在触发解析漏洞，
    /// 应当记录并警惕，而不是简单当作"不是媒体"。
    ProbeFailed {
        /// 退出码，被信号杀死时为 `None`。
        code: Option<i32>,
        /// stderr 的内容（已截断）。
        stderr: String,
    },

    /// ffprobe 的输出无法解析为预期结构。
    ///
    /// 通常意味着 FFmpeg 版本的输出格式与预期不符——
    /// 这类问题必须显式报出来，否则会静默产生错误的分级结果。
    MalformedOutput {
        /// 具体哪里不符合预期。
        reason: String,
    },

    /// 子进程 I/O 失败。
    Io(std::io::Error),

    /// 图像处理失败（缩略图生成）。
    Image(String),

    /// 输入数据本身不合法（如 MP4 box 结构损坏）。
    InvalidInput {
        /// 具体原因。
        reason: String,
    },
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FfmpegUnavailable { tried } => {
                write!(f, "找不到可用的 ffprobe")?;
                if !tried.is_empty() {
                    write!(f, "（已尝试：{}）", tried.join("、"))?;
                }
                Ok(())
            }
            Self::NotMedia { detail } => {
                if detail.is_empty() {
                    write!(f, "不是可识别的媒体文件")
                } else {
                    write!(f, "不是可识别的媒体文件：{detail}")
                }
            }
            Self::ProbeFailed { code, stderr } => {
                match code {
                    Some(c) => write!(f, "ffprobe 异常退出（退出码 {c}）")?,
                    None => write!(f, "ffprobe 被信号终止")?,
                }
                if !stderr.is_empty() {
                    write!(f, "：{stderr}")?;
                }
                Ok(())
            }
            Self::MalformedOutput { reason } => {
                write!(f, "ffprobe 输出不符合预期：{reason}")
            }
            Self::Io(e) => write!(f, "子进程 I/O 失败：{e}"),
            Self::Image(m) => write!(f, "图像处理失败：{m}"),
            Self::InvalidInput { reason } => write!(f, "输入数据不合法：{reason}"),
        }
    }
}

impl std::error::Error for MediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl MediaError {
    /// 是否属于"能力缺失"而非"真的出错"。
    ///
    /// 调用方用它区分「该降级」与「该报错」：前者要静默降级并继续，
    /// 后者要让用户看到。把这个判断收在这里，避免各调用点各写一套
    /// `matches!` 而逐渐不一致。
    #[must_use]
    pub const fn is_degradable(&self) -> bool {
        matches!(self, Self::FfmpegUnavailable { .. } | Self::NotMedia { .. })
    }

    /// 稳定的错误码字符串，供 CLI `--json` 与日志使用。
    ///
    /// 恒为英文常量，不随界面语言变化——否则脚本无法据此判断。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::FfmpegUnavailable { .. } => "FFMPEG_UNAVAILABLE",
            Self::NotMedia { .. } => "NOT_MEDIA",
            Self::ProbeFailed { .. } => "PROBE_FAILED",
            Self::MalformedOutput { .. } => "MALFORMED_OUTPUT",
            Self::Io(_) => "IO_ERROR",
            Self::Image(_) => "IMAGE_ERROR",
            Self::InvalidInput { .. } => "INVALID_INPUT",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degradable_only_for_capability_gaps() {
        assert!(MediaError::FfmpegUnavailable { tried: vec![] }.is_degradable());
        assert!(
            MediaError::NotMedia {
                detail: String::new()
            }
            .is_degradable()
        );
        // 进程崩溃可能是恶意文件触发漏洞，绝不能当作"降级即可"
        assert!(
            !MediaError::ProbeFailed {
                code: Some(139),
                stderr: String::new()
            }
            .is_degradable()
        );
        assert!(
            !MediaError::MalformedOutput {
                reason: String::new()
            }
            .is_degradable()
        );
    }

    #[test]
    fn codes_are_stable_and_distinct() {
        let all = [
            MediaError::FfmpegUnavailable { tried: vec![] }.code(),
            MediaError::NotMedia {
                detail: String::new(),
            }
            .code(),
            MediaError::ProbeFailed {
                code: None,
                stderr: String::new(),
            }
            .code(),
            MediaError::MalformedOutput {
                reason: String::new(),
            }
            .code(),
            MediaError::Image(String::new()).code(),
            MediaError::InvalidInput {
                reason: String::new(),
            }
            .code(),
        ];
        // 每个码必须唯一，否则调用方无法据此分流
        let uniq: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(uniq.len(), all.len());
        // 必须是英文大写常量形式
        for c in all {
            assert!(
                c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'),
                "错误码 {c} 不是英文大写常量"
            );
        }
    }

    #[test]
    fn unavailable_message_lists_tried_paths() {
        let e = MediaError::FfmpegUnavailable {
            tried: vec!["PATH".into(), "tools/ffmpeg".into()],
        };
        let msg = e.to_string();
        // 空泛的"找不到"对用户没用，必须指出找过哪里
        assert!(msg.contains("PATH"), "缺少已尝试位置：{msg}");
        assert!(msg.contains("tools/ffmpeg"), "缺少已尝试位置：{msg}");
    }
}
