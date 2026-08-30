//! Noise IK 加密信道。
//!
//! # 为什么是 IK
//!
//! IK 表示「发起方**已知**（Known）响应方的静态公钥」。这正是配对之后
//! 的状态：配对时双方交换了静态公钥并持久化。由此：
//!
//! - **1-RTT**：两条消息完成握手，不需要额外往返；
//! - **前向保密**：每次会话有独立的临时密钥，长期私钥泄露也解不开
//!   历史流量；
//! - **响应方身份被验证**：中间人没有响应方私钥，握手必定失败。
//!
//! 发起方的身份则通过握手中加密传输的静态公钥来验证——服务端拿到后
//! 与已配对设备列表比对，不在列表里就断开。
//!
//! # 分帧：TCP 是字节流
//!
//! Noise 只定义单条消息的加解密，不管消息边界。TCP 也不保证一次 read
//! 拿到完整消息。因此每条消息前加 2 字节大端长度前缀——
//! Noise 消息上限 65535，正好用 `u16` 表示，不会浪费也不会溢出。
//!
//! 读取时**先读满 2 字节长度，再读满对应字节数**，两处都用 `read_exact`。
//! 少了这一步，在真实网络（尤其是大消息跨多个 TCP 段）上会随机出错，
//! 而本机 loopback 测试往往一次就读全，测不出来。
//!
//! # 单条消息的容量
//!
//! Spike 实测：Noise 传输态单条消息的**明文**上限是 65519 字节
//! （65535 密文上限减 16 字节 AEAD tag）。[`crate::wire::MAX_FRAME`]
//! 取的就是这个值。

use crate::error::{NetError, Result};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// Noise 握手模式。
///
/// 改动它等同于协议不兼容：双方必须用完全相同的模式串。
pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

/// Noise 单条消息的密文上限（协议规定）。
const NOISE_MAX_MSG: usize = 65535;

/// 静态密钥对。
///
/// 配对时生成并持久化，之后每次连接都复用。
pub struct StaticKeypair {
    /// 公钥，会广播指纹并在配对时交换。
    pub public: Vec<u8>,
    /// 私钥。**绝不能离开本机。**
    private: Vec<u8>,
}

impl StaticKeypair {
    /// 生成一对新的静态密钥。
    ///
    /// # Errors
    /// 底层随机源不可用时返回错误。
    pub fn generate() -> Result<Self> {
        let params: snow::params::NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|_| NetError::Noise("Noise 模式串解析失败".into()))?;
        let kp = snow::Builder::new(params).generate_keypair()?;
        Ok(Self { public: kp.public, private: kp.private })
    }

    /// 从已保存的字节恢复。
    #[must_use]
    pub fn from_parts(public: Vec<u8>, private: Vec<u8>) -> Self {
        Self { public, private }
    }

    /// 私钥字节。
    ///
    /// 仅用于持久化。调用方必须保证写入的位置是加密的——
    /// 静态私钥泄露意味着攻击者可以冒充本设备。
    #[must_use]
    pub fn private_bytes(&self) -> &[u8] {
        &self.private
    }

    /// 本设备的指纹。
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 8] {
        crate::discovery::fingerprint(&self.public)
    }
}

impl Drop for StaticKeypair {
    fn drop(&mut self) {
        use zeroize::Zeroize as _;
        self.private.zeroize();
    }
}

/// 已建立的加密信道。
pub struct Channel<S> {
    stream: S,
    noise: snow::TransportState,
    /// 对方的静态公钥，握手中获得。
    peer_public: Vec<u8>,
}

