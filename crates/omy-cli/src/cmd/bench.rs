//! `omy bench`：KDF 与加解密性能基准。
//!
//! 这个命令有实际用途，不只是好看：Argon2 耗时**强烈依赖设备**
//! （内存带宽与 CPU），文档里的数字只能参考。用户需要在自己机器上
//! 实测，才能选定合适的 KDF 档位——移动端跑 sensitive 档可能直接失败。

use super::Ctx;
use crate::output::human_bytes;
use anyhow::Result;
use clap::Args as ClapArgs;
use omy_core::crypto::{Argon2Params, CipherId, Kek};
use serde_json::json;
use std::time::Instant;

/// `bench` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 只测指定档位，可重复；默认测全部
    #[arg(long, value_enum, value_name = "PROFILE")]
    pub profile: Vec<super::KdfProfile>,

    /// 跳过高内存档位（moderate / sensitive）
    #[arg(long)]
    pub quick: bool,

    /// 吞吐测试的数据量，如 64M
    #[arg(long, value_name = "SIZE", default_value = "64M")]
    pub throughput_size: String,
}

/// 执行 `bench`。
///
/// # Errors
///
/// 密钥派生失败或内存不足时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    // debug 构建下的数字毫无参考价值：实测 AEAD 吞吐差约 130 倍
    // （0.01 GB/s vs 1.35 GB/s），HKDF 差约 23 倍。
    // 不提示的话用户会拿 debug 数字去选 KDF 档位，得出完全错误的结论。
    if cfg!(debug_assertions) {
        ctx.out.warn(
            "当前是 debug 构建，性能数字不具参考价值（实测吞吐可差 100 倍以上）。\n\
             请用 cargo build --release 后再测：target/release/omy bench",
        );
    }

    let salt = [0x5Au8; 16];
    let profiles: Vec<super::KdfProfile> = if a.profile.is_empty() {
        if a.quick {
            vec![super::KdfProfile::Mobile, super::KdfProfile::Interactive]
        } else {
            vec![
                super::KdfProfile::Mobile,
                super::KdfProfile::Interactive,
                super::KdfProfile::Moderate,
                super::KdfProfile::Sensitive,
            ]
        }
    } else {
        a.profile.clone()
    };

    let mut kdf_rows = Vec::new();
    ctx.out.info("Argon2id 基准（本机实测）：");

    let mut interactive_ms = 0f64;
    for p in &profiles {
        let params = p.params();
        let t0 = Instant::now();
        let kek = Kek::from_password(b"benchmark-password", &salt, params)?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        // 防止被优化掉
        std::hint::black_box(&kek);

        if *p == super::KdfProfile::Interactive {
            interactive_ms = ms;
        }

        ctx.out.info(&format!(
            "  {:<12} ({:>4} MiB, t={})   {:>8.1} ms{}",
            p.name(),
            params.m_kib / 1024,
            params.t,
            ms,
            if *p == super::KdfProfile::Interactive {
                "   ← 默认"
            } else {
                ""
            }
        ));
        kdf_rows.push(json!({
            "profile": p.name(),
            "m_kib": params.m_kib,
            "t": params.t,
            "p": params.p,
            "ms": ms,
        }));
    }

    // HKDF：每文件每 slot 都要做，这个数字决定扫描是否可行
    let kek = Kek::from_password(b"benchmark-password", &salt, Argon2Params::TEST_WEAK)?;
    let uuid = [0x11u8; 16];
    let rounds = 200_000u32;
    let t1 = Instant::now();
    let mut sink = 0u8;
    for i in 0..rounds {
        let k = kek.derive_slot_key(&uuid, (i % 8) as u16);
        sink ^= k.as_bytes()[0];
    }
    std::hint::black_box(sink);
    let hkdf_us = t1.elapsed().as_secs_f64() * 1_000_000.0 / f64::from(rounds);

    ctx.out.info(&format!("\nHKDF（每文件每 slot）        {hkdf_us:>8.3} µs"));
    if interactive_ms > 0.0 {
        let ratio = interactive_ms * 1000.0 / hkdf_us;
        ctx.out.info(&format!("比值                        {ratio:>8.0}×"));
        // 只报告「密钥运算部分」的耗时，不冒充端到端扫描耗时。
        // 实测扫描 300 个文件约 95 ms，说明目录遍历与文件头读取（I/O）
        // 才是主导项；把 HKDF 外推当成扫描耗时会低估一个数量级以上。
        ctx.out.info(&format!(
            "\n扫描 10,000 个文件的密钥运算 {:.2} s（不含文件 I/O，实际耗时以 I/O 为主）",
            hkdf_us * 10_000.0 / 1_000_000.0
        ));
        ctx.out.info(&format!(
            "若每文件独立跑 Argon2         {:.0} s ← 这是必须缓存 KEK 的原因",
            interactive_ms * 10_000.0 / 1000.0
        ));
    }

    // AEAD 吞吐
    let size = crate::output::parse_size(&a.throughput_size)?;
    let n = usize::try_from(size).unwrap_or(64 * 1024 * 1024);
    let data = vec![0xA5u8; n];
    let mut aead_rows = Vec::new();

    ctx.out.info("\n加解密吞吐：");
    for (name, cipher) in [
        ("ChaCha20-Poly1305", CipherId::ChaCha20Poly1305),
        ("AES-256-GCM", CipherId::Aes256Gcm),
    ] {
        let key = omy_core::crypto::SecretKey::from_bytes([0x33u8; 32]);
        // base_nonce 是 7 字节：块序号与 final 标志占掉剩余部分，
        // 由 chunk_nonce 组装成完整 nonce（见 payload 模块）
        let base_nonce = [0u8; 7];
        let t = Instant::now();
        let ct = omy_core::payload::encrypt_chunk(
            &key, cipher, &base_nonce, &uuid, 0, true, &data,
        )?;
        let enc_s = t.elapsed().as_secs_f64();

        let t = Instant::now();
        let pt = omy_core::payload::decrypt_chunk(
            &key, cipher, &base_nonce, &uuid, 0, true, &ct,
        )?;
        let dec_s = t.elapsed().as_secs_f64();
        anyhow::ensure!(pt.len() == data.len(), "解密长度不符，基准无效");

        let enc_gbs = n as f64 / enc_s / 1e9;
        let dec_gbs = n as f64 / dec_s / 1e9;
        ctx.out.info(&format!(
            "  {name:<20} 加密 {enc_gbs:>5.2} GB/s   解密 {dec_gbs:>5.2} GB/s"
        ));
        aead_rows.push(json!({
            "cipher": name,
            "encrypt_gbps": enc_gbs,
            "decrypt_gbps": dec_gbs,
        }));
    }

    ctx.out.info(&format!("\n（吞吐测试数据量 {}）", human_bytes(size)));

    ctx.out.result(
        "",
        &json!({
            "kdf": kdf_rows,
            "hkdf_us": hkdf_us,
            "aead": aead_rows,
            "throughput_bytes": size,
        }),
    );
    Ok(())
}
