//! 配对握手：把 SPAKE2 与 Noise 静态公钥的交换串起来。
//!
//! # 完整流程
//!
//! ```text
//!   展示方                                    输入方
//!   （屏幕上显示 6 位 PIN）                    （手动输入 PIN）
//!
//!   ── SPAKE2 消息 ──────────────────────────────>
//!   <────────────────────────────── SPAKE2 消息 ──
//!
//!   两侧各自算出共享密钥（此时还不能信任对方）
//!
//!   ── 密钥确认 ────────────────────────────────>
//!   <──────────────────────────────── 密钥确认 ──
//!
//!   确认通过 → PIN 一致，信道可信
//!
//!   ── 加密的静态公钥 ──────────────────────────>
//!   <────────────────────────── 加密的静态公钥 ──
//!
//!   双方持久化对方公钥，之后用 Noise IK 直接连
//! ```
//!
//! # 为什么静态公钥要加密传输
//!
//! 公钥本身不是秘密，明文发也不影响 Noise 的安全性。但加密传输能防止
//! **被动观察者建立设备关联**：局域网里的旁观者若能看到公钥，就能记录
//! "这两台设备配过对"，长期积累可以画出用户的设备关系图。
//!
//! 用 SPAKE2 派生的密钥加密后，旁观者只看到无区别的随机字节。
//!
//! # 一次 PIN 只用一次
//!
//! 每个函数都消费 self 或只能调用一次。SPAKE2 的安全性建立在
//! "每次协议执行只允许一次在线猜测"上——允许用同一 PIN 重试
//! 就把在线猜测变成了离线字典攻击。

use crate::channel::StaticKeypair;
use crate::error::{NetError, Result};
use crate::pairing::{Pairing, Role};

/// 配对成功后需要持久化的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedDevice {
    /// 对方的静态公钥，之后 Noise IK 用它。
    pub public_key: Vec<u8>,
    /// 对方的设备名，用于界面显示。
    ///
    /// **不可信**：由对方自称。真正的身份凭据是公钥。
    pub name: String,
}

impl PairedDevice {
    /// 对方的指纹。
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 8] {
        crate::discovery::fingerprint(&self.public_key)
    }
}

/// 静态公钥交换时的 HKDF label。
const LABEL_KEY_XFER: &[u8] = b"omy/v1/pairing/key-transfer";

/// 设备名的长度上限，与 discovery 保持一致。
const MAX_NAME: usize = crate::discovery::MAX_DEVICE_NAME;

/// 在一条已建立的字节流上完成配对。
///
/// `role` 决定密钥确认时用哪个方向的 tag，两侧必须不同。
/// 通常展示 PIN 的一方用 [`Role::Responder`]，输入 PIN 的一方用
/// [`Role::Initiator`]。
///
/// # Errors
/// PIN 不匹配、对方取消、网络错误或对方发来的数据非法时返回错误。
pub async fn pair_over<S>(
    stream: &mut S,
    pin: &str,
    role: Role,
    local: &StaticKeypair,
    local_name: &str,
) -> Result<PairedDevice>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    crate::discovery::validate_device_name(local_name)?;

    // ---- 第 1 步：SPAKE2 消息交换 ----
    let (state, our_msg) = Pairing::start(pin, role)?;
    write_chunk(stream, &our_msg).await?;
    let their_msg = read_chunk(stream).await?;

    // ⚠️ 这一步成功**不代表** PIN 正确。SPAKE2 在 PIN 不同时照样
    // 返回 Ok，只是双方拿到不同的密钥（spike 实测结论）
    let (confirm_state, our_confirm) = state.finish(&their_msg)?;

    // ---- 第 2 步：密钥确认。这才是真正判断 PIN 的地方 ----
    write_chunk(stream, &our_confirm).await?;
    let their_confirm = read_chunk(stream).await?;
    let keys = confirm_state.verify(&their_confirm)?;

    // ---- 第 3 步：加密交换静态公钥与设备名 ----
    let xfer_key = keys.derive(LABEL_KEY_XFER);
    let payload = encode_identity(&local.public, local_name)?;
    let sealed = seal(&xfer_key, role, &payload)?;
    write_chunk(stream, &sealed).await?;

    let their_sealed = read_chunk(stream).await?;
    // 用**对方**的方向解密：两侧用不同的 nonce 派生，
    // 否则同一密钥加密两条消息会重用 nonce，那是致命错误
    let their_payload = open(&xfer_key, peer_role(role), &their_sealed)?;
    let (public_key, name) = decode_identity(&their_payload)?;

    Ok(PairedDevice { public_key, name })
}

