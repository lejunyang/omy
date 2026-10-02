//! 真实 WebDAV 服务器端到端测试。
//!
//! # 为什么不用 mock
//!
//! `webdav.rs` 里的单元测试把网络挡在外面（只读拒绝写、路径编码、base64），
//! 但它们回答不了最关键的问题：**我们发的 `PROPFIND` / `Range GET` / `PUT` /
//! `MOVE`，一个真实的服务端到底认不认？返回的 XML / 206 / 状态码我们又解对了没？**
//! 这些只有对着真服务器跑一遍才算数。
//!
//! 这里用 `dav-server`（`webdav-handler` 的活跃 fork，基于 hyper 1.x，
//! 且带 `localfs_windows.rs`——原作者的 0.2 版在 Windows 因 `ino()` 编译失败）
//! 在随机端口起一个把本地临时目录映射成 WebDAV 的服务器，外层再包一圈
//! service：一边做 Basic 认证，一边把每个请求的方法、路径、`Range` 头记下来。
//!
//! 记录请求是验证「seek 不放大」的唯一可靠手段：直接跳到文件尾部时，
//! 断言服务端**从未收到过**针对前部密文块的 Range，而不是只看解密结果对不对
//! （结果对也可能是先偷偷下了整个文件）。

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
use http::{header, Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use omy_core::crypto::{Argon2Params, Kek};
use omy_core::file::{encrypt, open, EncryptOptions, RandomMaterial};
use omy_core::source::read_source_range;
use omy_remote::cache::BlockCache;
use omy_remote::source::RemoteSource;
use omy_remote::store::RemoteStore;
use omy_remote::webdav::{Vendor, WebDavConfig, WebDavStore};
use omy_remote::{
    DecryptStreamRequest, commit_upload, decrypt_stream_to_local, ensure_remote_dir,
    ensure_safe_remote,
};
use omy_remote::Error as RemoteError;
use tokio::net::TcpListener;

/// 一次 HTTP 请求的留痕。
#[derive(Clone, Debug)]
struct Rec {
    method: String,
    path: String,
    range: Option<String>,
}

/// 测试服务器句柄。drop 前应调 [`Server::shutdown`] 清理临时目录。
struct Server {
    port: u16,
    root: PathBuf,
    records: Arc<Mutex<Vec<Rec>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    fn snapshot(&self) -> Vec<Rec> {
        self.records.lock().expect("记录锁").clone()
    }

    /// GET 请求里 `Range: bytes=a-b` 的起点 `a`，用于判断到底拉了哪些字节。
    fn get_starts_after(&self, split: usize) -> Vec<u64> {
        self.records
            .lock()
            .expect("记录锁")
            .iter()
            .skip(split)
            .filter(|r| r.method == "GET")
            .filter_map(|r| r.range.as_ref())
            .filter_map(|hv| {
                let rest = hv.strip_prefix("bytes=")?;
                let start = rest.split('-').next()?;
                start.trim().parse::<u64>().ok()
            })
            .collect()
    }

    async fn shutdown(self) {
        self.task.abort();
        // 给 accept 循环一点时间真正退出、关闭文件句柄，否则 Windows 上
        // 紧接着删目录会报「目录非空/正在使用」
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// 在随机端口起一个把 `root` 映射为 WebDAV 根的服务器。
///
/// `auth = Some((user, pass))` 时整个站点要求 Basic 认证。
async fn spawn_server(root: PathBuf, auth: Option<(&str, &str)>) -> Server {
    let fs = LocalFs::new(&root, false, false, false);
    let handler = DavHandler::builder()
        .filesystem(fs)
        .locksystem(FakeLs::new())
        .build_handler();

    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("绑定随机端口");
    let port = listener.local_addr().expect("本地地址").port();

    let records = Arc::new(Mutex::new(Vec::<Rec>::new()));
    let records_for_task = Arc::clone(&records);
    let auth = auth.map(|(u, p)| (u.to_owned(), p.to_owned()));

    let task = tokio::spawn(async move {
        loop {
            // 服务器被 abort 时 accept 返回错误，循环随之结束
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let handler = handler.clone();
            let records = Arc::clone(&records_for_task);
            let auth = auth.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let svc = service_fn(move |req: Request<Incoming>| {
                    let handler = handler.clone();
                    let records = Arc::clone(&records);
                    let auth = auth.clone();
                    async move {
                        // 先留痕，再决定是否放行——认证失败的请求也要能在测试里看到
                        records.lock().expect("记录锁").push(Rec {
                            method: req.method().as_str().to_owned(),
                            path: req.uri().path().to_owned(),
                            range: req
                                .headers()
                                .get(header::RANGE)
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned),
                        });

                        // Basic 认证：直接比对期望的 Authorization 头
                        if let Some((u, p)) = &auth {
                            let expected = format!("Basic {}", b64_encode(&format!("{u}:{p}")));
                            let provided = req
                                .headers()
                                .get(header::AUTHORIZATION)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or("");
                            if provided != expected {
                                let resp = Response::builder()
                                    .status(StatusCode::UNAUTHORIZED)
                                    .header(header::WWW_AUTHENTICATE, "Basic realm=\"omy-test\"")
                                    .body(dav_body("auth required"))
                                    .expect("401 响应可构造");
                                return Ok::<_, Infallible>(resp);
                            }
                        }
                        Ok(handler.handle(req).await)
                    }
                });
                if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                    eprintln!("测试服务器连接处理失败: {e}");
                }
            });
        }
    });

    Server {
        port,
        root,
        records,
        task,
    }
}

