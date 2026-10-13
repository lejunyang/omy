//! 真实 TCP 响应容错：正规 dav-server 不会生成畸形范围头，必须在协议边界注入。
use std::time::Duration;

use omy_remote::webdav::{WebDavConfig, WebDavStore};
use omy_remote::{Error, RemoteStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

struct Fixture {
    store: WebDavStore,
    request: oneshot::Receiver<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture(response: String, keep_open: bool) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("随机端口");
    let address = listener.local_addr().expect("地址");
    let (send, request) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("接请求");
        let mut request = Vec::new();
        loop {
            let byte = socket.read_u8().await.expect("请求头");
            request.push(byte);
            if request.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let _ = send.send(String::from_utf8(request).expect("HTTP 头"));
        // 客户端可在读头后立即拒绝，服务端写失败也是预期的取消行为。
        let _ = socket.write_all(response.as_bytes()).await;
        if keep_open {
            std::future::pending::<()>().await;
        }
    });
    let store = WebDavStore::new(WebDavConfig {
        base_url: format!("http://{address}/"),
        username: "u".into(),
        password: "p".into(),
        writable: true,
        timeout: Duration::from_secs(3),
        ..WebDavConfig::default()
    })
    .expect("客户端");
    Fixture {
        store,
        request,
        task,
    }
}

fn response(status: u16, headers: &str, body: &str) -> String {
    format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n{body}")
}

/// 不这样会怎样：错误偏移、伪造长度或截断响应会被当成有效密文并长期缓存。
#[tokio::test]
async fn partial_ranges_validate_headers_and_body() {
    for headers in [
        "Content-Length: 3\r\n",
        "Content-Range: bytes 1-3/10\r\nContent-Length: 3\r\n",
        "Content-Range: bytes 2-5/10\r\n",
        "Content-Range: bytes 2-1/10\r\n",
        "Content-Range: bytes 2-4/4\r\n",
        "Content-Range: bytes 2-3/10\r\n",
        "Content-Range: items 2-4/10\r\n",
        "Content-Range: bytes +2-4/10\r\n",
        "Content-Range: bytes 2-4/18446744073709551616\r\n",
        "Content-Range: bytes 2-4/10\r\nContent-Length: 4\r\n",
        "Content-Range: bytes 2-4/10\r\nContent-Encoding: gzip\r\n",
    ] {
        let f = fixture(response(206, headers, "cde"), false).await;
        assert!(
            matches!(
                f.store.read_range("/f", 2, 3).await,
                Err(Error::Protocol(_))
            ),
            "畸形 206 必须拒绝：{headers}"
        );
    }
    for body in ["cd", "cdef"] {
        let f = fixture(
            response(206, "Content-Range: bytes 2-4/10\r\n", body),
            false,
        )
        .await;
        assert!(
            matches!(
                f.store.read_range("/f", 2, 3).await,
                Err(Error::Protocol(_))
            ),
            "实际长度不符必须拒绝：{body}"
        );
    }
    let f = fixture(
        response(
            206,
            "Content-Range: bytes 2-4/10\r\nContent-Length: 3\r\n",
            "cd",
        ),
        false,
    )
    .await;
    assert!(
        matches!(f.store.read_range("/f", 2, 3).await, Err(Error::Network(_))),
        "Content-Length 尚未读满就断线必须报告网络失败"
    );
}

/// 不这样会怎样：文件尾短读被误报损坏，或超过 u32 的偏移被截断成文件开头。
#[tokio::test]
async fn valid_ranges_include_eof_and_large_offsets() {
    for (header, offset, len, body) in [
        ("bytes 2-4/10", 2, 3, "cde"),
        ("bytes 8-9/10", 8, 20, "ij"),
        ("bytes 2-4/*", 2, 3, "cde"),
        (
            "bytes 8589934592-8589934594/1099511627776",
            8_589_934_592,
            3,
            "xyz",
        ),
        (
            "bytes 18446744073709551615-18446744073709551615/*",
            u64::MAX,
            1,
            "z",
        ),
    ] {
        let f = fixture(
            response(
                206,
                &format!(
                    "Content-Range: {header}\r\nContent-Length: {}\r\n",
                    body.len()
                ),
                body,
            ),
            false,
        )
        .await;
        assert_eq!(
            f.store
                .read_range("/f", offset, len)
                .await
                .expect("合法范围"),
            body.as_bytes(),
            "只能返回请求偏移的字节"
        );
    }
}

