//! 流式加密与内存加密必须产出逐字节相同的文件。
//!
//! 流式路径先写占位头部、最后 seek 回去写真头部；任何一个 TLV 的顺序、长度
//! 或 MAC 覆盖范围与内存路径不同，都会让同一份输入得到不同的字节——这类差异
//! 不一定让文件打不开（自洽的头部照样能解），所以只测「能解密」不够，
//! 必须逐字节比较。

#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
#![allow(clippy::cast_possible_truncation, clippy::arithmetic_side_effects)]

use std::io::Cursor;

use omy_core::crypto::{Argon2Params, Fek, Kek};
use omy_core::file::{
    EncryptOptions, RandomMaterial, encrypt_stream_with_fek, encrypt_with_fek, open,
};
use omy_core::header::{MIN_CHUNK_SIZE, SLOT_AREA_LEN, SLOT_LEN};

fn salt() -> [u8; 16] {
    core::array::from_fn(|i| (i as u8).wrapping_mul(7) ^ 0x3D)
}

fn kek() -> Kek {
    Kek::from_password(b"stream", &salt(), Argon2Params::TEST_WEAK).unwrap()
}

/// 固定的随机材料：slot 填充也给定，输出才可逐字节比较。
fn rnd(keks: usize) -> RandomMaterial {
    let pad: Vec<u8> = (0..SLOT_AREA_LEN - keks * SLOT_LEN).map(|i| (i % 251) as u8).collect();
    RandomMaterial {
        file_uuid: core::array::from_fn(|i| (i as u8).wrapping_mul(13) ^ 0x51),
        base_nonce: core::array::from_fn(|i| (i as u8).wrapping_mul(29) ^ 0x7E),
        slot_padding: Some(pad),
    }
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| ((i * 31) % 253) as u8).collect()
}

fn both(plain: &[u8], opts: &EncryptOptions) -> (Vec<u8>, Vec<u8>) {
    let fek = Fek::random();
    let mem = encrypt_with_fek(plain, &[kek()], &salt(), opts, &rnd(1), &fek).unwrap();
    let mut out = Cursor::new(Vec::new());
    encrypt_stream_with_fek(
        &mut Cursor::new(plain), plain.len() as u64, &[kek()], &salt(), opts, &rnd(1), &fek,
        &mut out, None, None,
    )
    .unwrap();
    (mem.bytes, out.into_inner())
}

#[test]
fn stream_matches_memory_byte_for_byte() {
    let thumb: Vec<u8> = (0u32..900).map(|i| (i % 199) as u8).collect();
    for compress in [false, true] {
        for len in [0usize, 1, MIN_CHUNK_SIZE as usize, MIN_CHUNK_SIZE as usize * 3 + 17] {
            let opts = EncryptOptions {
                filename: Some("视频 片段.mp4".into()),
                preserve_extension: true,
                compress,
                chunk_size: MIN_CHUNK_SIZE,
                argon2: Argon2Params::TEST_WEAK,
                thumbnail: Some(thumb.clone()),
                ..EncryptOptions::default()
            };
            let (mem, streamed) = both(&data(len), &opts);
            assert_eq!(mem, streamed, "compress={compress} len={len}");
            let opened = open(&streamed, &[kek()]).unwrap();
            assert_eq!(opened.decrypt_all(&streamed).unwrap(), data(len));
        }
    }
}

/// 源比声明的短或长（加密过程中文件被改动）必须报错，不能写出头部与载荷
/// 不一致的文件。
#[test]
fn stream_rejects_length_mismatch() {
    let opts = EncryptOptions {
        chunk_size: MIN_CHUNK_SIZE,
        argon2: Argon2Params::TEST_WEAK,
        ..EncryptOptions::default()
    };
    let fek = Fek::random();
    let src = data(5000);
    for declared in [4000u64, 6000] {
        let mut out = Cursor::new(Vec::new());
        let r = encrypt_stream_with_fek(
            &mut Cursor::new(&src), declared, &[kek()], &salt(), &opts, &rnd(1), &fek,
            &mut out, None, None,
        );
        assert!(r.is_err(), "declared={declared} 必须报错");
    }
}

