//! 远程位置的配置落盘与凭据封装：CLI 与 GUI **共用**这一份。
//!
//! # 为什么抽出来
//!
//! 「一个 WebDAV 位置怎么进配置、密码怎么加密进配置、怎么从配置恢复成驱动」
//! 这三件事，GUI 与 CLI 都要做：GUI 在侧栏加位置，CLI 在命令行加位置。
//! 两份实现各写一份的话，同一个位置 id、同一份密码信封格式会在某次改动后
//! 漂移——CLI 加的位置 GUI 认不出，或 GUI 加的密码 CLI 解不开，
//! 而两边功能表现上都不报错。
//!
//! 这里只收**两端都用得到**的部分：厂商串、密码信封的封/解、
//! `SavedPlace` ↔ [`WebDavConfig`] 的互相转换、位置 id 序号解析。
//! Telegram 专有的运行态（连接代理、user_id、session 占位）留在 GUI 层，
//! 本模块不碰——本阶段 CLI 只做 WebDAV 纵向切片。

use std::collections::HashSet;

use omy_config::SavedPlace;
use omy_secret::{Envelope, ProtectKey};

use crate::webdav::{Vendor, WebDavConfig};

/// 凭据在本机凭据库里的服务名。
///
/// **CLI 与 GUI 必须逐字相同**：两边用它定位同一把保护密钥。若 CLI 用了
/// 不同的服务名，命令行加的位置密码会被另一把密钥封上，GUI 解不开——
/// 表现是「CLI 加的位置在 GUI 里要求重新登录」，且不报错。
pub const SECRET_SERVICE: &str = "omy-remote-places";

/// 所有远程位置共用的那一把密钥的 id。
///
/// 只用一把：每个位置一条钥匙串记录的话，删位置时漏清就会在用户的
/// 钥匙串里堆垃圾，而 Linux 的 Secret Service 对条目数也不友好。
pub const SECRET_KEY_ID: &str = "places-key-v1";

/// 取本机的保护密钥（**每次现取，不缓存**）。
///
/// 钥匙串可能中途被锁上，缓存会让我们用一把已经无权使用的密钥，
/// 错误也就推迟到更难解释的地方才出现。
///
/// 取不到（无可用凭据库）返回 `None`：此时位置仍可添加，只是密码不落盘，
/// 下次要重新输入——绝不退回明文存储。
#[must_use]
pub fn protect_key() -> Option<ProtectKey> {
    let p = omy_secret::default_protector(SECRET_SERVICE).ok()?;
    p.retrieve_or_create(SECRET_KEY_ID).ok()
}

/// 从配置里的字符串解析厂商。
///
/// 未知值回落到 [`Vendor::Generic`] 而不是报错：配置可能是更高版本写的，
/// 为一个厂商名让整个位置用不了并不值得。
#[must_use]
pub fn parse_vendor(s: &str) -> Vendor {
    match s {
        "nextcloud" | "owncloud" => Vendor::Nextcloud,
        _ => Vendor::Generic,
    }
}

/// 厂商转回配置里的字符串。
///
/// 与 [`parse_vendor`] 是一对——改一处必须改两处，
/// 否则存进去的值读回来会变成 [`Vendor::Generic`]，针对该厂商的兼容处理静默失效。
#[must_use]
pub fn vendor_str(v: Vendor) -> &'static str {
    match v {
        Vendor::Nextcloud => "nextcloud",
        Vendor::Generic => "generic",
    }
}

/// 把一个 WebDAV 密码封进配置里的信封。
///
/// 没有密码就不造信封（匿名位置）；有密码但拿不到保护密钥时也返回 `None`
/// ——两种情况在配置里都表现为 `secret` 缺失，界面提示重新登录。**绝不返回明文。**
#[must_use]
pub fn seal_password(key: Option<&ProtectKey>, password: &str) -> Option<toml::Value> {
    if password.is_empty() {
        return None;
    }
    key.and_then(|k| omy_secret::seal(k, password.as_bytes()).ok())
        .and_then(|env| toml::Value::try_from(env).ok())
}

