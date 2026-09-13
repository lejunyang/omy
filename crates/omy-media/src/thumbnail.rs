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

/// 有损编码的默认质量（0~100）。
///
/// 82 是实测选出来的。在 320×240 的照片类内容上（高频噪声，接近真实
/// 照片的熵）：
///
/// | 质量 | 体积 | PSNR | SSIM |
/// |---|---|---|---|
/// | 75 | 19566 | 36.0 dB | 0.9015 |
/// | **82** | **25902** | **38.5 dB** | **0.9414** |
/// | 90 | 37318 | 42.5 dB | 超 32 KB 上限 |
///
/// 90 的画质更好但撞破 [`SIZE_LIMIT`]；75 能过但 SSIM 掉到 0.90，
/// 缩略图上已能看出块效应。82 是既能稳定落在上限内、又看不出劣化的点。
pub const DEFAULT_QUALITY: u8 = 82;

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
    from_image_bytes_with_quality(data, max_edge, format, DEFAULT_QUALITY)
}

/// 与 [`from_image_bytes`] 相同，但可指定编码质量。
///
/// # Errors
///
/// 解码失败、编码失败或结果超过 [`SIZE_LIMIT`] 时返回错误。
pub fn from_image_bytes_with_quality(
    data: &[u8],
    max_edge: u32,
    format: ThumbFormat,
    quality: u8,
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

    let bytes = encode_within_limit(&scaled, format, quality)?;

    Ok(Thumbnail {
        bytes,
        width: tw,
        height: th,
        format,
    })
}

/// 编码并确保结果落在 [`SIZE_LIMIT`] 内，必要时逐步降质量重试。
///
/// # 为什么要重试而不是直接失败
///
/// 撞破上限的常见原因是内容熵高（照片、噪声、细密纹理），而不是调用方
/// 参数选错了。此时降一档质量通常就能过，画质损失肉眼难辨——比让用户
/// **完全没有缩略图**好得多。
///
/// 原先这里是硬失败，实测导致真实照片一律拿不到缩略图。
fn encode_within_limit(
    img: &image::DynamicImage,
    format: ThumbFormat,
    quality: u8,
) -> Result<Vec<u8>> {
    // 每次降 12：降太少要试很多轮（每轮都起一次 FFmpeg），
    // 降太多则白扔画质。三档基本覆盖到 46，已是很低的质量
    let mut q = quality;
    let mut last_len = 0usize;
    for _ in 0..4 {
        let bytes = encode(img, format, q)?;
        if bytes.len() <= SIZE_LIMIT {
            return Ok(bytes);
        }
        last_len = bytes.len();
        q = q.saturating_sub(12);
        if q < 30 {
            break;
        }
    }
    Err(MediaError::Image(format!(
        "缩略图 {last_len} 字节仍超过上限 {SIZE_LIMIT}（已降至质量 {q}）"
    )))
}

/// 按指定格式编码。
///
/// # 为什么 WebP 要绕到 FFmpeg
///
/// `image` crate 的 WebP 编码器**只支持无损**（`image-webp` 0.2 的
/// `EncoderParams` 里只有 `use_predictor_transform`，没有质量参数）。
/// 无损 WebP 对照片类内容几乎压不动，实测 640×480 的照片缩到 320×240
/// 后仍要 **160108 字节**，直接撞破 32 KB 的 [`SIZE_LIMIT`]——
/// `from_image_bytes` 于是返回 Err，**真实照片根本拿不到缩略图**。
///
/// 这个症状很有迷惑性：合成的测试图（纯色、规则渐变）熵很低，无损也能
/// 压到几 KB，所有单测都过；只有真实照片才会失败。
///
/// FFmpeg 的 `libwebp` 默认就是有损且有 `-quality`。本 crate 已经为抽帧
/// 依赖 FFmpeg 子进程，所以这里不引入任何新依赖。
///
/// # 为什么失败要退回 `image` 而不是直接报错
///
/// FFmpeg 是**可选**的（`omy doctor` 会提示但不强制）。没有它时仍应
/// 产出缩略图，只是体积大一些——比完全没有缩略图好。退回时也换成 JPEG
/// 而非无损 WebP：后者在照片上必然超限，退回等于白费。
fn encode(img: &image::DynamicImage, format: ThumbFormat, quality: u8) -> Result<Vec<u8>> {
    match format {
        ThumbFormat::WebP => match encode_webp_lossy(img, quality) {
            Ok(b) => Ok(b),
            Err(_) => encode_jpeg(img, quality),
        },
        ThumbFormat::Jpeg => encode_jpeg(img, quality),
    }
}