/// 容器索引最后定稿：占位与真值同长度时写入真值；长度不同报错。
#[test]
fn folder_index_is_finalized_after_payload() {
    let placeholder = vec![0u8; 64];
    let real: Vec<u8> = (0u8..64).collect();
    let opts = EncryptOptions {
        chunk_size: MIN_CHUNK_SIZE,
        argon2: Argon2Params::TEST_WEAK,
        folder_index: Some(placeholder),
        ..EncryptOptions::default()
    };
    let fek = Fek::random();
    let mut out = Cursor::new(Vec::new());
    let real2 = real.clone();
    encrypt_stream_with_fek(
        &mut Cursor::new(data(100)), 100, &[kek()], &salt(), &opts, &rnd(1), &fek,
        &mut out, None, Some(&mut || Ok(real2.clone())),
    )
    .unwrap();
    // 与「一开始就给真值」的内存版逐字节相同
    let mem_opts = EncryptOptions { folder_index: Some(real.clone()), ..opts.clone() };
    let mem = encrypt_with_fek(&data(100), &[kek()], &salt(), &mem_opts, &rnd(1), &fek).unwrap();
    assert_eq!(out.into_inner(), mem.bytes);

    let mut out2 = Cursor::new(Vec::new());
    let r = encrypt_stream_with_fek(
        &mut Cursor::new(data(100)), 100, &[kek()], &salt(), &opts, &rnd(1), &fek,
        &mut out2, None, Some(&mut || Ok(vec![1u8; 10])),
    );
    assert!(r.is_err(), "定稿长度与占位不同必须报错");
}

/// 流式打包 + 流式加密，与内存打包 + 内存加密逐字节相同。
///
/// 不这样会怎样：流式路径的文件顺序、偏移或逐文件哈希与内存路径不一致，
/// 产物照样能打开，但容器里的文件会张冠李戴或校验失败。
#[test]
fn streamed_folder_matches_packed_folder() {
    let root = std::env::temp_dir().join(format!("omy-stream-pack-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let src = root.join("相册");
    std::fs::create_dir_all(src.join("子目录")).unwrap();
    std::fs::create_dir_all(src.join("空目录")).unwrap();
    std::fs::write(src.join("a.txt"), data(3000)).unwrap();
    std::fs::write(src.join("子目录").join("b.bin"), data(MIN_CHUNK_SIZE as usize * 2 + 5)).unwrap();
    std::fs::write(src.join("z.empty"), b"").unwrap();

    let packed = omy_core::pack::pack_folder(&src, None).unwrap();
    let opts = EncryptOptions {
        chunk_size: MIN_CHUNK_SIZE,
        argon2: Argon2Params::TEST_WEAK,
        folder_index: Some(packed.index.encode()),
        ..EncryptOptions::default()
    };
    let fek = Fek::random();
    let mem = encrypt_with_fek(&packed.payload, &[kek()], &salt(), &opts, &rnd(1), &fek).unwrap();

    let plan = omy_core::pack::plan_folder(&src).unwrap();
    assert_eq!(plan.total_size, packed.payload.len() as u64);
    let mut reader = plan.reader();
    let hashes = reader.hashes();
    let stream_opts = EncryptOptions { folder_index: Some(plan.placeholder_index().unwrap()), ..opts.clone() };
    let mut out = Cursor::new(Vec::new());
    encrypt_stream_with_fek(
        &mut reader, plan.total_size, &[kek()], &salt(), &stream_opts, &rnd(1), &fek,
        &mut out, None,
        Some(&mut || Ok(plan.finish(&hashes.borrow())?.encode())),
    )
    .unwrap();
    assert_eq!(out.into_inner(), mem.bytes);
    let _ = std::fs::remove_dir_all(&root);
}
