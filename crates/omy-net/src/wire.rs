//! 线路协议：请求与响应的编解码。
//!
//! # 为什么手写二进制而不用 serde
//!
//! 这一层直接解析**来自网络的不可信字节**。用 `serde_json` 会引入
//! 一整套通用解析器作为攻击面，而我们只需要五种消息、字段总数不到 20。
//! 手写编解码可以做到：
//!
//! - 每个长度字段解析时**立即**对照剩余字节校验，不给 `Vec::with_capacity`
//!   传入未经检查的数字（这是最经典的远程内存耗尽路径）；
//! - 全程不 panic：所有切片走 `get()`，所有算术走 `checked_*`；
//! - 拒绝比接受更容易——不认识的东西一律报错，不做"尽力而为"的解析。
//!
//! # 与 Noise 的分层
//!
//! 本模块只管**一条消息**的字节格式。消息的加密、认证、防重放由下层的
//! Noise 传输态负责。Spike 实测 Noise 单条消息明文上限 **65519 字节**
//! （65535 密文上限 − 16 字节 tag），因此 [`MAX_FRAME`] 取该值，
//! 更大的负载由 [`Response::ReadOk`] 分多条消息传输。

use crate::error::{NetError, Result};

/// 单条线路消息的最大字节数。
///
/// 等于 Noise 传输态单条消息的明文上限（spike 实测值）。超过这个长度的
/// 数据必须由调用方拆分。
pub const MAX_FRAME: usize = 65519;

/// 单次 READ 允许请求的最大字节数。
///
/// 取值明显小于 [`MAX_FRAME`]，为响应头部留出余量。客户端要读更多数据
/// 就发多个请求——这也让服务端的内存占用有确定上界，
/// 不随客户端的胃口增长。
pub const MAX_READ_LEN: u32 = 60 * 1024;

/// 不透明文件句柄。
///
/// **有意不含路径信息**。服务端内部维护 handle → 路径的映射，
/// 客户端永远看不到磁盘布局，也就不存在路径穿越的可能——
/// 不是靠校验拦住 `../`，而是让客户端根本没有表达路径的手段。
pub type Handle = [u8; 16];

/// 客户端请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// 列出共享点内的全部 omy 文件。
    List,
    /// 查询单个文件的元信息。
    Stat {
        /// 目标文件的不透明句柄。
        handle: Handle,
    },
    /// 读取密文的一个区间。
    ///
    /// `offset` 是**文件绝对偏移**（含头部），因为服务端不解析载荷结构，
    /// 只做字节搬运。客户端拿到密文后自行解密。
    Read {
        /// 目标文件的不透明句柄。
        handle: Handle,
        /// 文件绝对偏移（含头部）。
        offset: u64,
        /// 请求的字节数，不得超过 [`MAX_READ_LEN`]。
        len: u32,
    },
    /// 心跳，用于保活与探测连接是否仍然有效。
    Ping,
}

/// 服务端响应。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// LIST 的结果。
    ListOk {
        /// 共享点内的全部条目，按 handle 排序以保证顺序稳定。
        entries: Vec<Entry>,
    },
    /// STAT 的结果。
    StatOk {
        /// 文件的元信息。
        info: FileInfo,
    },
    /// READ 的结果：一段密文。
    ReadOk {
        /// 密文字节。可能短于请求长度（读到文件末尾时）。
        data: Vec<u8>,
    },
    /// PING 的应答。
    Pong,
    /// 出错。`code` 是稳定的机器可读常量，`msg` 仅供人看。
    Err {
        /// 稳定的机器可读错误码。
        code: ErrCode,
        /// 人类可读的描述，不应用于程序判断。
        msg: String,
    },
}

/// LIST 返回的单个条目。
///
/// ⚠️ **不含明文文件名**。服务端零密钥（决策 D-30 / DEC-16），
/// 它根本无从得知文件叫什么。`name_ct` 是从文件头原样取出的
/// 文件名密文，由客户端用自己的 KEK 解密。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 该文件的不透明句柄，后续 STAT/READ 用它指代。
    pub handle: Handle,
    /// 密文总长（磁盘上的字节数）。
    pub size: u64,
    /// 文件头长度，客户端据此知道载荷从哪开始。
    pub header_len: u32,
    /// 文件头原始字节。
    ///
    /// 客户端需要它来解密文件名、读取媒体元信息与缩略图。
    /// 直接随 LIST 返回可以避免"列个目录要来回几十次往返"。
    /// 头部本身是加密的，服务端转发密文不构成泄露。
    pub header: Vec<u8>,
}

