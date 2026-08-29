//! 转封装：把非 MP4 容器的音视频流重新装进 fMP4，供 MSE 播放。
//!
//! # 这是 P2 播放路径
//!
//! `<video>` 不支持 MKV，但里面的 H.264/AAC 流本身浏览器完全能解。
//! 转封装只换容器、不碰编码（`-c copy`），耗时是**数十毫秒**级别，
//! 而全解码转码是数百毫秒到秒级。
//!
//! # 为什么产出 fMP4 而不是普通 MP4
//!
//! 普通 MP4 的 `moov` 必须包含全部样本的偏移表，muxer 要在写完所有数据
//! 后**回头改写**文件开头。管道输出做不到回退，所以只能用分片格式：
//! 每个 `moof` 自带该片段的信息，可以边产边发。
//!
//! 这也正好是 MSE 需要的格式：先 append 一个 init segment（`ftyp`+`moov`），
//! 再逐个 append media segment（`moof`+`mdat`）。
//!
//! # 已实测确认的参数
//!
//! Spike 逐个组合验证过，结论：
//!
//! | 参数 | 作用 | 不加会怎样 |
//! |---|---|---|
//! | `empty_moov` | 让 `moov` 不含样本表 | muxer 需要 seek 回写，管道下失败 |
//! | `frag_keyframe` | 在关键帧处切片 | 片段边界可能落在非关键帧，seek 不准 |
//! | `default_base_moof` | 偏移基准明确 | 某些播放器解析出错位 |
//!
//! # 关于 seek
//!
//! **不要**用 FFmpeg 的输入 seek（`-ss` 放在 `-i` 前）。实测在管道输入下
//! 它会失败，报 `Seek to desired resync point failed`，产物只有
//! `ftyp`+`moov` 而没有 `moof`——因为管道无法 seek。
//!
//! 正确做法是调用方先用 [`crate::mkv`] 算出目标时间对应的字节区间，
//! 只把那段字节（拼上文件头）喂进来。这样 FFmpeg 从头读到尾，
//! 但"头"和"尾"本身就只是用户要看的那一小段。
//!
//! # ⚠️ 不要用管道探测去核对 fMP4 的时长
//!
//! `empty_moov` 让 `moov` 不含总时长，时长信息分散在各个 `moof` 里。
//! ffprobe 走管道读不到文件末尾，只能报**第一个 fragment** 的时长。
//!
//! 实测对照（`spikes/dbg-fmp4-duration.ps1`）：
//!
//! | 探测方式 | 报告时长 |
//! |---|---|
//! | fMP4 + 文件路径（可 seek） | 60.02s ✅ |
//! | fMP4 + 管道（不可 seek） | **2.04s** ❌ |
//! | 非分片 MP4 + 管道 | 60.00s ✅ |
//! | 真解码全片 | 60.02s ✅ |
//!
//! 这是 fMP4 的固有特性，**不是 remux 出了问题**。曾据此误判整文件
//! remux 失败，实际产物有 60 个 `moof`、真解码满 60 秒。
//! 要核对完整性就数 `moof` 数量，或用文件路径而非管道探测。

use crate::error::{MediaError, Result};
use crate::ffprobe::{self, Tool};

/// 单次 remux 的输入上限。
///
/// 防止把整部电影一次性喂进来——那既慢又占内存，
/// 而且违背按需产片段的设计。正常一个片段是几百 KB 到几 MB。
const MAX_INPUT_BYTES: usize = 256 * 1024 * 1024;

/// fMP4 分片时长（微秒）。
///
/// 2 秒是流媒体的常用值：太短则 `moof` 开销占比高，
/// 太长则 seek 粒度粗、起播慢。
const FRAG_DURATION_US: &str = "2000000";

/// remux 的产物。
#[derive(Debug, Clone)]
pub struct RemuxOutput {
    /// fMP4 字节。
    pub data: Vec<u8>,
    /// FFmpeg 的警告信息（stderr 中的可疑行）。
    ///
    /// 即使成功也可能有警告，例如某条轨道被丢弃。
    /// 调用方应当展示给用户——"能播但没声音"比直接失败更让人困惑。
    pub warnings: Vec<String>,
}

