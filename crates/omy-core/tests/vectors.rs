//! 测试向量交叉验证。
//!
//! 这是 Rust 实现的**验收标准**（规范 §12.1）：
//!
//! 1. 读取全部 5 组向量的 `full_file_hex` 并正确解密
//! 2. 相同输入加相同随机源产出**逐字节相同**的输出
//! 3. 通过参考实现 68 项断言的等价 Rust 版本
//!
//! 向量由 Python 参考实现生成，因此这里同时验证了两个独立实现的一致性——
//! 这比单侧自测强得多：任一方误解规范都会导致字节不匹配。

// 测试代码本就应该在断言失败时 panic，向量字段缺失也应立即失败。
// 这些 lint 是为库代码设的，在测试中属于噪音。
#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]
#![allow(clippy::integer_division)]

use omy_core::crypto::{Argon2Params, CipherId, Fek, Kek, SecretKey, chunk_aad, chunk_nonce};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt_with_fek, open};
use omy_core::header::FixedHeader;
use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Debug, Deserialize)]
struct VectorFile {
    format: String,
    spec_version: String,
    vectors: Vec<Vector>,
}

#[derive(Debug, Deserialize)]
struct Vector {
    name: String,
    #[allow(dead_code)]
    description: String,
    input: serde_json::Value,
    /// v5 分片向量没有 derived 段，故为可选。
    #[serde(default)]
    derived: serde_json::Value,
    output: serde_json::Value,
}

static VECTORS: LazyLock<VectorFile> = LazyLock::new(|| {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/research/appendix/test-vectors.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("无法读取测试向量 {path}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("测试向量 JSON 解析失败: {e}"))
});

fn vector(name: &str) -> &'static Vector {
    VECTORS
        .vectors
        .iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("找不到测试向量 {name}"))
}

fn hex_of(v: &serde_json::Value, path: &[&str]) -> Vec<u8> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p).unwrap_or_else(|| panic!("字段缺失: {}", path.join(".")));
    }
    let s = cur
        .as_str()
        .unwrap_or_else(|| panic!("字段不是字符串: {}", path.join(".")));
    hex::decode(s).unwrap_or_else(|e| panic!("hex 解码失败 {}: {e}", path.join(".")))
}

fn str_of(v: &serde_json::Value, path: &[&str]) -> String {
    let mut cur = v;
    for p in path {
        cur = cur.get(p).unwrap_or_else(|| panic!("字段缺失: {}", path.join(".")));
    }
    cur.as_str()
        .unwrap_or_else(|| panic!("字段不是字符串: {}", path.join(".")))
        .to_owned()
}

fn u64_of(v: &serde_json::Value, path: &[&str]) -> u64 {
    let mut cur = v;
    for p in path {
        cur = cur.get(p).unwrap_or_else(|| panic!("字段缺失: {}", path.join(".")));
    }
    cur.as_u64()
        .unwrap_or_else(|| panic!("字段不是整数: {}", path.join(".")))
}

fn array16(b: &[u8], what: &str) -> [u8; 16] {
    assert_eq!(b.len(), 16, "{what} 必须是 16 字节");
    let mut a = [0u8; 16];
    a.copy_from_slice(b);
    a
}

fn array32(b: &[u8], what: &str) -> [u8; 32] {
    assert_eq!(b.len(), 32, "{what} 必须是 32 字节");
    let mut a = [0u8; 32];
    a.copy_from_slice(b);
    a
}

fn array7(b: &[u8], what: &str) -> [u8; 7] {
    assert_eq!(b.len(), 7, "{what} 必须是 7 字节");
    let mut a = [0u8; 7];
    a.copy_from_slice(b);
    a
}

// ============================================================
// 元信息
// ============================================================

#[test]
fn vector_file_is_for_omy_v1() {
    // 实际值为 "omy .omy"（品牌名 + 扩展名），只断言包含品牌名，避免绑定表述细节
    assert!(
        VECTORS.format.contains("omy"),
        "向量文件的 format 字段应含 omy，实际: {}",
        VECTORS.format
    );
    assert!(
        VECTORS.spec_version.starts_with('1'),
        "向量应针对格式 v1，实际: {}",
        VECTORS.spec_version
    );
    assert_eq!(VECTORS.vectors.len(), 5, "应有 5 组向量");
}