/// STAT 返回的详细信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// 密文总长（磁盘上的字节数）。
    pub size: u64,
    /// 文件头长度，客户端据此知道载荷从哪开始。
    pub header_len: u32,
}

/// 稳定的错误码。
///
/// 用显式判别值而非依赖枚举顺序：这些数字会出现在网络字节流里，
/// 中间插入一个变体就会让新旧版本对不上。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrCode {
    /// 未知的 handle。
    NoSuchHandle = 1,
    /// 请求的区间超出文件范围。
    RangeOutOfBounds = 2,
    /// 请求长度超过 [`MAX_READ_LEN`]。
    ReadTooLarge = 3,
    /// 服务端读取磁盘失败。
    IoError = 4,
    /// 请求格式非法。
    BadRequest = 5,
    /// 共享已被吊销或会话过期。
    Revoked = 6,
}

impl ErrCode {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::NoSuchHandle,
            2 => Self::RangeOutOfBounds,
            3 => Self::ReadTooLarge,
            4 => Self::IoError,
            5 => Self::BadRequest,
            6 => Self::Revoked,
            _ => return None,
        })
    }

    /// 机器可读的稳定字符串，用于 CLI 的 `--json` 输出。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoSuchHandle => "NO_SUCH_HANDLE",
            Self::RangeOutOfBounds => "RANGE_OUT_OF_BOUNDS",
            Self::ReadTooLarge => "READ_TOO_LARGE",
            Self::IoError => "IO_ERROR",
            Self::BadRequest => "BAD_REQUEST",
            Self::Revoked => "REVOKED",
        }
    }
}

// 消息类型标签。同样用显式值，理由同 ErrCode。
const T_LIST: u8 = 0x01;
const T_STAT: u8 = 0x02;
const T_READ: u8 = 0x03;
const T_PING: u8 = 0x04;

const T_LIST_OK: u8 = 0x81;
const T_STAT_OK: u8 = 0x82;
const T_READ_OK: u8 = 0x83;
const T_PONG: u8 = 0x84;
const T_ERR: u8 = 0x85;

/// 只增不减的写入器。
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new(tag: u8) -> Self {
        Self { buf: vec![tag] }
    }
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    /// 写入带长度前缀的字节串。
    fn bytes(&mut self, v: &[u8]) -> Result<()> {
        let len = u32::try_from(v.len()).map_err(|_| NetError::FrameTooLarge)?;
        self.u32(len);
        self.raw(v);
        Ok(())
    }
    fn finish(self) -> Result<Vec<u8>> {
        if self.buf.len() > MAX_FRAME {
            return Err(NetError::FrameTooLarge);
        }
        Ok(self.buf)
    }
}

/// 逐字段前进的读取器。
///
/// 所有取值都经过边界检查，**任何情况下都不会 panic**——
/// 输入完全由攻击者控制，越界访问就是可远程触发的拒绝服务。
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(NetError::MalformedFrame)?;
        let s = self.buf.get(self.pos..end).ok_or(NetError::MalformedFrame)?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(*self.take(1)?.first().ok_or(NetError::MalformedFrame)?)
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        let arr: [u8; 4] = b.try_into().map_err(|_| NetError::MalformedFrame)?;
        Ok(u32::from_le_bytes(arr))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        let arr: [u8; 8] = b.try_into().map_err(|_| NetError::MalformedFrame)?;
        Ok(u64::from_le_bytes(arr))
    }

    fn handle(&mut self) -> Result<Handle> {
        let b = self.take(16)?;
        b.try_into().map_err(|_| NetError::MalformedFrame)
    }

    /// 读取带长度前缀的字节串。
    ///
    /// **关键**：先取长度，再**立刻**用 `take` 对照剩余字节校验，
    /// 校验通过后才复制。绝不能先 `Vec::with_capacity(len)` ——
    /// 那样一条 9 字节的恶意消息就能让服务端尝试分配 4 GiB。
    fn bytes(&mut self) -> Result<Vec<u8>> {
        // 用 try_from 而非 as：32 位平台上 u32 恰好不截断，但显式转换
        // 能表达"这里可能失败"，也防止日后字段类型变宽时静默出错
        let len = usize::try_from(self.u32()?).map_err(|_| NetError::MalformedFrame)?;
        Ok(self.take(len)?.to_vec())
    }

    /// 确认已读到末尾。
    ///
    /// 尾部多余字节说明发送方与我们对格式的理解不一致，必须拒绝：
    /// 静默忽略会让协议在版本演进时出现两边"都能解析但解释不同"的裂缝。
    fn finish(self) -> Result<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(NetError::MalformedFrame)
        }
    }
}

