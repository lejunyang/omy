//! 局域网：设备发现、配对、共享。
//!
//! # 配对为什么要用「任务 + 轮询」而不是一个 async 命令
//!
//! 配对天然要等人：一端显示 6 位配对码，另一端在**另一台设备上**
//! 敲进去。这中间可能过一分钟。
//!
//! 如果做成一个 async 命令一直 await，会有三个问题：
//!
//! | 问题 | 后果 |
//! |---|---|
//! | 前端拿不到中间状态 | 界面只能显示「请稍候」，不知道是在等连接还是在握手 |
//! | 用户无法取消 | 配对码泄露后想立刻作废也做不到 |
//! | 窗口关闭时任务泄漏 | 后台线程还在监听端口 |
//!
//! 所以拆成：`pair_listen_start` 立刻返回配对码 → 前端轮询
//! `pair_status` → 完成或超时。这也正是原型界面 §5 画的交互。
//!
//! # 共享服务的密钥边界
//!
//! `share_start` **不需要**任何文件密码——服务端只搬运密文，
//! 解密在访问端完成（决策 DEC-16）。这一点必须在界面上说清楚，
//! 否则用户会以为开共享等于把密码交出去。

use crate::devices::{DeviceError, DeviceSession, hex8};
use omy_net::store::DeviceRecord;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 配对任务的当前阶段。
///
/// 做成枚举而不是布尔组合：`waiting` + `done` 两个布尔能表达出
/// 「既在等又完成了」这种不存在的状态，而枚举不会。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum PairPhase {
    /// 正在等对方连入。
    Waiting {
        /// 要念给对方听的 6 位配对码。
        pin: String,
        /// 本机监听端口，对方要用它连过来。
        port: u16,
    },
    /// 正在握手。
    Handshaking,
    /// 配对成功。
    Done {
        /// 对方设备名。
        name: String,
        /// 对方指纹，**要人工核对**。
        fingerprint: String,
        /// 本机指纹，对方屏幕上应显示同一个值。
        own_fingerprint: String,
    },
    /// 失败或超时。
    Failed {
        /// 翻译键。
        code: String,
    },
    /// 没有正在进行的配对。
    Idle,
}

/// 配对任务句柄。
pub struct PairTask {
    phase: Arc<Mutex<PairPhase>>,
    /// 用于取消：drop 掉发送端会让监听任务收到通知。
    cancel: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl PairTask {
    /// 新建一个空闲任务。
    #[must_use]
    pub fn new() -> Self {
        Self {
            phase: Arc::new(Mutex::new(PairPhase::Idle)),
            cancel: Mutex::new(None),
        }
    }

    /// 当前阶段。
    pub fn phase(&self) -> PairPhase {
        self.phase
            .lock()
            .map(|g| g.clone())
            .unwrap_or(PairPhase::Idle)
    }

    /// 设置阶段。
    fn set(&self, p: PairPhase) {
        if let Ok(mut g) = self.phase.lock() {
            *g = p;
        }
    }

    /// 取消正在进行的配对。
    ///
    /// 配对码一旦显示过就该能立刻作废——6 位数字的空间只有一百万，
    /// 挂着不管等于给在线猜测留时间窗。
    pub fn cancel(&self) {
        if let Ok(mut g) = self.cancel.lock()
            && let Some(tx) = g.take() {
                // 接收端可能已经走了，发送失败无所谓
                let _ = tx.send(());
            }
        self.set(PairPhase::Idle);
    }

    /// 是否正在进行。
    pub fn is_busy(&self) -> bool {
        matches!(
            self.phase(),
            PairPhase::Waiting { .. } | PairPhase::Handshaking
        )
    }
}

impl Default for PairTask {
    fn default() -> Self {
        Self::new()
    }
}

/// 共享服务的状态。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShareStatus {
    /// 是否正在共享。
    pub running: bool,
    /// 正在共享的目录。
    pub dir: Option<String>,
    /// 监听地址，形如 `0.0.0.0:51234`。
    pub addr: Option<String>,
    /// 共享的文件数。
    pub files: usize,
    /// 已服务过的连接数。
    pub connections: usize,
}

