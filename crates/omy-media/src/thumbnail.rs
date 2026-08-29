//! 缩略图生成。
//!
//! 依据文档 §10.4（决策 D-10）：缩略图加密后存入 `TLV_THUMBNAIL`，
//! 建议 ≤ 32 KB。收益是列表页只读 header 就能显示网格视图。
//!
//! # 图片与视频走不同路径
//!
//! - **图片**：用 `image` crate 在进程内缩放。图片解码库虽也有风险，
//!   但 `image` 是纯 Rust，内存安全性远好于 C 实现，且已在
//!   Cargo.toml 里**关掉了默认特性**只开必要格式，缩小攻击面。
//! - **视频**：必须走 FFmpeg 子进程（隔离要求），抽指定时间点的帧。
//!
//! # 视频自选帧不在这里实现
//!
//! 文档 §10.4 指出更好的做法是让用户在应用内播放到想要的一帧，
//! 前端 `canvas.drawImage` 取图后交给 Rust 加密——完全复用已有播放链路，
//! 零额外依赖。本模块只负责**自动生成**的那条路径。

use crate::error::{MediaError, Result};
use crate::ffprobe::{self, Tool};

/// 缩略图的目标最大边长（像素）。
///
/// 320 是权衡：网格视图里足够清晰，编码后通常在 10~20 KB，
/// 留出余量满足 ≤ 32 KB 的建议上限。
pub const DEFAULT_MAX_EDGE: u32 = 320;

/// 缩略图字节数的硬上限。
///
/// 超过就说明质量参数选得不对。header 会被列表页高频读取，
/// 让它膨胀会直接抵消缩略图带来的收益。
pub const SIZE_LIMIT: usize = 32 * 1024;

/// 输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbFormat {
    /// WebP：同等质量下体积最小，各平台 WebView 均支持。
    WebP,
    /// JPEG：兼容性兜底。
    Jpeg,
}

impl ThumbFormat {
    /// 对应的 MIME 类型。
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::WebP => "image/webp",
            Self::Jpeg => "image/jpeg",
        }
    }
}

/// 生成结果。
#[derive(Debug, Clone)]
pub struct Thumbnail {
    /// 编码后的字节。
    pub bytes: Vec<u8>,
    /// 实际宽度。
    pub width: u32,
    /// 实际高度。
    pub height: u32,
    /// 格式。
    pub format: ThumbFormat,
}

/// 等比缩放后的尺寸。
///
/// 独立成函数是为了能单独测边界：零尺寸、极端长宽比、
/// 比目标还小的图（不应放大）。
#[must_use]
pub fn fit_within(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 || max_edge == 0 {
        return (0, 0);
    }
    // 本来就比目标小就不放大：放大只会让文件变大而无信息增益
    if width <= max_edge && height <= max_edge {
        return (width, height);
    }
    let (w, h) = (u64::from(width), u64::from(height));
    let m = u64::from(max_edge);
    let (nw, nh) = if w >= h {
        (m, (h * m).div_ceil(w))
    } else {
        ((w * m).div_ceil(h), m)
    };
    // 极端长宽比下短边可能算成 0，至少保留 1 像素，
    // 否则编码器会拒绝零尺寸图像
    (
        u32::try_from(nw.max(1)).unwrap_or(max_edge),
        u32::try_from(nh.max(1)).unwrap_or(max_edge),
    )
}

/// 从图片字节生成缩略图。
///
/// # Errors
///
/// 解码失败、编码失败或结果超过 [`SIZE_LIMIT`] 时返回错误。
pub fn from_image_bytes(
    data: &[u8],
    max_edge: u32,
    format: ThumbFormat,
) -> Result<Thumbnail> {
    // 显式限定格式猜测：不依赖扩展名，只看实际内容。
    // 扩展名可以撒谎，而我们要把这些字节喂给解码器。
    let img = image::load_from_memory(data)
        .map_err(|e| MediaError::Image(format!("解码失败：{e}")))?;

    let (w, h) = (img.width(), img.height());
    let (tw, th) = fit_within(w, h, max_edge);
    if tw == 0 || th == 0 {
        return Err(MediaError::Image(format!("非法尺寸 {w}x{h}")));
    }

    // Lanczos3 质量最好；缩略图只生成一次，慢一点无妨
    let scaled = if (tw, th) == (w, h) {
        img
    } else {
        img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3)
    };

    let bytes = encode(&scaled, format)?;
    if bytes.len() > SIZE_LIMIT {
        return Err(MediaError::Image(format!(
            "缩略图 {} 字节超过上限 {SIZE_LIMIT}，应降低尺寸或质量",
            bytes.len()
        )));
    }

    Ok(Thumbnail {
        bytes,
        width: tw,
        height: th,
        format,
    })
}