/// 转封装选项。
#[derive(Debug, Clone)]
pub struct RemuxOptions {
    /// 只保留这些流。空表示全部保留。
    ///
    /// 格式同 FFmpeg 的 `-map`，如 `0:v:0`、`0:a:1`。
    /// 文档 §5.4 的"多轨道可选"就靠这个：一个 DTS+AAC 双音轨的 MKV
    /// 判为 P3，但只映射 AAC 那条就能降到 P2。
    pub map: Vec<String>,
    /// 是否切分片段（`frag_keyframe`）。
    pub fragment: bool,
    /// 分片时长（微秒）。`None` 用默认值。
    pub frag_duration_us: Option<u64>,
    /// 丢弃字幕轨。
    ///
    /// MP4 只能装 `mov_text`，PGS/VobSub 这类位图字幕装不进去，
    /// 强行 remux 会失败。判定为图形字幕时应当置真。
    pub drop_subtitles: bool,
    /// 超时。
    pub timeout: Option<std::time::Duration>,
}

impl Default for RemuxOptions {
    fn default() -> Self {
        Self {
            map: Vec::new(),
            fragment: true,
            frag_duration_us: None,
            drop_subtitles: false,
            timeout: None,
        }
    }
}

/// 从 FFmpeg 的 stderr 里挑出值得转达给用户的行。
///
/// FFmpeg 输出极其啰嗦，绝大多数是无用的构建信息与进度。
/// 只保留真正影响播放结果的。
fn extract_warnings(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        // 这些模式意味着"结果和用户预期可能不一致"
        let interesting = l.contains("Could not find codec")
            || l.contains("not supported")
            || l.contains("Ignoring")
            || l.contains("dropped")
            || l.contains("Dropping")
            || l.contains("does not contain")
            || l.contains("Non-monotonous")
            || l.contains("codec frame size is not set");
        if interesting && out.len() < 8 {
            out.push(l.to_owned());
        }
    }
    out
}

/// 构造 FFmpeg 参数。
fn build_args(opts: &RemuxOptions) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        // 只留错误，进度信息会淹没有用的警告
        "-loglevel".into(),
        "warning".into(),
        "-i".into(),
        "pipe:0".into(),
    ];

    if opts.map.is_empty() {
        // 不指定时保留视频与音频，字幕按选项决定。
        // 不能只写 `-map 0`：那会带上附件、章节等 MP4 装不下的东西。
        args.push("-map".into());
        args.push("0:v?".into());
        args.push("-map".into());
        args.push("0:a?".into());
        if !opts.drop_subtitles {
            args.push("-map".into());
            args.push("0:s?".into());
        }
    } else {
        for m in &opts.map {
            args.push("-map".into());
            args.push(m.clone());
        }
    }

    // 只换容器，不重编码——这是 P2 的全部意义
    args.push("-c".into());
    args.push("copy".into());

    if opts.drop_subtitles {
        args.push("-sn".into());
    } else {
        // 文本字幕转成 MP4 能装的 mov_text；
        // 位图字幕转不了，调用方应当先设 drop_subtitles
        args.push("-c:s".into());
        args.push("mov_text".into());
    }

    let mut flags = String::from("empty_moov+default_base_moof");
    if opts.fragment {
        flags.push_str("+frag_keyframe");
    }
    args.push("-movflags".into());
    args.push(flags);

    if opts.fragment {
        args.push("-frag_duration".into());
        args.push(
            opts.frag_duration_us
                .map_or_else(|| FRAG_DURATION_US.to_owned(), |d| d.to_string()),
        );
    }

    args.push("-f".into());
    args.push("mp4".into());
    args.push("pipe:1".into());
    args
}