/// 正在运行的共享服务。
pub struct ShareTask {
    inner: Mutex<Option<RunningShare>>,
}

struct RunningShare {
    dir: String,
    addr: String,
    files: usize,
    connections: Arc<std::sync::atomic::AtomicUsize>,
    handle: omy_net::server_loop::RunningServer,
}

impl ShareTask {
    /// 新建一个空闲任务。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// 当前状态。
    pub fn status(&self) -> ShareStatus {
        match self.inner.lock() {
            Ok(g) => match g.as_ref() {
                Some(r) => ShareStatus {
                    running: true,
                    dir: Some(r.dir.clone()),
                    addr: Some(r.addr.clone()),
                    files: r.files,
                    connections: r.connections.load(std::sync::atomic::Ordering::Relaxed),
                },
                None => ShareStatus {
                    running: false,
                    dir: None,
                    addr: None,
                    files: 0,
                    connections: 0,
                },
            },
            Err(_) => ShareStatus {
                running: false,
                dir: None,
                addr: None,
                files: 0,
                connections: 0,
            },
        }
    }

    /// 是否正在运行。
    pub fn is_running(&self) -> bool {
        self.inner.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    fn set(&self, r: Option<RunningShare>) {
        if let Ok(mut g) = self.inner.lock() {
            *g = r;
        }
    }

    /// 取出正在运行的服务（用于停止）。
    fn take(&self) -> Option<RunningShare> {
        self.inner.lock().ok().and_then(|mut g| g.take())
    }
}

impl Default for ShareTask {
    fn default() -> Self {
        Self::new()
    }
}

/// 局域网中发现的一台设备。
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredDevice {
    /// 设备名。**来自对方广播，不可信**。
    pub name: String,
    /// 指纹，用它指代设备。
    pub fingerprint: String,
    /// `IP:端口`，没有可用地址时为 `None`。
    pub addr: Option<String>,
    /// 协议版本是否兼容。
    pub compatible: bool,
    /// 是否已配对。
    pub paired: bool,
    /// 已配对但授权过期。
    pub expired: bool,
}

/// 搜索局域网。
///
/// # Errors
///
/// mDNS 失败时返回。
pub fn discover(
    session: &DeviceSession,
    timeout: Duration,
) -> Result<Vec<DiscoveredDevice>, DeviceError> {
    let found = omy_net::discovery::browse(timeout).map_err(|_| DeviceError::Internal)?;

    // 已配对判断走**指纹**：广播里没有完整公钥，就算有也不能信——
    // 任何人都能广播任意内容
    let known: Vec<([u8; 8], bool)> = session
        .read(|s| {
            s.devices()
                .iter()
                .map(|d| (d.fingerprint(), d.is_expired()))
                .collect()
        })
        .unwrap_or_default();

    Ok(found
        .iter()
        .map(|p| {
            let hit = known.iter().find(|(fp, _)| *fp == p.fingerprint);
            DiscoveredDevice {
                name: p.name.clone(),
                fingerprint: p.fingerprint_hex(),
                addr: p
                    .preferred_addr()
                    .map(|ip| format!("{ip}:{}", p.port)),
                compatible: p.is_compatible(),
                paired: hit.is_some(),
                expired: hit.is_some_and(|(_, e)| *e),
            }
        })
        .collect())
}

/// 开始等待对方连入完成配对。
///
/// 立刻返回配对码，实际等待在后台进行。前端轮询 `phase()` 拿进展。
///
/// # Errors
///
/// 设备库未打开或端口绑定失败时返回。
pub async fn pair_listen_start(
    task: Arc<PairTask>,
    session: Arc<DeviceSession>,
    port: u16,
    expires_days: u32,
) -> Result<PairPhase, DeviceError> {
    if task.is_busy() {
        return Err(DeviceError::Internal);
    }
    if !session.is_open() {
        return Err(DeviceError::NotOpen);
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .map_err(|_| DeviceError::Internal)?;
    let local = listener.local_addr().map_err(|_| DeviceError::Internal)?;

    let pin = omy_net::pairing::generate_pin();
    let phase = PairPhase::Waiting {
        pin: pin.clone(),
        port: local.port(),
    };
    task.set(phase.clone());

    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
    if let Ok(mut g) = task.cancel.lock() {
        *g = Some(cancel_tx);
    }

    let t = Arc::clone(&task);
    let s = Arc::clone(&session);
    tauri::async_runtime::spawn(async move {
        let outcome = wait_for_peer(&listener, &s, &pin, cancel_rx).await;
        finish_pairing(&t, &s, outcome, expires_days);
    });

    Ok(phase)
}

/// 等对方连入并完成配对握手。
///
/// # 为什么失败要继续等，而不是结束
///
/// 这个端口在配对期间是**开放**的，谁都能连。实测中发现：
/// 只要有人连一下（哪怕是端口扫描器、另一个程序的探测），
/// 握手就会失败，整轮配对随之作废——用户盯着屏幕上还显示着的
/// 配对码，在另一台设备上怎么输都没用，且没有任何提示。
///
/// 所以单次握手失败只跳过这一个连接，继续等下一个。真正结束的
/// 条件只有三个：配对成功、总时限到、用户取消。
///
/// 这不会削弱安全性：PAKE 的抗猜测能力本来就不依赖「只允许试一次」，
/// 而且总时限仍然限制着尝试次数。
async fn wait_for_peer(
    listener: &tokio::net::TcpListener,
    session: &DeviceSession,
    pin: &str,
    cancel: tokio::sync::oneshot::Receiver<()>,
) -> Result<omy_net::handshake::PairedDevice, DeviceError> {
    // 配对必须限时：一直挂着等于把 PIN 的有效期无限延长，
    // 而 6 位数字只有一百万种可能
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut cancel = cancel;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DeviceError::PairFailed);
        }