impl<S> Channel<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// 作为发起方建立信道。
    ///
    /// `peer_public` 是配对时保存的对方静态公钥。若对方换了密钥
    /// （重装、恢复出厂），握手会失败——这是**期望行为**，
    /// 说明需要重新配对，不应自动接受新密钥。
    ///
    /// # Errors
    /// 握手失败、网络错误或协议不匹配时返回错误。
    pub async fn connect(
        mut stream: S,
        local: &StaticKeypair,
        peer_public: &[u8],
    ) -> Result<Self> {
        let params: snow::params::NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|_| NetError::Noise("Noise 模式串解析失败".into()))?;
        // snow 的 builder 每个方法都返回 Result（重复设参会报
        // ParameterOverwrite），不能像常见 builder 那样直接链式调用
        let mut hs = snow::Builder::new(params)
            .local_private_key(local.private_bytes())?
            .remote_public_key(peer_public)?
            .build_initiator()?;

        let mut buf = vec![0u8; NOISE_MAX_MSG];

        // -> e, es, s, ss
        let n = hs.write_message(&[], &mut buf)?;
        write_frame(&mut stream, buf.get(..n).unwrap_or(&[])).await?;

        // <- e, ee, se
        let msg = read_frame(&mut stream).await?;
        hs.read_message(&msg, &mut buf)?;

        if !hs.is_handshake_finished() {
            return Err(NetError::Noise("握手未完成".into()));
        }
        let peer = hs.get_remote_static().unwrap_or(&[]).to_vec();
        let noise = hs.into_transport_mode()?;
        Ok(Self { stream, noise, peer_public: peer })
    }

    /// 作为响应方接受信道。
    ///
    /// 响应方**不需要**预知对方公钥：IK 模式下发起方会在握手中把自己的
    /// 静态公钥加密发过来。握手完成后用 [`Self::peer_public`] 取出，
    /// 与已配对设备列表比对——不在列表里就应当立即断开。
    ///
    /// # Errors
    /// 握手失败或网络错误时返回错误。
    pub async fn accept(mut stream: S, local: &StaticKeypair) -> Result<Self> {
        let params: snow::params::NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|_| NetError::Noise("Noise 模式串解析失败".into()))?;
        let mut hs = snow::Builder::new(params)
            .local_private_key(local.private_bytes())?
            .build_responder()?;

        let mut buf = vec![0u8; NOISE_MAX_MSG];

        let msg = read_frame(&mut stream).await?;
        hs.read_message(&msg, &mut buf)?;

        let n = hs.write_message(&[], &mut buf)?;
        write_frame(&mut stream, buf.get(..n).unwrap_or(&[])).await?;

        if !hs.is_handshake_finished() {
            return Err(NetError::Noise("握手未完成".into()));
        }
        let peer = hs.get_remote_static().unwrap_or(&[]).to_vec();
        let noise = hs.into_transport_mode()?;
        Ok(Self { stream, noise, peer_public: peer })
    }

    /// 对方的静态公钥。
    ///
    /// 服务端**必须**在处理任何请求前用它比对已配对设备列表。
    /// 握手成功只证明对方持有某个私钥，不证明那是你认识的设备。
    #[must_use]
    pub fn peer_public(&self) -> &[u8] {
        &self.peer_public
    }

    /// 对方的指纹。
    #[must_use]
    pub fn peer_fingerprint(&self) -> [u8; 8] {
        crate::discovery::fingerprint(&self.peer_public)
    }

    /// 发送一条消息。
    ///
    /// # Errors
    /// 消息超长、加密失败或网络错误时返回错误。
    pub async fn send(&mut self, plaintext: &[u8]) -> Result<()> {
        if plaintext.len() > crate::wire::MAX_FRAME {
            return Err(NetError::FrameTooLarge);
        }
        let mut buf = vec![0u8; NOISE_MAX_MSG];
        let n = self.noise.write_message(plaintext, &mut buf)?;
        write_frame(&mut self.stream, buf.get(..n).unwrap_or(&[])).await
    }

    /// 收一条消息。
    ///
    /// # Errors
    /// 网络错误、解密失败（消息被篡改）或对方关闭连接时返回错误。
    pub async fn recv(&mut self) -> Result<Vec<u8>> {
        let ct = read_frame(&mut self.stream).await?;
        let mut buf = vec![0u8; NOISE_MAX_MSG];
        // 解密失败说明消息被篡改或密钥不同步，两种都必须断开：
        // 继续用一个状态已经错乱的信道毫无意义
        let n = self.noise.read_message(&ct, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// 发一个请求并等应答。
    ///
    /// # Errors
    /// 编解码或网络失败时返回错误；对方返回错误响应时返回
    /// [`NetError::Remote`]。
    pub async fn request(&mut self, req: &crate::wire::Request) -> Result<crate::wire::Response> {
        self.send(&req.encode()?).await?;
        let raw = self.recv().await?;
        let resp = crate::wire::Response::decode(&raw)?;
        crate::serve::check_response(resp)
    }
}

/// 写一条带 2 字节大端长度前缀的帧。
async fn write_frame<S>(stream: &mut S, data: &[u8]) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let len = u16::try_from(data.len()).map_err(|_| NetError::FrameTooLarge)?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(data).await?;
    stream.flush().await?;
    Ok(())
}

