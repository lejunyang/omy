//! 顶层封装：完整的加密文件读写。
//!
//! 把 header、slot、TLV、载荷各层串成实际可用的 API。
//!
//! # 读取顺序不可调换
//!
//! 严格遵循规范 §9：
//!
//! 1. 校验 magic 与版本 → 2. 解析固定头 → 3. 解包 FEK →
//! 4. **验证 header MAC** → 5. 检查 CRITICAL TLV → 6. 读载荷
//!
//! 第 4 步必须在第 5、6 步**之前**。原因：TLV 与载荷的解读方式（是否压缩、
//! chunk_size、plaintext_size）全部来自头部字段，若不先验证 MAC，攻击者可以
//! 篡改这些字段诱导错误解读。MAC 验证失败必须立即终止。

use crate::crypto::{Argon2Params, CipherId, Fek, Kek, SecretKey, content_hash, header_mac, verify_header_mac};
use crate::error::{Error, Result};
use crate::header::{
    COMPRESS_NONE, COMPRESS_ZSTD, DEFAULT_CHUNK_SIZE, FIXED_HEADER_LEN, FixedHeader,
    HEADER_MAC_LEN, KDF_ARGON2ID, SLOT_AREA_LEN, SLOT_AREA_OFFSET, SLOT_COUNT, SLOT_COUNT_U8,
    TLV_AREA_OFFSET, VERSION_MAJOR, VERSION_MINOR, flags, validate_chunk_size,
};
use crate::payload::{decrypt_payload, encrypt_payload};
use crate::slot::{build_slot_area, unwrap_fek};
use crate::tlv::{
    CompressionIndexEntry, TlvEntry, TlvSet, decode_compression_index, encode_compression_index,
    encrypt_entry, pad_filename, tlv_flags, types, unpad_filename,
};
use crate::util::{Writer, random_16};

/// 加密选项。
#[derive(Debug, Clone)]
pub struct EncryptOptions {
    /// 原始文件名。为 `None` 时不写入文件名 TLV。
    pub filename: Option<String>,
    /// 是否以明文保留后缀。便于在不解密的情况下按类型筛选，但会泄露文件类型。
    pub preserve_extension: bool,
    /// 是否启用 zstd 压缩。
    pub compress: bool,
    /// zstd 压缩级别（1–22）。
    pub zstd_level: i32,
    /// 明文分块大小。
    pub chunk_size: u32,
    /// AEAD 算法。
    pub cipher: CipherId,
    /// Argon2 参数。
    pub argon2: Argon2Params,
    /// 是否写入内容哈希以支持解密后自检。
    pub write_content_hash: bool,
    /// 加密缩略图数据（WebP/JPEG），建议不超过 32 KB。
    pub thumbnail: Option<Vec<u8>>,
    /// 媒体元信息 JSON。
    pub media_meta: Option<Vec<u8>>,
}

impl Default for EncryptOptions {
    fn default() -> Self {
        Self {
            filename: None,
            preserve_extension: false,
            compress: false,
            zstd_level: 3,
            chunk_size: DEFAULT_CHUNK_SIZE,
            cipher: CipherId::ChaCha20Poly1305,
            argon2: Argon2Params::default(),
            write_content_hash: true,
            thumbnail: None,
            media_meta: None,
        }
    }
}

/// 加密所需的随机材料。
///
/// 单独抽出来是为了让加密过程**可确定性复现**——测试向量验证要求「相同输入加相同
/// 随机源产出逐字节相同输出」，若随机数在函数内部生成就无法做到。
#[derive(Debug, Clone)]
pub struct RandomMaterial {
    /// 文件唯一标识。
    pub file_uuid: [u8; 16],
    /// 载荷 nonce 前缀。
    pub base_nonce: [u8; 7],
    /// 未使用 slot 的填充字节。为 `None` 时现场生成随机值。
    pub slot_padding: Option<Vec<u8>>,
}

impl RandomMaterial {
    /// 从系统 CSPRNG 生成。
    #[must_use]
    pub fn generate() -> Self {
        let mut bn = [0u8; 7];
        crate::util::fill_random(&mut bn);
        Self { file_uuid: random_16(), base_nonce: bn, slot_padding: None }
    }
}

/// 已加密文件的完整字节与其头部信息。
#[derive(Debug)]
pub struct EncryptedFile {
    /// 完整文件字节。
    pub bytes: Vec<u8>,
    /// 解析好的固定头。
    pub header: FixedHeader,
}

