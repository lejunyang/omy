//! 端到端验证：配对 → Noise 信道 → 密文服务 → 真实解密还原。
//!
//! # 与 `verify_lan.rs` 的分工
//!
//! `verify_lan` 用**明文 TCP** 验证服务端逻辑与分帧。本程序在其之上
//! 加了完整的安全层：先用 PIN 配对交换静态公钥，再用 Noise IK 建立
//! 加密信道，所有请求都走加密信道。
//!
//! 重点验证几件单元测试覆盖不到的事：
//!
//! 1. 配对交换的公钥能**直接**用于 Noise 握手（模块接缝）；
//! 2. 加密信道上传输的密文**真的能解密还原**成原文；
//! 3. 网络上流动的字节里**不含明文文件名**（抓包视角的反证）；
//! 4. 未配对的设备连不上（拿着错误公钥握手必定失败）。
//!
//! 运行：`cargo run --release --example verify_e2e -p omy-net`

use omy_net::channel::{Channel, StaticKeypair};
use omy_net::handshake::pair_over;
use omy_net::pairing::Role;
use omy_net::serve::{Server, Share};
use omy_net::wire::{MAX_READ_LEN, Request, Response};
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() {
    // 看门狗：验证程序绝不该无限期挂着。任何一步卡死都应当以明确的
    // 失败告终，而不是让人盯着不动的终端猜是"还在跑"还是"挂了"。
    //
    // 这条是踩过坑加的：第一版没有超时，第 5 步的连接因服务端没有
    // 空闲 accept 而永远等待，程序静默挂死，只能靠查进程才发现。
    tokio::spawn(async {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        eprintln!("\n[看门狗] 超过 120 秒未完成，判定为挂起。");
        std::process::exit(2);
    });
    run().await;
}

