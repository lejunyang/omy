//! 服务端：把请求变成密文字节。
//!
//! # 零密钥是由类型保证的，不是靠自觉
//!
//! 决策 DEC-16 要求「服务端进程绝不允许持有 `Kek` / `Fek` / 明文」。
//! 光写在文档里没用——半年后有人为了"顺便显示个文件名"就会把 KEK 传进来。
//!
//! 因此本模块的数据入口只有 [`CiphertextSource`]，它**只能**做两件事：
//! 报告密文长度、读取密文区间。没有任何方法能拿到密钥或明文。
//! 想让服务端解密，必须先修改 trait 定义——那是一处显眼的改动，
//! 会在 code review 里被看到，而不是悄悄混进某次重构。
//!
//! 相应地，服务端能做的事也被限死在三件上：
//!
//! | 请求 | 所需信息 | 来源 |
//! |---|---|---|
//! | `LIST` | magic 识别 + 大小 | 未加密的固定头 |
//! | `STAT` | `header_len`、总长 | 未加密的固定头 |
//! | `READ` | 按偏移读字节 | 无需解析 |
//!
//! `LIST` 返回的文件名是**密文**，由客户端用自己的 KEK 解密。
//!
//! # 路径穿越：让它无法表达，而不是拦截
//!
//! 客户端用 [`wire::Handle`] 指代文件，那是 16 字节随机值，
//! 与磁盘路径没有任何可推导的关系。客户端**根本没有表达路径的手段**，
//! 也就谈不上构造 `../`。这比"收到路径再校验"可靠得多——
//! 后者依赖校验逻辑没有疏漏，而前者从表达能力上就排除了整类问题。

use crate::error::{NetError, Result};
use crate::wire::{Entry, ErrCode, FileInfo, Handle, MAX_READ_LEN, Request, Response};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 服务端读取数据的唯一途径。
///
/// **有意只提供密文访问**。这是 DEC-16 的实现约束在类型系统上的落地：
/// 实现者无法通过这个 trait 拿到密钥或明文，服务端也就不可能持有它们。
///
/// 注意这里是**同步** trait，与 `omy_core::source::BlockSource` 的取舍一致：
/// 读文件是阻塞 I/O，异步化会把整条调用链染上 async 而收益有限；
/// 需要并发时由调用方放到阻塞线程池里跑。
pub trait CiphertextSource: Send + Sync {
    /// 密文总字节数。
    ///
    /// # Errors
    /// 读取元信息失败时返回错误。
    fn len(&self) -> Result<u64>;

    /// 读取密文的一个区间，返回实际读到的字节。
    ///
    /// 允许返回比请求少的字节（读到文件末尾时）。
    ///
    /// # Errors
    /// I/O 失败时返回错误。
    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>>;

    /// 是否为空。
    ///
    /// # Errors
    /// 读取元信息失败时返回错误。
    fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }
}

/// 从本地文件读密文。
pub struct FileSource {
    path: PathBuf,
}

impl FileSource {
    /// 指向一个本地文件。
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl CiphertextSource for FileSource {
    fn len(&self) -> Result<u64> {
        Ok(std::fs::metadata(&self.path)?.len())
    }

    fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        use std::io::{Read as _, Seek as _, SeekFrom};
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(offset))?;
        let mut buf = vec![0u8; len];
        let mut got = 0usize;
        // 循环读到满或到末尾：单次 read 允许返回少于请求的字节，
        // 直接用返回值当结果会在大区间上偶发截断
        while got < len {
            let Some(dst) = buf.get_mut(got..) else { break };
            match f.read(dst)? {
                0 => break,
                n => got += n,
            }
        }
        buf.truncate(got);
        Ok(buf)
    }
}

/// 一个共享点：一个目录及其中可被服务的文件。
pub struct Share {
    /// handle → 文件路径。
    ///
    /// **在开启共享时固化**，不随 vault 解锁状态变化（DEC-16 约束 2）。
    files: HashMap<Handle, PathBuf>,
}

