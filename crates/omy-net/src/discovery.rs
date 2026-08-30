//! mDNS 设备发现。
//!
//! # 广播里放什么，不放什么
//!
//! mDNS 是**局域网内明文广播**，同一网段的任何设备都能收到。因此
//! TXT 记录里只放建立连接所必需的信息，且每一项都要经得起"被陌生人
//! 看到"的检验：
//!
//! | 放 | 理由 |
//! |---|---|
//! | 协议版本 | 版本不匹配时能立刻给出清晰提示，而不是连上去再失败 |
//! | 设备指纹（静态公钥哈希前 8 字节） | 已配对的设备据此认出对方，无需重新配对 |
//! | 设备显示名 | 用户要能分辨"书房台式机"和"客厅笔记本" |
//!
//! **不放**：文件数量、共享路径、用户名、vault 是否解锁、文件名密文。
//! 这些要么泄露隐私，要么帮助攻击者筛选目标。设备名本身由用户填写，
//! 界面上会提示"局域网内可见"。
//!
//! # 为什么指纹是哈希而不是公钥本身
//!
//! 公钥全文 32 字节，base64 后 44 字符，塞进 TXT 记录会让每个 mDNS
//! 包变大且没有必要——指纹只用于"这台是不是我认识的那台"的快速匹配，
//! 真正的身份验证由 Noise IK 握手完成（对方必须持有对应私钥）。
//! 8 字节指纹的碰撞概率对局域网规模完全够用，且即使碰撞也只是多试
//! 一次握手，不构成安全问题。

use crate::error::{NetError, Result};
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

/// TXT 记录的键名。
///
/// 保持简短：mDNS 包有大小限制，键名越长留给值的空间越少。
const KEY_VERSION: &str = "v";
const KEY_FINGERPRINT: &str = "fp";
const KEY_NAME: &str = "n";

/// 发现到的一台设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    /// 用户可见的设备名。
    ///
    /// 来自对方广播，**不可信**——任何人都能广播任意名字。
    /// 界面上不能只凭这个名字判断身份，必须配合指纹或首次配对。
    pub name: String,
    /// 静态公钥指纹（8 字节）。
    pub fingerprint: [u8; 8],
    /// 对方声称的协议版本。
    pub version: u16,
    /// 可用于连接的地址。
    pub addrs: Vec<IpAddr>,
    /// 服务端口。
    pub port: u16,
}

impl Peer {
    /// 指纹的十六进制表示，用于界面显示与日志。
    #[must_use]
    pub fn fingerprint_hex(&self) -> String {
        hex::encode(self.fingerprint)
    }

    /// 协议版本是否与本机兼容。
    #[must_use]
    pub fn is_compatible(&self) -> bool {
        self.version == crate::PROTOCOL_VERSION
    }

    /// 取第一个可用地址。
    ///
    /// 优先 IPv4：局域网场景下 IPv4 更可能直接可达，
    /// IPv6 链路本地地址还需要 scope id 才能连。
    #[must_use]
    pub fn preferred_addr(&self) -> Option<IpAddr> {
        self.addrs
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| self.addrs.first())
            .copied()
    }
}

/// 从静态公钥算出指纹。
///
/// 用 SHA-256 取前 8 字节。**不是**直接截断公钥——公钥的某些字节
/// 可能有结构（如 X25519 的高位清零），截断会让指纹分布不均。
#[must_use]
pub fn fingerprint(public_key: &[u8]) -> [u8; 8] {
    use sha2::Digest as _;
    let mut h = sha2::Sha256::new();
    // 加域分隔前缀：同一公钥在别处若也做哈希，不会得到相同结果
    h.update(b"omy/v1/device-fingerprint");
    h.update(public_key);
    let d = h.finalize();
    let mut out = [0u8; 8];
    out.copy_from_slice(d.get(..8).unwrap_or(&[0u8; 8]));
    out
}

/// 把设备信息编成 TXT 记录的键值对。
///
/// # Errors
/// 设备名含非法字符或过长时返回错误。
pub fn make_txt(
    name: &str,
    public_key: &[u8],
) -> Result<Vec<(String, String)>> {
    validate_device_name(name)?;
    Ok(vec![
        (KEY_VERSION.to_owned(), crate::PROTOCOL_VERSION.to_string()),
        (
            KEY_FINGERPRINT.to_owned(),
            hex::encode(fingerprint(public_key)),
        ),
        (KEY_NAME.to_owned(), name.to_owned()),
    ])
}