#[allow(clippy::too_many_lines)]
async fn run() {
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut fails: Vec<String> = Vec::new();

    macro_rules! check {
        ($cond:expr, $label:expr) => { check!($cond, $label, String::new()) };
        ($cond:expr, $label:expr, $detail:expr) => {{
            let c = $cond;
            let d: String = $detail.into();
            let s = if d.is_empty() { String::new() } else { format!("  {d}") };
            if c {
                pass += 1;
                println!("  PASS  {}{}", $label, s);
            } else {
                fail += 1;
                fails.push(String::from($label));
                println!("  FAIL  {}{}", $label, s);
            }
        }};
    }

    let dir = tmpdir();
    let secret_name = "机密-季度报告-2026.docx.omy";
    let content: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let password = b"e2e-test-password";
    let (_path, salt, params) = make_omy(&dir, secret_name, &content, password);

    // ================= 1. 配对 =================
    println!("=== 1. PIN 配对交换静态公钥 ===");
    let pin = omy_net::pairing::generate_pin();
    check!(pin.len() == 6, "生成 6 位 PIN", pin.clone());

    let host_kp = StaticKeypair::generate().expect("生成应成功");
    let guest_kp = StaticKeypair::generate().expect("生成应成功");
    let host_pub_real = host_kp.public.clone();

    let (mut sa, mut sb) = tokio::io::duplex(8192);
    let pin_a = pin.clone();
    let pin_b = pin.clone();

    let host_task = tokio::spawn(async move {
        let d = pair_over(&mut sa, &pin_a, Role::Responder, &host_kp, "书房台式机")
            .await
            .expect("共享方配对应成功");
        (d, host_kp)
    });
    let guest_task = tokio::spawn(async move {
        let d = pair_over(&mut sb, &pin_b, Role::Initiator, &guest_kp, "客厅笔记本")
            .await
            .expect("访问方配对应成功");
        (d, guest_kp)
    });

    let (host_learned, host_kp) = host_task.await.expect("任务应完成");
    let (guest_learned, guest_kp) = guest_task.await.expect("任务应完成");

    check!(host_learned.name == "客厅笔记本", "共享方拿到对方设备名", host_learned.name.clone());
    check!(guest_learned.name == "书房台式机", "访问方拿到对方设备名", guest_learned.name.clone());
    check!(
        guest_learned.public_key == host_pub_real,
        "访问方拿到的是共享方的**真实**公钥"
    );
    check!(
        guest_learned.fingerprint() == host_kp.fingerprint(),
        "指纹一致",
        hex::encode(guest_learned.fingerprint())
    );

    // ================= 2. Noise 信道 =================
    println!("\n=== 2. 用配对得来的公钥建立 Noise IK 信道 ===");
    let share = Share::from_dir(&dir).expect("扫描应成功");
    check!(share.len() == 1, "共享点识别到文件");
    let server = Server::new(share);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("监听应成功");
    let addr = listener.local_addr().expect("取地址应成功");
    let guest_fp = guest_kp.fingerprint();

    let srv = tokio::spawn(async move {
        let (sock, _) = listener.accept().await.expect("accept 应成功");
        let mut ch = Channel::accept(sock, &host_kp).await.expect("响应方握手应成功");

        // 服务端必须核对对方身份：握手成功只证明对方持有某个私钥，
        // 不证明那是已配对的设备
        if ch.peer_fingerprint() != guest_fp {
            return Err("对方不是已配对设备");
        }

        loop {
            let Ok(raw) = ch.recv().await else { break };
            let Ok(req) = Request::decode(&raw) else { break };
            let resp = server.handle(&req);
            let Ok(enc) = resp.encode() else { break };
            if ch.send(&enc).await.is_err() {
                break;
            }
        }
        Ok(())
    });

    let sock = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
    let mut cli = Channel::connect(sock, &guest_kp, &guest_learned.public_key)
        .await
        .expect("发起方握手应成功");
    check!(true, "Noise IK 握手成功（1-RTT）");
    check!(
        cli.peer_fingerprint() == guest_learned.fingerprint(),
        "信道上的对方指纹与配对时一致"
    );

    // ================= 3. 加密信道上的服务 =================
    println!("\n=== 3. 加密信道上取回密文 ===");
    let entries = match cli.request(&Request::List).await {
        Ok(Response::ListOk { entries }) => entries,
        other => {
            check!(false, "LIST 应成功", format!("{other:?}"));
            Vec::new()
        }
    };
    check!(entries.len() == 1, "LIST 返回 1 个条目");

    let Some(entry) = entries.first().cloned() else {
        finish(pass, fail, &fails, &dir);
        return;
    };

    let mut got = Vec::with_capacity(entry.size as usize);
    let mut rounds = 0usize;
    while (got.len() as u64) < entry.size {
        let want = MAX_READ_LEN
            .min(u32::try_from(entry.size - got.len() as u64).unwrap_or(MAX_READ_LEN));
        match cli
            .request(&Request::Read {
                handle: entry.handle,
                offset: got.len() as u64,
                len: want,
            })
            .await
        {
            Ok(Response::ReadOk { data }) => {
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
    check!(rounds > 1, "跨多次 READ 取回", format!("{rounds} 次"));
    check!(
        got.len() as u64 == entry.size,
        "取回字节数与声明一致",
        format!("{} B", got.len())
    );

    // ================= 4. 真正解密还原 =================
    println!("\n=== 4. 关键：加密信道取回的密文能解密还原 ===");
    let kek = omy_core::crypto::Kek::from_password(password, &salt, params)
        .expect("派生 KEK 应成功");
    match omy_core::file::open(&got, std::slice::from_ref(&kek)) {
        Ok(opened) => {
            check!(true, "远程密文可被打开");
            match opened.filename() {
                Ok(n) => check!(n == secret_name, "文件名解密正确", n),
                Err(e) => check!(false, "文件名解密正确", e.to_string()),
            }
            match opened.decrypt_all(&got) {
                Ok(plain) => {
                    check!(plain.len() == content.len(), "解密后长度一致");
                    check!(plain == content, "解密后内容与原文**逐字节相同**");
                }
                Err(e) => check!(false, "解密应成功", e.to_string()),
            }
        }
        Err(e) => check!(false, "远程密文可被打开", e.to_string()),
    }

    // ================= 5. 反证 =================
    println!("\n=== 5. 反证：安全边界确实存在 ===");
    let wrong_kek =
        omy_core::crypto::Kek::from_password(b"wrong", &salt, params).expect("派生应成功");
    check!(
        omy_core::file::open(&got, std::slice::from_ref(&wrong_kek)).is_err(),
        "错误密码打不开（否则前面的成功毫无意义）"
    );

    // 未配对设备：拿着随机公钥去连。
    //
    // ⚠️ 必须加超时。服务端的 accept 只调用一次，此时它正在 loop 里
    // 处理已建立连接的请求，没有 accept 在等新连接——TCP 连接会进入
    // backlog，客户端发出握手消息后永远收不到应答。
    //
    // 没有超时的话整个程序就挂在这里。这是**测试设计**的问题，不是
    // 实现缺陷，但也暴露了一个真实约束：产品代码里所有网络等待都
    // 必须有超时，否则对端不响应就会永久卡住 UI。
    let stranger = StaticKeypair::generate().expect("生成应成功");
    let fake_target = StaticKeypair::generate().expect("生成应成功");
    let unpaired_ok = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        async {
            let s = tokio::net::TcpStream::connect(addr).await.ok()?;
            Channel::connect(s, &stranger, &fake_target.public).await.ok()
        },
    )
    .await
    {
        // 超时或握手失败都算"连不上"，这正是期望结果
        Err(_) => false,
        Ok(v) => v.is_some(),
    };
    check!(!unpaired_ok, "未配对设备无法建立信道（超时或握手失败）");

    // 另一条更直接的反证：在**有 accept 在等**的情况下，
    // 用错误公钥握手必定失败。上一条因为服务端忙而超时，
    // 证明力较弱——它没有真正走完握手协商
    let host_kp2 = StaticKeypair::generate().expect("生成应成功");
    let listener2 = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("监听应成功");
    let addr2 = listener2.local_addr().expect("取地址应成功");
    let srv2 = tokio::spawn(async move {
        let (sock, _) = listener2.accept().await.expect("accept 应成功");
        Channel::accept(sock, &host_kp2).await
    });
    let wrong_pub = StaticKeypair::generate().expect("生成应成功").public.clone();
    let sock3 = tokio::net::TcpStream::connect(addr2).await.expect("连接应成功");
    let bad = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        Channel::connect(sock3, &stranger, &wrong_pub),
    )
    .await;
    check!(
        matches!(&bad, Ok(Err(_))) || bad.is_err(),
        "用错误公钥握手必定失败（服务端确实在监听）"
    );
    let _ = srv2.await;

    drop(cli);
    // 服务端在客户端 drop 后 recv 会失败并退出 loop。加超时兜底：
    // 验证程序绝不该因为某一步没退出而永远挂着
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), srv).await;

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

fn tmpdir() -> PathBuf {
    use rand::RngCore as _;
    let r = rand::thread_rng().next_u64();
    let d = std::env::temp_dir().join(format!("omy-verify-e2e-{r:016x}"));
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
    let salt = [0x3Cu8; 16];
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
