//! 远端共享：连接已配对设备，浏览并预览对方的加密文件。
//!
//! # 为什么不能照搬 CLI 的做法
//!
//! CLI 的 `share connect --fetch` 是「把整个文件取回来，落盘，
//! 然后用 `omy decrypt` 解开」。对命令行这没问题。
//!
//! 但 GUI 要**在应用内预览**，照搬会同时踩两条红线：
//!
//! | 照搬的后果 | 与之冲突的目标 |
//! |---|---|
//! | 看一个 1 GB 的视频要先等它下完 | 可用性 |
//! | 取回的密文落盘，解密后的明文也要落盘 | **明文不落盘**（本项目的核心安全目标）|
//!
//! 所以这里做的是**流式随机读**：`omy_core::payload::read_range`
//! 需要哪一段密文，就发一条 `Request::Read` 去取哪一段。
//! 播放器 seek 到 80% 处，就只取那附近的几十 KB。
//!
//! 这条路能走通，是因为线路协议本来就带 `offset` / `len`
//! （见 `wire.rs`）——服务端只做字节搬运，不关心结构。
//!
//! # 连接是有状态的，而 Tauri 命令是无状态的
//!
//! Noise 信道握手一次要几十毫秒，每次预览都重连既慢又浪费。
//! 所以连接放在会话里，用 `Mutex` 串行化——协议是严格的
//! 请求-响应模型，本来就不允许在一条信道上并发发送。

use crate::devices::{DeviceError, DeviceSession};
use omy_core::crypto::Kek;
use omy_net::wire::{Entry, Handle, Request, Response};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 一条到远端设备的连接。
pub struct RemoteSession {
    inner: tokio::sync::Mutex<Option<Connected>>,
    /// 连接的元信息。单独用 `std::sync::Mutex` 存，
    /// 让同步的状态查询不必进异步运行时。
    meta: Mutex<Option<PeerMeta>>,
    /// handle → 条目，供协议处理器按 id 反查。
    files: Mutex<HashMap<String, RemoteFile>>,
}

struct Connected {
    channel: omy_net::channel::Channel<tokio::net::TcpStream>,
}

/// 已连接设备的元信息。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerMeta {
    /// 对方设备名，取自**本地设备库**而非网络广播。
    pub name: String,
    /// 对方指纹。
    pub fingerprint: String,
    /// 实际连接的地址。
    pub addr: String,
}

/// 远端的一个文件。
///
/// 与本地的 `FileEntry` 平行，但多了 `handle`（远端寻址用），
/// 少了 `path`（我们根本不知道对方的磁盘布局，这是有意的）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteFile {
    /// 进程内标识，前端与协议处理器用它指代。
    pub id: String,
    /// 远端句柄。不透明，不含路径信息。
    #[serde(skip)]
    pub handle: Handle,
    /// 密文总长（含头部）。
    pub size: u64,
    /// 头部长度。
    pub header_len: u32,
    /// 解开的真实文件名。密码不对时为 `None`。
    pub name: Option<String>,
    /// 明文大小，解不开时为 `None`。
    pub plaintext_size: Option<u64>,
    /// 是否已用会话里的密钥解开。
    pub unlocked: bool,
    /// MIME 类型。
    pub mime: Option<String>,
    /// 大类：video / audio / image / text / other。
    pub kind: Option<String>,
    /// 播放分级（p1 直通 / p2 转封装 / p3 全解码）。
    pub tier: Option<String>,
    /// 时长，仅音视频有。
    pub duration_ms: Option<u64>,
    /// 是否有缩略图。
    pub has_thumb: bool,
    /// 原始文件头。解密与预览都要用它。
    #[serde(skip)]
    pub header: Vec<u8>,
}

