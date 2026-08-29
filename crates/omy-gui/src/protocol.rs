//! `omystream://` 自定义协议：按需解密并以 HTTP 语义返回内容。
//!
//! # 这个协议的存在理由
//!
//! WebView 里的 `<video>` / `<img>` 只认 URL，不能直接喂字节。
//! 若把明文写成临时文件再让它读，明文就落盘了——这正是本项目
//! 要避免的 L1 泄露。自定义协议让我们在**内存里**响应请求：
//! 播放器要哪一段，就解密哪一段。
//!
//! # Spike S1 / S5 实测确立的约束（不要凭直觉改动）
//!
//! 以下每一条都是 `spikes/webview-range` 里踩出来的，
//! 改动前请先重跑那个 spike：
//!
//! | 约束 | 不遵守的后果 |
//! |---|---|
//! | 必须用**异步**协议 | 同步版本阻塞 WebView 线程，大文件解密时界面卡死 |
//! | `OPTIONS` 必须在解密**之前**短路 | 预检请求会触发整个文件的解密 |
//! | 必须发 CORS 头 | 页面在 `tauri.localhost`、协议在 `omystream://`，不同源：`fetch` 被拦、canvas 被污染 |
//! | 必须 `Expose-Headers: Content-Range` | 否则 JS 读不到它，无法判断 Range 是否真的生效 |
//! | 开放结尾的 Range 必须**设上限** | `bytes=1572864-` 会导致跳到 35% 就解密 65% 的文件 |
//! | 必须发 `no-store` | 否则 WebView 可能把解密后的明文写进磁盘缓存（S5） |
//! | Range 不可满足必须回 **416** | 钳到末尾会让播放器拿到错误数据却以为成功 |
//!
//! # URL 形式
//!
//! ```text
//! omystream://localhost/file/<id>        原始明文（P1 直通）
//! omystream://localhost/thumb/<id>       解密后的缩略图
//! ```
//!
//! id 由前端从文件列表拿到，是进程内的临时标识，不含路径信息。

use crate::state::AppState;
use omy_core::crypto::Kek;
use omy_core::file::OpenedFile;
use std::sync::Arc;
use tauri::http::{Method, Request, Response, StatusCode, header};

/// 单次响应的字节上限。
///
/// WebView 的 seek 请求是**开放结尾**的（`bytes=1572864-`）。
/// 老实返回「起点到文件末尾」意味着跳到 35% 就要解密 65% 的文件，
/// 按需解密的意义被完全抵消——这是 spike 日志里实测到的。
///
/// HTTP 允许服务端返回比请求更小的区间，只要 `Content-Range`
/// 如实描述返回了什么；播放器需要更多数据时会继续发请求。
///
/// 2 MiB：够覆盖若干秒的播放缓冲，又不至于一次解密过多。
const MAX_SPAN: u64 = 2 * 1024 * 1024;

/// 解析出的请求目标。
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// 文件正文。
    File(String),
    /// 缩略图。
    Thumb(String),
}

/// 从 URI 路径解析目标。
///
/// 只认已知前缀，其余一律 `None` → 404。不做任何「猜测」式回退：
/// 未知路径静默当成文件请求会让拼错的 URL 表现为解密失败，很难查。
#[must_use]
pub fn parse_target(path: &str) -> Option<Target> {
    let p = path.trim_start_matches('/');
    if let Some(id) = p.strip_prefix("file/") {
        if id.is_empty() {
            return None;
        }
        return Some(Target::File(decode_id(id)));
    }
    if let Some(id) = p.strip_prefix("thumb/") {
        if id.is_empty() {
            return None;
        }
        return Some(Target::Thumb(decode_id(id)));
    }
    None
}