        let accept = tokio::time::timeout(remaining, listener.accept());
        let (mut sock, _) = tokio::select! {
            r = accept => match r {
                Err(_) => return Err(DeviceError::PairFailed), // 到点了
                Ok(Err(_)) => continue,                        // accept 出错，接着等
                Ok(Ok(pair)) => pair,
            },
            _ = &mut cancel => return Err(DeviceError::Cancelled),
        };

        // keypair 与 device_name 要在同一次 read 里取出——分两次的话，
        // 中间可能被 close() 掉，第二次就拿不到了
        let (keypair, name) = session.read(|s| {
            (
                omy_net::channel::StaticKeypair::from_parts(
                    s.public_key().to_vec(),
                    s.keypair().private_bytes().to_vec(),
                ),
                s.device_name().to_owned(),
            )
        })?;

        // 单次握手也要限时：对方连上后不说话会一直占着这一轮
        let attempt = tokio::time::timeout(
            Duration::from_secs(20),
            omy_net::handshake::pair_over(
                &mut sock,
                pin,
                omy_net::pairing::Role::Responder,
                &keypair,
                &name,
            ),
        )
        .await;

        match attempt {
            Ok(Ok(device)) => return Ok(device),
            // 握手失败或超时：这个连接不算数，继续等真正的对端
            Ok(Err(_)) | Err(_) => continue,
        }
    }
}

