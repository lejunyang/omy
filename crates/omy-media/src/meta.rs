//! `TLV_MEDIA_META` 的序列化结构（规范 §4.4 的 `0x0004`）。
//!
//! # 为什么不直接序列化 `MediaInfo`
//!
//! [`crate::probe::MediaInfo`] 是 **ffprobe 输出的忠实映射**，字段多且
//! 带一堆只在探测阶段有用的东西（`pix_fmt`、`frame_rate`、原始 tags）。
//! 把它整个写进 header 有三个问题：
//!
//! 1. **体积**：header 要常驻内存做列表页渲染，10000 个文件就是 10000 份；
//! 2. **稳定性**：ffprobe 换版本可能增删字段，直接映射会让格式随之漂移；
//! 3. **泄露**：`tags` 里可能有拍摄设备、软件、甚至 GPS——这是加密应用，
//!    不该把这些原样搬进（虽已加密的）元信息里。
//!
//! 所以这里定义一个**独立的、稳定的**结构，只保留文档 §5.4 列出的字段，
//! 由 [`MediaMeta::from_probe`] 做一次有损转换。
//!
//! # 兼容性策略
//!
//! 反序列化时所有字段都容许缺失（`#[serde(default)]`），因为：
//!
//! - 旧版本写的 meta 不会有新加的字段；
//! - 这是**非 CRITICAL** TLV，读不动时应当降级为「没有元信息」，
//!   而不是让整个文件打不开。

use crate::probe::MediaInfo;
use crate::tier::{self, PlaybackTier, TierVerdict};
use serde::{Deserialize, Serialize};

/// 当前 meta 结构的版本号。
///
/// 与文件格式版本无关，仅标识这段 JSON 的 schema。
/// 递增时机：**删除或改变现有字段语义**。纯新增字段不递增，
/// 因为旧读者靠 `serde(default)` 就能容忍。
pub const META_VERSION: u32 = 1;

/// 视频轨的精简描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoMeta {
    /// 编码名，如 `h264`。
    pub codec: String,
    /// 宽度（像素）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    /// 高度（像素）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// profile，如 `high`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// level，如 41 表示 4.1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<u32>,
    /// RFC 6381 codec 串，供 `MediaSource.isTypeSupported()` 直接用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_codec: Option<String>,
}

/// 音频轨的精简描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioMeta {
    /// 流序号。**必须保留**：备选方案要靠它指定切换到哪条轨。
    pub index: u32,
    /// 编码名，如 `aac`。
    pub codec: String,
    /// 声道数。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channels: Option<u32>,
    /// 语言标签，如 `eng`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// 轨道标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// RFC 6381 codec 串。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_codec: Option<String>,
}

/// 字幕轨的精简描述。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleMeta {
    /// 流序号。
    pub index: u32,
    /// 编码名，如 `ass`、`subrip`。
    pub codec: String,
    /// 语言标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// 轨道标题。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// 是否为强制字幕。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub forced: bool,
}

/// 写入 `TLV_MEDIA_META` 的完整结构。
///
/// 字段布局对应文档 §5.4 的示例 JSON。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMeta {
    /// schema 版本。
    #[serde(default = "default_version")]
    pub version: u32,
    /// 容器名。可能是逗号分隔的多值，如 `mov,mp4,m4a,3gp,3g2,mj2`。
    pub container: String,
    /// 时长（毫秒）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// 总码率（bps）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u64>,
    /// 主视频轨。纯音频文件为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoMeta>,
    /// 全部音频轨。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audio: Vec<AudioMeta>,
    /// 全部字幕轨。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtitles: Vec<SubtitleMeta>,
    /// 播放分级结论。
    pub playback_tier: TierMeta,
}

const fn default_version() -> u32 {
    META_VERSION
}

/// 分级结论的序列化形式。
///
/// 不直接复用 [`TierVerdict`]：那是运行时结构，字段可能随实现调整；
/// 这里是**格式的一部分**，必须独立且稳定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TierMeta {
    /// 默认等级，`"P1"` / `"P2"` / `"P3"`。
    pub default: String,
    /// 判定理由，直接展示给用户。
    pub reason: String,
    /// 备选方案。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<AlternativeMeta>,
}

