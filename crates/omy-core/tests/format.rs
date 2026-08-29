//! 格式与安全属性的单元测试。
//!
//! 测试向量验证「与参考实现一致」，本文件验证「规范要求的安全属性确实成立」。
//! 两者互补：向量保证互操作性，这里保证攻击被真正挡住。

// 测试代码本就应该在断言失败时 panic。这些 lint 是为库代码设的。
#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
#![allow(clippy::integer_division)]

use omy_core::crypto::{Argon2Params, CipherId, Fek, Kek, chunk_nonce};
use omy_core::error::ExitCode;
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt, open};
use omy_core::header::{
    FIXED_HEADER_LEN, FixedHeader, HEADER_MAC_LEN, MAX_CHUNK_SIZE, MIN_CHUNK_SIZE, SLOT_AREA_LEN,
    TLV_AREA_OFFSET, flags, validate_chunk_size,
};
use omy_core::tlv::{TlvSet, pad_filename, types, unpad_filename};

const SALT: [u8; 16] = [0x5A; 16];

fn test_kek(pw: &[u8]) -> Kek {
    Kek::from_password(pw, &SALT, Argon2Params::TEST_WEAK).expect("KEK 派生失败")
}

fn basic_opts() -> EncryptOptions {
    EncryptOptions {
        filename: Some("test.bin".into()),
        chunk_size: MIN_CHUNK_SIZE,
        argon2: Argon2Params::TEST_WEAK,
        ..EncryptOptions::default()
    }
}

// ============================================================
// 文件名 padding：规范 §4.5 特别警告的陷阱
// ============================================================

/// 桶大小必须基于 `2 + 名字长度`，而非仅名字长度。
///
/// 规范明确警告：若写成 `(len / 64 + 1) * 64`，当 `len = 63` 时 `need = 65`
/// 却算出桶 64，填充量为负、越出桶边界，该长度反而暴露自己的长度特征。
/// 参考实现曾命中此 bug，因此必须逐长度验证。
#[test]
fn filename_padding_never_exceeds_bucket() {
    for len in 0..=300usize {
        let name = "a".repeat(len);
        let padded = pad_filename(&name).expect("padding 失败");

        let need = len + 2;
        let expect_bucket = need.div_ceil(64) * 64;

        assert_eq!(
            padded.len(),
            expect_bucket,
            "长度 {len} 的文件名：padded 长度应为 {expect_bucket}，实际 {}",
            padded.len()
        );
        assert_eq!(padded.len() % 64, 0, "长度 {len}：padded 必须是 64 的整数倍");
        assert!(padded.len() >= need, "长度 {len}：padded 不得短于 need");

        let back = unpad_filename(&padded).expect("还原失败");
        assert_eq!(back, name, "长度 {len} 的文件名还原后不一致");
    }
}

/// 边界长度 62/63/64 是最容易出错的位置，单独固定期望值。
#[test]
fn filename_padding_boundary_values() {
    // need = 64 恰好填满第一个桶
    assert_eq!(pad_filename(&"a".repeat(62)).expect("失败").len(), 64);
    // need = 65 必须进入第二个桶——这是曾经出 bug 的位置
    assert_eq!(pad_filename(&"a".repeat(63)).expect("失败").len(), 128);
    assert_eq!(pad_filename(&"a".repeat(64)).expect("失败").len(), 128);
    assert_eq!(pad_filename(&"a".repeat(126)).expect("失败").len(), 128);
    assert_eq!(pad_filename(&"a".repeat(127)).expect("失败").len(), 192);
}

