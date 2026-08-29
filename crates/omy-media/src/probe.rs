//! 媒体探测：把 ffprobe 的 JSON 输出转成结构化信息。
//!
//! # 反序列化的现实约束
//!
//! ffprobe 的 JSON **类型不统一**（已实测确认，见
//! `spikes/probe-ffprobe-contract.ps1`）：`duration`、`size`、`bit_rate`
//! 是**字符串**，而 `channels`、`width`、`index` 是**数字**。同一字段在
//! 不同版本、不同容器下还可能缺失或为 `"N/A"`。
//!
//! 因此所有数值字段都经 [`LooseNumber`] 解析，容忍字符串/数字/缺失/`N/A`
//! 四种形态。直接用 `f64` 反序列化会在真实文件上失败。

use crate::error::{MediaError, Result};
use crate::ffprobe::{self, Tool};
use serde::Deserialize;

/// 探测输入的来源。
pub enum Source<'a> {
    /// 内存中的明文字节（加密文件解密后）。
    ///
    /// 只需要前若干字节即可探测，不必传整个文件——
    /// 调用方应只传头部，见 [`PROBE_HEAD_BYTES`]。
    Bytes(&'a [u8]),
    /// 本地未加密文件路径。
    Path(&'a std::path::Path),
}

/// 探测所需的头部字节数。
///
/// moov 在尾部的 MP4 用管道探测会失败（无法 seek），但对**分级**而言
/// 头部信息已足够：容器类型与编码 ID 都在前部。若需要精确时长，
/// 调用方应改用 [`Source::Path`] 或提供完整字节。
///
/// 取 8 MiB 是权衡：mkv 的 SeekHead 与 Tracks 通常在前几百 KB，
/// 但某些封装工具会把 Tracks 放得很后。
pub const PROBE_HEAD_BYTES: usize = 8 * 1024 * 1024;

/// 宽松数值：容忍 ffprobe 把数字写成字符串。
#[derive(Debug, Clone, Copy, Default)]
struct LooseNumber(Option<f64>);

impl<'de> Deserialize<'de> for LooseNumber {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct V;
        impl<'d> Visitor<'d> for V {
            type Value = LooseNumber;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("数字、数字字符串或 null")
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                Ok(LooseNumber(Some(v)))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(LooseNumber(Some(v as f64)))
            }
            #[allow(clippy::cast_precision_loss)]
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(LooseNumber(Some(v as f64)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                // "N/A" 是 ffprobe 表示"不适用"的约定，不是错误
                let t = v.trim();
                if t.is_empty() || t.eq_ignore_ascii_case("n/a") {
                    return Ok(LooseNumber(None));
                }
                Ok(LooseNumber(t.parse::<f64>().ok()))
            }
            fn visit_none<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseNumber(None))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(LooseNumber(None))
            }
            fn visit_some<D: serde::Deserializer<'d>>(
                self,
                d: D,
            ) -> std::result::Result<Self::Value, D::Error> {
                d.deserialize_any(self)
            }
        }
        d.deserialize_option(V)
    }
}

