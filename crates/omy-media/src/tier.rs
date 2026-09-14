//! 播放路径分级：判定文件走 P1 直通 / P2 转封装 / P3 全解码。
//!
//! 依据 `docs/research/04-media-playback.md` §5.2 的兼容性矩阵。
//!
//! # 为什么分级要给出理由和备选
//!
//! 文档 §5.4 要求把分级写入 `TLV_MEDIA_META`，让列表页无需重新探测就能
//! 判断能否内嵌播放。但**只给一个等级是不够的**：一个 DTS + H.264 的 MKV
//! 被判为 P3，可它若另有一条 AAC 音轨，切过去就能降到 P2。所以结果里
//! 必须带上**原因**与**备选方案**，否则 UI 无法引导用户。
//!
//! # 平台差异不在这里硬编码
//!
//! 本模块给出的是**保守的跨平台基线**。真实能力必须在运行时用
//! `MediaSource.isTypeSupported()` 探测（文档 §6.3 明确要求），
//! 因为同一编码在 Chromium 与 WebKit、乃至不同版本间表现不同。
//! 硬编码平台判断会随浏览器版本失效。

use crate::probe::MediaInfo;

/// 播放路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PlaybackTier {
    /// ⚡ 直通：自定义协议 + Range，零处理。
    P1,
    /// 🔄 转封装：FFmpeg demux → fMP4 → MSE。
    P2,
    /// 🐌 全解码：FFmpeg 解码转码 → MSE。
    P3,
}

impl PlaybackTier {
    /// 用于 UI 的图标。
    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::P1 => "⚡",
            Self::P2 => "🔄",
            Self::P3 => "🐌",
        }
    }

    /// 稳定的字符串标识，供序列化与日志使用。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::P1 => "P1",
            Self::P2 => "P2",
            Self::P3 => "P3",
        }
    }
}

/// 降级到某等级的备选方案。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Alternative {
    /// 采用此方案可达到的等级。
    pub tier: PlaybackTier,
    /// 具体操作说明，如"选择音轨 1（aac）"。
    pub note: String,
    /// 若需切换音轨，这里是目标流序号。
    pub audio_index: Option<u32>,
}

/// 分级结论。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TierVerdict {
    /// 默认采用的等级。
    pub tier: PlaybackTier,
    /// 为何是这个等级——UI 要显示给用户，不能只给个代号。
    pub reason: String,
    /// 可降低等级的备选方案。
    pub alternatives: Vec<Alternative>,
    /// 容器本身是否被 `<video>` 原生支持。
    pub container_ok: bool,
    /// 是否存在无法封入 MP4 的轨道（决定 P2 是否可行）。
    pub needs_transcode: bool,
}

/// `<video>` 原生支持的容器（跨平台交集，保守）。
///
/// **刻意不含 `webm`**：见 [`container_natively_supported`] 的说明——
/// ffprobe 对 MKV 与 WebM 返回同一个 `format_name`（`matroska,webm`），
/// 单看容器名无法区分，必须结合编码判断。
const NATIVE_CONTAINERS: &[&str] = &["mp4", "mov", "m4a", "ogg", "mp3", "wav", "flac"];

/// WebM 容器规范允许的视频编码。
///
/// WebM 是 Matroska 的严格子集。判断一个 `matroska,webm` 文件到底是
/// 可播的 WebM 还是不可播的 MKV，唯一可靠的依据就是里面的编码是否
/// 都在 WebM 的允许范围内。
const WEBM_VIDEO: &[&str] = &["vp8", "vp9", "av1"];

/// WebM 容器规范允许的音频编码。
const WEBM_AUDIO: &[&str] = &["vorbis", "opus"];

/// `<video>` 原生支持的视频编码（保守交集）。
///
/// HEVC / AV1 刻意**不列入**：两者都依赖硬件与版本
///（文档 §5.2 标为 ⚠️），运行时探测才准。
const NATIVE_VIDEO: &[&str] = &["h264", "vp8", "vp9", "theora"];

/// `<video>` 原生支持的音频编码（保守交集）。
///
/// AC-3 / E-AC-3 不列入：Chrome 已移除支持。
const NATIVE_AUDIO: &[&str] = &["aac", "mp3", "opus", "vorbis", "flac", "pcm_s16le", "pcm_s24le"];

