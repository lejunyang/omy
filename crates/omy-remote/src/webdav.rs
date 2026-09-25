//! WebDAV 驱动。
//!
//! # 为什么 WebDAV 是首选
//!
//! 一个驱动覆盖 NAS、Nextcloud、坚果云，以及所有被 AList / CloudDrive2 /
//! LitePan 中转出来的云盘——光鸭本身也在其中。而厂商私有 API 的成本不在
//! 于写，在于**写完之后一直要跟**：接口变了不会有人通知你，只会收到用户
//! 报障。RFC 4918 冻结了二十年。
//!
//! # 读取根本不需要「WebDAV 客户端」
//!
//! 这是它最适合 omy 的地方：
//!
//! - **列目录**用 `PROPFIND`（XML），交给 `reqwest_dav`；
//! - **读文件就是普通 HTTP GET**，`Range: bytes=a-b` 直接可用。
//!
//! 也就是说流式播放这条最关键的链路是白送的，不像云盘私有 API 那样
//! 还要先换直链、再处理直链过期。
//!
//! # 但不要假设「标准协议就没有差异」
//!
//! rclone 的 WebDAV 后端维护了 8 个 `vendor` 分支，这本身就是证据。
//! 我们用到的子集小得多（列目录 + Range 读 + 整文件写），但仍保留
//! [`Vendor`]，把差异集中在一处而不是散落到调用点。

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_RANGE, RANGE};
use reqwest::StatusCode;

use crate::store::{Entry, RemoteStore};
use crate::{Capabilities, Error, Result};

/// 路径**段**的百分号编码集合（段间的 `/` 在调用前已 split，不参与编码）。
///
/// 为什么不能直接用 `NON_ALPHANUMERIC`：它只放行 `A-Za-z0-9`，连
/// `.` `-` `_` `~` 都编码，于是 `movie.mkv.omy` 会变成 `movie%2Emkv%2Eomy`。
/// RFC 3986 虽允许服务端把 `%2E` 解回 `.`，但坚果云等部分服务端 / 网关会把
/// `%2E` 当成文件名字面量直接 404——而扩展名在文件名里无处不在，这个兼容性
/// 必须在客户端保证。这里以 `CONTROLS`（控制字符，非 ASCII 的 UTF-8 字节由
/// `utf8_percent_encode` 无条件编码）为底，只追加真正会改变 URL 语义、必须
/// 转义的 ASCII 标点，保留 unreserved 字符（含 `.` `-` `_` `~`）与 path 子
/// 分界符（`: @ ! $ & ' ( ) * + , ; =`）。
const PATH_SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}')
    .add(b'%')
    .add(b'[')
    .add(b']')
    .add(b'^')
    .add(b'|')
    .add(b'\\');


/// 服务端厂商。用于吸收「标准之上」的现实差异。
///
/// 先只区分 `Generic`：我们用到的子集在各家基本一致，预先写八套分支
/// 只会是负担——rclone 那些分支有一半是为修改时间、哈希、分块上传
/// 准备的，而这些我们都不用。等真实服务端暴露问题再加。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Vendor {
    /// 按 RFC 4918 的通用实现。
    #[default]
    Generic,
    /// Nextcloud / ownCloud 系。
    Nextcloud,
}

/// 连接配置。
#[derive(Debug, Clone)]
pub struct WebDavConfig {
    /// 服务端根 URL，如 `https://example.com/remote.php/dav/files/me/`。
    pub base_url: String,
    /// 用户名。匿名访问时留空。
    pub username: String,
    /// 密码或应用专用密码。
    pub password: String,
    /// 厂商。
    pub vendor: Vendor,
    /// 是否允许写。
    ///
    /// 由用户在添加位置时选择：同一个账号既可以挂成可写，也可以刻意
    /// 挂成只读以防误操作。声明为只读时驱动**不会**发出任何写请求。
    pub writable: bool,
    /// 请求超时。
    pub timeout: Duration,
}