/// 构造 `dav_server::body::Body`。包一层免得在测试里反复写类型转换。
fn dav_body(s: &'static str) -> dav_server::body::Body {
    s.to_owned().into()
}

/// 最小 base64 编码，仅用于在测试服务器端算期望的 Basic 头。
fn b64_encode(input: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = input.as_bytes();
    let mut out = String::new();
    for c in input.chunks(3) {
        let b0 = u32::from(c[0]);
        let b1 = c.get(1).map_or(0, |&b| u32::from(b));
        let b2 = c.get(2).map_or(0, |&b| u32::from(b));
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if c.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 每次测试独立的临时目录。
fn unique_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("omy_webdav_{tag}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

/// 用弱 KDF（测试速度）加密 `plain`，返回完整 `.omy` 字节与对应 KEK。
fn make_omy(plain: &[u8], password: &[u8], name: Option<&str>) -> (Vec<u8>, Kek) {
    let salt = [0x42u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(password, &salt, params).expect("派生 KEK");
    let opts = EncryptOptions {
        filename: name.map(str::to_owned),
        argon2: params,
        ..EncryptOptions::default()
    };
    // encrypt 只借用 KEK。`&[kek]` 数组字面量会把 kek move 进临时数组，
    // 这里要在调用后继续返回 kek，故用 from_ref 得到纯借用的 &[Kek]
    let enc = encrypt(
        plain,
        std::slice::from_ref(&kek),
        &salt,
        &opts,
        &RandomMaterial::generate(),
    )
    .expect("加密");
    (enc.bytes, kek)
}

/// 连到测试服务器的客户端。
fn store_for(base: &str, user: &str, pass: &str, writable: bool) -> WebDavStore {
    WebDavStore::new(WebDavConfig {
        base_url: base.to_owned(),
        username: user.to_owned(),
        password: pass.to_owned(),
        vendor: Vendor::Generic,
        writable,
        ..WebDavConfig::default()
    })
    .expect("构造 WebDavStore")
}

/// 列目录并按名字找条目。
async fn find_entry(store: &WebDavStore, dir: &str, name: &str) -> omy_remote::Entry {
    let items = store.list(dir).await.expect("列目录");
    items
        .into_iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("根目录应列出 {name}"))
}

/// 核心链路：列目录 → Range 读 → 跳到尾部只读尾部 → 解密逐字节一致 →
/// 证明没有从头下载 → 二次读命中缓存零请求。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_seek_range_and_cache() {
    let root = unique_dir("seek");

    // 3.5 MiB 确定性、不可压缩的数据，确保横跨多个 1 MiB 远程缓存块
    let plain: Vec<u8> = (0..(3 * 1024 * 1024 + 512 * 1024))
        .map(|i| (i % 251) as u8)
        .collect();
    let (movie_bytes, kek) = make_omy(&plain, b"movie-pw", Some("movie.mp4"));
    std::fs::write(root.join("movie.omy"), &movie_bytes).expect("写 movie.omy");

    // 子目录 + 中文名带空格的文件，验证 PROPFIND 解析与路径编码往返
    std::fs::create_dir_all(root.join("影视")).expect("建中文目录");
    let (cn_bytes, _cn_kek) = make_omy(&vec![0x7Au8; 4096], b"pw", Some("大片 2.mkv"));
    std::fs::write(root.join("影视").join("大片 2.mkv.omy"), &cn_bytes).expect("写中文名文件");
    std::fs::create_dir_all(root.join("sub")).expect("建 sub 目录");
    let (note_bytes, _) = make_omy(b"secret note content".repeat(40).as_slice(), b"pw", None);
    std::fs::write(root.join("sub").join("notes.omy"), &note_bytes).expect("写 notes");
    std::fs::write(root.join("plain.txt"), b"not encrypted").expect("写普通文件");

    let server = spawn_server(root.clone(), None).await;
    let base = server.base_url();
    let store = Arc::new(store_for(&base, "", "", false));

    // 1) PROPFIND 根目录：四类条目都要在，目录在前，且不含「目录自身」
    let root_items = store.list("").await.expect("列根目录");
    let names: Vec<&str> = root_items.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"movie.omy"), "根列出 movie.omy: {names:?}");
    assert!(names.contains(&"plain.txt"), "根列出 plain.txt: {names:?}");
    assert!(names.contains(&"sub"), "根列出 sub: {names:?}");
    assert!(names.contains(&"影视"), "根列出中文目录: {names:?}");
    assert!(!names.iter().any(|n| n.is_empty()), "不能有空名/自身条目");
    assert!(
        root_items.iter().all(|e| e.id != "/" && !e.id.is_empty()),
        "不能把根自身列出来"
    );
    // 目录排在文件前
    let first_file = root_items.iter().position(|e| !e.is_dir);
    let last_dir = root_items.iter().rposition(|e| e.is_dir);
    if let (Some(ff), Some(ld)) = (first_file, last_dir) {
        assert!(ld < ff, "目录应排在文件之前");
    }

    // movie.omy 的总长度来自 PROPFIND 的 getcontentlength
    let movie = find_entry(&store, "", "movie.omy").await;
    assert!(!movie.is_dir);
    assert_eq!(
        movie.size,
        Some(movie_bytes.len() as u64),
        "content-length 要准"
    );

    // 中文名 + 空格文件：进中文目录、Range 读前 50 字节必须成功
    let cn = find_entry(&store, "", "影视").await;
    assert!(cn.is_dir);
    let cn_movie = find_entry(&store, &cn.id, "大片 2.mkv.omy").await;
    let head_cn = store
        .read_range(&cn_movie.id, 0, 50)
        .await
        .expect("读中文名文件头部");
    assert_eq!(head_cn.len(), 50, "Range 读长度要精确");

    // 2) 取头部（打开文件只需要头部门票，不下载载荷）
    const HEAD_FETCH: u64 = 4096;
    let head = store
        .read_range(&movie.id, 0, HEAD_FETCH)
        .await
        .expect("读头部");
    let parsed = omy_core::file::peek_header(&head).expect("解析头部");
    let payload_start = u64::from(parsed.header_len);
    assert!(
        u64::from(parsed.header_len) <= HEAD_FETCH,
        "本测试的头部抓取窗口要覆盖完整头部（真实产品打开时需按 header_len+MAC 补读）"
    );

    // 缓存目录放在服务器根之外的临时位置
    let cache_root = root.join("cache");
    let cache = BlockCache::new(&cache_root, 10 * 1024 * 1024).expect("建缓存");
    // 缓存句柄要 move 进阻塞线程，这里留一份廉价克隆（仅含路径与上限）用于断言
    let cache_out = cache.clone();
    let rt = tokio::runtime::Handle::current();

    // 记录「打开来源」之前的请求数，之后的 GET 都是 seek 触发的载荷请求
    let split = server.snapshot().len();

    // 偏移在闭包外算好（Copy 进闭包），明文本体留在闭包外做逐字节断言
    const TAIL: u64 = 64 * 1024;
    const MID_LEN: u64 = 10_000;
    let off_tail = plain.len() as u64 - TAIL;
    let off_mid = plain.len() as u64 - 200_000;

    let store2 = Arc::clone(&store);
    let id = movie.id.clone();
    let place = base.clone();
    let total = movie_bytes.len() as u64;
    let head_for_blocking = head.clone();
    let records = Arc::clone(&server.records);
    let outcome = tokio::task::spawn_blocking(
        move || -> Result<(Vec<u8>, Vec<u8>, usize, usize), String> {
            let src = RemoteSource::new(
                store2,
                place,
                id,
                &head_for_blocking,
                total,
                Some(cache),
                rt,
            )
            .map_err(|e| format!("RemoteSource: {e}"))?;
            let opened = open(&head_for_blocking, &[kek]).map_err(|e| format!("open: {e}"))?;

            // 直接跳到「片尾」：读明文最后 64 KiB
            let got_tail = read_source_range(&src, &opened, off_tail, TAIL)
                .map_err(|e| format!("读尾部: {e}"))?;

            let n_after_first = records.lock().map_err(|e| e.to_string())?.len();

            // 再读尾部更靠前一点、但仍落在最后一个 1 MiB 块内的 10 KiB——
            // 这段应当**完全命中缓存**，不产生任何新的网络请求
            let got_mid = read_source_range(&src, &opened, off_mid, MID_LEN)
                .map_err(|e| format!("读缓存段: {e}"))?;

            let n_after_second = records.lock().map_err(|e| e.to_string())?.len();
            Ok((got_tail, got_mid, n_after_first, n_after_second))
        },
    )
    .await
    .expect("spawn_blocking 完成")
    .expect("远程读取成功");

    let (got_tail, got_mid, n_after_first, n_after_second) = outcome;

    // 3) 解密结果必须与原始明文逐字节一致
    let tail_us = TAIL as usize;
    assert_eq!(
        got_tail,
        plain[plain.len() - tail_us..],
        "片尾解密逐字节一致"
    );
    let off_mid_us = off_mid as usize;
    assert_eq!(
        got_mid,
        plain[off_mid_us..off_mid_us + MID_LEN as usize],
        "缓存段解密一致"
    );

    // 4) seek 不放大：打开来源之后的所有载荷 GET，起点都不得早于「载荷起点 + 2 MiB」。
    //    即跳到片尾时，前两个 1 MiB 密文块（约等于片头两分钟）根本没被请求过。
    let starts = server.get_starts_after(split);
    assert!(!starts.is_empty(), "seek 至少应触发一次载荷 GET");
    let floor = payload_start + 2 * 1024 * 1024;
    for s in &starts {
        assert!(
            *s >= floor,
            "跳到片尾却请求了前部密文（起点 {s} < {floor}），seek 放大了：{starts:?}"
        );
    }

    // 5) 缓存命中：第二次读完全落在已下载的最后一块内，不应新增任何请求
    assert_eq!(
        n_after_first, n_after_second,
        "同一块内的二次读应命中缓存、零新增请求（{n_after_first} -> {n_after_second}）"
    );

    // 6) 密文块确实落盘，且占用量为正
    assert!(cache_out.used() > 0, "缓存目录应有密文块");

    server.shutdown().await;
}