/// 加密数据，产出完整的 omy 文件字节。
///
/// `vault_salt` 应在同一 vault 内保持一致，这样多个文件可复用同一次 Argon2 计算。
///
/// # Errors
///
/// - [`Error::TooManySlots`]：KEK 超过 8 个
/// - [`Error::InvalidChunkSize`]：分块大小超出允许范围
/// - [`Error::Compression`]：压缩失败
pub fn encrypt(
    plaintext: &[u8],
    keks: &[Kek],
    vault_salt: &[u8; 16],
    opts: &EncryptOptions,
    rnd: &RandomMaterial,
) -> Result<EncryptedFile> {
    if keks.is_empty() {
        return Err(Error::TooManySlots { got: 0, max: SLOT_COUNT });
    }
    validate_chunk_size(opts.chunk_size)?;

    let fek = Fek::random();
    encrypt_with_fek(plaintext, keks, vault_salt, opts, rnd, &fek)
}

/// 用指定 FEK 加密。
///
/// 这是**底层 API**：不做 `chunk_size` 的推荐范围校验，只要求非零。
/// 因此可用于复现测试向量（v3/v4 使用 4096 / 2048 字节的小块）以及处理
/// 其它实现产出的合法文件。面向用户的入口请用 [`encrypt`]，它会做策略校验。
///
/// # Errors
///
/// - [`Error::MalformedHeader`]：`chunk_size` 为 0，或字段溢出
/// - [`Error::Compression`]：压缩失败
pub fn encrypt_with_fek(
    plaintext: &[u8],
    keks: &[Kek],
    vault_salt: &[u8; 16],
    opts: &EncryptOptions,
    rnd: &RandomMaterial,
    fek: &Fek,
) -> Result<EncryptedFile> {
    if opts.chunk_size == 0 {
        return Err(Error::MalformedHeader { reason: "chunk_size must not be zero" });
    }

    let mut file_flags = 0u32;
    if opts.filename.is_some() {
        file_flags |= flags::FILENAME_ENCRYPTED;
    }
    if opts.preserve_extension {
        file_flags |= flags::EXT_PRESERVED;
    }
    if opts.compress {
        file_flags |= flags::COMPRESSED;
    }
    if opts.thumbnail.is_some() {
        file_flags |= flags::HAS_THUMBNAIL;
    }

    // 先构造不含索引的 TLV，算出载荷，再回填压缩索引。
    // 压缩索引的长度取决于块数，而块数与 TLV 无关，所以两趟即可收敛——
    // 但索引 TLV 的存在会改变 header_len，而索引内容里的 ct_offset 是**相对载荷起始**的，
    // 不受 header_len 影响，因此不会循环依赖。
    let mut tlvs = TlvSet::new();

    if let Some(name) = &opts.filename {
        let padded = pad_filename(name)?;
        tlvs.push(encrypt_entry(
            types::FILENAME,
            tlv_flags::CRITICAL,
            &padded,
            fek,
            opts.cipher,
        )?);
    }
    if opts.preserve_extension {
        let ext = opts
            .filename
            .as_deref()
            .and_then(|n| n.rsplit_once('.').map(|(_, e)| e))
            .unwrap_or("");
        tlvs.push(TlvEntry::new(types::PLAIN_EXT, 0, ext.as_bytes().to_vec()));
    }
    if let Some(t) = &opts.thumbnail {
        tlvs.push(encrypt_entry(types::THUMBNAIL, 0, t, fek, opts.cipher)?);
    }
    if let Some(m) = &opts.media_meta {
        tlvs.push(encrypt_entry(types::MEDIA_META, 0, m, fek, opts.cipher)?);
    }
    if opts.write_content_hash {
        let h = content_hash(plaintext);
        tlvs.push(encrypt_entry(types::CONTENT_HASH, 0, &h, fek, opts.cipher)?);
    }

    let mut header = FixedHeader {
        version_major: VERSION_MAJOR,
        version_minor: VERSION_MINOR,
        header_len: 0, // 稍后计算
        file_uuid: rnd.file_uuid,
        vault_salt: *vault_salt,
        flags: file_flags,
        cipher_id: opts.cipher,
        kdf_id: KDF_ARGON2ID,
        compress_id: if opts.compress { COMPRESS_ZSTD } else { COMPRESS_NONE },
        slot_count: SLOT_COUNT_U8,
        argon2_m_kib: opts.argon2.m_kib,
        argon2_t: opts.argon2.t,
        argon2_p: opts.argon2.p,
        chunk_size: opts.chunk_size,
        plaintext_size: plaintext.len() as u64,
        base_nonce: rnd.base_nonce,
        chunk_version: 0,
        tlv_len: 0, // 稍后计算
    };

    let payload_key = fek.derive_payload_key(&header.file_uuid);
    let (payload, index) = encrypt_payload(&header, &payload_key, plaintext, opts.zstd_level)?;

    // 压缩模式必须写 CRITICAL 索引 TLV，否则无法定位块
    if opts.compress {
        let enc = encode_compression_index(&index);
        tlvs.push(encrypt_entry(
            types::COMPRESSION_INDEX,
            tlv_flags::CRITICAL,
            &enc,
            fek,
            opts.cipher,
        )?);
    }

    let tlv_blob = tlvs.to_bytes();
    header.tlv_len = u32::try_from(tlv_blob.len())
        .map_err(|_| Error::MalformedHeader { reason: "tlv area exceeds u32" })?;
    header.header_len = u32::try_from(
        TLV_AREA_OFFSET
            .saturating_add(tlv_blob.len())
            .saturating_add(HEADER_MAC_LEN),
    )
    .map_err(|_| Error::MalformedHeader { reason: "header_len exceeds u32" })?;

    let fixed = header.to_bytes()?;
    let slot_area = if let Some(pad) = &rnd.slot_padding {
        build_slot_area_with_padding(keks, fek, &header.file_uuid, opts.cipher, pad)?
    } else {
        build_slot_area(keks, fek, &header.file_uuid, opts.cipher)?
    };

    // MAC 覆盖 fixed_header || slot_area || tlv_blob
    let mut covered = Writer::with_capacity(header.header_len as usize);
    covered.bytes(&fixed).bytes(&slot_area).bytes(&tlv_blob);
    let mac_key = fek.derive_header_mac_key(&header.file_uuid);
    let mac = header_mac(&mac_key, covered.as_slice());

    let mut out = Writer::with_capacity(
        (header.header_len as usize).saturating_add(payload.len()),
    );
    out.bytes(covered.as_slice()).bytes(&mac).bytes(&payload);

    let bytes = out.into_vec();
    debug_assert_eq!(
        bytes.len(),
        (header.header_len as usize).saturating_add(payload.len()),
        "输出长度与 header_len + payload 不符"
    );

    Ok(EncryptedFile { bytes, header })
}