impl Default for WebDavConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            username: String::new(),
            password: String::new(),
            vendor: Vendor::Generic,
            writable: false,
            timeout: Duration::from_secs(30),
        }
    }
}

/// WebDAV 存储。
pub struct WebDavStore {
    cfg: WebDavConfig,
    http: reqwest::Client,
    dav: reqwest_dav::Client,
}

impl std::fmt::Debug for WebDavStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不打印 cfg：里面有密码。日志里出现一次明文密码就等于泄露，
        // 而 Debug 很容易被顺手塞进错误信息
        f.debug_struct("WebDavStore").field("base_url", &self.cfg.base_url).finish_non_exhaustive()
    }
}

impl WebDavStore {
    /// 建连时用的配置。
    ///
    /// 持久化时要把这些字段写回配置文件，所以必须能读回来。
    ///
    /// 返回引用而非克隆：`WebDavConfig` 里有密码，多一份克隆就多一处
    /// 可能被遗留在内存里的明文。调用方只取自己要的字段。
    #[must_use]
    pub fn config(&self) -> &WebDavConfig {
        &self.cfg
    }

    /// 建立连接（不发请求，仅构造客户端）。
    ///
    /// # Errors
    ///
    /// URL 非法或 TLS 初始化失败时返回。
    pub fn new(cfg: WebDavConfig) -> Result<Self> {
        // 自己校验 URL：reqwest_dav 的 ClientBuilder 只是存下字符串，
        // 构造阶段不发请求，所以 "not a url" 这种也能建成功，直到第一次
        // 浏览才报一个语焉不详的网络错误。
        //
        // 对用户来说差别很大：地址输错时应当在「添加位置」那一步就说
        // 「这不是一个合法的地址」，而不是加完之后点进去才失败。
        // 恢复配置时也靠这个把坏记录跳过。
        let parsed = url::Url::parse(&cfg.base_url)
            .map_err(|e| Error::Protocol(format!("地址无法解析：{e}")))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(Error::Protocol(format!(
                "地址必须以 http:// 或 https:// 开头，实际是 {}://",
                parsed.scheme()
            )));
        }
        if !parsed.has_host() {
            return Err(Error::Protocol(String::from("地址里没有主机名")));
        }

        let mut builder = reqwest::Client::builder()
            .timeout(cfg.timeout)
            // 跟随重定向时 reqwest 会丢掉 Authorization 头（这是正确的
            // 安全行为：不该把凭据发给未知主机）。部分服务端读取时会 302
            // 到另一个域，届时表现为 401——rclone 专门为此有个
            // auth_redirect 选项。这里先保持默认的安全行为。
            ;
        if is_loopback_host(&parsed) {
            // 本机地址不走代理，理由见 `is_loopback_host`
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(|e| Error::Network(e.to_string()))?;

        let mut b = reqwest_dav::ClientBuilder::new().set_host(cfg.base_url.clone());
        if !cfg.username.is_empty() {
            b = b.set_auth(reqwest_dav::Auth::Basic(
                cfg.username.clone(),
                cfg.password.clone(),
            ));
        }
        // 列目录那条链路也必须用同一个客户端，否则代理策略只对 GET 生效，
        // 而 PROPFIND 仍旧走代理——那才是实际踩到的现象（见 map_dav_err
        // 上方注释里记的 502）。
        b = b.set_agent(http.clone());
        let dav = b.build().map_err(|e| Error::Network(e.to_string()))?;

        Ok(Self { cfg, http, dav })
    }

    /// 把条目 id（相对路径）拼成绝对 URL。
    ///
    /// 路径里的中文、空格必须百分号编码，否则部分服务端直接 400。
    /// 但 `/` 不能编码——它是路径分隔符，编码后会变成文件名的一部分。
    fn url_for(&self, id: &str) -> String {
        let base = self.cfg.base_url.trim_end_matches('/');
        let rel = id.trim_start_matches('/');
        if rel.is_empty() {
            return format!("{base}/");
        }
        let encoded = rel
            .split('/')
            .map(|seg| {
                percent_encoding::utf8_percent_encode(seg, PATH_SEGMENT).to_string()
            })
            .collect::<Vec<_>>()
            .join("/");
        format!("{base}/{encoded}")
    }