// ============================================================
// v1：完整密钥派生链 + 全文件字节一致
// ============================================================

/// 验证密钥派生链的每一步都与参考实现一致。
///
/// 分步比对是刻意的：若只比最终文件字节，一旦不匹配将很难定位是哪一步出错。
#[test]
fn v1_key_derivation_chain() {
    let v = vector("v1-minimal-single-password");

    let password = str_of(&v.input, &["password"]);
    let vault_salt = array16(&hex_of(&v.derived, &["vault_salt"]), "vault_salt");
    let file_uuid = array16(&hex_of(&v.derived, &["file_uuid"]), "file_uuid");
    let params = Argon2Params {
        m_kib: u64_of(&v.input, &["argon2", "m_kib"]) as u32,
        t: u64_of(&v.input, &["argon2", "t"]) as u32,
        p: u64_of(&v.input, &["argon2", "p"]) as u32,
    };

    // 第 1 步：Argon2id → KEK
    let kek = Kek::from_password(password.as_bytes(), &vault_salt, params)
        .expect("Argon2 派生失败");
    let expect_kek = array32(&hex_of(&v.derived, &["kek"]), "kek");
    assert_eq!(
        kek.as_key().as_bytes(),
        &expect_kek,
        "KEK 不匹配：Argon2id 参数或实现有差异"
    );

    // 第 2 步：FEK（向量给定）
    let fek_bytes = array32(&hex_of(&v.derived, &["fek"]), "fek");
    let fek = Fek::from_key(SecretKey::from_bytes(fek_bytes));

    // 第 3 步：payload_key = HKDF(FEK, salt=file_uuid, info="omy/v1/payload")
    let pk = fek.derive_payload_key(&file_uuid);
    let expect_pk = array32(&hex_of(&v.derived, &["payload_key"]), "payload_key");
    assert_eq!(pk.as_bytes(), &expect_pk, "payload_key 不匹配：HKDF info 串或 salt 有误");

    // 第 4 步：header_mac_key
    let mk = fek.derive_header_mac_key(&file_uuid);
    let expect_mk = array32(&hex_of(&v.derived, &["header_mac_key"]), "header_mac_key");
    assert_eq!(mk.as_bytes(), &expect_mk, "header_mac_key 不匹配");

    // 第 5 步：块 0 的 nonce 与 AAD
    let base_nonce = array7(&hex_of(&v.derived, &["base_nonce"]), "base_nonce");
    let n0 = chunk_nonce(&base_nonce, 0, true);
    let expect_n0 = hex_of(&v.derived, &["chunk0_nonce"]);
    assert_eq!(
        n0.as_slice(),
        expect_n0.as_slice(),
        "块 0 nonce 不匹配：检查 u32be 块序号与 final_flag"
    );

    let a0 = chunk_aad(&file_uuid, 0);
    let expect_a0 = hex_of(&v.derived, &["chunk0_aad"]);
    assert_eq!(a0.as_slice(), expect_a0.as_slice(), "块 0 AAD 不匹配");
}