/// 复现真实视频：开启压缩但内容本身已压缩（高熵、压不进去），且不足一个 1 MiB 块。
///
/// 不这样会怎样：早期端到端用 `i % 251` 充当「不可压缩」数据，但它是周期序列、
/// 实际高度可压缩，走的是整块压缩分支，给了虚假信心——25 项断言全绿却播不了
/// 真实 H.264。真实视频压不进去，会落到「逐块判定不可压缩」的分支。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn small_incompressible_compressed_read() {
    let root = unique_dir("incompress");

    // xorshift 伪随机：高熵且确定性，zstd 基本压不动
    let n = 84_755usize;
    let mut plain = Vec::with_capacity(n);
    let mut x = 0x1234_5678u32;
    for _ in 0..n {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        plain.push((x & 0xff) as u8);
    }

    let salt = [0x44u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(b"pw", &salt, params).expect("KEK");
    let opts = EncryptOptions {
        filename: Some(String::from("v.mp4")),
        argon2: params,
        compress: true,
        ..EncryptOptions::default()
    };
    let enc = encrypt(
        &plain,
        std::slice::from_ref(&kek),
        &salt,
        &opts,
        &RandomMaterial::generate(),
    )
    .expect("加密");
    let bytes = enc.bytes;
    std::fs::write(root.join("v.mp4.omy"), &bytes).expect("写盘");

    let server = spawn_server(root.clone(), None).await;
    let base = server.base_url();
    let store = Arc::new(store_for(&base, "", "", false));
    let e = find_entry(&store, "", "v.mp4.omy").await;
    assert_eq!(e.size, Some(bytes.len() as u64), "content-length 要准");

    let total = bytes.len() as u64;
    let want_len = plain.len() as u64;
    let rt = tokio::runtime::Handle::current();
    let store2 = Arc::clone(&store);
    let id = e.id.clone();
    let place = base.clone();
    let head = bytes.clone(); // peek_header 只需前缀；给完整字节等价于「头部已完整补读」
    let got = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let src = RemoteSource::new(store2, place, id, &head, total, None, rt)
            .map_err(|e| format!("RemoteSource: {e}"))?;
        let opened = open(&head, &[kek]).map_err(|e| format!("open: {e}"))?;
        read_source_range(&src, &opened, 0, want_len).map_err(|e| format!("read: {e}"))
    })
    .await
    .expect("spawn_blocking 完成")
    .expect("远程读取成功");

    assert_eq!(got.len(), plain.len(), "长度不符");
    assert_eq!(got, plain, "高熵小文件远程解密必须逐字节一致");
    server.shutdown().await;
}

