//! 模糊测试：随机变异输入，验证解析器绝不 panic。
//!
//! 这是独立于 `hostile.rs` 的验证通道。`hostile.rs` 测的是**预想到的**攻击模式，
//! 本程序做的是**无偏随机变异**——覆盖那些没预想到的组合。
//!
//! 判据：无论输入多离谱，都必须返回 `Err`，绝不 panic。
//! 对 Tauri 应用而言，库里一次 panic 就可能拖垮整个进程。
//!
//! 用法：`cargo run --release --example fuzz_parse -- [迭代次数] [seed]`

// 这是变异测试工具，算术与索引用于构造畸形输入而非处理不可信数据，
// 因此局部放宽相关 lint，不影响库代码的严格检查。
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]
#![allow(clippy::cast_possible_truncation, clippy::indexing_slicing)]

use omy_core::crypto::{Argon2Params, Kek};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt, is_omy_file, open, peek_header};
use omy_core::header::MIN_CHUNK_SIZE;
use omy_core::shard::{ShardHeader, analyze_coverage, merge, recover_header_from_shard};
use omy_core::tlv::{TlvSet, decode_compression_index, unpad_filename};

/// 简单的确定性 PRNG，便于复现失败用例。
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xFF) as u8
    }
}

fn main() {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(20_000);
    let seed: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x1234_5678_9ABC_DEF0);

    let salt = [0x11u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(b"fuzz", &salt, params).expect("KEK 派生失败");

    // 造一个合法文件作为变异基底
    let opts = EncryptOptions {
        filename: Some("fuzz-target.bin".into()),
        chunk_size: MIN_CHUNK_SIZE,
        argon2: params,
        ..EncryptOptions::default()
    };
    let base = encrypt(
        &vec![0x42u8; 3000],
        &[kek],
        &salt,
        &opts,
        &RandomMaterial::generate(),
    )
    .expect("加密失败")
    .bytes;

    println!("基底文件 {} 字节，迭代 {iters} 次，seed = 0x{seed:016X}", base.len());

    let mut rng = Rng(seed);
    let mut stats = [0usize; 6]; // [解析Err, 解析Ok, 打开Err, 打开Ok, TLV, 分片]

    for i in 0..iters {
        let mut data = base.clone();

        // 随机挑一种变异策略
        match rng.below(8) {
            0 => {
                // 单字节翻转
                let p = rng.below(data.len());
                if let Some(b) = data.get_mut(p) {
                    *b ^= 1u8 << rng.below(8);
                }
            }
            1 => {
                // 随机截断
                let n = rng.below(data.len().max(1));
                data.truncate(n);
            }
            2 => {
                // 整段随机覆写
                let start = rng.below(data.len());
                let len = rng.below(64).min(data.len().saturating_sub(start));
                for k in 0..len {
                    if let Some(b) = data.get_mut(start + k) {
                        *b = rng.byte();
                    }
                }
            }
            3 => {
                // 把某个 u32 字段改成极值，专打长度类字段
                let p = rng.below(data.len().saturating_sub(4).max(1));
                let v: [u8; 4] = match rng.below(4) {
                    0 => u32::MAX.to_le_bytes(),
                    1 => 0u32.to_le_bytes(),
                    2 => (u32::MAX / 2).to_le_bytes(),
                    _ => 0xDEAD_BEEFu32.to_le_bytes(),
                };
                for (k, vb) in v.iter().enumerate() {
                    if let Some(b) = data.get_mut(p + k) {
                        *b = *vb;
                    }
                }
            }
            4 => {
                // 尾部追加垃圾
                let n = rng.below(200);
                for _ in 0..n {
                    data.push(rng.byte());
                }
            }
            5 => {
                // 纯随机数据，长度随机
                let n = rng.below(1200);
                data = (0..n).map(|_| rng.byte()).collect();
            }
            6 => {
                // 保留正确 magic 但其余全随机——专门冲击「已识别为 omy」之后的路径
                let n = rng.below(1200).max(8);
                data = (0..n).map(|_| rng.byte()).collect();
                if data.len() >= 8 {
                    data[..8].copy_from_slice(&omy_core::MAGIC_FILE);
                }
            }
            _ => {
                // 空或极短输入
                data.truncate(rng.below(16));
            }
        }

        // 以下每个调用都必须么返回 Ok 么返回 Err，绝不 panic
        let _ = is_omy_file(&data);

        match peek_header(&data) {
            Ok(_) => stats[1] += 1,
            Err(_) => stats[0] += 1,
        }

        let k2 = Kek::from_password(b"fuzz", &salt, params).expect("KEK 派生失败");
        match open(&data, &[k2]) {
            Ok(opened) => {
                stats[3] += 1;
                // 打开成功也要继续压测后续路径
                let _ = opened.filename();
                let _ = opened.thumbnail();
                let _ = opened.compression_index();
                let _ = opened.stored_content_hash();
                let _ = opened.decrypt_all(&data);
            }
            Err(_) => stats[2] += 1,
        }

        // 直接对随机数据跑各个解析器
        if TlvSet::parse(&data).is_ok() {
            stats[4] += 1;
        }
        let _ = decode_compression_index(&data);
        let _ = unpad_filename(&data);

        if ShardHeader::parse(&data).is_ok() {
            stats[5] += 1;
        }
        let _ = recover_header_from_shard(&data);
        let _ = merge(&[data.clone()]);
        let _ = analyze_coverage(&[data.clone()]);

        if i % 5000 == 0 && i > 0 {
            println!("  ...{i} 次");
        }
    }

    println!("\n完成 {iters} 次变异，无 panic");
    println!("  peek_header: {} Err / {} Ok", stats[0], stats[1]);
    println!("  open:        {} Err / {} Ok", stats[2], stats[3]);
    println!("  TLV 可解析:  {}", stats[4]);
    println!("  分片头可解析: {}", stats[5]);
}
