//! FFmpeg 能力探测：这台机器上的这份 FFmpeg 究竟能做什么。
//!
//! # 为什么必须探测，而不能写死
//!
//! omy 随 Windows 产物内置一份**裁剪过**的 FFmpeg，它按「探测 / 抽帧 /
//! 转封装」的最小集编译，实测 `ffmpeg -encoders` 里只有 `libwebp` 与
//! `mov_text`——**一个视频编码器都没有**。
//!
//! 但用户随时可以换成自己的完整版（`OMY_FFMPEG` 排在查找顺序最前，也可以
//! 直接覆盖安装目录里的文件）。那时压缩、音轨转码就都可用了。
//!
//! 所以「能不能压缩」是**运行期事实**，不是编译期常量。写死成任何一个答案
//! 都会错：写死「不能」则用户换了完整版也用不上；写死「能」则内置版上点了
//! 按钮才报错，而那时用户已经等了很久。
//!
//! # 失效模式
//!
//! 把做不到的事情呈现成能做，代价不是一条错误提示——用户可能已经为一个
//! 两小时的视频等了很久，才发现编码器根本不存在。所以宁可探测慢一点。

use crate::ffprobe::{self, Tool};
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// 探测结果。
///
/// 字段刻意只放**决策需要**的东西，而不是把 FFmpeg 的几百个编码器原样
/// 倒给前端：界面要回答的问题只有「压缩项要不要禁用」「编码下拉里放什么」
/// 「转封装会不会丢音轨」这几个。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Capabilities {
    /// 可用的视频编码器（FFmpeg 的编码器名，如 `libx264`）。
    ///
    /// 空表示不能压缩。顺序按 [`PREFERRED_VIDEO_ENCODERS`] 的偏好排，
    /// 前端可直接拿第一个做默认值。
    pub video_encoders: Vec<String>,
    /// 可用的音频编码器中 omy 关心的那些（目前只有 `aac`）。
    ///
    /// 单列出来是因为它决定一个很具体的后果：源文件是 PCM/DTS 音轨时，
    /// 转封装进 MP4 必须把音轨转成 AAC，没有它就只能出**无声**的 MP4。
    pub audio_encoders: Vec<String>,
    /// 可用的输出容器（muxer 名，如 `mp4`）。
    pub muxers: Vec<String>,
    /// ffmpeg 是否存在。为假时上面几个必然为空。
    pub has_ffmpeg: bool,
    /// ffprobe 是否存在。
    ///
    /// 与 `has_ffmpeg` 分开：只有 ffprobe 时仍能探测和分级，只是不能抽帧。
    pub has_ffprobe: bool,
    /// ffmpeg 版本首行，用于 `doctor` 与问题排查。
    pub version: Option<String>,
}

impl Capabilities {
    /// 能否重新编码视频（即「压缩」是否可用）。
    #[must_use]
    pub fn can_reencode_video(&self) -> bool {
        !self.video_encoders.is_empty()
    }

    /// 能否编码 AAC。
    ///
    /// 决定「转封装到 MP4 时非 AAC 音轨会不会被丢掉」。
    #[must_use]
    pub fn can_encode_aac(&self) -> bool {
        self.audio_encoders.iter().any(|e| e == "aac")
    }

    /// 是否支持某个输出容器。
    #[must_use]
    pub fn has_muxer(&self, name: &str) -> bool {
        self.muxers.iter().any(|m| m == name)
    }
}

/// omy 认得的视频编码器，按推荐顺序。
///
/// 只列这几个而不是收下 FFmpeg 报的全部：完整版里有上百个编码器，其中
/// 绝大多数（`a64multi`、`asv1` 之类）对 omy 毫无意义，全塞进下拉框只会
/// 让用户无从选择。硬件编码器（nvenc/qsv/amf）暂不列入——它们在不同机器上
/// 可用性差异极大，报「有」却在实际编码时失败比不报更糟。
///
/// 元组第二项是给界面的稳定标识，避免前端按名字字符串匹配。
const PREFERRED_VIDEO_ENCODERS: &[(&str, &str)] = &[
    ("libx264", "h264"),
    ("libx265", "hevc"),
    ("libvpx-vp9", "vp9"),
    ("libsvtav1", "av1"),
    ("libaom-av1", "av1"),
];

/// omy 关心的音频编码器。
const WANTED_AUDIO_ENCODERS: &[&str] = &["aac"];

/// omy 可能用作输出的容器。
const WANTED_MUXERS: &[&str] = &["mp4", "mov", "matroska", "webm"];

/// 进程级缓存。
///
/// 探测要起两次子进程（`-encoders` 与 `-muxers`），每次几十毫秒。界面上
/// 每选中一个视频都问一次的话，几百个文件就是几十秒的无谓开销。
///
/// 缓存到进程结束为止：用户中途换掉 FFmpeg 的话需要重启，这个取舍与
/// `ffprobe.rs` 里 `FFPROBE_PATH` 的缓存一致。想立即生效可用 [`refresh`]。
static CAPS: OnceLock<Capabilities> = OnceLock::new();