/// 主动连接对方完成配对。
///
/// # Errors
///
/// 配对码格式错误、连接失败或握手失败时返回。
pub async fn pair_connect(
    task: Arc<PairTask>,
    session: Arc<DeviceSession>,
    addr: String,
    pin: String,
    expires_days: u32,
) -> Result<PairPhase, DeviceError> {
    if !session.is_open() {
        return Err(DeviceError::NotOpen);
    }
    let pin = pin.trim().to_owned();
    if omy_net::pairing::validate_pin(&pin).is_err() {
        return Err(DeviceError::BadDeviceName);
    }

    task.set(PairPhase::Handshaking);

    let (keypair, name) = session.read(|s| {
        (
            omy_net::channel::StaticKeypair::from_parts(
                s.public_key().to_vec(),
                s.keypair().private_bytes().to_vec(),
            ),
            s.device_name().to_owned(),
        )
    })?;

    let outcome = async {
        // 连不上和握手失败要分开报：前者是地址或网络的问题，
        // 后者是配对码不对。报同一个码的话，用户不知道该改哪个
        let mut sock = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .map_err(|_| DeviceError::ConnectFailed)?
        .map_err(|_| DeviceError::ConnectFailed)?;

        tokio::time::timeout(
            Duration::from_secs(20),
            omy_net::handshake::pair_over(
                &mut sock,
                &pin,
                omy_net::pairing::Role::Initiator,
                &keypair,
                &name,
            ),
        )
        .await
        .map_err(|_| DeviceError::PairFailed)?
        .map_err(|_| DeviceError::PairFailed)
    }
    .await;

    // 失败要**同时**做两件事：写进 task（供轮询看到）并返回 Err。
    //
    // 只写 task 的话，前端 `await pairWith(...)` 会正常返回，
    // 界面据此以为配对成功了——真实状态要等下一次轮询才暴露，
    // 中间那一两秒用户看到的是错的。
    let failed = outcome.as_ref().err().copied();
    finish_pairing(&task, &session, outcome, expires_days);
    match failed {
        Some(e) => Err(e),
        None => Ok(task.phase()),
    }
}

/// 把配对结果写进设备库并更新阶段。
///
/// `cancelled` 为真时**什么都不做**：用户已经通过 `cancel()` 把阶段
/// 设成 Idle 了，后台任务再写一个 Failed 回去，屏幕上就会冒出一条
/// 「配对失败」——用户明明是自己点的取消。
fn finish_pairing(
    task: &PairTask,
    session: &DeviceSession,
    outcome: Result<omy_net::handshake::PairedDevice, DeviceError>,
    expires_days: u32,
) {
    match outcome {
        Ok(learned) => {
            let expires = if expires_days == 0 {
                None
            } else {
                Some(Duration::from_secs(u64::from(expires_days) * 86400))
            };
            let rec = DeviceRecord::from_pairing(&learned, expires);
            let fingerprint = hex8(&learned.fingerprint());
            let name = learned.name.clone();

            // 存盘失败必须让用户知道：内存里配对成功但没落盘的话，
            // 重启后配对记录消失，而用户以为已经配好了
            match session.mutate(|s| {
                s.upsert(rec);
                Ok(hex8(&s.fingerprint()))
            }) {
                Ok(own) => task.set(PairPhase::Done {
                    name,
                    fingerprint,
                    own_fingerprint: own,
                }),
                Err(e) => task.set(PairPhase::Failed {
                    code: e.code().to_owned(),
                }),
            }
        }
        // 用户主动取消不是失败，不要覆盖已经设成 Idle 的阶段
        Err(DeviceError::Cancelled) => {}
        Err(e) => task.set(PairPhase::Failed {
            code: e.code().to_owned(),
        }),
    }
}

