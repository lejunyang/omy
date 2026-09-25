//! 真正的密钥轮换：换掉 FEK 并重新加密载荷。
//!
//! # 与 [`crate::keyslot`] 的分工
//!
//! [`keyslot::rewrite_slots`](crate::keyslot::rewrite_slots) 改的是「谁能打开
//! 这个文件」，FEK 与载荷密文一字节不变。这有一个用户很容易误解的后果：
//! **移除一个密码只影响这一份文件**。攻击者若留有旧副本，仍能用被移除的密码
//! 打开那个副本——因为那份副本的 slot 区还是旧的，里面包裹的还是同一个 FEK。
//!
//! 本模块解决的正是这件事：换一个全新的 FEK、全新的 `file_uuid` 与
//! `base_nonce`，把载荷整个重新加密一遍。此后旧密码不但打不开这个文件，
//! 旧密码派生出的 KEK 也不再对应任何有效的 FEK。
//!
//! # 代价必须如实告知
//!
//! 成本与文件大小成正比（文档 03 §4.3），不是毫秒级：
//!
//! - 全部密文字节都变了。对外部观察者是一个全新文件，增量备份要重传整份，
//!   跨文件去重失效；
//! - 大文件是数分钟到数小时的读写。
//!
//! 所以这**不能**做成密码管理的默认行为，只能是用户明确选择的另一个操作。
//!
//! # 它仍然挡不住什么
//!
//! 轮换只保证「今后拿到这一份文件的人」用旧密码打不开。已经流出去的旧副本
//! 是一份独立的密文，它的 FEK 早已被旧密码包裹好，本操作对它无能为力。
//! 唯一诚实的表述是：轮换让这份文件从此与旧密码无关，**但不能收回已经泄露
//! 的副本**。UI 不能把它说成「彻底作废旧密码」。

use crate::crypto::{Fek, Kek, header_mac};
use crate::error::{Error, Result};
use crate::file::{EncryptOptions, EncryptedFile, RandomMaterial};
use crate::header::{FixedHeader, HEADER_MAC_LEN, SLOT_AREA_LEN, SLOT_AREA_OFFSET, flags};
use crate::payload::ProgressFn;
use crate::tlv::types;

/// 轮换的结果。
#[derive(Debug)]
pub struct RotateOutcome {
    /// 重新加密后的完整文件字节。
    pub bytes: Vec<u8>,
    /// 解开原文件时命中的 slot 下标。
    pub opened_slot: u16,
    /// 轮换后占用的 slot 数量。
    pub slot_used: usize,
    /// 明文字节数，供调用方展示「重写了多少数据」。
    pub plaintext_size: u64,
}

/// 外部编辑后需要随新内容重建的媒体附加信息。
///
/// 三个字段使用 `Option` 表示“新文件没有该信息”，而不是“沿用旧值”。
/// 外部编辑可能把视频改成文本；沿用旧缩略图或 moov 会让头部描述另一份内容。
#[derive(Debug, Clone, Default)]
pub struct ReplacementMetadata {
    /// 新内容的缩略图。
    pub thumbnail: Option<Vec<u8>>,
    /// 新内容的媒体元信息。
    pub media_meta: Option<Vec<u8>>,
    /// 新内容的 MP4 moov 副本。
    pub moov_cache: Option<Vec<u8>>,
}