impl Request {
    /// 编码为线路字节。
    ///
    /// # Errors
    /// 超过 [`MAX_FRAME`] 时返回 [`NetError::FrameTooLarge`]。
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = match self {
            Self::List => Writer::new(T_LIST),
            Self::Stat { handle } => {
                let mut w = Writer::new(T_STAT);
                w.raw(handle);
                w
            }
            Self::Read { handle, offset, len } => {
                let mut w = Writer::new(T_READ);
                w.raw(handle);
                w.u64(*offset);
                w.u32(*len);
                w
            }
            Self::Ping => Writer::new(T_PING),
        };
        let _ = &mut w;
        w.finish()
    }

    /// 从线路字节解码。
    ///
    /// # Errors
    /// 格式非法、长度不符或有多余尾部字节时返回 [`NetError::MalformedFrame`]。
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        let tag = r.u8()?;
        let req = match tag {
            T_LIST => Self::List,
            T_STAT => Self::Stat { handle: r.handle()? },
            T_READ => {
                let handle = r.handle()?;
                let offset = r.u64()?;
                let len = r.u32()?;
                Self::Read { handle, offset, len }
            }
            T_PING => Self::Ping,
            _ => return Err(NetError::MalformedFrame),
        };
        r.finish()?;
        Ok(req)
    }
}

impl Response {
    /// 编码为线路字节。
    ///
    /// # Errors
    /// 超过 [`MAX_FRAME`] 时返回 [`NetError::FrameTooLarge`]。
    pub fn encode(&self) -> Result<Vec<u8>> {
        match self {
            Self::ListOk { entries } => {
                let mut w = Writer::new(T_LIST_OK);
                let n = u32::try_from(entries.len()).map_err(|_| NetError::FrameTooLarge)?;
                w.u32(n);
                for e in entries {
                    w.raw(&e.handle);
                    w.u64(e.size);
                    w.u32(e.header_len);
                    w.bytes(&e.header)?;
                }
                w.finish()
            }
            Self::StatOk { info } => {
                let mut w = Writer::new(T_STAT_OK);
                w.u64(info.size);
                w.u32(info.header_len);
                w.finish()
            }
            Self::ReadOk { data } => {
                let mut w = Writer::new(T_READ_OK);
                w.bytes(data)?;
                w.finish()
            }
            Self::Pong => Writer::new(T_PONG).finish(),
            Self::Err { code, msg } => {
                let mut w = Writer::new(T_ERR);
                w.u8(*code as u8);
                w.bytes(msg.as_bytes())?;
                w.finish()
            }
        }
    }

    /// 从线路字节解码。
    ///
    /// # Errors
    /// 格式非法、长度不符或有多余尾部字节时返回 [`NetError::MalformedFrame`]。
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Reader::new(buf);
        let tag = r.u8()?;
        let resp = match tag {
            T_LIST_OK => {
                let n = usize::try_from(r.u32()?).map_err(|_| NetError::MalformedFrame)?;
                // 不预分配 n 个元素：n 来自网络。
                // 每轮循环里的 take 会自然限制真实条目数——
                // 声称有 40 亿条但只给 10 字节的消息会在第一轮就失败。
                let mut entries = Vec::new();
                for _ in 0..n {
                    let handle = r.handle()?;
                    let size = r.u64()?;
                    let header_len = r.u32()?;
                    let header = r.bytes()?;
                    entries.push(Entry { handle, size, header_len, header });
                }
                Self::ListOk { entries }
            }
            T_STAT_OK => {
                let size = r.u64()?;
                let header_len = r.u32()?;
                Self::StatOk { info: FileInfo { size, header_len } }
            }
            T_READ_OK => Self::ReadOk { data: r.bytes()? },
            T_PONG => Self::Pong,
            T_ERR => {
                let code = ErrCode::from_u8(r.u8()?).ok_or(NetError::MalformedFrame)?;
                let raw = r.bytes()?;
                // 错误消息只供人看，非法 UTF-8 用替换字符而非拒绝整条消息：
                // 因解析错误消息失败而丢掉错误本身，会让排查更困难
                let msg = String::from_utf8_lossy(&raw).into_owned();
                Self::Err { code, msg }
            }
            _ => return Err(NetError::MalformedFrame),
        };
        r.finish()?;
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> Handle {
        [n; 16]
    }

