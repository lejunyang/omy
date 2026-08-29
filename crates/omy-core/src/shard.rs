//! 分片格式：切分、合并、冗余头恢复（规范 §8）。
//!
//! # 冗余 header 的价值
//!
//! `shard_index > 0` 的分片可携带完整主 header 副本。这样即使第 0 片丢失，
//! 任意一片仍能独立识别文件身份、读出加密文件名和缩略图，UI 不至于显示成
//! 一堆无名碎片。
//!
//! 代价是每片增加 `header_len` 字节（典型 656 B 到 20 KB，取决于是否含缩略图）。

use crate::error::{Error, Result};
use crate::header::{MAGIC_SHARD, SHARD_HEADER_LEN};
use crate::util::{Cursor, Writer};

/// 分片头（56 字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardHeader {
    /// 与主文件一致，用于归属判定。
    pub file_uuid: [u8; 16],
    /// 本片序号，从 0 开始。
    pub shard_index: u32,
    /// 总片数。
    pub shard_total: u32,
    /// 本片数据在原密文中的偏移。
    pub data_offset: u64,
    /// 本片数据长度。
    pub data_len: u64,
    /// 本片数据的 CRC-32（IEEE）。
    pub crc32: u32,
    /// 冗余主 header 长度，0 表示无。
    pub redundant_header_len: u32,
}

impl ShardHeader {
    /// 解析分片头。
    ///
    /// # Errors
    ///
    /// - [`Error::Truncated`]：输入不足 56 字节
    /// - [`Error::BadMagic`]：magic 不是 `"OMYSHRD"` + 0x01
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < SHARD_HEADER_LEN {
            return Err(Error::Truncated {
                context: "shard header",
                need: SHARD_HEADER_LEN,
                got: data.len(),
            });
        }
        let mut c = Cursor::new(data, "shard header");
        let magic: [u8; 8] = c.take_array()?;
        if magic != MAGIC_SHARD {
            return Err(Error::BadMagic { got: magic });
        }
        let h = Self {
            file_uuid: c.take_array()?,
            shard_index: c.u32le()?,
            shard_total: c.u32le()?,
            data_offset: c.u64le()?,
            data_len: c.u64le()?,
            crc32: c.u32le()?,
            redundant_header_len: c.u32le()?,
        };
        debug_assert_eq!(c.position(), SHARD_HEADER_LEN, "分片头布局与规范不符");
        Ok(h)
    }

    /// 序列化为 56 字节。
    ///
    /// # Errors
    ///
    /// 序列化结果长度异常时返回 [`Error::MalformedHeader`]。
    pub fn to_bytes(&self) -> Result<[u8; SHARD_HEADER_LEN]> {
        let mut w = Writer::with_capacity(SHARD_HEADER_LEN);
        w.bytes(&MAGIC_SHARD)
            .bytes(&self.file_uuid)
            .u32le(self.shard_index)
            .u32le(self.shard_total)
            .u64le(self.data_offset)
            .u64le(self.data_len)
            .u32le(self.crc32)
            .u32le(self.redundant_header_len);

        let v = w.into_vec();
        let mut out = [0u8; SHARD_HEADER_LEN];
        let src = v.get(..SHARD_HEADER_LEN).ok_or(Error::MalformedHeader {
            reason: "serialized shard header shorter than 56 bytes",
        })?;
        out.copy_from_slice(src);
        Ok(out)
    }

    /// 本片数据段在分片文件内的起始偏移。
    #[must_use]
    pub const fn data_start(&self) -> usize {
        SHARD_HEADER_LEN.saturating_add(self.redundant_header_len as usize)
    }
}

/// 计算 CRC-32（IEEE）。
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

/// 把完整密文切分为多个分片。
///
/// `redundant_header` 若提供，会附加到 `shard_index > 0` 的每一片中
/// （第 0 片本身就含主 header，无需冗余）。
///
/// # Errors
///
/// [`Error::MalformedHeader`]：`shard_size` 为 0，或片数超过 `u32` 上限。
pub fn split(
    full_ciphertext: &[u8],
    file_uuid: &[u8; 16],
    shard_size: usize,
    redundant_header: Option<&[u8]>,
) -> Result<Vec<Vec<u8>>> {
    if shard_size == 0 {
        return Err(Error::MalformedHeader { reason: "shard_size must not be zero" });
    }
    let total = full_ciphertext.len().div_ceil(shard_size).max(1);
    let total_u32 = u32::try_from(total)
        .map_err(|_| Error::MalformedHeader { reason: "shard count exceeds u32" })?;

    let mut shards = Vec::with_capacity(total);
    for i in 0..total {
        let start = i.saturating_mul(shard_size);
        let end = start.saturating_add(shard_size).min(full_ciphertext.len());
        let data = full_ciphertext.get(start..end).unwrap_or(&[]);

        let i_u32 = u32::try_from(i)
            .map_err(|_| Error::MalformedHeader { reason: "shard index exceeds u32" })?;
        // 第 0 片已含主 header，不再附冗余副本
        let redundant = if i_u32 == 0 { None } else { redundant_header };
        let rlen = redundant.map_or(0, <[u8]>::len);

        let sh = ShardHeader {
            file_uuid: *file_uuid,
            shard_index: i_u32,
            shard_total: total_u32,
            data_offset: start as u64,
            data_len: data.len() as u64,
            crc32: crc32(data),
            redundant_header_len: u32::try_from(rlen).map_err(|_| Error::MalformedHeader {
                reason: "redundant header exceeds u32",
            })?,
        };

        let mut w = Writer::with_capacity(SHARD_HEADER_LEN.saturating_add(rlen).saturating_add(data.len()));
        w.bytes(&sh.to_bytes()?);
        if let Some(r) = redundant {
            w.bytes(r);
        }
        w.bytes(data);
        shards.push(w.into_vec());
    }
    Ok(shards)
}