/// 按指定格式编码。
fn encode(img: &image::DynamicImage, format: ThumbFormat) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        ThumbFormat::WebP => {
            // image crate 的 WebP 编码器只支持无损，
            // 对照片类内容体积偏大。转成 RGB8 去掉 alpha 可省一些。
            let rgb = image::DynamicImage::ImageRgb8(img.to_rgb8());
            rgb.write_to(&mut out, image::ImageFormat::WebP)
                .map_err(|e| MediaError::Image(format!("WebP 编码失败：{e}")))?;
        }
        ThumbFormat::Jpeg => {
            let rgb = image::DynamicImage::ImageRgb8(img.to_rgb8());
            rgb.write_to(&mut out, image::ImageFormat::Jpeg)
                .map_err(|e| MediaError::Image(format!("JPEG 编码失败：{e}")))?;
        }
    }
    Ok(out.into_inner())
}

/// 从视频的明文字节抽取指定时间点的帧。
///
/// # 为什么用 `-ss` 放在 `-i` 之前
///
/// 放在前面是**输入级 seek**，FFmpeg 会跳过前面的数据直接定位，
/// 快得多。放在后面是输出级 seek，会解码并丢弃前面所有帧——
/// 对一小时的视频取第 50 分钟的帧，两者差几个数量级。
///
/// 代价是输入级 seek 只能定位到关键帧，时间点可能有偏差。
/// 对缩略图而言这个偏差无关紧要。
///
/// # moov 在尾部的 MP4 会先在主进程内重排（实测得出）
///
/// 管道输入**无法 seek**，因此 moov 在尾部的 MP4 直接失败：
/// FFmpeg 报 `partial file` 与 `Cannot determine format after EOF`，
/// **stdout 完全为空**。实测排除了各种参数猜想——去掉 `-ss`、
/// 去掉 `scale`、把 `-ss` 移到 `-i` 之后都一样；让 FFmpeg 自己
/// 用 `-movflags frag_keyframe+empty_moov` remux 也只产出 1301 字节
/// 空壳，因为它同样读不到 moov。
///
/// 这类文件在真实世界很常见（录屏、相机直出、`-c copy` 输出）。
/// 解法是用 [`crate::mp4::to_faststart`] 在**主进程内用纯 Rust**
/// 把 moov 前移并修正 `stco`/`co64` 偏移，再喂给管道。
/// 这样既解决问题，又不违反「FFmpeg 子进程无文件系统访问」的安全约束。
///
/// 注意**不能**只是简单搬动 moov 而不修正偏移——实测会报
/// `Invalid NAL unit size (583027732 > 5369)`。
///
/// # Errors
///
/// FFmpeg 不可用、无任何产出、产物无法解码或超限时返回错误。
pub fn from_video_bytes(data: Vec<u8>, at_seconds: f64, max_edge: u32) -> Result<Thumbnail> {
    if !at_seconds.is_finite() || at_seconds < 0.0 {
        return Err(MediaError::InvalidInput {
            reason: format!("时间点 {at_seconds} 非法"),
        });
    }

    // 尾部 moov 的 MP4 先重排。非 MP4 容器（MKV/WebM 等）没有这个问题。
    // 重排失败时退回原数据：可能本来就能工作，不该因为优化失败而放弃。
    let prepared = if crate::mp4::looks_like_mp4(&data) {
        match crate::mp4::to_faststart(&data) {
            Ok(Some(reordered)) => reordered,
            // Ok(None) 表示已是 faststart，无需改动
            Ok(None) | Err(_) => data,
        }
    } else {
        data
    };

    extract_frame(prepared, at_seconds, max_edge)
}