/// 取能力集，结果被缓存。
#[must_use]
pub fn capabilities() -> &'static Capabilities {
    CAPS.get_or_init(detect)
}

/// 绕过缓存重新探测。
///
/// 供「我换了 FFmpeg，重新检测一下」这类显式操作使用。返回新结果，但
/// **不更新** `OnceLock`——`OnceLock` 按设计只能写一次，强行替换需要
/// 换成 `RwLock`，而那会让每次读都付锁的代价。调用方自行持有返回值即可。
#[must_use]
pub fn refresh() -> Capabilities {
    detect()
}

/// 从 `ffmpeg -encoders` / `-muxers` 的输出里挑出名字。
///
/// # 输出格式
///
/// ```text
/// Encoders:
///  V..... = Video
///  ------
///  V....D libwebp               (codec webp)
///  S..... mov_text
/// ```
///
/// 要点有两个，都踩过：
///
/// 1. **`V..... = Video` 这类图例行必须排除**。它的第二列是 `=`，若不排除
///    会得到一个名叫 `=` 的编码器。
/// 2. **分隔线 `------` 之前的都是表头**，同样要跳过。
///
/// 已实测内置裁剪版与 gyan 完整版的格式一致，同一套规则两边都成立。
fn parse_listing(text: &str, want_prefix: char) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut past_header = false;
    for line in text.lines() {
        if !past_header {
            // FFmpeg 版本间列数不同，分隔线可能是 `--` 或 `------`；
            // 只认「至少两个连字符且没有别的字符」，避免把普通说明误当分隔线。
            if is_header_separator(line) {
                past_header = true;
            }
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(flags), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        // 图例行形如 `V..... = Video`，第二列是 `=`
        if name == "=" {
            continue;
        }
        if flags.starts_with(want_prefix) {
            out.insert(name.to_owned());
        }
    }
    out
}

/// muxer 列表的格式与 encoder 略有不同：标志位是 ` E ` 而不是 `V....D`。
///
/// ```text
///  E mp4             MP4 (MPEG-4 Part 14)
/// ```
fn parse_muxers(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut past_header = false;
    for line in text.lines() {
        if !past_header {
            if is_header_separator(line) {
                past_header = true;
            }
            continue;
        }
        let mut it = line.split_whitespace();
        let (Some(flag), Some(name)) = (it.next(), it.next()) else {
            continue;
        };
        if flag == "E" && name != "=" {
            out.insert(name.to_owned());
        }
    }
    out
}

fn is_header_separator(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 2 && line.bytes().all(|b| b == b'-')
}