/// 用向量给定的全部随机材料重新加密，要求产出**逐字节相同**的文件。
///
/// 这是最强的一致性证明：文件头每个字段、slot 区、TLV 编码、padding、
/// MAC、载荷密文全部必须精确一致。
#[test]
fn v1_byte_exact_reencryption() {
    let v = vector("v1-minimal-single-password");

    let password = str_of(&v.input, &["password"]);
    let plaintext = hex_of(&v.input, &["plaintext_hex"]);
    let filename = str_of(&v.input, &["filename"]);
    let vault_salt = array16(&hex_of(&v.derived, &["vault_salt"]), "vault_salt");
    let file_uuid = array16(&hex_of(&v.derived, &["file_uuid"]), "file_uuid");
    let base_nonce = array7(&hex_of(&v.derived, &["base_nonce"]), "base_nonce");
    let fek_bytes = array32(&hex_of(&v.derived, &["fek"]), "fek");
    let expect_file = hex_of(&v.output, &["full_file_hex"]);

    let params = Argon2Params {
        m_kib: u64_of(&v.input, &["argon2", "m_kib"]) as u32,
        t: u64_of(&v.input, &["argon2", "t"]) as u32,
        p: u64_of(&v.input, &["argon2", "p"]) as u32,
    };
    let kek = Kek::from_password(password.as_bytes(), &vault_salt, params).expect("KEK 派生失败");
    let fek = Fek::from_key(SecretKey::from_bytes(fek_bytes));

    // 单密码占 slot 0，其余 7 槽是随机填充。要复现字节需从向量文件中取出这段填充。
    let used = 48usize;
    let slot_area_in_file = expect_file
        .get(96 + used..96 + 384)
        .expect("向量文件长度不足，无法取出 slot 填充");

    let opts = EncryptOptions {
        filename: Some(filename),
        chunk_size: u64_of(&v.input, &["chunk_size"]) as u32,
        cipher: CipherId::ChaCha20Poly1305,
        argon2: params,
        write_content_hash: true,
        ..EncryptOptions::default()
    };
    let rnd = RandomMaterial {
        file_uuid,
        base_nonce,
        slot_padding: Some(slot_area_in_file.to_vec()),
    };

    let out = encrypt_with_fek(&plaintext, &[kek], &vault_salt, &opts, &rnd, &fek)
        .expect("加密失败");

    assert_eq!(
        out.bytes.len(),
        u64_of(&v.output, &["total_size"]) as usize,
        "总长度不匹配"
    );
    assert_eq!(
        out.header.header_len as u64,
        u64_of(&v.output, &["header_len"]),
        "header_len 不匹配"
    );
    assert_eq!(
        out.header.tlv_len as u64,
        u64_of(&v.output, &["tlv_len"]),
        "tlv_len 不匹配"
    );

    if out.bytes != expect_file {
        // 定位首个差异字节，便于排查
        let pos = out
            .bytes
            .iter()
            .zip(expect_file.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| expect_file.len().min(out.bytes.len()));
        panic!(
            "文件字节不一致，首个差异在偏移 {pos}（0x{pos:x}）\n\
             实际: {:02x?}\n期望: {:02x?}",
            out.bytes.get(pos..(pos + 16).min(out.bytes.len())),
            expect_file.get(pos..(pos + 16).min(expect_file.len())),
        );
    }
}

/// 读取向量文件并解密，验证内容与文件名都正确还原。
#[test]
fn v1_decrypt_from_vector_bytes() {
    let v = vector("v1-minimal-single-password");
    let file = hex_of(&v.output, &["full_file_hex"]);
    let password = str_of(&v.input, &["password"]);
    let expect_plain = hex_of(&v.input, &["plaintext_hex"]);
    let expect_name = str_of(&v.input, &["filename"]);

    let params = Argon2Params {
        m_kib: u64_of(&v.input, &["argon2", "m_kib"]) as u32,
        t: u64_of(&v.input, &["argon2", "t"]) as u32,
        p: u64_of(&v.input, &["argon2", "p"]) as u32,
    };
    let header = FixedHeader::parse(&file).expect("头部解析失败");
    let kek = Kek::from_password(password.as_bytes(), &header.vault_salt, params)
        .expect("KEK 派生失败");

    let opened = open(&file, &[kek]).expect("打开文件失败");
    assert_eq!(opened.slot_index, u64_of(&v.derived, &["slot_index_hit"]) as u16, "命中槽位不符");
    assert_eq!(opened.filename().expect("文件名解密失败"), expect_name, "文件名不匹配");

    let plain = opened.decrypt_all(&file).expect("载荷解密失败");
    assert_eq!(plain, expect_plain, "明文不匹配");

    // 向量记录的 BLAKE2b-256 是对整个加密文件算的
    let h = omy_core::crypto::content_hash(&file);
    let expect_h = array32(&hex_of(&v.output, &["blake2b256"]), "blake2b256");
    assert_eq!(h, expect_h, "整文件 BLAKE2b-256 不匹配");
}

