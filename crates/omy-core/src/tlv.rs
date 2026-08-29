//! TLV 扩展区的编解码。
//!
//! 编码：`type u16le | flags u16le | length u32le | value`（规范 §4.1）。
//!
//! # CRITICAL 位是格式可扩展性的关键
//!
//! 机制类似 PNG chunk 的大小写位：未知但标记 CRITICAL 的条目**必须**导致拒绝打开，
//! 因为它意味着「不理解此字段就无法正确解读文件」。未知且非 CRITICAL 的条目可安全忽略，
//! 且在重写文件时应原样保留。

use crate::crypto::{CipherId, Fek, ZERO_NONCE};
use crate::error::{Error, Result};
use crate::header::{FILENAME_BUCKET, parse_limits};
use crate::util::{Cursor, Writer};

/// TLV 条目头长度（type 2 + flags 2 + length 4）。
pub const TLV_HEADER_LEN: usize = 8;

/// 压缩索引单条记录长度（u64 offset + u32 ct_len + u32 plain_len）。
pub const INDEX_ENTRY_LEN: usize = 16;

/// 已分配的 TLV 类型（规范 §4.4）。
pub mod types {
    /// 加密的原文件名。CRITICAL + ENCRYPTED。
    pub const FILENAME: u16 = 0x0001;
    /// 明文后缀（不含点）。
    pub const PLAIN_EXT: u16 = 0x0002;
    /// 加密缩略图。
    pub const THUMBNAIL: u16 = 0x0003;
    /// 媒体元信息 JSON。
    pub const MEDIA_META: u16 = 0x0004;
    /// 压缩块索引表。CRITICAL + ENCRYPTED。
    pub const COMPRESSION_INDEX: u16 = 0x0005;
    /// MP4 moov box 副本。
    pub const MOOV_CACHE: u16 = 0x0006;
    /// 分片信息。CRITICAL。
    pub const SHARD_INFO: u16 = 0x0007;
    /// 原始文件元数据 JSON。
    pub const ORIGINAL_META: u16 = 0x0008;
    /// 原始明文的 BLAKE2b-256。
    pub const CONTENT_HASH: u16 = 0x0009;
    /// zstd 训练字典。
    pub const ZSTD_DICT: u16 = 0x000A;
    /// portable slot 的独立盐与参数。
    pub const PORTABLE_SLOT: u16 = 0x000B;
    /// 字幕数据。
    pub const SUBTITLES: u16 = 0x000C;
    /// 转码参数与原文件哈希。
    pub const TRANSCODE_INFO: u16 = 0x000D;
    /// 容器模式的目录索引。CRITICAL + ENCRYPTED。
    pub const FOLDER_INDEX: u16 = 0x000E;
    /// 伪装模式的真实 header 偏移。
    pub const DISGUISE_INFO: u16 = 0x000F;

    /// 私有类型区间下界。官方实现必须忽略此区间的未知类型。
    pub const PRIVATE_RANGE_START: u16 = 0x8000;

    /// 判断类型是否为本实现已知。
    #[must_use]
    pub const fn is_known(t: u16) -> bool {
        matches!(
            t,
            FILENAME
                | PLAIN_EXT
                | THUMBNAIL
                | MEDIA_META
                | COMPRESSION_INDEX
                | MOOV_CACHE
                | SHARD_INFO
                | ORIGINAL_META
                | CONTENT_HASH
                | ZSTD_DICT
                | PORTABLE_SLOT
                | SUBTITLES
                | TRANSCODE_INFO
                | FOLDER_INDEX
                | DISGUISE_INFO
        )
    }
}

/// TLV flags 位（规范 §4.2）。
pub mod tlv_flags {
    /// 不认识此类型时必须拒绝打开文件。
    pub const CRITICAL: u16 = 1 << 0;
    /// value 是密文，需用 FEK 派生密钥解密。
    pub const ENCRYPTED: u16 = 1 << 1;
}

/// 单个 TLV 条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlvEntry {
    /// 类型号。
    pub tlv_type: u16,
    /// 标志位。
    pub flags: u16,
    /// 原始 value（若置 ENCRYPTED 则为密文）。
    pub value: Vec<u8>,
}

impl TlvEntry {
    /// 构造条目。
    #[must_use]
    pub const fn new(tlv_type: u16, flags: u16, value: Vec<u8>) -> Self {
        Self { tlv_type, flags, value }
    }

    /// 是否为 CRITICAL。
    #[must_use]
    pub const fn is_critical(&self) -> bool {
        self.flags & tlv_flags::CRITICAL != 0
    }

    /// 是否为加密条目。
    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        self.flags & tlv_flags::ENCRYPTED != 0
    }

    /// 编码后占用的总字节数。
    ///
    /// 未标 `const`：`Vec::len` 在 const 上下文中需要 Rust 1.87，而本 crate MSRV 为 1.85。
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        TLV_HEADER_LEN.saturating_add(self.value.len())
    }
}

/// TLV 区的集合。保留原始顺序，以便重写文件时保持字节稳定。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlvSet {
    entries: Vec<TlvEntry>,
}