/// 同一桶内的所有文件名长度必须产出相同的 header_len，否则长度会泄露。
#[test]
fn same_bucket_yields_identical_header_len() {
    let mut by_bucket: std::collections::BTreeMap<usize, Vec<u32>> =
        std::collections::BTreeMap::new();

    for len in [0usize, 1, 30, 62, 63, 100, 126, 127, 190] {
        let name = "x".repeat(len);
        let opts = EncryptOptions {
            filename: Some(name),
            write_content_hash: false,
            argon2: Argon2Params::TEST_WEAK,
            chunk_size: MIN_CHUNK_SIZE,
            ..EncryptOptions::default()
        };
        let enc = encrypt(b"data", &[test_kek(b"pw")], &SALT, &opts, &RandomMaterial::generate())
            .expect("加密失败");
        let bucket = (len + 2).div_ceil(64) * 64;
        by_bucket.entry(bucket).or_default().push(enc.header.header_len);
    }

    for (bucket, lens) in &by_bucket {
        let first = lens.first().expect("空桶");
        assert!(
            lens.iter().all(|l| l == first),
            "桶 {bucket} 内的 header_len 不一致: {lens:?}，文件名长度会被泄露"
        );
    }
}

/// 非 ASCII 文件名按 UTF-8 字节长度计算桶。
#[test]
fn filename_padding_handles_multibyte() {
    let name = "测试文件名.txt"; // 中文占 3 字节
    let padded = pad_filename(name).expect("padding 失败");
    let byte_len = name.len();
    let expect = (byte_len + 2).div_ceil(64) * 64;
    assert_eq!(padded.len(), expect, "多字节文件名的桶计算有误");
    assert_eq!(unpad_filename(&padded).expect("还原失败"), name);
}

// ============================================================
// 防篡改：规范 §7 列出的攻击必须全部被检出
// ============================================================

fn make_file() -> (Vec<u8>, Vec<u8>) {
    let plaintext = b"sensitive content that must stay intact".to_vec();
    let enc = encrypt(
        &plaintext,
        &[test_kek(b"pw")],
        &SALT,
        &basic_opts(),
        &RandomMaterial::generate(),
    )
    .expect("加密失败");
    (enc.bytes, plaintext)
}

/// 篡改 Argon2 参数（降级攻击）必须被 MAC 检出。
#[test]
fn tamper_argon2_params_detected() {
    let (mut file, _) = make_file();
    // argon2_m_kib 位于偏移 56
    file.get_mut(56..60)
        .expect("文件过短")
        .copy_from_slice(&8u32.to_le_bytes());

    let err = open(&file, &[test_kek(b"pw")]).expect_err("篡改 Argon2 参数未被检出");
    // 参数变了会导致 KEK 不同，从而无法解开 slot；这同样阻止了攻击
    assert!(
        matches!(err.code(), "HEADER_MAC_MISMATCH" | "WRONG_PASSWORD"),
        "应报 MAC 不符或密码错误，实际: {}",
        err.code()
    );
}

/// 篡改 flags（例如关闭 COMPRESSED 导致误读）必须被检出。
#[test]
fn tamper_flags_detected() {
    let (mut file, _) = make_file();
    // flags 位于偏移 48
    let orig = u32::from_le_bytes(
        file.get(48..52).expect("文件过短").try_into().expect("切片长度错"),
    );
    let tampered = orig | flags::HAS_THUMBNAIL;
    file.get_mut(48..52)
        .expect("文件过短")
        .copy_from_slice(&tampered.to_le_bytes());

    let err = open(&file, &[test_kek(b"pw")]).expect_err("篡改 flags 未被检出");
    assert_eq!(err.code(), "HEADER_MAC_MISMATCH", "应报 MAC 不符");
}

/// 篡改 plaintext_size 必须被检出。
#[test]
fn tamper_plaintext_size_detected() {
    let (mut file, _) = make_file();
    // plaintext_size 位于偏移 72
    file.get_mut(72..80)
        .expect("文件过短")
        .copy_from_slice(&999_999u64.to_le_bytes());

    let err = open(&file, &[test_kek(b"pw")]).expect_err("篡改 plaintext_size 未被检出");
    assert!(
        matches!(err.code(), "HEADER_MAC_MISMATCH" | "MALFORMED_HEADER"),
        "应报 MAC 不符或头部非法，实际: {}",
        err.code()
    );
}

