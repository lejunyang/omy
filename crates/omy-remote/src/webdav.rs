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
    /// 建立连接（不发请求，仅构造客户端）。
    ///
    /// # Errors
    ///
    /// URL 非法或 TLS 初始化失败时返回。
    pub fn new(cfg: WebDavConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            // 跟随重定向时 reqwest 会丢掉 Authorization 头（这是正确的
            // 安全行为：不该把凭据发给未知主机）。部分服务端读取时会 302
            // 到另一个域，届时表现为 401——rclone 专门为此有个
            // auth_redirect 选项。这里先保持默认的安全行为。
            .build()
            .map_err(|e| Error::Network(e.to_string()))?;

        let mut b = reqwest_dav::ClientBuilder::new().set_host(cfg.base_url.clone());
        if !cfg.username.is_empty() {
            b = b.set_auth(reqwest_dav::Auth::Basic(
                cfg.username.clone(),
                cfg.password.clone(),
            ));
        }
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
        })
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