impl TlvSet {
    /// 空集合。
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// 追加条目。
    pub fn push(&mut self, e: TlvEntry) -> &mut Self {
        self.entries.push(e);
        self
    }

    /// 全部条目。
    #[must_use]
    pub fn entries(&self) -> &[TlvEntry] {
        &self.entries
    }

    /// 条目数量。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按类型查找第一个条目。
    #[must_use]
    pub fn find(&self, tlv_type: u16) -> Option<&TlvEntry> {
        self.entries.iter().find(|e| e.tlv_type == tlv_type)
    }

    /// 编码后的总字节数。
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        self.entries.iter().map(TlvEntry::encoded_len).sum()
    }

    /// 解析 TLV 区。
    ///
    /// 只做结构解析，不检查未知 CRITICAL 类型——那需要在 MAC 验证通过之后再做，
    /// 见 [`TlvSet::check_critical`]。
    ///
    /// 条目数受 [`parse_limits::MAX_TLV_ENTRIES`] 限制，防止大量零长度条目耗尽内存。
    ///
    /// # Errors
    ///
    /// - [`Error::Truncated`]：长度字段越界或数据截断
    /// - [`Error::MalformedTlv`]：条目数超过上限
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut c = Cursor::new(data, "tlv area");
        let mut entries = Vec::new();
        while c.remaining() > 0 {
            if entries.len() >= parse_limits::MAX_TLV_ENTRIES {
                return Err(Error::MalformedTlv {
                    tlv_type: 0,
                    reason: "too many TLV entries",
                });
            }
            if c.remaining() < TLV_HEADER_LEN {
                return Err(Error::Truncated {
                    context: "tlv entry header",
                    need: TLV_HEADER_LEN,
                    got: c.remaining(),
                });
            }
            let tlv_type = c.u16le()?;
            let flags = c.u16le()?;
            let length = c.u32le()? as usize;
            // take() 会做边界检查，因此这里无需额外校验 length 上界：
            // 声明超长的条目会因数据不足而返回 Truncated
            let value = c.take(length)?.to_vec();
            entries.push(TlvEntry { tlv_type, flags, value });
        }
        Ok(Self { entries })
    }

    /// 检查是否存在未知且 CRITICAL 的条目。
    ///
    /// 规范 §9 步骤 7 要求此时拒绝打开文件。私有区间（≥ 0x8000）的未知类型即使标记
    /// CRITICAL 也应忽略，因为那是第三方实现的自用字段。
    ///
    /// # Errors
    ///
    /// 存在未知 CRITICAL 条目时返回 [`Error::UnknownCriticalTlv`]。
    pub fn check_critical(&self) -> Result<()> {
        for e in &self.entries {
            if e.is_critical()
                && !types::is_known(e.tlv_type)
                && e.tlv_type < types::PRIVATE_RANGE_START
            {
                return Err(Error::UnknownCriticalTlv { tlv_type: e.tlv_type });
            }
        }
        Ok(())
    }

    /// 序列化。
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(self.encoded_len());
        for e in &self.entries {
            // length 字段是 u32，超长条目在构造时就应被拒绝；这里用饱和转换兜底
            let len = u32::try_from(e.value.len()).unwrap_or(u32::MAX);
            w.u16le(e.tlv_type).u16le(e.flags).u32le(len).bytes(&e.value);
        }
        w.into_vec()
    }

    /// 解密某个加密条目的 value。
    ///
    /// # Errors
    ///
    /// - [`Error::MissingTlv`]：条目不存在
    /// - [`Error::MalformedTlv`]：条目未标记 ENCRYPTED，或解密认证失败
    pub fn decrypt_value(&self, tlv_type: u16, fek: &Fek, cipher: CipherId) -> Result<Vec<u8>> {
        let e = self.find(tlv_type).ok_or(Error::MissingTlv { tlv_type })?;
        if !e.is_encrypted() {
            return Err(Error::MalformedTlv {
                tlv_type,
                reason: "entry is not marked ENCRYPTED",
            });
        }
        let key = fek.derive_tlv_key(tlv_type);
        cipher
            .decrypt(&key, &ZERO_NONCE, &e.value, &[])?
            .ok_or(Error::MalformedTlv { tlv_type, reason: "AEAD authentication failed" })
    }
}

/// 构造一个加密 TLV 条目。
///
/// # Errors
///
/// AEAD 加密失败时返回错误。
pub fn encrypt_entry(
    tlv_type: u16,
    extra_flags: u16,
    plaintext: &[u8],
    fek: &Fek,
    cipher: CipherId,
) -> Result<TlvEntry> {
    let key = fek.derive_tlv_key(tlv_type);
    let ct = cipher.encrypt(&key, &ZERO_NONCE, plaintext, &[])?;
    Ok(TlvEntry::new(tlv_type, extra_flags | tlv_flags::ENCRYPTED, ct))
}