    /// 认证头。空用户名时不加。
    fn auth_header(&self) -> Option<HeaderValue> {
        if self.cfg.username.is_empty() {
            return None;
        }
        // 不引入 base64 依赖：Basic 认证的编码就这几行
        let raw = format!("{}:{}", self.cfg.username, self.cfg.password);
        let encoded = base64_encode(raw.as_bytes());
        HeaderValue::from_str(&format!("Basic {encoded}")).ok()
    }

    /// 把 HTTP 状态映射为分类错误。
    ///
    /// 界面对不同原因的处理完全不同：401 要弹登录，429 要退避重试，
    /// 404 要提示文件可能已被别处删掉。混成一个「操作失败」的话，
    /// 用户不知道下一步该干什么。
    fn map_status(status: StatusCode, what: &str) -> Error {
        match status {
            StatusCode::UNAUTHORIZED => Error::Unauthorized,
            StatusCode::FORBIDDEN => Error::Forbidden,
            StatusCode::NOT_FOUND => Error::NotFound(what.to_owned()),
            StatusCode::TOO_MANY_REQUESTS => Error::RateLimited,
            s => Error::Protocol(format!("{what}: HTTP {s}")),
        }
    }
}

impl RemoteStore for WebDavStore {
    fn capabilities(&self) -> Capabilities {
        if self.cfg.writable {
            Capabilities::cloud_writable()
        } else {
            Capabilities::read_only()
        }
    }