/// 用指定的填充字节构建 slot 区，供确定性复现使用。
fn build_slot_area_with_padding(
    keks: &[Kek],
    fek: &Fek,
    file_uuid: &[u8; 16],
    cipher: CipherId,
    padding: &[u8],
) -> Result<Vec<u8>> {
    use crate::crypto::ZERO_NONCE;
    use crate::header::SLOT_LEN;

    if keks.len() > SLOT_COUNT {
        return Err(Error::TooManySlots { got: keks.len(), max: SLOT_COUNT });
    }
    let mut w = Writer::with_capacity(SLOT_AREA_LEN);
    for (i, kek) in keks.iter().enumerate() {
        let idx = u16::try_from(i)
            .map_err(|_| Error::TooManySlots { got: keks.len(), max: SLOT_COUNT })?;
        let wk = kek.derive_slot_key(file_uuid, idx);
        w.bytes(&cipher.encrypt(&wk, &ZERO_NONCE, fek.as_key().as_bytes(), &[])?);
    }
    let need = SLOT_AREA_LEN.saturating_sub(keks.len().saturating_mul(SLOT_LEN));
    let pad = padding.get(..need).ok_or(Error::MalformedHeader {
        reason: "provided slot padding shorter than required",
    })?;
    w.bytes(pad);
    Ok(w.into_vec())
}

/// 已打开的加密文件：头部已验证，FEK 已解包。
///
/// 持有 FEK 与载荷密钥，可按需解密 TLV 与载荷块。析构时密钥自动擦除。
#[derive(Debug)]
pub struct OpenedFile {
    /// 固定头。
    pub header: FixedHeader,
    /// TLV 集合（value 仍为原始形态，加密条目未解密）。
    pub tlvs: TlvSet,
    /// 命中的 KEK 在候选列表中的下标。
    pub kek_index: usize,
    /// 命中的 slot 序号。
    pub slot_index: u16,
    fek: Fek,
    payload_key: SecretKey,
}

impl OpenedFile {
    /// 载荷密钥。
    #[must_use]
    pub const fn payload_key(&self) -> &SecretKey {
        &self.payload_key
    }