/// 跑一次 `ffmpeg <arg>` 并取 stdout。
fn list(arg: &str) -> Option<String> {
    let out = ffprobe::run_with_timeout(
        Tool::Ffmpeg,
        &[
            std::ffi::OsString::from("-hide_banner"),
            std::ffi::OsString::from(arg),
        ],
        None,
    )
    .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 真正执行探测。
fn detect() -> Capabilities {
    let has_ffprobe = ffprobe::has_ffprobe();
    let has_ffmpeg = ffprobe::has_ffmpeg();

    let mut caps = Capabilities {
        has_ffmpeg,
        has_ffprobe,
        ..Capabilities::default()
    };

    if !has_ffmpeg {
        return caps;
    }

    caps.version = ffprobe::version(Tool::Ffmpeg).ok();

    if let Some(text) = list("-encoders") {
        let v = parse_listing(&text, 'V');
        // 按偏好顺序挑，而不是按字母序——前端拿第一个当默认值时，
        // 应该拿到 H.264 而不是碰巧排在前面的 AV1（后者慢得多）
        for (name, _) in PREFERRED_VIDEO_ENCODERS {
            if v.contains(*name) {
                caps.video_encoders.push((*name).to_owned());
            }
        }
        let a = parse_listing(&text, 'A');
        for name in WANTED_AUDIO_ENCODERS {
            if a.contains(*name) {
                caps.audio_encoders.push((*name).to_owned());
            }
        }
    }

    if let Some(text) = list("-muxers") {
        let m = parse_muxers(&text);
        for name in WANTED_MUXERS {
            if m.contains(*name) {
                caps.muxers.push((*name).to_owned());
            }
        }
    }

    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内置裁剪版的真实输出（实测复制，不是手编的）。
    const BUILTIN_ENCODERS: &str = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ...X.. = Codec is experimental
 ....B. = Supports draw_horiz_band
 .....D = Supports direct rendering method 1
 ------
 V....D libwebp               (codec webp)
 S..... mov_text             
";

    /// 完整版的片段（实测自 gyan 9.0.1）。
    const FULL_ENCODERS: &str = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 ------
 V....D a64multi             Multicolor charset for Commodore 64 (codec a64_multi)
 V....D libaom-av1           libaom AV1 (codec av1)
 V..... libsvtav1            SVT-AV1 encoder (codec av1)
 V....D libx264              libx264 H.264 / AVC (codec h264)
 V....D libx265              libx265 H.265 / HEVC (codec hevc)
 V....D libvpx-vp9           libvpx VP9 (codec vp9)
 A....D aac                  AAC (Advanced Audio Coding)
 A....D libmp3lame           libmp3lame MP3 (codec mp3)
 S..... mov_text             
";

    const MUXERS: &str = "\
Muxers:
 E mp4             MP4 (MPEG-4 Part 14)
 E matroska        Matroska
 E mov             QuickTime / MOV
 E webp            WebP
 --
 E mp4             MP4 (MPEG-4 Part 14)
 E matroska        Matroska
 E mov             QuickTime / MOV
 E webp            WebP
";

    /// 裁剪版里一个视频编码器都没有——这是「压缩不可用」的判据。
    ///
    /// 不这样断言会怎样：若解析把 `= Video` 这类图例行当成编码器收下，
    /// 裁剪版会被判成「有视频编码器」，界面于是放开压缩按钮，
    /// 用户点下去等半天才失败。
    #[test]
    fn builtin_has_no_video_encoder() {
        let v = parse_listing(BUILTIN_ENCODERS, 'V');
        assert!(
            !v.contains("="),
            "图例行 `V..... = Video` 被当成了编码器名"
        );
        assert_eq!(v.len(), 1, "裁剪版只应有 libwebp，实际: {v:?}");
        assert!(v.contains("libwebp"));
    }

    /// 裁剪版没有 AAC —— 决定转封装会不会丢音轨。
    #[test]
    fn builtin_has_no_aac() {
        let a = parse_listing(BUILTIN_ENCODERS, 'A');
        assert!(a.is_empty(), "裁剪版不应有任何音频编码器，实际: {a:?}");
    }

    /// 完整版能认出我们关心的几个。
    #[test]
    fn full_build_detects_wanted_encoders() {
        let v = parse_listing(FULL_ENCODERS, 'V');
        for want in ["libx264", "libx265", "libvpx-vp9", "libaom-av1"] {
            assert!(v.contains(want), "没认出 {want}");
        }
        let a = parse_listing(FULL_ENCODERS, 'A');
        assert!(a.contains("aac"));
    }

    /// 表头里的 `A..... = Audio` 不能被 'A' 前缀误收。
    ///
    /// 不这样断言会怎样：完整版的音频编码器集合里会混进一个 `=`，
    /// 虽然不影响 `can_encode_aac`，但同样的疏漏在视频那边就会让
    /// 裁剪版被误判成可以压缩。
    #[test]
    fn legend_rows_never_become_encoder_names() {
        for text in [BUILTIN_ENCODERS, FULL_ENCODERS] {
            for p in ['V', 'A', 'S'] {
                assert!(!parse_listing(text, p).contains("="), "图例行泄漏为编码器名");
            }
        }
    }

    /// muxer 的标志位格式与 encoder 不同，要单独解析。
    #[test]
    fn muxers_are_parsed() {
        let m = parse_muxers(MUXERS);
        assert!(m.contains("mp4"));
        assert!(m.contains("matroska"));
        assert!(!m.contains("="));
    }

    /// 空输入不 panic——FFmpeg 不存在时 list() 返回 None，
    /// 但若它返回了空字符串也必须安全。
    #[test]
    fn empty_input_is_safe() {
        assert!(parse_listing("", 'V').is_empty());
        assert!(parse_muxers("").is_empty());
        // 只有表头、没有分隔线时也不能把表头当数据
        assert!(parse_listing("Encoders:\n V..... = Video\n", 'V').is_empty());
    }

    /// 偏好顺序：H.264 要排在 AV1 前面。
    ///
    /// 不这样断言会怎样：前端拿 `video_encoders[0]` 作默认值，
    /// 若按字母序则 libaom-av1 排第一，用户压一个视频要等几十倍的时间，
    /// 而界面上看不出任何异常。
    #[test]
    fn h264_is_preferred_over_av1() {
        let names: Vec<&str> = PREFERRED_VIDEO_ENCODERS.iter().map(|(n, _)| *n).collect();
        let i264 = names.iter().position(|n| *n == "libx264").unwrap();
        let iav1 = names.iter().position(|n| *n == "libaom-av1").unwrap();
        assert!(i264 < iav1, "H.264 应排在 AV1 之前");
    }

    /// `can_*` 系列如实反映字段。
    #[test]
    fn capability_helpers_follow_fields() {
        let empty = Capabilities::default();
        assert!(!empty.can_reencode_video());
        assert!(!empty.can_encode_aac());

        let full = Capabilities {
            video_encoders: vec!["libx264".into()],
            audio_encoders: vec!["aac".into()],
            muxers: vec!["mp4".into()],
            has_ffmpeg: true,
            has_ffprobe: true,
            version: None,
        };
        assert!(full.can_reencode_video());
        assert!(full.can_encode_aac());
        assert!(full.has_muxer("mp4"));
        assert!(!full.has_muxer("webm"));
    }
}