/// 可写位置的完整往返：建目录 → 上传 → 改名 → 再传 → 删除；
/// 只读位置必须在发出任何 HTTP 请求之前拒绝全部写操作。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writable_roundtrip_and_readonly_refuses() {
    let root = unique_dir("write");
    let server = spawn_server(root.clone(), None).await;
    let base = server.base_url();

    let w = Arc::new(store_for(&base, "", "", true));
    assert!(w.capabilities().any_write());

    // MKCOL
    w.create_dir("", "movies").await.expect("建目录");
    assert!(
        root.join("movies").is_dir(),
        "服务器磁盘上应出现 movies 目录"
    );

    // PUT
    let payload = b"ciphertext-blob".to_vec();
    let e = w.write("/movies", "a.omy", &payload).await.expect("上传");
    assert_eq!(e.name, "a.omy");
    let on_disk = std::fs::read(root.join("movies").join("a.omy")).expect("读回上传文件");
    assert_eq!(on_disk, payload, "上传内容要逐字节落盘");

    // 列目录能看到刚上传的文件
    let listed = w.list("/movies").await.expect("列 movies");
    assert!(listed.iter().any(|e| e.name == "a.omy"), "列出刚上传的文件");

    // MOVE 改名
    w.rename("/movies/a.omy", "b.omy").await.expect("改名");
    assert!(!root.join("movies").join("a.omy").exists(), "旧名应消失");
    assert!(root.join("movies").join("b.omy").exists(), "新名应存在");

    // DELETE
    w.delete("/movies/b.omy").await.expect("删除");
    assert!(
        !root.join("movies").join("b.omy").exists(),
        "删除后磁盘上应消失"
    );

    drop(w);

    // 只读客户端：四个写操作都要在发请求前拒绝
    let ro = Arc::new(store_for(&base, "", "", false));
    assert!(!ro.capabilities().any_write());
    let before = server.snapshot().len();
    assert!(matches!(
        ro.create_dir("", "x").await,
        Err(RemoteError::Unsupported("create_dir"))
    ));
    assert!(matches!(
        ro.write("/", "x", b"y").await,
        Err(RemoteError::Unsupported("write"))
    ));
    assert!(matches!(
        ro.rename("/movies", "z").await,
        Err(RemoteError::Unsupported("rename"))
    ));
    assert!(matches!(
        ro.delete("/movies").await,
        Err(RemoteError::Unsupported("delete"))
    ));
    let after = server.snapshot().len();
    assert_eq!(
        before, after,
        "只读拒绝必须发生在发请求之前，服务器不应收到任何写动词"
    );
    // 只读拒绝不能真的改动磁盘
    assert!(!root.join("x").exists(), "被拒绝的建目录不能落盘");

    server.shutdown().await;
}