    /// 文件加密密钥。
    #[must_use]
    pub const fn fek(&self) -> &Fek {
        &self.fek
    }

    /// 解密并还原原始文件名。
    ///
    /// # Errors
    ///
    /// - [`Error::MissingTlv`]：文件未存储文件名
    /// - [`Error::MalformedTlv`]：解密失败或内容非法
    pub fn filename(&self) -> Result<String> {
        let padded = self.tlvs.decrypt_value(types::FILENAME, &self.fek, self.header.cipher_id)?;
        unpad_filename(&padded)
    }

    /// 明文后缀（若存在）。
    #[must_use]
    pub fn plain_extension(&self) -> Option<String> {
        self.tlvs
            .find(types::PLAIN_EXT)
            .map(|e| String::from_utf8_lossy(&e.value).into_owned())
    }

    /// 解密缩略图。
    ///
    /// # Errors
    ///
    /// 条目不存在或解密失败时返回错误。
    pub fn thumbnail(&self) -> Result<Vec<u8>> {
        self.tlvs.decrypt_value(types::THUMBNAIL, &self.fek, self.header.cipher_id)
    }

    /// 解密媒体元信息。
    ///
    /// # Errors
    ///
    /// 条目不存在或解密失败时返回错误。
    pub fn media_meta(&self) -> Result<Vec<u8>> {
        self.tlvs.decrypt_value(types::MEDIA_META, &self.fek, self.header.cipher_id)
    }

    /// 记录在 TLV 中的原始明文哈希。
    ///
    /// # Errors
    ///
    /// 条目不存在、解密失败或长度不是 32 字节时返回错误。
    pub fn stored_content_hash(&self) -> Result<[u8; 32]> {
        let v = self.tlvs.decrypt_value(types::CONTENT_HASH, &self.fek, self.header.cipher_id)?;
        let s = v.get(..32).ok_or(Error::MalformedTlv {
            tlv_type: types::CONTENT_HASH,
            reason: "content hash must be 32 bytes",
        })?;
        let mut out = [0u8; 32];
        out.copy_from_slice(s);
        Ok(out)
    }

    /// 压缩索引表（仅压缩文件存在）。
    ///
    /// # Errors
    ///
    /// 压缩文件缺少索引，或索引内容非法时返回错误。
    pub fn compression_index(&self) -> Result<Vec<CompressionIndexEntry>> {
        if !self.header.has_flag(flags::COMPRESSED) {
            return Ok(Vec::new());
        }
        let raw =
            self.tlvs.decrypt_value(types::COMPRESSION_INDEX, &self.fek, self.header.cipher_id)?;
        decode_compression_index(&raw)
    }

    /// 解密全部载荷。
    ///
    /// 若文件写有内容哈希，会自动校验；不匹配返回 [`Error::ContentHashMismatch`]。
    ///
    /// # Errors
    ///
    /// - [`Error::ChunkAuthFailed`]：某块认证失败
    /// - [`Error::ContentHashMismatch`]：内容哈希校验失败
    pub fn decrypt_all(&self, file_bytes: &[u8]) -> Result<Vec<u8>> {
        let start = self.header.header_len as usize;
        let payload = file_bytes.get(start..).ok_or(Error::Truncated {
            context: "payload",
            need: start,
            got: file_bytes.len(),
        })?;

        let idx = self.compression_index()?;
        let idx_ref = if idx.is_empty() { None } else { Some(idx.as_slice()) };
        let plain = decrypt_payload(&self.header, &self.payload_key, payload, idx_ref)?;

        // 有哈希就校验——这是可还原性承诺的自检手段
        if let Ok(expected) = self.stored_content_hash() {
            use subtle::ConstantTimeEq;
            let actual = content_hash(&plain);
            if !bool::from(actual.ct_eq(&expected)) {
                return Err(Error::ContentHashMismatch);
            }
        }
        Ok(plain)
    }
}

/// 仅解析头部而不尝试解密，用于快速识别文件。
///
/// 扫描目录时先用它判断「这是不是 omy 文件」，避免对每个文件都做密钥尝试。
///
/// # Errors
///
/// - [`Error::BadMagic`]：不是 omy 文件
/// - [`Error::UnsupportedVersion`]：版本过高
/// - [`Error::Truncated`]：数据不足
pub fn peek_header(data: &[u8]) -> Result<FixedHeader> {
    FixedHeader::parse(data)
}

