//! 整文件的视频转换：转封装，以及（当 FFmpeg 支持时）重新编码。
//!
//! # 为什么不复用 `remux.rs`
//!
//! 那个模块服务的是**播放**：把解密后的一小段字节喂进管道，产出 fMP4 给
//! MSE 播放器，因此它有 256 MiB 的输入上限、强制 `frag_keyframe` 分片、
//! 且全程在内存里。
//!
//! 这里服务的是**加密前的一次性转换**：输入是磁盘上一个可能几 GB 的原始
//! 文件，输出也要落到磁盘。把 1.2 GB 的视频塞进 `Vec<u8>` 再喂管道，内存
//! 峰值就是文件的两倍，而且无谓——这条路径上文件本来就是明文，让 FFmpeg
//! 自己读写文件既快又省。
//!
//! 两者的产物也不同：播放要分片 fMP4，这里要普通 MP4 + faststart。
//! 强行合并成一个函数会让两边的参数互相干扰，所以**刻意分开**。
//!
//! # 安全边界
//!
//! 这里 FFmpeg 直接读写磁盘，与 `omy-media` 顶层注释里「子进程不访问文件
//! 系统」的约定不同。这是有意的例外，成立的前提是：**输入是用户自己选中的
//! 未加密文件，输出是我们指定的临时路径，全程不涉及任何密钥或密文**。
//! 加密之后的内容仍然只走管道，那条约束没有放松。

use crate::error::{MediaError, Result};
use crate::ffprobe::{self, Tool};
use std::path::{Path, PathBuf};

/// 转换任务的超时。
///
/// 转封装通常几秒，但重新编码一部长片可能要几十分钟，所以给得很宽。
/// 完全不设上限不行——恶意构造的文件可能让 FFmpeg 陷进死循环。
const CONVERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4 * 3600);

/// 目标容器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    /// MP4：兼容性最好。
    Mp4,
    /// MOV。
    Mov,
    /// Matroska（.mkv）：字幕与多音轨支持最好。
    Mkv,
}

impl Container {
    /// FFmpeg 的 muxer 名。
    ///
    /// 注意 mkv 的 muxer 叫 `matroska` 而不是 `mkv`——按扩展名写
    /// `-f mkv` 会报 `Unknown output format`。
    #[must_use]
    pub const fn muxer(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "matroska",
        }
    }

    /// 文件扩展名（不含点）。
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
        }
    }

    /// 这个容器能不能直接装下该音频编码。
    ///
    /// MP4/MOV 的音轨编码是有限集：PCM、DTS、Vorbis 这些装不进去，
    /// `-c copy` 会直接失败。判断为假时要么转码音轨，要么丢掉它——
    /// **不能什么都不做**，那会得到一个失败或无声的产物。
    #[must_use]
    pub fn accepts_audio(self, codec: &str) -> bool {
        match self {
            // matroska 几乎什么都能装
            Self::Mkv => true,
            Self::Mp4 | Self::Mov => matches!(
                codec,
                "aac" | "mp3" | "alac" | "ac3" | "eac3" | "opus" | "flac"
            ),
        }
    }
}

/// 音轨的处理方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPlan {
    /// 直接拷贝，不重编码。
    Copy,
    /// 转成 AAC。需要 FFmpeg 有 aac 编码器。
    ToAac,
    /// 丢掉音轨。
    ///
    /// 只在「容器装不下 + 没有 AAC 编码器」时才会走到，
    /// 且**必须**在界面上提前告诉用户。
    Drop,
}

/// 决定音轨怎么处理。
///
/// 这个判断单独成函数是为了能被测试直接调用：它决定用户会不会拿到一个
/// 无声的视频，而那种缺陷在产物上「看起来」完全正常——文件能播、画面正常，
/// 只是没声音，很容易到用户手里才发现。
#[must_use]
pub fn plan_audio(container: Container, audio_codec: Option<&str>, has_aac_encoder: bool) -> AudioPlan {
    let Some(codec) = audio_codec else {
        // 本来就没有音轨，拷贝是空操作
        return AudioPlan::Copy;
    };
    if container.accepts_audio(codec) {
        return AudioPlan::Copy;
    }
    if has_aac_encoder {
        AudioPlan::ToAac
    } else {
        AudioPlan::Drop
    }
}