/// 用新明文替换单文件载荷，同时逐字节保留原槽区。
///
/// 这是外部编辑回写用的格式操作。它与 [`rotate_fek`] 的目标相反：轮换要让旧
/// FEK/旧密码失效，这里必须让原有密码、设备密钥、恢复码全部继续有效。因此保留
/// `file_uuid`、FEK 和完整 slot area，只生成新的 `base_nonce` 并重建载荷、内容
/// 哈希及媒体 TLV。新 nonce 是硬约束；同一 `(FEK, file_uuid)` 下复用旧 nonce
/// 加密不同明文会破坏 AEAD 安全性。
///
/// 目录容器不能走这条路径：第三方编辑器拿到的是一个普通文件，无法表达容器索引
/// 与多文件载荷的同步修改。
///
/// # Errors
///
/// 原文件打不开、是目录容器、槽区或头部截断，以及重新加密失败时返回错误。
pub fn replace_plaintext_preserving_slots(
    data: &[u8],
    unlock: &[Kek],
    plaintext: &[u8],
    metadata: &ReplacementMetadata,
    new_base_nonce: [u8; 7],
) -> Result<EncryptedFile> {
    let opened = crate::file::open(data, unlock)?;
    if opened.is_container() {
        return Err(Error::MalformedHeader {
            reason: "container payload cannot be replaced as a single file",
        });
    }
    let old = FixedHeader::parse(data)?;
    let slot_end = SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN);
    let old_slots = data
        .get(SLOT_AREA_OFFSET..slot_end)
        .ok_or(Error::Truncated {
            context: "key slot area",
            need: slot_end,
            got: data.len(),
        })?;

    let mut opts = rebuild_options(&opened, &old, 1)?;
    // FEK 没变，槽位目录也必须逐字节保留原类型；不能像轮换那样按 keep 重建。
    opts.slot_directory = if opened.is_slot_managed() {
        Some(opened.raw_slot_directory()?)
    } else {
        None
    };
    opts.thumbnail = metadata.thumbnail.clone();
    opts.media_meta = metadata.media_meta.clone();
    opts.moov_cache = metadata.moov_cache.clone();

    // encrypt_with_fek 需要一个 KEK 来造临时槽区；该槽区马上会被原槽区覆盖。
    // 使用实际命中的 KEK，避免传入一个不相关候选使中间产物不可自检。
    let keeper = unlock
        .get(opened.kek_index)
        .ok_or(Error::NoMatchingSlot)?
        .duplicate();
    let rnd = RandomMaterial {
        file_uuid: old.file_uuid,
        base_nonce: new_base_nonce,
        slot_padding: None,
    };
    let mut out = crate::file::encrypt_with_fek(
        plaintext,
        &[keeper],
        &old.vault_salt,
        &opts,
        &rnd,
        opened.fek(),
    )?;

    let new_header = FixedHeader::parse(&out.bytes)?;
    let new_slot_end = SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN);
    let output_len = out.bytes.len();
    let dst = out
        .bytes
        .get_mut(SLOT_AREA_OFFSET..new_slot_end)
        .ok_or(Error::Truncated {
            context: "replacement key slot area",
            need: new_slot_end,
            got: output_len,
        })?;
    dst.copy_from_slice(old_slots);

    // 槽区属于 header MAC 覆盖范围，换回原字节后必须重算。MAC 本身位于
    // header_len 末尾，covered 恰好是它之前的全部固定头、槽区与 TLV。
    let mac_pos = (new_header.header_len as usize).saturating_sub(HEADER_MAC_LEN);
    let covered = out.bytes.get(..mac_pos).ok_or(Error::Truncated {
        context: "replacement header",
        need: mac_pos,
        got: out.bytes.len(),
    })?;
    let mac_key = opened.fek().derive_header_mac_key(&old.file_uuid);
    let mac = header_mac(&mac_key, covered);
    let total_len = out.bytes.len();
    let mac_dst = out
        .bytes
        .get_mut(mac_pos..new_header.header_len as usize)
        .ok_or(Error::Truncated {
            context: "replacement header mac",
            need: new_header.header_len as usize,
            got: total_len,
        })?;
    mac_dst.copy_from_slice(&mac);
    out.header = new_header;
    Ok(out)
}

