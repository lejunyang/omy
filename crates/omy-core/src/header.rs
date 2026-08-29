//! 格式常量与 Fixed Header 编解码。
//!
//! 全部定义来自 `docs/research/02-file-format-spec.md` §2。
//! **所有多字节整数为小端序**，唯一例外是载荷 nonce/AAD 中的块序号（大端序，见
//! [`crate::crypto::chunk_nonce`]）。

use crate::crypto::CipherId;
use crate::error::{Error, Result};
use crate::util::{Cursor, Writer};

/// 主文件 magic：`"OMYFILE"` + 世代号 `0x01`。
pub const MAGIC_FILE: [u8; 8] = *b"OMYFILE\x01";
/// 分片文件 magic：`"OMYSHRD"` + 世代号 `0x01`。
pub const MAGIC_SHARD: [u8; 8] = *b"OMYSHRD\x01";

/// 本实现支持的格式主版本上限。文件声明更高主版本时必须拒绝打开。
pub const VERSION_MAJOR: u16 = 1;
/// 本实现写出的次版本号。
pub const VERSION_MINOR: u16 = 0;

/// Fixed Header 长度。
pub const FIXED_HEADER_LEN: usize = 96;
/// 单个 key slot 长度（32 字节密文 + 16 字节 tag）。
pub const SLOT_LEN: usize = 48;
/// key slot 数量，固定为 8（可否认性依赖固定槽数）。
pub const SLOT_COUNT: usize = 8;
/// [`SLOT_COUNT`] 的 `u8` 形式，用于写入 header 的 `slot_count` 字段。
pub const SLOT_COUNT_U8: u8 = 8;

// 两者必须一致，否则写出的 header 与实际槽数不符
const _: () = assert!(SLOT_COUNT == SLOT_COUNT_U8 as usize, "SLOT_COUNT 与其 u8 形式不一致");
/// Key Slot Area 总长度。
pub const SLOT_AREA_LEN: usize = SLOT_LEN * SLOT_COUNT;
/// Key Slot Area 起始偏移。
pub const SLOT_AREA_OFFSET: usize = FIXED_HEADER_LEN;
/// TLV 区起始偏移。
pub const TLV_AREA_OFFSET: usize = SLOT_AREA_OFFSET + SLOT_AREA_LEN;
/// Header MAC 长度。
pub const HEADER_MAC_LEN: usize = 32;

/// 分片头长度。
pub const SHARD_HEADER_LEN: usize = 56;

/// 允许的最小分块大小（64 KiB）。
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
/// 允许的最大分块大小（16 MiB）。
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;
/// 默认分块大小（256 KiB），兼顾 seek 粒度与吞吐。
pub const DEFAULT_CHUNK_SIZE: u32 = 256 * 1024;
/// 移动端建议的分块大小上限（1 MiB）。
pub const MOBILE_MAX_CHUNK_SIZE: u32 = 1024 * 1024;

/// 文件名 padding 的桶大小。
pub const FILENAME_BUCKET: usize = 64;

/// 固定头中 `header_len` 字段的字节偏移。
///
/// 布局：`magic(8) + version_major(2) + version_minor(2)` = 12。
///
/// 扫描器需要在**不做完整解析**的前提下先取出 `header_len`，以便决定还要再读
/// 多少字节（完整解析需要 slot 区之后的数据，此时还没读到）。因此这个偏移必须
/// 作为公开常量暴露，而不是散落的魔数。
pub const HEADER_LEN_FIELD_OFFSET: usize = 12;

// 若固定头布局调整而此常量未同步，扫描器会读到错误的长度且不报错——用静态断言锁住
const _: () = assert!(
    HEADER_LEN_FIELD_OFFSET == MAGIC_FILE.len() + 2 + 2,
    "HEADER_LEN_FIELD_OFFSET 必须等于 magic + version_major + version_minor 的长度"
);

