//! 载荷层：分块 AEAD 加解密、随机访问、zstd 压缩。
//!
//! 每块**独立**加密，这是三项能力的共同基础（规范 §5）：
//!
//! - **视频任意 seek**：未压缩时 `ct_offset(i)` 有闭式公式，无需索引表
//! - **缺片降级播放**：缺失的片只影响它覆盖的块，其余块仍可解密
//! - **有界内存**：不必把整个文件读入内存
//!
//! # 防篡改的三重绑定
//!
//! | 机制 | 防御的攻击 |
//! |---|---|
//! | nonce 含 `final_flag` | 截断文件尾部 |
//! | AAD 含 `file_uuid` | 跨文件移植块 |
//! | AAD 含块序号 | 重排块顺序 |

use crate::crypto::{CipherId, SecretKey, TAG_LEN, chunk_aad, chunk_nonce};
use crate::error::{Error, Result};
use crate::header::{COMPRESS_ZSTD, FixedHeader, flags};
use crate::tlv::CompressionIndexEntry;

/// 加密单个明文块。
///
/// `index` 是块序号，`is_final` 标记是否为最后一块（参与 nonce 构造，防截断）。
///
/// # Errors
///
/// AEAD 加密失败时返回错误。
pub fn encrypt_chunk(
    payload_key: &SecretKey,
    cipher: CipherId,
    base_nonce: &[u8; 7],
    file_uuid: &[u8; 16],
    index: u32,
    is_final: bool,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let nonce = chunk_nonce(base_nonce, index, is_final);
    let aad = chunk_aad(file_uuid, index);
    cipher.encrypt(payload_key, &nonce, plaintext, &aad)
}

/// 解密单个密文块。
///
/// # Errors
///
/// [`Error::ChunkAuthFailed`]：认证失败。可能原因是数据损坏、被篡改、
/// 块序号错误，或文件被截断（`is_final` 不匹配）。
pub fn decrypt_chunk(
    payload_key: &SecretKey,
    cipher: CipherId,
    base_nonce: &[u8; 7],
    file_uuid: &[u8; 16],
    index: u32,
    is_final: bool,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    let nonce = chunk_nonce(base_nonce, index, is_final);
    let aad = chunk_aad(file_uuid, index);
    cipher
        .decrypt(payload_key, &nonce, ciphertext, &aad)?
        .ok_or(Error::ChunkAuthFailed { index: u64::from(index) })
}

/// 分块加密整个明文。
///
/// 返回 `(密文, 压缩索引)`。仅当启用压缩时索引非空——未压缩时偏移由公式算出，
/// 存索引纯属浪费。
///
/// # Errors
///
/// - [`Error::MalformedHeader`]：块数超过 `u32` 上限
/// - [`Error::Compression`]：zstd 压缩失败
pub fn encrypt_payload(
    header: &FixedHeader,
    payload_key: &SecretKey,
    plaintext: &[u8],
    zstd_level: i32,
) -> Result<(Vec<u8>, Vec<CompressionIndexEntry>)> {
    let compressed = header.compress_id == COMPRESS_ZSTD;
    let chunk_size = header.chunk_size as usize;
    let n_chunks = header.n_chunks();

    let n_chunks_u32 = u32::try_from(n_chunks)
        .map_err(|_| Error::MalformedHeader { reason: "chunk count exceeds u32" })?;

    let mut out = Vec::new();
    let mut index = Vec::new();

    for i in 0..n_chunks_u32 {
        let start = (i as usize).saturating_mul(chunk_size);
        let end = start.saturating_add(chunk_size).min(plaintext.len());
        // 空文件时 start=end=0，切片为空，仍会产出 1 个块（只含 tag）
        let raw = plaintext.get(start..end).unwrap_or(&[]);
        let is_final = i.saturating_add(1) == n_chunks_u32;

        let body = if compressed {
            zstd::bulk::compress(raw, zstd_level)
                .map_err(|e| Error::Compression { reason: e.to_string() })?
        } else {
            raw.to_vec()
        };

        let ct = encrypt_chunk(
            payload_key,
            header.cipher_id,
            &header.base_nonce,
            &header.file_uuid,
            i,
            is_final,
            &body,
        )?;

        if compressed {
            index.push(CompressionIndexEntry {
                ct_offset: out.len() as u64,
                ct_len: u32::try_from(ct.len()).map_err(|_| Error::MalformedHeader {
                    reason: "chunk ciphertext exceeds u32",
                })?,
                plain_len: u32::try_from(raw.len()).map_err(|_| Error::MalformedHeader {
                    reason: "chunk plaintext exceeds u32",
                })?,
            });
        }
        out.extend_from_slice(&ct);
    }

    Ok((out, index))
}