/// 实际执行抽帧。
fn extract_frame(data: Vec<u8>, at_seconds: f64, max_edge: u32) -> Result<Thumbnail> {
    let ss = format!("{at_seconds:.3}");
    let scale = format!("scale='min({max_edge},iw)':-2");
    let args = [
        "-v", "error",
        // 输入级 seek，见上文
        "-ss", ss.as_str(),
        "-i", "-",
        "-frames:v", "1",
        "-vf", scale.as_str(),
        // 不要音频，省解码开销
        "-an",
        // 输出到 stdout 必须显式指定格式
        "-f", "webp",
        "-",
    ];

    let out = ffprobe::run_piped(Tool::Ffmpeg, &args, data)?;

    // 判据是**产物能否解码**，不是退出码。
    // FFmpeg 对某些输入会带警告地成功，也会以 0 退出码产出垃圾。
    if out.stdout.is_empty() {
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: out.stderr,
        });
    }

    // 回读确认产出的确实是能解码的图片，而不是一堆字节。
    let img = image::load_from_memory(&out.stdout).map_err(|e| {
        MediaError::Image(format!(
            "FFmpeg 产出的帧无法解码：{e}（退出码 {:?}，诊断：{}）",
            out.code, out.stderr
        ))
    })?;
    let (w, h) = (img.width(), img.height());

    if out.stdout.len() > SIZE_LIMIT {
        // 超限则用 image 重新编码压一遍，而不是直接失败
        let (tw, th) = fit_within(w, h, max_edge.min(DEFAULT_MAX_EDGE));
        let scaled = img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3);
        let bytes = encode(&scaled, ThumbFormat::Jpeg)?;
        if bytes.len() > SIZE_LIMIT {
            return Err(MediaError::Image(format!(
                "缩略图 {} 字节仍超过上限 {SIZE_LIMIT}",
                bytes.len()
            )));
        }
        return Ok(Thumbnail {
            bytes,
            width: tw,
            height: th,
            format: ThumbFormat::Jpeg,
        });
    }

    Ok(Thumbnail {
        bytes: out.stdout,
        width: w,
        height: h,
        format: ThumbFormat::WebP,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_preserves_aspect_ratio() {
        assert_eq!(fit_within(1920, 1080, 320), (320, 180));
        assert_eq!(fit_within(1080, 1920, 320), (180, 320));
        // 正方形
        assert_eq!(fit_within(500, 500, 320), (320, 320));
    }

    #[test]
    fn fit_does_not_upscale() {
        // 比目标小的图不应放大：放大只增体积不增信息
        assert_eq!(fit_within(100, 80, 320), (100, 80));
        assert_eq!(fit_within(320, 320, 320), (320, 320));
    }

    #[test]
    fn fit_handles_extreme_aspect_ratio() {
        // 10000x1 的全景图：短边算出来会是 0，
        // 必须保底 1 像素，否则编码器拒绝零尺寸
        let (w, h) = fit_within(10000, 1, 320);
        assert_eq!(w, 320);
        assert!(h >= 1, "短边不能为 0，实际 {h}");
    }

    #[test]
    fn fit_rejects_zero_dimensions() {
        assert_eq!(fit_within(0, 100, 320), (0, 0));
        assert_eq!(fit_within(100, 0, 320), (0, 0));
        assert_eq!(fit_within(100, 100, 0), (0, 0));
    }

    #[test]
    fn generates_thumbnail_from_synthetic_png() {
        // 构造一张真实的 PNG 再缩放，验证整条链路。
        // 用渐变而非纯色：纯色图会被编码器压到极小，
        // 无法暴露"体积是否合理"的问题。
        let mut img = image::RgbImage::new(800, 600);
        for (x, y, px) in img.enumerate_pixels_mut() {
            #[allow(clippy::cast_possible_truncation)]
            let r = (x % 256) as u8;
            #[allow(clippy::cast_possible_truncation)]
            let g = (y % 256) as u8;
            *px = image::Rgb([r, g, 128]);
        }
        let dynimg = image::DynamicImage::ImageRgb8(img);
        let mut png = std::io::Cursor::new(Vec::new());
        dynimg
            .write_to(&mut png, image::ImageFormat::Png)
            .expect("写 PNG 失败");

        let t = from_image_bytes(&png.into_inner(), 320, ThumbFormat::Jpeg).unwrap();
        assert_eq!((t.width, t.height), (320, 240));
        assert!(!t.bytes.is_empty());
        assert!(t.bytes.len() <= SIZE_LIMIT, "体积 {} 超限", t.bytes.len());

        // 回读验证：产出的字节必须真能解码成图片，
        // 且尺寸与声明一致。只检查"非空"是不够的。
        let back = image::load_from_memory(&t.bytes).expect("产物无法解码");
        assert_eq!(back.width(), 320);
        assert_eq!(back.height(), 240);
    }

    #[test]
    fn webp_output_is_decodable() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(200, 100, |x, y| {
            #[allow(clippy::cast_possible_truncation)]
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 64])
        }));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();

        let t = from_image_bytes(&png.into_inner(), 320, ThumbFormat::WebP).unwrap();
        assert_eq!(t.format, ThumbFormat::WebP);
        assert_eq!(t.format.mime(), "image/webp");
        // 不放大，尺寸应保持原样
        assert_eq!((t.width, t.height), (200, 100));
        let back = image::load_from_memory(&t.bytes).expect("WebP 产物无法解码");
        assert_eq!(back.width(), 200);
    }

    #[test]
    fn garbage_input_is_rejected() {
        let e = from_image_bytes(b"not an image at all", 320, ThumbFormat::Jpeg).unwrap_err();
        assert_eq!(e.code(), "IMAGE_ERROR");
    }

    #[test]
    fn empty_input_is_rejected() {
        assert!(from_image_bytes(&[], 320, ThumbFormat::Jpeg).is_err());
    }

    #[test]
    fn negative_timestamp_is_rejected() {
        let e = from_video_bytes(vec![0u8; 16], -1.0, 320).unwrap_err();
        assert_eq!(e.code(), "INVALID_INPUT");
    }

    #[test]
    fn nan_timestamp_is_rejected() {
        assert!(from_video_bytes(vec![0u8; 16], f64::NAN, 320).is_err());
        assert!(from_video_bytes(vec![0u8; 16], f64::INFINITY, 320).is_err());
    }
}