/// 最小限度的百分号解码。
///
/// id 是我们自己生成的十六进制串，本不该含特殊字符；
/// 但 WebView 有时会对 URL 做规范化，这里容错处理。
fn decode_id(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i) {
            Some(&b'%') => {
                let hex = s.get(i.saturating_add(1)..i.saturating_add(3));
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i = i.saturating_add(3);
                    }
                    // 非法转义原样保留，不吞字符
                    None => {
                        out.push(b'%');
                        i = i.saturating_add(1);
                    }
                }
            }
            Some(&b) => {
                out.push(b);
                i = i.saturating_add(1);
            }
            None => break,
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 解析 `Range: bytes=start-end`，返回闭区间 `(start, end)`。
///
/// 只支持单区间——`<video>` 不会发多区间请求。
/// 返回 `None` 表示**不可满足**，调用方必须回 416 而不是钳到末尾。
#[must_use]
pub fn parse_range(h: &str, total: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    // 多区间直接拒绝，而不是只取第一个：
    // 静默降级会让调用方拿到不完整数据却以为成功
    if spec.contains(',') {
        return None;
    }
    let (s, e) = spec.split_once('-')?;
    let (s, e) = (s.trim(), e.trim());

    if total == 0 {
        return None;
    }
    let last = total.saturating_sub(1);

    if s.is_empty() {
        // suffix 形式：bytes=-500 表示最后 500 字节
        let n: u64 = e.parse().ok()?;
        if n == 0 {
            return None;
        }
        return Some((total.saturating_sub(n.min(total)), last));
    }

    let start: u64 = s.parse().ok()?;
    if start > last {
        return None; // 超出范围必须 416
    }
    let end = if e.is_empty() {
        last
    } else {
        e.parse::<u64>().ok()?.min(last)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}

/// 给响应加上所有必需的公共头。
///
/// 抽成函数是因为**每一条错误路径也必须带 CORS 头**。
/// 若只在成功路径加，前端 `fetch` 拿到的 4xx/5xx 会被 CORS 拦成
/// 网络错误，看不到真实状态码，排查时会误以为是协议没注册。
fn with_common_headers(b: tauri::http::response::Builder) -> tauri::http::response::Builder {
    b.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .header(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            "Content-Range, Content-Length, Accept-Ranges",
        )
        // S5：禁止任何形式的缓存，否则解密后的明文可能落盘
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .header(header::PRAGMA, "no-cache")
        .header("X-Content-Type-Options", "nosniff")
}

/// 构造一个只有状态码的响应（错误路径用）。
fn bare(status: StatusCode) -> Response<Vec<u8>> {
    with_common_headers(Response::builder().status(status))
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 处理一次协议请求。
pub fn handle(state: &Arc<AppState>, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    // CORS 预检必须在任何解密之前短路。
    // spike 里早先版本漏了这个分支，OPTIONS 被当成 GET，
    // 结果预检触发了整个文件的解密。
    if request.method() == Method::OPTIONS {
        return with_common_headers(
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header(header::ACCESS_CONTROL_ALLOW_METHODS, "GET, HEAD, OPTIONS")
                .header(header::ACCESS_CONTROL_ALLOW_HEADERS, "Range, Content-Type")
                .header(header::ACCESS_CONTROL_MAX_AGE, "600"),
        )
        .body(Vec::new())
        .unwrap_or_else(|_| Response::new(Vec::new()));
    }

    let Some(target) = parse_target(request.uri().path()) else {
        return bare(StatusCode::NOT_FOUND);
    };

    let id = match &target {
        Target::File(id) | Target::Thumb(id) => id.as_str(),
    };

    // 锁定态一律 403，绝不因为「id 存在」就返回内容。
    // 这道检查不能省：协议是 WebView 里任何脚本都能访问的入口，
    // 不能假设只有我们自己的前端会请求它。
    let Some(entry) = state.file(id) else {
        return bare(StatusCode::NOT_FOUND);
    };
    if !entry.unlocked {
        return bare(StatusCode::FORBIDDEN);
    }

    match target {
        Target::File(_) => serve_file(state, request, &entry),
        Target::Thumb(_) => serve_thumb(state, &entry),
    }
}

/// 打开文件并取得密钥。
///
/// 每次请求都重新打开是有意为之：不缓存 `OpenedFile` 意味着
/// 不在内存里长期持有 payload key。代价是每次多一次 header 解析
/// （微秒级，因为 KEK 已在会话里，不需要重跑 Argon2）。
fn open_for(state: &Arc<AppState>, path: &str) -> Option<(Vec<u8>, OpenedFile)> {
    let bytes = std::fs::read(path).ok()?;
    let h = omy_core::file::peek_header(&bytes).ok()?;
    let keks: Vec<Kek> = state.with_session(|s| {
        s.all_for(&h.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect()
    })?;
    let opened = omy_core::file::open(&bytes, &keks).ok()?;
    Some((bytes, opened))
}

/// 返回文件正文，支持 Range。
fn serve_file(
    state: &Arc<AppState>,
    request: &Request<Vec<u8>>,
    entry: &crate::state::FileEntry,
) -> Response<Vec<u8>> {
    let Some((bytes, opened)) = open_for(state, &entry.path) else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };

    let total = opened.header.plaintext_size;
    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    let (start, end_inclusive, is_partial) = match range_header {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
                // 416 必须带 Content-Range 告知真实长度，
                // 播放器据此纠正自己的认知
                return with_common_headers(
                    Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{total}")),
                )
                .body(Vec::new())
                .unwrap_or_else(|_| Response::new(Vec::new()));
            }
        },
        None => (0u64, total.saturating_sub(1), false),
    };

    // 只对 Range 请求设上限。无 Range 的请求必须返回完整内容，
    // 否则 200 响应与 Content-Length 不一致，播放器会认为文件被截断。
    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 压缩文件必须带上压缩索引，否则 read_range 无法把
    // 明文偏移映射到压缩块——会读出错位的数据。
    // core 对非压缩文件返回空 Vec，所以这里统一处理即可。
    let cindex = opened.compression_index().unwrap_or_default();
    let cindex = if cindex.is_empty() {
        None
    } else {
        Some(cindex)
    };

    // 关键契约：fetch_ct 收到的 off 是**相对载荷起点**的偏移，
    // 不是文件绝对偏移——read_range 内部已经减掉了 header_len。
    // 必须加回来才能正确索引整个文件。spike 里漏了这一步，
    // 表现为 chunk 0 认证失败 → MEDIA_ERR_SRC_NOT_SUPPORTED。
    let header_len = u64::from(opened.header.header_len);
    let data = match omy_core::payload::read_range(
        &opened.header,
        opened.payload_key(),
        cindex.as_deref(),
        start,
        length,
        |payload_off, len| {
            let abs = payload_off.saturating_add(header_len);
            let s = usize::try_from(abs).unwrap_or(usize::MAX);
            let e = usize::try_from(abs.saturating_add(len)).unwrap_or(usize::MAX);
            bytes
                .get(s..e)
                .map(<[u8]>::to_vec)
                .ok_or(omy_core::Error::Truncated {
                    context: "omystream ciphertext fetch",
                    need: e,
                    got: bytes.len(),
                })
        },
    ) {
        Ok(d) => d,
        Err(_) => return bare(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let mime = entry.mime.clone().unwrap_or_else(|| {
        String::from("application/octet-stream")
    });

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
            // 必须声明，否则部分 WebView 根本不发 Range 请求
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, data.len().to_string()),
    );

    b = if is_partial {
        b.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end_inclusive}/{total}"),
        )
    } else {
        b.status(StatusCode::OK)
    };

    b.body(data).unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 返回解密后的缩略图。