/// 从一条配置记录里解出 WebDAV 密码。
///
/// 返回 `(password, need_relogin)`：
/// - 记录本来就没有密码（匿名位置）→ `("", false)`；
/// - 有密文且解开了 → `(password, false)`；
/// - 有密文却解不开（换了机器、清了钥匙串，或信封损坏）→ `("", true)`。
///
/// 第二种与第一种必须分开：用户换机器后看到位置还在、只是要重新登录，
/// 与「这本来就是个匿名位置」是两回事，给用户的提示不同。
pub fn open_password(key: Option<&ProtectKey>, sp: &SavedPlace) -> (String, bool) {
    let Some(v) = sp.secret.as_ref() else {
        return (String::new(), false);
    };
    let env: Envelope = match v.clone().try_into() {
        Ok(e) => e,
        Err(_) => return (String::new(), true),
    };
    let pt = key.and_then(|k| omy_secret::unseal(k, &env).ok());
    match pt {
        Some(plain) => (String::from_utf8_lossy(&plain).into_owned(), false),
        None => (String::new(), true),
    }
}

/// 把一个 WebDAV 位置组装成可持久化的配置记录。纯逻辑，不碰磁盘。
#[must_use]
pub fn webdav_to_saved(
    id: &str,
    name: &str,
    cfg: &WebDavConfig,
    key: Option<&ProtectKey>,
) -> SavedPlace {
    SavedPlace {
        id: String::from(id),
        name: String::from(name),
        // 与驱动自报的 kind 一致，不在这里写字面量：两处各写一份迟早对不上
        kind: String::from("webdav"),
        url: cfg.base_url.clone(),
        username: cfg.username.clone(),
        vendor: String::from(vendor_str(cfg.vendor)),
        writable: cfg.writable,
        secret: seal_password(key, &cfg.password),
        // WebDAV 无账号 user id
        user_id: None,
    }
}

/// 把一条配置记录还原成 WebDAV 驱动所需的配置。
///
/// 非 WebDAV 记录返回 `None`（调用方应交给其它 provider 处理）。
/// 密码解不开时位置仍返回，只是密码为空、`need_relogin` 为真——
/// 用户会看到这个位置还在、标着需要重新登录，而不是以为配置丢了。
#[must_use]
pub fn saved_to_webdav(sp: &SavedPlace, key: Option<&ProtectKey>) -> Option<(WebDavConfig, bool)> {
    if sp.kind != "webdav" {
        return None;
    }
    let (password, need_relogin) = open_password(key, sp);
    let cfg = WebDavConfig {
        base_url: sp.url.clone(),
        username: sp.username.clone(),
        password,
        writable: sp.writable,
        vendor: parse_vendor(&sp.vendor),
        ..WebDavConfig::default()
    };
    Some((cfg, need_relogin))
}

/// 从一个位置 id 里取出序号（`p7` → 7）。认不出返回 `None`。
///
/// GUI 的单调计数器与 CLI 的「取最大值 +1」分配法都要靠它解析已有 id，
/// 集中一处避免两套正则/解析漂移。
#[must_use]
pub fn seq_of(id: &str) -> Option<usize> {
    id.strip_prefix('p').and_then(|n| n.parse().ok())
}

