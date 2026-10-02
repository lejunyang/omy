//! 代理地址的归一化。
//!
//! # 为什么需要这一层
//!
//! **grammers 0.10 只接受 `socks5://`**（已实测：`http://` 报
//! `proxy scheme not supported: http`，`socks5h://` 同样被拒，缺端口也拒）。
//!
//! 而系统代理给出的地址**通常是 `http://` 形式**：Windows 的
//! `ProxyServer` 注册表项、`HTTP_PROXY` 环境变量、Clash 那类工具的「混合端口」
//! 都是这样。把用户填的 `http://127.0.0.1:7897` 原样递给 grammers，用户会收到
//! 一句「proxy scheme not supported: http」——而他填的地址明明是能用的，
//! 那个端口同时会说 SOCKS5。
//!
//! 所以这里显式处理：能转就转成 `socks5://`，不能转的当场说清楚为什么。
//!
//! # 混合端口这件事是实测过的
//!
//! 同一个 `127.0.0.1:7897` 上，SOCKS5 的 CONNECT 与 HTTP 的 CONNECT 都能通到
//! Telegram 的数据中心。所以把 `http://host:port` 改写成 `socks5://host:port`
//! 在这类工具上是可行的。
//!
//! **但这是个假设，不是保证**：只提供 HTTP CONNECT、不开 SOCKS5 的代理确实存在。
//! 那种情况下改写后的地址连不上，用户会看到一个连接错误。这比报「scheme 不支持」
//! 好——后者让人以为填错了格式，前者至少指向「这个代理不行」。真要支持纯 HTTP
//! 代理，得在连接层自己套一个 CONNECT 隧道，本期不做。

/// 归一化后的代理地址，可直接交给 grammers。
///
/// 只可能是 `socks5://host:port` 或 `socks5://user:pass@host:port`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyUrl(String);

impl ProxyUrl {
    /// 取出可交给 grammers 的字符串。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 注意：若地址里带用户名密码，这里会把它们打出来。调用方不要把
        // 代理地址整条写进日志——与 WebDAV 那边不打印密码是同一条理由。
        f.write_str(&self.0)
    }
}

/// 代理地址不可用的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProxyError {
    /// 缺端口。
    ///
    /// grammers 要求必须有端口（实测：`socks5://127.0.0.1` 报
    /// `proxy port is missing from url`）。在这里拦住并说清楚要补什么，
    /// 比让它到连接层再报一句英文好。
    #[error("代理地址缺少端口号，请写成 host:port 的形式")]
    MissingPort,
    /// 缺主机名。
    #[error("代理地址缺少主机名")]
    MissingHost,
    /// scheme 不是能转成 SOCKS5 的那几种。
    ///
    /// 错误信息里点名它**期望**什么，而不是只说「不支持」：用户看到
    /// 「不支持 socks4」只会困惑，看到「请用 socks5」就知道该改成什么。
    #[error("不支持的代理类型 {scheme}，请使用 socks5（或能同时提供 socks5 的混合端口）")]
    UnsupportedScheme {
        /// 用户填的那个 scheme。
        scheme: String,
    },
}