/// 设备名的长度上限（字节）。
///
/// mDNS 的单条 TXT 属性上限是 255 字节，但整个 TXT 记录还要装下
/// 其他字段，且包越小越可靠。63 字节足够放 20 个中文字符。
pub const MAX_DEVICE_NAME: usize = 63;

/// 校验设备名。
///
/// # Errors
/// 为空、超长或含控制字符时返回 [`NetError::Discovery`]。
pub fn validate_device_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(NetError::Discovery("设备名不能为空".into()));
    }
    if name.len() > MAX_DEVICE_NAME {
        return Err(NetError::Discovery(format!(
            "设备名过长（{} 字节，上限 {MAX_DEVICE_NAME}）",
            name.len()
        )));
    }
    // 控制字符会破坏 TXT 记录的解析，也可能被用来伪造界面显示
    if name.chars().any(char::is_control) {
        return Err(NetError::Discovery("设备名不能含控制字符".into()));
    }
    Ok(())
}

/// 从 TXT 键值对解析出设备信息。
///
/// 解析失败返回 `None` 而非错误：局域网里可能有其他程序广播同名服务，
/// 或者是不兼容的旧版本。安静跳过比刷一屏错误更合适。
#[must_use]
pub fn parse_txt<H: std::hash::BuildHasher>(
    props: &HashMap<String, String, H>,
    addrs: Vec<IpAddr>,
    port: u16,
) -> Option<Peer> {
    let version: u16 = props.get(KEY_VERSION)?.parse().ok()?;
    let fp_hex = props.get(KEY_FINGERPRINT)?;
    let fp_bytes = hex::decode(fp_hex).ok()?;
    let fingerprint: [u8; 8] = fp_bytes.as_slice().try_into().ok()?;
    let name = props.get(KEY_NAME)?.clone();
    // 对方的设备名同样要校验：恶意广播可能塞控制字符来搞乱界面
    if validate_device_name(&name).is_err() {
        return None;
    }
    Some(Peer { name, fingerprint, version, addrs, port })
}

/// 浏览局域网内的 omy 设备。
///
/// 阻塞收集 `timeout` 时长内发现的设备，然后返回。
///
/// 为什么是"收集一段时间"而不是流式回调：mDNS 的响应是陆续到达的，
/// 没有"发现完毕"这个时刻。界面上通常是"点一下搜索、转几秒、出列表"，
/// 与这个模型吻合。需要持续监听的场景可以定期重新调用。
///
/// # Errors
/// mDNS 守护进程启动或浏览失败时返回 [`NetError::Discovery`]。
pub fn browse(timeout: Duration) -> Result<Vec<Peer>> {
    use mdns_sd::{ServiceDaemon, ServiceEvent};

    let mdns = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
    let rx = mdns
        .browse(crate::SERVICE_TYPE)
        .map_err(|e| NetError::Discovery(e.to_string()))?;

    let deadline = std::time::Instant::now() + timeout;
    // 用指纹去重：同一台设备可能通过多个网卡被发现多次
    let mut found: HashMap<[u8; 8], Peer> = HashMap::new();

    while let Some(remain) = deadline.checked_duration_since(std::time::Instant::now()) {
        let Ok(ev) = rx.recv_timeout(remain) else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            let props: HashMap<String, String> = info
                .get_properties()
                .iter()
                .map(|p| (p.key().to_owned(), p.val_str().to_owned()))
                .collect();
            let addrs: Vec<IpAddr> = info.get_addresses().iter().map(mdns_sd::ScopedIp::to_ip_addr).collect();
            if let Some(peer) = parse_txt(&props, addrs, info.get_port()) {
                // 同一设备多次出现时合并地址，而不是保留最后一个：
                // 不同网卡发现的地址都可能有用，连接时可逐个尝试
                found
                    .entry(peer.fingerprint)
                    .and_modify(|p| {
                        for a in &peer.addrs {
                            if !p.addrs.contains(a) {
                                p.addrs.push(*a);
                            }
                        }
                    })
                    .or_insert(peer);
            }
        }
    }

    let _ = mdns.shutdown();
    let mut peers: Vec<Peer> = found.into_values().collect();
    // 按指纹排序让结果稳定，界面上设备不会跳来跳去
    peers.sort_by_key(|p| p.fingerprint);
    Ok(peers)
}