/// 外部编辑必须按打开时的远端版本条件覆盖；另一客户端先改过时，旧基线应返回
/// Conflict，且不能把对方内容静默覆盖掉。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conditional_replace_detects_remote_conflict() {
    let root = unique_dir("conditional_replace");
    std::fs::write(root.join("note.txt"), b"base").expect("写初始文件");
    let server = spawn_server(root.clone(), None).await;
    let store = store_for(&server.base_url(), "", "", true);

    let (initial, baseline) = store
        .read_with_revision("/note.txt")
        .await
        .expect("同时读取初始内容与版本");
    assert_eq!(initial, b"base", "下载内容必须来自该版本响应");
    let baseline = baseline.expect("测试服务器应提供 ETag 或 Last-Modified");
    // Last-Modified 可能只有秒级精度；跨过一秒才能确保只提供时间戳的服务端也变更版本。
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    store
        .replace_if_revision("/note.txt", b"other device", Some(&baseline))
        .await
        .expect("另一设备持有当前版本时应允许覆盖");

    let err = store
        .replace_if_revision("/note.txt", b"stale local edit", Some(&baseline))
        .await
        .expect_err("旧版本条件写必须失败");
    assert!(
        matches!(err, RemoteError::Conflict),
        "应分类为远端冲突: {err:?}"
    );
    assert_eq!(
        std::fs::read(root.join("note.txt")).expect("读冲突后的远端文件"),
        b"other device",
        "冲突时不能覆盖其他设备的版本"
    );

    server.shutdown().await;
}
/// Basic 认证：错误密码要被分类成 `Unauthorized`（界面据此弹重新登录，
/// 而不是笼统报「操作失败」）；正确密码正常工作。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn basic_auth_success_and_failure() {
    let root = unique_dir("auth");
    std::fs::write(root.join("f.omy"), b"x").expect("放一个文件");
    let server = spawn_server(root, Some(("omy", "secret"))).await;
    let base = server.base_url();

    // 错误密码 → Unauthorized（而不是 Network/Protocol）
    let bad = store_for(&base, "omy", "wrong", false);
    let err = bad.list("").await.expect_err("错误密码必须失败");
    assert!(
        matches!(err, RemoteError::Unauthorized),
        "错误密码应分类为 Unauthorized，实际: {err:?}"
    );

    // 匿名（不带凭据）→ 同样 401
    let anon = store_for(&base, "", "", false);
    assert!(matches!(
        anon.list("").await,
        Err(RemoteError::Unauthorized)
    ));

    // 正确密码 → 正常列出
    let good = store_for(&base, "omy", "secret", false);
    let items = good.list("").await.expect("正确密码应能列目录");
    assert!(items.iter().any(|e| e.name == "f.omy"));

    // 服务器确实收到过带错误凭据的挑战请求
    assert!(server.snapshot().iter().any(|r| r.method == "PROPFIND"));

    server.shutdown().await;
}