/// 无法封入 MP4 容器的音频编码。
///
/// 这是 P2（转封装）的硬限制：MP4 装不下这些，remux 必然失败，
/// 只能转码（落到 P3）。文档 §5.3 特别强调了这一点——
/// 用户手上的 MKV 之所以是 MKV，往往正因为装的是这些。
const MP4_INCOMPATIBLE_AUDIO: &[&str] = &[
    "dts",
    "dts_hd",
    "truehd",
    "mlp",
    "vorbis",
    "pcm_bluray",
    "pcm_dvd",
];

/// 无法转为 `mov_text` 的图形字幕。
///
/// PGS/VobSub 是位图字幕，MP4 的 `mov_text` 只能装文本，
/// remux 时字幕会丢失。
///
/// `pub` 是为了让 `convert.rs` 复用同一份判据。**不要在别处复制一份**：
/// 两处定义迟早分叉，而分叉的后果是分级说「会丢字幕」、转换却当成文本
/// 去转（或反过来），两条路径给用户的说法自相矛盾。
pub const BITMAP_SUBTITLES: &[&str] = &["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"];

/// 是否为位图字幕。
#[must_use]
pub fn is_bitmap_subtitle(codec: &str) -> bool {
    contains_ci(BITMAP_SUBTITLES, codec)
}

/// 是否为带样式的字幕（ASS / SSA）。
///
/// 单独识别是因为它有一个**别的文本字幕没有的代价**：转成 mov_text 后
/// 文字还在，但字体、颜色、定位、卡拉OK 特效全部丢失。用户看到的是
/// 「字幕还在，只是变成了白底黑字」——不给提示的话会以为是我们弄坏了。
#[must_use]
pub fn is_styled_subtitle(codec: &str) -> bool {
    contains_ci(&["ass", "ssa"], codec)
}

fn contains_ci(list: &[&str], v: &str) -> bool {
    list.iter().any(|x| x.eq_ignore_ascii_case(v))
}

/// 容器是否被 `<video>` 原生支持。
///
/// # 为什么不能只比容器名
///
/// ffprobe 对 **MKV 和 WebM 返回完全相同**的 `format_name`
///（`matroska,webm`），因为 WebM 就是 Matroska 的子集，用的是同一个
/// demuxer。但 `<video>` **支持 WebM、不支持 MKV**（文档 §5.2）。
///
/// 若把 `webm` 直接放进原生容器列表，所有 MKV 都会被误判为 P1，
/// 用户点开就是播放失败——这是最初实现踩的坑，被测试抓住。
///
/// 唯一可靠的区分方式是看**内容是否符合 WebM 规范**：
/// WebM 只允许 VP8/VP9/AV1 视频与 Vorbis/Opus 音频。
/// 装着 H.264 + AAC 的 `matroska,webm` 一定是 MKV，不是 WebM。
fn container_natively_supported(info: &MediaInfo) -> bool {
    if NATIVE_CONTAINERS.iter().any(|c| info.container_is(c)) {
        return true;
    }
    // matroska 家族：只有内容全部符合 WebM 规范才当作可播
    if info.container_is("webm") || info.container_is("matroska") {
        let video_ok = info
            .video
            .iter()
            .all(|v| contains_ci(WEBM_VIDEO, &v.codec));
        let audio_ok = info
            .audio
            .iter()
            .all(|a| contains_ci(WEBM_AUDIO, &a.codec));
        return video_ok && audio_ok;
    }
    false
}