/// 正在广播的服务。
///
/// `Drop` 时自动注销并关闭守护进程——忘记停止广播会让设备在
/// 局域网里一直可见，这是隐私问题而不只是资源泄漏。
pub struct Advertiser {
    daemon: mdns_sd::ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    /// 开始广播本机服务。
    ///
    /// # Errors
    /// 设备名非法或 mDNS 守护进程启动失败时返回错误。
    pub fn start(name: &str, public_key: &[u8], port: u16) -> Result<Self> {
        use mdns_sd::{ServiceDaemon, ServiceInfo};

        let txt = make_txt(name, public_key)?;
        let fp = hex::encode(fingerprint(public_key));

        let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;

        // 实例名用指纹而非设备名：设备名可能含空格、中文、特殊字符，
        // 而实例名是 DNS 记录的一部分，用十六进制最省心。
        // 用户可见的名字放在 TXT 里
        let instance = format!("omy-{fp}");
        let host = format!("{instance}.local.");

        let props: Vec<(&str, &str)> = txt
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // 传空地址让库自己探测本机所有网卡地址：
        // 手动指定会在多网卡、VPN、Docker 网桥等场景下选错
        let info = ServiceInfo::new(
            crate::SERVICE_TYPE,
            &instance,
            &host,
            "",
            port,
            &props[..],
        )
        .map_err(|e| NetError::Discovery(e.to_string()))?
        .enable_addr_auto();

        let fullname = info.get_fullname().to_owned();
        daemon
            .register(info)
            .map_err(|e| NetError::Discovery(e.to_string()))?;

        Ok(Self { daemon, fullname })
    }

    /// 本服务的完整 mDNS 名称。
    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        // 尽力注销。失败也没有补救手段，但至少不要 panic——
        // Drop 里 panic 会在展开过程中导致进程直接中止
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn fingerprint_is_deterministic_and_distinct() {
        let a = fingerprint(b"public-key-a");
        let b = fingerprint(b"public-key-b");
        assert_eq!(a, fingerprint(b"public-key-a"), "同一公钥应得到相同指纹");
        assert_ne!(a, b, "不同公钥应得到不同指纹");
    }

    /// 指纹必须是哈希，不能是公钥的前缀。
    ///
    /// 若直接截断，两个前 8 字节相同的公钥就会碰撞——
    /// 而攻击者可以刻意构造这种公钥。
    #[test]
    fn fingerprint_is_not_a_prefix_of_the_key() {
        let key = [0xABu8; 32];
        let fp = fingerprint(&key);
        assert_ne!(&fp[..], &key[..8], "指纹不能是公钥的前缀");
    }

    #[test]
    fn txt_roundtrip() {
        let key = b"my-static-public-key-32-bytes!!!";
        let txt = make_txt("书房台式机", key).expect("构造应成功");
        let map: HashMap<String, String> = txt.into_iter().collect();

        let peer = parse_txt(&map, vec!["192.168.1.5".parse().expect("地址应合法")], 9000)
            .expect("解析应成功");
        assert_eq!(peer.name, "书房台式机");
        assert_eq!(peer.fingerprint, fingerprint(key));
        assert_eq!(peer.version, crate::PROTOCOL_VERSION);
        assert_eq!(peer.port, 9000);
        assert!(peer.is_compatible());
    }

    #[test]
    fn parse_rejects_missing_fields() {
        // 缺各个必需字段都应返回 None 而非 panic
        let cases = vec![
            props(&[("fp", "0011223344556677"), ("n", "x")]),
            props(&[("v", "1"), ("n", "x")]),
            props(&[("v", "1"), ("fp", "0011223344556677")]),
            props(&[]),
        ];
        for c in cases {
            assert!(parse_txt(&c, vec![], 1).is_none(), "缺字段应返回 None");
        }
    }