/// 启动共享服务。
///
/// # Errors
///
/// 目录无效、设备库未打开或端口绑定失败时返回。
pub async fn share_start(
    task: Arc<ShareTask>,
    session: Arc<DeviceSession>,
    dir: String,
    port: u16,
    advertise: bool,
) -> Result<ShareStatus, DeviceError> {
    if task.is_running() {
        return Err(DeviceError::Internal);
    }
    let path = std::path::PathBuf::from(&dir);
    if !path.is_dir() {
        return Err(DeviceError::Internal);
    }

    let (store, keypair, device_name) = session.read(|s| {
        // 服务端主循环要独立持有一份身份与设备列表：它跑在另一个任务里，
        // 生命周期与这里的会话锁无关。`duplicate_for_serve` 的名字
        // 说明了这份副本的去向——不是随手 clone 私钥
        let copy = s.duplicate_for_serve();
        let kp = omy_net::channel::StaticKeypair::from_parts(
            s.public_key().to_vec(),
            s.keypair().private_bytes().to_vec(),
        );
        (copy, kp, s.device_name().to_owned())
    })?;

    let share = omy_net::serve::Share::from_dir(&path).map_err(|_| DeviceError::Internal)?;
    let files = share.len();

    let cfg = omy_net::server_loop::ServeConfig {
        bind: std::net::SocketAddr::from(([0, 0, 0, 0], port)),
        advertise_as: advertise.then_some(device_name),
        ..omy_net::server_loop::ServeConfig::default()
    };

    let connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&connections);

    let running = omy_net::server_loop::serve(
        cfg,
        Arc::new(omy_net::serve::Server::new(share)),
        Arc::new(store),
        Arc::new(keypair),
        move |ev| {
            if matches!(ev, omy_net::server_loop::ConnOutcome::Served { .. }) {
                counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        },
    )
    .await
    .map_err(|_| DeviceError::Internal)?;

    let addr = running.local_addr().to_string();
    task.set(Some(RunningShare {
        dir,
        addr,
        files,
        connections,
        handle: running,
    }));
    Ok(task.status())
}