/// 解析不可信输入时的健壮性上界（见 `docs/research/07-platform-and-sidechannels.md` §8.3）。
///
/// # 为什么这些上界必须存在
///
/// 头部里的长度字段来自**不可信的文件内容**。若不设上界，一个 12 字节的恶意头部
/// 声明 `tlv_len = 0xFFFFFFFF` 就能让实现尝试分配 4 GiB 内存——无需任何有效密钥，
/// 纯粹的拒绝服务。
///
/// # 这一组与 [`MIN_CHUNK_SIZE`] 那一组的区别
///
/// 本模块存在**两套**边界，用途完全不同，不可混用：
///
/// | 用途 | 常量 | 作用位置 |
/// |---|---|---|
/// | 解析上界（防 DoS） | `parse_limits::*` | 读取任何文件时，**必须**校验 |
/// | 新建策略范围 | [`MIN_CHUNK_SIZE`]/[`MAX_CHUNK_SIZE`] | 仅创建文件时校验 |
///
/// 解析上界比策略范围宽松得多，这是刻意的：格式允许的取值范围应大于本实现推荐用户
/// 选择的范围，否则会拒绝其它实现产出的合法文件（测试向量 v3/v4 即用 4096/2048
/// 字节的块）。
pub mod parse_limits {
    /// `header_len` 上限：64 MiB。含缩略图的头部也远小于此值。
    pub const MAX_HEADER_LEN: u32 = 64 * 1024 * 1024;
    /// `tlv_len` 上限：32 MiB。
    pub const MAX_TLV_LEN: u32 = 32 * 1024 * 1024;
    /// TLV 条目数上限，防止大量零长度条目耗尽内存。
    pub const MAX_TLV_ENTRIES: usize = 4096;
    /// 压缩索引条目数上限。
    pub const MAX_INDEX_ENTRIES: u32 = 16 * 1024 * 1024;
    /// 解析时接受的最大分块大小：64 MiB。
    ///
    /// 这是**唯一**需要卡住的分块边界：过大的 `chunk_size` 会让单块解密
    /// 一次性分配巨额内存。
    pub const MAX_PARSE_CHUNK_SIZE: u32 = 64 * 1024 * 1024;
    /// 容器模式目录条目数上限。
    pub const MAX_FOLDER_ENTRIES: usize = 10_000_000;
}

/// KDF 标识：Argon2id。
pub const KDF_ARGON2ID: u8 = 1;
/// 压缩标识：不压缩。
pub const COMPRESS_NONE: u8 = 0;
/// 压缩标识：zstd。
pub const COMPRESS_ZSTD: u8 = 1;

/// `flags` 位定义（规范 §2.1）。
pub mod flags {
    /// TLV 中存有加密的原文件名。
    pub const FILENAME_ENCRYPTED: u32 = 1 << 0;
    /// 后缀以明文单独存放。
    pub const EXT_PRESERVED: u32 = 1 << 1;
    /// 载荷分块经 zstd 压缩，必须存在压缩索引表。
    pub const COMPRESSED: u32 = 1 << 2;
    /// 本文件是分片集合的一部分。
    pub const SHARDED: u32 = 1 << 3;
    /// TLV 中存有加密缩略图。
    pub const HAS_THUMBNAIL: u32 = 1 << 4;
    /// 容器模式（整个文件夹打包）。
    pub const CONTAINER: u32 = 1 << 5;
    /// 伪装模式，真实 header 不在文件起始处。
    pub const DISGUISED: u32 = 1 << 6;
    /// 载荷非原始字节（转码过），无法 bit-for-bit 还原。
    pub const TRANSCODED: u32 = 1 << 7;
    /// 时间戳等元数据已被抹平。
    pub const META_NORMALIZED: u32 = 1 << 8;
    /// 载荷尾部有随机填充以隐藏真实大小。
    pub const SIZE_PADDED: u32 = 1 << 9;

    /// 本实现已知的全部位。其余位保留，写入时必须为 0。
    pub const KNOWN_MASK: u32 = FILENAME_ENCRYPTED
        | EXT_PRESERVED
        | COMPRESSED
        | SHARDED
        | HAS_THUMBNAIL
        | CONTAINER
        | DISGUISED
        | TRANSCODED
        | META_NORMALIZED
        | SIZE_PADDED;
}

