//! Spike S1 / S5：Tauri v2 自定义协议的 Range 支持与缓存落盘验证。
//!
//! # 为什么必须实测
//!
//! 路线图把这两项标为最高优先级：
//!
//! - **S1**：若自定义协议无法返回 `206 Partial Content` + `Content-Range`，
//!   `<video>` 就无法任意 seek，整个「加密视频直接播放」方案不成立，
//!   必须退回本地 HTTP server（带来端口占用与 CORS 代价）。
//! - **S5**：若 WebView 把响应缓存到磁盘，解密后的明文就落盘了，
//!   这是 L1 泄露，直接违背本项目的核心安全目标。
//!
//! 两者都**不能靠读文档下结论**——WebView2 / WKWebView / WebKitGTK
//! 的实际行为与文档常有差异，且随版本变化。
//!
//! # 本 spike 的关键设计
//!
//! 协议处理器里**真实调用 `omy-core` 解密**，而不是返回明文文件。
//! 只有这样才能验证真实链路：Range 请求 → 计算涉及哪些块 →
//! 只解密这些块 → 返回精确区间。若拿明文文件糊弄，S1 通过了也说明不了
//! 加密路径可行。

use omy_core::crypto::Kek;
use omy_core::file::OpenedFile;
use std::sync::{Arc, Mutex};
use tauri::http::{Request, Response, StatusCode, header};

/// 加载好的加密视频，供协议处理器复用。
struct Vault {
    /// 完整密文（spike 图省事全读进内存；产品应走 `BlockSource`）。
    bytes: Vec<u8>,
    /// 已解包的文件，持有 payload key。
    opened: OpenedFile,
    /// 明文总长度。
    plaintext_len: u64,
    /// MIME 类型，由调用方指定。
    mime: String,
    /// 累计统计，用于最后输出报告。
    stats: Mutex<Stats>,
}

/// 请求统计。这些数字是 S1 结论的证据。
#[derive(Debug, Default, Clone)]
struct Stats {
    /// 总请求数。
    total: u32,
    /// 带 Range 头的请求数。
    with_range: u32,
    /// 返回 206 的次数。
    partial: u32,
    /// 返回 200 的次数。
    full: u32,
    /// 请求过的 Range 起点，用于判断是否真的在 seek。
    range_starts: Vec<u64>,
    /// 累计返回字节数。
    bytes_sent: u64,
    /// 累计解密字节数（含块对齐的浪费部分）。
    bytes_decrypted: u64,
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let omy_path = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("用法: omy-spike-webview <file.omy> <password> [mime]"))?;
    let password = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("缺少密码参数"))?;
    let mime = args.next().unwrap_or_else(|| String::from("video/mp4"));

    let bytes = std::fs::read(&omy_path)?;
    println!("已读入 {} （{} 字节）", omy_path, bytes.len());

    // 用文件头里记录的参数派生 KEK——不能用默认档位
    let h = omy_core::file::peek_header(&bytes)?;
    println!(
        "头部参数: Argon2 m={} KiB t={} p={}, chunk_size={}",
        h.argon2_m_kib, h.argon2_t, h.argon2_p, h.chunk_size
    );
    let kek = Kek::from_password(password.as_bytes(), &h.vault_salt, h.argon2_params())?;
    let opened = omy_core::file::open(&bytes, std::slice::from_ref(&kek))?;
    let plaintext_len = opened.header.plaintext_size;
    println!("解包成功，明文长度 {plaintext_len} 字节，MIME {mime}");

    let vault = Arc::new(Vault {
        bytes,
        opened,
        plaintext_len,
        mime,
        stats: Mutex::new(Stats::default()),
    });
    let vault_for_exit = Arc::clone(&vault);

    tauri::Builder::default()
        // 关键：异步协议。同步版本会阻塞 WebView 线程，
        // 大文件解密时界面会卡死。
        .register_asynchronous_uri_scheme_protocol("omystream", move |_ctx, request, responder| {
            let v = Arc::clone(&vault);
            // 解密可能耗时，必须离开 WebView 线程
            std::thread::spawn(move || {
                let resp = handle(&v, &request);
                responder.respond(resp);
            });
        })
        .setup(|_app| Ok(()))
        .run(tauri::generate_context!())?;

    // 退出时打印统计——这是 S1 的判定依据
    let s = vault_for_exit
        .stats
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();
    print_report(&s);
    Ok(())
}