impl LooseNumber {
    const fn f64(self) -> Option<f64> {
        self.0
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn u64(self) -> Option<u64> {
        self.0.and_then(|v| {
            if v.is_finite() && v >= 0.0 {
                Some(v as u64)
            } else {
                None
            }
        })
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn u32(self) -> Option<u32> {
        self.u64().and_then(|v| u32::try_from(v).ok())
    }
}

// ---------- ffprobe 的原始 JSON 结构 ----------

#[derive(Debug, Deserialize)]
struct RawProbe {
    #[serde(default)]
    streams: Vec<RawStream>,
    #[serde(default)]
    format: Option<RawFormat>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    #[serde(default)]
    format_name: Option<String>,
    #[serde(default)]
    format_long_name: Option<String>,
    #[serde(default)]
    duration: LooseNumber,
    #[serde(default)]
    size: LooseNumber,
    #[serde(default)]
    bit_rate: LooseNumber,
    #[serde(default)]
    tags: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    #[serde(default)]
    index: LooseNumber,
    #[serde(default)]
    codec_name: Option<String>,
    #[serde(default)]
    codec_type: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    /// 实测发现 ffprobe 9.x 直接给出 `avc1.64001e` 这类串，
    /// 正是 `MediaSource.isTypeSupported()` 需要的形式，
    /// 不必自己从 profile/level 拼装（拼错的风险很高）。
    #[serde(default)]
    mime_codec_string: Option<String>,
    #[serde(default)]
    width: LooseNumber,
    #[serde(default)]
    height: LooseNumber,
    #[serde(default)]
    level: LooseNumber,
    #[serde(default)]
    pix_fmt: Option<String>,
    #[serde(default)]
    channels: LooseNumber,
    #[serde(default)]
    channel_layout: Option<String>,
    #[serde(default)]
    sample_rate: LooseNumber,
    #[serde(default)]
    duration: LooseNumber,
    #[serde(default)]
    bit_rate: LooseNumber,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    nb_frames: LooseNumber,
    #[serde(default)]
    disposition: Option<std::collections::BTreeMap<String, i64>>,
    #[serde(default)]
    tags: Option<std::collections::BTreeMap<String, String>>,
}

// ---------- 对外的结构化结果 ----------

/// 视频轨信息。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VideoStream {
    /// 流序号。
    pub index: u32,
    /// 编码名，如 `h264`。
    pub codec: String,
    /// RFC 6381 codec 串，如 `avc1.64001e`。供 `isTypeSupported` 使用。
    pub mime_codec: Option<String>,
    /// 宽（像素）。
    pub width: Option<u32>,
    /// 高（像素）。
    pub height: Option<u32>,
    /// profile，如 `High`。
    pub profile: Option<String>,
    /// level，如 30 表示 3.0。
    pub level: Option<u32>,
    /// 像素格式，如 `yuv420p`。判断 10bit / HDR 用。
    pub pix_fmt: Option<String>,
    /// 平均帧率的字面值，如 `30/1`。
    pub frame_rate: Option<String>,
    /// 帧数。
    pub frames: Option<u64>,
    /// 是否为封面图轨（`attached_pic`）。
    ///
    /// **必须区分**：MP3 内嵌封面会被识别成一条 video 流，
    /// 若不排除，音频文件会被误判为视频。
    pub attached_pic: bool,
}

/// 音频轨信息。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AudioStream {
    /// 流序号。
    pub index: u32,
    /// 编码名，如 `aac`、`dts`。
    pub codec: String,
    /// RFC 6381 codec 串，如 `mp4a.40.2`。
    pub mime_codec: Option<String>,
    /// 声道数。
    pub channels: Option<u32>,
    /// 声道布局，如 `stereo`、`5.1`。
    pub channel_layout: Option<String>,
    /// 采样率。
    pub sample_rate: Option<u32>,
    /// 语言标签（ISO 639）。
    pub language: Option<String>,
    /// 轨道标题。
    pub title: Option<String>,
    /// 是否为默认轨。
    pub default: bool,
}

/// 字幕轨信息。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SubtitleStream {
    /// 流序号。
    pub index: u32,
    /// 编码名，如 `subrip`、`ass`、`hdmv_pgs_subtitle`。
    pub codec: String,
    /// 语言标签。
    pub language: Option<String>,
    /// 轨道标题。
    pub title: Option<String>,
    /// 是否为默认轨。
    pub default: bool,
    /// 是否强制显示。
    pub forced: bool,
}

/// 完整的媒体信息。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MediaInfo {
    /// 容器名。ffprobe 给的是逗号分隔的多值，这里保留原样。
    pub container: String,
    /// 容器的可读名。
    pub container_long: Option<String>,
    /// 时长（毫秒）。
    pub duration_ms: Option<u64>,
    /// 文件大小（字节）。管道探测时通常不可得。
    pub size: Option<u64>,
    /// 总码率。
    pub bit_rate: Option<u64>,
    /// 视频轨（已排除封面图轨）。
    pub video: Vec<VideoStream>,
    /// 音频轨。
    pub audio: Vec<AudioStream>,
    /// 字幕轨。
    pub subtitles: Vec<SubtitleStream>,
    /// 容器级标签。
    pub tags: std::collections::BTreeMap<String, String>,
}

impl MediaInfo {
    /// 容器是否匹配给定名称。
    ///
    /// ffprobe 的 `format_name` 是**逗号分隔的多值**
    /// （如 `mov,mp4,m4a,3gp,3g2,mj2`），直接用 `==` 比较必然失配。
    /// 这个坑很容易踩，所以把判断收在这里。
    #[must_use]
    pub fn container_is(&self, name: &str) -> bool {
        self.container
            .split(',')
            .any(|c| c.trim().eq_ignore_ascii_case(name))
    }