/// 把用户填的 / 系统给的代理地址归一化成 grammers 能吃的形式。
///
/// 接受这几种写法：
///
/// | 输入 | 结果 | 为什么 |
/// |---|---|---|
/// | `socks5://h:p` | 原样 | 本来就对 |
/// | `socks5h://h:p` | 改成 `socks5://` | 两者只差「域名谁解析」，而 grammers 拒收 `socks5h` |
/// | `http://h:p` / `https://h:p` | 改成 `socks5://` | 系统代理多是这个形式，混合端口上 SOCKS5 同样可用 |
/// | `h:p`（无 scheme） | 补成 `socks5://` | 用户常直接填 `127.0.0.1:7897` |
///
/// 空字符串返回 `Ok(None)`，表示「不用代理」——把「没填」和「填错了」分开，
/// 否则用户清空这一项会收到一句错误。
///
/// # Errors
///
/// 缺主机、缺端口，或 scheme 无法转成 SOCKS5 时返回。
pub fn normalize(raw: &str) -> Result<Option<ProxyUrl>, ProxyError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    // 拆 scheme。没有 `://` 就当成用户直接填了 host:port
    let (scheme, rest) = match raw.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::from("socks5"), raw),
    };

    match scheme.as_str() {
        // socks5h 只是「让代理解析域名」，SOCKS5 协议本身相同；grammers 拒收
        // 这个 scheme，但按 socks5 递给它是等价的（它自己会解析主机）
        "socks5" | "socks5h" | "http" | "https" => {}
        other => {
            return Err(ProxyError::UnsupportedScheme {
                scheme: other.to_string(),
            })
        }
    }

    // 去掉可能的路径部分：`http://127.0.0.1:7897/` 这种带尾斜杠的很常见
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    if authority.is_empty() {
        return Err(ProxyError::MissingHost);
    }

    // 认证信息原样保留，只校验后面的 host:port。
    //
    // 用最后一个 '@' 分界，因为密码里可以有 '@'。这里**只取 hostport 去校验**，
    // 前缀不做解析就拼回去——省掉一次没必要的拆分，也免得写出一段分不出对错的
    // 代码：既然原样拼回，从第一个还是最后一个 '@' 切开，输出完全相同。
    let (auth_prefix, hostport) = match authority.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, authority),
    };

    if hostport.is_empty() {
        return Err(ProxyError::MissingHost);
    }
    // 从**最后一个**冒号切出端口。IPv6 字面量（`[::1]:1080`）内部全是冒号，
    // 按第一个冒号切会把地址切碎。
    //
    // 不必额外判断「是不是以 ']' 收尾」：`[::1]` 这种缺端口的写法切出来的
    // 端口是 "1]"，会被下面的数字校验挡成 MissingPort——结论已经正确，
    // 多加一个分支只是写出一段测不出对错的代码。
    let Some((host, port)) = hostport.rsplit_once(':') else {
        return Err(ProxyError::MissingPort);
    };
    if host.is_empty() {
        return Err(ProxyError::MissingHost);
    }
    // 端口必须是合法的 u16：空的、带字母的、超范围的都不行。
    // 放过去的话 grammers 会在连接层报一句英文，指不到这一步
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) || port.parse::<u16>().is_err() {
        return Err(ProxyError::MissingPort);
    }

    let out = match auth_prefix {
        Some(a) => format!("socks5://{a}@{hostport}"),
        None => format!("socks5://{hostport}"),
    };
    Ok(Some(ProxyUrl(out)))
}