/// 处理一次协议请求。
fn handle(v: &Vault, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let range_header = request
        .headers()
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);

    {
        // 先记账再处理，确保即使后面出错也留下痕迹
        if let Ok(mut s) = v.stats.lock() {
            s.total = s.total.saturating_add(1);
            if range_header.is_some() {
                s.with_range = s.with_range.saturating_add(1);
            }
        }
    }

    let total = v.plaintext_len;
    let (start, end_inclusive, is_partial) = match range_header.as_deref() {
        Some(h) => match parse_range(h, total) {
            Some(r) => (r.0, r.1, true),
            None => {
                // Range 不合法必须回 416，且带 Content-Range 告知真实长度
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                    .body(Vec::new())
                    .unwrap_or_else(|_| Response::new(Vec::new()));
            }
        },
        None => (0u64, total.saturating_sub(1), false),
    };

    let length = end_inclusive.saturating_sub(start).saturating_add(1);

    // 真实解密：read_range 通过 fetch_ct 闭包按需索取密文区间，
    // 只有涉及的块会被读取和解密。这正是任意 seek 可行的关键：
    // 跳到 90% 位置不需要解密前面 90% 的数据。
    //
    // 顺带统计闭包实际取了多少密文，作为「按需读取」的证据——
    // 若这个数字接近整个文件，说明实现退化成了全量解密。
    let ct_read = std::cell::Cell::new(0u64);
    let data = match omy_core::payload::read_range(
        &v.opened.header,
        v.opened.payload_key(),
        None, // 未压缩，无压缩索引
        start,
        length,
        |off, len| {
            ct_read.set(ct_read.get().saturating_add(len));
            let s = usize::try_from(off).unwrap_or(usize::MAX);
            let e = usize::try_from(off.saturating_add(len)).unwrap_or(usize::MAX);
            v.bytes
                .get(s..e)
                .map(<[u8]>::to_vec)
                .ok_or(omy_core::Error::Truncated {
                    context: "spike ciphertext fetch",
                    need: e,
                    got: v.bytes.len(),
                })
        },
    ) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("解密失败 offset={start} len={length}: {e}");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Vec::new())
                .unwrap_or_else(|_| Response::new(Vec::new()));
        }
    };

    if let Ok(mut s) = v.stats.lock() {
        s.bytes_sent = s.bytes_sent.saturating_add(data.len() as u64);
        s.bytes_decrypted = s.bytes_decrypted.saturating_add(ct_read.get());
        if is_partial {
            s.partial = s.partial.saturating_add(1);
            s.range_starts.push(start);
        } else {
            s.full = s.full.saturating_add(1);
        }
    }

    println!(
        "{} bytes={}-{}/{}  返回 {} 字节（实读密文 {} 字节）",
        if is_partial { "206" } else { "200" },
        start,
        end_inclusive,
        total,
        data.len(),
        ct_read.get()
    );

    let mut b = Response::builder()
        .header(header::CONTENT_TYPE, &v.mime)
        // 必须声明支持 Range，否则部分 WebView 不会发 Range 请求
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, data.len().to_string())
        // S5 的核心对策：禁止任何形式的缓存。
        // 若不加这些头，WebView 可能把解密后的明文写入磁盘缓存。
        .header(header::CACHE_CONTROL, "no-store, no-cache, must-revalidate")
        .header(header::PRAGMA, "no-cache")
        .header("X-Content-Type-Options", "nosniff");

    if is_partial {
        b = b
            .status(StatusCode::PARTIAL_CONTENT)
            .header(
                header::CONTENT_RANGE,
                format!("bytes {start}-{end_inclusive}/{total}"),
            );
    } else {
        b = b.status(StatusCode::OK);
    }

    b.body(data).unwrap_or_else(|_| Response::new(Vec::new()))
}

