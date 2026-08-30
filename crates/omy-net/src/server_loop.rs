//! 服务端主循环：接受连接、核对身份、服务请求。
//!
//! # 这一层要解决的问题
//!
//! 前面的模块都是"给定输入算出输出"的纯逻辑。主循环第一次面对
//! **敌意的并发环境**，几件事必须在这里落实：
//!
//! | 问题 | 不处理的后果 |
//! |---|---|
//! | 未授权连接 | 任何人都能读共享文件 |
//! | 握手慢连接 | 攻击者连上不发数据，耗尽连接槽 |
//! | 连接数无上限 | 几千个连接打满内存与文件描述符 |
//! | 单连接异常拖垮全局 | 一个畸形客户端让所有人无法访问 |
//!
//! # 为什么每个连接都要独立超时
//!
//! Noise 握手需要两次往返。攻击者可以连上 TCP 但**永不发送**握手消息，
//! 服务端就会一直等在 `read_exact` 上。几百个这样的连接就能占满
//! 并发上限，正常用户再也连不进来——这是最廉价的拒绝服务方式，
//! 不需要带宽也不需要算力。
//!
//! 因此握手阶段有独立且较短的超时（[`HANDSHAKE_TIMEOUT`]），
//! 已建立的连接则用较长的空闲超时（[`IDLE_TIMEOUT`]）。
//!
//! # 锁屏不影响服务（DEC-16）
//!
//! 本模块**不持有任何密钥**：`Server` 只有 [`CiphertextSource`]，
//! `Store` 只用于查已配对设备。共享方 vault 锁定与否对这里没有影响，
//! 这正是决策 DEC-16 的实现体现。
//!
//! [`CiphertextSource`]: crate::serve::CiphertextSource

use crate::channel::{Channel, StaticKeypair};
use crate::error::{NetError, Result};
use crate::serve::{Server, Session};
use crate::store::Store;
use crate::wire::{Request, Response};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// 握手必须在此时间内完成。
///
/// 取值偏短：正常握手是两次局域网往返，毫秒级即可完成。给到 10 秒
/// 已经非常宽松，同时让"连上不说话"的连接快速被清掉。
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 已建立连接的空闲超时。
///
/// 与文档 §5.3 的「60 秒无活动自动关闭」一致。播放视频时会持续发
/// READ 请求，不会触发；真正空闲的连接则及时释放。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// 同时服务的最大连接数。
///
/// 局域网场景下几台设备而已，32 远超实际需要。设上限是为了防止
/// 恶意端无限建连——没有上限的话，接受连接这个动作本身就是攻击面。
pub const MAX_CONNECTIONS: usize = 32;

/// 服务端配置。
pub struct ServeConfig {
    /// 监听地址。`0` 端口表示由系统分配。
    pub bind: SocketAddr,
    /// 广播给局域网的设备名。`None` 表示不广播（仅接受直连）。
    pub advertise_as: Option<String>,
    /// 握手超时。
    pub handshake_timeout: Duration,
    /// 空闲超时。
    pub idle_timeout: Duration,
    /// 最大并发连接数。
    pub max_connections: usize,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            // 默认绑所有网卡：局域网共享的意义就在于被同网段设备访问。
            // 端口交给系统分配，避免与其他程序冲突——真实端口通过
            // mDNS 广播，客户端不需要预先知道
            bind: SocketAddr::from(([0, 0, 0, 0], 0)),
            advertise_as: None,
            handshake_timeout: HANDSHAKE_TIMEOUT,
            idle_timeout: IDLE_TIMEOUT,
            max_connections: MAX_CONNECTIONS,
        }
    }
}

/// 一次连接的处理结果，用于访问日志与统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnOutcome {
    /// 正常服务后断开。
    Served {
        /// 对方指纹。
        peer: [u8; 8],
        /// 处理的请求数。
        requests: usize,
    },
    /// 握手失败或超时。
    HandshakeFailed,
    /// 握手成功但不是已配对设备。
    ///
    /// 这条要与 [`Self::HandshakeFailed`] 区分：前者可能只是网络问题，
    /// 后者说明**有设备主动尝试访问但未获授权**，值得提示用户。
    Unauthorized {
        /// 对方指纹，可用于提示"是否要配对这台设备"。
        peer: [u8; 8],
    },
    /// 因超出并发上限被拒。
    Rejected,
}

