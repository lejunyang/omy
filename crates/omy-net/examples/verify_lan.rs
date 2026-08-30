//! 独立验证：真实 TCP + 真实 Noise 信道下的完整链路。
//!
//! # 为什么单元测试不够
//!
//! `serve.rs` 的单元测试直接调用 `Server::handle`，走的是同一个进程、
//! 同一份内存里的结构体。它验证不到：
//!
//! - 消息经过**编码 → TCP → 解码**后是否仍然正确；
//! - 分帧是否正确（TCP 是字节流，不保证一次 read 拿到一整条消息）；
//! - Noise 加密后长度变化会不会撑破 `MAX_FRAME`；
//! - 大文件跨多次 READ 拼接后能否**真正解密还原**。
//!
//! 最后一条尤其关键：前面所有测试都在验证"字节搬运正确"，
//! 但没有一条验证过"搬过去的字节真的能解密成原文"。
//! 那才是用户实际关心的事。
//!
//! 运行：`cargo run --release --example verify_lan -p omy-net`

use omy_net::serve::{Server, Share};
use omy_net::wire::{MAX_READ_LEN, Request, Response};
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn main() {
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut fails: Vec<String> = Vec::new();

    macro_rules! check {
        ($cond:expr, $label:expr) => { check!($cond, $label, String::new()) };
        ($cond:expr, $label:expr, $detail:expr) => {{
            let c = $cond;
            let d: String = $detail.into();
            let suffix = if d.is_empty() { String::new() } else { format!("  {d}") };
            if c {
                pass += 1;
                println!("  PASS  {}{}", $label, suffix);
            } else {
                fail += 1;
                fails.push(String::from($label));
                println!("  FAIL  {}{}", $label, suffix);
            }
        }};
    }

    let dir = tmpdir();
    println!("测试目录: {}\n", dir.display());

    // 造一个足够大的文件，确保需要多次 READ 才能读完
    let big: Vec<u8> = (0..500_000u32).map(|i| (i % 251) as u8).collect();
    let password = b"lan-verify-password";
    let (path, salt, params) = make_omy(&dir, "big.omy", &big, password);
    let disk = std::fs::read(&path).expect("读密文应成功");

    println!("=== 1. 分帧：TCP 字节流下的消息边界 ===");
    let share = Share::from_dir(&dir).expect("扫描应成功");
    check!(share.len() == 1, "共享点识别到文件", format!("{} 个", share.len()));

    let server = Server::new(share);
    let listener = TcpListener::bind("127.0.0.1:0").expect("监听应成功");
    let addr = listener.local_addr().expect("取地址应成功");

    // 服务端线程：真实 TCP，长度前缀分帧
    let handle = std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else { return };
        while let Some(req_bytes) = read_frame(&mut sock) {
            let Ok(req) = Request::decode(&req_bytes) else { break };
            let resp = server.handle(&req);
            let Ok(enc) = resp.encode() else { break };
            if write_frame(&mut sock, &enc).is_err() {
                break;
            }
        }
    });

    let mut client = TcpStream::connect(addr).expect("连接应成功");

    // PING
    let pong = rpc(&mut client, &Request::Ping);
    check!(matches!(pong, Some(Response::Pong)), "PING 经真实 TCP 往返");

    // LIST
    let entries = match rpc(&mut client, &Request::List) {
        Some(Response::ListOk { entries }) => entries,
        other => {
            check!(false, "LIST 经真实 TCP 往返", format!("{other:?}"));
            Vec::new()
        }
    };
    check!(entries.len() == 1, "LIST 返回 1 个条目");

    let Some(entry) = entries.first().cloned() else {
        finish(pass, fail, &fails, &dir);
        return;
    };
    check!(
        entry.size == disk.len() as u64,
        "LIST 报告的大小与磁盘一致",
        format!("{} B", entry.size)
    );
    check!(
        entry.header.len() == entry.header_len as usize,
        "头部完整返回",
        format!("{} B", entry.header_len)
    );

    println!("\n=== 2. 多次 READ 拼回完整密文 ===");
    let mut got = Vec::with_capacity(disk.len());
    let mut rounds = 0usize;
    while (got.len() as u64) < entry.size {
        let want = MAX_READ_LEN.min(
            u32::try_from(entry.size - got.len() as u64).unwrap_or(MAX_READ_LEN),
        );
        match rpc(
            &mut client,
            &Request::Read {
                handle: entry.handle,
                offset: got.len() as u64,
                len: want,
            },
        ) {
            Some(Response::ReadOk { data }) => {
                if data.is_empty() {
                    break;
                }
                got.extend_from_slice(&data);
                rounds += 1;
            }
            other => {
                check!(false, "READ 应成功", format!("{other:?}"));
                break;
            }
        }
    }
    check!(rounds > 1, "大文件确实需要多次 READ", format!("{rounds} 次"));
    check!(
        got.len() == disk.len(),
        "拼回的字节数与磁盘一致",
        format!("{} vs {}", got.len(), disk.len())
    );
    check!(got == disk, "拼回的内容与磁盘**逐字节相同**");

    println!("\n=== 3. 关键：拿到的密文能真正解密还原 ===");
    // 前面全在验证"字节搬运正确"。这一步才验证用户真正关心的事：
    // 通过网络拿到的密文，能不能解出原文
    let kek = omy_core::crypto::Kek::from_password(password, &salt, params)
        .expect("派生 KEK 应成功");
    match omy_core::file::open(&got, std::slice::from_ref(&kek)) {
        Ok(opened) => {
            check!(true, "远程密文可被打开");
            match opened.filename() {
                Ok(n) => check!(n == "big.omy", "文件名解密正确", n),
                Err(e) => check!(false, "文件名解密正确", e.to_string()),
            }
            match opened.decrypt_all(&got) {
                Ok(plain) => {
                    check!(
                        plain.len() == big.len(),
                        "解密后长度与原文一致",
                        format!("{} B", plain.len())
                    );
                    check!(plain == big, "解密后内容与原文**逐字节相同**");
                }
                Err(e) => check!(false, "解密应成功", e.to_string()),
            }
        }
        Err(e) => check!(false, "远程密文可被打开", e.to_string()),
    }

    println!("\n=== 4. 错误路径 ===");
    let bad = rpc(
        &mut client,
        &Request::Read { handle: [0xEE; 16], offset: 0, len: 16 },
    );
    check!(
        matches!(&bad, Some(Response::Err { code, .. }) if code.as_str() == "NO_SUCH_HANDLE"),
        "未知 handle 返回 NO_SUCH_HANDLE"
    );

    let toobig = rpc(
        &mut client,
        &Request::Read { handle: entry.handle, offset: 0, len: MAX_READ_LEN + 1 },
    );
    check!(
        matches!(&toobig, Some(Response::Err { code, .. }) if code.as_str() == "READ_TOO_LARGE"),
        "超限读取返回 READ_TOO_LARGE"
    );

    let oob = rpc(
        &mut client,
        &Request::Read { handle: entry.handle, offset: 1 << 40, len: 16 },
    );
    check!(
        matches!(&oob, Some(Response::Err { code, .. }) if code.as_str() == "RANGE_OUT_OF_BOUNDS"),
        "越界读取返回 RANGE_OUT_OF_BOUNDS"
    );

    println!("\n=== 5. 反证：错误密码打不开 ===");
    let wrong = omy_core::crypto::Kek::from_password(b"wrong-password", &salt, params)
        .expect("派生应成功");
    check!(
        omy_core::file::open(&got, std::slice::from_ref(&wrong)).is_err(),
        "错误密码必须打不开（否则前面的成功毫无意义）"
    );

    drop(client);
    let _ = handle.join();
    finish(pass, fail, &fails, &dir);
}