/// 对方的角色。
fn peer_role(r: Role) -> Role {
    match r {
        Role::Initiator => Role::Responder,
        Role::Responder => Role::Initiator,
    }
}

/// 按角色派生 nonce。
///
/// 同一个 `xfer_key` 要加密两条消息（各方向一条）。ChaCha20-Poly1305
/// **绝不能用同一个 (key, nonce) 加密两条不同的消息**——那会直接泄露
/// 明文异或值并使认证失效。这里用角色区分 nonce 的第一个字节，
/// 保证两个方向永不相同。
fn nonce_for(role: Role) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[0] = match role {
        Role::Initiator => 1,
        Role::Responder => 2,
    };
    n
}

/// 用配对密钥加密。
fn seal(key: &[u8; 32], role: Role, plaintext: &[u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::{Aead as _, KeyInit as _};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
    let nonce = nonce_for(role);
    cipher
        .encrypt(&nonce.into(), plaintext)
        .map_err(|_| NetError::PairingFailed)
}

/// 用配对密钥解密。
fn open(key: &[u8; 32], role: Role, ciphertext: &[u8]) -> Result<Vec<u8>> {
    use chacha20poly1305::aead::{Aead as _, KeyInit as _};
    let cipher = chacha20poly1305::ChaCha20Poly1305::new(key.into());
    let nonce = nonce_for(role);
    cipher
        .decrypt(&nonce.into(), ciphertext)
        .map_err(|_| NetError::PairingFailed)
}

/// 编码「公钥 + 设备名」。
fn encode_identity(public_key: &[u8], name: &str) -> Result<Vec<u8>> {
    let pk_len = u8::try_from(public_key.len()).map_err(|_| NetError::PairingFailed)?;
    let name_bytes = name.as_bytes();
    let name_len = u8::try_from(name_bytes.len()).map_err(|_| NetError::PairingFailed)?;
    let mut out = Vec::with_capacity(2 + public_key.len() + name_bytes.len());
    out.push(pk_len);
    out.extend_from_slice(public_key);
    out.push(name_len);
    out.extend_from_slice(name_bytes);
    Ok(out)
}

/// 解码「公钥 + 设备名」。
///
/// 全程边界检查：这些字节虽然经过认证加密（只有知道 PIN 的人能构造），
/// 但对方仍可能是实现有 bug 的旧版本，或是刻意构造畸形数据的恶意端。
fn decode_identity(buf: &[u8]) -> Result<(Vec<u8>, String)> {
    let pk_len = usize::from(*buf.first().ok_or(NetError::PairingFailed)?);
    let pk_end = 1usize.checked_add(pk_len).ok_or(NetError::PairingFailed)?;
    let public_key = buf.get(1..pk_end).ok_or(NetError::PairingFailed)?.to_vec();
    // X25519 公钥恒为 32 字节。长度不对说明对方实现有问题，
    // 继续下去只会在 Noise 握手时以更难排查的方式失败
    if public_key.len() != 32 {
        return Err(NetError::PairingFailed);
    }

    let name_len = usize::from(*buf.get(pk_end).ok_or(NetError::PairingFailed)?);
    if name_len > MAX_NAME {
        return Err(NetError::PairingFailed);
    }
    let name_start = pk_end.checked_add(1).ok_or(NetError::PairingFailed)?;
    let name_end = name_start.checked_add(name_len).ok_or(NetError::PairingFailed)?;
    let name_bytes = buf.get(name_start..name_end).ok_or(NetError::PairingFailed)?;
    let name = String::from_utf8(name_bytes.to_vec()).map_err(|_| NetError::PairingFailed)?;
    // 对方的设备名同样要校验：控制字符可用于伪造界面显示
    crate::discovery::validate_device_name(&name).map_err(|_| NetError::PairingFailed)?;

    // 尾部不应有多余字节
    if name_end != buf.len() {
        return Err(NetError::PairingFailed);
    }
    Ok((public_key, name))
}

/// 配对期间单条消息的上限。
///
/// 配对交换的都是小消息（SPAKE2 消息 33 字节、确认值 32 字节、
/// 身份约 100 字节）。设一个远低于常规帧上限的值，
/// 让畸形输入尽早失败。
const MAX_PAIR_CHUNK: usize = 1024;

async fn write_chunk<S>(stream: &mut S, data: &[u8]) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let len = u16::try_from(data.len()).map_err(|_| NetError::FrameTooLarge)?;
    if usize::from(len) > MAX_PAIR_CHUNK {
        return Err(NetError::FrameTooLarge);
    }
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_chunk<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut lenb = [0u8; 2];
    stream.read_exact(&mut lenb).await?;
    let len = usize::from(u16::from_be_bytes(lenb));
    if len > MAX_PAIR_CHUNK {
        return Err(NetError::FrameTooLarge);
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑一次双向配对。
    async fn run_pair(
        pin_a: &str,
        pin_b: &str,
    ) -> (Result<PairedDevice>, Result<PairedDevice>) {
        let (mut a, mut b) = tokio::io::duplex(8192);
        let ka = StaticKeypair::generate().expect("生成应成功");
        let kb = StaticKeypair::generate().expect("生成应成功");
        let pin_a = pin_a.to_owned();
        let pin_b = pin_b.to_owned();

        let ta = tokio::spawn(async move {
            pair_over(&mut a, &pin_a, Role::Initiator, &ka, "设备A").await
        });
        let tb = tokio::spawn(async move {
            pair_over(&mut b, &pin_b, Role::Responder, &kb, "设备B").await
        });
        (
            ta.await.expect("任务应完成"),
            tb.await.expect("任务应完成"),
        )
    }

    #[tokio::test]
    async fn successful_pairing_exchanges_keys() {
        let (ra, rb) = run_pair("314159", "314159").await;
        let da = ra.expect("A 侧配对应成功");
        let db = rb.expect("B 侧配对应成功");

        assert_eq!(da.name, "设备B", "A 应拿到 B 的名字");
        assert_eq!(db.name, "设备A", "B 应拿到 A 的名字");
        assert_eq!(da.public_key.len(), 32, "公钥应为 32 字节");
        assert_eq!(db.public_key.len(), 32);
        assert_ne!(da.public_key, db.public_key, "双方公钥应不同");
    }

    /// 交换到的公钥必须能直接用于 Noise IK。
    ///
    /// 这是配对与信道之间的接缝，最容易出问题：公钥格式对不上、
    /// 存反了方向、多了长度前缀……都要到真正连接时才暴露。
    #[tokio::test]
    async fn exchanged_keys_actually_work_for_noise() {
        let (mut a, mut b) = tokio::io::duplex(8192);
        let alice = StaticKeypair::generate().expect("生成应成功");
        let bob = StaticKeypair::generate().expect("生成应成功");
        let alice_pub = alice.public.clone();
        let bob_pub = bob.public.clone();

        let ta = tokio::spawn(async move {
            let d = pair_over(&mut a, "271828", Role::Initiator, &alice, "A")
                .await
                .expect("A 配对应成功");
            (d, alice)
        });
        let tb = tokio::spawn(async move {
            let d = pair_over(&mut b, "271828", Role::Responder, &bob, "B")
                .await
                .expect("B 配对应成功");
            (d, bob)
        });
        let (a_learned, a_keys) = ta.await.expect("任务应完成");
        let (b_learned, b_keys) = tb.await.expect("任务应完成");

        // 交换到的公钥应当与对方真实公钥一致
        assert_eq!(a_learned.public_key, bob_pub, "A 拿到的应是 B 的真实公钥");
        assert_eq!(b_learned.public_key, alice_pub, "B 拿到的应是 A 的真实公钥");

        // 用配对得来的公钥真的建一条 Noise 信道
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("监听应成功");
        let addr = listener.local_addr().expect("取地址应成功");
        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept 应成功");
            crate::channel::Channel::accept(sock, &b_keys).await
        });
        let sock = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let cli =
            crate::channel::Channel::connect(sock, &a_keys, &a_learned.public_key).await;

        assert!(cli.is_ok(), "配对交换的公钥必须能直接用于 Noise IK 握手");
        assert!(srv.await.expect("任务应完成").is_ok());
    }

    /// PIN 不匹配时**两侧都**必须失败。
    #[tokio::test]
    async fn mismatched_pin_fails_on_both_sides() {
        let (ra, rb) = run_pair("111111", "222222").await;
        assert!(ra.is_err(), "A 侧应失败");
        assert!(rb.is_err(), "B 侧应失败");
    }

    /// 两个方向必须用不同的 nonce。
    ///
    /// 同一密钥 + 同一 nonce 加密两条不同消息会直接泄露明文异或值
    /// 并使认证失效。这条测试固定住这个不变量。
    #[test]
    fn directions_use_distinct_nonces() {
        assert_ne!(
            nonce_for(Role::Initiator),
            nonce_for(Role::Responder),
            "两个方向的 nonce 必须不同，否则同密钥重用 nonce"
        );
    }

    /// 用同一方向的 nonce 解密另一方向的密文必须失败。
    #[test]
    fn cross_direction_decryption_fails() {
        let key = [7u8; 32];
        let ct = seal(&key, Role::Initiator, b"secret").expect("加密应成功");
        assert!(
            open(&key, Role::Initiator, &ct).is_ok(),
            "同方向应能解开"
        );
        assert!(
            open(&key, Role::Responder, &ct).is_err(),
            "跨方向解密必须失败"
        );
    }

    #[test]
    fn identity_roundtrip() {
        let pk = vec![0xABu8; 32];
        let enc = encode_identity(&pk, "我的电脑").expect("编码应成功");
        let (got_pk, got_name) = decode_identity(&enc).expect("解码应成功");
        assert_eq!(got_pk, pk);
        assert_eq!(got_name, "我的电脑");
    }

    #[test]
    fn identity_rejects_wrong_key_length() {
        // 公钥不是 32 字节
        let enc = encode_identity(&[0u8; 16], "x").expect("编码应成功");
        assert!(
            decode_identity(&enc).is_err(),
            "非 32 字节公钥应拒绝，否则会在 Noise 握手时以更难排查的方式失败"
        );
    }

    #[test]
    fn identity_rejects_malformed() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],                    // 空
            vec![32],                  // 只有长度
            vec![32, 1, 2, 3],         // 公钥被截断
            {
                let mut v = vec![32];
                v.extend_from_slice(&[0u8; 32]);
                v // 缺名字长度
            },
            {
                let mut v = vec![32];
                v.extend_from_slice(&[0u8; 32]);
                v.push(10);
                v.extend_from_slice(b"abc"); // 名字被截断
                v
            },
            {
                let mut v = vec![32];
                v.extend_from_slice(&[0u8; 32]);
                v.push(1);
                v.extend_from_slice(b"ab"); // 尾部多余字节
                v
            },
        ];
        for c in cases {
            assert!(decode_identity(&c).is_err(), "畸形输入应拒绝: {c:?}");
        }
    }

    #[test]
    fn identity_rejects_control_chars_in_name() {
        let mut v = vec![32];
        v.extend_from_slice(&[0u8; 32]);
        let bad = "名字\u{1b}[31m";
        v.push(u8::try_from(bad.len()).expect("长度应可转换"));
        v.extend_from_slice(bad.as_bytes());
        assert!(
            decode_identity(&v).is_err(),
            "含控制字符的设备名必须拒绝"
        );
    }

    #[test]
    fn identity_rejects_invalid_utf8() {
        let mut v = vec![32];
        v.extend_from_slice(&[0u8; 32]);
        v.push(3);
        v.extend_from_slice(&[0xFF, 0xFE, 0xFD]);
        assert!(decode_identity(&v).is_err(), "非法 UTF-8 应拒绝");
    }

    #[tokio::test]
    async fn rejects_bad_local_name() {
        let (mut a, _b) = tokio::io::duplex(8192);
        let ka = StaticKeypair::generate().expect("生成应成功");
        let r = pair_over(&mut a, "123456", Role::Initiator, &ka, "").await;
        assert!(r.is_err(), "空设备名应在发起前就被拒绝");
    }

    #[tokio::test]
    async fn rejects_bad_pin_format() {
        let (mut a, _b) = tokio::io::duplex(8192);
        let ka = StaticKeypair::generate().expect("生成应成功");
        let r = pair_over(&mut a, "12345", Role::Initiator, &ka, "x").await;
        assert!(r.is_err(), "PIN 位数不对应拒绝");
    }

    /// 配对期消息上限必须远低于常规帧上限，让畸形输入尽早失败。
    ///
    /// 用编译期断言而非运行时 assert：两者都是常量，运行时检查
    /// 永远不会失败，等于没测。写成 const 断言后，若有人把
    /// `MAX_PAIR_CHUNK` 调大到超过帧上限，**编译就过不去**。
    const _: () = assert!(MAX_PAIR_CHUNK < crate::wire::MAX_FRAME);
}