/// 备选方案的序列化形式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlternativeMeta {
    /// 可达到的等级。
    pub tier: String,
    /// 操作说明。
    pub note: String,
    /// 需切换到的音轨序号。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_index: Option<u32>,
}

impl MediaMeta {
    /// 从探测结果构造。
    ///
    /// 只取主视频轨（`video[0]`）——多视频轨极罕见，且 `<video>` 也只播一条。
    /// 音频与字幕保留全部，因为切轨是 P3→P2 降级的主要手段。
    #[must_use]
    pub fn from_probe(info: &MediaInfo) -> Self {
        let verdict = tier::classify(info);
        Self::from_probe_with_verdict(info, &verdict)
    }

    /// 从探测结果与已算好的分级构造。
    ///
    /// 调用方已经调过 [`tier::classify`] 时用这个，避免重复计算。
    #[must_use]
    pub fn from_probe_with_verdict(info: &MediaInfo, verdict: &TierVerdict) -> Self {
        let video = info.video.first().map(|v| VideoMeta {
            codec: v.codec.clone(),
            width: v.width,
            height: v.height,
            profile: v.profile.clone(),
            level: v.level,
            mime_codec: v.mime_codec.clone(),
        });

        let audio = info
            .audio
            .iter()
            .map(|a| AudioMeta {
                index: a.index,
                codec: a.codec.clone(),
                channels: a.channels,
                lang: a.language.clone(),
                title: a.title.clone(),
                mime_codec: a.mime_codec.clone(),
            })
            .collect();

        let subtitles = info
            .subtitles
            .iter()
            .map(|s| SubtitleMeta {
                index: s.index,
                codec: s.codec.clone(),
                lang: s.language.clone(),
                title: s.title.clone(),
                forced: s.forced,
            })
            .collect();

        Self {
            version: META_VERSION,
            container: info.container.clone(),
            duration_ms: info.duration_ms,
            bit_rate: info.bit_rate,
            video,
            audio,
            subtitles,
            playback_tier: TierMeta {
                default: verdict.tier.as_str().to_owned(),
                reason: verdict.reason.clone(),
                alternatives: verdict
                    .alternatives
                    .iter()
                    .map(|a| AlternativeMeta {
                        tier: a.tier.as_str().to_owned(),
                        note: a.note.clone(),
                        audio_index: a.audio_index,
                    })
                    .collect(),
            },
        }
    }