/// 换掉 FEK 并重新加密整个载荷。
///
/// `unlock` 用于解开现有文件（其中一个命中即可）；`keep` 是轮换后应当能打开
/// 该文件的全部 KEK。`rnd` 提供新的 `file_uuid` 与 `base_nonce`——**必须是
/// 新生成的**，复用旧值会让 nonce 在两份不同密文间重复。
///
/// # 保留了什么
///
/// 文件名、明文后缀、缩略图、媒体元信息、`moov` 缓存、容器索引、压缩设置、
/// 分块大小、AEAD 算法、Argon2 参数全部按原样重建，所以轮换前后**打开后看到
/// 的东西完全一样**，只有密钥和密文变了。
///
/// 这里逐项搬运而不是走 `EncryptOptions::default()`：漏掉任何一项都会静默
/// 丢数据——丢了容器索引，一个加密文件夹会变成一堆拼接的字节；丢了 Argon2
/// 参数，头部记录的参数与派生 KEK 用的参数不一致，文件当场打不开。
///
/// # Errors
///
/// - [`Error::NoMatchingSlot`]：`unlock` 里没有能解开该文件的 KEK
/// - [`Error::HeaderMacMismatch`]：原文件头部已被篡改
/// - [`Error::ContentHashMismatch`]：原文件载荷已损坏
/// - [`Error::MalformedHeader`]：`keep` 为空
/// - [`Error::TooManySlots`]：`keep` 超过 8 个
pub fn rotate_fek(
    data: &[u8],
    unlock: &[Kek],
    keep: &[Kek],
    rnd: &RandomMaterial,
) -> Result<RotateOutcome> {
    rotate_fek_with_progress(data, unlock, keep, rnd, None, None)
}

/// 与 [`rotate_fek`] 相同，但分别回报解密与加密两个阶段的进度。
///
/// 分成两个回调而不是合成一条总进度：轮换要把载荷**读一遍再写一遍**，
/// 合成一条会让进度条在中点莫名减速，用户以为卡住了。
///
/// # Errors
///
/// 与 [`rotate_fek`] 一致。
pub fn rotate_fek_with_progress(
    data: &[u8],
    unlock: &[Kek],
    keep: &[Kek],
    rnd: &RandomMaterial,
    decrypting: Option<ProgressFn<'_>>,
    encrypting: Option<ProgressFn<'_>>,
) -> Result<RotateOutcome> {
    if keep.is_empty() {
        return Err(Error::MalformedHeader {
            reason: "refusing to leave a file with zero key slots; it could never be opened again",
        });
    }

    // 完整 open：验头部 MAC。跳过的话会把篡改后的字段连同新 MAC 一起签进去。
    let opened = crate::file::open(data, unlock)?;
    let header = FixedHeader::parse(data)?;

    // decrypt_all 会在文件写有内容哈希时自动校验。这一步必须在换 FEK 之前
    // 做：若原文件载荷已经坏了，重新加密只会把损坏的明文用新密钥固定下来，
    // 而且旧密文被覆盖后再也无从追查。
    let plain = opened.decrypt_all_with_progress(data, decrypting)?;

    let opts = rebuild_options(&opened, &header, keep.len())?;

    // 新 FEK。file_uuid 与 base_nonce 由调用方通过 rnd 传入新值——
    // 载荷密钥是 HKDF(FEK, file_uuid)，两者都换才能保证新旧密文之间
    // 不共用任何 (key, nonce)
    let fek = Fek::random();
    let enc: EncryptedFile = crate::file::encrypt_with_fek_and_progress(
        &plain,
        keep,
        &header.vault_salt,
        &opts,
        rnd,
        &fek,
        encrypting,
    )?;

    Ok(RotateOutcome {
        bytes: enc.bytes,
        opened_slot: opened.slot_index,
        slot_used: keep.len(),
        plaintext_size: plain.len() as u64,
    })
}