/// 读一条带 2 字节大端长度前缀的帧。
///
/// 两处都用 `read_exact`：TCP 不保证一次 read 拿到完整数据，
/// 大消息在真实网络上会跨多个段到达。
async fn read_frame<S>(stream: &mut S) -> Result<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut lenb = [0u8; 2];
    stream.read_exact(&mut lenb).await?;
    let len = usize::from(u16::from_be_bytes(lenb));
    // 长度上限由 u16 天然保证（≤65535），不需要额外校验，
    // 也就不存在"按对方声称的长度盲目分配"的问题
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{Request, Response};

    /// 建立一对已连接的信道。
    async fn pair() -> (Channel<tokio::net::TcpStream>, Channel<tokio::net::TcpStream>) {
        let srv_kp = StaticKeypair::generate().expect("生成密钥应成功");
        let cli_kp = StaticKeypair::generate().expect("生成密钥应成功");
        let srv_pub = srv_kp.public.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("监听应成功");
        let addr = listener.local_addr().expect("取地址应成功");

        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept 应成功");
            Channel::accept(sock, &srv_kp).await.expect("响应方握手应成功")
        });

        let sock = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let cli = Channel::connect(sock, &cli_kp, &srv_pub)
            .await
            .expect("发起方握手应成功");
        let srv = srv.await.expect("服务端任务应完成");
        (cli, srv)
    }

    #[tokio::test]
    async fn handshake_and_roundtrip() {
        let (mut cli, mut srv) = pair().await;

        cli.send(b"hello").await.expect("发送应成功");
        let got = srv.recv().await.expect("接收应成功");
        assert_eq!(got, b"hello");

        srv.send(b"world").await.expect("发送应成功");
        let got = cli.recv().await.expect("接收应成功");
        assert_eq!(got, b"world");
    }

    /// 服务端必须能拿到发起方的公钥，否则无法比对已配对设备。
    #[tokio::test]
    async fn responder_learns_initiator_public_key() {
        let (cli, srv) = pair().await;
        assert!(!srv.peer_public().is_empty(), "响应方应拿到发起方公钥");
        assert!(!cli.peer_public().is_empty(), "发起方应拿到响应方公钥");
        assert_eq!(
            srv.peer_fingerprint().len(),
            8,
            "指纹应为 8 字节，供比对已配对设备"
        );
    }

    /// 连错公钥必须失败——这正是中间人防护。
    ///
    /// 断言**发起方**失败，而不是「任一方失败」。用 `||` 会让测试在
    /// 「服务端因别的原因先断开」时也通过，那样就验证不到真正的防护点：
    /// IK 模式下发起方在第一条消息里就用对方公钥做 DH，公钥不对则
    /// 响应方解不开，发起方也收不到有效应答。
    #[tokio::test]
    async fn wrong_server_key_fails() {
        let srv_kp = StaticKeypair::generate().expect("生成应成功");
        let cli_kp = StaticKeypair::generate().expect("生成应成功");
        // 客户端拿着**别人**的公钥去连
        let wrong = StaticKeypair::generate().expect("生成应成功");
        let wrong_pub = wrong.public.clone();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("监听应成功");
        let addr = listener.local_addr().expect("取地址应成功");

        let srv = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept 应成功");
            Channel::accept(sock, &srv_kp).await
        });

        let sock = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let cli_res = Channel::connect(sock, &cli_kp, &wrong_pub).await;

        assert!(
            cli_res.is_err(),
            "公钥不匹配时发起方必须失败，否则中间人可以冒充服务端"
        );
        // 响应方也应失败：它解不开用错误公钥加密的第一条消息
        let srv_res = srv.await.expect("任务应完成");
        assert!(srv_res.is_err(), "响应方应拒绝用错误公钥加密的握手消息");
    }

    /// 用正确公钥必须成功——上一条测试的对照。
    ///
    /// 少了这条，即使 `connect` 因为某个 bug 变成"永远失败"，
    /// 上一条也照样通过。
    #[tokio::test]
    async fn correct_key_succeeds() {
        let (cli, srv) = pair().await;
        assert!(!cli.peer_public().is_empty());
        assert!(!srv.peer_public().is_empty());
    }

    /// 单个会话承载多轮请求（连接复用）。
    #[tokio::test]
    async fn many_rounds_on_one_session() {
        let (mut cli, mut srv) = pair().await;
        for i in 0..64u32 {
            let msg = format!("req-{i}");
            cli.send(msg.as_bytes()).await.expect("发送应成功");
            let got = srv.recv().await.expect("接收应成功");
            assert_eq!(got, msg.as_bytes(), "第 {i} 轮内容应一致");

            let resp = vec![u8::try_from(i % 256).unwrap_or(0); 4096];
            srv.send(&resp).await.expect("发送应成功");
            let got = cli.recv().await.expect("接收应成功");
            assert_eq!(got, resp, "第 {i} 轮响应应一致");
        }
    }

    /// 接近上限的大消息必须能过——这是分帧正确性的关键。
    #[tokio::test]
    async fn max_size_message_survives() {
        let (mut cli, mut srv) = pair().await;
        let big = vec![0x5Au8; crate::wire::MAX_FRAME];
        cli.send(&big).await.expect("上限大小的消息应能发送");
        let got = srv.recv().await.expect("接收应成功");
        assert_eq!(got.len(), big.len(), "大消息不应被截断");
        assert_eq!(got, big, "大消息内容应完整");
    }

    #[tokio::test]
    async fn oversized_message_is_rejected_locally() {
        let (mut cli, _srv) = pair().await;
        let too_big = vec![0u8; crate::wire::MAX_FRAME + 1];
        assert!(
            cli.send(&too_big).await.is_err(),
            "超限消息应在本地就被拒绝，而不是发出去让对方失败"
        );
    }

    /// wire 层与信道层能配合工作。
    #[tokio::test]
    async fn request_helper_works() {
        let (mut cli, mut srv) = pair().await;

        let srv_task = tokio::spawn(async move {
            let raw = srv.recv().await.expect("接收应成功");
            let req = Request::decode(&raw).expect("解码应成功");
            assert_eq!(req, Request::Ping);
            let resp = Response::Pong.encode().expect("编码应成功");
            srv.send(&resp).await.expect("发送应成功");
        });

        let resp = cli.request(&Request::Ping).await.expect("请求应成功");
        assert_eq!(resp, Response::Pong);
        srv_task.await.expect("服务端任务应完成");
    }

    /// 错误响应应转成 Err，而不是让调用方自己判断。
    #[tokio::test]
    async fn request_converts_remote_error() {
        let (mut cli, mut srv) = pair().await;

        let srv_task = tokio::spawn(async move {
            let _ = srv.recv().await.expect("接收应成功");
            let resp = Response::Err {
                code: crate::wire::ErrCode::NoSuchHandle,
                msg: "x".into(),
            }
            .encode()
            .expect("编码应成功");
            srv.send(&resp).await.expect("发送应成功");
        });

        let e = cli
            .request(&Request::Stat { handle: [0; 16] })
            .await
            .expect_err("远端错误应转成 Err");
        assert_eq!(e.code(), "NO_SUCH_HANDLE");
        srv_task.await.expect("服务端任务应完成");
    }

    #[test]
    fn keypair_fingerprint_matches_discovery() {
        let kp = StaticKeypair::generate().expect("生成应成功");
        assert_eq!(
            kp.fingerprint(),
            crate::discovery::fingerprint(&kp.public),
            "信道层与发现层必须算出相同的指纹，否则设备匹配会失败"
        );
    }

    /// 一次只肯给 1 字节的流，用来逼出分帧错误。
    ///
    /// loopback 上的大消息往往一次 read 就全拿到，`read_exact` 写成
    /// `read` 也照样通过——直到在真实网络上随机出错。这个包装强制
    /// 每次读取只返回 1 字节，把"必须读满"这件事变成硬性要求。
    struct DripStream {
        inner: tokio::io::DuplexStream,
    }

    impl AsyncRead for DripStream {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            // 只允许本次读取填 1 字节
            let mut one = [0u8; 1];
            let mut small = tokio::io::ReadBuf::new(&mut one);
            match std::pin::Pin::new(&mut self.inner).poll_read(cx, &mut small) {
                std::task::Poll::Ready(Ok(())) => {
                    let filled = small.filled().to_vec();
                    buf.put_slice(&filled);
                    std::task::Poll::Ready(Ok(()))
                }
                other => other,
            }
        }
    }

    impl AsyncWrite for DripStream {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }
        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// 分帧在「每次只能读到 1 字节」的极端情况下仍然正确。
    ///
    /// 这条才真正验证 `read_exact` 的必要性。若把它换成 `read`，
    /// 本测试会立刻失败，而其他所有测试仍然通过。
    #[tokio::test]
    async fn framing_survives_byte_at_a_time_reads() {
        let srv_kp = StaticKeypair::generate().expect("生成应成功");
        let cli_kp = StaticKeypair::generate().expect("生成应成功");
        let srv_pub = srv_kp.public.clone();

        let (a, b) = tokio::io::duplex(1 << 20);
        let srv = tokio::spawn(async move {
            Channel::accept(DripStream { inner: b }, &srv_kp)
                .await
                .expect("响应方握手应成功")
        });
        let mut cli = Channel::connect(DripStream { inner: a }, &cli_kp, &srv_pub)
            .await
            .expect("发起方握手应成功");
        let mut srv = srv.await.expect("服务端任务应完成");

        // 发一条明显跨多次读取的大消息
        let payload = vec![0xC3u8; 40_000];
        cli.send(&payload).await.expect("发送应成功");
        let got = srv.recv().await.expect("接收应成功");
        assert_eq!(got.len(), payload.len(), "逐字节读取下不应截断");
        assert_eq!(got, payload, "逐字节读取下内容应完整");
    }
}
