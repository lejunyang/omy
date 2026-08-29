//! MIME 推导与预览类型判定。
//!
//! # 为什么这一层在 GUI 而不在 omy-media
//!
//! `omy-media` 关心的是「这个文件的编码是什么、能不能直通播放」，
//! 属于媒体领域。而「WebView 需要什么 MIME 才肯渲染」是浏览器的
//! 关注点——同一个文件在 `<video>` 和外部播放器里需要的信息不同。
//! 把它放进 omy-media 会让那个 crate 依赖浏览器的实现细节。
//!
//! # 首期支持范围（用户拍板）
//!
//! | 类型 | 决定 |
//! |---|---|
//! | HEIC / HEIF | **支持**。WebView 多数不认，走转码到 JPEG |
//! | SVG | 用 `<img>` 渲染，**不内联进 DOM** |
//! | GIF / APNG | 支持，按图片处理（浏览器原生会动） |
//! | RAW | 首期不做 |
//! | PDF | 不内置预览，走外部应用 |
//!
//! ## SVG 为什么必须用 `<img>`
//!
//! SVG 可以内嵌 `<script>`。若把解密后的 SVG 直接插进 DOM，
//! 那段脚本就在我们的页面上下文里执行了——它能读到 `omystream://`
//! 的所有内容，等于把整个库交给一个来路不明的文件。
//! `<img>` 加载的 SVG 处于**受限模式**，脚本不执行、外部引用不加载。
//! 这是浏览器提供的隔离，比我们自己写过滤器可靠。
//!
//! CSP 里 `object-src 'none'` 与 `frame-src 'none'` 是同一考虑的
//! 另一半：堵住 `<object>` / `<embed>` / `<iframe>` 这几条会让 SVG
//! 恢复脚本能力的路径。

use omy_media::MediaMeta;

/// 预览类别，决定前端用哪个组件。
pub mod kind {
    /// 视频，用 `<video>`。
    pub const VIDEO: &str = "video";
    /// 音频，用 `<audio>`。
    pub const AUDIO: &str = "audio";
    /// 图片，用 `<img>`。
    pub const IMAGE: &str = "image";
    /// 纯文本，取回后渲染为文本。
    pub const TEXT: &str = "text";
    /// 无法内置预览，只能外部打开。
    pub const OTHER: &str = "other";
}

/// 从文件名取小写后缀。
#[must_use]
pub fn extension_of(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

/// 仅按后缀判断类别与 MIME。
///
/// 用于没有媒体元信息的文件——文本、SVG 这类不走 ffprobe 的类型。
#[must_use]
pub fn by_extension(name: &str) -> (&'static str, String) {
    let ext = extension_of(name);
    if let Some(m) = image_mime(&ext) {
        return (kind::IMAGE, m.to_owned());
    }
    if let Some(m) = text_mime(&ext) {
        return (kind::TEXT, m.to_owned());
    }
    if let Some(m) = video_mime(&ext) {
        return (kind::VIDEO, m.to_owned());
    }
    if let Some(m) = audio_mime(&ext) {
        return (kind::AUDIO, m.to_owned());
    }
    (kind::OTHER, String::from("application/octet-stream"))
}

/// 结合媒体元信息判断类别与 MIME。
///
/// 优先用元信息里的轨道构成（有视频轨就是视频），后缀只用来
/// 挑具体的 MIME——因为 ffprobe 的容器名往往是多值的
/// （`mov,mp4,m4a,3gp,3g2,mj2`），不能直接当 MIME 用。
#[must_use]
pub fn classify(name: &str, meta: &MediaMeta) -> (&'static str, String) {
    let ext = extension_of(name);

    // 图片走 ffprobe 时会被识别成「单帧视频」，
    // 所以必须先按后缀排除，否则 PNG 会被当成视频播
    if let Some(m) = image_mime(&ext) {
        return (kind::IMAGE, m.to_owned());
    }

    if meta.is_audio_only() {
        let m = audio_mime(&ext).unwrap_or("audio/mpeg");
        return (kind::AUDIO, m.to_owned());
    }
    if meta.video.is_some() {
        let m = video_mime(&ext).unwrap_or("video/mp4");
        return (kind::VIDEO, m.to_owned());
    }
    by_extension(name)
}

/// 图片 MIME。
///
/// HEIC/HEIF 在这里给出真实 MIME，但**多数 WebView 不认**——
/// 前端拿到 `image/heic` 时会转而请求转码后的 JPEG。
/// 不在这里直接返回 `image/jpeg` 撒谎：那样前端就无从知道
/// 这个文件需要转码，会直接把 HEIC 字节喂给 `<img>` 然后显示破图。
#[must_use]
pub fn image_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "png" | "apng" => "image/png",
        "jpg" | "jpeg" | "jpe" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "avif" => "image/avif",
        // 用 <img> 渲染，受限模式下脚本不执行——见模块文档
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    })
}

/// 该图片格式能否被 WebView 直接渲染。
///
/// 不能的需要先转码成 JPEG/PNG 再喂给 `<img>`。
#[must_use]
pub fn image_needs_transcode(ext: &str) -> bool {
    // TIFF 除 Safari 外普遍不支持；HEIC/HEIF 只有 Safari 认。
    // 与其做浏览器嗅探，不如统一转码——转一次几十毫秒，
    // 而猜错的代价是用户看到破图且不知道为什么。
    matches!(ext, "heic" | "heif" | "tif" | "tiff")
}