/// 不这样会怎样：忽略 Range 的 TB 级文件会被全量缓冲，或者 seek 扫过全部前缀。
#[tokio::test]
async fn ignored_range_is_bounded_even_without_eof() {
    for headers in ["Content-Length: 1099511627776\r\n", ""] {
        let f = fixture(response(200, headers, "abcdefgh"), true).await;
        let got = tokio::time::timeout(Duration::from_secs(1), f.store.read_range("/f", 0, 3))
            .await
            .expect("前缀不能等待整文件 EOF")
            .expect("有界前缀");
        assert_eq!(got, b"abc", "不能返回超出请求的字节");
        let f = fixture(response(200, headers, ""), true).await;
        let got = tokio::time::timeout(
            Duration::from_secs(1),
            f.store.read_range("/f", 8_589_934_592, 3),
        )
        .await
        .expect("非零偏移不能消费响应体");
        assert!(
            matches!(got, Err(Error::Unsupported("range_read"))),
            "随机读取无 Range 的服务端必须明确报不支持"
        );
    }
    let f = fixture(response(200, "Content-Length: 2\r\n", "ab"), false).await;
    assert_eq!(
        f.store.read_range("/f", 0, 3).await.expect("文件尾"),
        b"ab",
        "完整文件小于请求时允许短读"
    );
    for (status, headers) in [(200, "Content-Range: bytes 0-2/3\r\n"), (204, "")] {
        let f = fixture(response(status, headers, ""), false).await;
        assert!(
            matches!(
                f.store.read_range("/f", 0, 3).await,
                Err(Error::Protocol(_))
            ),
            "不能把其它成功状态当有效区间"
        );
    }
}

/// 不这样会怎样：鉴权与限流不能触发正确处理，中文特殊字符路径会读到其它对象。
#[tokio::test]
async fn request_encoding_and_error_classification() {
    let mut f = fixture(
        response(
            206,
            "Content-Range: bytes 2-4/10\r\nContent-Length: 3\r\n",
            "cde",
        ),
        false,
    )
    .await;
    f.store
        .read_range("/影视/中文 #%.omy", 2, 3)
        .await
        .expect("中文路径");
    let request = (&mut f.request).await.expect("请求留痕");
    assert!(
        request.starts_with("GET /%E5%BD%B1%E8%A7%86/%E4%B8%AD%E6%96%87%20%23%25.omy HTTP/1.1"),
        "中文和保留字符应只编码一次：{request}"
    );
    let lower = request.to_ascii_lowercase();
    assert!(lower.contains("range: bytes=2-4\r\n"), "必须发送精确范围");
    assert!(
        lower.contains("accept-encoding: identity\r\n"),
        "压缩会破坏字节偏移"
    );
    assert!(
        request.contains("authorization: Basic dTpw\r\n"),
        "Range GET 也需要认证"
    );
    for status in [401, 403, 404, 429, 416] {
        let f = fixture(response(status, "Content-Length: 0\r\n", ""), false).await;
        let error = f.store.read_range("/f", 2, 3).await.expect_err("错误状态");
        assert!(
            matches!(
                (status, error),
                (401, Error::Unauthorized)
                    | (403, Error::Forbidden)
                    | (404, Error::NotFound(_))
                    | (429, Error::RateLimited)
                    | (416, Error::Protocol(_))
            ),
            "状态分类必须准确：{status}"
        );
    }
}

/// 不这样会怎样：溢出 Range 被饱和成另一个区间；零长度请求仍触发网络。
#[tokio::test]
async fn empty_and_overflow_ranges_do_not_send_requests() {
    let store = WebDavStore::new(WebDavConfig {
        base_url: "http://127.0.0.1:1/".into(),
        ..WebDavConfig::default()
    })
    .expect("构造");
    assert!(
        store
            .read_range("/f", u64::MAX, 0)
            .await
            .expect("不应联网")
            .is_empty(),
        "空范围无需网络"
    );
    assert!(
        matches!(
            store.read_range("/f", u64::MAX, 2).await,
            Err(Error::Protocol(_))
        ),
        "溢出必须在发请求前拒绝"
    );
    assert!(
        matches!(
            store.read_range("/f", 0, u64::MAX).await,
            Err(Error::Protocol(_))
        ),
        "不可表示的缓冲长度必须提前拒绝"
    );
}
