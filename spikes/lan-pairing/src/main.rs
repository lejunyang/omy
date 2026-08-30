//! Spike：验证局域网共享的协议栈能否真正跑通。
//!
//! 实现前必须先证伪的几件事：
//!
//! | # | 问题 | 为什么必须先验 |
//! |---|---|---|
//! | 1 | SPAKE2 用 4 位 PIN 能否派生出一致的强密钥 | 配对的全部安全性建立在这上面 |
//! | 2 | PIN 不同时双方是否**确实**得到不同密钥 | 若相同则整个配对形同虚设 |
//! | 3 | Noise IK 能否用 SPAKE2 交换的静态公钥建立信道 | 决定长期信道方案 |
//! | 4 | 单个 Noise 会话能否承载多次请求-响应 | 文档 §6.3 要求连接复用 |
//! | 5 | Noise 的单条消息长度上限是多少 | 直接决定 READ 的分块大小 |
//! | 6 | 篡改密文能否被检测 | 信道完整性 |
//!
//! 全部在**真实的两个握手状态机**之间跑，不是自己跟自己对话。

use anyhow::{Context as _, Result, bail};
use rand::RngCore as _;

/// Noise 的握手模式。
///
/// IK：发起方已知响应方静态公钥（配对时交换过），1-RTT 建立，提供前向保密。
/// 这正是文档 §4.2 选定的模式。
const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

fn main() {
    let mut failed = 0usize;
    let mut passed = 0usize;
    let mut fails: Vec<String> = Vec::new();

    let mut check = |cond: bool, label: &str, detail: &str| {
        if cond {
            passed += 1;
            println!("  PASS  {label}{}", fmt_detail(detail));
        } else {
            failed += 1;
            fails.push(label.to_owned());
            println!("  FAIL  {label}{}", fmt_detail(detail));
        }
    };

    println!("=== 1. SPAKE2 配对（4 位 PIN）===");
    match spake2_same_pin() {
        Ok((k1, k2)) => {
            check(k1 == k2, "相同 PIN 双方派生出一致的密钥", &hex_head(&k1));
            check(k1.len() >= 32, "派生密钥至少 32 字节", &format!("{} B", k1.len()));
            // 弱 PIN 派生出的密钥必须仍是高熵的——这正是 PAKE 的意义
            check(
                !k1.iter().all(|b| *b == k1[0]),
                "密钥不是常量（排除实现退化）",
                "",
            );
        }
        Err(e) => check(false, "相同 PIN 双方派生出一致的密钥", &e.to_string()),
    }

    println!("\n=== 2. PIN 不匹配必须失败 ===");
    match spake2_diff_pin() {
        Ok((k1, k2)) => {
            // 注意：SPAKE2 在 PIN 不同时**不报错**，而是双方得到不同的密钥。
            // 真正的拒绝发生在后续的密钥确认环节——这一点很容易被误解为
            // 「PAKE 会自己检测错误密码」，从而漏掉密钥确认步骤。
            check(k1 != k2, "PIN 不同时双方密钥不同（不是静默相同）", "");
            check(
                !k1.is_empty() && !k2.is_empty(),
                "PIN 不同时 finish 本身不报错（必须靠密钥确认拒绝）",
                "已确认此行为",
            );
        }
        Err(e) => check(false, "PIN 不同时双方密钥不同", &e.to_string()),
    }

    println!("\n=== 3. Noise IK 信道 ===");
    match noise_roundtrip() {
        Ok(r) => {
            check(r.handshake_ok, "IK 握手完成", &format!("{} 条消息", r.msgs));
            check(r.echo_ok, "加密信道上的请求-响应正确", "");
            check(
                r.multi_ok,
                "单个会话承载多次请求（连接复用）",
                &format!("{} 轮", r.rounds),
            );
            check(
                r.max_payload > 0,
                "单条消息的最大负载",
                &format!("{} B", r.max_payload),
            );
            check(
                r.tamper_detected,
                "篡改密文被检测（信道完整性）",
                "",
            );
        }
        Err(e) => check(false, "IK 握手完成", &e.to_string()),
    }

    println!("\n{}", "=".repeat(60));
    println!("结果: {passed} 通过, {failed} 失败");
    if !fails.is_empty() {
        println!("\n失败项:");
        for f in &fails {
            println!("  - {f}");
        }
    }
    println!("{}", "=".repeat(60));

    if failed > 0 {
        std::process::exit(1);
    }
}

fn fmt_detail(d: &str) -> String {
    if d.is_empty() {
        String::new()
    } else {
        format!("  {d}")
    }
}

fn hex_head(b: &[u8]) -> String {
    hex::encode(b.get(..8).unwrap_or(b))
}

/// 双方使用相同 PIN 跑一次 SPAKE2。
fn spake2_same_pin() -> Result<(Vec<u8>, Vec<u8>)> {
    run_spake2(b"4B7F", b"4B7F")
}

/// 双方 PIN 不同。
fn spake2_diff_pin() -> Result<(Vec<u8>, Vec<u8>)> {
    run_spake2(b"4B7F", b"0000")
}

/// 跑一次完整的 SPAKE2 交换。
///
/// 用 `start_symmetric`：配对时谁先扫谁的码是不确定的，没有天然的
/// 客户端/服务端之分。对称模式两边代码完全一样，少一处出错的可能。
fn run_spake2(pin_a: &[u8], pin_b: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    use spake2::{Ed25519Group, Identity, Password, Spake2};

    // identity 把密钥绑定到具体用途上。不同服务用不同 id，
    // 可防止攻击者把配对消息挪用到别的会话里（文档 §4.1 的中间人防护）
    let id = Identity::new(b"omy/v1/pairing");

    let (s_a, msg_a) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(pin_a), &id);
    let (s_b, msg_b) = Spake2::<Ed25519Group>::start_symmetric(&Password::new(pin_b), &id);

    let key_a = s_a
        .finish(&msg_b)
        .map_err(|e| anyhow::anyhow!("A 侧 finish 失败: {e:?}"))?;
    let key_b = s_b
        .finish(&msg_a)
        .map_err(|e| anyhow::anyhow!("B 侧 finish 失败: {e:?}"))?;

    Ok((key_a, key_b))
}