    /// 主视频轨（第一条非封面图的视频轨）。
    #[must_use]
    pub fn primary_video(&self) -> Option<&VideoStream> {
        self.video.first()
    }

    /// 是否含有实际的视频画面。
    ///
    /// 封面图轨已在解析阶段排除，因此这里为真即代表真的是视频。
    #[must_use]
    pub fn has_video(&self) -> bool {
        !self.video.is_empty()
    }

    /// 是否为纯音频。
    #[must_use]
    pub fn is_audio_only(&self) -> bool {
        self.video.is_empty() && !self.audio.is_empty()
    }
}

/// 从 ffprobe 的 JSON 文本解析。
///
/// 独立成公开函数是为了让测试能用**固定的 JSON 样本**验证解析逻辑，
/// 不依赖本机是否装了 FFmpeg——否则解析器的正确性无法在 CI 上验证。
///
/// # Errors
///
/// JSON 无法解析，或不含任何流与格式信息时返回错误。
pub fn parse_probe_json(json: &str) -> Result<MediaInfo> {
    let raw: RawProbe = serde_json::from_str(json).map_err(|e| MediaError::MalformedOutput {
        reason: format!("JSON 解析失败：{e}"),
    })?;

    // 既无流也无格式，说明根本不是媒体文件
    if raw.streams.is_empty() && raw.format.is_none() {
        return Err(MediaError::NotMedia {
            detail: "ffprobe 未返回任何流或格式信息".to_owned(),
        });
    }

    let fmt = raw.format.unwrap_or(RawFormat {
        format_name: None,
        format_long_name: None,
        duration: LooseNumber::default(),
        size: LooseNumber::default(),
        bit_rate: LooseNumber::default(),
        tags: None,
    });

    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut subtitles = Vec::new();
    // 收集流级时长与码率，供容器级缺失时回退
    let mut raw_stream_durations: Vec<f64> = Vec::new();
    let mut raw_stream_bitrates: Vec<u64> = Vec::new();

    for s in raw.streams {
        let idx = s.index.u32().unwrap_or(0);
        let codec = s.codec_name.clone().unwrap_or_else(|| "unknown".to_owned());
        let disp = s.disposition.unwrap_or_default();
        let flag = |k: &str| disp.get(k).copied().unwrap_or(0) != 0;
        let tags = s.tags.unwrap_or_default();
        let tag = |k: &str| {
            tags.iter()
                .find(|(tk, _)| tk.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.clone())
        };
        if let Some(d) = s.duration.f64() {
            raw_stream_durations.push(d);
        }
        if let Some(b) = s.bit_rate.u64() {
            raw_stream_bitrates.push(b);
        }

        match s.codec_type.as_deref() {
            Some("video") => {
                let attached = flag("attached_pic") || flag("still_image");
                let vs = VideoStream {
                    index: idx,
                    codec,
                    mime_codec: s.mime_codec_string,
                    width: s.width.u32(),
                    height: s.height.u32(),
                    profile: s.profile,
                    level: s.level.u32(),
                    pix_fmt: s.pix_fmt,
                    frame_rate: s.avg_frame_rate.filter(|r| r != "0/0"),
                    frames: s.nb_frames.u64(),
                    attached_pic: attached,
                };
                // 封面图轨不计入视频轨：否则带封面的 MP3 会被当成视频，
                // UI 会显示播放器而不是音频界面。
                if !attached {
                    video.push(vs);
                }
            }
            Some("audio") => audio.push(AudioStream {
                index: idx,
                codec,
                mime_codec: s.mime_codec_string,
                channels: s.channels.u32(),
                channel_layout: s.channel_layout,
                sample_rate: s.sample_rate.u32(),
                language: tag("language").filter(|l| l != "und"),
                title: tag("title"),
                default: flag("default"),
            }),
            Some("subtitle") => subtitles.push(SubtitleStream {
                index: idx,
                codec,
                language: tag("language").filter(|l| l != "und"),
                title: tag("title"),
                default: flag("default"),
                forced: flag("forced"),
            }),
            // data / attachment 轨（如 MKV 内嵌字体）当前不处理，
            // 但将来 ASS 渲染需要提取字体附件，届时在此扩展。
            _ => {}
        }
    }

    // 时长优先取容器级；缺失时回退到最长的流。
    // 管道探测 mkv 时容器时长常常缺失，但流级时长可能有值——
    // 若不回退，UI 上会显示"未知时长"，进度条也无法工作。
    let stream_max_duration = raw_stream_durations
        .into_iter()
        .filter(|d| d.is_finite() && *d >= 0.0)
        .fold(None::<f64>, |acc, d| Some(acc.map_or(d, |a| a.max(d))));

    let duration_ms = fmt
        .duration
        .f64()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .or(stream_max_duration)
        .and_then(|sec| {
            if sec.is_finite() && sec >= 0.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some((sec * 1000.0).round() as u64)
            } else {
                None
            }
        });

    // 总码率缺失时用各流码率之和估算，同样是为了 UI 不显示空白
    let bit_rate = fmt.bit_rate.u64().or_else(|| {
        let sum: u64 = raw_stream_bitrates.iter().copied().sum();
        if sum > 0 { Some(sum) } else { None }
    });

    Ok(MediaInfo {
        container: fmt.format_name.unwrap_or_else(|| "unknown".to_owned()),
        container_long: fmt.format_long_name,
        duration_ms,
        size: fmt.size.u64(),
        bit_rate,
        video,
        audio,
        subtitles,
        tags: fmt.tags.unwrap_or_default(),
    })
}