    fn describe(&self) -> String {
        format!("webdav:{}", self.cfg.base_url)
    }

    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>> {
        let path = if dir_id.is_empty() { "/" } else { dir_id };
        let items = self
            .dav
            .list(path, reqwest_dav::Depth::Number(1))
            .await
            .map_err(map_dav_err)?;

        let base_norm = normalize(dir_id);
        let mut out = Vec::new();
        for it in items {
            match it {
                reqwest_dav::list_cmd::ListEntity::File(f) => {
                    let href = decode_href(&f.href);
                    // PROPFIND Depth:1 会把目录自身也列出来，跳过它，
                    // 否则每进一层目录都会多出一个指向自己的条目
                    if normalize(&href) == base_norm {
                        continue;
                    }
                    out.push(Entry {
                        name: basename(&href),
                        id: href,
                        is_dir: false,
                        size: u64::try_from(f.content_length).ok(),
                        mtime: Some(f.last_modified.timestamp()),
                        etag: Some(f.tag.unwrap_or_default()).filter(|s| !s.is_empty()),
                        // WebDAV 没有「随目录列举一起送来的缩略图」这个概念
                        thumb: None,
                        media_tab: None,
                    });
                }
                reqwest_dav::list_cmd::ListEntity::Folder(d) => {
                    let href = decode_href(&d.href);
                    if normalize(&href) == base_norm {
                        continue;
                    }
                    out.push(Entry {
                        name: basename(&href),
                        id: href,
                        is_dir: true,
                        size: None,
                        mtime: Some(d.last_modified.timestamp()),
                        etag: None,
                        thumb: None,
                        media_tab: None,
                    });
                }
            }
        }
        // 顺序要稳定：目录在前，同类按名字。服务端返回顺序不保证，
        // 每次刷新都变的话网格视图会跳
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        if len == 0 {
            return Ok(Vec::new());
        }
        let end = offset.saturating_add(len).saturating_sub(1);

        let mut headers = HeaderMap::new();
        if let Some(a) = self.auth_header() {
            headers.insert(AUTHORIZATION, a);
        }
        let Ok(rv) = HeaderValue::from_str(&format!("bytes={offset}-{end}")) else {
            return Err(Error::Protocol(String::from("非法的 Range 区间")));
        };
        headers.insert(RANGE, rv);

        let resp = self
            .http
            .get(self.url_for(id))
            .headers(headers)
            .send()
            .await
            .map_err(|e| Error::Network(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(Self::map_status(status, id));
        }

        // 服务端可以合法地忽略 Range（RFC 7233：MAY ignore），此时返回
        // 200 和整个文件。不裁剪的话上层会把整文件当成一小段密文去解，
        // 表现为「文件明明没坏却一直认证失败」——极难归因。
        let ignored_range = status != StatusCode::PARTIAL_CONTENT
            && resp.headers().get(CONTENT_RANGE).is_none();

        let body = resp.bytes().await.map_err(|e| Error::Network(e.to_string()))?;

        if ignored_range {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            let want = usize::try_from(len).unwrap_or(usize::MAX);
            let slice = body.get(start..).unwrap_or(&[]);
            let take = slice.len().min(want);
            return Ok(slice.get(..take).unwrap_or(&[]).to_vec());
        }
        Ok(body.to_vec())
    }

    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> Result<Entry> {
        if !self.cfg.writable {
            return Err(Error::Unsupported("write"));
        }
        let path = join(dir_id, name);
        self.dav.put(&path, data.to_vec()).await.map_err(map_dav_err)?;
        Ok(Entry {
            id: path,
            name: name.to_owned(),
            is_dir: false,
            size: Some(data.len() as u64),
            mtime: None,
            etag: None,
            thumb: None,
            media_tab: None,
        })
    }

    async fn delete(&self, id: &str) -> Result<()> {
        if !self.cfg.writable {
            return Err(Error::Unsupported("delete"));
        }
        self.dav.delete(id).await.map_err(map_dav_err)
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
        if !self.cfg.writable {
            return Err(Error::Unsupported("rename"));
        }
        let parent = dirname(id);
        let target = join(&parent, new_name);
        self.dav.mv(id, &target).await.map_err(map_dav_err)
    }

    async fn create_dir(&self, parent_id: &str, name: &str) -> Result<Entry> {
        if !self.cfg.writable {
            return Err(Error::Unsupported("create_dir"));
        }
        let path = join(parent_id, name);
        self.dav.mkcol(&path).await.map_err(map_dav_err)?;
        Ok(Entry {
            id: path,
            name: name.to_owned(),
            is_dir: true,
            size: None,
            mtime: None,
            etag: None,
            thumb: None,
            media_tab: None,
        })
    }
}

/// 这个地址是不是指向本机（或链路本地）。
///
/// # 为什么要单独判断，而不是信任系统的「代理例外」
///
/// `reqwest` **不会**自动为 `127.0.0.1` 绕过代理。它在 Windows 上读注册表里的
/// `ProxyEnable` / `ProxyServer` / `ProxyOverride`（经由 `hyper-util`），但
/// `ProxyOverride` 用的是 WinINET 的通配语法（`127.*`、`<local>`），而
/// `hyper-util` 的 `NoProxy` 只认精确 IP、CIDR 和域名后缀——`127.*` 和
/// `<local>` 两种写法它都表达不了，于是对 IP 形式的主机就只查 IP 匹配表，
/// 本机地址**一个都不在里面**。
///
/// 结果是：用户开着系统代理（很常见）去访问局域网或本机的 WebDAV 时，请求会被
/// 送进代理，而代理连不到那个内网地址，于是回一个**代理自己生成的**错误。
///
/// 这个现象极具误导性：我们的集成测试服务端只会回 401 和 207，却出现过一次
/// 502 —— 502 是代理回的，不是服务端回的。当时的报错是「错误密码应分类为
/// Unauthorized，实际 502」，看起来像认证分类逻辑坏了，而真正的原因与认证
/// 毫无关系。用户侧的表现会更糟：他会以为自己的 NAS 或 WebDAV 服务出了问题。
///
/// 所以这里显式绕过。判断范围取「本机 + 链路本地」而不是「所有私有网段」：
/// 企业环境里确实存在需要经代理访问内网的配置，把 `10.0.0.0/8` 一起排除会
/// 反过来破坏那种场景；而本机地址经代理转发在任何配置下都没有意义。
fn is_loopback_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(ip)) => ip.is_loopback() || ip.is_link_local(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        // 主机名形式：只认这两个字面量。不做 DNS 解析——构造客户端时
        // 发同步 DNS 请求会阻塞 UI，而且解析结果还可能变。
        Some(url::Host::Domain(d)) => {
            d.eq_ignore_ascii_case("localhost") || d.eq_ignore_ascii_case("localhost.")
        }
        None => false,
    }
}