/// 读系统代理设置，作为代理输入框的默认值。
///
/// # 为什么需要它
///
/// 用户常以为「开了全局代理」就等于程序能直连。实际上 Clash 那类工具的
/// 「全局」是**系统代理设置 + 可能的 TUN**：没开 TUN 时它只接管遵循系统
/// 代理的 HTTP 流量，**不接管应用自己发起的裸 TCP**——而 MTProto 正是裸
/// TCP。于是会出现「浏览器能上网，omy 却连不上 Telegram」。
///
/// 把系统代理读出来当默认值，用户就不必对着一个空输入框猜要填什么。
///
/// # 返回值
///
/// 已经过 [`normalize`]，所以拿到的一定是 grammers 能接受的 `socks5://`
/// 形式。读不到、没开、或格式不合法时返回 `None`——**探测是便利而不是
/// 前提**，失败只意味着回落到「要用户自己填」，不能让登录入口本身消失。
///
/// 非 Windows 平台目前一律返回 `None`：那些平台上代理配置的来源不统一
/// （环境变量、GSettings、各发行版自己的机制），猜错比不猜更糟。
#[must_use]
pub fn detect_system_proxy() -> Option<ProxyUrl> {
    #[cfg(target_os = "windows")]
    {
        let raw = windows_system_proxy()?;
        // 系统代理里常见 `http=127.0.0.1:7897;https=...` 这种按协议分列的
        // 写法，取第一段就够——同一个混合端口通常同时提供 SOCKS5
        let first = raw.split(';').next().unwrap_or(&raw).trim().to_string();
        let addr = first.split_once('=').map_or(first.as_str(), |(_, v)| v);
        normalize(addr).ok().flatten()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// 从注册表读 Windows 的系统代理，未启用时返回 `None`。
///
/// 不引第三方 crate：要的只是两个值，而多一个依赖就多一份供应链面。
#[cfg(target_os = "windows")]
fn windows_system_proxy() -> Option<String> {
    use std::os::windows::ffi::OsStrExt as _;

    // winreg 的最小用法：这里只读 HKCU 下的两个值，手写 FFI 比引 crate 划算
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: isize,
            lpsubkey: *const u16,
            lpvalue: *const u16,
            dwflags: u32,
            pdwtype: *mut u32,
            pvdata: *mut core::ffi::c_void,
            pcbdata: *mut u32,
        ) -> i32;
    }
    const HKEY_CURRENT_USER: isize = -2_147_483_647_i32 as isize;
    const RRF_RT_REG_DWORD: u32 = 0x0000_0010;
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    const SUBKEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let sub = wide(SUBKEY);

    // ProxyEnable：为 0 就是没开，这时 ProxyServer 里可能还留着旧值，
    // 照用会把用户引到一个他已经关掉的代理上
    let name = wide("ProxyEnable");
    let mut val: u32 = 0;
    let mut len: u32 = 4;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(val).cast(),
            std::ptr::addr_of_mut!(len),
        )
    };
    if rc != 0 || val == 0 {
        return None;
    }

    let name = wide("ProxyServer");
    let mut buf = [0u16; 512];
    let mut len: u32 = u32::try_from(std::mem::size_of_val(&buf)).ok()?;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            std::ptr::addr_of_mut!(len),
        )
    };
    if rc != 0 {
        return None;
    }
    let chars = (len as usize / 2).saturating_sub(1);
    let s: String = String::from_utf16_lossy(buf.get(..chars)?);
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 连通性自检的结果（登录**之前**判断「路通不通」）。
///
/// # 为什么要分三态
///
/// 实测本机直连 MTProto 数据中心超时、经 socks5 才通。对很多网络环境代理是
/// 必需项，而用户分不清「连不上」是代理、网络还是 api_id 的问题。先自检就把
/// 归因定下来。三态要分开，因为用户该做的事完全不同：通了就继续；代理地址
/// 不合法要改地址（重试多少次都没用）；连不上才是去配代理或查网络。
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ConnCheck {
    /// `ok` | `bad_proxy` | `no_route`。
    pub status: &'static str,
    /// 这次自检花了多久（毫秒）。6 秒和 0.3 秒对用户意义不一样。
    pub elapsed_ms: u64,
    /// 自检时用的代理（false 表示直连）。据此说「经代理」还是「直连」。
    pub via_proxy: bool,
}

/// 对 Telegram 主数据中心做一次 TCP 连通性自检。
///
/// # 只测 TCP，不做 MTProto 握手
///
/// 握手失败还可能是 api_id 的问题，那是另一回事；混在一起就违背了「把归因
/// 定下来」这个目的。TCP 够判断「路通不通」，而且失败得快。
///
/// # 与 GUI / CLI 共用
///
/// 这层原本只在 GUI 登录页里；CLI 要做可脚本化的 `telegram check`，同一份
/// 握手报文不该写第二份（两套实现迟早漂移），故下沉到此处，两端都调它。
///
/// `proxy` 传已经过 [`normalize`] 的 `socks5://` 地址；`None` 表示直连。
/// 调用方若在解析代理时就失败（例如手动地址非法），应自己给出 `bad_proxy`——
/// 那是「配置错」，和这里「路不通」是两回事。
pub async fn probe_connection(proxy: Option<&str>) -> ConnCheck {
    use std::time::Instant;

    let t0 = Instant::now();
    // 目标取 DC2（149.154.167.51:443）——Telegram 主用入口之一。
    // 只连一个就够：自检回答的是「路通不通」，不是「哪个 DC 最快」。
    let target = ("149.154.167.51", 443_u16);
    let ok = match proxy {
        Some(p) => probe_via_socks5(p, target).await,
        None => probe_direct(target).await,
    };
    ConnCheck {
        status: if ok { "ok" } else { "no_route" },
        elapsed_ms: u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX),
        via_proxy: proxy.is_some(),
    }
}

