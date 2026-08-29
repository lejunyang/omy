//! 恶意输入的健壮性测试（规范 §8.3）。
//!
//! # 这些测试在验证什么
//!
//! 每一项都构造一个**真实的恶意文件**并要求实现拒绝它。关键在于：
//! 攻击者无需任何有效密钥就能构造这些输入——它们只需要通过头部解析这一关，
//! 就能让实现分配巨额内存或读取越界。
//!
//! 这些测试若不存在，上界代码可能从未被真正触发过，等于没写。

#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
#![allow(clippy::expect_used, clippy::cast_possible_truncation)]
// 测试里构造恶意输入需要直接做偏移与长度算术；这些是测试数据的构造过程，
// 不是库代码的运行路径，因此局部放宽而不放宽库级 lint。
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]

use omy_core::crypto::{Argon2Params, Kek};
use omy_core::header::{
    FIXED_HEADER_LEN, HEADER_LEN_FIELD_OFFSET, MAGIC_FILE, TLV_AREA_OFFSET, parse_limits,
};
use omy_core::tlv::decode_compression_index;
use omy_core::{EncryptOptions, Error, FixedHeader, RandomMaterial, encrypt};

/// 造一个结构合法的最小文件，供后续逐字段篡改。
fn baseline() -> Vec<u8> {
    let salt = [0x42u8; 16];
    let kek = Kek::from_password(b"pw", &salt, Argon2Params::TEST_WEAK).expect("kek");
    let opts = EncryptOptions { argon2: Argon2Params::TEST_WEAK, ..EncryptOptions::default() };
    encrypt(b"payload", &[kek], &salt, &opts, &RandomMaterial::generate())
        .expect("encrypt")
        .bytes
}