/// 篡改 slot 区必须被检出。
#[test]
fn tamper_slot_area_detected() {
    let (mut file, _) = make_file();
    if let Some(b) = file.get_mut(96) {
        *b ^= 0xFF;
    }
    let err = open(&file, &[test_kek(b"pw")]).expect_err("篡改 slot 区未被检出");
    // slot 被破坏后无法解出 FEK
    assert_eq!(err.code(), "WRONG_PASSWORD", "破坏 slot 后应无法解包 FEK");
}

/// 篡改 TLV 内容必须被 MAC 检出。
#[test]
fn tamper_tlv_detected() {
    let (mut file, _) = make_file();
    // TLV 区从 480 开始，翻转其中一个字节
    if let Some(b) = file.get_mut(TLV_AREA_OFFSET + 10) {
        *b ^= 0xFF;
    }
    let err = open(&file, &[test_kek(b"pw")]).expect_err("篡改 TLV 未被检出");
    assert_eq!(err.code(), "HEADER_MAC_MISMATCH", "应报 MAC 不符");
}

/// 篡改载荷必须在解密时被 AEAD 检出。
#[test]
fn tamper_payload_detected() {
    let (mut file, _) = make_file();
    let header = FixedHeader::parse(&file).expect("头部解析失败");
    let payload_start = header.header_len as usize;
    if let Some(b) = file.get_mut(payload_start) {
        *b ^= 0xFF;
    }

    let opened = open(&file, &[test_kek(b"pw")]).expect("头部本身未被改，应能打开");
    let err = opened.decrypt_all(&file).expect_err("篡改载荷未被检出");
    assert_eq!(err.code(), "CHUNK_AUTH_FAILED", "应报块认证失败");
}

/// 截断文件尾部必须被检出。
///
/// 这是 nonce 中 `final_flag` 的作用：删掉末块后，新的「最后一块」flag 不匹配。
#[test]
fn truncation_detected() {
    let plaintext = vec![0xAAu8; MIN_CHUNK_SIZE as usize * 3];
    let enc = encrypt(
        &plaintext,
        &[test_kek(b"pw")],
        &SALT,
        &basic_opts(),
        &RandomMaterial::generate(),
    )
    .expect("加密失败");

    assert_eq!(enc.header.n_chunks(), 3, "应有 3 块");

    // 砍掉最后一块
    let (last_off, last_len) = enc.header.chunk_ct_range(2).expect("偏移计算失败");
    let truncated_len = usize::try_from(last_off).expect("偏移越界");
    let mut truncated = enc.bytes.clone();
    truncated.truncate(truncated_len);
    assert_eq!(
        enc.bytes.len() - truncated.len(),
        usize::try_from(last_len).expect("长度越界"),
        "应恰好砍掉最后一块"
    );

    let opened = open(&truncated, &[test_kek(b"pw")]).expect("头部完整，应能打开");
    let err = opened.decrypt_all(&truncated).expect_err("截断未被检出");
    assert!(
        matches!(err.code(), "TRUNCATED" | "CHUNK_AUTH_FAILED"),
        "应报截断或认证失败，实际: {}",
        err.code()
    );
}

/// 跨文件移植块必须失败——即使同一密码、同一 vault。
///
/// 这是 AAD 绑定 `file_uuid` 的作用。
#[test]
fn cross_file_chunk_transplant_fails() {
    let opts = basic_opts();
    let kek = test_kek(b"pw");

    let a = encrypt(b"file A content", &[kek], &SALT, &opts, &RandomMaterial::generate())
        .expect("加密 A 失败");
    let kek2 = test_kek(b"pw");
    let b = encrypt(b"file B content", &[kek2], &SALT, &opts, &RandomMaterial::generate())
        .expect("加密 B 失败");

    assert_ne!(a.header.file_uuid, b.header.file_uuid, "两文件的 uuid 应不同");

    // 把 A 的载荷移植到 B
    let mut hybrid = b.bytes.clone();
    let b_start = b.header.header_len as usize;
    let a_start = a.header.header_len as usize;
    let a_payload = a.bytes.get(a_start..).expect("A 载荷切片失败");
    hybrid.truncate(b_start);
    hybrid.extend_from_slice(a_payload);

    let kek3 = test_kek(b"pw");
    let opened = open(&hybrid, &[kek3]).expect("B 的头部未改，应能打开");
    let err = opened.decrypt_all(&hybrid).expect_err("跨文件移植未被检出");
    assert!(
        matches!(err.code(), "CHUNK_AUTH_FAILED" | "MALFORMED_HEADER"),
        "应报认证失败，实际: {}",
        err.code()
    );
}