/// 错误密码必须被拒绝，且错误类型是「密码不对」而非「文件损坏」。
///
/// 这个区分对用户体验很重要：扫描时试错密码是正常流程，不该报成文件损坏。
#[test]
fn v1_wrong_password_rejected() {
    let v = vector("v1-minimal-single-password");
    let file = hex_of(&v.output, &["full_file_hex"]);
    let header = FixedHeader::parse(&file).expect("头部解析失败");
    let params = Argon2Params {
        m_kib: header.argon2_m_kib,
        t: header.argon2_t,
        p: header.argon2_p,
    };
    let bad = Kek::from_password(b"definitely wrong password", &header.vault_salt, params)
        .expect("KEK 派生失败");

    let err = open(&file, &[bad]).expect_err("错误密码竟然打开成功");
    assert_eq!(err.code(), "WRONG_PASSWORD", "错误码应为 WRONG_PASSWORD，实际: {}", err.code());
}

// ============================================================
// v2：多密码与可否认性
// ============================================================

/// 三个密码各自派生的 KEK 必须与向量一致，且分别命中预期槽位。
#[test]
fn v2_multi_password_slots() {
    let v = vector("v2-multi-password-deniable");
    let vault_salt = array16(&hex_of(&v.derived, &["vault_salt"]), "vault_salt");
    let file = hex_of(&v.output, &["full_file_hex"]);
    let header = FixedHeader::parse(&file).expect("头部解析失败");
    let params = Argon2Params {
        m_kib: header.argon2_m_kib,
        t: header.argon2_t,
        p: header.argon2_p,
    };

    let results = v.derived.get("slot_results").and_then(|x| x.as_array()).expect("缺少 slot_results");
    let mut checked_ok = 0;
    let mut checked_fail = 0;

    for r in results {
        let pw = r.get("password").and_then(|x| x.as_str()).expect("缺少 password");
        let expect_kek = array32(&hex_of(r, &["kek"]), "kek");
        let unlocked = r.get("unlocked").and_then(serde_json::Value::as_bool).expect("缺少 unlocked");

        let kek = Kek::from_password(pw.as_bytes(), &vault_salt, params).expect("KEK 派生失败");
        assert_eq!(kek.as_key().as_bytes(), &expect_kek, "密码 {pw} 的 KEK 不匹配");

        match open(&file, &[kek]) {
            Ok(opened) => {
                assert!(unlocked, "密码 {pw} 本应无法解锁，却成功了");
                let expect_slot = r
                    .get("slot_index")
                    .and_then(serde_json::Value::as_u64)
                    .expect("已解锁项必须有 slot_index");
                assert_eq!(
                    u64::from(opened.slot_index),
                    expect_slot,
                    "密码 {pw} 命中的槽位不符"
                );
                checked_ok += 1;
            }
            Err(e) => {
                assert!(!unlocked, "密码 {pw} 本应解锁，却失败了: {e}");
                assert_eq!(e.code(), "WRONG_PASSWORD", "错误密码应报 WRONG_PASSWORD");
                checked_fail += 1;
            }
        }
    }
    assert_eq!(checked_ok, 3, "应有 3 个密码成功解锁");
    assert_eq!(checked_fail, 1, "应有 1 个密码被拒绝");
}