/// 转换请求。
#[derive(Debug, Clone)]
pub struct ConvertOptions {
    /// 目标容器。
    pub container: Container,
    /// 音轨处理方式。
    pub audio: AudioPlan,
    /// 是否把索引移到文件开头（MP4/MOV 才有意义）。
    pub faststart: bool,
    /// 重新编码的参数。`None` 表示只转封装（`-c:v copy`）。
    pub video_encode: Option<VideoEncode>,
}

/// 视频重新编码参数。
#[derive(Debug, Clone)]
pub struct VideoEncode {
    /// FFmpeg 编码器名，如 `libx264`。必须来自能力探测的结果。
    pub encoder: String,
    /// CRF 质量值。越小越好越大。
    pub crf: u8,
    /// 缩放到的高度（像素）。`None` 保持原样。
    ///
    /// 只给高度、宽度按比例算：写死 1920x1080 会把竖屏视频拉变形。
    pub height: Option<u32>,
    /// 目标帧率。`None` 保持原样。
    pub fps: Option<u32>,
}

/// 转换结果。
#[derive(Debug, Clone)]
pub struct ConvertOutput {
    /// 产物路径。
    pub path: PathBuf,
    /// 产物字节数。
    pub size: u64,
    /// 值得转达给用户的警告。
    pub warnings: Vec<String>,
}

/// 构造 FFmpeg 参数。
///
/// 单独成函数以便测试断言，不必真去跑一次几分钟的转换。
fn build_args(input: &Path, output: &Path, opts: &ConvertOptions) -> Vec<std::ffi::OsString> {
    let mut a: Vec<std::ffi::OsString> = Vec::new();
    // 用宏而不是闭包：闭包会一直持有 `a` 的可变借用，而中间还要
    // `a.push(path.into())` 推入 OsString，两者冲突编译不过。
    macro_rules! push {
        ($s:expr) => {
            a.push(std::ffi::OsString::from($s))
        };
    }

    push!("-hide_banner");
    push!("-loglevel");
    push!("warning");
    // 覆盖已存在的输出文件。我们用的是自己生成的临时路径，
    // 不加这个会在重试时卡在「File exists. Overwrite? [y/N]」等输入
    push!("-y");
    push!("-i");
    a.push(input.into());

    // 选流：视频与音频都可选（`?` 后缀），没有就跳过而不是报错。
    // 不能写 `-map 0`：那会带上附件、章节等 MP4 装不下的东西
    push!("-map");
    push!("0:v:0?");
    if opts.audio != AudioPlan::Drop {
        push!("-map");
        push!("0:a?");
    }

    // 视频
    match &opts.video_encode {
        None => {
            push!("-c:v");
            push!("copy");
        }
        Some(enc) => {
            push!("-c:v");
            push!(enc.encoder.as_str());
            push!("-crf");
            push!(enc.crf.to_string());
            if let Some(h) = enc.height {
                // -2 让宽度按比例自动算并保证是偶数：写 -1 时算出奇数宽度，
                // yuv420p 要求偶数，编码器会直接报错
                push!("-vf");
                push!(format!("scale=-2:{h}"));
            }
            if let Some(f) = enc.fps {
                push!("-r");
                push!(f.to_string());
            }
        }
    }

    // 音频
    match opts.audio {
        AudioPlan::Copy => {
            push!("-c:a");
            push!("copy");
        }
        AudioPlan::ToAac => {
            push!("-c:a");
            push!("aac");
            push!("-b:a");
            push!("192k");
        }
        AudioPlan::Drop => push!("-an"),
    }

    // 字幕：一律丢弃。
    //
    // 这条是有意的简化，且必须在界面上说明。原因是字幕的正确处理要分三种
    // 情况（文本转 mov_text / 位图只能丢 / mkv 可直接拷），而判断依据要靠
    // 逐轨探测。在「加密前顺手转一下」这个场景里，多数文件没有字幕，
    // 为它引入三分支的复杂度不划算——真需要保留字幕的用户应该选 MKV 并
    // 走「不处理」。
    push!("-sn");

    if opts.faststart && matches!(opts.container, Container::Mp4 | Container::Mov) {
        push!("-movflags");
        push!("+faststart");
    }

    push!("-f");
    push!(opts.container.muxer());
    a.push(output.into());
    a
}