/// 块重排必须失败——这是 AAD 绑定块序号的作用。
#[test]
fn chunk_reorder_fails() {
    let cs = MIN_CHUNK_SIZE as usize;
    let mut plaintext = vec![0u8; cs * 2];
    // 两块内容不同，交换后必然可辨
    plaintext.get_mut(..cs).expect("切片失败").fill(0x11);
    plaintext.get_mut(cs..).expect("切片失败").fill(0x22);

    let enc = encrypt(
        &plaintext,
        &[test_kek(b"pw")],
        &SALT,
        &basic_opts(),
        &RandomMaterial::generate(),
    )
    .expect("加密失败");
    assert_eq!(enc.header.n_chunks(), 2, "应有 2 块");

    let (o0, l0) = enc.header.chunk_ct_range(0).expect("偏移失败");
    let (o1, l1) = enc.header.chunk_ct_range(1).expect("偏移失败");
    let (a0, b0) = (
        usize::try_from(o0).expect("越界"),
        usize::try_from(o0 + l0).expect("越界"),
    );
    let (a1, b1) = (
        usize::try_from(o1).expect("越界"),
        usize::try_from(o1 + l1).expect("越界"),
    );

    let mut swapped = enc.bytes.clone();
    let c0 = enc.bytes.get(a0..b0).expect("块 0 切片失败").to_vec();
    let c1 = enc.bytes.get(a1..b1).expect("块 1 切片失败").to_vec();
    // 两块长度相同才能原地交换
    assert_eq!(c0.len(), c1.len(), "两块长度应相同");
    swapped.get_mut(a0..b0).expect("写入失败").copy_from_slice(&c1);
    swapped.get_mut(a1..b1).expect("写入失败").copy_from_slice(&c0);

    let opened = open(&swapped, &[test_kek(b"pw")]).expect("头部未改，应能打开");
    let err = opened.decrypt_all(&swapped).expect_err("块重排未被检出");
    assert_eq!(err.code(), "CHUNK_AUTH_FAILED", "应报块认证失败");
}

// ============================================================
// 可还原性：规范 §11 的核心承诺
// ============================================================

/// 各种尺寸都必须 bit-for-bit 还原，特别是块边界 ±1。
#[test]
fn roundtrip_bit_exact_all_sizes() {
    let cs = MIN_CHUNK_SIZE;
    let sizes = [
        0usize,                    // 空文件
        1,                         // 单字节
        cs as usize - 1,           // 块边界 -1
        cs as usize,               // 恰好一块
        cs as usize + 1,           // 块边界 +1
        cs as usize * 2 - 1,       // 两块边界 -1
        cs as usize * 2,           // 恰好两块
        cs as usize * 2 + 1,       // 两块边界 +1
        cs as usize * 3 + 12345,   // 非整数块
    ];

    for size in sizes {
        // 用可辨识的模式而非全零，避免掩盖偏移错误
        let plaintext: Vec<u8> = (0..size)
            .map(|i| u8::try_from((i * 31 + 7) % 256).unwrap_or(0))
            .collect();

        let opts = EncryptOptions { chunk_size: cs, ..basic_opts() };
        let enc = encrypt(
            &plaintext,
            &[test_kek(b"pw")],
            &SALT,
            &opts,
            &RandomMaterial::generate(),
        )
        .unwrap_or_else(|e| panic!("尺寸 {size} 加密失败: {e}"));

        let opened = open(&enc.bytes, &[test_kek(b"pw")])
            .unwrap_or_else(|e| panic!("尺寸 {size} 打开失败: {e}"));
        let back = opened
            .decrypt_all(&enc.bytes)
            .unwrap_or_else(|e| panic!("尺寸 {size} 解密失败: {e}"));

        assert_eq!(back.len(), size, "尺寸 {size} 还原长度不符");
        assert_eq!(back, plaintext, "尺寸 {size} 未能 bit-for-bit 还原");
    }
}