/// 判断字节流是否以 omy 主文件 magic 开头。
///
/// 只看前 8 字节，是扫描时最廉价的过滤手段。
#[must_use]
pub fn is_omy_file(data: &[u8]) -> bool {
    data.get(..8).is_some_and(|m| m == crate::header::MAGIC_FILE)
}

/// 打开加密文件：解包 FEK 并验证头部完整性。
///
/// # 顺序不可调换
///
/// 严格按规范 §9：解包 FEK → **立即验证 MAC** → 再检查 CRITICAL TLV。
/// 若在 MAC 验证前就依据头部字段解读 TLV 或载荷，攻击者可篡改字段诱导错误解读。
///
/// # Errors
///
/// - [`Error::NoMatchingSlot`]：没有 KEK 能解开任何 slot。**这不代表文件损坏**，
///   也可能只是该文件不属于当前解锁的密码
/// - [`Error::HeaderMacMismatch`]：文件被篡改
/// - [`Error::UnknownCriticalTlv`]：存在无法理解的必需字段
pub fn open(data: &[u8], keks: &[Kek]) -> Result<OpenedFile> {
    let header = FixedHeader::parse(data)?;

    let hlen = header.header_len as usize;
    if data.len() < hlen {
        return Err(Error::Truncated { context: "header", need: hlen, got: data.len() });
    }

    let slot_end = SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN);
    let slot_area = data.get(SLOT_AREA_OFFSET..slot_end).ok_or(Error::Truncated {
        context: "key slot area",
        need: slot_end,
        got: data.len(),
    })?;

    let tlv_end = TLV_AREA_OFFSET.saturating_add(header.tlv_len as usize);
    let tlv_blob = data.get(TLV_AREA_OFFSET..tlv_end).ok_or(Error::Truncated {
        context: "tlv area",
        need: tlv_end,
        got: data.len(),
    })?;

    let mac_end = tlv_end.saturating_add(HEADER_MAC_LEN);
    let stored_mac = data.get(tlv_end..mac_end).ok_or(Error::Truncated {
        context: "header mac",
        need: mac_end,
        got: data.len(),
    })?;

    // 步骤 5：解包 FEK
    let (fek, kek_index, slot_index) =
        unwrap_fek(slot_area, keks, &header.file_uuid, header.cipher_id)?;

    // 步骤 6：立即验证 MAC，失败必须终止
    let covered = data.get(..tlv_end).ok_or(Error::Truncated {
        context: "mac-covered region",
        need: tlv_end,
        got: data.len(),
    })?;
    let mac_key = fek.derive_header_mac_key(&header.file_uuid);
    let mut expected = [0u8; HEADER_MAC_LEN];
    let src = stored_mac.get(..HEADER_MAC_LEN).ok_or(Error::Truncated {
        context: "header mac",
        need: HEADER_MAC_LEN,
        got: stored_mac.len(),
    })?;
    expected.copy_from_slice(src);
    if !verify_header_mac(&mac_key, covered, &expected) {
        return Err(Error::HeaderMacMismatch);
    }

    // 步骤 7：MAC 已确认可信，现在可以安全解读 TLV
    let tlvs = TlvSet::parse(tlv_blob)?;
    tlvs.check_critical()?;

    // 压缩文件必须带索引，否则无法定位块
    if header.has_flag(flags::COMPRESSED) && tlvs.find(types::COMPRESSION_INDEX).is_none() {
        return Err(Error::MissingTlv { tlv_type: types::COMPRESSION_INDEX });
    }

    let payload_key = fek.derive_payload_key(&header.file_uuid);
    Ok(OpenedFile { header, tlvs, kek_index, slot_index, fek, payload_key })
}

/// 便捷函数：用单个密码打开文件。
///
/// # 性能警告
///
/// 每次调用都会跑一次 Argon2id（默认参数约 180 ms）。**批量扫描时不要用它**——
/// 应先用 [`Kek::from_password`] 派生一次并缓存，再对每个文件调用 [`open`]。
///
/// # Errors
///
/// 同 [`open`]，另加 Argon2 派生失败。
pub fn open_with_password(data: &[u8], password: &[u8]) -> Result<OpenedFile> {
    let header = FixedHeader::parse(data)?;
    let params = Argon2Params {
        m_kib: header.argon2_m_kib,
        t: header.argon2_t,
        p: header.argon2_p,
    };
    let kek = Kek::from_password(password, &header.vault_salt, params)?;
    open(data, &[kek])
}

/// 固定头长度，便于调用方按需分批读取。
pub const PEEK_SIZE: usize = FIXED_HEADER_LEN;