/// 服务端忽略 Range、整文件返回时，客户端要按 offset 自行裁剪，
/// 不能把整文件当成那一小段（这条在真实服务器上通常走 206，
/// 此处仅验证 read_range 对「请求区间」的基本正确性，忽略-Range 分支由单元/容错覆盖）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn range_get_returns_exact_window() {
    let root = unique_dir("range");
    // 5 MiB 全 0xCD 文件，足够大，服务端对中段 Range 一定回 206 而不是整文件
    let data = vec![0xCDu8; 5 * 1024 * 1024];
    std::fs::write(root.join("blob.bin"), &data).expect("写大文件");
    let server = spawn_server(root, None).await;
    let store = store_for(&server.base_url(), "", "", false);

    let e = find_entry(&store, "", "blob.bin").await;
    let got = store
        .read_range(&e.id, 1_000_000, 12345)
        .await
        .expect("中段 Range");
    assert_eq!(
        got.len(),
        12345,
        "返回长度必须恰好是请求长度，不能是整个文件"
    );
    assert!(got.iter().all(|&b| b == 0xCD), "内容正确");

    // 通过请求留痕确认 Range 起点/终点与目标路径都正确（服务端据此回 206）
    let recs = server.snapshot();
    assert!(
        recs.iter().any(|r| {
            r.method == "GET"
                && r.path.ends_with("blob.bin")
                && r.range.as_deref() == Some("bytes=1000000-1012344")
        }),
        "留痕为: {recs:?}"
    );

    server.shutdown().await;
}