/// 视频 MIME。
#[must_use]
pub fn video_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        // MKV 的 MIME。WebView 基本不认这个容器，
        // 播放时会走 P2 转封装——但 MIME 仍如实给出，
        // 让前端能据此判断需要转封装
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mov" => "video/quicktime",
        "flv" => "video/x-flv",
        "wmv" => "video/x-ms-wmv",
        "ts" => "video/mp2t",
        _ => return None,
    })
}

/// 音频 MIME。
#[must_use]
pub fn audio_mime(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wma" => "audio/x-ms-wma",
        _ => return None,
    })
}

/// 文本 MIME。
///
/// 一律返回 `text/plain`（而非 `text/html`、`application/javascript` 等），
/// 且前端只把它当文本渲染。理由与 SVG 相同：
/// 若给出 `text/html`，WebView 可能把它当页面解析，其中的脚本
/// 就在我们的上下文里跑起来了。
///
/// 用户想看渲染后的 HTML？那是「用外部应用打开」的事。
#[must_use]
pub fn text_mime(ext: &str) -> Option<&'static str> {
    const TEXT_EXTS: &[&str] = &[
        "txt", "md", "markdown", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "toml", "ini",
        "conf", "cfg", "rs", "py", "js", "ts", "jsx", "tsx", "c", "h", "cpp", "hpp", "java", "go",
        "rb", "php", "sh", "bat", "ps1", "sql", "css", "scss", "html", "htm", "vue", "svelte",
        "srt", "vtt", "ass", "ssa", "lrc",
    ];
    if TEXT_EXTS.contains(&ext) {
        // 全部按纯文本处理——见函数文档
        Some("text/plain; charset=utf-8")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_extraction() {
        assert_eq!(extension_of("a.mp4"), "mp4");
        assert_eq!(extension_of("A.MP4"), "mp4", "必须转小写");
        assert_eq!(extension_of("a.b.tar.gz"), "gz", "取最后一段");
        assert_eq!(extension_of("noext"), "");
        assert_eq!(extension_of(""), "");
        assert_eq!(extension_of(".hidden"), "hidden");
    }

    #[test]
    fn html_is_served_as_plain_text() {
        // 这条守护一个真实的 XSS 面：若 HTML 以 text/html 返回，
        // WebView 会当页面解析，其中的脚本就能读取整个库
        let (k, m) = by_extension("evil.html");
        assert_eq!(k, kind::TEXT);
        assert!(m.starts_with("text/plain"), "HTML 必须按纯文本返回，实得 {m}");

        let (_, m2) = by_extension("x.js");
        assert!(m2.starts_with("text/plain"), "JS 同理");
    }

    #[test]
    fn svg_gets_image_mime_for_img_tag() {
        // SVG 用 <img> 渲染，浏览器的受限模式会禁掉脚本
        let (k, m) = by_extension("logo.svg");
        assert_eq!(k, kind::IMAGE);
        assert_eq!(m, "image/svg+xml");
    }

    #[test]
    fn heic_reports_true_mime_not_a_lie() {
        // 不能假装 HEIC 是 JPEG：那样前端会直接把 HEIC 字节
        // 喂给 <img>，用户看到破图却不知道要转码
        let (k, m) = by_extension("IMG_1234.heic");
        assert_eq!(k, kind::IMAGE);
        assert_eq!(m, "image/heic");
        assert!(image_needs_transcode("heic"));
        assert!(image_needs_transcode("heif"));
        // 而这些不需要
        assert!(!image_needs_transcode("png"));
        assert!(!image_needs_transcode("jpg"));
        assert!(!image_needs_transcode("gif"));
        assert!(!image_needs_transcode("svg"));
    }

    #[test]
    fn gif_and_apng_are_images() {
        // 用户明确要求支持这两个动图格式
        assert_eq!(by_extension("x.gif").0, kind::IMAGE);
        assert_eq!(by_extension("x.apng").0, kind::IMAGE);
        assert_eq!(by_extension("x.apng").1, "image/png", "APNG 用 PNG 的 MIME");
    }

    #[test]
    fn raw_formats_fall_through_to_other() {
        // 首期不做 RAW，不能假装支持
        for raw in ["cr2", "nef", "arw", "dng", "orf"] {
            let (k, _) = by_extension(&format!("x.{raw}"));
            assert_eq!(k, kind::OTHER, "{raw} 首期不支持，应归 other");
        }
    }

    #[test]
    fn pdf_is_not_previewable() {
        // 用户明确说 PDF 暂不内置预览
        let (k, _) = by_extension("doc.pdf");
        assert_eq!(k, kind::OTHER);
    }

    #[test]
    fn mkv_reports_real_mime() {
        // 如实给出 MKV 的 MIME，前端据此判断要走 P2 转封装
        let (k, m) = by_extension("movie.mkv");
        assert_eq!(k, kind::VIDEO);
        assert_eq!(m, "video/x-matroska");
    }

    #[test]
    fn subtitle_files_are_text() {
        for s in ["srt", "vtt", "ass", "ssa"] {
            let (k, _) = by_extension(&format!("movie.{s}"));
            assert_eq!(k, kind::TEXT, "{s} 应可作为文本预览");
        }
    }

    #[test]
    fn unknown_falls_back_to_octet_stream() {
        let (k, m) = by_extension("mystery.xyzzy");
        assert_eq!(k, kind::OTHER);
        assert_eq!(m, "application/octet-stream");
    }
}
