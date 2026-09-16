//! 开发/自测用：把一个本地目录通过 WebDAV 暴露在 127.0.0.1，
//! 供 GUI「远程位置」端到端验证（浏览 / 点播 / seek / 密文缓存）。
//!
//! 这不是产品代码，不进发布物；真实链路的断言在 `tests/webdav_server.rs`，
//! 这个 example 只负责在本机起一个人手/探针可连的服务器。
//!
//! 用法：
//! ```text
//! cargo run -p omy-remote --example dav_server -- <root目录> [端口]
//! ```
//! 不带认证，只监听回环地址——测试夹具不含真实凭据，也不该暴露到局域网。
//!
//! # 仅供自测的故障/延迟注入钩子
//!
//! 真实网络会抖动，端到端测试却需要**确定性**地复现「读取失败」与「识别中」。
//! 因此在被服务目录里放两个标记文件即可触发（只影响 GET/HEAD 读正文/头部，
//! 不影响 PROPFIND 列目录，所以条目依旧会出现）：
//!
//! - `<文件>.failmarker` 存在：把该文件的读请求改写到一个不存在的路径，
//!   服务端返回 404，前端应把它标成「未能读取」；删掉标记后点击重试应恢复。
//! - `<文件>.slowmarker` 存在：读该文件前延迟，marker 内容是毫秒数（缺省 1500），
//!   用来让「边扫边出」的识别中骨架稳定停留、可被断言/截图。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
use http::uri::{PathAndQuery, Uri};
use http::{Method, Request};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = args
        .get(1)
        .cloned()
        .expect("用法: dav_server <root目录> [端口]");
    let port: u16 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(8799);

    // 参数与 tests/webdav_server.rs 保持一致：不做 case 检查、不隐藏点文件，
    // 免得测试里能列到的文件这里列不到
    let fs = LocalFs::new(&root, false, false, false);
    let handler = DavHandler::builder()
        .filesystem(fs)
        .locksystem(FakeLs::new())
        .build_handler();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
        .await
        .expect("绑定端口");
    eprintln!("WebDAV 正在服务 {root} → http://127.0.0.1:{port}/ （Ctrl-C 退出）");

    loop {
        let (stream, _) = listener.accept().await.expect("accept");
        let handler = handler.clone();
        let root_for_conn = root.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |mut req: Request<Incoming>| {
                let handler = handler.clone();
                let root = root_for_conn.clone();
                async move {
                    maybe_inject(&mut req, &root).await;
                    Ok::<_, Infallible>(handler.handle(req).await)
                }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                eprintln!("连接处理失败: {e}");
            }
        });
    }
}

/// 按 root 下的标记文件对读请求做故障/延迟注入。只处理 GET/HEAD。
async fn maybe_inject(req: &mut Request<Incoming>, root: &str) {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return;
    }
    // 测试夹具文件名都用 ASCII，这里不做百分号解码；trim 掉前导 '/' 得相对路径
    let rel = req.uri().path().trim_start_matches('/');
    if rel.is_empty() {
        return;
    }

    let fail_marker = PathBuf::from(root).join(format!("{rel}.failmarker"));
    if tokio::fs::try_exists(&fail_marker).await.unwrap_or(false) {
        // 不改状态码、不另造响应体（那要和 DavHandler 的 body 类型对齐）：
        // 直接把 URI 改写到一个必然不存在的路径，交给 handler 回 404，
        // 前端读取失败 → probe_failed，与真实网络抖动走同一条路径。
        if let Ok(pq) = PathAndQuery::try_from(format!("/__omy_injected_fail__/{rel}")) {
            let mut parts = std::mem::take(req.uri_mut()).into_parts();
            parts.path_and_query = Some(pq);
            if let Ok(uri) = Uri::from_parts(parts) {
                *req.uri_mut() = uri;
            }
        }
        return;
    }

    let slow_marker = PathBuf::from(root).join(format!("{rel}.slowmarker"));
    if tokio::fs::try_exists(&slow_marker).await.unwrap_or(false) {
        let millis = tokio::fs::read_to_string(&slow_marker)
            .await
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(1500);
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }
}