/// 合并分片，还原完整密文。
///
/// 按规范 §8.3 的顺序校验：magic → uuid 一致性 → CRC → 排序 → 缺片检查 → 拼接。
/// 输入可以是任意顺序。
///
/// # Errors
///
/// - [`Error::BadMagic`]：某片不是分片文件
/// - [`Error::ShardUuidMismatch`]：某片属于其它文件
/// - [`Error::ShardCrcMismatch`]：某片数据损坏
/// - [`Error::MissingShards`]：缺少某些片，错误中列出具体序号
pub fn merge(shards: &[Vec<u8>]) -> Result<Vec<u8>> {
    if shards.is_empty() {
        return Err(Error::MissingShards { missing: Vec::new(), total: 0 });
    }

    let mut parsed: Vec<(ShardHeader, &[u8])> = Vec::with_capacity(shards.len());
    let mut expect_uuid: Option<[u8; 16]> = None;
    let mut expect_total: Option<u32> = None;

    for raw in shards {
        let h = ShardHeader::parse(raw)?;

        match expect_uuid {
            None => expect_uuid = Some(h.file_uuid),
            Some(u) if u != h.file_uuid => {
                return Err(Error::ShardUuidMismatch { index: h.shard_index });
            }
            Some(_) => {}
        }
        if expect_total.is_none() {
            expect_total = Some(h.shard_total);
        }

        let start = h.data_start();
        let len = usize::try_from(h.data_len).map_err(|_| Error::MalformedHeader {
            reason: "shard data_len exceeds addressable range",
        })?;
        let end = start.saturating_add(len);
        let data = raw.get(start..end).ok_or(Error::Truncated {
            context: "shard data",
            need: end,
            got: raw.len(),
        })?;

        let computed = crc32(data);
        if computed != h.crc32 {
            return Err(Error::ShardCrcMismatch {
                index: h.shard_index,
                expected: h.crc32,
                computed,
            });
        }
        parsed.push((h, data));
    }

    let total = expect_total.unwrap_or(0);
    parsed.sort_by_key(|(h, _)| h.shard_index);

    // 检查 0..total 无缺失
    let present: Vec<u32> = parsed.iter().map(|(h, _)| h.shard_index).collect();
    let missing: Vec<u32> = (0..total).filter(|i| !present.contains(i)).collect();
    if !missing.is_empty() {
        return Err(Error::MissingShards { missing, total });
    }

    let cap = parsed.iter().map(|(_, d)| d.len()).sum();
    let mut out = Vec::with_capacity(cap);
    for (_, d) in &parsed {
        out.extend_from_slice(d);
    }
    Ok(out)
}

/// 从分片中恢复冗余主 header 副本。
///
/// 用于第 0 片丢失时仍能读出文件名与缩略图。
///
/// # Errors
///
/// - [`Error::BadMagic`]：不是分片文件
/// - [`Error::MalformedHeader`]：该片不含冗余 header
/// - [`Error::Truncated`]：数据不完整
pub fn recover_header_from_shard(shard: &[u8]) -> Result<Vec<u8>> {
    let h = ShardHeader::parse(shard)?;
    if h.redundant_header_len == 0 {
        return Err(Error::MalformedHeader {
            reason: "this shard carries no redundant header",
        });
    }
    let len = usize::try_from(h.redundant_header_len).map_err(|_| Error::MalformedHeader {
        reason: "redundant_header_len exceeds addressable range",
    })?;
    let end = SHARD_HEADER_LEN.saturating_add(len);
    let hdr = shard.get(SHARD_HEADER_LEN..end).ok_or(Error::Truncated {
        context: "redundant header",
        need: end,
        got: shard.len(),
    })?;
    Ok(hdr.to_vec())
}

/// 分片集合的缺片情况，用于降级播放（规范 §8.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardCoverage {
    /// 总片数。
    pub total: u32,
    /// 缺失的片序号。
    pub missing: Vec<u32>,
    /// 缺失区间在原密文中的 `(偏移, 长度)`，落在这些区间的读取必须失败。
    pub holes: Vec<(u64, u64)>,
}

impl ShardCoverage {
    /// 某个密文区间是否完整可读。
    #[must_use]
    pub fn is_readable(&self, offset: u64, length: u64) -> bool {
        let end = offset.saturating_add(length);
        !self.holes.iter().any(|&(ho, hl)| {
            let hend = ho.saturating_add(hl);
            offset < hend && ho < end
        })
    }
}

/// 分析分片集合的缺片情况。
///
/// # Errors
///
/// 解析分片头失败时返回错误。
pub fn analyze_coverage(shards: &[Vec<u8>]) -> Result<ShardCoverage> {
    let mut heads = Vec::with_capacity(shards.len());
    for raw in shards {
        heads.push(ShardHeader::parse(raw)?);
    }
    let total = heads.first().map_or(0, |h| h.shard_total);
    let present: Vec<u32> = heads.iter().map(|h| h.shard_index).collect();
    let missing: Vec<u32> = (0..total).filter(|i| !present.contains(i)).collect();

    // 用已知片推断标准片大小：非末片的 data_len 即片大小
    let shard_size = heads
        .iter()
        .filter(|h| h.shard_index.saturating_add(1) < h.shard_total)
        .map(|h| h.data_len)
        .max()
        .or_else(|| heads.first().map(|h| h.data_len))
        .unwrap_or(0);

    let holes = missing
        .iter()
        .map(|&i| (u64::from(i).saturating_mul(shard_size), shard_size))
        .collect();

    Ok(ShardCoverage { total, missing, holes })
}