/// Fixed Header（96 字节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedHeader {
    /// 格式主版本。
    pub version_major: u16,
    /// 格式次版本。
    pub version_minor: u16,
    /// 从文件起始到载荷起始的总字节数。
    pub header_len: u32,
    /// 文件唯一标识，用于 nonce 派生、AAD 绑定、分片归属。
    pub file_uuid: [u8; 16],
    /// Argon2id 盐，同一 vault 内所有文件相同。
    pub vault_salt: [u8; 16],
    /// 位标志，见 [`flags`]。
    pub flags: u32,
    /// AEAD 算法。
    pub cipher_id: CipherId,
    /// KDF 算法，当前仅 Argon2id。
    pub kdf_id: u8,
    /// 压缩算法。
    pub compress_id: u8,
    /// key slot 数量，固定为 8。
    pub slot_count: u8,
    /// Argon2 内存成本（KiB）。
    pub argon2_m_kib: u32,
    /// Argon2 迭代次数。
    pub argon2_t: u32,
    /// Argon2 并行度。
    pub argon2_p: u32,
    /// 明文分块大小。
    pub chunk_size: u32,
    /// 原始明文总长度。
    pub plaintext_size: u64,
    /// 随机 nonce 前缀，与块序号组合成完整 nonce。
    pub base_nonce: [u8; 7],
    /// 保留给未来的就地编辑功能，当前必须为 0。
    pub chunk_version: u8,
    /// TLV 区总字节数。
    pub tlv_len: u32,
}

impl FixedHeader {
    /// 解析 Fixed Header。
    ///
    /// 按规范 §9 步骤 1–3 的顺序校验：先 magic，再版本，最后字段一致性。
    /// 版本检查必须在字段解析**之前**完成——否则可能按错误的布局解读未来版本的头部。
    ///
    /// # Errors
    ///
    /// - [`Error::Truncated`]：输入不足 96 字节
    /// - [`Error::BadMagic`]：magic 不匹配
    /// - [`Error::UnsupportedVersion`]：主版本高于本实现
    /// - [`Error::MalformedHeader`]：字段取值非法
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < FIXED_HEADER_LEN {
            return Err(Error::Truncated {
                context: "fixed header",
                need: FIXED_HEADER_LEN,
                got: data.len(),
            });
        }
        let mut c = Cursor::new(data, "fixed header");

        let magic: [u8; 8] = c.take_array()?;
        if magic != MAGIC_FILE {
            return Err(Error::BadMagic { got: magic });
        }

        let version_major = c.u16le()?;
        let version_minor = c.u16le()?;
        if version_major > VERSION_MAJOR {
            return Err(Error::UnsupportedVersion {
                found_major: version_major,
                found_minor: version_minor,
                supported_major: VERSION_MAJOR,
            });
        }

        let header_len = c.u32le()?;
        let file_uuid: [u8; 16] = c.take_array()?;
        let vault_salt: [u8; 16] = c.take_array()?;
        let flags = c.u32le()?;
        let cipher_id = CipherId::from_u8(c.u8()?)?;
        let kdf_id = c.u8()?;
        let compress_id = c.u8()?;
        let slot_count = c.u8()?;
        let argon2_m_kib = c.u32le()?;
        let argon2_t = c.u32le()?;
        let argon2_p = c.u32le()?;
        let chunk_size = c.u32le()?;
        let plaintext_size = c.u64le()?;
        let base_nonce: [u8; 7] = c.take_array()?;
        let chunk_version = c.u8()?;
        let tlv_len = c.u32le()?;
        let _reserved = c.u32le()?; // 规范要求读取时忽略

        debug_assert_eq!(c.position(), FIXED_HEADER_LEN, "fixed header 布局与规范不符");

