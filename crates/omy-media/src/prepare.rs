//! 加密前的媒体准备：一次探测，产出全部媒体 TLV。
//!
//! # 为什么要有这个模块
//!
//! 三个媒体 TLV（`MEDIA_META` / `MOOV_CACHE` / `THUMBNAIL`）都依赖同一次
//! 探测结果。若让调用方分别调 `probe` → `classify` → `to_faststart` →
//! `thumbnail`，会有三个问题：
//!
//! 1. **重复探测**：每次都要起一个 ffprobe 子进程，几十毫秒起；
//! 2. **易漏步骤**：容易忘了 MP4 要存 moov、或忘了非 MP4 不该存；
//! 3. **失败处理不一致**：缩略图生成失败**不应**让整个加密失败，
//!    但这个判断散落在调用方就会写错。
//!
//! 所以这里给一个入口：给字节，拿回可以直接塞进 `EncryptOptions` 的东西。
//!
//! # 本模块不依赖 omy-core
//!
//! 返回的是裸 `Vec<u8>`，而不是 `EncryptOptions`。因为 `omy-media` 是
//! LGPL 而 `omy-core` 是 MIT/Apache，**core 不能依赖 media**，
//! media 也不该反向依赖 core 来避免循环。装配由上层（cli / gui）完成。

use crate::error::MediaError;
use crate::meta::MediaMeta;
use crate::mp4;
use crate::probe::{self, MediaInfo, Source};
use crate::thumbnail::{self, DEFAULT_MAX_EDGE, ThumbFormat};
use crate::tier::{self, TierVerdict};

/// 缩略图的取图策略。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThumbSource {
    /// 不生成缩略图。
    None,
    /// 图片文件：直接缩放原图。
    Image,
    /// 视频：抽取指定秒数处的帧。
    ///
    /// 用户可自选特定帧（需求确认项 B.2）。
    VideoAt(f64),
    /// 视频：自动选取。取时长的 10% 处，避开片头黑帧。
    VideoAuto,
}

/// 准备选项。
#[derive(Debug, Clone)]
pub struct PrepareOptions {
    /// 是否写入媒体元信息。
    pub media_meta: bool,
    /// 是否为 MP4 缓存 moov box。
    ///
    /// 非 MP4 容器会自动跳过，置 `true` 也不会出错。
    pub moov_cache: bool,
    /// 缩略图策略。
    pub thumbnail: ThumbSource,
    /// 缩略图最长边（像素）。
    pub thumb_max_edge: u32,
    /// 缩略图编码格式。
    pub thumb_format: ThumbFormat,
}

impl Default for PrepareOptions {
    /// 默认全开，缩略图自动选帧。
    ///
    /// 之所以默认开启：这些都是**纯优化**，不改动载荷、不影响可还原性，
    /// 而收益（列表页秒开、起播免 seek）很直接。
    fn default() -> Self {
        Self {
            media_meta: true,
            moov_cache: true,
            thumbnail: ThumbSource::VideoAuto,
            thumb_max_edge: DEFAULT_MAX_EDGE,
            thumb_format: ThumbFormat::WebP,
        }
    }
}

impl PrepareOptions {
    /// 全部关闭。用于用户明确不想存任何附加信息时。
    #[must_use]
    pub const fn none() -> Self {
        Self {
            media_meta: false,
            moov_cache: false,
            thumbnail: ThumbSource::None,
            thumb_max_edge: DEFAULT_MAX_EDGE,
            thumb_format: ThumbFormat::WebP,
        }
    }

    /// 针对图片文件的预设：存元信息与缩略图，无 moov。
    #[must_use]
    pub const fn for_image() -> Self {
        Self {
            media_meta: true,
            moov_cache: false,
            thumbnail: ThumbSource::Image,
            thumb_max_edge: DEFAULT_MAX_EDGE,
            thumb_format: ThumbFormat::WebP,
        }
    }
}

/// 准备结果。
///
/// 每一项都是 `Option`：**缺失是正常的**，不是错误。
/// 非 MP4 没有 moov，纯音频没有帧可抽，探测失败时全都没有。
#[derive(Debug, Clone, Default)]
pub struct Prepared {
    /// `TLV_MEDIA_META` 的内容（JSON 字节）。
    pub media_meta: Option<Vec<u8>>,
    /// `TLV_MOOV_CACHE` 的内容（原始 moov box 字节）。
    pub moov_cache: Option<Vec<u8>>,
    /// `TLV_THUMBNAIL` 的内容（WebP/JPEG 字节）。
    pub thumbnail: Option<Vec<u8>>,
    /// 探测到的媒体信息，供调用方展示或进一步判断。
    pub info: Option<MediaInfo>,
    /// 分级结论。
    pub verdict: Option<TierVerdict>,
    /// 处理过程中的**非致命**问题。
    ///
    /// 例如「缩略图生成失败」——加密仍应继续，但要让用户知道。
    /// 不用 `Result` 表达是因为这些不该中断流程。
    pub warnings: Vec<String>,
}

