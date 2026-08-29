//! [`BlockSource`]：密文块的统一来源抽象。
//!
//! 对应设计文档 [11 号 §1.2](../../docs/research/11-engineering-roadmap.md)。
//! 本地文件、分片集合、容器条目、局域网远程设备都实现同一个 trait，
//! 使播放器、缩略图、扫描逻辑只写一遍。
//!
//! 这个抽象直接决定决策 D-04（局域网只传密文）能否干净落地——
//! 远程实现返回的就是密文，解密统一在上层完成，服务端永不接触明文。
//!
//! # 与设计文档的一处偏离
//!
//! 文档给出的草案是 `#[async_trait]`。此处实现为**同步** trait，理由：
//!
//! 1. [`payload::read_range`](crate::payload::read_range) 是纯 CPU 的解密逻辑。
//!    若 `BlockSource` 为异步，会迫使它连带异步化，进而污染整条调用链。
//! 2. `omy-core` 是 MIT OR Apache-2.0 且要保持轻量，不应引入 async 运行时依赖。
//! 3. CLI 是同步程序，异步会凭空增加复杂度。
//!
//! 远程实现（`omy-net`）在自己内部桥接异步即可——它本就需要运行时，
//! 由它承担这个成本比让 core 承担更合理。GUI 侧在后台线程调用，不阻塞 UI。

use crate::error::{Error, Result};
use crate::file::OpenedFile;
use crate::header::FixedHeader;
use crate::payload::read_range;
use crate::tlv::CompressionIndexEntry;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// 密文块来源。
///
/// 实现者只需提供「按载荷内偏移取密文」的能力，随机访问、跨块拼接、
/// 解压都由上层统一处理。
pub trait BlockSource {
    /// 文件头。实现者应缓存，避免每次重新解析。
    fn header(&self) -> &FixedHeader;

    /// 读取载荷区中 `[offset, offset + len)` 的**密文**。
    ///
    /// `offset` 相对载荷区起点（即 `header_len` 之后），不是文件绝对偏移。
    ///
    /// # Errors
    ///
    /// 越界、I/O 失败或远程不可达时返回错误。
    fn read_ct(&self, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// 载荷区密文总长。
    fn ct_len(&self) -> u64;

    /// 是否为远程来源。UI 据此显示网络状态、调整预读策略。
    fn is_remote(&self) -> bool {
        false
    }

    /// 来源的可读描述，用于错误信息与日志。
    fn describe(&self) -> String {
        String::from("<block source>")
    }
}

/// 内存中的完整文件。测试与小文件场景使用。
#[derive(Debug)]
pub struct MemorySource {
    header: FixedHeader,
    /// 仅载荷部分，不含头部。
    payload: Vec<u8>,
}

impl MemorySource {
    /// 从完整文件字节构造。
    ///
    /// # Errors
    ///
    /// 头部解析失败或数据短于头部声明长度时返回错误。
    pub fn new(file_bytes: &[u8]) -> Result<Self> {
        let header = crate::file::peek_header(file_bytes)?;
        let start = usize::try_from(header.header_len)
            .map_err(|_| Error::MalformedHeader { reason: "header_len exceeds usize" })?;
        let payload = file_bytes
            .get(start..)
            .ok_or(Error::Truncated {
                context: "payload area",
                need: start,
                got: file_bytes.len(),
            })?
            .to_vec();
        Ok(Self { header, payload })
    }
}

impl BlockSource for MemorySource {
    fn header(&self) -> &FixedHeader {
        &self.header
    }

    fn read_ct(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let start = usize::try_from(offset)
            .map_err(|_| Error::ChunkOutOfRange { index: offset, total: self.ct_len() })?;
        let n = usize::try_from(len)
            .map_err(|_| Error::ChunkOutOfRange { index: len, total: self.ct_len() })?;
        let end = start
            .checked_add(n)
            .ok_or(Error::ChunkOutOfRange { index: offset, total: self.ct_len() })?;
        self.payload
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(Error::Truncated {
                context: "memory ciphertext range",
                need: end,
                got: self.payload.len(),
            })
    }

    fn ct_len(&self) -> u64 {
        self.payload.len() as u64
    }

    fn describe(&self) -> String {
        String::from("memory")
    }
}

/// 本地文件，按需 seek 读取，不整体载入内存。
///
/// 这是播放大视频的默认来源：内存占用与文件大小无关。
#[derive(Debug)]
pub struct LocalFileSource {
    path: PathBuf,
    header: FixedHeader,
    payload_start: u64,
    payload_len: u64,
}

impl LocalFileSource {
    /// 打开本地 `.omy` 文件。
    ///
    /// 只读取头部，载荷按需读取。
    ///
    /// # Errors
    ///
    /// 文件不存在、无法读取或头部无效时返回错误。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut f = std::fs::File::open(&path)?;