/// Noise 往返测试的结果。
struct NoiseResult {
    handshake_ok: bool,
    msgs: usize,
    echo_ok: bool,
    multi_ok: bool,
    rounds: usize,
    max_payload: usize,
    tamper_detected: bool,
}

/// 用 Noise IK 建立信道并跑多轮请求-响应。
fn noise_roundtrip() -> Result<NoiseResult> {
    let params = NOISE_PATTERN.parse().context("解析 Noise 模式失败")?;

    // 双方的长期静态密钥。真实场景里这是配对时交换并持久化的。
    //
    // NoiseParams 不实现 Copy 且 Builder 会消费它，所以每次都重新 parse。
    // 看着啰嗦，但 parse 是纯字符串解析，开销可忽略
    let kp_i = snow::Builder::new(params)
        .generate_keypair()
        .context("生成发起方密钥失败")?;
    let kp_r = snow::Builder::new(NOISE_PATTERN.parse()?)
        .generate_keypair()
        .context("生成响应方密钥失败")?;

    // IK 的前提：发起方**已知**响应方的静态公钥。
    // 这个公钥正是配对阶段通过 SPAKE2 信道交换过来的。
    //
    // 注意 builder 的每个方法都返回 Result（重复设置同一参数会报
    // ParameterOverwrite），必须逐个 ?，不能像常见的 builder 那样直接链式调用
    let mut ini = snow::Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(&kp_i.private)?
        .remote_public_key(&kp_r.public)?
        .build_initiator()
        .context("构建发起方失败")?;

    // 响应方不需要预知对方公钥：IK 模式下发起方会在握手中把自己的
    // 静态公钥加密发过来，这正是 IK 里那个 "K"（Known）只指响应方的原因
    let mut res = snow::Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(&kp_r.private)?
        .build_responder()
        .context("构建响应方失败")?;

    let mut buf = vec![0u8; 65535];
    let mut buf2 = vec![0u8; 65535];
    let mut msgs = 0usize;

    // -> e, es, s, ss
    let n = ini.write_message(&[], &mut buf).context("握手消息 1 失败")?;
    msgs += 1;
    res.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2)
        .context("响应方读握手消息 1 失败")?;

    // <- e, ee, se
    let n = res.write_message(&[], &mut buf).context("握手消息 2 失败")?;
    msgs += 1;
    ini.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2)
        .context("发起方读握手消息 2 失败")?;

    if !ini.is_handshake_finished() || !res.is_handshake_finished() {
        bail!("握手未完成");
    }

    let mut ini = ini.into_transport_mode().context("发起方转传输模式失败")?;
    let mut res = res.into_transport_mode().context("响应方转传输模式失败")?;

    // 单轮请求-响应
    let req = b"READ handle=abc offset=0 len=1024";
    let n = ini.write_message(req, &mut buf)?;
    let m = res.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2)?;
    let echo_ok = buf2.get(..m) == Some(&req[..]);

    // 多轮：验证连接复用（Noise 的 nonce 会自增，多轮必须都能通过）
    let mut rounds = 0usize;
    let mut multi_ok = true;
    for i in 0..64u32 {
        let payload = format!("REQ#{i}");
        let n = match ini.write_message(payload.as_bytes(), &mut buf) {
            Ok(v) => v,
            Err(_) => {
                multi_ok = false;
                break;
            }
        };
        let m = match res.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2) {
            Ok(v) => v,
            Err(_) => {
                multi_ok = false;
                break;
            }
        };
        if buf2.get(..m) != Some(payload.as_bytes()) {
            multi_ok = false;
            break;
        }
        // 反向也发一次，模拟真实的响应
        let resp = vec![0xABu8; 4096];
        let n = match res.write_message(&resp, &mut buf) {
            Ok(v) => v,
            Err(_) => {
                multi_ok = false;
                break;
            }
        };
        let m = match ini.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2) {
            Ok(v) => v,
            Err(_) => {
                multi_ok = false;
                break;
            }
        };
        if buf2.get(..m) != Some(&resp[..]) {
            multi_ok = false;
            break;
        }
        rounds += 1;
    }

    // 探测单条消息的负载上限：Noise 规定密文上限 65535，
    // 减去 16 字节 tag 就是明文上限。这个数字直接决定 READ 的分块大小
    let mut max_payload = 0usize;
    for size in [16384usize, 32768, 65519, 65520] {
        let data = vec![0u8; size];
        if ini.write_message(&data, &mut buf).is_ok() {
            max_payload = size;
        } else {
            break;
        }
    }

    // 篡改检测：改一个字节，读取必须失败
    let mut rng = rand::thread_rng();
    let mut probe = vec![0u8; 256];
    rng.fill_bytes(&mut probe);
    let n = ini.write_message(&probe, &mut buf)?;
    if let Some(b) = buf.get_mut(10) {
        *b ^= 0xFF;
    }
    let tamper_detected = res.read_message(buf.get(..n).unwrap_or(&[]), &mut buf2).is_err();

    Ok(NoiseResult {
        handshake_ok: true,
        msgs,
        echo_ok,
        multi_ok,
        rounds,
        max_payload,
        tamper_detected,
    })
}