    #[test]
    fn request_roundtrip() {
        let cases = vec![
            Request::List,
            Request::Ping,
            Request::Stat { handle: h(7) },
            Request::Read { handle: h(9), offset: 1 << 40, len: 4096 },
        ];
        for c in cases {
            let enc = c.encode().expect("编码应成功");
            let dec = Request::decode(&enc).expect("解码应成功");
            assert_eq!(c, dec, "往返应保持一致");
        }
    }

    #[test]
    fn response_roundtrip() {
        let cases = vec![
            Response::Pong,
            Response::ReadOk { data: vec![1, 2, 3, 4] },
            Response::StatOk { info: FileInfo { size: 12345, header_len: 656 } },
            Response::Err { code: ErrCode::NoSuchHandle, msg: "没有这个文件".into() },
            Response::ListOk {
                entries: vec![
                    Entry { handle: h(1), size: 100, header_len: 656, header: vec![0xAA; 32] },
                    Entry { handle: h(2), size: 200, header_len: 700, header: vec![0xBB; 16] },
                ],
            },
        ];
        for c in cases {
            let enc = c.encode().expect("编码应成功");
            let dec = Response::decode(&enc).expect("解码应成功");
            assert_eq!(c, dec, "往返应保持一致");
        }
    }

    #[test]
    fn rejects_unknown_tag() {
        assert!(Request::decode(&[0xFF]).is_err());
        assert!(Response::decode(&[0x00]).is_err());
    }

    #[test]
    fn rejects_empty() {
        assert!(Request::decode(&[]).is_err());
        assert!(Response::decode(&[]).is_err());
    }

    #[test]
    fn rejects_truncated() {
        // STAT 缺 handle
        assert!(Request::decode(&[T_STAT, 1, 2, 3]).is_err());
        // READ 有 handle 但缺 offset/len
        let mut b = vec![T_READ];
        b.extend_from_slice(&[0u8; 16]);
        assert!(Request::decode(&b).is_err());
    }

    #[test]
    fn rejects_trailing_garbage() {
        let mut enc = Request::List.encode().expect("编码应成功");
        enc.push(0xFF);
        assert!(
            Request::decode(&enc).is_err(),
            "尾部多余字节必须拒绝：静默忽略会在版本演进时造成解释分歧"
        );
    }

    /// 最重要的一条：声称长度巨大但实际没有数据，不得导致大量分配。
    #[test]
    fn huge_length_prefix_does_not_allocate() {
        let mut b = vec![T_READ_OK];
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // 声称有 4 GiB
        // 后面什么都没有
        let r = Response::decode(&b);
        assert!(r.is_err(), "必须拒绝而不是尝试分配 4 GiB");
    }

    #[test]
    fn huge_entry_count_does_not_allocate() {
        let mut b = vec![T_LIST_OK];
        b.extend_from_slice(&u32::MAX.to_le_bytes()); // 声称 40 亿条
        let r = Response::decode(&b);
        assert!(r.is_err(), "必须在第一条就失败，而不是预分配");
    }

    #[test]
    fn rejects_unknown_err_code() {
        let mut b = vec![T_ERR, 0xEE];
        b.extend_from_slice(&0u32.to_le_bytes());
        assert!(Response::decode(&b).is_err(), "未知错误码必须拒绝");
    }

    #[test]
    fn err_msg_tolerates_invalid_utf8() {
        let mut b = vec![T_ERR, ErrCode::IoError as u8];
        let bad = [0xFFu8, 0xFE, 0xFD];
        b.extend_from_slice(&u32::try_from(bad.len()).expect("长度应可转换").to_le_bytes());
        b.extend_from_slice(&bad);
        let r = Response::decode(&b).expect("非法 UTF-8 不应导致整条消息被拒");
        match r {
            Response::Err { code, .. } => assert_eq!(code, ErrCode::IoError),
            other => panic!("应解析为 Err，实际 {other:?}"),
        }
    }

    #[test]
    fn frame_size_limit_enforced() {
        let big = Response::ReadOk { data: vec![0u8; MAX_FRAME] };
        assert!(big.encode().is_err(), "超过 MAX_FRAME 必须报错而非静默截断");
    }