/// 把一段媒体字节转封装成 fMP4。
///
/// `input` 应当是**可独立 demux** 的完整数据：
/// 对 MKV 而言是 `文件头 + Cluster 区间`（见 [`crate::mkv::splice`]），
/// 不能是裸的 Cluster——实测裸 Cluster 会报 `Invalid data found`，
/// 因为解码器拿不到 Tracks 里的编码参数。
///
/// # Errors
///
/// FFmpeg 不可用、输入超过上限、进程失败或产出的不是有效 MP4 时返回错误。
pub fn remux_to_fmp4(input: &[u8], opts: &RemuxOptions) -> Result<RemuxOutput> {
    if input.is_empty() {
        return Err(MediaError::InvalidInput {
            reason: "输入为空".to_string(),
        });
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(MediaError::InvalidInput {
            reason: format!(
                "输入 {} 字节超过单次 remux 上限 {MAX_INPUT_BYTES}——应当分段处理",
                input.len()
            ),
        });
    }

    let args = build_args(opts);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

    let out = ffprobe::run_piped(Tool::Ffmpeg, &arg_refs, input.to_vec())?;

    let warnings = extract_warnings(&out.stderr);

    if !out.success() {
        let msg = out.stderr.as_str();
        // 取最后几行——FFmpeg 的真正死因通常在末尾
        let mut detail: Vec<&str> = msg.lines().rev().take(4).collect();
        detail.reverse();
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: format!("remux 失败：{}", detail.join(" | ")),
        });
    }

    // 关键：不能只看退出码和长度。
    //
    // 缺陷 #9 的教训——FFmpeg 的错误文本也有长度，曾被误判为有效产物。
    // 这里验证产物真的是 MP4 结构。
    if !crate::mp4::looks_like_mp4(&out.stdout) {
        return Err(MediaError::MalformedOutput {
            reason: format!(
                "remux 产出 {} 字节，但不是有效 MP4（前 16 字节：{:?}）",
                out.stdout.len(),
                out.stdout.get(..16.min(out.stdout.len()))
            ),
        });
    }

    Ok(RemuxOutput {
        data: out.stdout,
        warnings,
    })
}

/// 产出 MSE 需要的 init segment（`ftyp` + `moov`，不含媒体数据）。
///
/// MSE 的 `SourceBuffer` 必须先收到 init segment 才能接受后续片段。
/// 它描述轨道数量、编码参数、时间基准，而不含任何样本。
///
/// # 实现说明
///
/// 用 `-frames:v 0 -frames:a 0` 让 FFmpeg 不写任何帧。实测产出
/// 1253 字节，结构为 `ftyp`+`moov`（无 `moof`），正是所需。
///
/// # Errors
///
/// 同 [`remux_to_fmp4`]。
pub fn init_segment(input: &[u8], opts: &RemuxOptions) -> Result<RemuxOutput> {
    let mut args = build_args(opts);
    // 插在 -f mp4 之前：FFmpeg 的输出选项必须在输出 URL 之前
    let pos = args.len().saturating_sub(3);
    args.splice(
        pos..pos,
        [
            "-frames:v".to_owned(),
            "0".to_owned(),
            "-frames:a".to_owned(),
            "0".to_owned(),
        ],
    );

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = ffprobe::run_piped(Tool::Ffmpeg, &arg_refs, input.to_vec())?;
    let warnings = extract_warnings(&out.stderr);

    if !out.success() || !crate::mp4::looks_like_mp4(&out.stdout) {
        let msg = out.stderr.as_str();
        let mut tail: Vec<&str> = msg.lines().rev().take(3).collect();
        tail.reverse();
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: format!("产出 init segment 失败：{}", tail.join(" | ")),
        });
    }

    Ok(RemuxOutput {
        data: out.stdout,
        warnings,
    })
}

