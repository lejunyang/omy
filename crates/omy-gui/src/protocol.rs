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
//! omystream://localhost/file/<id>         本地文件明文（P1 直通）
//! omystream://localhost/thumb/<id>        本地缩略图
//! omystream://localhost/rfile/<id>        远端文件明文
//! omystream://localhost/rthumb/<id>       远端缩略图
//! omystream://localhost/pfile/<token>     远程存储位置（WebDAV 等）文件明文
//! omystream://localhost/pthumb/<token>    远程存储位置文件缩略图
//! ```
//!
//! id 由前端从文件列表拿到，是进程内的临时标识，不含路径信息。
//! `pfile` 的 token 由 `remote_place_open` 颁发，对应一个带密文块缓存的
//! 远程来源；多次 Range 请求复用同一来源，seek 才不会重复下载。
//!
//! # 远端为什么能走同一条路
//!
//! `omy_core::payload::read_range` 取密文用的是一个**闭包**。
//! 本地实现是「从已读入的字节里切一段」，远端实现是「发一条
//! `Request::Read` 去对方那里取一段」——除此之外，解密、Range
//! 处理、CORS、no-store 全都一模一样。
//!
//! 结果是：远端 1 GB 的视频拖动进度条时，只取那附近的几十 KB，
//! 既不用先下完整个文件，也没有任何明文落盘。

use crate::place_files::{PlaceFiles, PlaceThumbs};
use crate::remote::RemoteSession;
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
    /// 本地文件正文。
    File(String),
    /// 本地缩略图。
    Thumb(String),
    /// 远端文件正文。
    RemoteFile(String),
    /// 远端缩略图。
    RemoteThumb(String),
    /// 远程存储位置（WebDAV 等）文件正文。
    PlaceFile(String),
    /// 远程存储位置缩略图。
    PlaceThumb(String),
    /// 远程目录容器里的一个条目（`/pcitem/<token>`）。
    PlaceContainerItem(String),
    /// 未加密文件的正文（磁盘明文直读）。
    Plain(String),
    /// 容器（加密文件夹）内单个文件的正文。
    ContainerItem(String),
}