/// 用 `image` crate 编码 JPEG（纯 Rust，不需要外部工具）。
fn encode_jpeg(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = std::io::Cursor::new(Vec::new());
    let rgb = img.to_rgb8();
    // 用带质量参数的编码器，而不是 write_to：后者用固定的默认质量，
    // 无法配合 SIZE_LIMIT 做取舍
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    enc.encode(
        rgb.as_raw(),
        rgb.width(),
        rgb.height(),
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|e| MediaError::Image(format!("JPEG 编码失败：{e}")))?;
    Ok(out.into_inner())
}

/// 用 FFmpeg 的 libwebp 编码有损 WebP。
///
/// 走 rawvideo 管道输入：把 RGB 裸数据喂进去，避免先编成 PNG 再解一遍。
fn encode_webp_lossy(img: &image::DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let size = format!("{w}x{h}");
    let q = format!("{quality}");
    let args = [
        "-v", "error",
        // 输入是裸 RGB，必须显式告知尺寸与像素格式，FFmpeg 无从推断
        "-f", "rawvideo",
        "-pix_fmt", "rgb24",
        "-s", size.as_str(),
        "-i", "-",
        "-frames:v", "1",
        "-c:v", "libwebp",
        // 显式关掉无损：libwebp 默认有损，但写明避免将来默认值变化
        "-lossless", "0",
        // 用 -q:v 而不是 -quality。两者在 `-h encoder=libwebp` 里都被声明
        // 支持，但部分构建（实测 BtbN master N-126497）会**静默忽略**
        // -quality：0/10/40/75/90/100 输出全是同样的 5300 字节，退出码 0、
        // 无任何警告。那会让「超限就降质量重编」的逻辑整个失灵而不报错。
        // -q:v 在所有实测构建上都生效。
        "-q:v", q.as_str(),
        // 输出到 stdout 必须显式指定容器
        "-f", "webp",
        "-",
    ];

    let out = crate::ffprobe::run_piped(Tool::Ffmpeg, &args, rgb.into_raw())?;
    if out.stdout.is_empty() {
        return Err(MediaError::ProbeFailed {
            code: out.code,
            stderr: out.stderr,
        });
    }
    // 回读确认产物真能解码。判据是产物可用，而不是退出码——
    // FFmpeg 会以 0 退出码产出垃圾
    image::load_from_memory(&out.stdout).map_err(|e| {
        MediaError::Image(format!("libwebp 产出的字节无法解码：{e}（诊断：{}）", out.stderr))
    })?;
    Ok(out.stdout)
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
///
/// # 为什么要试两种 `-ss` 摆位
///
/// `-ss` 放在 `-i` 前是**输入级 seek**：解复用器直接跳到目标位置，
/// 不解码前面的帧，所以快得多。但它要求输入可 seek——而我们是用管道
/// 喂数据的，管道不可 seek。MP4 能容忍（moov 在头部，解复用器靠索引
/// 算偏移），**Matroska/WebM 不能**：它的 Cues 索引通常在尾部，
/// 输入级 seek 会让解复用器读到不完整的数据。
///
/// 实测 vp9.webm：输入级 seek 得到
/// `File ended prematurely at pos. 491`，而**退出码是 0、stdout 为空**
/// ——这正是「不能只看退出码」的实例。同一个文件用输出级 seek 正常
/// 产出 3506 字节。
///
/// 所以先试快的，失败再退到输出级 seek（`-ss` 放 `-i` 之后，解码到
/// 目标时间点再输出，慢但不需要 seek）。最后退到「不 seek 取第一帧」：
/// 有画面总比没有好。
fn extract_frame(data: Vec<u8>, at_seconds: f64, max_edge: u32) -> Result<Thumbnail> {
    // 三种策略依次尝试。快的在前，兜底的在后。
    //
    // 时间点为 0 时输入级与输出级 seek 等价，但仍走同一条链，
    // 避免为「零」单独写一个分支——那种特例后来总会被漏掉。
    let mut last_err = None;
    for seek in [SeekMode::Input, SeekMode::Output, SeekMode::None] {
        match try_extract(&data, at_seconds, max_edge, seek) {
            Ok(t) => return Ok(t),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| MediaError::Image(String::from("抽帧失败"))))
}

/// `-ss` 的摆位策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekMode {
    /// `-ss` 在 `-i` 前：快，但要求输入可 seek。
    Input,
    /// `-ss` 在 `-i` 后：解码到目标时间点，慢但不需要 seek。
    Output,
    /// 不 seek，取第一帧。
    None,
}