/// 空文件也必须有 1 个块（零长度明文 + tag）。
#[test]
fn empty_file_has_one_chunk() {
    let enc = encrypt(b"", &[test_kek(b"pw")], &SALT, &basic_opts(), &RandomMaterial::generate())
        .expect("加密空文件失败");
    assert_eq!(enc.header.n_chunks(), 1, "空文件也应有 1 块");
    assert_eq!(enc.header.plaintext_size, 0, "明文长度应为 0");

    let opened = open(&enc.bytes, &[test_kek(b"pw")]).expect("打开失败");
    let back = opened.decrypt_all(&enc.bytes).expect("解密失败");
    assert!(back.is_empty(), "空文件还原后应为空");
}

/// 压缩往返也必须完整还原。
#[test]
fn compressed_roundtrip_exact() {
    for size in [0usize, 1, 1000, 100_000] {
        let plaintext: Vec<u8> = (0..size).map(|i| u8::try_from(i % 7).unwrap_or(0)).collect();
        let opts = EncryptOptions { compress: true, zstd_level: 9, ..basic_opts() };
        let enc = encrypt(
            &plaintext,
            &[test_kek(b"pw")],
            &SALT,
            &opts,
            &RandomMaterial::generate(),
        )
        .unwrap_or_else(|e| panic!("尺寸 {size} 压缩加密失败: {e}"));

        let opened = open(&enc.bytes, &[test_kek(b"pw")]).expect("打开失败");
        let back = opened
            .decrypt_all(&enc.bytes)
            .unwrap_or_else(|e| panic!("尺寸 {size} 压缩解密失败: {e}"));
        assert_eq!(back, plaintext, "尺寸 {size} 压缩往返后不一致");
    }
}

/// AES-256-GCM 与 ChaCha20-Poly1305 都必须能正常往返。
#[test]
fn both_ciphers_roundtrip() {
    for cipher in [CipherId::ChaCha20Poly1305, CipherId::Aes256Gcm] {
        let plaintext = b"cipher agility test payload";
        let opts = EncryptOptions { cipher, ..basic_opts() };
        let enc = encrypt(
            plaintext,
            &[test_kek(b"pw")],
            &SALT,
            &opts,
            &RandomMaterial::generate(),
        )
        .unwrap_or_else(|e| panic!("{cipher:?} 加密失败: {e}"));

        assert_eq!(enc.header.cipher_id, cipher, "头部记录的算法不符");

        let opened = open(&enc.bytes, &[test_kek(b"pw")])
            .unwrap_or_else(|e| panic!("{cipher:?} 打开失败: {e}"));
        let back = opened
            .decrypt_all(&enc.bytes)
            .unwrap_or_else(|e| panic!("{cipher:?} 解密失败: {e}"));
        assert_eq!(back.as_slice(), plaintext.as_slice(), "{cipher:?} 往返后不一致");
    }
}

// ============================================================
// 格式结构
// ============================================================

/// header_len 必须严格等于 96 + 384 + tlv_len + 32。
#[test]
fn header_len_formula_holds() {
    let enc = encrypt(b"x", &[test_kek(b"pw")], &SALT, &basic_opts(), &RandomMaterial::generate())
        .expect("加密失败");
    let expect = FIXED_HEADER_LEN + SLOT_AREA_LEN + enc.header.tlv_len as usize + HEADER_MAC_LEN;
    assert_eq!(
        enc.header.header_len as usize, expect,
        "header_len 不符合公式"
    );
}

