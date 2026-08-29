//! 核查模糊测试中「open 成功率过半」是否合理。
//!
//! 疑点：随机变异后仍有 54% 的文件能被 open 成功。
//! 若变异落在被 MAC 覆盖的头部区域，open **必须**失败；
//! 只有落在载荷区时 open 才应成功（此时错误应在 decrypt 阶段暴露）。
//!
//! 本程序按变异位置分类统计，验证这个边界是否被严格执行。
//!
//! 用法：`cargo run --release --example audit_mac_scope`

// 这是审计工具，算术用于统计计数而非处理不可信输入，
// 因此局部放宽相关 lint，不影响库代码的严格检查。
#![allow(clippy::arithmetic_side_effects, clippy::integer_division)]

use omy_core::crypto::{Argon2Params, Kek};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt, open};
use omy_core::header::MIN_CHUNK_SIZE;

fn main() {
    let salt = [0x11u8; 16];
    let params = Argon2Params::TEST_WEAK;
    let kek = Kek::from_password(b"fuzz", &salt, params).expect("KEK 派生失败");

    let opts = EncryptOptions {
        filename: Some("fuzz-target.bin".into()),
        chunk_size: MIN_CHUNK_SIZE,
        argon2: params,
        ..EncryptOptions::default()
    };
    let enc = encrypt(
        &vec![0x42u8; 3000],
        &[kek],
        &salt,
        &opts,
        &RandomMaterial::generate(),
    )
    .expect("加密失败");
    let base = enc.bytes;
    let hlen = enc.header.header_len as usize;

    println!("文件 {} 字节，header_len = {hlen}，载荷 {} 字节",
             base.len(), base.len() - hlen);
    println!("对每个字节位置逐一翻转最低位，统计 open 与 decrypt 的结果\n");

    let mut header_open_ok = 0usize;
    let mut header_open_err = 0usize;
    let mut payload_open_ok = 0usize;
    let mut payload_open_err = 0usize;
    let mut payload_decrypt_ok = 0usize;
    let mut payload_decrypt_err = 0usize;
    let mut header_leak = Vec::new();

    for pos in 0..base.len() {
        let mut data = base.clone();
        if let Some(b) = data.get_mut(pos) {
            *b ^= 0x01;
        }

        let k = Kek::from_password(b"fuzz", &salt, params).expect("KEK 派生失败");
        let in_header = pos < hlen;

        match open(&data, &[k]) {
            Ok(opened) => {
                if in_header {
                    header_open_ok += 1;
                    // 头部被改却能打开：记下来仔细看
                    if header_leak.len() < 20 {
                        header_leak.push(pos);
                    }
                } else {
                    payload_open_ok += 1;
                    // 载荷被改，open 成功是预期的；但解密必须失败
                    match opened.decrypt_all(&data) {
                        Ok(_) => payload_decrypt_ok += 1,
                        Err(_) => payload_decrypt_err += 1,
                    }
                }
            }
            Err(_) => {
                if in_header {
                    header_open_err += 1;
                } else {
                    payload_open_err += 1;
                }
            }
        }
    }

    println!("=== 头部区域（0..{hlen}）===");
    println!("  open 失败: {header_open_err}  <- 期望：全部");
    println!("  open 成功: {header_open_ok}   <- 期望：0");
    if !header_leak.is_empty() {
        println!("  可疑位置: {header_leak:?}");
    }

    println!("\n=== 载荷区域（{hlen}..{}）===", base.len());
    println!("  open 成功: {payload_open_ok}  <- 期望：全部（头部完好）");
    println!("  open 失败: {payload_open_err}");
    println!("    其中 decrypt 失败: {payload_decrypt_err}  <- 期望：全部");
    println!("    其中 decrypt 成功: {payload_decrypt_ok}   <- 期望：0");

    let mut bad = false;
    if header_open_ok != 0 {
        println!("\nFAIL 头部被篡改却能 open，MAC 覆盖范围有漏洞");
        bad = true;
    }
    if payload_decrypt_ok != 0 {
        println!("\nFAIL 载荷被篡改却能解密成功，AEAD 未生效");
        bad = true;
    }
    if !bad {
        println!("\nPASS 头部任一字节翻转必被 MAC 检出；载荷任一字节翻转必被 AEAD 检出");
        println!("     模糊测试中 54% 的 open 成功率来自载荷区变异，属正常现象");
    }
    if bad {
        std::process::exit(1);
    }
}