/// 解密整个载荷。
///
/// `index` 在压缩模式下必须提供（规范要求压缩文件必带 CRITICAL 索引 TLV）。
///
/// # Errors
///
/// - [`Error::MissingTlv`]：压缩模式但未提供索引
/// - [`Error::ChunkAuthFailed`]：某块认证失败
/// - [`Error::Compression`]：解压失败
/// - [`Error::MalformedHeader`]：解密结果长度与 `plaintext_size` 不符
pub fn decrypt_payload(
    header: &FixedHeader,
    payload_key: &SecretKey,
    ciphertext: &[u8],
    index: Option<&[CompressionIndexEntry]>,
) -> Result<Vec<u8>> {
    let compressed = header.has_flag(flags::COMPRESSED);
    let n_chunks = header.n_chunks();
    let n_chunks_u32 = u32::try_from(n_chunks)
        .map_err(|_| Error::MalformedHeader { reason: "chunk count exceeds u32" })?;

    // 预分配容量以避免反复扩容。32 位平台上 plaintext_size 可能超过 usize 上限，
    // 此时退化为不预分配（后续切片操作会给出明确的 Truncated 错误）。
    let mut out = usize::try_from(header.plaintext_size)
        .map_or_else(|_| Vec::new(), Vec::with_capacity);

    if compressed {
        let idx = index.ok_or(Error::MissingTlv {
            tlv_type: crate::tlv::types::COMPRESSION_INDEX,
        })?;
        if idx.len() as u64 != n_chunks {
            return Err(Error::MalformedTlv {
                tlv_type: crate::tlv::types::COMPRESSION_INDEX,
                reason: "index entry count does not match chunk count",
            });
        }
        for (i, e) in idx.iter().enumerate() {
            let i_u32 = u32::try_from(i)
                .map_err(|_| Error::MalformedHeader { reason: "chunk index exceeds u32" })?;
            let start = usize::try_from(e.ct_offset)
                .map_err(|_| Error::MalformedTlv {
                    tlv_type: crate::tlv::types::COMPRESSION_INDEX,
                    reason: "ct_offset exceeds addressable range",
                })?;
            let end = start.saturating_add(e.ct_len as usize);
            let ct = ciphertext.get(start..end).ok_or(Error::Truncated {
                context: "compressed chunk",
                need: end,
                got: ciphertext.len(),
            })?;
            let is_final = i_u32.saturating_add(1) == n_chunks_u32;

            let body = decrypt_chunk(
                payload_key,
                header.cipher_id,
                &header.base_nonce,
                &header.file_uuid,
                i_u32,
                is_final,
                ct,
            )?;
            let plain = zstd::bulk::decompress(&body, e.plain_len as usize)
                .map_err(|err| Error::Compression { reason: err.to_string() })?;
            out.extend_from_slice(&plain);
        }
    } else {
        for i in 0..n_chunks_u32 {
            let (off, len) = header.chunk_ct_range(u64::from(i))?;
            // 载荷切片的偏移是相对文件起始的，需减去 header_len
            let rel = off
                .checked_sub(u64::from(header.header_len))
                .ok_or(Error::MalformedHeader { reason: "chunk offset precedes payload" })?;
            let start = usize::try_from(rel).map_err(|_| Error::MalformedHeader {
                reason: "chunk offset exceeds addressable range",
            })?;
            let end = start.saturating_add(usize::try_from(len).map_err(|_| {
                Error::MalformedHeader { reason: "chunk length exceeds addressable range" }
            })?);
            let ct = ciphertext.get(start..end).ok_or(Error::Truncated {
                context: "payload chunk",
                need: end,
                got: ciphertext.len(),
            })?;
            let is_final = i.saturating_add(1) == n_chunks_u32;

            let plain = decrypt_chunk(
                payload_key,
                header.cipher_id,
                &header.base_nonce,
                &header.file_uuid,
                i,
                is_final,
                ct,
            )?;
            out.extend_from_slice(&plain);
        }
    }

    if out.len() as u64 != header.plaintext_size {
        return Err(Error::MalformedHeader {
            reason: "decrypted length does not match plaintext_size",
        });
    }
    Ok(out)
}