impl RemoteSession {
    /// 创建一个空会话（未连接）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: tokio::sync::Mutex::new(None),
            meta: Mutex::new(None),
            files: Mutex::new(HashMap::new()),
        }
    }

    /// 当前连接的元信息。
    pub fn peer(&self) -> Option<PeerMeta> {
        self.meta.lock().ok().and_then(|g| g.clone())
    }

    /// 是否已连接。
    pub fn is_connected(&self) -> bool {
        self.meta.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// 按 id 取一个远端文件。
    pub fn file(&self, id: &str) -> Option<RemoteFile> {
        self.files.lock().ok().and_then(|g| g.get(id).cloned())
    }

    /// 当前列表。
    pub fn list(&self) -> Vec<RemoteFile> {
        let Ok(g) = self.files.lock() else {
            return Vec::new();
        };
        let mut v: Vec<RemoteFile> = g.values().cloned().collect();
        // 顺序要稳定：解开名字的按名字排，没解开的按 id 排在后面。
        // 每次刷新顺序都变的话，网格视图会跳来跳去
        v.sort_by(|a, b| match (&a.name, &b.name) {
            (Some(x), Some(y)) => x.cmp(y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.id.cmp(&b.id),
        });
        v
    }

    /// 断开连接并清空列表。
    pub async fn disconnect(&self) {
        {
            let mut g = self.inner.lock().await;
            // Channel 的 Drop 会关掉 socket
            *g = None;
        }
        if let Ok(mut m) = self.meta.lock() {
            *m = None;
        }
        if let Ok(mut f) = self.files.lock() {
            f.clear();
        }
    }
}

impl Default for RemoteSession {
    fn default() -> Self {
        Self::new()
    }
}

/// 连接一台已配对设备并拉取文件列表。
///
/// # Errors
///
/// 设备未配对、授权过期、连不上或握手失败时返回。
pub async fn connect(
    remote: Arc<RemoteSession>,
    devices: Arc<DeviceSession>,
    fingerprint: &str,
    addr: Option<String>,
) -> Result<PeerMeta, DeviceError> {
    let want = crate::devices::parse_fingerprint(fingerprint)
        .ok_or(DeviceError::NoSuchDevice)?;

    // 按指纹在**本地设备库**里反查公钥，不相信网络上广播的东西。
    //
    // mDNS 的 TXT 记录只带指纹不带公钥——这是有意的：任何人都能
    // 广播任意内容，从广播里取公钥的话，攻击者广播自己的公钥配上
    // 别人的名字就能冒充。指纹只用来「在我认识的设备里找是哪一台」。
    let (peer_public, peer_name, keypair) = devices.read(|s| {
        let dev = s.find_by_fingerprint(&want);
        let kp = omy_net::channel::StaticKeypair::from_parts(
            s.public_key().to_vec(),
            s.keypair().private_bytes().to_vec(),
        );
        (
            dev.map(|d| d.public_key.clone()),
            dev.map(|d| d.name.clone()),
            kp,
        )
    })?;

    let (Some(peer_public), Some(peer_name)) = (peer_public, peer_name) else {
        return Err(DeviceError::NoSuchDevice);
    };

    // 过期要单独报：与「没配过」是两回事，用户的处理方式不同
    let expired = devices
        .read(|s| s.find_by_fingerprint(&want).is_some_and(|d| d.is_expired()))
        .unwrap_or(false);
    if expired {
        return Err(DeviceError::AuthExpired);
    }

    // 地址：优先用给定的，否则去局域网找
    let addr = match addr {
        Some(a) if !a.trim().is_empty() => a,
        _ => {
            let found = tokio::task::spawn_blocking(|| {
                omy_net::discovery::browse(Duration::from_secs(3))
            })
            .await
            .map_err(|_| DeviceError::Internal)?
            .map_err(|_| DeviceError::Internal)?;

            let peer = found
                .iter()
                .find(|p| p.fingerprint == want)
                .ok_or(DeviceError::PeerNotFound)?;
            if !peer.is_compatible() {
                return Err(DeviceError::Incompatible);
            }
            let ip = peer.preferred_addr().ok_or(DeviceError::PeerNotFound)?;
            format!("{ip}:{}", peer.port)
        }
    };

    let sock = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(&addr),
    )
    .await
    .map_err(|_| DeviceError::ConnectFailed)?
    .map_err(|_| DeviceError::ConnectFailed)?;

    // 握手失败最常见的原因是对方吊销了本机授权。
    // 这与「连不上」要分开报——用户该做的事完全不同
    let channel = omy_net::channel::Channel::connect(sock, &keypair, &peer_public)
        .await
        .map_err(|_| DeviceError::Unauthorized)?;

    let meta = PeerMeta {
        name: peer_name,
        fingerprint: crate::devices::hex8(&want),
        addr,
    };

    {
        let mut g = remote.inner.lock().await;
        *g = Some(Connected { channel });
    }
    if let Ok(mut m) = remote.meta.lock() {
        *m = Some(meta.clone());
    }

    Ok(meta)
}