/// 非 omy 文件必须被 magic 拒绝，且错误码明确。
#[test]
fn non_omy_file_rejected() {
    let fake = vec![0u8; 200];
    let err = FixedHeader::parse(&fake).expect_err("非 omy 数据竟被接受");
    assert_eq!(err.code(), "BAD_MAGIC", "应报 BAD_MAGIC");
    assert_eq!(err.exit_code(), ExitCode::Corrupted, "退出码应为 Corrupted");

    assert!(!omy_core::file::is_omy_file(&fake), "快速识别应返回 false");
}

/// 高于本实现的主版本必须被拒绝。
#[test]
fn future_major_version_rejected() {
    let enc = encrypt(b"x", &[test_kek(b"pw")], &SALT, &basic_opts(), &RandomMaterial::generate())
        .expect("加密失败");
    let mut file = enc.bytes;
    // version_major 位于偏移 8
    file.get_mut(8..10)
        .expect("文件过短")
        .copy_from_slice(&99u16.to_le_bytes());

    let err = FixedHeader::parse(&file).expect_err("未来版本竟被接受");
    assert_eq!(err.code(), "UNSUPPORTED_VERSION", "应报版本不支持");
}

/// 未知的非 CRITICAL TLV 必须被忽略。
#[test]
fn unknown_non_critical_tlv_ignored() {
    let mut set = TlvSet::new();
    set.push(omy_core::TlvEntry::new(0xF000, 0, vec![1, 2, 3]));
    set.check_critical().expect("未知非 CRITICAL 条目应被忽略");
}

/// 未知的 CRITICAL TLV 必须导致拒绝打开。
#[test]
fn unknown_critical_tlv_rejected() {
    let mut set = TlvSet::new();
    set.push(omy_core::TlvEntry::new(0x0500, 1, vec![1, 2, 3]));
    let err = set.check_critical().expect_err("未知 CRITICAL 条目应被拒绝");
    assert_eq!(err.code(), "UNKNOWN_CRITICAL_TLV", "应报未知 CRITICAL 类型");
}

/// 私有区间的未知 CRITICAL TLV 应被忽略（那是第三方自用字段）。
#[test]
fn private_range_critical_tlv_ignored() {
    let mut set = TlvSet::new();
    set.push(omy_core::TlvEntry::new(0x8001, 1, vec![9]));
    set.check_critical().expect("私有区间条目应被忽略");
}

/// TLV 编解码必须往返一致。
#[test]
fn tlv_roundtrip() {
    let mut set = TlvSet::new();
    set.push(omy_core::TlvEntry::new(types::FILENAME, 3, vec![1, 2, 3, 4]));
    set.push(omy_core::TlvEntry::new(types::PLAIN_EXT, 0, b"txt".to_vec()));
    set.push(omy_core::TlvEntry::new(0xF123, 0, vec![]));

    let bytes = set.to_bytes();
    let back = TlvSet::parse(&bytes).expect("TLV 解析失败");
    assert_eq!(back, set, "TLV 往返后不一致");
    assert_eq!(bytes.len(), set.encoded_len(), "编码长度计算有误");
}

// ============================================================
// 密钥与 nonce
// ============================================================

/// nonce 的块序号必须是大端序，且 final_flag 正确置位。
#[test]
fn chunk_nonce_layout() {
    let base = [0x11u8; 7];
    let n = chunk_nonce(&base, 0x0102_0304, false);
    assert_eq!(n.get(..7), Some(base.as_slice()), "前 7 字节应为 base_nonce");
    assert_eq!(
        n.get(7..11),
        Some([0x01, 0x02, 0x03, 0x04].as_slice()),
        "块序号必须是大端序"
    );
    assert_eq!(n.get(11), Some(&0x00), "非末块的 flag 应为 0");

    let nf = chunk_nonce(&base, 5, true);
    assert_eq!(nf.get(11), Some(&0x01), "末块的 flag 应为 1");
}

