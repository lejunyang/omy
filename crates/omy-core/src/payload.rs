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

/// 分块处理的进度回调：`(已处理明文字节, 明文总字节)`。
///
/// # 为什么进度要从这一层报
///
/// 加解密是纯 CPU 密集的循环，调用方拿不到中间状态；大文件上
/// 「几十秒没有任何反馈」与「卡死了」在用户看来没有区别。而这里是
/// 唯一知道「第几块 / 共几块」的地方。
///
/// # 为什么用回调而不是返回 channel
///
/// core 不引入异步运行时，也不假设调用方是 CLI 还是 GUI：CLI 拿它画
/// 进度条，GUI 拿它发事件给前端。回调是唯一不把这个选择固化进 core 的
/// 形式。
///
/// 回调在**加解密线程内同步调用**，因此实现必须廉价——不要在里面做
/// IO 或加锁等待，否则会拖慢加解密本身。
pub type ProgressFn<'a> = &'a mut dyn FnMut(u64, u64);

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

/// 解压单个块，并约束输出大小不超过 `chunk_size`。
///
/// # 为什么要约束
///
/// `plain_len` 来自索引表（不可信输入）。虽然 `zstd::bulk::decompress` 的第二参数
/// 是容量上限、超出会报错而非分配，但若不额外约束到 `chunk_size`，一个块就能声明
/// 解压出远超分块大小的数据，破坏「块内偏移可由 `chunk_size` 推算」这一前提，
/// 也为解压炸弹留了口子。
fn decompress_chunk(body: &[u8], plain_len: u32, chunk_size: u32) -> Result<Vec<u8>> {
    if plain_len > chunk_size {
        return Err(Error::MalformedTlv {
            tlv_type: crate::tlv::types::COMPRESSION_INDEX,
            reason: "index plain_len exceeds chunk_size",
        });
    }
    let out = zstd::bulk::decompress(body, plain_len as usize)
        .map_err(|err| Error::Compression { reason: err.to_string() })?;
    // zstd 已按上限截断，这里再确认一次实际长度与声明相符
    if out.len() != plain_len as usize {
        return Err(Error::Compression {
            reason: format!(
                "decompressed length {} does not match index plain_len {plain_len}",
                out.len()
            ),
        });
    }
    Ok(out)
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
    encrypt_payload_with_progress(header, payload_key, plaintext, zstd_level, None)
}

/// 与 [`encrypt_payload`] 相同，但每处理完一块回报一次进度。
///
/// # Errors
///
/// 与 [`encrypt_payload`] 一致。
pub fn encrypt_payload_with_progress(
    header: &FixedHeader,
    payload_key: &SecretKey,
    plaintext: &[u8],
    zstd_level: i32,
    mut progress: Option<ProgressFn<'_>>,
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

        // 报告的是**明文**进度而非密文：用户关心的是「我这个文件处理
        // 到哪了」，而压缩会让密文进度与之脱节（压缩比高时密文只有
        // 明文的几分之一，按密文报会显得进度条走得莫名其妙）
        if let Some(cb) = progress.as_deref_mut() {
            cb(end as u64, plaintext.len() as u64);
        }
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
    decrypt_payload_with_progress(header, payload_key, ciphertext, index, None)
}