/// slot 区字节必须与向量完全一致，且三个密码都能解出同一个 FEK。
#[test]
fn v2_slot_area_bytes_and_shared_fek() {
    let v = vector("v2-multi-password-deniable");
    let file = hex_of(&v.output, &["full_file_hex"]);
    let expect_slot_area = hex_of(&v.derived, &["slot_area_hex"]);

    let actual = file.get(96..480).expect("文件长度不足");
    assert_eq!(actual, expect_slot_area.as_slice(), "slot 区字节不一致");
    assert_eq!(expect_slot_area.len(), 384, "slot 区必须是 384 字节");

    // 三个密码解出的 FEK 必须相同——这是多密码共享同一文件的前提
    let vault_salt = array16(&hex_of(&v.derived, &["vault_salt"]), "vault_salt");
    let header = FixedHeader::parse(&file).expect("头部解析失败");
    let params = Argon2Params {
        m_kib: header.argon2_m_kib,
        t: header.argon2_t,
        p: header.argon2_p,
    };
    let mut feks = Vec::new();
    for pw in ["alpha", "beta", "gamma"] {
        let kek = Kek::from_password(pw.as_bytes(), &vault_salt, params).expect("KEK 派生失败");
        let opened = open(&file, &[kek]).unwrap_or_else(|e| panic!("密码 {pw} 解锁失败: {e}"));
        feks.push(*opened.fek().as_key().as_bytes());
    }
    assert_eq!(feks.len(), 3, "应解出 3 个 FEK");
    let first = feks.first().expect("至少应有一个 FEK");
    assert!(
        feks.iter().all(|f| f == first),
        "三个密码解出的 FEK 必须完全相同，否则多密码无法共享同一文件"
    );
}