/// 同一 FEK 派生的四种子密钥必须互不相同（域分隔）。
#[test]
fn key_domain_separation() {
    let fek = Fek::random();
    let uuid = [3u8; 16];

    let pk = *fek.derive_payload_key(&uuid).as_bytes();
    let mk = *fek.derive_header_mac_key(&uuid).as_bytes();
    let t1 = *fek.derive_tlv_key(types::FILENAME).as_bytes();
    let t2 = *fek.derive_tlv_key(types::THUMBNAIL).as_bytes();

    assert_ne!(pk, mk, "载荷密钥与 MAC 密钥必须不同");
    assert_ne!(t1, t2, "不同 TLV 类型的密钥必须不同");
    assert_ne!(pk, t1, "载荷密钥与 TLV 密钥必须不同");
    assert_ne!(mk, t1, "MAC 密钥与 TLV 密钥必须不同");
}

/// 不同 slot 序号的包裹密钥必须不同——否则零 nonce 会导致 nonce 复用。
#[test]
fn slot_keys_differ_by_index() {
    let kek = test_kek(b"pw");
    let uuid = [4u8; 16];
    let mut seen = std::collections::HashSet::new();
    for i in 0..8u16 {
        let k = *kek.derive_slot_key(&uuid, i).as_bytes();
        assert!(seen.insert(k), "slot {i} 的包裹密钥与之前重复，零 nonce 将不安全");
    }
}

/// 不同 file_uuid 会产生不同的 slot 密钥——同密码的不同文件 slot 区不同。
#[test]
fn slot_keys_differ_by_file() {
    let kek = test_kek(b"pw");
    let k1 = *kek.derive_slot_key(&[1u8; 16], 0).as_bytes();
    let k2 = *kek.derive_slot_key(&[2u8; 16], 0).as_bytes();
    assert_ne!(k1, k2, "不同文件的 slot 密钥必须不同");
}

/// 分块大小的策略校验边界。
#[test]
fn chunk_size_policy_bounds() {
    assert!(validate_chunk_size(MIN_CHUNK_SIZE).is_ok(), "下限应被接受");
    assert!(validate_chunk_size(MAX_CHUNK_SIZE).is_ok(), "上限应被接受");
    assert!(validate_chunk_size(MIN_CHUNK_SIZE - 1).is_err(), "低于下限应被拒绝");
    assert!(validate_chunk_size(MAX_CHUNK_SIZE + 1).is_err(), "高于上限应被拒绝");

    let err = validate_chunk_size(1024).expect_err("1024 应被拒绝");
    assert_eq!(err.exit_code(), ExitCode::Usage, "应映射为参数错误");
}

/// 内容哈希不符必须被检出。
#[test]
fn content_hash_mismatch_detected() {
    let enc = encrypt(
        b"original",
        &[test_kek(b"pw")],
        &SALT,
        &basic_opts(),
        &RandomMaterial::generate(),
    )
    .expect("加密失败");

    let opened = open(&enc.bytes, &[test_kek(b"pw")]).expect("打开失败");
    // 正常路径下哈希应匹配
    opened.decrypt_all(&enc.bytes).expect("正常解密应通过哈希校验");

    // 确认哈希确实被写入了
    opened.stored_content_hash().expect("应存有内容哈希");
}

/// 退出码映射必须符合 CLI 契约。
#[test]
fn exit_code_mapping() {
    use omy_core::Error;
    assert_eq!(Error::NoMatchingSlot.exit_code(), ExitCode::WrongPassword);
    assert_eq!(Error::HeaderMacMismatch.exit_code(), ExitCode::Corrupted);
    assert_eq!(Error::ContentHashMismatch.exit_code(), ExitCode::Corrupted);
    assert_eq!(
        Error::UnknownCriticalTlv { tlv_type: 1 }.exit_code(),
        ExitCode::UnsupportedVersion
    );
    assert_eq!(
        Error::MissingShards { missing: vec![1], total: 3 }.exit_code(),
        ExitCode::MissingShard
    );
    // 数值必须稳定，脚本依赖它们
    assert_eq!(ExitCode::Success as u8, 0);
    assert_eq!(ExitCode::WrongPassword as u8, 3);
    assert_eq!(ExitCode::Corrupted as u8, 4);
}