    /// 序列化为写入 TLV 的字节。
    ///
    /// 用紧凑格式（无缩进）：这段数据要常驻内存，且没人直接阅读它。
    ///
    /// # Errors
    ///
    /// 序列化失败时返回错误。实践中不会发生，因为所有字段都是普通类型。
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// 从 TLV 字节反序列化。
    ///
    /// # Errors
    ///
    /// JSON 非法或缺少必需字段（`container`、`playback_tier`）时返回错误。
    /// 调用方应把错误当作「无可用元信息」而降级，不要让文件打不开——
    /// 这是非 CRITICAL TLV。
    pub fn from_json_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }

    /// 解析出的默认播放等级。
    ///
    /// 无法识别时返回 `None`，调用方应重新探测而不是猜一个等级——
    /// 猜错会让 UI 显示错误的播放按钮。
    #[must_use]
    pub fn tier(&self) -> Option<PlaybackTier> {
        match self.playback_tier.default.as_str() {
            "P1" => Some(PlaybackTier::P1),
            "P2" => Some(PlaybackTier::P2),
            "P3" => Some(PlaybackTier::P3),
            _ => None,
        }
    }

    /// 是否为纯音频。
    #[must_use]
    pub fn is_audio_only(&self) -> bool {
        self.video.is_none() && !self.audio.is_empty()
    }

    /// 视频分辨率（宽, 高），两者都存在时才返回。
    #[must_use]
    pub fn resolution(&self) -> Option<(u32, u32)> {
        let v = self.video.as_ref()?;
        Some((v.width?, v.height?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::parse_probe_json;

    /// 构造一个 ffprobe 风格的 JSON。
    fn probe_json(streams: &str, format: &str) -> String {
        format!(r#"{{"streams":[{streams}],"format":{{{format}}}}}"#)
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let json = probe_json(
            r#"{"index":0,"codec_type":"video","codec_name":"h264","width":1920,
                "height":1080,"profile":"High","level":41},
               {"index":1,"codec_type":"audio","codec_name":"aac","channels":2,
                "tags":{"language":"eng","title":"English"}},
               {"index":2,"codec_type":"subtitle","codec_name":"ass",
                "disposition":{"forced":1},"tags":{"language":"chs"}}"#,
            r#""format_name":"matroska,webm","duration":"1423.0","bit_rate":"4500000""#,
        );
        let info = parse_probe_json(&json).expect("解析探测结果");
        let meta = MediaMeta::from_probe(&info);

        let bytes = meta.to_json_bytes().expect("序列化");
        let back = MediaMeta::from_json_bytes(&bytes).expect("反序列化");
        assert_eq!(meta, back, "往返必须完全一致");

        // 逐项核对关键字段，而不是只看往返相等——
        // 若 from_probe 漏映射某字段，往返测试照样会通过
        assert_eq!(back.container, "matroska,webm");
        assert_eq!(back.duration_ms, Some(1_423_000));
        assert_eq!(back.bit_rate, Some(4_500_000));

        let v = back.video.as_ref().expect("应有视频轨");
        assert_eq!(v.codec, "h264");
        assert_eq!(v.width, Some(1920));
        assert_eq!(v.height, Some(1080));
        assert_eq!(v.level, Some(41));
        assert_eq!(back.resolution(), Some((1920, 1080)));

        assert_eq!(back.audio.len(), 1);
        assert_eq!(back.audio[0].index, 1, "音轨序号必须保留，切轨要用");
        assert_eq!(back.audio[0].lang.as_deref(), Some("eng"));
        assert_eq!(back.audio[0].channels, Some(2));

        assert_eq!(back.subtitles.len(), 1);
        assert_eq!(back.subtitles[0].codec, "ass");
        assert!(back.subtitles[0].forced, "forced 位必须保留");
    }

    #[test]
    fn tier_is_recoverable_from_json() {
        // MKV + H.264 + AAC → P2
        let json = probe_json(
            r#"{"index":0,"codec_type":"video","codec_name":"h264"},
               {"index":1,"codec_type":"audio","codec_name":"aac"}"#,
            r#""format_name":"matroska,webm""#,
        );
        let info = parse_probe_json(&json).expect("解析");
        let meta = MediaMeta::from_probe(&info);
        assert_eq!(meta.tier(), Some(PlaybackTier::P2));

        let back = MediaMeta::from_json_bytes(&meta.to_json_bytes().expect("ser"))
            .expect("de");
        assert_eq!(back.tier(), Some(PlaybackTier::P2), "等级必须能从 JSON 还原");
        assert!(!back.playback_tier.reason.is_empty(), "理由不能为空");
    }

    #[test]
    fn unknown_tier_string_returns_none() {
        // 未来版本可能引入 P4；旧读者必须返回 None 而不是猜一个
        let mut meta = minimal_meta();
        meta.playback_tier.default = "P4".into();
        assert_eq!(meta.tier(), None, "无法识别的等级必须返回 None");
    }

    fn minimal_meta() -> MediaMeta {
        MediaMeta {
            version: META_VERSION,
            container: "mp4".into(),
            duration_ms: None,
            bit_rate: None,
            video: None,
            audio: Vec::new(),
            subtitles: Vec::new(),
            playback_tier: TierMeta {
                default: "P1".into(),
                reason: "test".into(),
                alternatives: Vec::new(),
            },
        }
    }

    #[test]
    fn missing_optional_fields_deserialize() {
        // 只有必需字段的最小 JSON：必须能读，因为旧版本写的就是这样
        let json = br#"{"container":"mp4","playback_tier":{"default":"P1","reason":"ok"}}"#;
        let meta = MediaMeta::from_json_bytes(json).expect("最小 JSON 应能解析");
        assert_eq!(meta.container, "mp4");
        assert_eq!(meta.version, META_VERSION, "缺失版本号应取默认值");
        assert!(meta.audio.is_empty());
        assert_eq!(meta.tier(), Some(PlaybackTier::P1));
    }

    #[test]
    fn missing_required_field_errors() {
        // 缺 container：必须报错而不是给个空串，否则 UI 会显示"容器: "
        let json = br#"{"playback_tier":{"default":"P1","reason":"ok"}}"#;
        assert!(MediaMeta::from_json_bytes(json).is_err());
        // 缺 playback_tier
        let json2 = br#"{"container":"mp4"}"#;
        assert!(MediaMeta::from_json_bytes(json2).is_err());
    }

    #[test]
    fn empty_collections_are_omitted() {
        // 空集合不该出现在 JSON 里——10000 个文件each省几十字节是有意义的
        let meta = minimal_meta();
        let s = String::from_utf8(meta.to_json_bytes().expect("ser")).expect("utf8");
        assert!(!s.contains("audio"), "空音轨列表不该序列化：{s}");
        assert!(!s.contains("subtitles"), "空字幕列表不该序列化：{s}");
        assert!(!s.contains("video"), "无视频轨不该序列化：{s}");
        assert!(!s.contains("duration_ms"), "缺失时长不该序列化：{s}");
        assert!(!s.contains("alternatives"), "空备选不该序列化：{s}");
    }

    #[test]
    fn probe_tags_are_not_leaked() {
        // ffprobe 的 format tags 常含拍摄设备、软件版本、甚至 GPS。
        // 这是加密应用，不该把它们搬进元信息——即使已加密。
        let json = probe_json(
            r#"{"index":0,"codec_type":"video","codec_name":"h264"}"#,
            r#""format_name":"mov,mp4","tags":{"encoder":"Lavf60.3.100",
               "location":"+39.9042+116.4074/","com.apple.quicktime.model":"iPhone 15"}"#,
        );
        let info = parse_probe_json(&json).expect("解析");
        // 探测结果里确实有这些 tag
        assert!(!info.tags.is_empty(), "前提：探测结果应含 tags");

        let s = String::from_utf8(
            MediaMeta::from_probe(&info).to_json_bytes().expect("ser"),
        )
        .expect("utf8");
        assert!(!s.contains("iPhone"), "设备型号不得进入 meta：{s}");
        assert!(!s.contains("39.9042"), "GPS 不得进入 meta：{s}");
        assert!(!s.contains("Lavf"), "编码器版本不得进入 meta：{s}");
    }

    #[test]
    fn audio_only_is_detected() {
        let json = probe_json(
            r#"{"index":0,"codec_type":"audio","codec_name":"mp3","channels":2}"#,
            r#""format_name":"mp3","duration":"180.5""#,
        );
        let info = parse_probe_json(&json).expect("解析");
        let meta = MediaMeta::from_probe(&info);
        assert!(meta.is_audio_only());
        assert_eq!(meta.duration_ms, Some(180_500));
        assert_eq!(meta.resolution(), None);
    }

    #[test]
    fn alternatives_survive_roundtrip() {
        // DTS + AAC 双音轨：应给出"切到 AAC 可降到 P2"的备选
        let json = probe_json(
            r#"{"index":0,"codec_type":"video","codec_name":"h264"},
               {"index":1,"codec_type":"audio","codec_name":"dts","channels":6},
               {"index":2,"codec_type":"audio","codec_name":"aac","channels":2}"#,
            r#""format_name":"matroska,webm""#,
        );
        let info = parse_probe_json(&json).expect("解析");
        let meta = MediaMeta::from_probe(&info);
        let back = MediaMeta::from_json_bytes(&meta.to_json_bytes().expect("ser"))
            .expect("de");

        assert_eq!(back.audio.len(), 2, "两条音轨都要保留");
        if let Some(alt) = back.playback_tier.alternatives.first() {
            // 备选必须带音轨序号，否则 UI 无法执行切轨
            assert!(alt.audio_index.is_some(), "备选方案应指明音轨序号");
            assert!(!alt.note.is_empty());
        }
    }

    #[test]
    fn json_shape_matches_documented_example() {
        // 文档 §5.4 给的示例用 playback_tier.default / reason / alternatives，
        // 字段名必须一致，否则 GUI 按文档写的解析代码会读不到
        let meta = minimal_meta();
        let v: serde_json::Value =
            serde_json::from_slice(&meta.to_json_bytes().expect("ser")).expect("解析");
        assert!(v.get("container").is_some());
        assert!(v.get("playback_tier").is_some());
        let t = v.get("playback_tier").expect("tier");
        assert!(t.get("default").is_some(), "字段名须为 default");
        assert!(t.get("reason").is_some(), "字段名须为 reason");
    }
}