/// 判定播放路径。
///
/// 判定顺序刻意如此：**先看编码能不能解，再看容器装不装得下**。
/// 因为编码不支持只能转码（P3），而容器不支持还有 remux 的余地（P2）——
/// 反过来判断会把本可 P2 的文件误判为 P3。
#[must_use]
pub fn classify(info: &MediaInfo) -> TierVerdict {
    let container_ok = container_natively_supported(info);

    // ---- 视频编码 ----
    let video_native = match info.primary_video() {
        // 无视频轨（纯音频）时不构成障碍
        None => true,
        Some(v) => contains_ci(NATIVE_VIDEO, &v.codec),
    };

    // ---- 音频编码 ----
    // 只要**存在一条**原生可解的音轨即可——播放器可以选它。
    // 这里是 P3 降 P2/P1 的关键：多音轨文件常有一条兼容轨。
    let native_audio_track = info
        .audio
        .iter()
        .find(|a| contains_ci(NATIVE_AUDIO, &a.codec));
    let audio_native = info.audio.is_empty() || native_audio_track.is_some();

    // 默认音轨是否原生可解。若默认轨不可解但存在可解轨，
    // 就是一个明确的"切换音轨即可降级"的备选。
    let default_audio = info
        .audio
        .iter()
        .find(|a| a.default)
        .or_else(|| info.audio.first());
    let default_audio_native = default_audio
        .map(|a| contains_ci(NATIVE_AUDIO, &a.codec))
        .unwrap_or(true);

    // ---- MP4 兼容性（决定 P2 可行性）----
    let has_mp4_incompatible_audio = info
        .audio
        .iter()
        .any(|a| contains_ci(MP4_INCOMPATIBLE_AUDIO, &a.codec));
    // 存在一条既原生可解、又能装进 MP4 的音轨，remux 才真的可行
    let remuxable_audio = info.audio.is_empty()
        || info
            .audio
            .iter()
            .any(|a| {
                contains_ci(NATIVE_AUDIO, &a.codec)
                    && !contains_ci(MP4_INCOMPATIBLE_AUDIO, &a.codec)
            });

    let has_bitmap_subs = info
        .subtitles
        .iter()
        .any(|s| contains_ci(BITMAP_SUBTITLES, &s.codec));

    let mut alternatives = Vec::new();

    // ---- 定级 ----
    let (tier, reason) = if !video_native {
        // 编码不支持，只能全解码
        let codec = info
            .primary_video()
            .map(|v| v.codec.clone())
            .unwrap_or_else(|| "unknown".to_owned());
        (
            PlaybackTier::P3,
            format!("视频编码 {codec} 不被 WebView 原生支持，需要转码"),
        )
    } else if !audio_native && !info.audio.is_empty() {
        // 视频能解但没有任何可解音轨
        let codecs: Vec<&str> = info.audio.iter().map(|a| a.codec.as_str()).collect();
        (
            PlaybackTier::P3,
            format!(
                "音频编码 {} 不被原生支持，需要转码音轨",
                codecs.join(" / ")
            ),
        )
    } else if container_ok && default_audio_native {
        // 容器与默认音轨都 OK —— 真正的直通
        (
            PlaybackTier::P1,
            "容器与编码均被 WebView 原生支持".to_owned(),
        )
    } else if container_ok {
        // 容器可以，但默认音轨要换
        (
            PlaybackTier::P2,
            "默认音轨不被原生支持，需切换音轨".to_owned(),
        )
    } else if remuxable_audio {
        // 容器不支持但可 remux
        (
            PlaybackTier::P2,
            format!(
                "容器 {} 不被 <video> 支持，但音视频编码可直接转封装",
                info.container
            ),
        )
    } else {
        (
            PlaybackTier::P3,
            format!(
                "容器 {} 不被支持，且音轨无法封入 MP4，需要转码",
                info.container
            ),
        )
    };

    // ---- 备选方案 ----
    // 默认轨不可解但存在可解轨：切换音轨是有意义的操作。
    //
    // 注意判断依据是**收益**而不是"等级是否降低"：容器不支持时，
    // 切轨前后都是 P2，但含义完全不同——切轨前要转码音轨，
    // 切轨后只需纯 remux，耗时差一个数量级。若按 target < tier 过滤，
    // 这个备选会被丢掉，用户就看不到本可以避免转码。
    if !default_audio_native {
        if let Some(a) = native_audio_track {
            let target = if container_ok {
                PlaybackTier::P1
            } else {
                PlaybackTier::P2
            };
            let note = if container_ok {
                format!("选择音轨 {}（{}）即可直通播放", a.index, a.codec)
            } else {
                format!(
                    "选择音轨 {}（{}）可仅转封装、无需转码",
                    a.index, a.codec
                )
            };
            alternatives.push(Alternative {
                tier: target,
                note,
                audio_index: Some(a.index),
            });
        }
    }

    // 视频可 copy 但音轨需转码：只转音轨比全解码快得多，值得单列
    if tier == PlaybackTier::P3 && video_native && !info.audio.is_empty() && !audio_native {
        alternatives.push(Alternative {
            tier: PlaybackTier::P2,
            note: "仅转码音轨（视频流可直接 copy）".to_owned(),
            audio_index: None,
        });
    }

    TierVerdict {
        tier,
        reason,
        alternatives,
        container_ok,
        // 图形字幕会在 remux 中丢失，也算需要额外处理
        needs_transcode: has_mp4_incompatible_audio || has_bitmap_subs || !video_native,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{AudioStream, MediaInfo, SubtitleStream, VideoStream};

    fn video(codec: &str) -> VideoStream {
        VideoStream {
            index: 0,
            codec: codec.to_owned(),
            mime_codec: None,
            width: Some(1920),
            height: Some(1080),
            profile: None,
            level: None,
            pix_fmt: None,
            frame_rate: None,
            frames: None,
            attached_pic: false,
        }
    }

    fn audio(index: u32, codec: &str, default: bool) -> AudioStream {
        AudioStream {
            index,
            codec: codec.to_owned(),
            mime_codec: None,
            channels: Some(2),
            channel_layout: None,
            sample_rate: Some(48000),
            language: None,
            title: None,
            default,
        }
    }

    fn info(container: &str, v: Vec<VideoStream>, a: Vec<AudioStream>) -> MediaInfo {
        MediaInfo {
            container: container.to_owned(),
            container_long: None,
            duration_ms: Some(60_000),
            size: None,
            bit_rate: None,
            video: v,
            audio: a,
            subtitles: vec![],
            tags: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn mp4_h264_aac_is_p1() {
        let i = info(
            "mov,mp4,m4a,3gp,3g2,mj2",
            vec![video("h264")],
            vec![audio(1, "aac", true)],
        );
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P1);
        assert!(v.container_ok);
        assert!(!v.needs_transcode);
    }

    #[test]
    fn mkv_h264_aac_is_p2_remux() {
        // 现代 web-dl：容器不支持但可直接 remux（文档 §5.3）
        let i = info(
            "matroska,webm",
            vec![video("h264")],
            vec![audio(1, "aac", true)],
        );
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P2);
        assert!(!v.container_ok);
        assert!(v.reason.contains("转封装"), "理由应说明可 remux：{}", v.reason);
    }

    #[test]
    fn bluray_remux_dts_is_p3_with_audio_only_transcode_alternative() {
        // 蓝光原盘：DTS 无法封入 MP4，必须转码（文档 §5.3）
        let i = info(
            "matroska,webm",
            vec![video("h264")],
            vec![audio(1, "dts", true)],
        );
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P3);
        assert!(v.needs_transcode);
        // 视频可 copy，只转音轨——这个备选必须给出，否则用户会以为要全解码
        assert!(
            v.alternatives
                .iter()
                .any(|a| a.tier == PlaybackTier::P2 && a.note.contains("copy")),
            "缺少「仅转音轨」备选：{:?}",
            v.alternatives
        );
    }

    #[test]
    fn multi_audio_track_offers_switch_to_downgrade() {
        // 动画压制常见：默认 DTS，另有 AAC 轨。切轨即可降级——
        // 这正是文档 §5.4 举的例子。
        let i = info(
            "matroska,webm",
            vec![video("h264")],
            vec![audio(1, "dts", true), audio(2, "aac", false)],
        );
        let v = classify(&i);
        // 存在可解音轨，所以不是 P3
        assert_eq!(v.tier, PlaybackTier::P2);
        let alt = v
            .alternatives
            .iter()
            .find(|a| a.audio_index == Some(2))
            .expect("应给出切到 aac 轨的备选");
        assert!(alt.note.contains("aac"));
        // 切轨前后都是 P2，但收益是"免去音轨转码"。
        // 说明文字必须体现这一点，否则用户不知道切了有什么用。
        assert!(
            alt.note.contains("转封装") || alt.note.contains("无需转码"),
            "备选说明应体现实际收益：{}",
            alt.note
        );
    }

    #[test]
    fn xvid_is_p3_regardless_of_container() {
        // XviD 两端都不支持（文档 §5.2 标为 P3）
        let i = info("avi", vec![video("mpeg4")], vec![audio(1, "mp3", true)]);
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P3);
        assert!(v.reason.contains("mpeg4"), "理由应点明编码：{}", v.reason);
    }

    #[test]
    fn audio_only_mp3_is_p1() {
        let i = info("mp3", vec![], vec![audio(0, "mp3", true)]);
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P1);
    }

    #[test]
    fn hevc_is_not_assumed_native() {
        // HEVC 依赖硬件与版本，保守基线不能当成原生支持，
        // 否则在不支持的机器上会直接播放失败。
        let i = info(
            "mov,mp4,m4a,3gp,3g2,mj2",
            vec![video("hevc")],
            vec![audio(1, "aac", true)],
        );
        let v = classify(&i);
        assert_ne!(v.tier, PlaybackTier::P1, "HEVC 不应被当作原生支持");
    }

    #[test]
    fn bitmap_subtitles_flag_needs_transcode() {
        let mut i = info(
            "matroska,webm",
            vec![video("h264")],
            vec![audio(1, "aac", true)],
        );
        i.subtitles.push(SubtitleStream {
            index: 2,
            codec: "hdmv_pgs_subtitle".to_owned(),
            language: None,
            title: None,
            default: false,
            forced: false,
        });
        let v = classify(&i);
        // 视频音频都能 remux，所以还是 P2
        assert_eq!(v.tier, PlaybackTier::P2);
        // 但图形字幕会丢，必须标记出来让 UI 提示
        assert!(v.needs_transcode, "PGS 字幕会在 remux 中丢失，应标记");
    }

    #[test]
    fn vorbis_in_mkv_cannot_remux_to_mp4() {
        // Vorbis 原生可解（WebM 里），但装不进 MP4。
        // 这个组合最容易判错：看编码以为能 P1，看容器以为能 remux，
        // 实际两者都不成立。
        let i = info("matroska,webm", vec![video("h264")], vec![audio(1, "vorbis", true)]);
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P3, "Vorbis 无法封入 MP4");
        assert!(v.needs_transcode);
    }

    #[test]
    fn webm_vp9_opus_is_p1() {
        let i = info("matroska,webm", vec![video("vp9")], vec![audio(1, "opus", true)]);
        let v = classify(&i);
        // 内容符合 WebM 规范 → 真的是 WebM，可直通
        assert_eq!(v.tier, PlaybackTier::P1);
        assert!(v.container_ok);
    }

    #[test]
    fn mkv_and_webm_share_format_name_but_differ_in_tier() {
        // 关键回归测试：ffprobe 对 MKV 与 WebM 返回**完全相同**的
        // format_name "matroska,webm"。若只比容器名，H.264+AAC 的 MKV
        // 会被误判为 P1，用户点开直接播放失败。
        // 最初实现正是这样写的，被这组用例抓出。
        let webm = info(
            "matroska,webm",
            vec![video("vp9")],
            vec![audio(1, "opus", true)],
        );
        let mkv = info(
            "matroska,webm",
            vec![video("h264")],
            vec![audio(1, "aac", true)],
        );
        assert_eq!(webm.container, mkv.container, "前提：两者容器名相同");

        assert_eq!(classify(&webm).tier, PlaybackTier::P1, "真 WebM 应可直通");
        assert_eq!(
            classify(&mkv).tier,
            PlaybackTier::P2,
            "装 H.264+AAC 的 matroska 是 MKV，<video> 不支持"
        );
    }

    #[test]
    fn matroska_with_vp9_but_aac_is_not_webm() {
        // VP9 视频符合 WebM，但 AAC 音频不符合 —— 整体不是合法 WebM
        let i = info(
            "matroska,webm",
            vec![video("vp9")],
            vec![audio(1, "aac", true)],
        );
        let v = classify(&i);
        assert!(!v.container_ok, "AAC 不在 WebM 规范内");
        assert_eq!(v.tier, PlaybackTier::P2);
    }

    #[test]
    fn tier_ordering_allows_comparison() {
        // 备选方案的筛选依赖 tier 的序关系，必须成立
        assert!(PlaybackTier::P1 < PlaybackTier::P2);
        assert!(PlaybackTier::P2 < PlaybackTier::P3);
    }

    #[test]
    fn no_audio_video_only_is_p1_in_mp4() {
        let i = info("mov,mp4,m4a,3gp,3g2,mj2", vec![video("h264")], vec![]);
        let v = classify(&i);
        assert_eq!(v.tier, PlaybackTier::P1, "无音轨不应构成障碍");
    }
}