/// 拉取远端文件列表，并用当前会话的密钥尝试解开文件名。
///
/// # Errors
///
/// 未连接或对方返回错误时返回。
pub async fn refresh(
    remote: &RemoteSession,
    state: &crate::state::AppState,
) -> Result<Vec<RemoteFile>, DeviceError> {
    let entries = {
        let mut g = remote.inner.lock().await;
        let conn = g.as_mut().ok_or(DeviceError::NotConnected)?;
        match conn
            .channel
            .request(&Request::List)
            .await
            .map_err(|_| DeviceError::ConnectFailed)?
        {
            Response::ListOk { entries } => entries,
            Response::Err { .. } => return Err(DeviceError::RemoteError),
            _ => return Err(DeviceError::RemoteError),
        }
    };

    let files: Vec<RemoteFile> = entries.iter().map(|e| describe(e, state)).collect();

    if let Ok(mut g) = remote.files.lock() {
        g.clear();
        for f in &files {
            g.insert(f.id.clone(), f.clone());
        }
    }

    // 复用 list() 而不是在这里再排一次：两处各自排序迟早会漂移，
    // 症状是「刷新后顺序和缓存读出来的不一样」
    Ok(remote.list())
}

/// 由一个远端条目生成展示信息，尽力用会话密钥解开。
///
/// 解不开是**正常情况**：对方共享的文件可能用了我们没有的密码。
/// 此时如实标成锁定，而不是把它从列表里藏起来——用户需要知道
/// 「有这么个文件，但我没有它的密码」。
fn describe(e: &Entry, state: &crate::state::AppState) -> RemoteFile {
    let id = handle_id(&e.handle);
    let mut f = RemoteFile {
        id,
        handle: e.handle,
        size: e.size,
        header_len: e.header_len,
        name: None,
        plaintext_size: None,
        unlocked: false,
        mime: None,
        kind: None,
        tier: None,
        duration_ms: None,
        has_thumb: false,
        header: e.header.clone(),
    };

    // header 里带 vault_salt，据此找会话里可能匹配的 KEK
    let Ok(peeked) = omy_core::file::peek_header(&e.header) else {
        return f;
    };
    let Some(keks) = state.with_session(|s| {
        s.all_for(&peeked.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect::<Vec<Kek>>()
    }) else {
        return f;
    };
    if keks.is_empty() {
        return f;
    }

    // open 只访问 data[..header_len]（解 slot、验 MAC、读 TLV 都在头里），
    // 所以**只传头部字节就够了**——不需要先把整个文件拉下来。
    // 这正是远端预览能做到「点开就播」的关键：文件名、媒体元信息、
    // 缩略图全在头部，载荷等播放器真的要了再按需取。
    let Ok(opened) = omy_core::file::open(&e.header, &keks) else {
        return f;
    };

    f.unlocked = true;
    f.plaintext_size = Some(opened.header.plaintext_size);
    f.has_thumb = opened.thumbnail().is_ok();

    let Ok(name) = opened.filename() else {
        return f;
    };

    let meta = opened
        .media_meta()
        .ok()
        .and_then(|raw| omy_media::MediaMeta::from_json_bytes(&raw).ok());

    match &meta {
        Some(m) => {
            let (kind, mime) = crate::mime::classify(&name, m);
            f.kind = Some(kind.to_owned());
            f.mime = Some(mime);
            // 播放分级只对音视频有意义。图片走 ffprobe 会被识别成
            // 「单帧视频」而带上 P3 分级，界面上就成了一张 PNG 标着
            // 「需要重新编码」——纯属误导（本地端踩过这个坑）
            if kind == crate::mime::kind::VIDEO || kind == crate::mime::kind::AUDIO {
                f.tier = Some(m.playback_tier.default.to_ascii_lowercase());
                f.duration_ms = m.duration_ms;
            }
        }
        None => {
            // 没有媒体元信息：按后缀兜底，文本和 SVG 这类全靠它
            let (kind, mime) = crate::mime::by_extension(&name);
            f.kind = Some(kind.to_owned());
            f.mime = Some(mime);
        }
    }
    f.name = Some(name);
    f
}

/// 由 handle 生成稳定的进程内 id。
///
/// 用完整 16 字节而非 CLI 那样取前 6 字节：CLI 只是给人看的展示用
/// 标识，这里却要**按 id 反查文件**，截断会引入碰撞——
/// 两个文件撞上同一个 id 意味着预览时会打开错的那个。
#[must_use]
pub fn handle_id(h: &Handle) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// 从远端读一段**密文**。
///
/// 这是流式预览的核心：`read_range` 要哪段就取哪段，
/// 而不是把整个文件拉下来。
///
/// # Errors
///
/// 未连接、对方报错或读取不完整时返回。
pub async fn read_at(
    remote: &RemoteSession,
    handle: Handle,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, DeviceError> {
    let mut out = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    let mut g = remote.inner.lock().await;
    let conn = g.as_mut().ok_or(DeviceError::NotConnected)?;

    while (out.len() as u64) < len {
        let remain = len.saturating_sub(out.len() as u64);
        // 单次 READ 有协议上限，超过要拆成多条
        let want = u32::try_from(remain)
            .unwrap_or(omy_net::wire::MAX_READ_LEN)
            .min(omy_net::wire::MAX_READ_LEN);

        let resp = conn
            .channel
            .request(&Request::Read {
                handle,
                offset: offset.saturating_add(out.len() as u64),
                len: want,
            })
            .await
            .map_err(|_| DeviceError::ConnectFailed)?;

        match resp {
            Response::ReadOk { data } if !data.is_empty() => out.extend_from_slice(&data),
            // 空响应表示读到文件末尾。不算错误，但要跳出，
            // 否则会无限循环发请求
            Response::ReadOk { .. } => break,
            _ => return Err(DeviceError::RemoteError),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_is_disconnected() {
        let s = RemoteSession::new();
        assert!(!s.is_connected());
        assert!(s.peer().is_none(), "未连接时不该泄露对端信息");
        assert!(s.list().is_empty());
        assert!(s.file("anything").is_none());
    }

    /// id 必须用完整 handle，不能截断。
    ///
    /// 截断会引入碰撞，而碰撞的后果是**预览时打开错的文件**——
    /// 这在测试里几乎不可能撞见，一旦发生却很难归因。
    #[test]
    fn handle_id_uses_full_handle() {
        let h: Handle = [0xAB; 16];
        let id = handle_id(&h);
        assert_eq!(id.len(), 32, "16 字节应得 32 个十六进制字符");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));

        // 只有最后一字节不同的两个 handle 必须得到不同 id
        let mut a: Handle = [1; 16];
        let mut b: Handle = [1; 16];
        a[15] = 0;
        b[15] = 1;
        assert_ne!(handle_id(&a), handle_id(&b), "末字节不同必须区分");

        // 前 6 字节相同、后面不同的也要区分（CLI 的短标识会撞）
        let mut c: Handle = [7; 16];
        c[10] = 9;
        assert_ne!(handle_id(&[7; 16]), handle_id(&c));
    }

    #[test]
    fn handle_id_pads_every_byte() {
        // 高位为 0 时必须补零，否则 id 长度不定
        assert_eq!(handle_id(&[0x0A; 16]).len(), 32);
        assert_eq!(handle_id(&[0; 16]), "0".repeat(32));
    }

    /// 远端文件序列化时不能带出 handle 与 header。
    ///
    /// handle 是寻址用的内部标识，header 是密文——两者进 WebView
    /// 都没有用途，只会多一处泄露面。
    #[test]
    fn remote_file_hides_internals() {
        let f = RemoteFile {
            id: String::from("abc"),
            handle: [0xEE; 16],
            size: 1000,
            header_len: 656,
            name: Some(String::from("片子.mp4")),
            plaintext_size: Some(900),
            unlocked: true,
            mime: Some(String::from("video/mp4")),
            kind: Some(String::from("video")),
            tier: Some(String::from("p1")),
            duration_ms: Some(60_000),
            has_thumb: true,
            header: vec![0xAA; 64],
        };
        let j = serde_json::to_string(&f).expect("序列化应成功");
        assert!(!j.contains("handle"), "不该带出 handle");
        assert!(!j.contains("header\""), "不该带出 header 字节");
        assert!(!j.contains("eeee"), "handle 内容不该出现");
        assert!(!j.contains("aaaa"), "header 内容不该出现");
        // 该有的要有
        assert!(j.contains("片子.mp4"));
        assert!(j.contains("video/mp4"));
    }

    #[test]
    fn peer_meta_serialization() {
        let m = PeerMeta {
            name: String::from("客厅电脑"),
            fingerprint: String::from("00112233aabbccdd"),
            addr: String::from("192.168.1.5:5000"),
        };
        let j = serde_json::to_string(&m).expect("序列化应成功");
        assert!(j.contains("客厅电脑"));
        assert!(j.contains("00112233aabbccdd"));
    }

    /// 未连接时读取必须报错，而不是返回空数据。
    ///
    /// 返回 `Ok(vec![])` 会让上层的 `read_range` 把它当成
    /// 「这段确实是空的」，进而解出一段全零的明文——
    /// 播放器不会报错，用户看到的是一段花屏或静音，
    /// 完全无从判断是文件坏了还是根本没连上。
    #[test]
    fn read_without_connection_errors() {
        let s = RemoteSession::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        let r = rt.block_on(read_at(&s, [0; 16], 0, 100));
        assert!(matches!(r, Err(DeviceError::NotConnected)));
    }

    /// 断开后必须什么都不剩。
    #[test]
    fn disconnect_clears_everything() {
        let s = RemoteSession::new();
        if let Ok(mut m) = s.meta.lock() {
            *m = Some(PeerMeta {
                name: String::from("x"),
                fingerprint: String::from("y"),
                addr: String::from("z"),
            });
        }
        if let Ok(mut g) = s.files.lock() {
            g.insert(
                String::from("k"),
                RemoteFile {
                    id: String::from("k"),
                    handle: [0; 16],
                    size: 1,
                    header_len: 1,
                    name: Some(String::from("secret.txt")),
                    plaintext_size: None,
                    unlocked: true,
                    mime: None,
                    kind: None,
                    tier: None,
                    duration_ms: None,
                    has_thumb: false,
                    header: Vec::new(),
                },
            );
        }
        assert!(s.is_connected());

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        rt.block_on(s.disconnect());

        assert!(!s.is_connected());
        assert!(s.peer().is_none());
        assert!(s.list().is_empty(), "断开后不能还留着文件名");
        assert!(s.file("k").is_none());
    }

    /// 指纹解析必须拒绝非法输入。
    ///
    /// 长度不对时若补零或截断，会「找到」一台并非用户所指的设备——
    /// 而后续所有加密操作都会正常完成，只是对象错了。
    #[test]
    fn fingerprint_parsing_is_strict() {
        assert!(crate::devices::parse_fingerprint("").is_none());
        assert!(crate::devices::parse_fingerprint("00").is_none());
        assert!(crate::devices::parse_fingerprint(&"0".repeat(15)).is_none());
        assert!(crate::devices::parse_fingerprint("zzzzzzzzzzzzzzzz").is_none());
        assert!(crate::devices::parse_fingerprint(&"a".repeat(16)).is_some());
    }

    /// 列表顺序必须稳定：解开的按名字排，没解开的排后面。
    ///
    /// 顺序每次刷新都变的话，网格视图会跳来跳去。
    #[test]
    fn list_order_is_stable() {
        let s = RemoteSession::new();
        let mk = |id: &str, name: Option<&str>| RemoteFile {
            id: String::from(id),
            handle: [0; 16],
            size: 0,
            header_len: 0,
            name: name.map(String::from),
            plaintext_size: None,
            unlocked: name.is_some(),
            mime: None,
            kind: None,
            tier: None,
            duration_ms: None,
            has_thumb: false,
            header: Vec::new(),
        };
        if let Ok(mut g) = s.files.lock() {
            g.insert(String::from("c"), mk("c", None));
            g.insert(String::from("a"), mk("a", Some("banana")));
            g.insert(String::from("b"), mk("b", Some("apple")));
            g.insert(String::from("d"), mk("d", None));
        }
        let v = s.list();
        let ids: Vec<&str> = v.iter().map(|f| f.id.as_str()).collect();
        // 解开的在前（按名字：apple < banana），锁定的在后（按 id）
        assert_eq!(ids, vec!["b", "a", "c", "d"]);

        // 再取一次必须完全一致
        let again: Vec<String> = s.list().into_iter().map(|f| f.id).collect();
        assert_eq!(ids, again.iter().map(String::as_str).collect::<Vec<_>>());
    }
}