/// 从 URI 路径解析目标。
///
/// 只认已知前缀，其余一律 `None` → 404。不做任何「猜测」式回退：
/// 未知路径静默当成文件请求会让拼错的 URL 表现为解密失败，很难查。
#[must_use]
pub fn parse_target(path: &str) -> Option<Target> {
    let p = path.trim_start_matches('/');
    // 顺序要紧：`rfile/` 必须在 `file/` 之前判断，否则
    // strip_prefix("file/") 对 "rfile/x" 不匹配倒是没错，
    // 但把它们写在一起更容易看出这是一组平行分支
    for (prefix, make) in [
        ("rfile/", Target::RemoteFile as fn(String) -> Target),
        ("rthumb/", Target::RemoteThumb as fn(String) -> Target),
        ("pfile/", Target::PlaceFile as fn(String) -> Target),
        ("pthumb/", Target::PlaceThumb as fn(String) -> Target),
        ("pcitem/", Target::PlaceContainerItem as fn(String) -> Target),
        ("file/", Target::File as fn(String) -> Target),
        ("thumb/", Target::Thumb as fn(String) -> Target),
        ("plain/", Target::Plain as fn(String) -> Target),
        ("citem/", Target::ContainerItem as fn(String) -> Target),
    ] {
        if let Some(id) = p.strip_prefix(prefix) {
            if id.is_empty() {
                return None;
            }
            return Some(make(decode_id(id)));
        }
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
pub fn handle(
    state: &Arc<AppState>,
    remote: &Arc<RemoteSession>,
    place_files: &PlaceFiles,
    place_thumbs: &crate::place_files::PlaceThumbs,
    place_containers: &crate::place_files::PlaceContainers,
    request: &Request<Vec<u8>>,
) -> Response<Vec<u8>> {
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

    match target {
        Target::File(id) => match state.file(&id) {
            Some(e) if e.unlocked => serve_file(state, request, &e),
            // 锁定态一律 403，绝不因为「id 存在」就返回内容。
            // 这道检查不能省：协议是 WebView 里任何脚本都能访问的入口，
            // 不能假设只有我们自己的前端会请求它
            Some(_) => bare(StatusCode::FORBIDDEN),
            None => bare(StatusCode::NOT_FOUND),
        },
        Target::Thumb(id) => match state.file(&id) {
            Some(e) if e.unlocked => serve_thumb(state, &e),
            Some(_) => bare(StatusCode::FORBIDDEN),
            None => bare(StatusCode::NOT_FOUND),
        },
        Target::RemoteFile(id) => serve_remote_file(state, remote, request, &id),
        Target::RemoteThumb(id) => serve_remote_thumb(state, remote, &id),
        Target::PlaceFile(token) => serve_place_file(state, place_files, request, &token),
        Target::PlaceThumb(token) => serve_place_thumb(state, place_thumbs, &token),
        Target::PlaceContainerItem(token) => {
            serve_place_container_item(state, place_files, place_containers, request, &token)
        }
        Target::Plain(token) => serve_plain(state, request, &token),
        Target::ContainerItem(token) => serve_container_item(state, request, &token),
    }
}

/// 返回未加密文件的正文，支持 Range。
///
/// # 与 `serve_file` 的关系
///
/// 两者的 Range 语义必须**完全一致**，否则会出现「加密的视频能拖，
/// 没加密的反而不能」这种莫名其妙的差异。所以这里同样：
/// 开放结尾截断到 `MAX_SPAN`、不可满足回 416、带全套 CORS 头。
///
/// 唯一的区别是取字节的方式——直接 seek 磁盘，不解密。
///
/// # 为什么不缓存整个文件
///
/// 用 `seek` + 定长读，而不是 `fs::read` 之后切片。播放一个 4 GB 的
/// 视频时，后者会把整个文件读进内存。
fn serve_plain(
    state: &Arc<AppState>,
    request: &Request<Vec<u8>>,
    token: &str,
) -> Response<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};

    // 只有被浏览过的文件才有 token。这道检查挡住的是
    // 「WebView 里的脚本构造 URL 去读任意文件」
    let Some(path) = state.plain.resolve(token) else {
        return bare(StatusCode::NOT_FOUND);
    };
    let Ok(mut f) = std::fs::File::open(&path) else {
        return bare(StatusCode::NOT_FOUND);
    };
    let Ok(md) = f.metadata() else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let total = md.len();

    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    let (start, end_inclusive, is_partial) = match range_header {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
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

    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 无 Range 的请求也要设上限：否则双击一个 4 GB 的视频，
    // WebView 的第一个请求（不带 Range）就会把 4 GB 读进内存。
    // 返回前 MAX_SPAN 并如实标注 206，播放器会继续要后面的
    let (length, is_partial, end_inclusive) = if !is_partial && length > MAX_SPAN {
        (MAX_SPAN, true, MAX_SPAN.saturating_sub(1))
    } else {
        (length, is_partial, end_inclusive)
    };

    if f.seek(SeekFrom::Start(start)).is_err() {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let cap = usize::try_from(length).unwrap_or(0);
    let mut data = vec![0u8; cap];
    let Ok(n) = f.read(&mut data) else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };
    data.truncate(n);

    // 文件可能在读的过程中被截短，如实按实际读到的长度回报，
    // 不然 Content-Length 与正文对不上，播放器会认为连接断了
    let end_inclusive = start
        .saturating_add(n as u64)
        .saturating_sub(1)
        .min(end_inclusive);

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (_, mime) = crate::mime::by_extension(&name);

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
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

/// 返回容器（加密文件夹）内**单个**文件的正文，支持 Range。
///
/// # 为什么能只解密其中一个文件
///
/// 容器的载荷就是把各个文件首尾相接后按块加密。索引里记着每个文件在
/// 明文载荷中的区间 `(offset, len)`，而 `read_range` 本来就支持从任意
/// 明文偏移读任意长度——它只会解开覆盖该区间的那几个块。
///
/// 所以这里做的事就是**加一层偏移**：外界请求容器内文件的第 `start`
/// 字节，实际要读的是载荷的第 `item.offset + start` 字节。
///
/// # 与 `serve_file` 的关系
///
/// Range 语义必须**完全一致**，否则会出现「同一个视频放在容器外能拖
/// 进度条、放进容器就不能」。所以开放结尾截断到 `MAX_SPAN`、不可满足
/// 回 416、CORS、`no-store` 全部照搬。
///
/// # 授权：每次请求都要回查
///
/// token 在登记表里**不构成**授权。锁定后同一个 token 仍在表里（虽然
/// `lock()` 会清），但 `state.file(entry_id)` 会查不到或 `unlocked`
/// 为假——这才是真正的判据。授权只看会话当下的状态，不看登记时的状态。
fn serve_container_item(
    state: &Arc<AppState>,
    request: &Request<Vec<u8>>,
    token: &str,
) -> Response<Vec<u8>> {
    let Some(item) = state.citem.resolve(token) else {
        return bare(StatusCode::NOT_FOUND);
    };

    // 关键授权检查：拿所属容器的 id 回查会话。
    // 锁定后这里会查不到条目（lock 清空了文件表），于是 403/404——
    // 不能因为「token 还在表里」就返回内容
    let Some(entry) = state.file(&item.entry_id) else {
        return bare(StatusCode::NOT_FOUND);
    };
    if !entry.unlocked {
        return bare(StatusCode::FORBIDDEN);
    }

    let Some((bytes, opened)) = open_for(state, &entry.path) else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // 这个文件自己的长度，不是整个容器的。Range 的 total 必须是它——
    // 否则播放器会以为文件有整个容器那么长，拖到后面读出别人的字节
    let total = item.size;

    // 索引里的区间必须落在载荷内。正常情况下恒成立，但索引来自文件
    // 内容，损坏或被篡改的容器可能给出越界区间——那会让下面的加法
    // 算出一个指向其他文件数据的偏移
    let payload_total = opened.header.plaintext_size;
    if item
        .offset
        .checked_add(total)
        .is_none_or(|end| end > payload_total)
    {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    }

    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    let (start, end_inclusive, is_partial) = match range_header {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
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

    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 无 Range 的请求同样要设上限：容器里可能有个 4 GB 的视频，
    // 第一个不带 Range 的请求会把它整个解密进内存。
    // 与 `serve_plain` 的处理一致：返回前 MAX_SPAN 并如实标 206
    let (length, is_partial, end_inclusive) = if !is_partial && length > MAX_SPAN {
        (MAX_SPAN, true, MAX_SPAN.saturating_sub(1))
    } else {
        (length, is_partial, end_inclusive)
    };

    let cindex = opened.compression_index().unwrap_or_default();
    let cindex = if cindex.is_empty() {
        None
    } else {
        Some(cindex)
    };

    let header_len = u64::from(opened.header.header_len);
    // 这一行就是「容器内单文件」与「整个文件」的**全部**区别：
    // 把请求的偏移平移到该文件在载荷中的位置
    let payload_start = item.offset.saturating_add(start);

    let data = match omy_core::payload::read_range(
        &opened.header,
        opened.payload_key(),
        cindex.as_deref(),
        payload_start,
        length,
        |payload_off, len| {
            // 与 serve_file 相同的契约：read_range 给的是相对载荷起点的
            // 偏移，取密文时要加回 header_len
            let abs = payload_off.saturating_add(header_len);
            let s = usize::try_from(abs).unwrap_or(usize::MAX);
            let e = usize::try_from(abs.saturating_add(len)).unwrap_or(usize::MAX);
            bytes
                .get(s..e)
                .map(<[u8]>::to_vec)
                .ok_or(omy_core::Error::Truncated {
                    context: "container item ciphertext fetch",
                    need: e,
                    got: bytes.len(),
                })
        },
    ) {
        Ok(d) => d,
        Err(_) => return bare(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, item.mime.clone())
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

/// 按魔数判断图片的 MIME 类型。
///
/// 缩略图的格式由加密时的 `thumb_format` 决定（默认 WebP，也可能是 JPEG），
/// 而这个选择**没有记录在 TLV 里**——所以读取侧只能看字节。
///
/// 认不出来时返回 `application/octet-stream` 而不是猜一个：猜错会让对端
/// 按错误格式解码得到裂图，而 octet-stream 至少能看出是类型问题。
fn sniff_image_mime(b: &[u8]) -> &'static str {
    // WebP 是 "RIFF" + 4 字节长度 + "WEBP"，两段都要看：
    // 只看 RIFF 会把 wav、avi 也认成图片。
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(&b"WEBP"[..]) {
        return "image/webp";
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg";
    }
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "image/png";
    }
    "application/octet-stream"
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
            // 按真实字节判断类型，不能写死。
            //
            // omy-media 的默认 thumb_format 是 **WebP**（同等质量体积更小），
            // 这里原先硬编码 image/jpeg，与实际内容不符。声明错的 MIME
            // 属于「大部分情况下能用」的那类缺陷：WebView 通常会嗅探真实
            // 格式照样显示，于是本地怎么点都正常；一旦遇到严格按
            // Content-Type 解码的一方（部分远端浏览器、缓存代理），
            // 图就变成裂图，而那时很难想到是 MIME 写错了。
            .header(header::CONTENT_TYPE, sniff_image_mime(&bytes))
            .header(header::CONTENT_LENGTH, bytes.len().to_string()),
    )
    .body(bytes)
    .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 返回远端文件正文，支持 Range。
///
/// # 与本地版的唯一区别
///
/// 取密文的闭包：本地从已读入的 `bytes` 里切片，这里发
/// `Request::Read` 去对方那里取。Range 处理、`MAX_SPAN` 截断、
/// 416、CORS、`no-store` 全部相同——**这些行为不能有两套实现**，
/// 否则远端会悄悄退化成「能播但拖不动」。
///
/// # 为什么用 `block_on`
///
/// 协议处理器是同步回调（Tauri 的 `register_asynchronous_uri_scheme_protocol`
/// 给的是同步闭包），而远端读取是 async。这里跑在 Tauri 为协议请求
/// 开的独立线程上，不是 UI 线程，阻塞它不会卡界面。
fn serve_remote_file(
    state: &Arc<AppState>,
    remote: &Arc<RemoteSession>,
    request: &Request<Vec<u8>>,
    id: &str,
) -> Response<Vec<u8>> {
    let Some(f) = remote.file(id) else {
        return bare(StatusCode::NOT_FOUND);
    };
    if !f.unlocked {
        return bare(StatusCode::FORBIDDEN);
    }

    // 头部随 LIST 一起拿到了，解密它不需要任何网络往返
    let Some(opened) = open_remote(state, &f) else {
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

    // 截断对远端更要紧：本地多解密几 MiB 只是费点 CPU，
    // 远端却要真的把这些字节从网线上拉过来
    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    let cindex = opened.compression_index().unwrap_or_default();
    let cindex = if cindex.is_empty() {
        None
    } else {
        Some(cindex)
    };

    let header_len = u64::from(opened.header.header_len);
    let handle = f.handle;
    let sess = Arc::clone(remote);

    let data = match omy_core::payload::read_range(
        &opened.header,
        opened.payload_key(),
        cindex.as_deref(),
        start,
        length,
        |payload_off, len| {
            // 同样要加回 header_len：read_range 给的是相对载荷起点的
            // 偏移，而 READ 请求的 offset 是文件绝对偏移
            let abs = payload_off.saturating_add(header_len);
            let s = Arc::clone(&sess);
            tauri::async_runtime::block_on(async move {
                crate::remote::read_at(&s, handle, abs, len).await
            })
            .map_err(|_| omy_core::Error::Truncated {
                context: "remote ciphertext fetch",
                need: usize::try_from(len).unwrap_or(usize::MAX),
                got: 0,
            })
        },
    ) {
        Ok(d) => d,
        Err(_) => return bare(StatusCode::INTERNAL_SERVER_ERROR),
    };

    let mime = f
        .mime
        .clone()
        .unwrap_or_else(|| String::from("application/octet-stream"));

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, mime)
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

/// 返回远端文件的缩略图。
///
/// 完全不用网络：缩略图在头部 TLV 里，而头部在 LIST 时就拿到了。
/// 所以远端的网格视图和本地一样快。
fn serve_remote_thumb(state: &Arc<AppState>, remote: &Arc<RemoteSession>, id: &str) -> Response<Vec<u8>> {
    let Some(f) = remote.file(id) else {
        return bare(StatusCode::NOT_FOUND);
    };
    if !f.unlocked {
        return bare(StatusCode::FORBIDDEN);
    }
    let Some(opened) = open_remote(state, &f) else {
        return bare(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Ok(bytes) = opened.thumbnail() else {
        return bare(StatusCode::NOT_FOUND);
    };

    with_common_headers(
        Response::builder()
            .status(StatusCode::OK)
            // 与本地 /thumb 同一个理由：按真实字节判断，不能写死。
            // 远端这条路径更要紧——对端可能是任意浏览器，不一定会做嗅探。
            .header(header::CONTENT_TYPE, sniff_image_mime(&bytes))
            .header(header::CONTENT_LENGTH, bytes.len().to_string()),
    )
    .body(bytes)
    .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 用会话密钥打开一个远端文件的头部。
///
/// 与 `open_for` 一样每次重新打开，不缓存 `OpenedFile`——
/// 理由相同：不在内存里长期持有 payload key。
///
/// 头部字节在 LIST 时就随条目一起拿到了，所以这里不产生任何网络往返。
fn open_remote(state: &Arc<AppState>, f: &crate::remote::RemoteFile) -> Option<OpenedFile> {
    let h = omy_core::file::peek_header(&f.header).ok()?;
    let keks: Vec<Kek> = state.with_session(|s| {
        s.all_for(&h.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect()
    })?;
    omy_core::file::open(&f.header, &keks).ok()
}

/// 返回远程存储位置（WebDAV 等）文件正文，支持 Range。
///
/// Range 处理、`MAX_SPAN` 截断、416、CORS、`no-store` 与本地/局域网版
/// **完全一致**——这些行为不能有第三套实现，否则云盘会退化成「能播但拖不动」。
/// 唯一区别是取密文的方式：这里走 [`omy_remote::source::RemoteSource`]，
/// 它内部做 1 MiB 块对齐、密文磁盘缓存与按需 Range 拉取。
fn serve_place_file(
    state: &Arc<AppState>,
    files: &PlaceFiles,
    request: &Request<Vec<u8>>,
    token: &str,
) -> Response<Vec<u8>> {
    let Some(f) = files.get(token) else {
        // 锁定后句柄表被清空，旧 token 一律 404
        return bare(StatusCode::NOT_FOUND);
    };

    // 普通文件（非 omy）：不解密，原样按 Range 转发。
    //
    // 远程位置里普通文件是主体内容（Telegram 频道里的图片、视频、文档），
    // 走 omy 解密路径的话 peek_header 必然失败、一律 403，
    // 表现就是「所有文件都打不开」。
    if let Some(total) = f.plain {
        return serve_plain_remote(&f, request, total);
    }

    let Some(opened) = open_place(state, &f) else {
        // token 还在但当前会话解不开（已锁定 / 换了密码库）：这是凭据问题，
        // 回 403 而不是 500，前端据此引导重新解锁
        return bare(StatusCode::FORBIDDEN);
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

    // 截断对云盘尤其要紧：多解一段就要真的从网线上多拉一段密文
    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 无 Range 的请求同样要设上限（与 serve_plain / serve_container 一致）：
    // 大视频的第一个请求常不带 Range，不截断会把整片从云端拉下、整块解密进内存。
    // 返回前 MAX_SPAN 并如实标 206，播放器会继续要后面的区间。
    let (length, is_partial, end_inclusive) = if !is_partial && length > MAX_SPAN {
        (MAX_SPAN, true, MAX_SPAN.saturating_sub(1))
    } else {
        (length, is_partial, end_inclusive)
    };

    // RemoteSource 实现了 BlockSource，块对齐 / 密文缓存 / Range 拉取都在
    // 它内部；read_source_range 再负责 header 偏移与压缩索引，所以这里
    // 不必像局域网版那样手算 payload 绝对偏移
    let data = match omy_core::source::read_source_range(&f.source, &opened, start, length) {
        Ok(d) => d,
        // 上游 WebDAV 读取失败属于「网关错误」，与本地解密失败区分开
        Err(_) => return bare(StatusCode::BAD_GATEWAY),
    };

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, f.mime.clone())
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

/// 按 Range 转发一个**未加密**的远程文件。
///
/// 与 [`serve_place_file`] 的加密分支共用同一套块缓存（`RemoteSource` 内部），
/// 所以拖动播放不会把同一段重复下载。
///
/// 这里不做任何解密：内容本来就是明文，硬套 omy 的解密路径只会在
/// `peek_header` 处失败，表现为「文件打不开」。
fn serve_plain_remote(
    f: &crate::place_files::OpenPlaceFile,
    request: &Request<Vec<u8>>,
    total: u64,
) -> Response<Vec<u8>> {
    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    let (start, end_inclusive, is_partial) = match range_header {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
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

    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 与加密分支同理：不带 Range 的首个请求也要截断，
    // 否则一个几百 MB 的视频会被整片拉进内存
    let (length, is_partial, end_inclusive) = if !is_partial && length > MAX_SPAN {
        (MAX_SPAN, true, MAX_SPAN.saturating_sub(1))
    } else {
        (length, is_partial, end_inclusive)
    };

    let data = match f.source.read_plain_range(start, length) {
        Ok(d) => d,
        // 上游读取失败属于网关错误，与本地解密失败区分开
        Err(_) => return bare(StatusCode::BAD_GATEWAY),
    };

    let mut b = with_common_headers(
        Response::builder()
            .header(header::CONTENT_TYPE, f.mime.clone())
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

/// 返回远程存储位置文件的缩略图。
///
/// 缩略图在头部 TLV 里，打开时头部已完整读入，所以这里不产生额外网络往返。
fn serve_place_thumb(
    state: &Arc<AppState>,
    thumbs: &PlaceThumbs,
    token: &str,
) -> Response<Vec<u8>> {
    // 缩略图表只登记文件头：列表浏览时已取回完整头部，这里不再发任何网络请求。
    let Some(header) = thumbs.get(token) else {
        return bare(StatusCode::NOT_FOUND);
    };
    let Some(opened) = open_place_header(state, &header) else {
        return bare(StatusCode::FORBIDDEN);
    };
    let Ok(bytes) = opened.thumbnail() else {
        // 没有缩略图是正常情况（非媒体或加密时没生成），前端回退类型图标
        return bare(StatusCode::NOT_FOUND);
    };

    with_common_headers(
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, sniff_image_mime(&bytes))
            .header(header::CONTENT_LENGTH, bytes.len().to_string()),
    )
    .body(bytes)
    .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 用会话密钥打开一个远程存储位置文件的头部。
///
/// 与 `open_for` / `open_remote` 一样每次请求重新打开，不在内存里长期
/// 持有 `OpenedFile` 与 payload key；磁盘缓存里也只有密文。
fn open_place(
    state: &Arc<AppState>,
    f: &crate::place_files::OpenPlaceFile,
) -> Option<OpenedFile> {
    open_place_header(state, &f.header)
}

/// 远程目录容器里的一个条目。
///
/// 与本地 `serve_container_item` 同构，区别只在密文从哪来：这里走
/// `RemoteSource`（块对齐、密文缓存、按需 Range 拉取），本地那条直接读磁盘。
///
/// # 授权不看登记时的状态
///
/// token 背后存的是「所属远程句柄 + 区间」，每次请求都要重新拿那个句柄并用
/// **当前**会话密钥打开。锁定后句柄表被清空，这里自然 404——登记那一刻的
/// 解锁状态不构成后续请求的授权依据。
fn serve_place_container_item(
    state: &Arc<AppState>,
    files: &PlaceFiles,
    containers: &crate::place_files::PlaceContainers,
    request: &Request<Vec<u8>>,
    token: &str,
) -> Response<Vec<u8>> {
    let Some(item) = containers.resolve(token) else {
        return bare(StatusCode::NOT_FOUND);
    };
    let Some(f) = files.get(&item.file_token) else {
        return bare(StatusCode::NOT_FOUND);
    };
    let Some(opened) = open_place(state, &f) else {
        return bare(StatusCode::FORBIDDEN);
    };

    let total = item.size;
    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok());

    let (start, end_inclusive, is_partial) = match range_header {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
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

    let end_inclusive = if is_partial {
        end_inclusive.min(start.saturating_add(MAX_SPAN).saturating_sub(1))
    } else {
        end_inclusive
    };
    let length = end_inclusive.saturating_sub(start).saturating_add(1);
    let (length, is_partial, end_inclusive) = if !is_partial && length > MAX_SPAN {
        (MAX_SPAN, true, MAX_SPAN.saturating_sub(1))
    } else {
        (length, is_partial, end_inclusive)
    };

    // 关键：区间要落在**容器明文里该条目的位置**上。
    // item.offset 是它在容器载荷中的起点，请求里的 start 是相对该条目的偏移，
    // 两者相加才是要读的绝对位置。漏加 item.offset 的话每个条目都会读到容器
    // 开头那一段——表现是「点开任何一个文件看到的都是同一份内容」。
    let abs = item.offset.saturating_add(start);
    let data = match omy_core::source::read_source_range(&f.source, &opened, abs, length) {
        Ok(d) => d,
        Err(_) => return bare(StatusCode::BAD_GATEWAY),
    };

    let mut b = Response::builder()
        .status(if is_partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, item.mime.clone())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, data.len().to_string());
    if is_partial {
        b = b.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end_inclusive}/{total}"),
        );
    }
    with_common_headers(b)
        .body(data)
        .unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 用会话密钥打开一段远程文件头（播放句柄与缩略图句柄共用）。
fn open_place_header(state: &Arc<AppState>, header: &[u8]) -> Option<OpenedFile> {
    let h = omy_core::file::peek_header(header).ok()?;
    let keks: Vec<Kek> = state.with_session(|s| {
        s.all_for(&h.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect()
    })?;
    omy_core::file::open(header, &keks).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_mime_follows_actual_bytes() {
        // 默认缩略图是 WebP。早先这里写死 image/jpeg，本地 WebView 会
        // 自己嗅探真实格式照样显示，所以怎么点都正常；只有严格按
        // Content-Type 解码的一方（部分远端浏览器、缓存代理）才会裂图。
        let webp = {
            let mut v = b"RIFF".to_vec();
            v.extend_from_slice(&1234u32.to_le_bytes());
            v.extend_from_slice(b"WEBPVP8 ");
            v
        };
        assert_eq!(sniff_image_mime(&webp), "image/webp");
        assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]), "image/jpeg");
        assert_eq!(
            sniff_image_mime(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A]),
            "image/png"
        );

        // RIFF 但不是 WebP：wav 也以 RIFF 开头。只看前四字节会把音频
        // 当成图片，于是给出 image/webp 这种明显错误的类型。
        let wav = {
            let mut v = b"RIFF".to_vec();
            v.extend_from_slice(&999u32.to_le_bytes());
            v.extend_from_slice(b"WAVEfmt ");
            v
        };
        assert_eq!(
            sniff_image_mime(&wav),
            "application/octet-stream",
            "RIFF 容器不等于 WebP"
        );

        // 太短的输入不能 panic —— omy-gui 禁用切片索引正是为了这个
        assert_eq!(sniff_image_mime(&[]), "application/octet-stream");
        assert_eq!(sniff_image_mime(b"RIFF"), "application/octet-stream");
    }

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
        assert_eq!(parse_target("/rfile/"), None);
        assert_eq!(parse_target("/rthumb/"), None);
    }

    /// 远端路径必须与本地路径分开解析。
    ///
    /// 若 `rfile/x` 被解析成本地的 `File("x")`，就会拿远端的 id
    /// 去本地列表里查——查不到返回 404，表现为「远端文件点开是空的」，
    /// 而日志里一切正常。
    #[test]
    fn remote_targets_are_distinct() {
        assert_eq!(
            parse_target("/rfile/deadbeef"),
            Some(Target::RemoteFile(String::from("deadbeef")))
        );
        assert_eq!(
            parse_target("/rthumb/deadbeef"),
            Some(Target::RemoteThumb(String::from("deadbeef")))
        );
        // 本地的不能被远端前缀吃掉
        assert_eq!(
            parse_target("/file/deadbeef"),
            Some(Target::File(String::from("deadbeef")))
        );
        assert_eq!(
            parse_target("/thumb/deadbeef"),
            Some(Target::Thumb(String::from("deadbeef")))
        );
        // 同一个 id 在两种前缀下必须解析成不同目标
        assert_ne!(parse_target("/file/x"), parse_target("/rfile/x"));
        assert_ne!(parse_target("/thumb/x"), parse_target("/rthumb/x"));
    }

    /// 容器内文件的路径必须与其他所有前缀分开解析。
    ///
    /// 若 `citem/x` 落到别的分支，就会拿容器条目的 token 去文件表里查——
    /// 查不到返回 404，表现为「容器里的文件双击打不开」，而日志里一切正常。
    #[test]
    fn container_item_target_is_distinct() {
        assert_eq!(
            parse_target("/citem/abcd"),
            Some(Target::ContainerItem(String::from("abcd")))
        );
        assert_eq!(parse_target("/citem/"), None, "空 token 必须拒绝");
        // 不能被任何其他前缀吃掉，也不能吃掉别人
        for other in [
            parse_target("/file/abcd"),
            parse_target("/plain/abcd"),
            parse_target("/rfile/abcd"),
            parse_target("/thumb/abcd"),
        ] {
            assert_ne!(parse_target("/citem/abcd"), other);
        }
    }

    /// 明文路径必须与加密路径分开解析。
    ///
    /// 若 `plain/x` 落到 `File("x")` 分支，就会拿明文 token 去
    /// 加密文件表里查——查不到返回 404，表现为「没加密的图片
    /// 双击打不开」，而日志里一切正常。
    #[test]
    fn plain_target_is_distinct() {
        assert_eq!(
            parse_target("/plain/abc123"),
            Some(Target::Plain(String::from("abc123")))
        );
        assert_eq!(parse_target("/plain/"), None, "空 token 必须拒绝");
        assert_ne!(parse_target("/plain/x"), parse_target("/file/x"));
        assert_ne!(parse_target("/plain/x"), parse_target("/rfile/x"));
        assert_ne!(parse_target("/plain/x"), parse_target("/thumb/x"));
    }

    /// 无 Range 的大文件请求也必须被截断。
    ///
    /// WebView 的第一个请求通常不带 Range。若老实返回整个文件，
    /// 双击一个 4 GB 的视频会把 4 GB 读进内存——加密路径靠
    /// MAX_SPAN 挡住了，明文路径同样需要这条保护。
    #[test]
    fn plain_caps_unranged_large_file() {
        let total: u64 = 4 * 1024 * 1024 * 1024;
        let (start, mut end, mut partial) = (0u64, total - 1, false);
        let mut length = end - start + 1;
        if !partial && length > MAX_SPAN {
            length = MAX_SPAN;
            partial = true;
            end = MAX_SPAN - 1;
        }
        assert!(partial, "超过上限时必须转成 206");
        assert_eq!(length, MAX_SPAN);
        assert_eq!(end, MAX_SPAN - 1);
    }

    /// 小文件不受截断影响，仍然是完整的 200。
    #[test]
    fn plain_small_file_stays_whole() {
        let total: u64 = 1024;
        let (start, end, mut partial) = (0u64, total - 1, false);
        let mut length = end - start + 1;
        if !partial && length > MAX_SPAN {
            length = MAX_SPAN;
            partial = true;
        }
        assert!(!partial, "小文件不该被切成 206");
        assert_eq!(length, total);
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

    /// 容器内每个文件都必须解出**自己**的字节。
    ///
    /// 这是本功能最危险的一处：偏移算错不会报错，只会安静地返回相邻
    /// 文件的内容。所以造一个真的加密容器，逐个文件比对完整内容——
    /// 只测「能读出东西」是不够的，必须测「读出的是对的那个东西」。
    ///
    /// 三个文件的内容故意用不同字节填充且长度不等，任何偏移错位都会
    /// 让断言失败。
    #[test]
    fn container_items_decrypt_to_their_own_bytes() {
        use omy_core::container::{ContainerBuilder, EntryMeta};
        use omy_core::file::{EncryptOptions, RandomMaterial, encrypt as core_encrypt};

        // 三段可区分的内容，长度刻意不同也刻意不是块大小的整数倍
        let files: [(&str, Vec<u8>); 3] = [
            ("a.bin", vec![0xAA; 5000]),
            ("b.bin", vec![0xBB; 137]),
            ("c.bin", vec![0xCC; 9001]),
        ];

        let mut payload = Vec::new();
        let mut b = ContainerBuilder::new(String::from("root"));
        for (name, data) in &files {
            b.add_file(
                vec![String::from(*name)],
                data.len() as u64,
                None,
                EntryMeta::default(),
            )
            .unwrap_or_else(|_| unreachable!("测试数据合法"));
            payload.extend_from_slice(data);
        }
        let index = b
            .finish()
            .unwrap_or_else(|_| unreachable!("测试数据合法"));

        let kek = omy_core::crypto::Kek::from_key(omy_core::crypto::SecretKey::from_bytes(
            [7u8; 32],
        ));
        let opts = EncryptOptions {
            folder_index: Some(index.encode()),
            ..EncryptOptions::default()
        };
        let enc = core_encrypt(
            &payload,
            &[kek.duplicate()],
            &[0u8; 16],
            &opts,
            &RandomMaterial::generate(),
        )
        .unwrap_or_else(|_| unreachable!("加密应当成功"));

        let enc = enc.bytes;
        let opened = omy_core::file::open(&enc, &[kek])
            .unwrap_or_else(|_| unreachable!("刚加密的文件应当能打开"));
        let parsed = opened
            .folder_index()
            .unwrap_or_else(|_| unreachable!("应当是容器"));
        let header_len = u64::from(opened.header.header_len);

        for (name, expected) in &files {
            let entry = parsed
                .find(name)
                .unwrap_or_else(|| unreachable!("索引里应有 {name}"));
            let (off, len) = entry
                .range()
                .unwrap_or_else(|| unreachable!("{name} 应有区间"));
            assert_eq!(len, expected.len() as u64, "{name} 长度不符");

            // 这段偏移换算与 serve_container_item 中的一致：
            // 容器内偏移 + 该文件起点 -> 载荷偏移；取密文时再加 header_len
            let got = omy_core::payload::read_range(
                &opened.header,
                opened.payload_key(),
                None,
                off,
                len,
                |payload_off, n| {
                    let abs = payload_off.saturating_add(header_len);
                    let s = usize::try_from(abs).unwrap_or(usize::MAX);
                    let e = usize::try_from(abs.saturating_add(n)).unwrap_or(usize::MAX);
                    enc.get(s..e)
                        .map(<[u8]>::to_vec)
                        .ok_or(omy_core::Error::Truncated {
                            context: "test fetch",
                            need: e,
                            got: enc.len(),
                        })
                },
            )
            .unwrap_or_else(|_| unreachable!("{name} 应当能读出"));

            assert_eq!(&got, expected, "{name} 读出的字节不是它自己的内容");
        }

        // 再验一次部分读取：容器内文件的 Range 起点要叠加它的偏移。
        // 少加这一层，拖进度条会读到前一个文件的尾巴
        let c = parsed
            .find("c.bin")
            .unwrap_or_else(|| unreachable!("应有 c.bin"));
        let (c_off, _) = c
            .range()
            .unwrap_or_else(|| unreachable!("c.bin 应有区间"));
        let mid = omy_core::payload::read_range(
            &opened.header,
            opened.payload_key(),
            None,
            c_off.saturating_add(1000),
            16,
            |payload_off, n| {
                let abs = payload_off.saturating_add(header_len);
                let s = usize::try_from(abs).unwrap_or(usize::MAX);
                let e = usize::try_from(abs.saturating_add(n)).unwrap_or(usize::MAX);
                enc.get(s..e)
                    .map(<[u8]>::to_vec)
                    .ok_or(omy_core::Error::Truncated {
                        context: "test fetch",
                        need: e,
                        got: enc.len(),
                    })
            },
        )
        .unwrap_or_else(|_| unreachable!("部分读取应当成功"));
        assert_eq!(mid, vec![0xCC; 16], "从中间读也必须落在 c.bin 内");
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