/// 解析 `Range: bytes=start-end`，返回闭区间 `(start, end)`。
///
/// 只支持单区间——`<video>` 不会发多区间请求。
/// 返回 `None` 表示不可满足，调用方应回 416。
fn parse_range(h: &str, total: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    // 多区间直接拒绝，而不是只取第一个：
    // 静默处理成单区间会让调用方拿到不完整数据却以为成功
    if spec.contains(',') {
        return None;
    }
    let (s, e) = spec.split_once('-')?;
    let s = s.trim();
    let e = e.trim();

    if total == 0 {
        return None;
    }
    let last = total.saturating_sub(1);

    if s.is_empty() {
        // suffix 形式: bytes=-500 表示最后 500 字节
        let n: u64 = e.parse().ok()?;
        if n == 0 {
            return None;
        }
        let start = total.saturating_sub(n.min(total));
        return Some((start, last));
    }

    let start: u64 = s.parse().ok()?;
    if start > last {
        return None; // 超出范围，必须回 416
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

/// 输出 S1 判定报告。
fn print_report(s: &Stats) {
    println!("\n{}", "=".repeat(64));
    println!("Spike S1 结果");
    println!("{}", "=".repeat(64));
    println!("总请求数          {}", s.total);
    println!("带 Range 的请求   {}", s.with_range);
    println!("返回 206          {}", s.partial);
    println!("返回 200          {}", s.full);
    println!("累计发送          {} 字节", s.bytes_sent);
    println!("累计实读密文      {} 字节", s.bytes_decrypted);

    let mut starts = s.range_starts.clone();
    starts.sort_unstable();
    starts.dedup();
    println!("不同的 Range 起点 {} 个", starts.len());
    if !starts.is_empty() {
        let show: Vec<String> = starts.iter().take(12).map(u64::to_string).collect();
        println!("  起点样本        {}", show.join(", "));
    }

    println!();
    // 判定标准写死在代码里，避免事后主观解读
    let seekable = s.with_range >= 2 && s.partial >= 2 && starts.len() >= 2;
    if seekable {
        println!("✅ S1 通过：WebView 发出了多个不同起点的 Range 请求，");
        println!("   且服务端 206 响应被接受 → <video> 可任意 seek。");
        println!("   结论：无需退回本地 HTTP server。");
    } else if s.total == 0 {
        println!("⚠️  未收到任何请求：请在窗口内点击「加载视频」再操作。");
    } else {
        println!("❌ S1 未通过：未观察到足够的 Range 请求。");
        println!("   需检查是否漏了 Accept-Ranges 头，或该平台确实不支持。");
    }
    println!("{}", "=".repeat(64));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        // 常规区间
        assert_eq!(parse_range("bytes=0-499", 10_000), Some((0, 499)));
        assert_eq!(parse_range("bytes=500-999", 10_000), Some((500, 999)));
        // 开放结尾
        assert_eq!(parse_range("bytes=9000-", 10_000), Some((9000, 9999)));
        // suffix 形式
        assert_eq!(parse_range("bytes=-500", 10_000), Some((9500, 9999)));
        // 超过总长的 end 被裁剪
        assert_eq!(parse_range("bytes=0-99999", 10_000), Some((0, 9999)));
        // 空格容错
        assert_eq!(parse_range("bytes= 100 - 200 ", 10_000), Some((100, 200)));
    }

    #[test]
    fn range_rejects_unsatisfiable() {
        // start 超出范围必须 None（→ 416），不能钳到末尾
        assert_eq!(parse_range("bytes=10000-", 10_000), None);
        assert_eq!(parse_range("bytes=20000-30000", 10_000), None);
        // end < start
        assert_eq!(parse_range("bytes=500-100", 10_000), None);
        // 多区间不静默降级
        assert_eq!(parse_range("bytes=0-99,200-299", 10_000), None);
        // 格式错误
        assert_eq!(parse_range("items=0-10", 10_000), None);
        assert_eq!(parse_range("bytes=abc", 10_000), None);
        assert_eq!(parse_range("bytes=", 10_000), None);
        // 零长度文件
        assert_eq!(parse_range("bytes=0-0", 0), None);
        // suffix 为 0 无意义
        assert_eq!(parse_range("bytes=-0", 10_000), None);
    }

    #[test]
    fn single_byte_file() {
        assert_eq!(parse_range("bytes=0-0", 1), Some((0, 0)));
        assert_eq!(parse_range("bytes=0-", 1), Some((0, 0)));
        assert_eq!(parse_range("bytes=1-", 1), None);
    }
}