/// 判断一段 fMP4 是否含有媒体片段（`moof`）。
///
/// init segment 只有 `ftyp`+`moov`；media segment 才有 `moof`+`mdat`。
/// 用来校验产物类型是否符合预期。
///
/// # Errors
///
/// box 结构损坏时返回错误。
pub fn has_fragments(data: &[u8]) -> Result<bool> {
    let boxes = crate::mp4::top_level_boxes(data)?;
    Ok(boxes.iter().any(|b| b.kind == *b"moof"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_use_verified_movflags() {
        let args = build_args(&RemuxOptions::default());
        let joined = args.join(" ");
        // 这三个 flag 是 spike 逐个验证过的，不能随意改动
        assert!(joined.contains("empty_moov"), "缺 empty_moov 会导致管道输出失败");
        assert!(
            joined.contains("default_base_moof"),
            "缺 default_base_moof 某些播放器会解析出错位"
        );
        assert!(joined.contains("frag_keyframe"), "缺 frag_keyframe 片段边界不在关键帧");
        assert!(joined.contains("-c copy"), "P2 必须只换容器不重编码");
    }

    #[test]
    fn never_uses_input_seek() {
        // 输入 seek 在管道下实测失效。若有人把 -ss 加到 -i 前面，
        // 产物会变成没有 moof 的空壳，且不报错——极难排查。
        let args = build_args(&RemuxOptions::default());
        let i_pos = args.iter().position(|a| a == "-i").expect("应有 -i");
        let ss_pos = args.iter().position(|a| a == "-ss");
        assert!(
            ss_pos.is_none_or(|p| p > i_pos),
            "-ss 绝不能出现在 -i 之前：管道输入下输入 seek 会失败"
        );
    }

    #[test]
    fn map_defaults_avoid_map_zero() {
        // `-map 0` 会带上附件和章节，MP4 装不下 → remux 失败。
        // 必须逐类型映射。
        let args = build_args(&RemuxOptions::default());
        for (i, a) in args.iter().enumerate() {
            if a == "-map" {
                let v = &args[i + 1];
                assert_ne!(v, "0", "不能用 -map 0，会带上 MP4 装不下的附件与章节");
                assert!(
                    v.ends_with('?'),
                    "映射应当用可选形式（如 0:v?），否则缺某类流时会直接失败：{v}"
                );
            }
        }
    }

    #[test]
    fn drop_subtitles_removes_subtitle_mapping() {
        let opts = RemuxOptions {
            drop_subtitles: true,
            ..RemuxOptions::default()
        };
        let args = build_args(&opts);
        let joined = args.join(" ");
        assert!(joined.contains("-sn"), "应当显式禁用字幕");
        assert!(!joined.contains("0:s?"), "不应再映射字幕流");
        assert!(
            !joined.contains("mov_text"),
            "已丢弃字幕就不该再指定字幕编码"
        );
    }

    #[test]
    fn custom_map_overrides_defaults() {
        let opts = RemuxOptions {
            map: vec!["0:v:0".into(), "0:a:1".into()],
            ..RemuxOptions::default()
        };
        let args = build_args(&opts);
        let joined = args.join(" ");
        assert!(joined.contains("0:v:0"));
        assert!(joined.contains("0:a:1"));
        // 用了自定义映射就不该再有默认的
        assert!(!joined.contains("0:v?"), "自定义映射时不应混入默认映射");
    }

    #[test]
    fn empty_input_is_rejected() {
        let e = remux_to_fmp4(&[], &RemuxOptions::default());
        assert!(e.is_err(), "空输入必须报错而不是交给 FFmpeg");
    }

    #[test]
    fn oversized_input_is_rejected_before_spawning() {
        // 不能把整部电影喂进来。这个检查要在启动进程**之前**做，
        // 否则会先复制一份几 GB 的内存再失败。
        let huge = vec![0u8; MAX_INPUT_BYTES + 1];
        let e = remux_to_fmp4(&huge, &RemuxOptions::default());
        assert!(e.is_err());
        let msg = format!("{}", e.unwrap_err());
        assert!(msg.contains("分段"), "错误信息应当提示正确做法：{msg}");
    }

    #[test]
    fn extract_warnings_filters_noise() {
        let stderr = "ffmpeg version 7.0\n\
                      configuration: --enable-gpl\n\
                      Stream #0:1: Audio: dts\n\
                      [mp4] Could not find codec parameters\n\
                      frame= 120 fps=0.0 q=-1.0 Lsize= 399KiB\n";
        let w = extract_warnings(stderr);
        assert_eq!(w.len(), 1, "只应保留真正的警告：{w:?}");
        assert!(w[0].contains("Could not find codec"));
    }

    #[test]
    fn extract_warnings_caps_output() {
        // 恶意或损坏文件可能刷屏，不能无限累积
        let mut s = String::new();
        for _ in 0..100 {
            s.push_str("something not supported here\n");
        }
        let w = extract_warnings(&s);
        assert!(w.len() <= 8, "警告数量应有上限，实际 {}", w.len());
    }

    #[test]
    fn init_segment_args_disable_frames() {
        let mut args = build_args(&RemuxOptions::default());
        let pos = args.len().saturating_sub(3);
        args.splice(
            pos..pos,
            [
                "-frames:v".to_owned(),
                "0".to_owned(),
                "-frames:a".to_owned(),
                "0".to_owned(),
            ],
        );
        // -frames 必须在输出 URL 之前，否则 FFmpeg 会报参数错误
        let vpos = args.iter().position(|a| a == "-frames:v").expect("应有");
        let opos = args.iter().position(|a| a == "pipe:1").expect("应有输出");
        assert!(vpos < opos, "-frames:v 必须在输出 URL 之前");
    }
}