/// 从 stderr 里挑出值得转达的行。
///
/// 与 `remux.rs` 的同名函数分开：那里关心的是播放相关的问题，
/// 这里还要关心编码器与容器不匹配这类转换特有的失败。
fn extract_warnings(stderr: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let interesting = l.contains("Could not find codec")
            || l.contains("not supported")
            || l.contains("Ignoring")
            || l.contains("dropped")
            || l.contains("Dropping")
            || l.contains("does not contain")
            || l.contains("Incompatible")
            || l.contains("deprecated pixel format")
            || l.contains("Non-monotonous");
        if interesting && out.len() < 8 {
            out.push(l.to_owned());
        }
    }
    out
}

/// 执行一次转换。
///
/// # Errors
///
/// FFmpeg 不可用、输入不存在、进程失败或产物为空时返回错误。
pub fn convert(input: &Path, output: &Path, opts: &ConvertOptions) -> Result<ConvertOutput> {
    if !input.is_file() {
        return Err(MediaError::InvalidInput {
            reason: format!("输入不是文件：{}", input.display()),
        });
    }

    let args = build_args(input, output, opts);
    let out = ffprobe::run_with_timeout(Tool::Ffmpeg, &args, Some(CONVERT_TIMEOUT))?;
    let warnings = extract_warnings(&out.stderr);

    if !out.success() {
        // 失败时把半成品删掉。留着的话，下次用户看到一个体积不对的文件，
        // 而它既不能播也不知道是哪来的
        let _ = std::fs::remove_file(output);
        let mut detail: Vec<&str> = out.stderr.lines().rev().take(4).collect();
        detail.reverse();
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: format!("转换失败：{}", detail.join(" | ")),
        });
    }

    // 退出码 0 不足以说明成功。
    //
    // 这是 remux.rs 缺陷 #9 的同类问题：FFmpeg 可能产出一个空文件或只有
    // 文件头的残片却仍然退出 0。不检查的话用户会拿到一个「存在但打不开」
    // 的文件，而错误要到播放时才暴露。
    let size = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        let _ = std::fs::remove_file(output);
        return Err(MediaError::MalformedOutput {
            reason: String::from("转换产出了空文件"),
        });
    }

    Ok(ConvertOutput {
        path: output.to_path_buf(),
        size,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(opts: &ConvertOptions) -> Vec<String> {
        build_args(Path::new("in.mov"), Path::new("out.mp4"), opts)
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect()
    }

    fn base() -> ConvertOptions {
        ConvertOptions {
            container: Container::Mp4,
            audio: AudioPlan::Copy,
            faststart: true,
            video_encode: None,
        }
    }

    /// mkv 的 muxer 名是 matroska，不是 mkv。
    ///
    /// 不这样断言会怎样：`-f mkv` 让 FFmpeg 报 Unknown output format，
    /// 而错误信息里只有 "mkv"，看起来像不支持这个格式。
    #[test]
    fn mkv_uses_matroska_muxer_name() {
        assert_eq!(Container::Mkv.muxer(), "matroska");
        assert_eq!(Container::Mkv.extension(), "mkv");
    }

    /// 只转封装时绝不能出现编码器参数。
    ///
    /// 不这样断言会怎样：混进 -c:v libx264 会让「秒级、零画质损失」变成
    /// 几十分钟的重新编码，而界面上还写着「不碰视频流」。
    #[test]
    fn remux_only_copies_video() {
        let a = args_of(&base());
        assert!(a.windows(2).any(|w| w == ["-c:v", "copy"]));
        assert!(!a.iter().any(|s| s.starts_with("libx")), "混进了编码器: {a:?}");
        assert!(!a.iter().any(|s| s == "-crf"));
    }

    /// PCM 音轨进 MP4：有 AAC 编码器时转码，没有时丢弃。
    ///
    /// 这是整个转换里最容易让用户吃亏的判断——丢音轨的产物看起来完全正常。
    #[test]
    fn pcm_into_mp4_needs_aac_or_gets_dropped() {
        assert_eq!(
            plan_audio(Container::Mp4, Some("pcm_s16le"), true),
            AudioPlan::ToAac
        );
        assert_eq!(
            plan_audio(Container::Mp4, Some("pcm_s16le"), false),
            AudioPlan::Drop
        );
        // 同样的 PCM 进 MKV 则可以直接拷贝——容器能装下
        assert_eq!(
            plan_audio(Container::Mkv, Some("pcm_s16le"), false),
            AudioPlan::Copy
        );
    }

    /// AAC 进 MP4 必须直接拷贝，不能白白重编码一遍。
    ///
    /// 不这样断言会怎样：把已经是 AAC 的音轨再转一次 AAC，是无谓的质量损失
    /// （有损转有损），而且用户完全看不出来。
    #[test]
    fn aac_into_mp4_is_copied_not_reencoded() {
        assert_eq!(plan_audio(Container::Mp4, Some("aac"), true), AudioPlan::Copy);
    }

    /// 没有音轨时不应该试图转码。
    #[test]
    fn missing_audio_track_plans_copy() {
        assert_eq!(plan_audio(Container::Mp4, None, false), AudioPlan::Copy);
    }

    /// 丢弃音轨时要给 -an，且不能再出现 -map 0:a。
    ///
    /// 不这样断言会怎样：既映射了音轨又加 -an，FFmpeg 的行为取决于参数顺序，
    /// 可能产出一条空音轨——播放器显示"有音轨但没声音"，比干脆没有更费解。
    #[test]
    fn dropping_audio_omits_the_mapping() {
        let mut o = base();
        o.audio = AudioPlan::Drop;
        let a = args_of(&o);
        assert!(a.iter().any(|s| s == "-an"));
        assert!(!a.windows(2).any(|w| w == ["-map", "0:a?"]), "仍映射了音轨: {a:?}");
    }

    /// 缩放必须用 -2 而不是 -1。
    ///
    /// 不这样断言会怎样：-1 可能算出奇数宽度，而 yuv420p 要求偶数，
    /// 编码直接失败，报错是 "width not divisible by 2"，
    /// 和用户选的「1080p」看不出关系。
    #[test]
    fn scale_keeps_width_even() {
        let mut o = base();
        o.video_encode = Some(VideoEncode {
            encoder: "libx264".into(),
            crf: 23,
            height: Some(1080),
            fps: None,
        });
        let a = args_of(&o);
        let i = a.iter().position(|s| s == "-vf").expect("缺少 -vf");
        assert_eq!(a.get(i + 1).map(String::as_str), Some("scale=-2:1080"));
    }

    /// faststart 只对 MP4/MOV 有意义，不该出现在 mkv 的参数里。
    ///
    /// 不这样断言会怎样：mkv 加 -movflags 会让 FFmpeg 报
    /// "Unrecognized option"，整个转换失败。
    #[test]
    fn faststart_is_skipped_for_mkv() {
        let mut o = base();
        o.container = Container::Mkv;
        let a = args_of(&o);
        assert!(!a.iter().any(|s| s == "-movflags"), "mkv 不该有 movflags: {a:?}");

        let a2 = args_of(&base());
        assert!(a2.windows(2).any(|w| w == ["-movflags", "+faststart"]));
    }

    /// 必须有 -y，否则重试时会卡在交互提示上等 stdin。
    ///
    /// 不这样断言会怎样：表现是转换「卡住不动」，没有任何错误，
    /// 只有超时之后才失败——而超时设的是 4 小时。
    #[test]
    fn overwrite_flag_is_present() {
        assert!(args_of(&base()).iter().any(|s| s == "-y"));
    }

    /// 输出格式要显式给 -f，不能依赖扩展名推断。
    #[test]
    fn output_format_is_explicit() {
        let a = args_of(&base());
        let i = a.iter().position(|s| s == "-f").expect("缺少 -f");
        assert_eq!(a.get(i + 1).map(String::as_str), Some("mp4"));
    }

    /// 不存在的输入要给出明确错误，而不是交给 FFmpeg 报一句看不懂的话。
    #[test]
    fn missing_input_is_rejected_early() {
        let r = convert(
            Path::new("definitely-not-here-9e3f.mov"),
            Path::new("out.mp4"),
            &base(),
        );
        assert!(matches!(r, Err(MediaError::InvalidInput { .. })));
    }
}