/// 读取明文的任意区间，只解密必要的块。
///
/// 这是视频 seek 的实现基础。`fetch_ct` 回调按 `(相对载荷的偏移, 长度)` 提供密文，
/// 使调用方能自由选择数据来源——本地文件、内存、局域网 HTTP Range 请求皆可，
/// 而无需把整个文件载入内存。
///
/// # Errors
///
/// - [`Error::ChunkOutOfRange`]：区间超出文件范围
/// - [`Error::ChunkAuthFailed`]：某块认证失败
/// - 回调返回的错误会向上传播
pub fn read_range<F>(
    header: &FixedHeader,
    payload_key: &SecretKey,
    index: Option<&[CompressionIndexEntry]>,
    offset: u64,
    length: u64,
    mut fetch_ct: F,
) -> Result<Vec<u8>>
where
    F: FnMut(u64, u64) -> Result<Vec<u8>>,
{
    if length == 0 {
        return Ok(Vec::new());
    }
    let end = offset
        .checked_add(length)
        .ok_or(Error::MalformedHeader { reason: "range end overflows" })?
        .min(header.plaintext_size);
    if offset >= header.plaintext_size {
        return Ok(Vec::new());
    }

    let n_chunks = header.n_chunks();
    let compressed = header.has_flag(flags::COMPRESSED);
    let cs = u64::from(header.chunk_size);

    let first = header.chunk_index_of(offset);
    let last = header.chunk_index_of(end.saturating_sub(1));

    let mut acc = Vec::with_capacity(usize::try_from(end.saturating_sub(offset)).unwrap_or(0));

    for i in first..=last {
        if i >= n_chunks {
            return Err(Error::ChunkOutOfRange { index: i, total: n_chunks });
        }
        let i_u32 =
            u32::try_from(i).map_err(|_| Error::MalformedHeader { reason: "chunk index exceeds u32" })?;
        let is_final = i.saturating_add(1) == n_chunks;

        let plain = if compressed {
            let idx = index.ok_or(Error::MissingTlv {
                tlv_type: crate::tlv::types::COMPRESSION_INDEX,
            })?;
            let e = idx.get(usize::try_from(i).unwrap_or(usize::MAX)).ok_or(
                Error::MalformedTlv {
                    tlv_type: crate::tlv::types::COMPRESSION_INDEX,
                    reason: "index shorter than chunk count",
                },
            )?;
            let ct = fetch_ct(e.ct_offset, u64::from(e.ct_len))?;
            let body = decrypt_chunk(
                payload_key,
                header.cipher_id,
                &header.base_nonce,
                &header.file_uuid,
                i_u32,
                is_final,
                &ct,
            )?;
            zstd::bulk::decompress(&body, e.plain_len as usize)
                .map_err(|err| Error::Compression { reason: err.to_string() })?
        } else {
            let (abs_off, ct_len) = header.chunk_ct_range(i)?;
            let rel = abs_off
                .checked_sub(u64::from(header.header_len))
                .ok_or(Error::MalformedHeader { reason: "chunk offset precedes payload" })?;
            let ct = fetch_ct(rel, ct_len)?;
            decrypt_chunk(
                payload_key,
                header.cipher_id,
                &header.base_nonce,
                &header.file_uuid,
                i_u32,
                is_final,
                &ct,
            )?
        };

        // 裁剪出本块与请求区间的交集
        let chunk_start = i.saturating_mul(cs);
        let want_from = offset.saturating_sub(chunk_start);
        let want_to = end.saturating_sub(chunk_start).min(plain.len() as u64);
        if want_from < want_to {
            let a = usize::try_from(want_from).unwrap_or(0);
            let b = usize::try_from(want_to).unwrap_or(plain.len());
            if let Some(s) = plain.get(a..b) {
                acc.extend_from_slice(s);
            }
        }
    }
    Ok(acc)
}

/// 未压缩载荷的总密文长度。
///
/// 每块附加一个 [`TAG_LEN`] 字节的认证标签。
#[must_use]
pub fn payload_ct_size(header: &FixedHeader) -> u64 {
    let n = header.n_chunks();
    header
        .plaintext_size
        .saturating_add(n.saturating_mul(TAG_LEN as u64))
}