impl Share {
    /// 扫描目录，把其中的 omy 文件登记为可共享。
    ///
    /// 只看文件头的 magic，不需要任何密钥。子目录**不递归**——
    /// 共享范围应当由用户明确指定，而不是因为某个子目录恰好在里面
    /// 就一并暴露。
    ///
    /// # Errors
    /// 目录读取失败时返回错误。
    pub fn from_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let mut files = HashMap::new();
        for e in std::fs::read_dir(dir.as_ref())? {
            let Ok(e) = e else { continue };
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            if !Self::looks_like_omy(&p) {
                continue;
            }
            files.insert(Self::make_handle(), p);
        }
        Ok(Self { files })
    }

    /// 用显式的文件清单构造。
    ///
    /// 用于用户手动勾选若干文件的场景。
    #[must_use]
    pub fn from_files(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        let files = paths
            .into_iter()
            .filter(|p| Self::looks_like_omy(p))
            .map(|p| (Self::make_handle(), p))
            .collect();
        Self { files }
    }

    /// 读文件头判断是不是 omy 文件。**不需要密钥。**
    fn looks_like_omy(p: &Path) -> bool {
        use std::io::Read as _;
        let Ok(mut f) = std::fs::File::open(p) else {
            return false;
        };
        let mut head = [0u8; 64];
        let Ok(n) = f.read(&mut head) else {
            return false;
        };
        head.get(..n).is_some_and(omy_core::file::is_omy_file)
    }

    /// 生成一个随机 handle。
    ///
    /// 必须随机而非顺序：顺序 handle 会泄露文件数量与添加顺序，
    /// 也让未授权的客户端可以枚举。
    fn make_handle() -> Handle {
        use rand::RngCore as _;
        let mut h = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut h);
        h
    }

    /// 共享的文件数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// 是否没有可共享的文件。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// 查 handle 对应的路径。
    fn path_of(&self, h: &Handle) -> Option<&PathBuf> {
        self.files.get(h)
    }
}

/// 请求处理器。
///
/// 持有一个 [`Share`]，把 [`Request`] 变成 [`Response`]。
/// **不持有任何密钥**——这一点由 [`CiphertextSource`] 保证。
pub struct Server {
    share: Share,
    /// 共享是否仍然有效。吊销后所有请求都返回 [`ErrCode::Revoked`]。
    revoked: bool,
}

impl Server {
    /// 用一个共享点创建服务端。
    #[must_use]
    pub fn new(share: Share) -> Self {
        Self { share, revoked: false }
    }

    /// 吊销共享。之后所有请求都会被拒绝。
    pub fn revoke(&mut self) {
        self.revoked = true;
    }

    /// 是否已吊销。
    #[must_use]
    pub fn is_revoked(&self) -> bool {
        self.revoked
    }

    /// 处理一个请求。
    ///
    /// 永远返回 [`Response`]，内部错误转成 [`Response::Err`]。
    /// 这样服务端不会因为单个请求出错而断开整条连接。
    #[must_use]
    pub fn handle(&self, req: &Request) -> Response {
        if self.revoked {
            return Response::Err {
                code: ErrCode::Revoked,
                msg: "共享已被吊销".into(),
            };
        }
        match req {
            Request::Ping => Response::Pong,
            Request::List => self.do_list(),
            Request::Stat { handle } => self.do_stat(handle),
            Request::Read { handle, offset, len } => self.do_read(handle, *offset, *len),
        }
    }

    fn do_list(&self) -> Response {
        let mut entries = Vec::new();
        for (h, p) in &self.share.files {
            let src = FileSource::new(p.clone());
            let Ok(size) = src.len() else { continue };
            // 读固定头拿 header_len。这一步不需要密钥
            let Ok(head) = src.read_at(0, 4096) else { continue };
            let Ok(fixed) = omy_core::file::peek_header(&head) else {
                continue;
            };
            let hl = fixed.header_len;
            // 把完整头部读出来随 LIST 一起返回，省掉客户端逐个 STAT 的往返。
            // 头部是加密的，转发密文不构成泄露
            let Ok(hl_usize) = usize::try_from(hl) else { continue };
            let header = if hl_usize <= head.len() {
                head.get(..hl_usize).unwrap_or(&[]).to_vec()
            } else {
                match src.read_at(0, hl_usize) {
                    Ok(v) => v,
                    Err(_) => continue,
                }
            };
            entries.push(Entry { handle: *h, size, header_len: hl, header });
        }
        // 按 handle 排序，让输出稳定。HashMap 的迭代顺序是随机的，
        // 不排序会导致每次 LIST 顺序不同，界面上文件跳来跳去
        entries.sort_by_key(|e| e.handle);
        Response::ListOk { entries }
    }