impl Prepared {
    /// 是否什么都没产出。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.media_meta.is_none() && self.moov_cache.is_none() && self.thumbnail.is_none()
    }
}

/// 为一段媒体明文准备全部媒体 TLV。
///
/// # 失败策略
///
/// **本函数不返回 `Err`。** 媒体附加信息全是优化项，任何一步失败都只应
/// 降级（少存一项）并记入 `warnings`，绝不能让用户的文件加密不了。
///
/// 想区分「不是媒体文件」与「探测出错」的调用方，看 `info` 是否为 `None`
/// 并读 `warnings`。
///
/// # 性能
///
/// 只跑**一次** ffprobe。moov 定位是纯 Rust 解析，不额外起进程。
/// 缩略图会再起一次 ffmpeg（仅视频），这是不可避免的。
#[must_use]
pub fn prepare(data: &[u8], opts: &PrepareOptions) -> Prepared {
    let mut out = Prepared::default();

    // 全关时直接返回，不做无谓探测
    if !opts.media_meta && !opts.moov_cache && opts.thumbnail == ThumbSource::None {
        return out;
    }

    // 图片走纯 Rust 路径，不需要 ffprobe。
    // 先试图片：图片文件占比高，且这条路不起子进程，快得多。
    if opts.thumbnail == ThumbSource::Image {
        match thumbnail::from_image_bytes(data, opts.thumb_max_edge, opts.thumb_format) {
            Ok(t) => out.thumbnail = Some(t.bytes),
            Err(e) => out.warnings.push(format!("图片缩略图生成失败：{e}")),
        }
    }

    // 一次探测，后续全部复用
    let info = match probe::probe(Source::Bytes(data)) {
        Ok(i) => i,
        Err(e) => {
            // 非媒体文件是常态（文本、压缩包……），不值得当成警告刷屏；
            // 真正的探测故障才记录
            if !matches!(e, MediaError::NotMedia { .. }) {
                out.warnings.push(format!("媒体探测失败：{e}"));
            }
            return out;
        }
    };

    let verdict = tier::classify(&info);

    if opts.media_meta {
        let meta = MediaMeta::from_probe_with_verdict(&info, &verdict);
        match meta.to_json_bytes() {
            Ok(b) => out.media_meta = Some(b),
            Err(e) => out.warnings.push(format!("媒体元信息序列化失败：{e}")),
        }
    }

    if opts.moov_cache {
        match extract_moov(data) {
            Ok(Some(m)) => out.moov_cache = Some(m),
            // 非 MP4 没有 moov，属正常情况
            Ok(None) => {}
            Err(e) => out.warnings.push(format!("moov 提取失败：{e}")),
        }
    }

    // 视频缩略图。图片路径已在前面处理过
    match opts.thumbnail {
        ThumbSource::VideoAt(sec) => {
            gen_video_thumb(data, sec, opts, &info, &mut out);
        }
        ThumbSource::VideoAuto => {
            // 取 10% 处：片头常有黑帧或台标，取首帧往往是纯黑。
            // 无时长信息时退回 1 秒——比 0 秒稳妥。
            let sec = info
                .duration_ms
                .map_or(1.0, |ms| (ms as f64 / 1000.0 * 0.1).clamp(0.0, 600.0));
            gen_video_thumb(data, sec, opts, &info, &mut out);
        }
        ThumbSource::None | ThumbSource::Image => {}
    }

    out.info = Some(info);
    out.verdict = Some(verdict);
    out
}

/// 抽视频帧，失败只记警告。
///
/// `info` 必须显式传入而不是从 `out.info` 读：本函数在
/// `out.info` 被赋值**之前**调用，从 `out` 读会永远拿到 `None`，
/// 导致纯音频文件也去起一次 ffmpeg 白跑。
fn gen_video_thumb(
    data: &[u8],
    sec: f64,
    opts: &PrepareOptions,
    info: &MediaInfo,
    out: &mut Prepared,
) {
    // 纯音频没有帧可抽，直接跳过而不是报错。
    // 注意带封面的 MP3 也会走到这里——它的封面是 attached_pic，
    // probe 已把它排除在视频轨之外，故 is_audio_only() 为真。
    if info.is_audio_only() {
        return;
    }
    // 完全没有视频轨也无从抽帧
    if !info.has_video() {
        return;
    }
    match thumbnail::from_video_bytes(data.to_vec(), sec, opts.thumb_max_edge) {
        Ok(t) => out.thumbnail = Some(t.bytes),
        Err(e) => out.warnings.push(format!("视频缩略图生成失败：{e}")),
    }
}

