//! 内部工具：随机数、小端序读写、常量时间比较。

use crate::error::{Error, Result};

/// 用操作系统 CSPRNG 填充缓冲区。
///
/// # Panics
///
/// 系统熵源不可用时 panic。这种情况下继续运行会产生可预测的密钥或 nonce，
/// 属于灾难性安全故障，快速失败比降级更安全。
pub fn fill_random(buf: &mut [u8]) {
    use rand_core::{OsRng, RngCore};
    OsRng.fill_bytes(buf);
}

/// 生成随机 16 字节，用作 `file_uuid` 或 `vault_salt`。
#[must_use]
pub fn random_16() -> [u8; 16] {
    let mut b = [0u8; 16];
    fill_random(&mut b);
    b
}

/// 顺序读取小端序整数的游标。
///
/// 每个方法都做边界检查并返回结构化错误，避免 panic —— 解析不可信输入时这点很重要。
pub struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
    /// 当前解析的结构名，用于错误信息定位。
    context: &'static str,
}

impl<'a> Cursor<'a> {
    /// 新建游标。
    #[must_use]
    pub const fn new(data: &'a [u8], context: &'static str) -> Self {
        Self { data, pos: 0, context }
    }

    /// 当前偏移量。
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// 剩余字节数。
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    /// 跳过 `n` 字节。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }

    /// 取出 `n` 字节切片。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated {
            context: self.context,
            need: n,
            got: self.remaining(),
        })?;
        let s = self.data.get(self.pos..end).ok_or(Error::Truncated {
            context: self.context,
            need: n,
            got: self.remaining(),
        })?;
        self.pos = end;
        Ok(s)
    }

    /// 取出固定长度数组。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn take_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let s = self.take(N)?;
        let mut a = [0u8; N];
        a.copy_from_slice(s);
        Ok(a)
    }

    /// 读 `u8`。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take_array::<1>()?[0])
    }

    /// 读小端序 `u16`。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn u16le(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take_array::<2>()?))
    }

    /// 读小端序 `u32`。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn u32le(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take_array::<4>()?))
    }

    /// 读小端序 `u64`。
    ///
    /// # Errors
    ///
    /// 剩余不足时返回 [`Error::Truncated`]。
    pub fn u64le(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take_array::<8>()?))
    }
}

/// 小端序写入缓冲区。
#[derive(Debug, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// 新建。
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 预分配容量。
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n) }
    }

    /// 当前长度。
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 追加原始字节。
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }

    /// 追加 `u8`。
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// 追加小端序 `u16`。
    pub fn u16le(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// 追加小端序 `u32`。
    pub fn u32le(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// 追加小端序 `u64`。
    pub fn u64le(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// 追加 `n` 个零字节。
    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len().saturating_add(n), 0);
        self
    }

    /// 追加 `n` 个随机字节。
    ///
    /// 用于填充未使用的 key slot 与文件名 padding —— 必须是随机而非零，
    /// 否则「哪些 slot 在用」和「文件名多长」会直接暴露。
    pub fn random(&mut self, n: usize) -> &mut Self {
        let start = self.buf.len();
        self.buf.resize(start.saturating_add(n), 0);
        if let Some(tail) = self.buf.get_mut(start..) {
            fill_random(tail);
        }
        self
    }

    /// 零填充到指定总长度。
    ///
    /// # Errors
    ///
    /// 当前长度已超过目标时返回 [`Error::MalformedHeader`]。
    pub fn pad_to(&mut self, total: usize) -> Result<&mut Self> {
        if self.buf.len() > total {
            return Err(Error::MalformedHeader { reason: "field overflow while padding" });
        }
        self.buf.resize(total, 0);
        Ok(self)
    }

    /// 取出内部缓冲区。
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }

    /// 借出内部缓冲区。
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.buf
    }
}
