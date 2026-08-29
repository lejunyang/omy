//! 实测扫描性能，验证两级 KDF 的可行性声明。
//!
//! 设计文档称：Argon2id 约 180 ms，HKDF 约 13 µs，比值约 13,700 倍，
//! 因此扫描 10,000 文件从 2.5 小时降到 3.4 秒。
//!
//! 这个数字决定了「多密码自动扫描」这个核心功能到底可不可行，必须实测而非引用。
//!
//! 用法：`cargo run --release --example bench_scan -- [文件数]`

// 这是性能测量工具，算术用于统计换算而非处理不可信输入，
// 因此局部放宽相关 lint，不影响库代码的严格检查。
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omy_core::crypto::{Argon2Params, Kek};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};
use omy_core::header::MIN_CHUNK_SIZE;
use omy_core::scan::{ScanOptions, scan_dir};
use omy_core::session::SessionKeys;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    let dir = std::env::temp_dir().join(format!(
        "omy_bench_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir)?;

    let salt = [0x33u8; 16];
    // 用生产级参数，否则测不出真实差距
    let params = Argon2Params::INTERACTIVE;

    println!("=== 1. Argon2id 单次耗时（生产参数 m={} KiB, t={}）===",
             params.m_kib, params.t);
    let t0 = Instant::now();
    let kek = Kek::from_password(b"the-real-password", &salt, params)?;
    let argon_ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!("    {argon_ms:.1} ms");

    println!("\n=== 2. HKDF 单次耗时（每文件每槽都要做）===");
    let uuid = [0x77u8; 16];
    let rounds = 100_000;
    let t1 = Instant::now();
    let mut sink = 0u8;
    for i in 0..rounds {
        let k = kek.derive_slot_key(&uuid, (i % 8) as u16);
        sink ^= k.as_bytes()[0];
    }
    let hkdf_us = t1.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(rounds);
    println!("    {hkdf_us:.2} µs  (校验和 {sink}，防止被优化掉)");
    println!("    比值: {:.0}×", argon_ms * 1000.0 / hkdf_us);

    println!("\n=== 3. 生成 {n} 个测试文件 ===");
    // 3 个不同密码，只有第 1 个会被解锁，其余作为「不属于当前密码」的对照
    let other1 = Kek::from_password(b"other-password-1", &salt, params)?;
    let other2 = Kek::from_password(b"other-password-2", &salt, params)?;

    let opts = EncryptOptions {
        filename: Some("scanned.dat".into()),
        chunk_size: MIN_CHUNK_SIZE,
        argon2: params,
        write_content_hash: false,
        ..EncryptOptions::default()
    };
    let payload = vec![0x5Au8; 512];

    let t2 = Instant::now();
    let mut mine = 0usize;
    for i in 0..n {
        // 1/3 用我的密码，2/3 用别的密码
        let k = match i % 3 {
            0 => {
                mine += 1;
                Kek::from_password(b"the-real-password", &salt, params)?
            }
            1 => Kek::from_password(b"other-password-1", &salt, params)?,
            _ => Kek::from_password(b"other-password-2", &salt, params)?,
        };
        let enc = encrypt(&payload, &[k], &salt, &opts, &RandomMaterial::generate())?;
        std::fs::write(dir.join(format!("f{i:05}.omy")), &enc.bytes)?;
    }
    // 混入一些非 omy 文件，验证扫描器能快速跳过
    for i in 0..(n / 4) {
        std::fs::write(dir.join(format!("noise{i:05}.bin")), b"not an omy file at all")?;
    }
    println!("    生成耗时 {:.1} s（含每文件一次 Argon2，故较慢）",
             t2.elapsed().as_secs_f64());
    println!("    其中 {mine} 个用当前密码加密，{} 个用其它密码", n - mine);
    let _ = (&other1, &other2);

    println!("\n=== 4. 扫描（KEK 已缓存，只做 HKDF + AEAD）===");
    let mut session = SessionKeys::new();
    let t3 = Instant::now();
    session.unlock_password("main", &salt, "the-real-password", params)?;
    let unlock_ms = t3.elapsed().as_secs_f64() * 1000.0;
    println!("    解锁（一次 Argon2）: {unlock_ms:.1} ms");

    let t4 = Instant::now();
    let result = scan_dir(&dir, &session, &ScanOptions::omy_only())?;
    let scan_s = t4.elapsed().as_secs_f64();

    println!("    扫描 {} 个文件耗时 {:.3} s", result.stats.files_examined, scan_s);
    println!("    识别为 omy: {}", result.stats.omy_found);
    println!("    成功解锁:   {}", result.unlocked().len());
    println!(
        "    平均每文件: {:.3} ms",
        scan_s * 1000.0 / result.stats.files_examined.max(1) as f64
    );

    let per_file_ms = scan_s * 1000.0 / result.stats.omy_found.max(1) as f64;
    println!("\n=== 5. 外推到 10,000 个文件 ===");
    println!("    两级 KDF:        {:.1} s", per_file_ms * 10_000.0 / 1000.0);
    println!("    若每文件跑 Argon2: {:.0} s ({:.1} 小时)",
             argon_ms * 10_000.0 / 1000.0,
             argon_ms * 10_000.0 / 1000.0 / 3600.0);

    // 正确性同样要验证：解锁数必须恰好等于用当前密码加密的文件数
    let unlocked = result.unlocked().len();
    println!("\n=== 6. 正确性 ===");
    if unlocked == mine {
        println!("    PASS 解锁数 {unlocked} == 用当前密码加密的文件数 {mine}");
    } else {
        println!("    FAIL 解锁数 {unlocked} != 期望 {mine}");
        std::fs::remove_dir_all(&dir)?;
        std::process::exit(1);
    }
    if result.stats.omy_found == n {
        println!("    PASS 识别出全部 {n} 个 omy 文件，噪音文件未被误判");
    } else {
        println!("    FAIL 识别 {} 个，期望 {n}", result.stats.omy_found);
        std::fs::remove_dir_all(&dir)?;
        std::process::exit(1);
    }

    std::fs::remove_dir_all(&dir)?;
    Ok(())
}