/// CLI 纵向切片真正走的两条路径：`write_stream` 上传 + 「按固定块循环读到空」的
/// 分块下载，以及跨位置（两个独立服务端）原样复制。
///
/// 为什么单独测：CLI 的下载/复制不是调一次 `read_range`，而是自写的
/// 「offset 递增、读到空为止」循环。块边界 off-by-one、最后一块被多读一次或
/// 少读一次，store 层的单次读写测试都测不出来——只有把 CLI 那个循环原样跑一遍
/// 真实服务器才能抓到。跨位置复制同理：源读完、目标写出、再核对磁盘字节。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_style_stream_upload_chunked_download_and_copy() {
    let root_a = unique_dir("cli_a");
    let root_b = unique_dir("cli_b");
    let srv_a = spawn_server(root_a.clone(), None).await;
    let srv_b = spawn_server(root_b.clone(), None).await;
    let a = Arc::new(store_for(&srv_a.base_url(), "", "", true));
    let b = Arc::new(store_for(&srv_b.base_url(), "", "", true));

    // 3.1 MiB 确定性数据，横跨多个 1 MiB 块，专门压块边界
    const CHUNK: u64 = 1 << 20;
    let payload: Vec<u8> = (0..(3 * 1024 * 1024 + 123_456))
        .map(|i| (i % 251) as u8)
        .collect();

    // 1) write_stream 上传（CLI upload 的真实调用，流式、不整段进内存）
    let reader = Box::new(std::io::Cursor::new(payload.clone()))
        as Box<dyn tokio::io::AsyncRead + Unpin + Send>;
    let ent = a
        .write_stream("", "blob.bin", payload.len() as u64, reader)
        .await
        .expect("write_stream 上传");
    assert_eq!(ent.name, "blob.bin");

    // 2) CLI download 的真实循环：先从 list 拿总大小，再按 CHUNK 读满为止。
    //    不能「读到空就停」：offset 越过文件尾时服务端回 416 错误而非空响应。
    let ent_full = find_entry(&a, "", "blob.bin").await;
    let total = ent_full.size.expect("content-length");
    let mut got = Vec::new();
    let mut off = 0u64;
    while off < total {
        let want = CHUNK.min(total - off);
        let buf = a.read_range(&ent.id, off, want).await.expect("分块读");
        off = off.saturating_add(buf.len() as u64);
        got.extend_from_slice(&buf);
    }
    assert_eq!(got.len(), payload.len(), "下载长度必须等于上传长度");
    assert_eq!(got, payload, "分块下载必须逐字节一致");

    // 3) CLI copy 的真实循环：按已知大小整段读 A，写 B，再核对 B 磁盘字节
    let mut copied = Vec::with_capacity(total as usize);
    let mut off = 0u64;
    while off < total {
        let want = CHUNK.min(total - off);
        let buf = a.read_range(&ent.id, off, want).await.expect("复制读");
        off = off.saturating_add(buf.len() as u64);
        copied.extend_from_slice(&buf);
    }
    b.write("/", "blob_copy.bin", &copied).await.expect("写到 B");
    let on_b = std::fs::read(root_b.join("blob_copy.bin")).expect("读回 B 磁盘");
    assert_eq!(on_b, payload, "跨位置复制后 B 磁盘字节必须与源一致");

    srv_a.shutdown().await;
    srv_b.shutdown().await;
}

/// `ensure_remote_dir`：逐级补建中间目录，且已存在时幂等不报错。
///
/// 不这样会怎样：WebDAV MKCOL 一次只建一层、父目录必须已存在。直接对 `/a/b/c`
/// 发 MKCOL 会被服务端 409 拒；客户端若不逐级建，递归上传就建不出嵌套目录。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ensure_remote_dir_creates_nested_and_is_idempotent() {
    let root = unique_dir("ensure_dir");
    let srv = spawn_server(root.clone(), None).await;
    let store = store_for(&srv.base_url(), "", "", true);

    ensure_remote_dir(&store, "/a/b/c").await.expect("逐级建 a/b/c");
    for p in [root.join("a"), root.join("a").join("b"), root.join("a").join("b").join("c")] {
        assert!(p.is_dir(), "远程应出现 {}", p.display());
    }
    // 再建一次：已存在的层级跳过，不能报「目录已存在」
    ensure_remote_dir(&store, "/a/b/c").await.expect("幂等重建");
    ensure_remote_dir(&store, "/a/b/d").await.expect("补建同级 d");
    assert!(root.join("a").join("b").join("d").is_dir(), "应补建 d");

    srv.shutdown().await;
}

/// `commit_upload`：WebDAV 同时支持 write+rename+delete 时，先写临时名再 MOVE
/// 成最终名——磁盘上最终名存在，且不留任何 `.omy-upload-*` 临时残留。
///
/// 不这样会怎样：直接写到最终名、中途失败会留下一个顶着最终名的半截文件，
/// 用户下次以为传好了；临时名方案让失败只污染一个会被删掉的临时对象。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_upload_temp_name_then_renames_without_residue() {
    let root = unique_dir("commit");
    let srv = spawn_server(root.clone(), None).await;
    let store = store_for(&srv.base_url(), "", "", true);
    let caps = store.capabilities();
    assert!(caps.write && caps.rename && caps.delete, "本用例需要走临时名分支");

    let payload = b"committed-bytes".to_vec();
    let reader = Box::new(std::io::Cursor::new(payload.clone()))
        as Box<dyn tokio::io::AsyncRead + Unpin + Send>;
    let entry = commit_upload(&store, caps, "", "final.omy", payload.len() as u64, reader, None)
        .await
        .expect("commit_upload");
    assert_eq!(entry.name, "final.omy");

    // 最终名落盘且内容一致
    let on_disk = std::fs::read(root.join("final.omy")).expect("读回");
    assert_eq!(on_disk, payload);

    // 根目录列表里不应残留任何临时名
    let listed = store.list("").await.expect("列根");
    let temps: Vec<&str> = listed
        .iter()
        .map(|e| e.name.as_str())
        .filter(|n| n.starts_with(".omy-upload-"))
        .collect();
    assert!(temps.is_empty(), "临时名应被 MOVE 掉，残留: {temps:?}");

    srv.shutdown().await;
}