/// 在一组已占用 id 之外，分配一个新的 `p{N}`。
///
/// 取「已有序号的最大值 +1」：CLI 是无状态的短进程，每次读取当前配置后
/// 选一个尚未被占用的 id 即可。`blocked` 用于额外排除磁盘上仍残留、
/// 但已不在配置里的 id（典型：被移除的 Telegram 位置留下的 session 文件）。
///
/// # 为什么不能用「列表长度 +1」
///
/// 「加两个 → 删第一个 → 再加一个」之后，长度 +1 会重新发出 `p2`，
/// 撞上仍在使用（或残留 session 文件）的 id。
#[must_use]
pub fn allocate_place_id(existing: &HashSet<String>, blocked: &dyn Fn(&str) -> bool) -> String {
    let mut seq = existing.iter().filter_map(|id| seq_of(id)).max().unwrap_or(0);
    loop {
        seq = seq.saturating_add(1);
        let candidate = format!("p{seq}");
        if !existing.contains(&candidate) && !blocked(&candidate) {
            return candidate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webdav::WebDavConfig;

    fn cfg(writable: bool) -> WebDavConfig {
        WebDavConfig {
            base_url: String::from("https://dav.example.com/dav"),
            username: String::from("u"),
            password: String::from("secret-pw"),
            writable,
            ..WebDavConfig::default()
        }
    }

    /// 厂商的写出与读回必须互为逆运算。
    ///
    /// 不这样会怎样：存 Nextcloud 读回 Generic，针对该厂商的兼容处理
    /// 静默失效——位置还能用，只是某些请求方式退回通用路径，很难察觉。
    #[test]
    fn vendor_roundtrips() {
        for v in [Vendor::Nextcloud, Vendor::Generic] {
            assert_eq!(parse_vendor(vendor_str(v)), v, "{v:?} 往返后变了");
        }
        // owncloud 历史上归到 Nextcloud 路径
        assert_eq!(parse_vendor("owncloud"), Vendor::Nextcloud);
        assert_eq!(parse_vendor("未知厂商"), Vendor::Generic);
    }

    /// 封了密码再解开，必须原样回来。
    ///
    /// 不这样会怎样：信封格式（n/c 字段）一旦与 omy-secret 升级错位，
    /// CLI 与 GUI 互相加的位置都解不开密码——而两边单测若不往返验证，
    /// 要到真实跨端使用时才暴露。
    #[test]
    fn password_seal_open_roundtrips() {
        let key = omy_secret::random_key();
        let sp = webdav_to_saved("p1", "NAS", &cfg(true), Some(&key));
        assert!(sp.secret.is_some(), "有密码必有信封");
        // 信封进了 toml，序列化里绝不能出现明文密码
        let text = toml::to_string(&sp).expect("序列化");
        assert!(!text.contains("secret-pw"), "配置里出现明文密码：{text}");

        let (pw, need) = open_password(Some(&key), &sp);
        assert!(!need, "同一把密钥不该要重新登录");
        assert_eq!(pw, "secret-pw");
    }

    /// 解不开的密码要如实标记 need_relogin，而不是返回空密码冒充匿名位置。
    ///
    /// 不这样会怎样：换了机器的用户看到位置还在，却以为它本来就不需要密码，
    /// 于是永远不会去补密码——CLI 连上去直接 401。
    #[test]
    fn wrong_key_reports_relogin() {
        let sp = webdav_to_saved("p1", "NAS", &cfg(true), Some(&omy_secret::random_key()));
        let other = omy_secret::random_key();
        let (pw, need) = open_password(Some(&other), &sp);
        assert!(need, "密钥对不上必须标记需要重新登录");
        assert!(pw.is_empty());
    }

    /// 匿名（无密码）位置不带 need_relogin。
    #[test]
    fn anonymous_place_is_not_relogin() {
        let mut c = cfg(false);
        c.password = String::new();
        let sp = webdav_to_saved("p1", "anon", &c, None);
        assert!(sp.secret.is_none());
        let (pw, need) = open_password(None, &sp);
        assert!(!need);
        assert!(pw.is_empty());
    }

    /// 非 webdav 记录不转成 WebDAV 配置。
    #[test]
    fn non_webdav_record_is_skipped() {
        let sp = SavedPlace {
            id: String::from("p1"),
            name: String::from("tg"),
            kind: String::from("telegram"),
            url: String::new(),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: None,
        };
        assert!(saved_to_webdav(&sp, None).is_none());
    }

    /// id 分配不能复用已占用的，也不能撞上被 blocked 的。
    ///
    /// 不这样会怎样：分配复用了刚删掉的 id，新位置会读到旧位置残留的
    /// session / 覆盖旧位置的凭据——这是多账号下最致命的一类错。
    #[test]
    fn allocate_id_avoids_taken_and_blocked() {
        let mut taken: HashSet<String> = ["p1", "p2", "p5"].iter().map(|s| String::from(*s)).collect();
        // 无 blocked：应取最大值 5 的下一个
        assert_eq!(allocate_place_id(&taken, &|_| false), "p6");
        // p6 被残留文件占着时要跳到 p7
        assert_eq!(
            allocate_place_id(&taken, &|id| id == "p6"),
            "p7"
        );
        // 空集合从 p1 开始
        taken.clear();
        assert_eq!(allocate_place_id(&taken, &|_| false), "p1");
    }
}