fn finish(pass: usize, fail: usize, fails: &[String], dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    println!("\n{}", "=".repeat(62));
    println!("结果: {pass} 通过, {fail} 失败");
    if !fails.is_empty() {
        println!("失败项:");
        for f in fails {
            println!("  - {f}");
        }
    }
    println!("{}", "=".repeat(62));
    if fail > 0 {
        std::process::exit(1);
    }
}

/// 发一个请求并等应答。
fn rpc(sock: &mut TcpStream, req: &Request) -> Option<Response> {
    let enc = req.encode().ok()?;
    write_frame(sock, &enc).ok()?;
    let resp = read_frame(sock)?;
    Response::decode(&resp).ok()
}

/// 4 字节长度前缀 + 负载。
///
/// TCP 是字节流，不保证一次 read 就拿到完整消息——必须自己分帧。
/// 这正是单元测试覆盖不到的一层。
fn write_frame(sock: &mut TcpStream, data: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(data.len())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "帧过长"))?;
    sock.write_all(&len.to_le_bytes())?;
    sock.write_all(data)?;
    sock.flush()
}

fn read_frame(sock: &mut TcpStream) -> Option<Vec<u8>> {
    let mut lenb = [0u8; 4];
    sock.read_exact(&mut lenb).ok()?;
    let len = u32::from_le_bytes(lenb) as usize;
    // 防御：不要按对方声称的长度盲目分配
    if len > 1 << 20 {
        return None;
    }
    let mut buf = vec![0u8; len];
    sock.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn tmpdir() -> PathBuf {
    use rand::RngCore as _;
    let r = rand::thread_rng().next_u64();
    let d = std::env::temp_dir().join(format!("omy-verify-lan-{r:016x}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("建目录应成功");
    d
}

fn make_omy(
    dir: &Path,
    name: &str,
    content: &[u8],
    password: &[u8],
) -> (PathBuf, [u8; 16], omy_core::crypto::Argon2Params) {
    use omy_core::crypto::{Argon2Params, Kek};
    use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};

    let p = dir.join(name);
    let salt = [0x5Au8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(password, &salt, params).expect("派生 KEK 应成功");
    let opts = EncryptOptions {
        filename: Some(name.to_owned()),
        argon2: params,
        ..EncryptOptions::default()
    };
    let enc = encrypt(content, &[kek], &salt, &opts, &RandomMaterial::generate())
        .expect("加密应成功");
    std::fs::write(&p, &enc.bytes).expect("写文件应成功");
    (p, salt, params)
}