        let h = Self {
            version_major,
            version_minor,
            header_len,
            file_uuid,
            vault_salt,
            flags,
            cipher_id,
            kdf_id,
            compress_id,
            slot_count,
            argon2_m_kib,
            argon2_t,
            argon2_p,
            chunk_size,
            plaintext_size,
            base_nonce,
            chunk_version,
            tlv_len,
        };
        h.validate()?;
        Ok(h)
    }

    /// 校验字段自洽性与解析上界。
    ///
    /// 上界校验在**分配任何内存之前**完成，这样恶意头部无法通过声明巨大长度
    /// 触发大额分配（见 `docs/research/07-platform-and-sidechannels.md` §8.3）。
    ///
    /// # Errors
    ///
    /// 任一字段非法、超出解析上界，或字段间矛盾时返回 [`Error::MalformedHeader`]。
    pub fn validate(&self) -> Result<()> {
        if self.slot_count as usize != SLOT_COUNT {
            return Err(Error::MalformedHeader { reason: "slot_count must be 8" });
        }
        if self.kdf_id != KDF_ARGON2ID {
            return Err(Error::MalformedHeader { reason: "unknown kdf_id" });
        }
        if self.compress_id != COMPRESS_NONE && self.compress_id != COMPRESS_ZSTD {
            return Err(Error::MalformedHeader { reason: "unknown compress_id" });
        }
        if self.chunk_version != 0 {
            return Err(Error::MalformedHeader { reason: "chunk_version must be 0 in v1" });
        }

        // ---- 解析上界：必须在任何内存分配之前拒绝 ----
        if self.header_len > parse_limits::MAX_HEADER_LEN {
            return Err(Error::MalformedHeader {
                reason: "header_len exceeds the 64 MiB parse limit",
            });
        }
        if self.tlv_len > parse_limits::MAX_TLV_LEN {
            return Err(Error::MalformedHeader {
                reason: "tlv_len exceeds the 32 MiB parse limit",
            });
        }
        // chunk_size 只卡上限，不卡下限。
        //
        // 设计文档 §8.3 曾建议 MIN_CHUNK_SIZE = 4096，但测试向量 v4 使用 2048 字节块，
        // v3 使用 4096。向量由参考实现产出且已验证，是可执行的事实；而小块**不构成
        // DoS 风险**（只会让文件略大），真正危险的是过大的 chunk_size 导致单块解密
        // 一次性分配巨额内存。因此这里只拒绝过大值和零值。
        if self.chunk_size == 0 {
            return Err(Error::MalformedHeader { reason: "chunk_size must not be zero" });
        }
        if self.chunk_size > parse_limits::MAX_PARSE_CHUNK_SIZE {
            return Err(Error::MalformedHeader {
                reason: "chunk_size exceeds the 64 MiB parse limit",
            });
        }

        // header_len 必须严格等于 96 + 384 + tlv_len + 32
        let expect_len = TLV_AREA_OFFSET
            .checked_add(self.tlv_len as usize)
            .and_then(|v| v.checked_add(HEADER_MAC_LEN))
            .ok_or(Error::MalformedHeader { reason: "tlv_len overflows header_len" })?;
        if (self.header_len as usize) != expect_len {
            return Err(Error::MalformedHeader {
                reason: "header_len does not match 96 + 384 + tlv_len + 32",
            });
        }

        // plaintext_size 与 chunk_size 组合出的块数不得超过 u32
        // （nonce 中的块序号是 u32be，越界会导致 nonce 复用）
        let n = self.n_chunks();
        if n > u64::from(u32::MAX) {
            return Err(Error::MalformedHeader {
                reason: "chunk count exceeds u32; nonce space would be exhausted",
            });
        }

        // COMPRESSED 与 compress_id 必须一致，否则会按错误方式解读载荷
        let compressed_flag = self.flags & flags::COMPRESSED != 0;
        if compressed_flag != (self.compress_id == COMPRESS_ZSTD) {
            return Err(Error::MalformedHeader {
                reason: "COMPRESSED flag contradicts compress_id",
            });
        }
        Ok(())
    }

    /// 序列化为 96 字节。
    ///
    /// # Errors
    ///
    /// 字段非法时返回 [`Error::MalformedHeader`]。
    pub fn to_bytes(&self) -> Result<[u8; FIXED_HEADER_LEN]> {
        self.validate()?;
        let mut w = Writer::with_capacity(FIXED_HEADER_LEN);
        w.bytes(&MAGIC_FILE)
            .u16le(self.version_major)
            .u16le(self.version_minor)
            .u32le(self.header_len)
            .bytes(&self.file_uuid)
            .bytes(&self.vault_salt)
            .u32le(self.flags)
            .u8(self.cipher_id as u8)
            .u8(self.kdf_id)
            .u8(self.compress_id)
            .u8(self.slot_count)
            .u32le(self.argon2_m_kib)
            .u32le(self.argon2_t)
            .u32le(self.argon2_p)
            .u32le(self.chunk_size)
            .u64le(self.plaintext_size)
            .bytes(&self.base_nonce)
            .u8(self.chunk_version)
            .u32le(self.tlv_len)
            .u32le(0); // reserved

        let v = w.into_vec();
        let mut out = [0u8; FIXED_HEADER_LEN];
        let src = v.get(..FIXED_HEADER_LEN).ok_or(Error::MalformedHeader {
            reason: "serialized fixed header shorter than 96 bytes",
        })?;
        out.copy_from_slice(src);
        Ok(out)
    }

    /// 载荷块总数。空文件也有 1 块。
    #[must_use]
    pub const fn n_chunks(&self) -> u64 {
        let cs = self.chunk_size as u64;
        if cs == 0 {
            return 1;
        }
        let n = self.plaintext_size.div_ceil(cs);
        if n == 0 { 1 } else { n }
    }

    /// 是否置位了指定 flag。
    #[must_use]
    pub const fn has_flag(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }

    /// 头部记录的 Argon2 参数。
    ///
    /// 解密时必须用**文件里记录的**参数派生 KEK，不能用当前的默认档位——
    /// 文件可能是用别的档位加密的，用错参数会得到完全不同的 KEK。
    #[must_use]
    pub const fn argon2_params(&self) -> crate::crypto::Argon2Params {
        crate::crypto::Argon2Params {
            m_kib: self.argon2_m_kib,
            t: self.argon2_t,
            p: self.argon2_p,
        }
    }

    /// 未压缩载荷中第 `index` 块密文的 `(偏移, 长度)`。
    ///
    /// 这是视频任意 seek 的数学基础：无需索引表，O(1) 定位。
    ///
    /// ```text
    /// ct_offset(i) = header_len + i × (chunk_size + 16)
    /// ct_len(i)    = min(chunk_size, plaintext_size - i × chunk_size) + 16
    /// ```
    ///
    /// # Errors
    ///
    /// - [`Error::MalformedHeader`]：文件是压缩的（此时公式不适用，须查索引表）
    /// - [`Error::ChunkOutOfRange`]：`index` 超出总块数
    pub fn chunk_ct_range(&self, index: u64) -> Result<(u64, u64)> {
        if self.has_flag(flags::COMPRESSED) {
            return Err(Error::MalformedHeader {
                reason: "chunk_ct_range is invalid for compressed payloads; use the index table",
            });
        }
        let total = self.n_chunks();
        if index >= total {
            return Err(Error::ChunkOutOfRange { index, total });
        }
        let cs = u64::from(self.chunk_size);
        let tag = crate::crypto::TAG_LEN as u64;

        let stride = cs
            .checked_add(tag)
            .ok_or(Error::MalformedHeader { reason: "chunk stride overflow" })?;
        let ct_offset = u64::from(self.header_len)
            .checked_add(index.checked_mul(stride).ok_or(Error::MalformedHeader {
                reason: "chunk offset overflow",
            })?)
            .ok_or(Error::MalformedHeader { reason: "chunk offset overflow" })?;

        let consumed = index
            .checked_mul(cs)
            .ok_or(Error::MalformedHeader { reason: "chunk offset overflow" })?;
        let plain_len = self.plaintext_size.saturating_sub(consumed).min(cs);
        let ct_len = plain_len
            .checked_add(tag)
            .ok_or(Error::MalformedHeader { reason: "chunk length overflow" })?;

        Ok((ct_offset, ct_len))
    }

    /// 明文偏移所属的块序号。
    ///
    /// `chunk_size` 为 0 时返回 0。正常文件不会出现该值（[`Self::validate`] 已拒绝），
    /// 这里只是避免除零 panic。
    #[must_use]
    pub const fn chunk_index_of(&self, plain_offset: u64) -> u64 {
        match plain_offset.checked_div(self.chunk_size as u64) {
            Some(v) => v,
            None => 0,
        }
    }
}

/// 校验分块大小落在**新建文件**的推荐范围内。
///
/// # 这是策略校验，不是格式校验
///
/// 只应在**创建新文件**时调用，用于阻止用户选择不合理的块大小。
///
/// **读取路径绝不能调用它**：格式本身只要求 `chunk_size` 非零，
/// 小于 [`MIN_CHUNK_SIZE`] 的文件在格式上完全合法（测试向量 v3/v4 就用了
/// 4096 与 2048 字节的块）。若在解析时强加此范围，实现将无法读取这些合法文件。
/// 格式层的自洽性校验在 [`FixedHeader::validate`] 中完成。
///
/// # Errors
///
/// 超出 [`MIN_CHUNK_SIZE`]–[`MAX_CHUNK_SIZE`] 时返回 [`Error::InvalidChunkSize`]。
pub const fn validate_chunk_size(chunk_size: u32) -> Result<()> {
    if chunk_size < MIN_CHUNK_SIZE || chunk_size > MAX_CHUNK_SIZE {
        return Err(Error::InvalidChunkSize {
            got: chunk_size,
            min: MIN_CHUNK_SIZE,
            max: MAX_CHUNK_SIZE,
        });
    }
    Ok(())
}
