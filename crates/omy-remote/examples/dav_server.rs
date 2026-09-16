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

use std::convert::Infallible;
use std::net::SocketAddr;

use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
use http::Request;
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
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req: Request<Incoming>| {
                let handler = handler.clone();
                async move { Ok::<_, Infallible>(handler.handle(req).await) }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                eprintln!("连接处理失败: {e}");
            }
        });
    }
}