/// 停止共享服务。
///
/// 必须**注销 mDNS 广播**，否则别的设备会在列表里看到一个连不上的
/// 幽灵条目，而且要等 TTL 过期才消失。
pub async fn share_stop(task: &ShareTask) -> ShareStatus {
    if let Some(r) = task.take() {
        let _ = r.handle.shutdown().await;
    }
    task.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tasks_are_idle() {
        let p = PairTask::new();
        assert!(matches!(p.phase(), PairPhase::Idle));
        assert!(!p.is_busy());

        let s = ShareTask::new();
        assert!(!s.is_running());
        let st = s.status();
        assert!(!st.running);
        assert!(st.dir.is_none(), "未运行时不该泄露目录");
    }

    /// 阶段序列化必须带 `phase` 标签，前端靠它分支。
    #[test]
    fn phase_serializes_with_tag() {
        let j = serde_json::to_string(&PairPhase::Waiting {
            pin: String::from("123456"),
            port: 5000,
        })
        .unwrap();
        assert!(j.contains(r#""phase":"waiting""#), "实得 {j}");
        assert!(j.contains("123456"));

        let j = serde_json::to_string(&PairPhase::Idle).unwrap();
        assert!(j.contains(r#""phase":"idle""#));

        let j = serde_json::to_string(&PairPhase::Done {
            name: String::from("对方"),
            fingerprint: String::from("00112233aabbccdd"),
            own_fingerprint: String::from("ffeeddccbbaa9988"),
        })
        .unwrap();
        assert!(j.contains(r#""phase":"done""#));
        // 两个指纹都要给：用户需要在两台设备上核对它们是否一致
        assert!(j.contains("own_fingerprint"));
    }

    #[test]
    fn is_busy_only_during_pairing() {
        let p = PairTask::new();
        p.set(PairPhase::Waiting {
            pin: String::from("000000"),
            port: 1,
        });
        assert!(p.is_busy());

        p.set(PairPhase::Handshaking);
        assert!(p.is_busy());

        // 完成与失败都不算忙——此时应当能发起新的配对
        p.set(PairPhase::Done {
            name: String::new(),
            fingerprint: String::new(),
            own_fingerprint: String::new(),
        });
        assert!(!p.is_busy());

        p.set(PairPhase::Failed {
            code: String::from("x"),
        });
        assert!(!p.is_busy());
    }

    #[test]
    fn cancel_returns_to_idle() {
        let p = PairTask::new();
        p.set(PairPhase::Waiting {
            pin: String::from("123456"),
            port: 1,
        });
        p.cancel();
        assert!(matches!(p.phase(), PairPhase::Idle));
        assert!(!p.is_busy(), "取消后应能立刻发起新配对");
    }

    /// 用户主动取消后，后台任务收尾时**不能**把阶段改写成 Failed。
    ///
    /// 实测中出现过：点了取消，屏幕上却弹出「配对失败」——
    /// 用户明明是自己取消的，却被告知失败了。
    #[test]
    fn cancel_does_not_become_failure() {
        let p = PairTask::new();
        let s = DeviceSession::new();
        p.set(PairPhase::Waiting {
            pin: String::from("123456"),
            port: 1,
        });
        p.cancel();
        assert!(matches!(p.phase(), PairPhase::Idle));

        // 模拟后台任务在取消之后才收尾
        finish_pairing(&p, &s, Err(DeviceError::Cancelled), 0);
        assert!(
            matches!(p.phase(), PairPhase::Idle),
            "取消后收尾不该把阶段改成 Failed，实得 {:?}",
            p.phase()
        );
    }

    /// 真正的失败仍然要报出来——上一条修复不能顺手把失败也吞掉。
    #[test]
    fn real_failure_still_reported() {
        let p = PairTask::new();
        let s = DeviceSession::new();
        p.set(PairPhase::Handshaking);

        finish_pairing(&p, &s, Err(DeviceError::PairFailed), 0);
        assert_eq!(
            failure_code(&p.phase()).as_deref(),
            Some("pair_failed"),
            "实得 {:?}",
            p.phase()
        );
    }

    /// 连不上与配对码错必须是不同的错误码。
    #[test]
    fn connect_and_pair_failures_are_distinct() {
        let p = PairTask::new();
        let s = DeviceSession::new();

        finish_pairing(&p, &s, Err(DeviceError::ConnectFailed), 0);
        let a = failure_code(&p.phase());

        finish_pairing(&p, &s, Err(DeviceError::PairFailed), 0);
        let b = failure_code(&p.phase());

        assert!(a.is_some() && b.is_some(), "两次都应报失败");
        assert_ne!(a, b, "用户要据此判断该改地址还是改配对码");
    }

    /// 从阶段里取出失败码，不是失败则为 `None`。
    fn failure_code(p: &PairPhase) -> Option<String> {
        match p {
            PairPhase::Failed { code } => Some(code.clone()),
            _ => None,
        }
    }

    /// 配对码必须是 6 位数字，且每次不同。
    ///
    /// 固定或可预测的配对码会让整个 PAKE 失去意义。
    #[test]
    fn generated_pins_vary() {
        let a = omy_net::pairing::generate_pin();
        let b = omy_net::pairing::generate_pin();
        assert_eq!(a.len(), 6);
        assert!(a.chars().all(|c| c.is_ascii_digit()));
        // 一百万分之一的碰撞概率，连续两次相同基本可判定为固定值
        assert_ne!(a, b, "配对码必须每次重新生成");
    }

    #[test]
    fn share_status_hides_details_when_stopped() {
        let s = ShareTask::new();
        let j = serde_json::to_string(&s.status()).unwrap();
        assert!(j.contains(r#""running":false"#));
        assert!(j.contains(r#""dir":null"#), "停止时不该带目录");
        assert!(j.contains(r#""addr":null"#));
    }

    #[test]
    fn discovered_device_serialization() {
        let d = DiscoveredDevice {
            name: String::from("客厅电脑"),
            fingerprint: String::from("00112233aabbccdd"),
            addr: Some(String::from("192.168.1.5:5000")),
            compatible: true,
            paired: false,
            expired: false,
        };
        let j = serde_json::to_string(&d).unwrap();
        // 界面要能区分「没配过」「配过但过期」「版本不兼容」三种状态
        assert!(j.contains("compatible"));
        assert!(j.contains("paired"));
        assert!(j.contains("expired"));
    }
}