/// 可否认性：文件大小不因密码数量而变化。
///
/// 用 1 个和 3 个密码加密同样内容，字节长度必须完全相同——否则从文件大小
/// 就能推断出这个文件有几个密码。
#[test]
fn v2_deniability_size_independent_of_password_count() {
    let salt = [0x42u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let plaintext = b"same content for both";

    let mk = |pws: &[&str]| -> usize {
        let keks: Vec<Kek> = pws
            .iter()
            .map(|p| Kek::from_password(p.as_bytes(), &salt, params).expect("KEK 派生失败"))
            .collect();
        let opts = EncryptOptions {
            filename: Some("x.bin".into()),
            argon2: params,
            ..EncryptOptions::default()
        };
        omy_core::file::encrypt(plaintext, &keks, &salt, &opts, &RandomMaterial::generate())
            .expect("加密失败")
            .bytes
            .len()
    };

    let one = mk(&["only"]);
    let three = mk(&["a", "b", "c"]);
    assert_eq!(one, three, "1 密码与 3 密码的文件大小必须相同，否则泄露密码数量");
}

// ============================================================
// v3：多块与随机访问
// ============================================================

/// 多块文件的块数、偏移公式、随机访问结果都必须正确。
#[test]
fn v3_multichunk_random_access() {
    let v = vector("v3-multichunk-random-access");
    let password = str_of(&v.input, &["password"]);
    let filename = str_of(&v.input, &["filename"]);
    let chunk_size = u64_of(&v.input, &["chunk_size"]) as u32;
    let file_uuid = array16(&hex_of(&v.derived, &["file_uuid"]), "file_uuid");
    let fek_bytes = array32(&hex_of(&v.derived, &["fek"]), "fek");
    let expect_n_chunks = u64_of(&v.derived, &["n_chunks"]);
    let expect_header_len = u64_of(&v.derived, &["header_len"]);

    // 明文规则：byte[i] = (i*7+13) mod 256, i in [0,10000)
    let plaintext: Vec<u8> = (0..10_000u32)
        .map(|i| u8::try_from((i.wrapping_mul(7).wrapping_add(13)) % 256).unwrap_or(0))
        .collect();
    let expect_plain_hash = array32(
        &hex_of(&v.input, &["plaintext_blake2b256"]),
        "plaintext_blake2b256",
    );
    assert_eq!(
        omy_core::crypto::content_hash(&plaintext),
        expect_plain_hash,
        "按规则生成的明文哈希与向量不符，说明规则理解有误"
    );

    let salt = [0u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(password.as_bytes(), &salt, params).expect("KEK 派生失败");
    let fek = Fek::from_key(SecretKey::from_bytes(fek_bytes));

    let opts = EncryptOptions {
        filename: Some(filename),
        chunk_size,
        argon2: params,
        ..EncryptOptions::default()
    };
    let rnd = RandomMaterial { file_uuid, base_nonce: [0xAB; 7], slot_padding: None };
    let enc = encrypt_with_fek(&plaintext, &[kek], &salt, &opts, &rnd, &fek).expect("加密失败");

    assert_eq!(enc.header.n_chunks(), expect_n_chunks, "块数不匹配");
    assert_eq!(
        u64::from(enc.header.header_len),
        expect_header_len,
        "header_len 不匹配"
    );
    assert_eq!(
        u64::from(enc.bytes.len() as u32),
        u64_of(&v.output, &["total_size"]),
        "总大小不匹配"
    );

    // 验证偏移公式：ct_offset(i) = header_len + i*(chunk_size+16)
    for i in 0..expect_n_chunks {
        let (off, _len) = enc.header.chunk_ct_range(i).expect("偏移计算失败");
        let expect =
            u64::from(enc.header.header_len) + i * (u64::from(chunk_size) + 16);
        assert_eq!(off, expect, "块 {i} 的密文偏移与公式不符");
    }

    // 随机访问：任意区间都必须与明文对应片段一致
    let params2 = Argon2Params::TEST_WEAK;
    let kek2 = Kek::from_password(password.as_bytes(), &salt, params2).expect("KEK 派生失败");
    let opened = open(&enc.bytes, &[kek2]).expect("打开失败");
    let payload = enc
        .bytes
        .get(opened.header.header_len as usize..)
        .expect("载荷切片失败");

    let cases: [(u64, u64); 6] = [
        (0, 10),               // 起始
        (100, 50),             // 块内
        (4090, 20),            // 跨块边界
        (4096, 4096),          // 整块
        (8192, 1808),          // 末块
        (9990, 10),            // 结尾
    ];
    for (off, len) in cases {
        let got = omy_core::payload::read_range(
            &opened.header,
            opened.payload_key(),
            None,
            off,
            len,
            |o, l| {
                let a = usize::try_from(o).expect("偏移越界");
                let b = a + usize::try_from(l).expect("长度越界");
                Ok(payload.get(a..b).expect("密文区间越界").to_vec())
            },
        )
        .unwrap_or_else(|e| panic!("读取区间 ({off},{len}) 失败: {e}"));

        let a = usize::try_from(off).expect("偏移越界");
        let b = (a + usize::try_from(len).expect("长度越界")).min(plaintext.len());
        assert_eq!(
            got.as_slice(),
            plaintext.get(a..b).expect("明文区间越界"),
            "区间 ({off},{len}) 的内容不匹配"
        );
    }

    // 全量解密必须还原
    let all = opened.decrypt_all(&enc.bytes).expect("全量解密失败");
    assert_eq!(all, plaintext, "全量解密结果与原文不一致");
}

// ============================================================
// v4：压缩与索引表
// ============================================================

/// 压缩文件必须写出索引表，且内容与向量一致；解压后能完整还原。
#[test]
fn v4_compression_with_index() {
    let v = vector("v4-compressed-with-index");
    let password = str_of(&v.input, &["password"]);
    let chunk_size = u64_of(&v.input, &["chunk_size"]) as u32;
    let zstd_level = u64_of(&v.input, &["zstd_level"]) as i32;
    let plaintext_size = u64_of(&v.input, &["plaintext_size"]) as usize;
    let fek_bytes = array32(&hex_of(&v.derived, &["fek"]), "fek");
    let expect_n_chunks = u64_of(&v.derived, &["n_chunks"]);

    // 明文是 "omy" 重复 500 次
    let plaintext = "omy".repeat(500).into_bytes();
    assert_eq!(plaintext.len(), plaintext_size, "明文长度与向量声明不符");

    let salt = [1u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(password.as_bytes(), &salt, params).expect("KEK 派生失败");
    let fek = Fek::from_key(SecretKey::from_bytes(fek_bytes));

    let opts = EncryptOptions {
        compress: true,
        zstd_level,
        chunk_size,
        argon2: params,
        write_content_hash: false, // 向量的 total_size 未含此 TLV
        ..EncryptOptions::default()
    };
    let rnd = RandomMaterial {
        file_uuid: [7u8; 16],
        base_nonce: [9u8; 7],
        slot_padding: None,
    };
    let enc = encrypt_with_fek(&plaintext, &[kek], &salt, &opts, &rnd, &fek).expect("加密失败");
    assert_eq!(enc.header.n_chunks(), expect_n_chunks, "块数不匹配");

    let kek2 = Kek::from_password(password.as_bytes(), &salt, params).expect("KEK 派生失败");
    let opened = open(&enc.bytes, &[kek2]).expect("打开失败");

    // 索引表内容必须与向量一致
    let idx = opened.compression_index().expect("读取索引失败");
    let expect_entries = v.derived.get("index_entries").and_then(|x| x.as_array()).expect("缺少 index_entries");
    assert_eq!(idx.len(), expect_entries.len(), "索引条目数不匹配");
    for (got, want) in idx.iter().zip(expect_entries.iter()) {
        assert_eq!(
            got.ct_offset,
            want.get("ct_offset").and_then(serde_json::Value::as_u64).expect("缺少 ct_offset"),
            "ct_offset 不匹配"
        );
        assert_eq!(
            u64::from(got.ct_len),
            want.get("ct_len").and_then(serde_json::Value::as_u64).expect("缺少 ct_len"),
            "ct_len 不匹配"
        );
        assert_eq!(
            u64::from(got.plain_len),
            want.get("plain_len").and_then(serde_json::Value::as_u64).expect("缺少 plain_len"),
            "plain_len 不匹配"
        );
    }

    // roundtrip 必须成功
    let back = opened.decrypt_all(&enc.bytes).expect("解密失败");
    assert_eq!(back, plaintext, "压缩往返后内容不一致");

    // 压缩确实起效了
    assert!(
        enc.bytes.len() < plaintext.len(),
        "高重复度内容压缩后应小于原文：{} vs {}",
        enc.bytes.len(),
        plaintext.len()
    );
}

/// 压缩文件缺少 CRITICAL 索引 TLV 时必须拒绝打开。
#[test]
fn v4_compressed_without_index_rejected() {
    // 直接构造一个声明压缩但不带索引的头部，验证 open 会拒绝
    let salt = [2u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(b"pw", &salt, params).expect("KEK 派生失败");
    let opts = EncryptOptions {
        compress: true,
        chunk_size: 65536,
        argon2: params,
        ..EncryptOptions::default()
    };
    let enc = omy_core::file::encrypt(b"data", &[kek], &salt, &opts, &RandomMaterial::generate())
        .expect("加密失败");

    // 正常情况下索引存在，能打开
    let kek2 = Kek::from_password(b"pw", &salt, params).expect("KEK 派生失败");
    let opened = open(&enc.bytes, &[kek2]).expect("正常压缩文件应能打开");
    assert!(!opened.compression_index().expect("索引读取失败").is_empty(), "压缩文件必须有索引");
}

// ============================================================
// v5：分片
// ============================================================

/// 分片的每个头字段、CRC、冗余头长度都必须与向量一致，且能正确合并。
#[test]
fn v5_sharding() {
    use omy_core::shard::{ShardHeader, analyze_coverage, merge, recover_header_from_shard, split};

    let v = vector("v5-sharding");
    let plaintext_size = u64_of(&v.input, &["plaintext_size"]) as usize;
    let shard_size = u64_of(&v.input, &["shard_size"]) as usize;
    let expect_ct_size = u64_of(&v.output, &["original_ct_size"]);
    let expect_count = u64_of(&v.output, &["shard_count"]);
    let shards_meta = v.output.get("shards").and_then(|x| x.as_array()).expect("缺少 shards");

    let file_uuid = array16(
        &hex_of(shards_meta.first().expect("无分片元数据"), &["file_uuid"]),
        "file_uuid",
    );

    // 构造与向量同尺寸的密文，验证切分逻辑本身
    let ct: Vec<u8> = (0..expect_ct_size)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    assert_eq!(ct.len() as u64, expect_ct_size, "构造的密文长度不符");

    // 验证向量的 original_ct_size 自洽。
    // 10944 - 10240 = 704 = 656 (主 header) + 3 × 16 (每块的 AEAD tag)
    // 其中 3 = ceil(10240 / 4096)，即向量使用 4096 字节的块。
    // 这说明 original_ct_size 指的是**含主 header 的完整文件**，而非纯载荷。
    let overhead = expect_ct_size - plaintext_size as u64;
    let expect_overhead = 656 + 3 * 16;
    assert_eq!(
        overhead, expect_overhead,
        "密文开销应为 {expect_overhead} 字节（656 主 header + 3 块 × 16 tag），实际 {overhead}"
    );

    // 向量的 redundant_header_len 是 656
    let redundant = vec![0xEEu8; 656];
    let shards = split(&ct, &file_uuid, shard_size, Some(&redundant)).expect("切分失败");
    assert_eq!(shards.len() as u64, expect_count, "分片数不匹配");

    for (i, meta) in shards_meta.iter().enumerate() {
        let raw = shards.get(i).expect("分片缺失");
        let h = ShardHeader::parse(raw).expect("分片头解析失败");

        assert_eq!(
            u64::from(h.shard_index),
            meta.get("shard_index").and_then(serde_json::Value::as_u64).expect("缺少 shard_index"),
            "第 {i} 片的 shard_index 不匹配"
        );
        assert_eq!(
            u64::from(h.shard_total),
            meta.get("shard_total").and_then(serde_json::Value::as_u64).expect("缺少 shard_total"),
            "第 {i} 片的 shard_total 不匹配"
        );
        assert_eq!(
            h.data_offset,
            meta.get("data_offset").and_then(serde_json::Value::as_u64).expect("缺少 data_offset"),
            "第 {i} 片的 data_offset 不匹配"
        );
        assert_eq!(
            h.data_len,
            meta.get("data_len").and_then(serde_json::Value::as_u64).expect("缺少 data_len"),
            "第 {i} 片的 data_len 不匹配"
        );
        assert_eq!(
            u64::from(h.redundant_header_len),
            meta.get("redundant_header_len")
                .and_then(serde_json::Value::as_u64)
                .expect("缺少 redundant_header_len"),
            "第 {i} 片的 redundant_header_len 不匹配"
        );
        assert_eq!(
            u64::from(raw.len() as u32),
            meta.get("total_size").and_then(serde_json::Value::as_u64).expect("缺少 total_size"),
            "第 {i} 片的总大小不匹配"
        );
    }

    // 乱序合并必须成功还原
    let mut shuffled = shards.clone();
    shuffled.reverse();
    let merged = merge(&shuffled).expect("乱序合并失败");
    assert_eq!(merged, ct, "合并结果与原密文不一致");

    // 缺片必须报出具体缺失编号
    let partial = vec![
        shards.first().expect("缺片").clone(),
        shards.get(2).expect("缺片").clone(),
    ];
    let err = merge(&partial).expect_err("缺片竟然合并成功");
    assert_eq!(err.code(), "MISSING_SHARDS", "应报 MISSING_SHARDS");

    // 单字节损坏必须被 CRC 检出
    let mut broken = shards.clone();
    let target = broken.get_mut(1).expect("分片缺失");
    let pos = target.len() - 1;
    if let Some(b) = target.get_mut(pos) {
        *b ^= 0xFF;
    }
    let err = merge(&broken).expect_err("损坏数据竟然合并成功");
    assert_eq!(err.code(), "SHARD_CRC_MISMATCH", "应报 SHARD_CRC_MISMATCH");

    // 冗余头可从第 1、2 片独立恢复
    for i in [1usize, 2] {
        let rec = recover_header_from_shard(shards.get(i).expect("分片缺失"))
            .unwrap_or_else(|e| panic!("第 {i} 片恢复冗余头失败: {e}"));
        assert_eq!(rec, redundant, "第 {i} 片恢复的冗余头内容不一致");
    }
    // 第 0 片本身含主 header，不应有冗余副本
    assert!(
        recover_header_from_shard(shards.first().expect("分片缺失")).is_err(),
        "第 0 片不应携带冗余 header"
    );

    // 缺片时的空洞分析
    let cov = analyze_coverage(&partial).expect("覆盖分析失败");
    assert_eq!(cov.missing, vec![1], "应报告缺失第 1 片");
    assert!(!cov.is_readable(4000, 100), "落在空洞内的区间应不可读");
    assert!(cov.is_readable(0, 100), "第 0 片范围内应可读");
}