/// ffprobe 的标准参数。
const PROBE_ARGS: &[&str] = &[
    "-v",
    "quiet",
    "-print_format",
    "json",
    "-show_format",
    "-show_streams",
];

/// 探测媒体信息。
///
/// # Errors
///
/// - [`MediaError::FfmpegUnavailable`]：没装 ffprobe，调用方应降级；
/// - [`MediaError::NotMedia`]：不是媒体文件，属预期结果；
/// - [`MediaError::ProbeFailed`]：进程崩溃或超时，需警惕恶意文件。
pub fn probe(source: Source<'_>) -> Result<MediaInfo> {
    let out = match source {
        Source::Bytes(b) => {
            // MP4 且 moov 在尾部时，只喂头部必然探测失败——元数据不在头部。
            // 先在主进程内用纯 Rust 把 moov 前移（见 mp4::to_faststart），
            // 这样头部就含有全部元数据了。
            //
            // 这不是可选优化：录屏、相机直出等大量真实文件都是尾部 moov，
            // 不处理的话它们的探测会全部失败。
            let reordered = if crate::mp4::looks_like_mp4(b) {
                crate::mp4::to_faststart(b).ok().flatten()
            } else {
                None
            };
            let src: &[u8] = reordered.as_deref().unwrap_or(b);

            // 只喂头部：整个文件可能几 GB，全喂进管道是浪费，
            // 而探测只需要头部信息。
            let head = src.get(..src.len().min(PROBE_HEAD_BYTES)).unwrap_or(src);
            let mut args: Vec<&str> = PROBE_ARGS.to_vec();
            args.push("-");
            ffprobe::run_piped(Tool::Ffprobe, &args, head.to_vec())?
        }
        Source::Path(p) => {
            let mut args: Vec<std::ffi::OsString> =
                PROBE_ARGS.iter().map(std::ffi::OsString::from).collect();
            args.push(p.into());
            ffprobe::run_with_timeout(Tool::Ffprobe, &args, None)?
        }
    };

    // 退出码非 0 且无输出：可能是崩溃，也可能只是不是媒体文件。
    // 用 stdout 是否为空来区分——ffprobe 对非媒体文件会正常退出但输出空结构。
    if !out.success() && out.stdout.is_empty() {
        // 137/139 等表示被信号杀死或段错误，必须当成 ProbeFailed
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: out.stderr,
        });
    }

    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        return Err(MediaError::NotMedia {
            detail: out.stderr,
        });
    }
    parse_probe_json(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实的 ffprobe 9.0.1 输出（实测采集，非手写）。
    /// 用固定样本让解析逻辑可在无 FFmpeg 环境下验证。
    const REAL_MP4: &str = r#"{
        "streams": [
            {"index": 0, "codec_name": "h264", "codec_type": "video",
             "profile": "High", "mime_codec_string": "avc1.64001e",
             "width": 640, "height": 360, "level": 30, "pix_fmt": "yuv420p",
             "avg_frame_rate": "30/1", "duration": "60.000000",
             "bit_rate": "521641", "nb_frames": "1800",
             "disposition": {"default": 1, "attached_pic": 0},
             "tags": {"language": "und", "handler_name": "VideoHandler"}},
            {"index": 1, "codec_name": "aac", "codec_type": "audio",
             "profile": "LC", "mime_codec_string": "mp4a.40.2",
             "sample_rate": "44100", "channels": 1, "channel_layout": "mono",
             "duration": "60.000000", "bit_rate": "64436",
             "disposition": {"default": 1},
             "tags": {"language": "und"}}
        ],
        "format": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "format_long_name": "QuickTime / MOV",
            "duration": "60.000000", "size": "4460362", "bit_rate": "594714",
            "tags": {"major_brand": "isom", "encoder": "Lavf63.1.101"}
        }
    }"#;

    #[test]
    fn parses_real_ffprobe_output() {
        let info = parse_probe_json(REAL_MP4).unwrap();
        assert_eq!(info.duration_ms, Some(60_000));
        assert_eq!(info.size, Some(4_460_362));
        assert_eq!(info.video.len(), 1);
        assert_eq!(info.audio.len(), 1);
        let v = &info.video[0];
        assert_eq!(v.codec, "h264");
        assert_eq!(v.width, Some(640));
        assert_eq!(v.height, Some(360));
        // mime_codec 直接来自 ffprobe，不自己拼装
        assert_eq!(v.mime_codec.as_deref(), Some("avc1.64001e"));
        assert_eq!(info.audio[0].mime_codec.as_deref(), Some("mp4a.40.2"));
        assert_eq!(info.audio[0].channels, Some(1));
    }

    #[test]
    fn container_match_handles_comma_separated_list() {
        let info = parse_probe_json(REAL_MP4).unwrap();
        // format_name 是 "mov,mp4,m4a,3gp,3g2,mj2"。
        // 直接用 == "mp4" 比较必然失配——这是最容易踩的坑。
        assert!(info.container_is("mp4"));
        assert!(info.container_is("mov"));
        assert!(info.container_is("MP4"), "应当大小写不敏感");
        assert!(!info.container_is("matroska"));
        // 不能被子串蒙对
        assert!(!info.container_is("p4"));
    }

    #[test]
    fn language_und_is_treated_as_unknown() {
        let info = parse_probe_json(REAL_MP4).unwrap();
        // "und" 是 ffprobe 表示"未定义"的约定，不该当成真实语言展示给用户
        assert_eq!(info.audio[0].language, None);
    }

    #[test]
    fn attached_picture_is_not_counted_as_video() {
        // 带封面的 MP3：封面会被报成一条 video 流。
        // 若不排除，音频文件会被误判为视频，UI 会显示错误的播放界面。
        let json = r#"{
            "streams": [
                {"index": 0, "codec_name": "mp3", "codec_type": "audio",
                 "channels": 2, "sample_rate": "44100"},
                {"index": 1, "codec_name": "mjpeg", "codec_type": "video",
                 "width": 500, "height": 500,
                 "disposition": {"attached_pic": 1}}
            ],
            "format": {"format_name": "mp3", "duration": "180.5"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert!(!info.has_video(), "封面图轨不应算作视频");
        assert!(info.is_audio_only(), "应识别为纯音频");
        assert_eq!(info.duration_ms, Some(180_500));
    }

    #[test]
    fn tolerates_numbers_as_strings_and_numbers() {
        // ffprobe 对同类字段有时给字符串有时给数字，必须都能吃下
        let json = r#"{
            "streams": [
                {"index": "3", "codec_name": "h264", "codec_type": "video",
                 "width": "1920", "height": 1080, "level": 41}
            ],
            "format": {"format_name": "matroska,webm", "duration": 7200,
                       "size": 1234567890}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.video[0].index, 3);
        assert_eq!(info.video[0].width, Some(1920));
        assert_eq!(info.video[0].height, Some(1080));
        assert_eq!(info.video[0].level, Some(41));
        assert_eq!(info.duration_ms, Some(7_200_000));
        assert_eq!(info.size, Some(1_234_567_890));
    }

    #[test]
    fn tolerates_na_and_missing_fields() {
        // 管道探测时 size 常为 "N/A"；某些容器缺 duration
        let json = r#"{
            "streams": [{"index": 0, "codec_name": "vp9", "codec_type": "video"}],
            "format": {"format_name": "matroska,webm", "size": "N/A"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.size, None, "N/A 应解析为 None 而非 0");
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.video[0].width, None);
        // 缺 codec_type 的流被忽略，但不应导致整体失败
        assert_eq!(info.video.len(), 1);
    }

    #[test]
    fn empty_probe_result_is_not_media() {
        let e = parse_probe_json(r#"{"streams": [], "programs": []}"#).unwrap_err();
        assert_eq!(e.code(), "NOT_MEDIA");
        assert!(e.is_degradable(), "非媒体文件应可降级处理");
    }

    #[test]
    fn malformed_json_is_reported_distinctly() {
        let e = parse_probe_json("not json at all").unwrap_err();
        assert_eq!(e.code(), "MALFORMED_OUTPUT");
        // 输出格式不符必须显式报出，不能静默产生错误分级
        assert!(!e.is_degradable());
    }

    #[test]
    fn subtitle_tracks_are_collected_with_flags() {
        let json = r#"{
            "streams": [
                {"index": 0, "codec_name": "h264", "codec_type": "video"},
                {"index": 2, "codec_name": "subrip", "codec_type": "subtitle",
                 "disposition": {"default": 1, "forced": 0},
                 "tags": {"language": "chi", "title": "简体中文"}},
                {"index": 3, "codec_name": "ass", "codec_type": "subtitle",
                 "disposition": {"forced": 1},
                 "tags": {"language": "jpn"}}
            ],
            "format": {"format_name": "matroska,webm"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.subtitles.len(), 2);
        assert_eq!(info.subtitles[0].codec, "subrip");
        assert_eq!(info.subtitles[0].language.as_deref(), Some("chi"));
        assert_eq!(info.subtitles[0].title.as_deref(), Some("简体中文"));
        assert!(info.subtitles[0].default);
        assert!(info.subtitles[1].forced);
    }

    #[test]
    fn zero_frame_rate_is_dropped() {
        // 音频流的 avg_frame_rate 是 "0/0"，作为帧率无意义
        let json = r#"{
            "streams": [{"index": 0, "codec_name": "h264", "codec_type": "video",
                         "avg_frame_rate": "0/0"}],
            "format": {"format_name": "mp4"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.video[0].frame_rate, None);
    }

    #[test]
    fn negative_and_nonfinite_durations_are_rejected() {
        let json = r#"{
            "streams": [{"index": 0, "codec_name": "h264", "codec_type": "video"}],
            "format": {"format_name": "mp4", "duration": "-5"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        // 负时长是无意义的，不能传播成巨大的 u64
        assert_eq!(info.duration_ms, None);
    }

    #[test]
    fn falls_back_to_stream_duration_when_container_lacks_it() {
        // 管道探测 MKV 时容器级 duration 常缺失，但流级有值。
        // 不回退会让 UI 显示"未知时长"、进度条无法工作。
        let json = r#"{
            "streams": [
                {"index": 0, "codec_name": "h264", "codec_type": "video",
                 "duration": "120.5", "bit_rate": "800000"},
                {"index": 1, "codec_name": "aac", "codec_type": "audio",
                 "duration": "119.9", "bit_rate": "128000"}
            ],
            "format": {"format_name": "matroska,webm"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        // 取最长的流时长
        assert_eq!(info.duration_ms, Some(120_500));
        // 容器级码率缺失 → 各流之和
        assert_eq!(info.bit_rate, Some(928_000));
    }

    #[test]
    fn container_duration_takes_precedence_over_streams() {
        let json = r#"{
            "streams": [{"index": 0, "codec_name": "h264",
                         "codec_type": "video", "duration": "10"}],
            "format": {"format_name": "mp4", "duration": "60", "bit_rate": "500000"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.duration_ms, Some(60_000), "容器级应优先");
        assert_eq!(info.bit_rate, Some(500_000));
    }

    #[test]
    fn negative_container_duration_falls_back_to_stream() {
        // 容器级是非法值时应回退而非直接放弃
        let json = r#"{
            "streams": [{"index": 0, "codec_name": "h264",
                         "codec_type": "video", "duration": "42"}],
            "format": {"format_name": "mp4", "duration": "-1"}
        }"#;
        let info = parse_probe_json(json).unwrap();
        assert_eq!(info.duration_ms, Some(42_000));
    }
}