/// 正在运行的服务。
pub struct RunningServer {
    /// 实际监听地址，`bind` 用 0 端口时由此获知真实端口。
    local_addr: SocketAddr,
    /// 停止信号。
    shutdown: tokio::sync::watch::Sender<bool>,
    /// 主循环任务。
    task: tokio::task::JoinHandle<()>,
    /// mDNS 广播，`Drop` 时自动注销。
    _advertiser: Option<crate::discovery::Advertiser>,
}

impl RunningServer {
    /// 实际监听地址。
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 请求停止并等待主循环退出。
    ///
    /// # Errors
    /// 主循环任务 panic 时返回错误。
    pub async fn shutdown(self) -> Result<()> {
        // 先发信号让 accept 循环退出，再等任务结束。
        // 忽略发送错误：接收端已经没了说明循环本就退出了
        let _ = self.shutdown.send(true);
        self.task
            .await
            .map_err(|e| NetError::Discovery(format!("主循环异常退出: {e}")))?;
        Ok(())
    }
}

/// 启动服务端。
///
/// 立即返回，服务在后台运行。用 [`RunningServer::shutdown`] 停止。
///
/// `on_event` 在每个连接结束时被调用，用于访问日志。它在主循环的
/// 任务里执行，**不应阻塞**——要做耗时的事请自行发到别的通道。
///
/// # Errors
/// 绑定端口失败或 mDNS 广播启动失败时返回错误。
pub async fn serve<F>(
    cfg: ServeConfig,
    server: Arc<Server>,
    store: Arc<Store>,
    keypair: Arc<StaticKeypair>,
    mut on_event: F,
) -> Result<RunningServer>
where
    F: FnMut(ConnOutcome) + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    let local_addr = listener.local_addr()?;

    // 广播真实端口，而不是配置里可能为 0 的那个
    let advertiser = match cfg.advertise_as.as_deref() {
        Some(name) => Some(crate::discovery::Advertiser::start(
            name,
            &keypair.public,
            local_addr.port(),
        )?),
        None => None,
    };

    let (tx, mut rx) = tokio::sync::watch::channel(false);

    // 事件通道：连接任务把结果发到这里，由独立任务调用 on_event。
    //
    // ⚠️ 绝不能在 accept 循环里等连接结束——那样就变成"一次只服务一个
    // 连接"，并发上限形同虚设，而且一个慢客户端会挡住所有人。
    // 这个错误在写第一版时犯过：用 oneshot 在循环里 await，
    // 表面上能跑通测试（测试都是单连接），实际是串行的。
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<ConnOutcome>();
    let ev_task = tokio::spawn(async move {
        while let Some(o) = ev_rx.recv().await {
            on_event(o);
        }
    });

    // 用信号量限制并发。permit 随连接任务一起 drop，
    // 不需要手动归还——手动归还必然在某条错误路径上被忘掉
    let sem = Arc::new(tokio::sync::Semaphore::new(cfg.max_connections));
    let handshake_timeout = cfg.handshake_timeout;
    let idle_timeout = cfg.idle_timeout;

    let task = tokio::spawn(async move {
        loop {
            tokio::select! {
                // 优先检查停止信号，避免停止后又接了新连接
                biased;

                changed = rx.changed() => {
                    if changed.is_err() || *rx.borrow() {
                        break;
                    }
                }

                accepted = listener.accept() => {
                    let Ok((sock, _peer)) = accepted else {
                        // accept 失败通常是临时的（文件描述符耗尽等）。
                        // 直接退出会让服务静默消失，稍等再试更合适
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };

                    let Ok(permit) = Arc::clone(&sem).try_acquire_owned() else {
                        // 超出并发上限：立即断开而不是排队。
                        // 排队会让攻击者用连接把正常用户挤到队尾
                        drop(sock);
                        let _ = ev_tx.send(ConnOutcome::Rejected);
                        continue;
                    };

                    let srv = Arc::clone(&server);
                    let st = Arc::clone(&store);
                    let kp = Arc::clone(&keypair);
                    let etx = ev_tx.clone();

                    // 立刻 spawn 并**不等待**，循环马上回到 accept。
                    //
                    // 把这里改成直接 await（第一版就是这么写的），
                    // serves_multiple_clients_concurrently 与
                    // enforces_connection_limit 会失败，其余 7 条
                    // 单连接测试照样全绿——已实测验证过
                    tokio::spawn(async move {
                        let outcome = handle_conn(
                            sock, &srv, &st, &kp, handshake_timeout, idle_timeout,
                        )
                        .await;
                        // permit 在此 drop，连接槽自动归还
                        drop(permit);
                        let _ = etx.send(outcome);
                    });
                }
            }
        }
        // 循环退出后关掉事件通道，让事件任务收尾
        drop(ev_tx);
        let _ = ev_task.await;
    });

    Ok(RunningServer {
        local_addr,
        shutdown: tx,
        task,
        _advertiser: advertiser,
    })
}