    #[test]
    fn max_read_len_fits_in_frame() {
        // MAX_READ_LEN 必须留够头部余量，否则服务端会构造出编码不了的响应
        let data = vec![0u8; MAX_READ_LEN as usize];
        let r = Response::ReadOk { data };
        assert!(
            r.encode().is_ok(),
            "按 MAX_READ_LEN 读满时响应必须能编码，否则上限设置自相矛盾"
        );
    }

    #[test]
    fn err_codes_are_stable() {
        // 这些数字在网络上流通，改动即协议不兼容
        assert_eq!(ErrCode::NoSuchHandle as u8, 1);
        assert_eq!(ErrCode::RangeOutOfBounds as u8, 2);
        assert_eq!(ErrCode::ReadTooLarge as u8, 3);
        assert_eq!(ErrCode::IoError as u8, 4);
        assert_eq!(ErrCode::BadRequest as u8, 5);
        assert_eq!(ErrCode::Revoked as u8, 6);
    }

    #[test]
    fn all_err_codes_roundtrip() {
        for c in [
            ErrCode::NoSuchHandle,
            ErrCode::RangeOutOfBounds,
            ErrCode::ReadTooLarge,
            ErrCode::IoError,
            ErrCode::BadRequest,
            ErrCode::Revoked,
        ] {
            assert_eq!(ErrCode::from_u8(c as u8), Some(c));
            assert!(!c.as_str().is_empty());
        }
    }

    /// 任何字节序列都不得让解码器 panic。
    ///
    /// 这一层直接解析来自网络的字节，一个 panic 就是可远程触发的
    /// 拒绝服务。用确定性的伪随机序列，失败时可复现。
    #[test]
    fn arbitrary_bytes_never_panic() {
        // 简单的 xorshift，不引入额外依赖且完全可复现
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for round in 0..20_000u32 {
            let len = (next() % 300) as usize;
            let mut buf = Vec::with_capacity(len);
            for _ in 0..len {
                buf.push((next() & 0xFF) as u8);
            }
            // 一半的输入强制用合法标签开头，以便更深入地走进解析逻辑；
            // 全随机的话绝大多数会在第一个字节就被拒，覆盖不到后面
            if round % 2 == 0 && !buf.is_empty() {
                let tags = [T_LIST, T_STAT, T_READ, T_PING];
                buf[0] = tags[(next() % 4) as usize];
            }
            let _ = Request::decode(&buf);

            let mut buf2 = buf.clone();
            if round % 2 == 1 && !buf2.is_empty() {
                let tags = [T_LIST_OK, T_STAT_OK, T_READ_OK, T_PONG, T_ERR];
                buf2[0] = tags[(next() % 5) as usize];
            }
            let _ = Response::decode(&buf2);
        }
    }

    /// 截断合法消息的任意位置都不得 panic。
    ///
    /// 比全随机更有针对性：这些前缀都是"看起来对了一半"的输入，
    /// 最容易触发边界错误。
    #[test]
    fn truncated_valid_messages_never_panic() {
        let msgs = vec![
            Request::Read { handle: [3; 16], offset: 999, len: 4096 }
                .encode()
                .expect("编码应成功"),
            Request::Stat { handle: [4; 16] }.encode().expect("编码应成功"),
        ];
        for m in &msgs {
            for cut in 0..m.len() {
                let _ = Request::decode(m.get(..cut).unwrap_or(&[]));
            }
        }

        let resps = vec![
            Response::ListOk {
                entries: vec![Entry {
                    handle: [1; 16],
                    size: 10,
                    header_len: 4,
                    header: vec![1, 2, 3, 4],
                }],
            }
            .encode()
            .expect("编码应成功"),
            Response::Err { code: ErrCode::IoError, msg: "abc".into() }
                .encode()
                .expect("编码应成功"),
        ];
        for m in &resps {
            for cut in 0..m.len() {
                let _ = Response::decode(m.get(..cut).unwrap_or(&[]));
            }
        }
    }

    /// 单字节翻转不得 panic（可能解码成功也可能失败，但绝不能崩）。
    #[test]
    fn bit_flips_never_panic() {
        let base = Response::ListOk {
            entries: vec![Entry {
                handle: [9; 16],
                size: 1234,
                header_len: 8,
                header: vec![0xAB; 8],
            }],
        }
        .encode()
        .expect("编码应成功");

        for i in 0..base.len() {
            for bit in 0..8u8 {
                let mut m = base.clone();
                if let Some(b) = m.get_mut(i) {
                    *b ^= 1 << bit;
                }
                let _ = Response::decode(&m);
            }
        }
    }
}