/// 按 64 字节桶填充文件名（规范 §4.5）。
///
/// ```text
/// need   = 2 + len(name_utf8)
/// bucket = ceil(need / 64) * 64
/// padded = u16le(真实长度) || name_utf8 || 0x00 × (bucket - need)
/// ```
///
/// # 实现陷阱
///
/// 桶大小必须基于 `2 + 名字长度`，**不能**只基于名字长度。若写成
/// `(len / 64 + 1) * 64`，当 `len = 63` 时 `need = 65` 而桶算成 64，
/// 填充量为负、`padded` 越出桶边界，该长度的文件名反而暴露自己的长度特征。
/// 参考实现在交叉验证中命中过这个 bug。
///
/// # Errors
///
/// 文件名字节长度超过 `u16::MAX` 时返回 [`Error::MalformedTlv`]。
pub fn pad_filename(name: &str) -> Result<Vec<u8>> {
    let bytes = name.as_bytes();
    let real_len = u16::try_from(bytes.len()).map_err(|_| Error::MalformedTlv {
        tlv_type: types::FILENAME,
        reason: "filename longer than 65535 bytes",
    })?;

    // 关键：need 含 2 字节长度前缀
    let need = bytes.len().saturating_add(2);
    let bucket = need.div_ceil(FILENAME_BUCKET).saturating_mul(FILENAME_BUCKET);
    debug_assert!(bucket >= need, "bucket 必须不小于 need，否则填充量为负");

    let mut w = Writer::with_capacity(bucket);
    w.u16le(real_len).bytes(bytes).zeros(bucket.saturating_sub(need));
    let out = w.into_vec();
    debug_assert_eq!(out.len(), bucket, "padded 长度必须恰好等于桶大小");
    Ok(out)
}

/// 从填充后的字节还原文件名。
///
/// # Errors
///
/// - [`Error::MalformedTlv`]：数据过短、长度前缀越界，或内容不是合法 UTF-8
pub fn unpad_filename(padded: &[u8]) -> Result<String> {
    let mut c = Cursor::new(padded, "filename tlv");
    let real_len = c.u16le().map_err(|_| Error::MalformedTlv {
        tlv_type: types::FILENAME,
        reason: "missing length prefix",
    })? as usize;
    let name = c.take(real_len).map_err(|_| Error::MalformedTlv {
        tlv_type: types::FILENAME,
        reason: "declared length exceeds padded data",
    })?;
    String::from_utf8(name.to_vec()).map_err(|_| Error::MalformedTlv {
        tlv_type: types::FILENAME,
        reason: "filename is not valid UTF-8",
    })
}

/// 压缩索引表的一条记录（规范 §6.2，每条 16 字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompressionIndexEntry {
    /// 相对载荷起始的密文偏移。
    pub ct_offset: u64,
    /// 该块密文长度（含 tag）。
    pub ct_len: u32,
    /// 该块解压后的明文长度。
    pub plain_len: u32,
}

/// 编码压缩索引表。
///
/// ```text
/// u32le entry_count
/// repeat: u64le ct_offset | u32le ct_len | u32le plain_len
/// ```
#[must_use]
pub fn encode_compression_index(entries: &[CompressionIndexEntry]) -> Vec<u8> {
    let count = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let mut w = Writer::with_capacity(
        4usize.saturating_add(entries.len().saturating_mul(INDEX_ENTRY_LEN)),
    );
    w.u32le(count);
    for e in entries {
        w.u64le(e.ct_offset).u32le(e.ct_len).u32le(e.plain_len);
    }
    w.into_vec()
}

/// 解码压缩索引表。
///
/// # 分配安全
///
/// `entry_count` 来自不可信输入，因此在 `Vec::with_capacity` **之前**先校验上界，
/// 再校验它与实际数据长度严格相符。否则声明 `entry_count = 0xFFFFFFFF` 的
/// 4 字节数据就能触发 64 GiB 的预分配。
///
/// # Errors
///
/// 条目数超限、数据截断或条目数与实际长度不符时返回 [`Error::MalformedTlv`]。
pub fn decode_compression_index(data: &[u8]) -> Result<Vec<CompressionIndexEntry>> {
    let mut c = Cursor::new(data, "compression index");
    let count_u32 = c.u32le().map_err(|_| Error::MalformedTlv {
        tlv_type: types::COMPRESSION_INDEX,
        reason: "missing entry_count",
    })?;

    // 先查上界，再谈分配
    if count_u32 > parse_limits::MAX_INDEX_ENTRIES {
        return Err(Error::MalformedTlv {
            tlv_type: types::COMPRESSION_INDEX,
            reason: "entry_count exceeds the parse limit",
        });
    }
    let count = count_u32 as usize;

    let need = count.checked_mul(INDEX_ENTRY_LEN).ok_or(Error::MalformedTlv {
        tlv_type: types::COMPRESSION_INDEX,
        reason: "entry_count overflows",
    })?;
    // 严格相等：多余或不足都说明数据不可信
    if c.remaining() != need {
        return Err(Error::MalformedTlv {
            tlv_type: types::COMPRESSION_INDEX,
            reason: "entry_count does not match data length",
        });
    }

    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let ct_offset = c.u64le()?;
        let ct_len = c.u32le()?;
        let plain_len = c.u32le()?;
        out.push(CompressionIndexEntry { ct_offset, ct_len, plain_len });
    }
    Ok(out)
}