        // 先读足够覆盖头部的前缀。头部含 slot 区与 TLV 区，
        // 长度由 header_len 声明，故先读固定部分再按需扩展。
        let mut probe = vec![0u8; crate::scan::MIN_PROBE_SIZE];
        let n = read_at_most(&mut f, &mut probe)?;
        probe.truncate(n);
        let partial = crate::file::peek_header(&probe)?;

        let hlen = partial.header_len;
        let total = f.metadata()?.len();
        if total < u64::from(hlen) {
            return Err(Error::Truncated {
                context: "local file header",
                need: hlen as usize,
                got: usize::try_from(total).unwrap_or(usize::MAX),
            });
        }

        // 重新读完整头部以完成解析（含头部 MAC 覆盖范围）
        let mut head = vec![0u8; hlen as usize];
        f.seek(SeekFrom::Start(0))?;
        f.read_exact(&mut head)?;
        let header = crate::file::peek_header(&head)?;

        Ok(Self {
            path,
            payload_start: u64::from(hlen),
            payload_len: total.saturating_sub(u64::from(hlen)),
            header,
        })
    }

    /// 文件路径。
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BlockSource for LocalFileSource {
    fn header(&self) -> &FixedHeader {
        &self.header
    }

    fn read_ct(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let end = offset
            .checked_add(len)
            .ok_or(Error::ChunkOutOfRange { index: offset, total: self.payload_len })?;
        if end > self.payload_len {
            return Err(Error::Truncated {
                context: "local file ciphertext range",
                need: usize::try_from(end).unwrap_or(usize::MAX),
                got: usize::try_from(self.payload_len).unwrap_or(usize::MAX),
            });
        }
        let n = usize::try_from(len)
            .map_err(|_| Error::ChunkOutOfRange { index: len, total: self.payload_len })?;

        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(
            self.payload_start
                .checked_add(offset)
                .ok_or(Error::ChunkOutOfRange { index: offset, total: self.payload_len })?,
        ))?;
        let mut buf = vec![0u8; n];
        f.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn ct_len(&self) -> u64 {
        self.payload_len
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// 尽力读取，返回实际字节数。文件短于缓冲区时不报错。
fn read_at_most(f: &mut std::fs::File, buf: &mut [u8]) -> Result<usize> {
    let mut total = 0usize;
    while total < buf.len() {
        let Some(slice) = buf.get_mut(total..) else {
            break;
        };
        match f.read(slice) {
            Ok(0) => break,
            Ok(n) => total = total.saturating_add(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(total)
}

/// 容器内单个条目的视图。
///
/// 把容器内某文件的 `[offset, size)` 映射为独立的明文流，
/// 使容器内文件与单独加密的文件对上层完全等价——
/// 这是「容器内单文件预览无需解开整个容器」的实现基础（文档 05 §2.3）。
#[derive(Debug)]
pub struct ContainerEntryView<'a, S: BlockSource> {
    inner: &'a S,
    base: u64,
    len: u64,
}

impl<'a, S: BlockSource> ContainerEntryView<'a, S> {
    /// 用条目在明文载荷流中的区间构造视图。
    #[must_use]
    pub const fn new(inner: &'a S, offset: u64, size: u64) -> Self {
        Self { inner, base: offset, len: size }
    }

    /// 该条目的明文长度。
    #[must_use]
    pub const fn plaintext_len(&self) -> u64 {
        self.len
    }

    /// 读取该条目内的明文区间。
    ///
    /// `offset` 相对条目自身起点。超出条目范围的部分被裁剪，
    /// 不会读到相邻文件的数据。
    ///
    /// # Errors
    ///
    /// 解密失败或来源读取失败时返回错误。
    pub fn read(
        &self,
        opened: &OpenedFile,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>> {
        if offset >= self.len {
            return Ok(Vec::new());
        }
        let avail = self.len.saturating_sub(offset);
        let want = length.min(avail);
        let abs = self
            .base
            .checked_add(offset)
            .ok_or(Error::MalformedHeader { reason: "entry offset overflow" })?;
        read_source_range(self.inner, opened, abs, want)
    }
}

/// 通过 [`BlockSource`] 读取明文区间。
///
/// 这是把 `BlockSource` 接到 [`payload::read_range`](crate::payload::read_range)
/// 的桥梁：来源只管给密文，解密与跨块拼接在此统一完成。
///
/// # Errors
///
/// 来源读取失败、区间越界或块认证失败时返回错误。
pub fn read_source_range<S: BlockSource + ?Sized>(
    src: &S,
    opened: &OpenedFile,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>> {
    let index: Option<Vec<CompressionIndexEntry>> =
        if src.header().has_flag(crate::header::flags::COMPRESSED) {
            Some(opened.compression_index()?)
        } else {
            None
        };

    read_range(
        src.header(),
        opened.payload_key(),
        index.as_deref(),
        offset,
        length,
        |ct_off, ct_len| src.read_ct(ct_off, ct_len),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Argon2Params, Kek};
    use crate::file::{EncryptOptions, RandomMaterial, encrypt, open};
    use crate::header::MIN_CHUNK_SIZE;

    fn make(payload: &[u8], compress: bool) -> (Vec<u8>, Kek) {
        let salt = [0x22u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"src", &salt, params).unwrap();
        let opts = EncryptOptions {
            filename: Some("s.bin".into()),
            chunk_size: MIN_CHUNK_SIZE,
            argon2: params,
            compress,
            ..EncryptOptions::default()
        };
        let k2 = Kek::from_password(b"src", &salt, params).unwrap();
        let enc = encrypt(payload, &[kek], &salt, &opts, &RandomMaterial::generate()).unwrap();
        (enc.bytes, k2)
    }

    #[test]
    fn memory_source_reads_ranges() {
        let plain: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let (bytes, kek) = make(&plain, false);
        let src = MemorySource::new(&bytes).unwrap();
        let opened = open(&bytes, &[kek]).unwrap();

        for (off, len) in [(0u64, 10u64), (65_530, 20), (199_990, 10), (0, 200_000)] {
            let got = read_source_range(&src, &opened, off, len).unwrap();
            let want = &plain[off as usize..(off + len) as usize];
            assert_eq!(got, want, "区间 ({off},{len}) 不符");
        }
        assert!(!src.is_remote());
    }

    #[test]
    fn local_file_source_matches_memory() {
        let plain: Vec<u8> = (0..150_000u32).map(|i| (i % 253) as u8).collect();
        let (bytes, kek) = make(&plain, false);

        let dir = std::env::temp_dir().join(format!("omy_src_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.omy");
        std::fs::write(&p, &bytes).unwrap();

        let src = LocalFileSource::open(&p).unwrap();
        let opened = open(&bytes, &[kek]).unwrap();

        assert_eq!(src.ct_len(), (bytes.len() - src.header().header_len as usize) as u64);

        for (off, len) in [(0u64, 100u64), (65_536, 1000), (149_000, 1000)] {
            let got = read_source_range(&src, &opened, off, len).unwrap();
            assert_eq!(got, &plain[off as usize..(off + len) as usize]);
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn compressed_source_reads_ranges() {
        // 高重复内容，确保压缩确实生效
        let plain = vec![0x5Au8; 300_000];
        let (bytes, kek) = make(&plain, true);
        let src = MemorySource::new(&bytes).unwrap();
        let opened = open(&bytes, &[kek]).unwrap();
        assert!(src.header().has_flag(crate::header::flags::COMPRESSED));

        let got = read_source_range(&src, &opened, 100, 5000).unwrap();
        assert_eq!(got, &plain[100..5100]);
    }

    #[test]
    fn container_view_clips_to_entry() {
        // 载荷模拟两个拼接的文件：前 1000 个 0xAA，后 1000 个 0xBB
        let mut plain = vec![0xAAu8; 1000];
        plain.extend_from_slice(&[0xBBu8; 1000]);
        let (bytes, kek) = make(&plain, false);
        let src = MemorySource::new(&bytes).unwrap();
        let opened = open(&bytes, &[kek]).unwrap();

        let second = ContainerEntryView::new(&src, 1000, 1000);
        assert_eq!(second.plaintext_len(), 1000);

        // 读第二个条目的全部，必须全是 0xBB——不能串到前一个文件
        let got = second.read(&opened, 0, 1000).unwrap();
        assert_eq!(got.len(), 1000);
        assert!(got.iter().all(|&b| b == 0xBB));

        // 请求超过条目长度，必须被裁剪而非读到界外
        let got2 = second.read(&opened, 900, 500).unwrap();
        assert_eq!(got2.len(), 100);
        assert!(got2.iter().all(|&b| b == 0xBB));

        // 起点越界返回空
        assert!(second.read(&opened, 1000, 10).unwrap().is_empty());
    }

    #[test]
    fn out_of_bounds_ct_read_errors() {
        let (bytes, _) = make(b"short", false);
        let src = MemorySource::new(&bytes).unwrap();
        assert!(src.read_ct(0, src.ct_len() + 1).is_err());
        assert!(src.read_ct(u64::MAX, 1).is_err());
    }

    #[test]
    fn describe_is_useful() {
        let (bytes, _) = make(b"x", false);
        let src = MemorySource::new(&bytes).unwrap();
        assert_eq!(src.describe(), "memory");
    }
}