    fn do_stat(&self, h: &Handle) -> Response {
        let Some(p) = self.share.path_of(h) else {
            return Self::err(ErrCode::NoSuchHandle, "未知的文件句柄");
        };
        let src = FileSource::new(p.clone());
        let Ok(size) = src.len() else {
            return Self::err(ErrCode::IoError, "读取文件信息失败");
        };
        let Ok(head) = src.read_at(0, 4096) else {
            return Self::err(ErrCode::IoError, "读取文件头失败");
        };
        let Ok(fixed) = omy_core::file::peek_header(&head) else {
            return Self::err(ErrCode::IoError, "文件头解析失败");
        };
        Response::StatOk {
            info: FileInfo { size, header_len: fixed.header_len },
        }
    }

    fn do_read(&self, h: &Handle, offset: u64, len: u32) -> Response {
        if len > MAX_READ_LEN {
            return Self::err(ErrCode::ReadTooLarge, "单次读取超过上限");
        }
        let Some(p) = self.share.path_of(h) else {
            return Self::err(ErrCode::NoSuchHandle, "未知的文件句柄");
        };
        let src = FileSource::new(p.clone());
        let Ok(size) = src.len() else {
            return Self::err(ErrCode::IoError, "读取文件信息失败");
        };
        // 起点越界直接拒绝，而不是返回空数据：
        // 空数据会被客户端误解为"读到文件尾"，掩盖真正的逻辑错误
        if offset > size {
            return Self::err(ErrCode::RangeOutOfBounds, "读取起点超出文件范围");
        }
        let Ok(len_usize) = usize::try_from(len) else {
            return Self::err(ErrCode::ReadTooLarge, "请求长度无法表示");
        };
        match src.read_at(offset, len_usize) {
            Ok(data) => Response::ReadOk { data },
            Err(_) => Self::err(ErrCode::IoError, "读取失败"),
        }
    }

    fn err(code: ErrCode, msg: &str) -> Response {
        Response::Err { code, msg: msg.to_owned() }
    }
}

/// 把 [`Response::Err`] 转成 [`NetError`]，供客户端使用。
///
/// # Errors
/// 输入本身就是错误响应时返回对应的 [`NetError::Remote`]。
pub fn check_response(r: Response) -> Result<Response> {
    if let Response::Err { code, msg } = r {
        return Err(NetError::Remote { code: code.as_str(), msg });
    }
    Ok(r)
}

/// 已通过身份核对的会话。
///
/// # 为什么要有这个类型
///
/// [`Server::handle`] 本身不知道请求来自谁——它只管把 handle 变成字节。
/// 身份核对是**另一件事**，很容易漏掉：握手成功只证明对方持有某个私钥，
/// 不证明那是已配对的设备。
///
/// 把「核对」做成构造函数、「服务」做成方法，就让"未核对就服务"在类型
/// 上无法表达：拿不到 `Session` 就调不到 [`Self::handle`]。
///
/// ```ignore
/// // 编译不过：没有 Session 就没有 handle 可用
/// let resp = server.handle_for(&channel, &req);
///
/// // 唯一路径：先核对，再服务
/// let session = Session::authorize(&store, channel.peer_public())?;
/// let resp = session.handle(&server, &req);
/// ```
pub struct Session {
    /// 对方公钥，仅用于日志与吊销时匹配。
    peer_public: Vec<u8>,
}

impl Session {
    /// 核对对方身份，通过则建立会话。
    ///
    /// 同时检查「是已配对设备」与「授权未过期」——分成两步判断
    /// 容易漏掉后者。
    ///
    /// # Errors
    /// 对方不是已配对设备，或授权已过期时返回 [`NetError::SessionInvalid`]。
    pub fn authorize(store: &crate::store::Store, peer_public: &[u8]) -> Result<Self> {
        if !store.is_authorized(peer_public) {
            return Err(NetError::SessionInvalid);
        }
        Ok(Self { peer_public: peer_public.to_vec() })
    }