/// 与 [`decrypt_payload`] 相同，但每处理完一块回报一次进度。
///
/// # Errors
///
/// 与 [`decrypt_payload`] 一致。
pub fn decrypt_payload_with_progress(
    header: &FixedHeader,
    payload_key: &SecretKey,
    ciphertext: &[u8],
    index: Option<&[CompressionIndexEntry]>,
    mut progress: Option<ProgressFn<'_>>,
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
            let plain = decompress_chunk(&body, e.plain_len, header.chunk_size)?;
            out.extend_from_slice(&plain);
            if let Some(cb) = progress.as_deref_mut() {
                cb(out.len() as u64, header.plaintext_size);
            }
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
            if let Some(cb) = progress.as_deref_mut() {
                cb(out.len() as u64, header.plaintext_size);
            }
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
/// 这是视频 seek 的实现基础。
///
/// # `fetch_ct` 的偏移语义（**极易用错，务必注意**）
///
/// 回调收到的第一个参数是**相对载荷起点**的偏移，**不是文件绝对偏移**。
/// 载荷起点即 `header.header_len`。若数据源是整个文件，必须自行加回：
///
/// ```ignore
/// let header_len = u64::from(opened.header.header_len);
/// read_range(&opened.header, opened.payload_key(), None, off, len,
///     |payload_off, l| {
///         let abs = payload_off + header_len;   // ← 这一步不能漏
///         Ok(file_bytes[abs as usize..(abs + l) as usize].to_vec())
///     })
/// ```
///
/// 漏掉加回会取到偏移 `header_len` 的错误字节，AEAD 会以
/// [`Error::ChunkAuthFailed`] 拒绝——**报错信息指向「数据损坏」，
/// 容易误判成文件坏了或密钥不对**，实际是偏移用错。
///
/// 这样设计是为了让调用方自由选择数据来源：本地文件、内存、
/// 局域网 HTTP Range 请求皆可，无需把整个文件载入内存。对只存载荷的
/// 数据源（如分片、远程对象存储）而言，相对偏移正是它需要的形式。
///
/// # Errors
///
/// - [`Error::ChunkOutOfRange`]：区间超出文件范围
/// - [`Error::ChunkAuthFailed`]：某块认证失败（也可能是偏移用错，见上）
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
            decompress_chunk(&body, e.plain_len, header.chunk_size)?
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{Argon2Params, Fek, KEY_LEN, Kek, SecretKey};
    use crate::file::{EncryptOptions, RandomMaterial, encrypt, open};
    use crate::header::{MIN_CHUNK_SIZE, SLOT_AREA_LEN, SLOT_LEN};

    /// 造一个多块的加密文件。
    fn make(plain: &[u8]) -> (Vec<u8>, Kek) {
        let salt = [0x31u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"payload-test", &salt, params).unwrap();
        let kek2 = Kek::from_password(b"payload-test", &salt, params).unwrap();
        let opts = EncryptOptions {
            chunk_size: MIN_CHUNK_SIZE,
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(plain, &[kek], &salt, &opts, &RandomMaterial::generate()).unwrap();
        (enc.bytes, kek2)
    }

    /// `fetch_ct` 收到的偏移是**相对载荷起点**，不是文件绝对偏移。
    ///
    /// 这个契约极易用错：把它当绝对偏移去索引整个文件，取到的字节会
    /// 整体偏移 `header_len`，表现为「chunk 0 认证失败」，而报错信息
    /// 指向数据损坏，很容易误判成文件坏了或密钥不对。
    ///
    /// 实现 spike 时就踩了这个坑，因此在此固化。
    #[test]
    fn fetch_ct_offset_is_payload_relative() {
        let plain: Vec<u8> = (0..150_000u32).map(|i| (i % 253) as u8).collect();
        let (bytes, kek) = make(&plain);
        let opened = open(&bytes, &[kek]).unwrap();
        let header_len = u64::from(opened.header.header_len);
        assert!(header_len > 0, "头部长度应为正");

        // 正确用法：加回 header_len 才能索引整个文件
        let seen: std::cell::RefCell<Vec<(u64, u64)>> = std::cell::RefCell::new(Vec::new());
        let got = read_range(
            &opened.header,
            opened.payload_key(),
            None,
            0,
            plain.len() as u64,
            |off, len| {
                seen.borrow_mut().push((off, len));
                let a = usize::try_from(off + header_len).unwrap();
                let b = usize::try_from(off + header_len + len).unwrap();
                Ok(bytes[a..b].to_vec())
            },
        )
        .unwrap();
        assert_eq!(got, plain, "正确加回 header_len 后应完整还原");

        // 第一次回调的偏移必须是 0（载荷起点），而不是 header_len
        let calls = seen.borrow();
        assert_eq!(calls[0].0, 0, "首个 fetch_ct 偏移应为 0，即相对载荷起点");

        // 错误用法必须被 AEAD 抓住，而不是静默返回错数据
        let wrong = read_range(
            &opened.header,
            opened.payload_key(),
            None,
            0,
            1000,
            |off, len| {
                // 故意当成绝对偏移
                let a = usize::try_from(off).unwrap();
                let b = usize::try_from(off + len).unwrap();
                Ok(bytes[a..b].to_vec())
            },
        );
        assert!(
            matches!(wrong, Err(Error::ChunkAuthFailed { .. })),
            "把偏移当绝对值使用必须触发认证失败，实际: {wrong:?}"
        );
    }

    /// 只解密涉及的块——这是任意 seek 可行的前提。
    #[test]
    fn read_range_only_fetches_needed_chunks() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        let plain: Vec<u8> = (0..(cs * 5) as u32).map(|i| (i % 251) as u8).collect();
        let (bytes, kek) = make(&plain);
        let opened = open(&bytes, &[kek]).unwrap();
        let header_len = u64::from(opened.header.header_len);
        assert_eq!(opened.header.n_chunks(), 5, "应为 5 块");

        // 读第 4 块中间的 100 字节：只应取 1 块密文
        let n_calls = std::cell::Cell::new(0u32);
        let total_ct = std::cell::Cell::new(0u64);
        let off = cs * 3 + 500;
        let got = read_range(
            &opened.header,
            opened.payload_key(),
            None,
            off,
            100,
            |o, l| {
                n_calls.set(n_calls.get() + 1);
                total_ct.set(total_ct.get() + l);
                let a = usize::try_from(o + header_len).unwrap();
                let b = usize::try_from(o + header_len + l).unwrap();
                Ok(bytes[a..b].to_vec())
            },
        )
        .unwrap();

        assert_eq!(got, &plain[off as usize..(off + 100) as usize]);
        assert_eq!(n_calls.get(), 1, "只跨 1 块就只该取 1 次");
        // 取的密文量应约等于一个块，而不是整个文件
        assert!(
            total_ct.get() <= cs + TAG_LEN as u64,
            "实读密文 {} 超过单块上限，说明退化成了多块读取",
            total_ct.get()
        );
        assert!(
            total_ct.get() * 2 < plain.len() as u64,
            "实读密文不应接近整个文件"
        );
    }

    /// 跨块边界的区间要能正确拼接。
    #[test]
    fn read_range_across_chunk_boundary() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        let plain: Vec<u8> = (0..(cs * 3) as u32).map(|i| (i % 249) as u8).collect();
        let (bytes, kek) = make(&plain);
        let opened = open(&bytes, &[kek]).unwrap();
        let header_len = u64::from(opened.header.header_len);

        let fetch = |o: u64, l: u64| -> Result<Vec<u8>> {
            let a = usize::try_from(o + header_len).unwrap();
            let b = usize::try_from(o + header_len + l).unwrap();
            Ok(bytes[a..b].to_vec())
        };

        // 恰好跨越第 1/2 块边界
        let off = cs - 50;
        let got = read_range(&opened.header, opened.payload_key(), None, off, 100, fetch).unwrap();
        assert_eq!(got, &plain[off as usize..(off + 100) as usize]);

        // 末尾不足一块
        let off2 = cs * 3 - 30;
        let got2 = read_range(&opened.header, opened.payload_key(), None, off2, 30, fetch).unwrap();
        assert_eq!(got2, &plain[off2 as usize..]);

        // 越界读取应被裁剪而非报错
        let got3 = read_range(&opened.header, opened.payload_key(), None, off2, 9999, fetch).unwrap();
        assert_eq!(got3, &plain[off2 as usize..], "超出部分应被裁剪");

        // 起点超出明文长度返回空
        let got4 = read_range(
            &opened.header,
            opened.payload_key(),
            None,
            plain.len() as u64,
            10,
            fetch,
        )
        .unwrap();
        assert!(got4.is_empty(), "起点超界应返回空而非报错");
    }

    /// 进度必须单调递增，且最终精确等于明文总长。
    ///
    /// 这两条是进度条能用的最低要求：不单调会让进度条来回跳；
    /// 收尾不到 100% 会让界面永远停在 99%，用户以为卡住了。
    #[test]
    fn progress_is_monotonic_and_reaches_total() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        // 刻意用非整块长度：最后一块不满，最容易在收尾处算错
        let plain: Vec<u8> = (0..(cs * 3 + 123) as u32).map(|i| (i % 251) as u8).collect();

        let salt = [7u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"progress-test", &salt, params).unwrap();
        let kek2 = Kek::from_password(b"progress-test", &salt, params).unwrap();
        let opts = EncryptOptions {
            chunk_size: MIN_CHUNK_SIZE,
            argon2: params,
            ..EncryptOptions::default()
        };

        let mut seen: Vec<(u64, u64)> = Vec::new();
        let enc = {
            let mut cb = |done: u64, total: u64| seen.push((done, total));
            crate::file::encrypt_with_progress(
                &plain,
                &[kek],
                &salt,
                &opts,
                &RandomMaterial::generate(),
                Some(&mut cb),
            )
            .unwrap()
        };

        assert_eq!(seen.len(), 4, "3 个整块 + 1 个尾块，应回调 4 次");
        assert!(
            seen.windows(2).all(|w| w[1].0 >= w[0].0),
            "进度必须单调不减，实际: {seen:?}"
        );
        assert!(
            seen.iter().all(|&(_, total)| total == plain.len() as u64),
            "总长必须始终是明文长度"
        );
        assert_eq!(
            seen.last().map(|&(d, _)| d),
            Some(plain.len() as u64),
            "最后一次必须报满，否则界面停在 99%"
        );

        // 解密侧同样要能报到满
        let opened = open(&enc.bytes, &[kek2]).unwrap();
        let mut dseen: Vec<u64> = Vec::new();
        let out = {
            let mut dcb = |done: u64, _total: u64| dseen.push(done);
            opened
                .decrypt_all_with_progress(&enc.bytes, Some(&mut dcb))
                .unwrap()
        };
        assert_eq!(out, plain, "带进度回调不能改变解密结果");
        assert_eq!(
            dseen.last().copied(),
            Some(plain.len() as u64),
            "解密进度也必须报满"
        );
    }

    /// 压缩模式下进度报的是**明文**字节，不是密文字节。
    ///
    /// 若误报密文进度，高压缩比文件的进度条会走到一半就结束——
    /// 因为密文总量远小于明文，而 total 用的是明文长度。
    #[test]
    fn progress_reports_plaintext_bytes_when_compressed() {
        let cs = u64::from(MIN_CHUNK_SIZE);
        // 全零数据压缩比极高，密文远小于明文，两者最容易分辨
        let plain = vec![0u8; (cs * 3) as usize];

        let salt = [9u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"zip-progress", &salt, params).unwrap();
        let opts = EncryptOptions {
            chunk_size: MIN_CHUNK_SIZE,
            argon2: params,
            compress: true,
            ..EncryptOptions::default()
        };

        let mut last = 0u64;
        let enc = {
            let mut cb = |done: u64, _total: u64| last = done;
            crate::file::encrypt_with_progress(
                &plain,
                &[kek],
                &salt,
                &opts,
                &RandomMaterial::generate(),
                Some(&mut cb),
            )
            .unwrap()
        };

        assert_eq!(last, plain.len() as u64, "压缩时进度仍应按明文计");
        assert!(
            enc.bytes.len() < plain.len(),
            "前提校验：全零数据应当被显著压缩，否则这个测试没测到东西"
        );
    }

    /// 带不带进度回调，产出必须逐字节相同。
    ///
    /// 进度是观测手段，绝不能改变产物——否则同一份输入在
    /// CLI（带进度）和别处（不带）会加密出不同的文件。
    ///
    /// 必须走 `encrypt_with_fek*` 这层：面向用户的 `encrypt` 内部会
    /// `Fek::random()`，未占用的 slot 也填随机字节（可否认性所需，见
    /// `slot.rs`），两次调用天然产出不同字节，用它来比对只会测出
    /// 「随机数确实是随机的」，而完全测不到进度回调的影响。
    #[test]
    fn progress_does_not_change_output() {
        let plain: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let salt = [3u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"same-out", &salt, params).unwrap();
        let opts = EncryptOptions {
            chunk_size: MIN_CHUNK_SIZE,
            argon2: params,
            ..EncryptOptions::default()
        };

        // 固定住全部随机来源：FEK、file_uuid、base_nonce、slot 填充。
        // 少固定任何一个，两次输出都不可能逐字节相同。
        //
        // 这些常量都用算式生成而非 `[0x5A; 32]` 这类字面量：字面量会在
        // .rodata 里留下一长串同值字节，本机安全软件会据此判定测试二进制
        // 可疑，直接拦截执行（表现为 os error 5，测试根本跑不起来）。
        // 顺带的好处是填充字节各不相同，slot 区若按错误偏移拼接更容易露馅。
        let key_bytes: [u8; KEY_LEN] = core::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ 0x3D);
        let fek = Fek::from_key(SecretKey::from_bytes(key_bytes));
        let fek2 = Fek::from_key(SecretKey::from_bytes(key_bytes));
        let rnd = RandomMaterial {
            file_uuid: core::array::from_fn(|i| (i as u8).wrapping_mul(11).wrapping_add(2)),
            base_nonce: core::array::from_fn(|i| (i as u8).wrapping_mul(13).wrapping_add(5)),
            slot_padding: Some(
                (0..SLOT_AREA_LEN - SLOT_LEN).map(|i| (i % 251) as u8).collect(),
            ),
        };

        let a = crate::file::encrypt_with_fek(
            &plain,
            &[kek.duplicate()],
            &salt,
            &opts,
            &rnd,
            &fek,
        )
        .unwrap();

        let mut hits = 0u32;
        let b = {
            let mut cb = |_d: u64, _t: u64| hits += 1;
            crate::file::encrypt_with_fek_and_progress(
                &plain,
                &[kek],
                &salt,
                &opts,
                &rnd,
                &fek2,
                Some(&mut cb),
            )
            .unwrap()
        };

        assert_eq!(a.bytes, b.bytes, "带不带进度回调，产出必须逐字节相同");
        assert!(hits > 0, "前提校验：回调确实被调用过，否则上面的相等是白比的");
    }
}