/// 直连一个 host:port，6 秒超时。
async fn probe_direct(target: (&str, u16)) -> bool {
    let addr = format!("{}:{}", target.0, target.1);
    matches!(
        tokio::time::timeout(
            std::time::Duration::from_secs(6),
            tokio::net::TcpStream::connect(addr),
        )
        .await,
        Ok(Ok(_))
    )
}

/// 经 SOCKS5 代理连一个 host:port。
///
/// 只做最小握手（无认证 + CONNECT），够验证「代理能不能把我送到那儿」。
/// 不引第三方 socks 客户端：这里只需要十几个字节的固定报文。
async fn probe_via_socks5(proxy: &str, target: (&str, u16)) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let hostport = proxy.trim_start_matches("socks5://");
    // 代理本身可能带认证前缀，取最后一个 '@' 之后的部分
    let hostport = hostport.rsplit_once('@').map_or(hostport, |(_, h)| h);

    let Ok(Ok(mut s)) = tokio::time::timeout(
        std::time::Duration::from_secs(6),
        tokio::net::TcpStream::connect(hostport),
    )
    .await
    else {
        return false;
    };

    // 问候：VER=5, NMETHODS=1, METHOD=0(无认证)
    if s.write_all(&[0x05, 0x01, 0x00]).await.is_err() {
        return false;
    }
    let mut buf = [0u8; 2];
    if s.read_exact(&mut buf).await.is_err() || buf[0] != 0x05 || buf[1] != 0x00 {
        return false;
    }

    // CONNECT 到目标（ATYP=1 IPv4）
    let Ok(ip) = target.0.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&ip.octets());
    req.extend_from_slice(&target.1.to_be_bytes());
    if s.write_all(&req).await.is_err() {
        return false;
    }

    // 回复：第 2 字节 0x00 表示成功
    let mut rep = [0u8; 4];
    if s.read_exact(&mut rep).await.is_err() {
        return false;
    }
    rep[1] == 0x00
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(raw: &str) -> String {
        normalize(raw)
            .expect("应当可归一化")
            .expect("应当有代理")
            .as_str()
            .to_string()
    }

    /// 系统代理探测的**输出契约**：要么没有，要么是 grammers 能用的形式。
    ///
    /// 不断言具体地址——它随机器而变，写死会让这个测试在别人机器上失败。
    /// 断言的是「探测到的东西一定已经过 normalize」：如果哪天有人图省事
    /// 直接把注册表里的 `http://127.0.0.1:7897` 返回出去，grammers 会在
    /// 连接层拒收，而症状是「填了代理还是连不上」，指不到这里。
    #[test]
    fn detected_system_proxy_is_always_usable_by_grammers() {
        let Some(p) = detect_system_proxy() else {
            // 没开系统代理的机器上就是 None，这也是合法结果
            return;
        };
        let s = p.as_str();
        assert!(
            s.starts_with("socks5://"),
            "探测结果必须已归一化成 socks5://，实际 {s}"
        );
        // 端口必须在：grammers 对缺端口的地址会报一句指不到配置的错误
        let hostport = s.trim_start_matches("socks5://");
        let hostport = hostport.rsplit_once('@').map_or(hostport, |(_, h)| h);
        let (_, port) = hostport
            .rsplit_once(':')
            .unwrap_or_else(|| panic!("探测结果缺端口：{s}"));
        assert!(
            port.parse::<u16>().is_ok(),
            "端口必须是合法 u16，实际 {port}"
        );
    }

    /// `http://` 必须被改写成 `socks5://`，不能原样递下去。
    ///
    /// 不这样会怎样：grammers 只认 socks5，会回一句
    /// 「proxy scheme not supported: http」——而用户填的地址其实是能用的
    /// （混合端口同时提供 SOCKS5，已实测）。他会以为格式写错了而反复改，
    /// 改不出来。
    #[test]
    fn http_scheme_is_rewritten_to_socks5() {
        assert_eq!(ok("http://127.0.0.1:7897"), "socks5://127.0.0.1:7897");
        assert_eq!(ok("https://127.0.0.1:7897"), "socks5://127.0.0.1:7897");
        // 系统代理常带尾斜杠
        assert_eq!(ok("http://127.0.0.1:7897/"), "socks5://127.0.0.1:7897");
        // 大写 scheme 也要认
        assert_eq!(ok("HTTP://127.0.0.1:7897"), "socks5://127.0.0.1:7897");
    }

    /// `socks5h://` 同样要改写：grammers 实测拒收这个 scheme。
    ///
    /// 不这样会怎样：用惯了 curl / 其它工具的用户会填 socks5h（那是「让代理解析
    /// 域名」的意思），而这里报「不支持」会让他莫名其妙——协议其实是一样的。
    #[test]
    fn socks5h_is_rewritten_too() {
        assert_eq!(ok("socks5h://127.0.0.1:7897"), "socks5://127.0.0.1:7897");
    }

    /// 已经是 socks5 的原样通过；没写 scheme 的补上。
    #[test]
    fn socks5_passes_through_and_bare_hostport_gets_scheme() {
        assert_eq!(ok("socks5://127.0.0.1:7897"), "socks5://127.0.0.1:7897");
        // 用户常直接填 host:port
        assert_eq!(ok("127.0.0.1:7897"), "socks5://127.0.0.1:7897");
        assert_eq!(ok("  127.0.0.1:7897  "), "socks5://127.0.0.1:7897", "要容忍空白");
    }

    /// 认证信息要保留，且密码里含 '@' 时不能切错。
    ///
    /// 不这样会怎样：用 `split_once('@')` 会在密码里那个 '@' 处切开，
    /// 得到一个主机名完全错误的地址——而现象是「代理连不上」，
    /// 没人会想到是密码里有个符号。
    #[test]
    fn credentials_survive_normalization() {
        assert_eq!(ok("socks5://u:p@1.2.3.4:1080"), "socks5://u:p@1.2.3.4:1080");
        assert_eq!(
            ok("http://user:pa@ss@1.2.3.4:1080"),
            "socks5://user:pa@ss@1.2.3.4:1080",
            "密码里的 @ 不能让主机名被切错"
        );
    }

    /// 空输入是「不用代理」，不是错误。
    ///
    /// 不这样会怎样：用户清空这一项想改回直连，却收到一句错误提示。
    #[test]
    fn empty_means_no_proxy() {
        assert_eq!(normalize(""), Ok(None));
        assert_eq!(normalize("   "), Ok(None));
    }

    /// 连通性自检结果的 JSON 形状是前端与 CLI 脚本共同依赖的契约。
    ///
    /// 不这样会怎样：字段名一旦改动（例如 status 改成 result），GUI 前端按
    /// `data.status` 分支就永远走到默认分支，而 CLI 脚本按 `status=="ok"`
    /// 判断也会全部误判——且都是静默错，编译期发现不了。
    #[test]
    fn conn_check_serializes_to_the_agreed_shape() {
        let c = ConnCheck { status: "ok", elapsed_ms: 42, via_proxy: true };
        let v = serde_json::to_value(c).expect("序列化");
        assert_eq!(v["status"], "ok");
        assert_eq!(v["elapsed_ms"], 42);
        assert_eq!(v["via_proxy"], true);

        // 三态字面量必须与文档一致：脚本就是按这三个串分支的
        for s in ["ok", "bad_proxy", "no_route"] {
            let c = ConnCheck { status: s, elapsed_ms: 0, via_proxy: false };
            assert_eq!(serde_json::to_value(c).unwrap()["status"], s);
        }
    }

    /// 缺端口要当场报错，并说清要补什么。
    ///
    /// 不这样会怎样：grammers 会回一句英文的
    /// 「proxy port is missing from url」，而界面上只能原样转述。
    #[test]
    fn missing_port_is_caught_locally() {
        assert_eq!(normalize("socks5://127.0.0.1"), Err(ProxyError::MissingPort));
        assert_eq!(normalize("http://example.com"), Err(ProxyError::MissingPort));
        assert_eq!(normalize("127.0.0.1"), Err(ProxyError::MissingPort));
        // 端口不是数字
        assert_eq!(normalize("socks5://127.0.0.1:abc"), Err(ProxyError::MissingPort));
        // 端口超出 u16
        assert_eq!(normalize("socks5://127.0.0.1:99999"), Err(ProxyError::MissingPort));

        let text = ProxyError::MissingPort.to_string();
        assert!(text.contains("端口"), "要点名缺的是端口");
        assert!(text.contains("host:port"), "要给出期望的形式");
    }

    /// 缺主机也要报错。
    #[test]
    fn missing_host_is_caught() {
        assert_eq!(normalize("socks5://:1080"), Err(ProxyError::MissingHost));
        assert_eq!(normalize("socks5://"), Err(ProxyError::MissingHost));
    }

    /// 真正转不了的 scheme 要报错，并告诉用户该用什么。
    ///
    /// 不这样会怎样：只说「不支持 socks4」，用户不知道该改成什么；
    /// 说「请使用 socks5」他就知道了。
    #[test]
    fn unsupported_scheme_names_what_to_use_instead() {
        let e = normalize("socks4://127.0.0.1:1080").expect_err("socks4 应被拒");
        assert_eq!(
            e,
            ProxyError::UnsupportedScheme {
                scheme: String::from("socks4")
            }
        );
        let text = e.to_string();
        assert!(text.contains("socks4"), "要回显用户填的那个，便于对照");
        assert!(text.contains("socks5"), "必须告诉用户该用什么");
    }

    /// IPv6 字面量要能处理，且不能把地址里的冒号误当端口分隔符。
    ///
    /// 不这样会怎样：`[::1]:1080` 会被按最后一个冒号之外的规则切坏，
    /// 要么报「缺端口」要么拼出一个非法地址——而用户填的是完全合法的写法。
    #[test]
    fn ipv6_literals_are_handled() {
        assert_eq!(ok("socks5://[::1]:1080"), "socks5://[::1]:1080");
        assert_eq!(ok("http://[2001:db8::1]:7897"), "socks5://[2001:db8::1]:7897");
        // 只有 IPv6 没端口，仍该报缺端口而不是把地址切坏
        assert_eq!(normalize("socks5://[::1]"), Err(ProxyError::MissingPort));
    }

    /// 输出一定是 grammers 能吃的形状。
    ///
    /// 不这样会怎样：归一化本身写错时，错误会推迟到连接层才暴露，
    /// 而那里的报错信息是英文且指不到这一步。
    #[test]
    fn output_is_always_socks5_with_a_port() {
        for raw in [
            "http://127.0.0.1:7897",
            "socks5h://a.example:1080",
            "1.2.3.4:9",
            "socks5://u:p@[::1]:1080",
        ] {
            let got = ok(raw);
            assert!(got.starts_with("socks5://"), "{raw} -> {got}");
            let tail = got.trim_start_matches("socks5://");
            let port = match tail.rfind(']') {
                Some(i) => tail.get(i + 1..).unwrap_or("").trim_start_matches(':'),
                None => tail.rsplit_once(':').map(|(_, p)| p).unwrap_or(""),
            };
            assert!(
                port.parse::<u16>().is_ok(),
                "{raw} -> {got} 的端口部分不是合法端口"
            );
        }
    }
}