/// 提取 MP4 的 moov box 原始字节。
///
/// # 为什么要先 faststart 重排
///
/// 缓存 moov 的目的是让播放器**不必 seek 到尾部**。但 moov 里的
/// `stco`/`co64` 记录的是绝对偏移——如果原文件 moov 在尾部，
/// 那些偏移是相对**原始布局**的。播放时若把这份 moov 与原始载荷配合使用，
/// 偏移仍然正确（因为载荷没动）。
///
/// 所以这里**直接取原始 moov，不做重排**。重排只用于喂 FFmpeg 探测，
/// 那是另一回事。若存了重排后的 moov，偏移就与实际载荷不符了。
///
/// 这个区别很容易搞错，故显式写明。
fn extract_moov(data: &[u8]) -> crate::Result<Option<Vec<u8>>> {
    if !mp4::looks_like_mp4(data) {
        return Ok(None);
    }
    let Some(r) = mp4::find_moov(data)? else {
        return Ok(None);
    };
    let start = usize::try_from(r.offset).map_err(|_| MediaError::InvalidInput {
        reason: "moov 偏移超出寻址范围".to_owned(),
    })?;
    let len = usize::try_from(r.size).map_err(|_| MediaError::InvalidInput {
        reason: "moov 长度超出寻址范围".to_owned(),
    })?;
    let end = start
        .checked_add(len)
        .ok_or_else(|| MediaError::InvalidInput {
            reason: "moov 区间计算溢出".to_owned(),
        })?;
    Ok(data.get(start..end).map(<[u8]>::to_vec))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_disabled_returns_empty_without_probing() {
        // 全关时不该做任何工作。传入明显非法的数据，
        // 若它去探测就会产生警告——以此证明确实跳过了。
        let out = prepare(&[0xFF; 100], &PrepareOptions::none());
        assert!(out.is_empty());
        assert!(out.warnings.is_empty(), "全关时不该有警告：{:?}", out.warnings);
        assert!(out.info.is_none());
    }

    #[test]
    fn non_media_does_not_warn() {
        // 非媒体文件是常态（加密的多数是文档、压缩包），
        // 不该为此刷警告
        let opts = PrepareOptions {
            thumbnail: ThumbSource::None,
            ..PrepareOptions::default()
        };
        let out = prepare(b"just plain text, definitely not media", &opts);
        assert!(out.media_meta.is_none());
        // 关键：不该有警告
        assert!(
            out.warnings.is_empty(),
            "非媒体文件不该产生警告：{:?}",
            out.warnings
        );
    }

    #[test]
    fn extract_moov_returns_none_for_non_mp4() {
        // PNG 头
        let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(extract_moov(&png).expect("不该报错"), None);
        // 空数据
        assert_eq!(extract_moov(&[]).expect("不该报错"), None);
    }

    #[test]
    fn extract_moov_finds_exact_bytes() {
        // 构造 ftyp + mdat + moov，验证取出的正是 moov 那一段
        let mut data = Vec::new();
        // ftyp
        data.extend_from_slice(&32u32.to_be_bytes());
        data.extend_from_slice(b"ftyp");
        data.extend_from_slice(&[0u8; 24]);
        // mdat
        data.extend_from_slice(&108u32.to_be_bytes());
        data.extend_from_slice(b"mdat");
        data.extend_from_slice(&[0xABu8; 100]);
        // moov
        let moov_start = data.len();
        data.extend_from_slice(&48u32.to_be_bytes());
        data.extend_from_slice(b"moov");
        data.extend_from_slice(&[0xCDu8; 40]);

        let got = extract_moov(&data).expect("应成功").expect("应找到 moov");
        assert_eq!(got.len(), 48);
        // 内容必须与原文件对应区间逐字节相同
        assert_eq!(got, &data[moov_start..moov_start + 48]);
        // 头 8 字节是 size + 'moov'
        assert_eq!(&got[4..8], b"moov");
    }

    #[test]
    fn image_thumbnail_works_without_ffprobe() {
        // 用 image crate 生成一张真实 PNG，验证图片路径不依赖外部工具
        let img = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 5) as u8, 128])
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("编码 PNG");

        let out = prepare(&png, &PrepareOptions::for_image());
        let thumb = out.thumbnail.expect("应生成缩略图");
        assert!(!thumb.is_empty());
        // 产物必须是可解码的图片，不能只看长度
        assert!(
            image::load_from_memory(&thumb).is_ok(),
            "缩略图必须能解码"
        );
        assert!(out.moov_cache.is_none(), "图片不该有 moov");
    }

    #[test]
    fn video_auto_picks_ten_percent() {
        // 只验证选帧算法，不起 ffmpeg。
        // 1000 秒的视频应取 100 秒处。
        let ms = 1_000_000u64;
        let sec = (ms as f64 / 1000.0 * 0.1).clamp(0.0, 600.0);
        assert!((sec - 100.0).abs() < 0.001);

        // 超长视频要被 clamp 住，否则会 seek 到很远的位置白等
        let long = 20_000_000u64;
        let sec2 = (long as f64 / 1000.0 * 0.1).clamp(0.0, 600.0);
        assert!((sec2 - 600.0).abs() < 0.001, "超长视频应被限制在 600 秒");
    }

    #[test]
    fn default_options_enable_optimizations() {
        // 默认应开启这些纯优化项——它们不影响可还原性
        let d = PrepareOptions::default();
        assert!(d.media_meta);
        assert!(d.moov_cache);
        assert_eq!(d.thumbnail, ThumbSource::VideoAuto);
    }
}