/// 处理单个连接。
async fn handle_conn(
    sock: tokio::net::TcpStream,
    server: &Server,
    store: &Store,
    keypair: &StaticKeypair,
    handshake_timeout: Duration,
    idle_timeout: Duration,
) -> ConnOutcome {
    // 关掉 Nagle：请求-响应模式下延迟合并会让每次往返多等 40 ms，
    // 视频 seek 时用户能直接感觉到
    let _ = sock.set_nodelay(true);

    // 握手必须限时。攻击者连上但不发数据是最廉价的拒绝服务方式
    let Ok(Ok(mut ch)) =
        tokio::time::timeout(handshake_timeout, Channel::accept(sock, keypair)).await
    else {
        return ConnOutcome::HandshakeFailed;
    };

    // 握手成功只证明对方持有某个私钥。必须核对是不是**已配对且未过期**
    // 的设备——这一步由 Session::authorize 强制
    let peer_fp = ch.peer_fingerprint();
    let Ok(session) = Session::authorize(store, ch.peer_public()) else {
        // 不给出"你没被授权"之外的任何信息：区分"不认识"和"已过期"
        // 会帮助攻击者判断自己是否曾被配对过
        return ConnOutcome::Unauthorized { peer: peer_fp };
    };

    let mut requests = 0usize;
    loop {
        let Ok(recv) = tokio::time::timeout(idle_timeout, ch.recv()).await else {
            break; // 空闲超时
        };
        let Ok(raw) = recv else {
            break; // 对方关闭或解密失败
        };

        let resp = match Request::decode(&raw) {
            Ok(req) => {
                requests = requests.saturating_add(1);
                session.handle(server, &req)
            }
            // 畸形请求回错误而不是断开：可能只是版本差异，
            // 让对方知道原因比静默断线更有用
            Err(_) => Response::Err {
                code: crate::wire::ErrCode::BadRequest,
                msg: "无法解析的请求".into(),
            },
        };

        let Ok(enc) = resp.encode() else { break };
        if ch.send(&enc).await.is_err() {
            break;
        }
    }

    ConnOutcome::Served { peer: peer_fp, requests }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::DeviceRecord;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// 测试用事件收集器。
    ///
    /// 包一层是因为 `MutexGuard` 绝不能跨 `await` 持有——那会在异步
    /// 环境里造成死锁，clippy 的 `await_holding_lock` 专门抓这个。
    /// 把取值封装成"进去拿完就出来"的函数，调用点不可能写错。
    #[derive(Clone)]
    struct Events(Arc<Mutex<Vec<ConnOutcome>>>);

    impl Events {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }

        fn push(&self, o: ConnOutcome) {
            if let Ok(mut g) = self.0.lock() {
                g.push(o);
            }
        }

        /// 拿一份快照，锁在函数返回前就释放了。
        fn snapshot(&self) -> Vec<ConnOutcome> {
            self.0.lock().map(|g| g.clone()).unwrap_or_default()
        }

        fn any(&self, f: impl Fn(&ConnOutcome) -> bool) -> bool {
            self.snapshot().iter().any(f)
        }
    }

    fn tmpdir(tag: &str) -> PathBuf {
        use rand::RngCore as _;
        let r = rand::thread_rng().next_u64();
        let d = std::env::temp_dir().join(format!("omy-serverloop-{tag}-{r:016x}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录应成功");
        d
    }

    fn make_omy(dir: &Path, name: &str) -> PathBuf {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};
        let p = dir.join(name);
        let salt = [1u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"pw", &salt, params).expect("派生应成功");
        let opts = EncryptOptions {
            filename: Some(name.to_owned()),
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(b"content", &[kek], &salt, &opts, &RandomMaterial::generate())
            .expect("加密应成功");
        std::fs::write(&p, &enc.bytes).expect("写文件应成功");
        p
    }

    /// 起一个服务，返回（服务、客户端密钥、事件收集器）。
    async fn spawn_server(
        dir: &Path,
        authorize_client: bool,
    ) -> (RunningServer, StaticKeypair, Vec<u8>, Events) {
        let share = crate::serve::Share::from_dir(dir).expect("扫描应成功");
        let server = Arc::new(Server::new(share));

        let host_kp = StaticKeypair::generate().expect("生成应成功");
        let host_pub = host_kp.public.clone();
        let client_kp = StaticKeypair::generate().expect("生成应成功");

        let mut store = Store::create("测试主机").expect("创建应成功");
        if authorize_client {
            store.upsert(DeviceRecord {
                public_key: client_kp.public.clone(),
                name: "测试客户端".into(),
                paired_at: 0,
                expires_at: 0,
            });
        }

        let events = Events::new();
        let ev = events.clone();

        let cfg = ServeConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            advertise_as: None, // 测试不广播，避免污染局域网
            handshake_timeout: Duration::from_millis(500),
            idle_timeout: Duration::from_millis(500),
            max_connections: 2,
        };

        let running = serve(
            cfg,
            server,
            Arc::new(store),
            Arc::new(host_kp),
            move |o| ev.push(o),
        )
        .await
        .expect("启动应成功");

        (running, client_kp, host_pub, events)
    }

    #[tokio::test]
    async fn serves_authorized_client() {
        let d = tmpdir("ok");
        make_omy(&d, "a.omy");
        let (running, client_kp, host_pub, events) = spawn_server(&d, true).await;

        let sock = tokio::net::TcpStream::connect(running.local_addr())
            .await
            .expect("连接应成功");
        let mut ch = Channel::connect(sock, &client_kp, &host_pub)
            .await
            .expect("握手应成功");

        let resp = ch.request(&Request::Ping).await.expect("PING 应成功");
        assert_eq!(resp, Response::Pong);

        let list = ch.request(&Request::List).await.expect("LIST 应成功");
        match list {
            Response::ListOk { entries } => assert_eq!(entries.len(), 1),
            other => panic!("应返回 ListOk，实际 {other:?}"),
        }

        drop(ch);
        // 等连接结束事件
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(
            events.any(|o| matches!(o, ConnOutcome::Served { requests, .. } if *requests >= 2)),
            "应记录已服务的连接，实际 {:?}",
            events.snapshot()
        );

        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 未配对的设备握手能成功，但必须在授权这一步被拒。
    #[tokio::test]
    async fn rejects_unauthorized_client() {
        let d = tmpdir("unauth");
        make_omy(&d, "a.omy");
        let (running, client_kp, host_pub, events) = spawn_server(&d, false).await;

        let sock = tokio::net::TcpStream::connect(running.local_addr())
            .await
            .expect("连接应成功");
        // 握手本身会成功——这正是必须单独做授权检查的原因
        let ch = Channel::connect(sock, &client_kp, &host_pub).await;

        // 服务端会在授权失败后立即断开，客户端可能在握手时就发现，
        // 也可能在第一个请求时才发现。两种都可接受
        if let Ok(mut ch) = ch {
            let r = tokio::time::timeout(
                Duration::from_secs(2),
                ch.request(&Request::List),
            )
            .await;
            assert!(
                matches!(&r, Ok(Err(_)) | Err(_)),
                "未授权设备不得读到文件列表，实际 {r:?}"
            );
        }

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            events.any(|o| matches!(o, ConnOutcome::Unauthorized { .. })),
            "应记录未授权连接，实际 {:?}",
            events.snapshot()
        );

        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 连上但不发握手数据的连接必须被超时清掉。
    ///
    /// 这是最廉价的拒绝服务方式：不需要带宽也不需要算力，
    /// 只要占着连接不说话。
    #[tokio::test]
    async fn silent_connection_times_out() {
        let d = tmpdir("silent");
        let (running, _kp, _pub, events) = spawn_server(&d, true).await;

        let sock = tokio::net::TcpStream::connect(running.local_addr())
            .await
            .expect("连接应成功");
        // 连上就什么都不做
        tokio::time::sleep(Duration::from_millis(900)).await;
        drop(sock);

        assert!(
            events.any(|o| matches!(o, ConnOutcome::HandshakeFailed)),
            "沉默连接应因握手超时被清掉，实际 {:?}",
            events.snapshot()
        );

        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 服务端能停下来，停止后不再接受连接。
    #[tokio::test]
    async fn shutdown_stops_accepting() {
        let d = tmpdir("shutdown");
        let (running, client_kp, host_pub, _events) = spawn_server(&d, true).await;
        let addr = running.local_addr();

        running.shutdown().await.expect("停止应成功");

        // 停止后连接应当失败或握手不成
        let r = tokio::time::timeout(Duration::from_secs(2), async {
            let sock = tokio::net::TcpStream::connect(addr).await.ok()?;
            Channel::connect(sock, &client_kp, &host_pub).await.ok()
        })
        .await;
        // 同上：Channel 不实现 Debug，先归约成布尔
        let connected = matches!(&r, Ok(Some(_)));
        assert!(!connected, "停止后不应还能建立信道");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn local_addr_reports_real_port() {
        let d = tmpdir("port");
        let (running, _kp, _pub, _ev) = spawn_server(&d, true).await;
        assert_ne!(
            running.local_addr().port(),
            0,
            "绑 0 端口后必须能查到系统分配的真实端口，否则 mDNS 广播的端口是错的"
        );
        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn default_config_is_sane() {
        let c = ServeConfig::default();
        assert!(c.handshake_timeout < c.idle_timeout, "握手超时应短于空闲超时");
        assert!(c.max_connections > 0);
        assert_eq!(c.bind.port(), 0, "默认应由系统分配端口");
        assert!(c.advertise_as.is_none(), "默认不广播，须显式开启");
    }

    /// 多个客户端必须能**同时**被服务。
    ///
    /// 这条是针对第一版真实缺陷加的：当时在 accept 循环里 await 连接
    /// 结束，服务实际是串行的——一次只服务一个客户端，后面的要排队。
    /// 所有单连接测试都通过，因为它们从不同时开两条连接。
    ///
    /// 判据是「两条连接同时保持打开时都能正常收发」，而不是
    /// 「两条连接先后都成功」——后者串行实现也能满足。
    #[tokio::test]
    async fn serves_multiple_clients_concurrently() {
        let d = tmpdir("concurrent");
        make_omy(&d, "a.omy");

        let share = crate::serve::Share::from_dir(&d).expect("扫描应成功");
        let server = Arc::new(Server::new(share));
        let host_kp = StaticKeypair::generate().expect("生成应成功");
        let host_pub = host_kp.public.clone();

        let c1 = StaticKeypair::generate().expect("生成应成功");
        let c2 = StaticKeypair::generate().expect("生成应成功");

        let mut store = Store::create("主机").expect("创建应成功");
        for kp in [&c1, &c2] {
            store.upsert(DeviceRecord {
                public_key: kp.public.clone(),
                name: "客户端".into(),
                paired_at: 0,
                expires_at: 0,
            });
        }

        let cfg = ServeConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            advertise_as: None,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(5),
            max_connections: 4,
        };
        let running = serve(cfg, server, Arc::new(store), Arc::new(host_kp), |_| {})
            .await
            .expect("启动应成功");
        let addr = running.local_addr();

        // 先建立两条连接，**都不关闭**
        let s1 = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let mut ch1 = Channel::connect(s1, &c1, &host_pub).await.expect("握手应成功");
        let s2 = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let mut ch2 = Channel::connect(s2, &c2, &host_pub).await.expect("握手应成功");

        // 两条都还开着的情况下交替收发。若服务是串行的，
        // 第二条根本走不到这一步（它还在队列里等第一条结束）
        for _ in 0..3 {
            let r1 = tokio::time::timeout(Duration::from_secs(2), ch1.request(&Request::Ping))
                .await
                .expect("连接 1 不应超时")
                .expect("连接 1 请求应成功");
            let r2 = tokio::time::timeout(Duration::from_secs(2), ch2.request(&Request::Ping))
                .await
                .expect("连接 2 不应超时——超时说明服务端是串行的")
                .expect("连接 2 请求应成功");
            assert_eq!(r1, Response::Pong);
            assert_eq!(r2, Response::Pong);
        }

        drop(ch1);
        drop(ch2);
        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 超出并发上限的连接被拒，且**不影响**已有连接。
    #[tokio::test]
    async fn enforces_connection_limit() {
        let d = tmpdir("limit");
        let share = crate::serve::Share::from_dir(&d).expect("扫描应成功");
        let server = Arc::new(Server::new(share));
        let host_kp = StaticKeypair::generate().expect("生成应成功");
        let host_pub = host_kp.public.clone();
        let client = StaticKeypair::generate().expect("生成应成功");

        let mut store = Store::create("主机").expect("创建应成功");
        store.upsert(DeviceRecord {
            public_key: client.public.clone(),
            name: "客户端".into(),
            paired_at: 0,
            expires_at: 0,
        });

        let events = Events::new();
        let ev = events.clone();
        let cfg = ServeConfig {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            advertise_as: None,
            handshake_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(5),
            max_connections: 1, // 只允许一条
        };
        let running = serve(cfg, server, Arc::new(store), Arc::new(host_kp), move |o| {
            ev.push(o);
        })
        .await
        .expect("启动应成功");
        let addr = running.local_addr();

        // 第一条占住唯一的槽
        let s1 = tokio::net::TcpStream::connect(addr).await.expect("连接应成功");
        let mut ch1 = Channel::connect(s1, &client, &host_pub).await.expect("握手应成功");
        assert_eq!(
            ch1.request(&Request::Ping).await.expect("首条连接应可用"),
            Response::Pong
        );

        // 第二条应被拒
        let s2 = tokio::net::TcpStream::connect(addr).await.expect("TCP 连接本身会成功");
        let second = tokio::time::timeout(
            Duration::from_secs(2),
            Channel::connect(s2, &client, &host_pub),
        )
        .await;
        // Channel 有意不实现 Debug（内含 Noise 传输态与密钥材料，
        // 误打印就是泄露），所以先归约成布尔再断言
        let second_ok = matches!(&second, Ok(Ok(_)));
        assert!(
            !second_ok,
            "超出上限的连接不应握手成功"
        );

        // 关键：第一条**仍然可用**。上限机制不能误伤已有连接
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), ch1.request(&Request::Ping))
                .await
                .expect("不应超时")
                .expect("已有连接应仍然可用"),
            Response::Pong,
            "拒绝新连接不得影响已建立的连接"
        );

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            events.any(|o| matches!(o, ConnOutcome::Rejected)),
            "应记录被拒的连接，实际 {:?}",
            events.snapshot()
        );

        drop(ch1);
        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 畸形请求回错误而不是断开连接。
    #[tokio::test]
    async fn malformed_request_does_not_kill_connection() {
        let d = tmpdir("malformed");
        make_omy(&d, "a.omy");
        let (running, client_kp, host_pub, _ev) = spawn_server(&d, true).await;

        let sock = tokio::net::TcpStream::connect(running.local_addr())
            .await
            .expect("连接应成功");
        let mut ch = Channel::connect(sock, &client_kp, &host_pub)
            .await
            .expect("握手应成功");

        // 发一段无法解析的字节
        ch.send(&[0xFF, 0xEE, 0xDD]).await.expect("发送应成功");
        let raw = ch.recv().await.expect("应收到错误响应而不是断线");
        let resp = Response::decode(&raw).expect("响应应可解析");
        assert!(
            matches!(&resp, Response::Err { code, .. } if *code == crate::wire::ErrCode::BadRequest),
            "畸形请求应返回 BAD_REQUEST，实际 {resp:?}"
        );

        // 连接仍然可用
        assert_eq!(
            ch.request(&Request::Ping).await.expect("连接应仍可用"),
            Response::Pong,
            "一个畸形请求不应让整条连接失效"
        );

        drop(ch);
        running.shutdown().await.expect("停止应成功");
        let _ = std::fs::remove_dir_all(&d);
    }
}