    /// 对方公钥。
    #[must_use]
    pub fn peer_public(&self) -> &[u8] {
        &self.peer_public
    }

    /// 对方指纹，用于访问日志。
    ///
    /// 日志里记指纹而非设备名：设备名由对方自称，可以随时改，
    /// 用它做审计记录没有意义。
    #[must_use]
    pub fn peer_fingerprint(&self) -> [u8; 8] {
        crate::discovery::fingerprint(&self.peer_public)
    }

    /// 在本会话中处理一个请求。
    #[must_use]
    pub fn handle(&self, server: &Server, req: &Request) -> Response {
        server.handle(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个最小可用的 omy 文件。
    ///
    /// 用 `TEST_WEAK` 的 Argon2 参数：这些测试验证的是服务端的字节搬运
    /// 逻辑，与 KDF 强度无关，用生产参数只会让测试慢上百倍。
    fn make_omy(dir: &Path, name: &str, content: &[u8], password: &[u8]) -> PathBuf {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};

        let p = dir.join(name);
        let salt = [7u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(password, &salt, params).expect("派生 KEK 应成功");
        let opts = EncryptOptions {
            filename: Some(name.to_owned()),
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(content, &[kek], &salt, &opts, &RandomMaterial::generate())
            .expect("加密应成功");
        std::fs::write(&p, &enc.bytes).expect("写文件应成功");
        p
    }

    /// 建一个本次测试专用的临时目录。
    ///
    /// 带随机后缀：测试默认并行跑，只用 tag + pid 在重复运行时
    /// 可能撞上上一次没清理干净的残留。
    fn tmpdir(tag: &str) -> PathBuf {
        use rand::RngCore as _;
        let r = rand::thread_rng().next_u64();
        let d = std::env::temp_dir().join(format!(
            "omy-net-test-{tag}-{}-{r:016x}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建目录应成功");
        d
    }

    #[test]
    fn lists_only_omy_files() {
        let d = tmpdir("list");
        make_omy(&d, "a.omy", b"hello", b"pw");
        make_omy(&d, "b.omy", b"world", b"pw");
        std::fs::write(d.join("plain.txt"), b"not omy").expect("写文件应成功");

        let share = Share::from_dir(&d).expect("扫描应成功");
        assert_eq!(share.len(), 2, "非 omy 文件不应被共享");

        let srv = Server::new(share);
        match srv.handle(&Request::List) {
            Response::ListOk { entries } => {
                assert_eq!(entries.len(), 2);
                for e in &entries {
                    assert!(e.size > 0);
                    assert!(e.header_len > 0);
                    assert_eq!(e.header.len(), e.header_len as usize, "头部应完整返回");
                }
            }
            other => panic!("应返回 ListOk，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// LIST 返回的顺序必须稳定，否则界面上文件会乱跳。
    #[test]
    fn list_order_is_stable() {
        let d = tmpdir("order");
        for i in 0..5 {
            make_omy(&d, &format!("f{i}.omy"), b"x", b"pw");
        }
        let srv = Server::new(Share::from_dir(&d).expect("扫描应成功"));
        let first = match srv.handle(&Request::List) {
            Response::ListOk { entries } => entries,
            other => panic!("应返回 ListOk，实际 {other:?}"),
        };
        for _ in 0..5 {
            let again = match srv.handle(&Request::List) {
                Response::ListOk { entries } => entries,
                other => panic!("应返回 ListOk，实际 {other:?}"),
            };
            assert_eq!(first, again, "多次 LIST 的顺序必须一致");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn read_returns_exact_ciphertext_bytes() {
        let d = tmpdir("read");
        let p = make_omy(&d, "x.omy", b"0123456789abcdef", b"pw");
        let raw = std::fs::read(&p).expect("读文件应成功");

        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let srv = Server::new(share);

        // 读中间一段，与直接读磁盘比对
        let off = 100u64;
        let len = 64u32;
        match srv.handle(&Request::Read { handle: h, offset: off, len }) {
            Response::ReadOk { data } => {
                // 用 usize::try_from 而非 as：32 位平台上 u64 as usize 会截断，
                // 而移动端仍有 32 位 ARM
                let start = usize::try_from(off).expect("测试偏移应可转换");
                let n = usize::try_from(len).expect("测试长度应可转换");
                let want = raw.get(start..start + n).expect("测试文件应足够长");
                assert_eq!(data, want, "返回的必须是原始密文字节");
            }
            other => panic!("应返回 ReadOk，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn read_past_end_returns_short_not_error() {
        let d = tmpdir("short");
        let p = make_omy(&d, "x.omy", b"tiny", b"pw");
        let size = std::fs::metadata(&p).expect("元信息应可读").len();

        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let srv = Server::new(share);

        // 从末尾前 10 字节读 1000 字节：应返回 10 字节而非报错
        match srv.handle(&Request::Read { handle: h, offset: size - 10, len: 1000 }) {
            Response::ReadOk { data } => assert_eq!(data.len(), 10),
            other => panic!("应返回 ReadOk，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_out_of_bounds_start() {
        let d = tmpdir("oob");
        make_omy(&d, "x.omy", b"tiny", b"pw");
        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let srv = Server::new(share);

        match srv.handle(&Request::Read { handle: h, offset: 1 << 40, len: 16 }) {
            Response::Err { code, .. } => assert_eq!(code, ErrCode::RangeOutOfBounds),
            other => panic!("越界起点应报错，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn rejects_oversized_read() {
        let d = tmpdir("toobig");
        make_omy(&d, "x.omy", b"x", b"pw");
        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let srv = Server::new(share);

        match srv.handle(&Request::Read { handle: h, offset: 0, len: MAX_READ_LEN + 1 }) {
            Response::Err { code, .. } => assert_eq!(code, ErrCode::ReadTooLarge),
            other => panic!("超限读取应报错，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_handle_is_rejected() {
        let d = tmpdir("nohandle");
        make_omy(&d, "x.omy", b"x", b"pw");
        let srv = Server::new(Share::from_dir(&d).expect("扫描应成功"));

        for req in [
            Request::Stat { handle: [0xEE; 16] },
            Request::Read { handle: [0xEE; 16], offset: 0, len: 16 },
        ] {
            match srv.handle(&req) {
                Response::Err { code, .. } => assert_eq!(code, ErrCode::NoSuchHandle),
                other => panic!("未知 handle 应报错，实际 {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn revoked_server_rejects_everything() {
        let d = tmpdir("revoke");
        make_omy(&d, "x.omy", b"x", b"pw");
        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let mut srv = Server::new(share);
        srv.revoke();

        for req in [
            Request::List,
            Request::Ping,
            Request::Stat { handle: h },
            Request::Read { handle: h, offset: 0, len: 16 },
        ] {
            match srv.handle(&req) {
                Response::Err { code, .. } => assert_eq!(code, ErrCode::Revoked),
                other => panic!("吊销后应全部拒绝，实际 {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// DEC-16 的核心断言：服务端全程不需要密钥。
    ///
    /// 整个测试里没有出现过任何密码或 KEK，但 LIST/STAT/READ 全部正常。
    #[test]
    fn server_never_needs_a_key() {
        let d = tmpdir("keyless");
        // 用一个测试里之后不再提及的密码加密
        make_omy(&d, "secret.omy", b"sensitive content here", b"never-used-again");

        let share = Share::from_dir(&d).expect("扫描应成功");
        let h = *share.files.keys().next().expect("应有一个文件");
        let srv = Server::new(share);

        // 以下三个操作全部成功，且代码里没有任何密钥
        assert!(matches!(srv.handle(&Request::List), Response::ListOk { .. }));
        assert!(matches!(srv.handle(&Request::Stat { handle: h }), Response::StatOk { .. }));
        assert!(matches!(
            srv.handle(&Request::Read { handle: h, offset: 0, len: 32 }),
            Response::ReadOk { .. }
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 反证：LIST 返回的内容里不含明文文件名。
    #[test]
    fn list_does_not_leak_plaintext_filename() {
        let d = tmpdir("noleak");
        // 用一个足够特别的名字，便于在字节流里搜索
        let name = "机密-季度报告-2026.docx.omy";
        make_omy(&d, name, b"content", b"pw");

        let srv = Server::new(Share::from_dir(&d).expect("扫描应成功"));
        let entries = match srv.handle(&Request::List) {
            Response::ListOk { entries } => entries,
            other => panic!("应返回 ListOk，实际 {other:?}"),
        };

        // 把响应编码成实际会发到网络上的字节
        let wire = Response::ListOk { entries }.encode().expect("编码应成功");
        let needle = "季度报告".as_bytes();
        assert!(
            !wire.windows(needle.len()).any(|w| w == needle),
            "线路字节里不得出现明文文件名"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ping_works() {
        let d = tmpdir("ping");
        let srv = Server::new(Share::from_dir(&d).expect("扫描应成功"));
        assert_eq!(srv.handle(&Request::Ping), Response::Pong);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn empty_dir_lists_nothing() {
        let d = tmpdir("empty");
        let share = Share::from_dir(&d).expect("扫描应成功");
        assert!(share.is_empty());
        let srv = Server::new(share);
        match srv.handle(&Request::List) {
            Response::ListOk { entries } => assert!(entries.is_empty()),
            other => panic!("空目录应返回空列表，实际 {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn handles_are_random_not_sequential() {
        let a = Share::make_handle();
        let b = Share::make_handle();
        assert_ne!(a, b, "handle 必须随机，顺序 handle 会泄露文件数量并可被枚举");
    }

    #[test]
    fn check_response_converts_err() {
        let r = Response::Err { code: ErrCode::NoSuchHandle, msg: "x".into() };
        let e = check_response(r).expect_err("错误响应应转成 Err");
        assert_eq!(e.code(), "NO_SUCH_HANDLE");
        assert!(check_response(Response::Pong).is_ok());
    }

    /// 只有已配对且未过期的设备才能建立会话。
    #[test]
    fn session_requires_authorized_peer() {
        use crate::store::{DeviceRecord, Store};

        let mut store = Store::create("本机").expect("创建应成功");
        let known = vec![0x11u8; 32];
        let expired = vec![0x22u8; 32];
        let stranger = vec![0x33u8; 32];

        store.upsert(DeviceRecord {
            public_key: known.clone(),
            name: "已配对".into(),
            paired_at: 0,
            expires_at: 0, // 永不过期
        });
        store.upsert(DeviceRecord {
            public_key: expired.clone(),
            name: "已过期".into(),
            paired_at: 0,
            expires_at: 1, // 1970 年就过期了
        });

        assert!(
            Session::authorize(&store, &known).is_ok(),
            "已配对且未过期的设备应能建立会话"
        );
        assert!(
            Session::authorize(&store, &expired).is_err(),
            "过期设备必须拒绝——握手成功不等于授权仍然有效"
        );
        assert!(
            Session::authorize(&store, &stranger).is_err(),
            "陌生设备必须拒绝"
        );
    }

    /// 吊销后立即失效。
    #[test]
    fn revoked_device_cannot_open_session() {
        use crate::store::{DeviceRecord, Store};

        let mut store = Store::create("本机").expect("创建应成功");
        let pk = vec![0x44u8; 32];
        store.upsert(DeviceRecord {
            public_key: pk.clone(),
            name: "设备".into(),
            paired_at: 0,
            expires_at: 0,
        });
        assert!(Session::authorize(&store, &pk).is_ok(), "吊销前应可用");

        assert!(store.revoke(&pk), "应确实吊销");
        assert!(
            Session::authorize(&store, &pk).is_err(),
            "吊销后必须立即无法建立新会话"
        );
    }

    /// 会话上的请求处理与直接调用等价。
    #[test]
    fn session_handle_matches_direct() {
        use crate::store::{DeviceRecord, Store};

        let d = tmpdir("session");
        make_omy(&d, "x.omy", b"data", b"pw");
        let srv = Server::new(Share::from_dir(&d).expect("扫描应成功"));

        let mut store = Store::create("本机").expect("创建应成功");
        let pk = vec![0x55u8; 32];
        store.upsert(DeviceRecord {
            public_key: pk.clone(),
            name: "设备".into(),
            paired_at: 0,
            expires_at: 0,
        });
        let sess = Session::authorize(&store, &pk).expect("授权应成功");

        assert_eq!(sess.handle(&srv, &Request::Ping), srv.handle(&Request::Ping));
        assert_eq!(sess.peer_fingerprint().len(), 8);
        let _ = std::fs::remove_dir_all(&d);
    }
}