/// 把 `reqwest_dav` 的错误映射成分类错误。
fn map_dav_err(e: reqwest_dav::Error) -> Error {
    // 该库把 HTTP 状态包在 Decode(StatusMismatched) 里，
    // 取不到时退回网络错误——宁可少分类，也不要把认证失败报成别的
    let s = e.to_string();
    if s.contains("401") {
        Error::Unauthorized
    } else if s.contains("403") {
        Error::Forbidden
    } else if s.contains("404") {
        Error::NotFound(s)
    } else if s.contains("429") {
        Error::RateLimited
    } else {
        Error::Network(s)
    }
}

/// 归一化路径，用于比较两个 href 是否指向同一处。
fn normalize(p: &str) -> String {
    let t = p.trim_matches('/');
    percent_decode(t)
}

/// href 里的路径是百分号编码的，要还原成可读名字。
fn decode_href(href: &str) -> String {
    let s = percent_decode(href);
    // 服务端返回的 href 可能带前缀（如 /remote.php/dav/files/me/a.txt），
    // 保留原样即可——它会被 url_for 重新编码后使用
    s
}

fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s).decode_utf8_lossy().into_owned()
}

/// 取路径最后一段作为显示名。
fn basename(p: &str) -> String {
    p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_owned()
}

/// 取父路径。
fn dirname(p: &str) -> String {
    let t = p.trim_end_matches('/');
    match t.rfind('/') {
        Some(i) => t.get(..i).unwrap_or("").to_owned(),
        None => String::new(),
    }
}

/// 拼接目录与名字，避免出现 `//` 或缺少分隔符。
fn join(dir: &str, name: &str) -> String {
    let d = dir.trim_end_matches('/');
    let n = name.trim_start_matches('/');
    if d.is_empty() { format!("/{n}") } else { format!("{d}/{n}") }
}