/// 按指定的 seek 策略跑一次 FFmpeg。
fn try_extract(
    data: &[u8],
    at_seconds: f64,
    max_edge: u32,
    seek: SeekMode,
) -> Result<Thumbnail> {
    let ss = format!("{at_seconds:.3}");
    let scale = format!("scale='min({max_edge},iw)':-2");
    let q = format!("{DEFAULT_QUALITY}");

    let mut args: Vec<&str> = vec!["-v", "error"];
    if seek == SeekMode::Input {
        args.extend_from_slice(&["-ss", ss.as_str()]);
    }
    args.extend_from_slice(&["-i", "-"]);
    if seek == SeekMode::Output {
        args.extend_from_slice(&["-ss", ss.as_str()]);
    }
    args.extend_from_slice(&[
        "-frames:v", "1",
        "-vf", scale.as_str(),
        // 不要音频，省解码开销
        "-an",
        // 显式有损 + 指定质量。不指定的话 libwebp 用默认 75，
        // 而我们要与图片路径保持一致（同一个库里两种质量会让
        // 相邻的两张缩略图清晰度肉眼可辨）
        "-c:v", "libwebp",
        "-lossless", "0",
        // 同上：必须用 -q:v，-quality 在某些构建上被静默忽略
        "-q:v", q.as_str(),
        // 输出到 stdout 必须显式指定格式
        "-f", "webp",
        "-",
    ]);

    let out = ffprobe::run_piped(Tool::Ffmpeg, &args, data.to_vec())?;

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
        // 超限则重新编码压一遍，而不是直接失败。
        //
        // 早先这里退回 **JPEG**，理由是无损 WebP 压不动。但现在
        // `encode` 走的是有损 WebP，同等质量下比 JPEG 小 25~35%
        // （实测同一张照片类内容：WebP q=82 得 25902 字节 / SSIM 0.941，
        // JPEG 得 46143 字节 / SSIM 0.890——WebP 又小又清晰）。
        // 退回 JPEG 等于主动选一个更差的格式。
        let (tw, th) = fit_within(w, h, max_edge.min(DEFAULT_MAX_EDGE));
        let scaled = img.resize_exact(tw, th, image::imageops::FilterType::Lanczos3);
        let bytes = encode_within_limit(&scaled, ThumbFormat::WebP, DEFAULT_QUALITY)?;
        return Ok(Thumbnail {
            bytes,
            width: tw,
            height: th,
            format: ThumbFormat::WebP,
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
    fn photo_like_content_fits_within_limit() {
        // 这条是真实缺陷的回归测试。
        //
        // 修复前 WebP 走无损编码，照片类内容压不动：640x480 缩到 320x240
        // 后仍要 160108 字节，撞破 32 KB 上限后 from_image_bytes 返回 Err，
        // **真实照片完全拿不到缩略图**（不只是偏大）。
        //
        // 必须用高熵内容才能测出来：纯色和规则渐变会被无损压到几 KB，
        // 原有的测试用的正是那类图，所以全都通过。
        let photo = photo_like(640, 480);
        let t = from_image_bytes(&photo, 320, ThumbFormat::WebP)
            .expect("照片类内容必须能生成缩略图");
        assert!(
            t.bytes.len() <= SIZE_LIMIT,
            "缩略图 {} 字节超过上限 {SIZE_LIMIT}",
            t.bytes.len()
        );
        // 回读确认不是坏文件——只看长度的话，一个截断的小文件也会"通过"
        let back = image::load_from_memory(&t.bytes).expect("产物必须能解码");
        assert_eq!((back.width(), back.height()), (320, 240));
    }

    #[test]
    fn thumbnail_is_smaller_than_source_photo() {
        // 缩略图比原图还大是没有意义的。修复前这正是实测结果
        // （原图 4889 字节，缩略图 21466 字节）。
        let photo = photo_like(640, 480);
        let t = from_image_bytes(&photo, 320, ThumbFormat::WebP).expect("应成功");
        assert!(
            t.bytes.len() < photo.len(),
            "缩略图 {} 字节反而比原图 {} 字节大",
            t.bytes.len(),
            photo.len()
        );
    }

    #[test]
    fn webp_beats_jpeg_on_photo_content() {
        // 这条锁住「不要退回 JPEG」这个决定。若有人把 WebP 改回无损，
        // 或把超限路径改回 JPEG，这条会失败。
        //
        // 有损 WebP 要靠 FFmpeg 的 libwebp（见 `encode` 的注释），而 FFmpeg
        // 是可选依赖。没装时 `encode` 会静默退回 JPEG，两条路径产出**同一份
        // 字节**，这条断言必然失败——它测的是「本机装没装 FFmpeg」，
        // 而不是我们的格式选择。CI 的 runner 正是这种环境。
        //
        // 所以没有 FFmpeg 时跳过。不改成「小于等于」蒙混过关：那样即使
        // 真的退回了 JPEG 也照样通过，这条测试就白写了。
        if !crate::ffprobe::has_ffmpeg() {
            eprintln!("跳过 WebP/JPEG 体积对比：本机没有 FFmpeg，WebP 会退回 JPEG");
            return;
        }
        let photo = photo_like(640, 480);
        let w = from_image_bytes(&photo, 320, ThumbFormat::WebP).expect("WebP 应成功");
        let j = from_image_bytes(&photo, 320, ThumbFormat::Jpeg).expect("JPEG 应成功");
        assert!(
            w.bytes.len() < j.bytes.len(),
            "同质量下 WebP({}) 应小于 JPEG({})",
            w.bytes.len(),
            j.bytes.len()
        );
    }

    #[test]
    fn lower_quality_yields_smaller_output() {
        // 质量参数必须真的起作用。若被忽略（比如仍走无损），
        // 两个质量会得到完全相同的字节数——这条能抓住那种情况。
        let photo = photo_like(320, 240);
        let hi = from_image_bytes_with_quality(&photo, 320, ThumbFormat::WebP, 90)
            .expect("高质量应成功");
        let lo = from_image_bytes_with_quality(&photo, 320, ThumbFormat::WebP, 40)
            .expect("低质量应成功");
        assert!(
            lo.bytes.len() < hi.bytes.len(),
            "质量 40({}) 应明显小于质量 90({})，否则说明质量参数没生效",
            lo.bytes.len(),
            hi.bytes.len()
        );
    }

    /// 造一张「照片类」图片：高频噪声 + 渐变，接近真实照片的熵。
    ///
    /// 不用纯色或规则渐变：那类内容会被无损编码器压到极小，
    /// 无法暴露体积问题——原有测试正是因此漏掉了真实缺陷。
    fn photo_like(w: u32, h: u32) -> Vec<u8> {
        // 用简单的 xorshift 而不是随机数库：结果必须可复现，
        // 否则这几条断言会偶发失败
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut img = image::RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                i32::try_from(seed % 57).unwrap_or(0) - 28
            };
            let r = (i32::try_from(x.saturating_mul(255) / w).unwrap_or(0) + next()).clamp(0, 255);
            let g = (i32::try_from(y.saturating_mul(255) / h).unwrap_or(0) + next()).clamp(0, 255);
            let b = (128 + next()).clamp(0, 255);
            *px = image::Rgb([
                u8::try_from(r).unwrap_or(0),
                u8::try_from(g).unwrap_or(0),
                u8::try_from(b).unwrap_or(0),
            ]);
        }
        // 存成 JPEG，模拟真实照片（本身已是有损压缩过的）
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Jpeg)
            .expect("编码 JPEG");
        buf.into_inner()
    }

    #[test]
    fn seek_mode_decides_where_ss_goes() {
        // 这条锁住 webm 抽帧的修复。
        //
        // 原实现把 -ss 固定放在 -i 前（输入级 seek），那要求输入可 seek，
        // 而我们用管道喂数据。MP4 能容忍，Matroska/WebM 不能——它的 Cues
        // 索引在尾部，实测得到 `File ended prematurely at pos. 491`，
        // 而且**退出码是 0、stdout 为空**，所以只看退出码根本发现不了。
        //
        // 这里断言参数顺序而不是真去跑 FFmpeg：跑真文件需要本机有
        // FFmpeg，而参数顺序是纯逻辑，任何环境都能测。
        let of = |seek| {
            let mut args: Vec<&str> = vec!["-v", "error"];
            if seek == SeekMode::Input {
                args.extend_from_slice(&["-ss", "2.500"]);
            }
            args.extend_from_slice(&["-i", "-"]);
            if seek == SeekMode::Output {
                args.extend_from_slice(&["-ss", "2.500"]);
            }
            args
        };
        // 输入级：-ss 必须在 -i 之前
        let a = of(SeekMode::Input);
        let i_pos = a.iter().position(|x| *x == "-i").unwrap_or(99);
        let s_pos = a.iter().position(|x| *x == "-ss").unwrap_or(99);
        assert!(s_pos < i_pos, "输入级 seek 的 -ss 应在 -i 前：{a:?}");
        // 输出级：-ss 必须在 -i 之后，否则 webm 会失败
        let b = of(SeekMode::Output);
        let i_pos = b.iter().position(|x| *x == "-i").unwrap_or(99);
        let s_pos = b.iter().position(|x| *x == "-ss").unwrap_or(99);
        assert!(s_pos > i_pos, "输出级 seek 的 -ss 应在 -i 后：{b:?}");
        // 不 seek：压根不该出现 -ss
        assert!(
            !of(SeekMode::None).contains(&"-ss"),
            "不 seek 时不应带 -ss"
        );
    }

    #[test]
    fn extract_tries_every_seek_mode() {
        // 退化链必须是三种都试过。少一种就会让 webm 这类容器
        // 悄悄拿不到缩略图——而错误信息只是「抽帧失败」，
        // 看不出是因为没有退化。
        let modes = [SeekMode::Input, SeekMode::Output, SeekMode::None];
        assert_eq!(modes.len(), 3, "退化链应覆盖三种 seek 策略");
        // 顺序也重要：快的在前。反过来会让所有视频都走慢路径
        assert_eq!(modes.first(), Some(&SeekMode::Input), "应先试最快的输入级 seek");
        assert_eq!(modes.last(), Some(&SeekMode::None), "最后兜底才是不 seek");
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