/// `ensure_safe_remote`：路径里的 `..` 必须在发请求前拒绝（路径穿越防护）。
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn ensure_safe_remote_rejects_dotdot() {
    assert!(ensure_safe_remote("/ok/path").is_ok());
    assert!(ensure_safe_remote("/a/../b").is_err(), "含 .. 必须拒绝");
    assert!(ensure_safe_remote("/a/b/..").is_err());
    assert!(ensure_safe_remote("..").is_err());
}

/// `decrypt_stream_to_local` 端到端：远程 .omy 边下边解，明文逐字节落本地目录，
/// 且本地不留 `.part`。
///
/// 不这样会怎样：早先 GUI 在 `place_cmds.rs` 里写了一份「.part + 改名 + 失败清理」，
/// CLI 若再写一遍，清理/二次检查迟早走岔。这里对着真服务器跑一遍共享核心，证明
/// 它解出来的明文与原始逐字节一致。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn decrypt_stream_to_local_roundtrip() {
    let plain: Vec<u8> = (0..(256 * 1024 + 777)).map(|i| (i % 251) as u8).collect();
    let (omy_bytes, kek) = make_omy(&plain, b"dec-pw", Some("note.txt"));

    let remote_root = unique_dir("dec_remote");
    let out_dir = unique_dir("dec_out");
    let srv = spawn_server(remote_root.clone(), None).await;
    let store = Arc::new(store_for(&srv.base_url(), "", "", true));

    // 先把 .omy 放到远程
    store.write("", "note.omy", &omy_bytes).await.expect("上传密文");
    let entry = find_entry(&store, "", "note.omy").await;
    let size = entry.size.expect("content-length");

    // open 一次拿文件名与明文大小（与 CLI/GUI 调用方同路径）
    let opened = omy_core::file::open(&omy_bytes, std::slice::from_ref(&kek)).expect("open 探针");
    let safe = omy_core::unpack::sanitize_filename(&opened.filename().expect("文件名"));
    let plaintext_size = opened.header.plaintext_size;

    let outcome = decrypt_stream_to_local(DecryptStreamRequest {
        store: Arc::clone(&store),
        place_id: "e2e".into(),
        id: entry.id.clone(),
        total_ct_size: size,
        header: omy_bytes.clone(),
        keks: vec![kek],
        cache: None,
        dest_dir: out_dir.clone(),
        safe_name: safe.clone(),
        plaintext_size,
        force: false,
        rt: tokio::runtime::Handle::current(),
    })
    .await
    .expect("流式解密到本地");

    assert_eq!(outcome.name, "note.txt");
    let got = std::fs::read(&outcome.saved_path).expect("读回本地明文");
    assert_eq!(got, plain, "解出的明文必须逐字节一致");
    assert_eq!(outcome.bytes, plain.len() as u64);
    // 不应留 .part
    assert!(
        !out_dir.join("note.txt.part").exists(),
        "成功后不应残留 .part"
    );

    // 再解一次、不 force：本地已存在应报 Conflict（覆盖守卫）
    let second = decrypt_stream_to_local(DecryptStreamRequest {
        store,
        place_id: "e2e".into(),
        id: entry.id,
        total_ct_size: size,
        header: omy_bytes,
        keks: vec![opened_kek_unused()],
        cache: None,
        dest_dir: out_dir,
        safe_name: safe,
        plaintext_size,
        force: false,
        rt: tokio::runtime::Handle::current(),
    })
    .await;
    assert!(matches!(second, Err(RemoteError::Conflict)), "不 force 二次解应冲突");

    srv.shutdown().await;
}

/// 占位：二次解需要一个 KEK，但第一个已 move 进闭包。这里只是为了让 Conflict
/// 分支不依赖密码正确性——冲突发生在写本地之前的存在性检查，根本不会用到 KEK。
fn opened_kek_unused() -> omy_core::crypto::Kek {
    let salt = [0u8; 16];
    omy_core::crypto::Kek::from_password(b"x", &salt, omy_core::crypto::Argon2Params::TEST_WEAK)
        .expect("派生占位 KEK")
}