/// 最小 base64 编码。
///
/// 只为 Basic 认证这一处，不值得为它引入一个依赖。
fn base64_encode(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for c in input.chunks(3) {
        let b0 = *c.first().unwrap_or(&0) as u32;
        let b1 = *c.get(1).unwrap_or(&0) as u32;
        let b2 = *c.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        out.push(T[idx[0] as usize] as char);
        out.push(T[idx[1] as usize] as char);
        out.push(if c.len() > 1 { T[idx[2] as usize] as char } else { '=' });
        out.push(if c.len() > 2 { T[idx[3] as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(writable: bool) -> WebDavStore {
        WebDavStore::new(WebDavConfig {
            base_url: String::from("https://dav.example.com/dav"),
            username: String::from("u"),
            password: String::from("p"),
            writable,
            ..WebDavConfig::default()
        })
        .expect("构造客户端")
    }

    /// 本机地址必须被判成「不走代理」，公网地址必须照常走代理。
    ///
    /// 不这样会怎样：reqwest 不会自动为 127.0.0.1 绕过代理（Windows 的
    /// ProxyOverride 用的是 `127.*`、`<local>` 这类通配语法，hyper-util 的
    /// NoProxy 表达不了，于是对 IP 主机只查 IP 匹配表，本机地址一个都不在
    /// 里面）。于是用户开着系统代理访问本机或局域网 WebDAV 时，请求被送进
    /// 代理，代理连不到那个内网地址，回一个代理自己生成的错误——用户会以为
    /// 自己的 NAS 坏了。我们的集成测试也因此出现过一次 502，而那个服务端
    /// 只会回 401/207。
    ///
    /// 反向的断言同样重要：**不能为了省事把所有地址都绕过代理**，否则墙后
    /// 的用户访问公网 WebDAV 会直接连不上。
    #[test]
    fn loopback_bypasses_proxy_but_public_hosts_do_not() {
        for s in [
            "http://127.0.0.1:8080/dav",
            "http://127.1.2.3/dav",
            "http://[::1]:8080/dav",
            "http://localhost:8080/dav",
            "http://LOCALHOST/dav",
            // 链路本地（169.254/16）：DHCP 失败时的自动地址，同样不该经代理
            "http://169.254.1.2/dav",
        ] {
            let u = url::Url::parse(s).expect("合法地址");
            assert!(is_loopback_host(&u), "{s} 应当绕过代理");
        }

        for s in [
            "https://dav.example.com/dav",
            "https://127.0.0.1.example.com/dav",
            "https://notlocalhost/dav",
            "https://localhost.example.com/dav",
            // 私有网段**不**绕过：企业环境里确实存在经代理访问内网的配置，
            // 把 10/8 一起排除会反过来破坏那种场景
            "http://10.0.0.5/dav",
            "http://192.168.1.10/dav",
        ] {
            let u = url::Url::parse(s).expect("合法地址");
            assert!(!is_loopback_host(&u), "{s} 不该绕过代理");
        }
    }

    /// 列目录与读文件必须共用同一个 HTTP 客户端。
    ///
    /// 不这样会怎样：代理策略只作用在 GET 上，而列目录走的是 reqwest_dav 自己
    /// 建的客户端、仍旧经代理——于是「能打开文件但列不出目录」，或者反过来。
    /// 这正是实际踩到的那个 502：失败的是 PROPFIND，不是 GET。
    ///
    /// 这里只能验证「确实调用了 set_agent」这件事的可观察后果：构造一个指向
    /// 本机的 store 不报错，且它内部两个客户端来自同一次 build。真正的证据是
    /// tests/webdav_server.rs 在故意设置死代理时仍全部通过。
    #[test]
    fn dav_client_shares_the_configured_agent() {
        let s = WebDavStore::new(WebDavConfig {
            base_url: String::from("http://127.0.0.1:8080/dav"),
            username: String::from("u"),
            password: String::from("p"),
            ..WebDavConfig::default()
        })
        .expect("本机地址应当能构造");
        // 配置回填必须保留原地址，否则设置界面会显示成另一个地址
        assert_eq!(s.config().base_url, "http://127.0.0.1:8080/dav");
    }

    /// 只读配置必须拒绝所有写操作，而且是在**发请求之前**。
    ///
    /// 不这样会怎样：向只读账号发 DELETE，服务端可能返回 403，也可能
    /// 意外成功（账号权限与用户选择不一致时）——后者就是数据丢失。
    #[test]
    fn read_only_refuses_writes_before_request() {
        let s = store(false);
        let rt = tokio::runtime::Builder::new_current_thread().build().expect("建运行时");
        assert!(matches!(rt.block_on(s.delete("/a")), Err(Error::Unsupported("delete"))));
        assert!(matches!(rt.block_on(s.write("/", "a", b"x")), Err(Error::Unsupported("write"))));
        assert!(matches!(rt.block_on(s.rename("/a", "b")), Err(Error::Unsupported("rename"))));
        assert!(matches!(
            rt.block_on(s.create_dir("/", "d")),
            Err(Error::Unsupported("create_dir"))
        ));
        assert!(!s.capabilities().any_write(), "能力位图要与行为一致");
    }

    /// 路径要编码，但 `/` 必须保留为分隔符；扩展名点号不能被编码。
    ///
    /// 不这样会怎样：把 `/` 也编码，整条路径会变成一个文件名，
    /// 服务端返回 404，而错误信息里看不出是编码问题。把 `.` 编码成 `%2E`
    /// 则会在坚果云等把 `%2E` 当字面量的服务端上直接 404（真实服务器回归）。
    #[test]
    fn url_encodes_segments_but_keeps_slash() {
        let s = store(false);
        let u = s.url_for("影视/沙丘 2.mkv");
        assert!(u.starts_with("https://dav.example.com/dav/"));
        assert!(u.contains('/'), "分隔符要保留");
        assert!(!u.contains(' '), "空格必须编码");
        assert!(!u.contains('影'), "中文必须编码");
        assert!(u.contains(".mkv"), "扩展名点号必须保留");
        assert!(!u.contains("%2E"), "点号不能编码成 %2E");
        // 段数不变
        assert_eq!(u.trim_start_matches("https://dav.example.com/dav/").split('/').count(), 2);
    }

    /// unreserved 字符（`. - _ ~`）原样保留，只有空格这类才编码。
    ///
    /// 不这样会怎样：备份类文件名 `Movie-2_backup.tar.omy` 被编码成一串
    /// `%2D`/`%5F`/`%2E`，部分服务端无法识别，且日志完全不可读。
    #[test]
    fn url_keeps_unreserved_punctuation() {
        let s = store(false);
        let u = s.url_for("My Movie-2_backup.tar.omy");
        assert!(u.ends_with("My%20Movie-2_backup.tar.omy"), "实际: {u}");
        assert!(!u.contains("%2D") && !u.contains("%5F") && !u.contains("%2E"));
    }

    /// 根路径要能正确拼出来。
    #[test]
    fn url_for_root() {
        let s = store(false);
        assert_eq!(s.url_for(""), "https://dav.example.com/dav/");
    }

    /// Basic 认证编码必须正确，否则每次请求都 401。
    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // 实际用法：user:pass
        assert_eq!(base64_encode(b"u:p"), "dTpw");
    }

    /// Debug 输出不能带出密码。
    ///
    /// 不这样会怎样：错误信息里顺手 `{:?}` 一下，明文密码就进了日志。
    #[test]
    fn debug_does_not_leak_password() {
        let s = store(true);
        let d = format!("{s:?}");
        assert!(!d.contains('p') || !d.contains("password"), "不能出现密码字段");
        assert!(!d.contains("Basic"));
        assert!(d.contains("dav.example.com"), "该有的定位信息要有");
    }

    /// 路径工具要处理好首尾斜杠。
    #[test]
    fn path_helpers() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/a/", "b"), "/a/b");
        assert_eq!(join("", "b"), "/b");
        assert_eq!(dirname("/a/b/c.txt"), "/a/b");
        assert_eq!(dirname("/a"), "");
        assert_eq!(basename("/a/b/c.txt"), "c.txt");
        assert_eq!(basename("/a/b/"), "b");
    }

    /// 归一化要能识别出「同一个目录的不同写法」。
    ///
    /// 不这样会怎样：PROPFIND 返回的自身条目去不掉，每进一层目录
    /// 就多出一个指向自己的项，用户会看到无限嵌套。
    #[test]
    fn normalize_identifies_same_path() {
        assert_eq!(normalize("/a/b/"), normalize("a/b"));
        assert_eq!(normalize("/%E5%BD%B1%E8%A7%86/"), normalize("影视"));
    }

    /// 可写配置声明的能力要与实际一致。
    #[test]
    fn writable_caps() {
        let c = store(true).capabilities();
        assert!(c.write && c.delete && c.rename && c.create_dir);
        assert!(!c.random_write, "WebDAV 没有部分写");
        assert!(c.range_read);
    }
}