    #[test]
    fn parse_rejects_malformed_values() {
        let cases = vec![
            // 版本不是数字
            props(&[("v", "abc"), ("fp", "0011223344556677"), ("n", "x")]),
            // 指纹不是十六进制
            props(&[("v", "1"), ("fp", "zzzz"), ("n", "x")]),
            // 指纹长度不对
            props(&[("v", "1"), ("fp", "00112233"), ("n", "x")]),
            // 设备名为空
            props(&[("v", "1"), ("fp", "0011223344556677"), ("n", "")]),
        ];
        for c in cases {
            assert!(parse_txt(&c, vec![], 1).is_none(), "非法值应返回 None: {c:?}");
        }
    }

    /// 恶意广播可能塞控制字符来搞乱界面显示。
    #[test]
    fn parse_rejects_control_chars_in_name() {
        let p = props(&[
            ("v", "1"),
            ("fp", "0011223344556677"),
            ("n", "正常名字\u{7}\u{1b}[31m"),
        ]);
        assert!(
            parse_txt(&p, vec![], 1).is_none(),
            "含控制字符的设备名必须拒绝，否则可用于伪造终端输出"
        );
    }

    #[test]
    fn version_mismatch_is_detected() {
        let mut p = props(&[("v", "1"), ("fp", "0011223344556677"), ("n", "x")]);
        p.insert("v".into(), "999".into());
        let peer = parse_txt(&p, vec![], 1).expect("应能解析");
        assert!(!peer.is_compatible(), "版本不同应判为不兼容");
        assert_eq!(peer.version, 999);
    }

    #[test]
    fn device_name_validation() {
        assert!(validate_device_name("正常名字").is_ok());
        assert!(validate_device_name("").is_err(), "空名应拒绝");
        assert!(validate_device_name(&"x".repeat(64)).is_err(), "超长应拒绝");
        assert!(validate_device_name("含\u{0}空字符").is_err(), "控制字符应拒绝");
        // 边界：恰好 63 字节应通过
        assert!(validate_device_name(&"x".repeat(63)).is_ok());
    }

    #[test]
    fn make_txt_rejects_bad_name() {
        assert!(make_txt("", b"key").is_err());
        assert!(make_txt(&"x".repeat(100), b"key").is_err());
    }

    #[test]
    fn prefers_ipv4() {
        let peer = Peer {
            name: "x".into(),
            fingerprint: [0; 8],
            version: 1,
            addrs: vec![
                "fe80::1".parse().expect("地址应合法"),
                "192.168.1.7".parse().expect("地址应合法"),
            ],
            port: 1,
        };
        assert_eq!(
            peer.preferred_addr(),
            Some("192.168.1.7".parse().expect("地址应合法")),
            "应优先选 IPv4：链路本地 IPv6 还需要 scope id 才能连"
        );
    }

    #[test]
    fn preferred_addr_falls_back_to_ipv6() {
        let peer = Peer {
            name: "x".into(),
            fingerprint: [0; 8],
            version: 1,
            addrs: vec!["fe80::1".parse().expect("地址应合法")],
            port: 1,
        };
        assert!(peer.preferred_addr().is_some(), "只有 IPv6 时也应返回");
    }

    #[test]
    fn empty_addrs_yields_none() {
        let peer = Peer {
            name: "x".into(),
            fingerprint: [0; 8],
            version: 1,
            addrs: vec![],
            port: 1,
        };
        assert!(peer.preferred_addr().is_none());
    }

    /// TXT 记录里不得出现任何与文件相关的信息。
    ///
    /// 这是隐私边界的回归测试：日后有人想"顺便广播文件数"时，
    /// 这条会失败并提醒他 mDNS 是明文广播。
    #[test]
    fn txt_contains_only_whitelisted_keys() {
        let txt = make_txt("测试设备", b"key").expect("构造应成功");
        let keys: Vec<&str> = txt.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys.len(),
            3,
            "TXT 记录只应有 3 个字段；新增字段前请确认它可以被局域网内任何人看到"
        );
        for k in &keys {
            assert!(
                [KEY_VERSION, KEY_FINGERPRINT, KEY_NAME].contains(k),
                "出现了未经审视的 TXT 字段: {k}"
            );
        }
    }

    #[test]
    fn fingerprint_hex_is_16_chars() {
        let peer = Peer {
            name: "x".into(),
            fingerprint: [0xAB; 8],
            version: 1,
            addrs: vec![],
            port: 1,
        };
        assert_eq!(peer.fingerprint_hex(), "abababababababab");
    }
}