/// 写入一个 u32 小端序字段。
fn put_u32(buf: &mut [u8], offset: usize, v: u32) {
    buf[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
}

// ============================================================
// header_len / tlv_len 的分配攻击
// ============================================================

#[test]
fn absurd_header_len_is_rejected_without_allocating() {
    let mut f = baseline();
    // 声明 4 GiB 的头部。若实现照此分配，进程直接 OOM。
    put_u32(&mut f, HEADER_LEN_FIELD_OFFSET, u32::MAX);

    let err = FixedHeader::parse(&f).expect_err("必须拒绝超大 header_len");
    assert!(
        matches!(err, Error::MalformedHeader { .. }),
        "应报告头部畸形，实际: {err:?}"
    );
}

#[test]
fn header_len_just_over_limit_is_rejected() {
    let mut f = baseline();
    put_u32(&mut f, HEADER_LEN_FIELD_OFFSET, parse_limits::MAX_HEADER_LEN + 1);
    assert!(FixedHeader::parse(&f).is_err(), "刚超过上限也必须拒绝");
}

#[test]
fn absurd_tlv_len_is_rejected() {
    let mut f = baseline();
    // tlv_len 位于固定头偏移 88（见规范 §2 布局）
    const TLV_LEN_OFFSET: usize = 88;
    put_u32(&mut f, TLV_LEN_OFFSET, u32::MAX);

    let err = FixedHeader::parse(&f).expect_err("必须拒绝超大 tlv_len");
    assert!(matches!(err, Error::MalformedHeader { .. }), "实际: {err:?}");
}

#[test]
fn tlv_len_over_limit_is_rejected() {
    let mut f = baseline();
    const TLV_LEN_OFFSET: usize = 88;
    // 同时把 header_len 改成与之「自洽」的值，确保拦下它的是上界而非一致性检查
    let bad_tlv = parse_limits::MAX_TLV_LEN + 1;
    put_u32(&mut f, TLV_LEN_OFFSET, bad_tlv);
    let consistent_header = TLV_AREA_OFFSET as u32 + bad_tlv + 32;
    put_u32(&mut f, HEADER_LEN_FIELD_OFFSET, consistent_header);

    assert!(
        FixedHeader::parse(&f).is_err(),
        "即使 header_len 与 tlv_len 自洽，超限的 tlv_len 也必须被拒绝"
    );
}

// ============================================================
// chunk_size 攻击
// ============================================================

#[test]
fn oversized_chunk_size_is_rejected() {
    let mut f = baseline();
    // chunk_size 位于固定头偏移 68
    const CHUNK_SIZE_OFFSET: usize = 68;
    put_u32(&mut f, CHUNK_SIZE_OFFSET, u32::MAX);

    let err = FixedHeader::parse(&f).expect_err("必须拒绝超大 chunk_size");
    assert!(matches!(err, Error::MalformedHeader { .. }), "实际: {err:?}");
}

#[test]
fn zero_chunk_size_is_rejected() {
    let mut f = baseline();
    const CHUNK_SIZE_OFFSET: usize = 68;
    put_u32(&mut f, CHUNK_SIZE_OFFSET, 0);
    assert!(FixedHeader::parse(&f).is_err(), "chunk_size 为 0 会导致除零，必须拒绝");
}

#[test]
fn small_chunk_size_is_accepted() {
    // 与上面两项相对：小块是**合法**的，不能因「看起来不寻常」而拒绝。
    // 测试向量 v3/v4 就用 4096 / 2048 字节块。
    let mut f = baseline();
    const CHUNK_SIZE_OFFSET: usize = 68;
    put_u32(&mut f, CHUNK_SIZE_OFFSET, 2048);

    // 改了 chunk_size 会破坏 MAC，但这里只关心 FixedHeader::parse 不因范围而拒绝
    let h = FixedHeader::parse(&f).expect("2048 字节块是合法的格式取值");
    assert_eq!(h.chunk_size, 2048);
}

#[test]
fn chunk_size_at_parse_limit_is_accepted() {
    let mut f = baseline();
    const CHUNK_SIZE_OFFSET: usize = 68;
    put_u32(&mut f, CHUNK_SIZE_OFFSET, parse_limits::MAX_PARSE_CHUNK_SIZE);
    let h = FixedHeader::parse(&f).expect("恰好等于上限应被接受");
    assert_eq!(h.chunk_size, parse_limits::MAX_PARSE_CHUNK_SIZE);
}

// ============================================================
// 压缩索引表的分配攻击
// ============================================================

#[test]
fn index_with_absurd_count_is_rejected_before_allocating() {
    // 这是最危险的一处：entry_count 直接驱动 Vec::with_capacity。
    // 声明 0xFFFFFFFF 条 × 16 字节 = 64 GiB。
    let mut data = u32::MAX.to_le_bytes().to_vec();
    data.extend_from_slice(&[0u8; 16]); // 只给一条的数据

    let err = decode_compression_index(&data).expect_err("必须拒绝");
    assert!(matches!(err, Error::MalformedTlv { .. }), "实际: {err:?}");
}

#[test]
fn index_count_over_limit_is_rejected() {
    let count = parse_limits::MAX_INDEX_ENTRIES + 1;
    let data = count.to_le_bytes().to_vec();
    assert!(decode_compression_index(&data).is_err(), "超过条目上限必须拒绝");
}

#[test]
fn index_count_mismatching_data_length_is_rejected() {
    // 声明 100 条但只给 2 条的数据
    let mut data = 100u32.to_le_bytes().to_vec();
    data.extend_from_slice(&[0u8; 32]);
    assert!(
        decode_compression_index(&data).is_err(),
        "条目数与数据长度不符必须拒绝——否则会读到未初始化或越界数据"
    );
}

#[test]
fn index_with_trailing_garbage_is_rejected() {
    // 声明 1 条，但给了 1 条 + 多余字节。多余数据说明输入不可信。
    let mut data = 1u32.to_le_bytes().to_vec();
    data.extend_from_slice(&[0u8; 16]);
    data.extend_from_slice(b"extra");
    assert!(decode_compression_index(&data).is_err(), "尾部有多余数据必须拒绝");
}

#[test]
fn valid_index_still_decodes() {
    // 确认上界没有把合法输入也挡掉
    let mut data = 2u32.to_le_bytes().to_vec();
    // 第 1 条：offset=0, ct_len=100, plain_len=4096
    data.extend_from_slice(&0u64.to_le_bytes());
    data.extend_from_slice(&100u32.to_le_bytes());
    data.extend_from_slice(&4096u32.to_le_bytes());
    // 第 2 条
    data.extend_from_slice(&100u64.to_le_bytes());
    data.extend_from_slice(&50u32.to_le_bytes());
    data.extend_from_slice(&2048u32.to_le_bytes());

    let idx = decode_compression_index(&data).expect("合法索引必须能解码");
    assert_eq!(idx.len(), 2);
    assert_eq!(idx[0].plain_len, 4096);
    assert_eq!(idx[1].ct_offset, 100);
}

#[test]
fn empty_index_is_valid() {
    let data = 0u32.to_le_bytes().to_vec();
    let idx = decode_compression_index(&data).expect("零条目是合法的");
    assert!(idx.is_empty());
}

// ============================================================
// 截断与短输入
// ============================================================

#[test]
fn truncated_fixed_header_is_rejected() {
    let f = baseline();
    for cut in [0, 1, 8, 50, FIXED_HEADER_LEN - 1] {
        let err = FixedHeader::parse(&f[..cut]);
        assert!(err.is_err(), "{cut} 字节的输入必须被拒绝");
    }
}

#[test]
fn magic_only_file_is_rejected() {
    // 只有 magic、没有后续内容——扫描器会遇到的典型损坏文件
    let err = FixedHeader::parse(&MAGIC_FILE);
    assert!(err.is_err(), "只有 magic 不足以构成合法头部");
}

#[test]
fn all_zero_input_is_rejected() {
    let zeros = [0u8; 1024];
    let err = FixedHeader::parse(&zeros).expect_err("全零输入必须拒绝");
    assert!(matches!(err, Error::BadMagic { .. }), "应在 magic 阶段就拒绝，实际: {err:?}");
}

#[test]
fn random_input_is_rejected_at_magic() {
    // 扫描时会遇到大量普通文件，必须在 magic 阶段快速排除
    let mut junk = vec![0u8; 2048];
    for (i, b) in junk.iter_mut().enumerate() {
        *b = (i * 31 + 7) as u8;
    }
    let err = FixedHeader::parse(&junk).expect_err("随机数据必须拒绝");
    assert!(matches!(err, Error::BadMagic { .. }), "实际: {err:?}");
}

// ============================================================
// 一致性攻击
// ============================================================

#[test]
fn header_len_inconsistent_with_tlv_len_is_rejected() {
    let mut f = baseline();
    // 只改 header_len，让它与 tlv_len 不再满足 96+384+tlv_len+32
    put_u32(&mut f, HEADER_LEN_FIELD_OFFSET, 1000);
    let err = FixedHeader::parse(&f).expect_err("必须拒绝");
    assert!(matches!(err, Error::MalformedHeader { .. }), "实际: {err:?}");
}

#[test]
fn bogus_slot_count_is_rejected() {
    let mut f = baseline();
    // slot_count 位于固定头偏移 55
    const SLOT_COUNT_OFFSET: usize = 55;
    for bad in [0u8, 1, 7, 9, 255] {
        let mut g = f.clone();
        g[SLOT_COUNT_OFFSET] = bad;
        assert!(
            FixedHeader::parse(&g).is_err(),
            "slot_count={bad} 必须被拒绝：可否认性依赖固定 8 槽"
        );
    }
    // 确认 8 是被接受的
    f[SLOT_COUNT_OFFSET] = 8;
    assert!(FixedHeader::parse(&f).is_ok());
}

#[test]
fn unknown_kdf_id_is_rejected() {
    let mut f = baseline();
    const KDF_ID_OFFSET: usize = 53;
    f[KDF_ID_OFFSET] = 99;
    assert!(FixedHeader::parse(&f).is_err(), "未知 KDF 必须拒绝而非按默认处理");
}

#[test]
fn unknown_compress_id_is_rejected() {
    let mut f = baseline();
    const COMPRESS_ID_OFFSET: usize = 54;
    f[COMPRESS_ID_OFFSET] = 99;
    assert!(FixedHeader::parse(&f).is_err(), "未知压缩算法必须拒绝");
}

#[test]
fn nonzero_chunk_version_is_rejected() {
    let mut f = baseline();
    // chunk_version 位于固定头偏移 87
    const CHUNK_VERSION_OFFSET: usize = 87;
    f[CHUNK_VERSION_OFFSET] = 1;
    assert!(
        FixedHeader::parse(&f).is_err(),
        "v1 要求 chunk_version 为 0；非零说明使用了未来的就地编辑格式"
    );
}