///
/// 缩略图存在元信息区的 TLV 里，不在载荷中——所以取它
/// 不需要解密整个文件，只要解开 header。这正是网格视图
/// 能在大量文件上保持流畅的原因（文档 §9 的 <50ms/项）。
fn serve_thumb(state: &Arc<AppState>, entry: &crate::state::FileEntry) -> Response<Vec<u8>> {
    let Some((_, opened)) = open_for(state, &entry.path) else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // 用 core 的高层方法而非手搓 TLV 解密：
    // TLV 的 value 是加密的，取原始字节没有意义
    let Ok(bytes) = opened.thumbnail() else {
        // 没有缩略图是**正常情况**（非媒体文件、或加密时没生成），
        // 用 404 而非 500——前端据此回退到类型图标
        return bare(StatusCode::NOT_FOUND);
    };

    with_common_headers(
        Response::builder()
            .status(StatusCode::OK)
            // 缩略图统一是 JPEG（由 omy-media 生成时固定）
            .header(header::CONTENT_TYPE, "image/jpeg")
            .header(header::CONTENT_LENGTH, bytes.len().to_string()),
    )
    .body(bytes)
    .unwrap_or_else(|_| Response::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_parsing() {
        assert_eq!(
            parse_target("/file/abc123"),
            Some(Target::File(String::from("abc123")))
        );
        assert_eq!(
            parse_target("/thumb/abc123"),
            Some(Target::Thumb(String::from("abc123")))
        );
        // 无前导斜杠也认
        assert_eq!(
            parse_target("file/x"),
            Some(Target::File(String::from("x")))
        );
    }

    #[test]
    fn target_rejects_unknown_and_empty() {
        // 未知路径必须 None → 404，不能猜测式回退成文件请求：
        // 那会让拼错的 URL 表现为「解密失败」，极难排查
        assert_eq!(parse_target("/"), None);
        assert_eq!(parse_target("/unknown/x"), None);
        assert_eq!(parse_target("/file"), None);
        assert_eq!(parse_target("/file/"), None, "空 id 必须拒绝");
        assert_eq!(parse_target("/thumb/"), None);
    }

    #[test]
    fn id_percent_decoding() {
        assert_eq!(decode_id("abc"), "abc");
        assert_eq!(decode_id("a%2Fb"), "a/b");
        // 非法转义原样保留，不吞字符
        assert_eq!(decode_id("a%zz"), "a%zz");
        assert_eq!(decode_id("a%"), "a%");
    }

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-499", 10_000), Some((0, 499)));
        assert_eq!(parse_range("bytes=9000-", 10_000), Some((9000, 9999)));
        assert_eq!(parse_range("bytes=-500", 10_000), Some((9500, 9999)));
        assert_eq!(parse_range("bytes=0-99999", 10_000), Some((0, 9999)));
        assert_eq!(parse_range("bytes= 100 - 200 ", 10_000), Some((100, 200)));
    }

    #[test]
    fn range_rejects_unsatisfiable() {
        // start 超范围必须 None（→416），不能钳到末尾：
        // 钳位会让播放器拿到错误数据却以为成功
        assert_eq!(parse_range("bytes=10000-", 10_000), None);
        assert_eq!(parse_range("bytes=500-100", 10_000), None);
        // 多区间不静默降级成单区间
        assert_eq!(parse_range("bytes=0-99,200-299", 10_000), None);
        assert_eq!(parse_range("items=0-10", 10_000), None);
        assert_eq!(parse_range("bytes=abc", 10_000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
        assert_eq!(parse_range("bytes=-0", 10_000), None);
    }

    #[test]
    fn single_byte_file() {
        assert_eq!(parse_range("bytes=0-0", 1), Some((0, 0)));
        assert_eq!(parse_range("bytes=0-", 1), Some((0, 0)));
        assert_eq!(parse_range("bytes=1-", 1), None);
    }

    #[test]
    fn max_span_caps_open_ended_range() {
        // 这是性能上的关键行为：开放结尾的 Range 必须被截断，
        // 否则跳到 35% 就要解密 65% 的文件。
        // 这里验证的是计算本身，实际生效在 serve_file 里。
        let total: u64 = 100 * 1024 * 1024;
        let (start, end) = parse_range("bytes=1572864-", total).unwrap_or((0, 0));
        assert_eq!(end, total - 1, "解析阶段仍是开放到末尾");
        let capped = end.min(start + MAX_SPAN - 1);
        assert_eq!(capped - start + 1, MAX_SPAN, "截断后正好 2 MiB");
        assert!(capped < end, "必须真的被截断");
    }

    #[test]
    fn error_responses_carry_cors_headers() {
        // 每条错误路径都要带 CORS 头，否则前端 fetch 拿到的
        // 4xx/5xx 会被拦成网络错误，看不到真实状态码
        for st in [
            StatusCode::NOT_FOUND,
            StatusCode::FORBIDDEN,
            StatusCode::INTERNAL_SERVER_ERROR,
        ] {
            let r = bare(st);
            assert_eq!(r.status(), st);
            assert!(
                r.headers().contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN),
                "{st} 缺 CORS 头"
            );
            assert!(
                r.headers().contains_key(header::CACHE_CONTROL),
                "{st} 缺 no-store"
            );
        }
    }
}
