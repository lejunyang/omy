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