/// 从已打开的文件里把加密选项原样重建出来。
///
/// # 为什么不能只挑「重要的几项」
///
/// 每一项漏掉都是静默的数据丢失，而且症状离原因很远：
///
/// - 丢 `folder_index`：加密文件夹变成一堆拼接字节，且 `CONTAINER` flag 没了，
///   解密方不会报错，只会解出一个内容诡异的「文件」；
/// - 丢 `argon2`：头部记录的参数与 KEK 实际派生用的参数不一致，文件当场
///   打不开（`encrypt.rs` 里有同一个坑的记录）；
/// - 丢 `chunk_size` / `cipher`：能打开，但与原文件的可比性没了，
///   分片、远程播放的偏移假设也随之失效。
fn rebuild_options(
    opened: &crate::file::OpenedFile,
    header: &FixedHeader,
    kept: usize,
) -> Result<EncryptOptions> {
    // 文件名：有 FILENAME TLV 才取。注意不能用「解不出来就算没有」——
    // 解不出来说明文件坏了或密钥不对，那时应该报错而不是悄悄丢掉文件名
    let filename = if header.has_flag(flags::FILENAME_ENCRYPTED) {
        Some(opened.filename()?)
    } else {
        None
    };

    let thumbnail = if opened.has_thumbnail() {
        Some(opened.thumbnail()?)
    } else {
        None
    };
    let media_meta = if opened.has_media_meta() {
        Some(opened.media_meta()?)
    } else {
        None
    };
    let moov_cache = if opened.has_moov_cache() {
        Some(opened.moov_cache()?)
    } else {
        None
    };
    let folder_index = if opened.is_container() {
        // 取解密后的原始字节而不是 parse 再 encode：后者会把索引重新
        // 序列化一遍，任何编码差异都会变成「轮换后容器打不开」
        Some(opened.raw_folder_index()?)
    } else {
        None
    };

    // 槽位目录要**按 keep 重建**，不能原样搬。
    //
    // 轮换换掉了 FEK，旧槽全部作废，能打开新文件的只有 keep 里那些。
    // 原样搬过去会让目录说「恢复码还在 slot 2」，而它其实已经废了——
    // 用户据此不再另存恢复码，等真忘密码那天才发现兜底早没了。实测过。
    //
    // 不带目录也不行：那会让文件从可管理模式静默退回可否认模式，
    // 用户收不到提示，只在下次想精确删协作者时发现能力没了。
    //
    // 顺序与 build_area 一致：前 n 个槽对应 keep[0..n]。类型一律记成
    // Vault——这一层拿不到「哪个是恢复码」的信息，调用方（key recovery
    // 等）若需要更准的类型，应在轮换后自己改写目录。宁可少说一点，
    // 也不能凭猜写一个可能是错的类型
    let slot_directory = if opened.is_slot_managed() {
        let mut dir = crate::slotdir::SlotDirectory::new();
        for i in 0..kept.min(crate::header::SLOT_COUNT) {
            dir.set(
                i,
                crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault),
            )?;
        }
        Some(dir.encode())
    } else {
        None
    };

    Ok(EncryptOptions {
        filename,
        slot_directory,
        preserve_extension: header.has_flag(flags::EXT_PRESERVED),
        compress: header.has_flag(flags::COMPRESSED),
        // 压缩级别无法从文件里读出来（格式不记录它，只记录压缩后的块索引）。
        // 用默认值：级别只影响体积，不影响可解密性与还原结果
        zstd_level: EncryptOptions::default().zstd_level,
        chunk_size: header.chunk_size,
        cipher: header.cipher_id,
        argon2: header.argon2_params(),
        // 原文件写了哈希就继续写。不能一律写：那会让「原本没有自检」的
        // 文件在轮换后多出一个 TLV，破坏「除密钥外内容等价」
        write_content_hash: opened.tlvs.find(types::CONTENT_HASH).is_some(),
        thumbnail,
        media_meta,
        moov_cache,
        folder_index,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::*;
    use crate::container::EntryMeta;
    use crate::crypto::Argon2Params;
    use crate::file::{RandomMaterial, encrypt};

    fn kek(pw: &str, salt: &[u8; 16]) -> Kek {
        Kek::from_password(pw.as_bytes(), salt, Argon2Params::TEST_WEAK).unwrap()
    }

    /// 造一个带尽量多元数据的文件，用来验证轮换不丢东西。
    fn sample(salt: &[u8; 16], pw: &[&str], plain: &[u8]) -> Vec<u8> {
        let keks: Vec<Kek> = pw.iter().map(|p| kek(p, salt)).collect();
        let opts = EncryptOptions {
            filename: Some(String::from("报告.txt")),
            preserve_extension: true,
            argon2: Argon2Params::TEST_WEAK,
            // 各不相同的字节：若按错误偏移拼接更容易被测出来
            thumbnail: Some((0..64u32).map(|i| (i % 251) as u8).collect()),
            media_meta: Some(br#"{"w":1920}"#.to_vec()),
            ..EncryptOptions::default()
        };
        encrypt(plain, &keks, salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes
    }

    #[test]
    fn rotated_file_opens_with_kept_password_and_returns_same_plaintext() {
        // 不这样会怎样：轮换若把明文写错（例如漏了解压、或用错 payload key），
        // 文件仍然「能打开」，但内容是垃圾——只断言能打开抓不到
        let salt = [0x31u8; 16];
        let plain = b"rotate me".repeat(500);
        let data = sample(&salt, &["pw-old"], &plain);

        let out = rotate_fek(
            &data,
            &[kek("pw-old", &salt)],
            &[kek("pw-new", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();

        let re = crate::file::open(&out.bytes, &[kek("pw-new", &salt)]).unwrap();
        assert_eq!(
            re.decrypt_all(&out.bytes).unwrap(),
            plain,
            "轮换后必须还原出原明文"
        );
        assert_eq!(out.plaintext_size, plain.len() as u64);
    }

    #[test]
    fn rotation_replaces_fek_and_uuid_so_old_ciphertext_is_gone() {
        let salt = [0x32u8; 16];
        let data = sample(&salt, &["pw-old"], b"payload bytes here");

        let before = FixedHeader::parse(&data).unwrap();
        let out = rotate_fek(
            &data,
            &[kek("pw-old", &salt)],
            &[kek("pw-old", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();
        let after = FixedHeader::parse(&out.bytes).unwrap();

        assert_ne!(before.file_uuid, after.file_uuid, "必须换 file_uuid");
        assert_ne!(before.base_nonce, after.base_nonce, "必须换 base_nonce");
        assert_ne!(
            data.get(before.header_len as usize..),
            out.bytes.get(before.header_len as usize..),
            "载荷密文必须全部改变，否则等于没轮换"
        );
    }

    #[test]
    fn old_fek_cannot_derive_new_payload_key() {
        // 这条才是轮换与 keyslot 改写的**实质区别**，也是整个模块的意义。
        //
        // 不这样会怎样：上面那条测试的三个断言全部由调用方传入的 rnd 决定
        // ——即使实现复用旧 FEK，file_uuid / base_nonce / 密文照样都会变
        // （载荷密钥是 HKDF(FEK, file_uuid)，uuid 换了密文自然全变）。变异
        // 测试确认「复用旧 FEK」这个缺陷能从那条测试下存活。
        //
        // 而复用旧 FEK 是真漏洞：攻击者用旧副本 + 旧密码解出 FEK_old，
        // 新文件头里的 file_uuid 是明文，于是 HKDF(FEK_old, uuid_new) 就是
        // 新文件的载荷密钥——他不需要新密码就能解开新文件。
        //
        // 断言写成「攻击不成立」而不是「FEK 字节变了」：比字节只能证明
        // 变了，写成攻击才能证明旧的用不了。
        let salt = [0x3Eu8; 16];
        let data = sample(&salt, &["pw-old"], b"payload bytes here");

        let old_opened = crate::file::open(&data, &[kek("pw-old", &salt)]).unwrap();
        let out = rotate_fek(
            &data,
            &[kek("pw-old", &salt)],
            &[kek("pw-new", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();
        let new_opened = crate::file::open(&out.bytes, &[kek("pw-new", &salt)]).unwrap();

        let forged = old_opened
            .fek()
            .derive_payload_key(&new_opened.header.file_uuid);
        assert_ne!(
            forged.as_bytes(),
            new_opened.payload_key().as_bytes(),
            "旧 FEK 不得能推出新文件的载荷密钥，否则持有旧副本的人不需要新密码就能解开新文件"
        );
    }

    #[test]
    fn old_password_cannot_open_rotated_file() {
        let salt = [0x33u8; 16];
        let data = sample(&salt, &["pw-old"], b"secret");
        let out = rotate_fek(
            &data,
            &[kek("pw-old", &salt)],
            &[kek("pw-new", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();
        assert!(crate::file::open(&out.bytes, &[kek("pw-old", &salt)]).is_err());
    }

    #[test]
    fn rotation_preserves_filename_thumbnail_and_media_meta() {
        // 不这样会怎样：rebuild_options 漏一项就静默丢数据。文件照样能打开、
        // 明文照样正确，只是缩略图不见了——最容易漏、也最难发现
        let salt = [0x34u8; 16];
        let data = sample(&salt, &["pw"], b"x");
        let out = rotate_fek(
            &data,
            &[kek("pw", &salt)],
            &[kek("pw", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();

        let re = crate::file::open(&out.bytes, &[kek("pw", &salt)]).unwrap();
        assert_eq!(re.filename().unwrap(), "报告.txt");
        assert_eq!(re.plain_extension().as_deref(), Some("txt"));
        assert_eq!(
            re.thumbnail().unwrap(),
            (0..64u32).map(|i| (i % 251) as u8).collect::<Vec<u8>>()
        );
        assert_eq!(re.media_meta().unwrap(), br#"{"w":1920}"#.to_vec());
        assert!(
            re.header.has_flag(flags::HAS_THUMBNAIL),
            "flag 也要跟着保留"
        );
    }

    #[test]
    fn rotation_preserves_format_parameters() {
        let salt = [0x35u8; 16];
        let data = sample(&salt, &["pw"], b"y");
        let before = FixedHeader::parse(&data).unwrap();
        let out = rotate_fek(
            &data,
            &[kek("pw", &salt)],
            &[kek("pw", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();
        let after = FixedHeader::parse(&out.bytes).unwrap();

        assert_eq!(before.chunk_size, after.chunk_size);
        assert_eq!(before.cipher_id, after.cipher_id);
        assert_eq!(
            before.vault_salt, after.vault_salt,
            "同一个 vault，salt 不能变"
        );
        // Argon2 参数错了文件当场打不开，且症状极具迷惑性（见 encrypt.rs 的记录）
        assert_eq!(before.argon2_m_kib, after.argon2_m_kib);
        assert_eq!(before.argon2_t, after.argon2_t);
        assert_eq!(before.argon2_p, after.argon2_p);
        assert_eq!(before.flags, after.flags, "flags 整体必须一致");
    }

    #[test]
    fn rotation_can_change_password_set_at_the_same_time() {
        // 轮换和改密码是一次操作：先轮换再改密码等于把载荷写两遍
        let salt = [0x36u8; 16];
        let data = sample(&salt, &["pw-a"], b"z");
        let out = rotate_fek(
            &data,
            &[kek("pw-a", &salt)],
            &[kek("pw-b", &salt), kek("pw-c", &salt)],
            &RandomMaterial::generate(),
        )
        .unwrap();

        assert_eq!(out.slot_used, 2);
        for p in ["pw-b", "pw-c"] {
            assert!(
                crate::file::open(&out.bytes, &[kek(p, &salt)]).is_ok(),
                "{p} 应能打开"
            );
        }
        assert!(crate::file::open(&out.bytes, &[kek("pw-a", &salt)]).is_err());
    }

    #[test]
    fn empty_keep_is_rejected() {
        let salt = [0x37u8; 16];
        let data = sample(&salt, &["pw"], b"w");
        assert!(rotate_fek(&data, &[kek("pw", &salt)], &[], &RandomMaterial::generate()).is_err());
    }

    #[test]
    fn wrong_unlock_password_fails() {
        let salt = [0x38u8; 16];
        let data = sample(&salt, &["pw"], b"v");
        assert!(
            rotate_fek(
                &data,
                &[kek("nope", &salt)],
                &[kek("pw", &salt)],
                &RandomMaterial::generate()
            )
            .is_err()
        );
    }

    #[test]
    fn tampered_header_is_rejected_before_rewriting() {
        // 不这样会怎样：跳过 MAC 校验就会给篡改后的头部算一个新的有效 MAC，
        // 等于替攻击者洗白
        let salt = [0x39u8; 16];
        let mut data = sample(&salt, &["pw"], b"u");
        data[64] ^= 0xFF; // plaintext_size 所在偏移
        assert!(
            rotate_fek(
                &data,
                &[kek("pw", &salt)],
                &[kek("pw", &salt)],
                &RandomMaterial::generate()
            )
            .is_err()
        );
    }

    #[test]
    fn rotation_preserves_compression() {
        let salt = [0x3Au8; 16];
        let keks = [kek("pw", &salt)];
        // 高度可压缩的数据，确保压缩确实生效
        let plain = b"aaaaaaaaaaaaaaaa".repeat(4096);
        let opts = EncryptOptions {
            compress: true,
            argon2: Argon2Params::TEST_WEAK,
            ..EncryptOptions::default()
        };
        let data = encrypt(&plain, &keks, &salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes;
        assert!(
            FixedHeader::parse(&data)
                .unwrap()
                .has_flag(flags::COMPRESSED)
        );

        let out = rotate_fek(&data, &keks, &keks, &RandomMaterial::generate()).unwrap();
        let after = FixedHeader::parse(&out.bytes).unwrap();
        assert!(after.has_flag(flags::COMPRESSED), "压缩设置必须保留");
        // 压缩块索引也必须重建，否则解密时无法定位块边界
        let re = crate::file::open(&out.bytes, &keks).unwrap();
        assert_eq!(re.decrypt_all(&out.bytes).unwrap(), plain);
    }

    #[test]
    fn rotation_preserves_container_flag_and_index() {
        // 丢了容器索引不会报错，只会把加密文件夹解成一堆拼接字节——
        // 这是最隐蔽的一种丢失
        let salt = [0x3Bu8; 16];
        let keks = [kek("pw", &salt)];
        let idx_bytes = {
            let mut b = crate::container::ContainerBuilder::new("folder");
            b.add_file(vec![String::from("a.txt")], 3, None, EntryMeta::default())
                .unwrap();
            b.finish().unwrap().encode()
        };
        let opts = EncryptOptions {
            folder_index: Some(idx_bytes.clone()),
            argon2: Argon2Params::TEST_WEAK,
            ..EncryptOptions::default()
        };
        let data = encrypt(b"abc", &keks, &salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes;

        let out = rotate_fek(&data, &keks, &keks, &RandomMaterial::generate()).unwrap();
        let re = crate::file::open(&out.bytes, &keks).unwrap();
        assert!(re.is_container(), "CONTAINER flag 必须保留");
        assert_eq!(
            re.raw_folder_index().unwrap(),
            idx_bytes,
            "索引字节必须逐字节一致"
        );
    }

    #[test]
    fn rotation_keeps_content_hash_absent_when_original_had_none() {
        // 不能一律写哈希：那会让原本没有自检的文件多出一个 TLV，
        // 破坏「除密钥外内容等价」
        let salt = [0x3Cu8; 16];
        let keks = [kek("pw", &salt)];
        let opts = EncryptOptions {
            write_content_hash: false,
            argon2: Argon2Params::TEST_WEAK,
            ..EncryptOptions::default()
        };
        let data = encrypt(b"t", &keks, &salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes;

        let out = rotate_fek(&data, &keks, &keks, &RandomMaterial::generate()).unwrap();
        let re = crate::file::open(&out.bytes, &keks).unwrap();
        assert!(
            re.stored_content_hash().is_err(),
            "原文件没有哈希，轮换后也不该有"
        );
    }

    #[test]
    fn replacement_preserves_all_slots_and_uses_new_plaintext() {
        // 不这样会怎样：外部编辑一次就可能让另一个密码或恢复码失效，用户直到
        // 真需要兜底时才发现；或者复用旧 nonce，让两版不同明文使用同一 AEAD nonce。
        let salt = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x4D);
        let keys = [kek("pw-a", &salt), kek("pw-b", &salt)];
        let opts = EncryptOptions {
            filename: Some(String::from("报告.txt")),
            argon2: Argon2Params::TEST_WEAK,
            ..EncryptOptions::default()
        };
        let original = encrypt(b"before", &keys, &salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes;
        let before = FixedHeader::parse(&original).unwrap();
        let slots_before = original
            .get(SLOT_AREA_OFFSET..SLOT_AREA_OFFSET + SLOT_AREA_LEN)
            .unwrap()
            .to_vec();
        let nonce = core::array::from_fn(|i| (i as u8).wrapping_mul(17) ^ 0xA3);

        let replaced = replace_plaintext_preserving_slots(
            &original,
            &keys,
            b"after external edit",
            &ReplacementMetadata::default(),
            nonce,
        )
        .unwrap();
        let after = FixedHeader::parse(&replaced.bytes).unwrap();

        assert_eq!(
            before.file_uuid, after.file_uuid,
            "槽位派生依赖 file_uuid，不能变"
        );
        assert_eq!(after.base_nonce, nonce, "必须采用新 nonce");
        assert_ne!(before.base_nonce, after.base_nonce, "不得复用旧 nonce");
        assert_eq!(
            replaced
                .bytes
                .get(SLOT_AREA_OFFSET..SLOT_AREA_OFFSET + SLOT_AREA_LEN)
                .unwrap(),
            slots_before,
            "所有已知和未知槽都必须逐字节保留",
        );
        for key in &keys {
            let opened = crate::file::open(&replaced.bytes, &[key.duplicate()]).unwrap();
            assert_eq!(
                opened.decrypt_all(&replaced.bytes).unwrap(),
                b"after external edit",
                "每把旧钥匙都要能打开修改后的新内容",
            );
            assert_eq!(opened.filename().unwrap(), "报告.txt");
        }
    }

    #[test]
    fn replacement_rejects_container() {
        let salt = core::array::from_fn(|i| (i as u8).wrapping_mul(5) ^ 0x71);
        let keys = [kek("pw", &salt)];
        let mut b = crate::container::ContainerBuilder::new("folder");
        b.add_file(vec![String::from("a.txt")], 3, None, EntryMeta::default())
            .unwrap();
        let opts = EncryptOptions {
            folder_index: Some(b.finish().unwrap().encode()),
            argon2: Argon2Params::TEST_WEAK,
            ..EncryptOptions::default()
        };
        let original = encrypt(b"abc", &keys, &salt, &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes;
        assert!(
            replace_plaintext_preserving_slots(
                &original,
                &keys,
                b"flat file",
                &ReplacementMetadata::default(),
                [9; 7],
            )
            .is_err(),
            "容器不能被第三方单文件编辑静默拍平",
        );
    }

    #[test]
    fn corrupted_payload_is_rejected_instead_of_being_re_signed() {
        // 载荷坏了还去重新加密，等于用新密钥把损坏内容固定下来，
        // 而且覆盖原文件后再也无从追查
        let salt = [0x3Du8; 16];
        let keks = [kek("pw", &salt)];
        let mut data = sample(&salt, &["pw"], &b"payload".repeat(100));
        let last = data.len().saturating_sub(1);
        data[last] ^= 0xFF;
        assert!(rotate_fek(&data, &keks, &keks, &RandomMaterial::generate()).is_err());
    }
}
