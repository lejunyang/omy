//! 验证：共享服务端在**完全没有密钥**的情况下能做多少事。
//!
//! 这直接决定「共享方锁屏后能否继续共享」。
//!
//! 设计文档 §1 说服务端是「纯密文块服务器，永远不接触密码、KEK、
//! FEK 或明文」，但 §5.3 又说「共享方 vault 锁定 → 立即停止服务
//! （因为连列出文件都需要解密 header）」。两者不可能同时成立。
//!
//! 本程序用真实加密文件验证到底哪个对：全程不构造任何 Kek，
//! 看看还能不能完成 LIST / STAT / READ 三种服务端操作。

use std::path::Path;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        String::from("spikes/fixtures/vault")
    });
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut fails: Vec<String> = Vec::new();

    macro_rules! check {
        ($cond:expr, $label:expr) => { check!($cond, $label, "") };
        ($cond:expr, $label:expr, $detail:expr) => {{
            let c = $cond;
            let d: String = $detail.into();
            if c {
                pass += 1;
                println!("  PASS  {}{}", $label, if d.is_empty() { String::new() } else { format!("  {d}") });
            } else {
                fail += 1;
                fails.push(String::from($label));
                println!("  FAIL  {}{}", $label, if d.is_empty() { String::new() } else { format!("  {d}") });
            }
            c
        }};
    }

    println!("=== 服务端在零密钥状态下的能力 ===");
    println!("目录: {dir}\n");

    let entries: Vec<_> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect(),
        Err(e) => {
            println!("无法读取目录: {e}");
            std::process::exit(1);
        }
    };

    if entries.is_empty() {
        println!("目录为空，先跑 spikes/make-gui-vault.ps1");
        std::process::exit(1);
    }

    // --- LIST：识别哪些是 omy 文件 ---
    let mut omy_files = Vec::new();
    for p in &entries {
        let Ok(head) = read_head(p, 4096) else { continue };
        if omy_core::file::is_omy_file(&head) {
            omy_files.push((p.clone(), head));
        }
    }
    check!(
        !omy_files.is_empty(),
        "无密钥即可识别 omy 文件（LIST 的前提）",
        format!("{} / {} 个", omy_files.len(), entries.len())
    );

    // --- STAT：读出服务端需要的元信息 ---
    let mut stat_ok = 0usize;
    let mut uuids = Vec::new();
    for (p, head) in &omy_files {
        match omy_core::file::peek_header(head) {
            Ok(h) => {
                stat_ok += 1;
                uuids.push(hex::encode(&h.file_uuid[..4]));
                let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                println!(
                    "        {:<24} header_len={:<6} 密文={:<10} uuid={}",
                    p.file_name().and_then(|n| n.to_str()).unwrap_or("?"),
                    h.header_len,
                    size,
                    hex::encode(&h.file_uuid[..4])
                );
            }
            Err(e) => println!("        解析失败 {}: {e}", p.display()),
        }
    }
    check!(
        stat_ok == omy_files.len(),
        "无密钥即可读出 header_len 与文件大小（STAT）",
        format!("{stat_ok} / {}", omy_files.len())
    );

    // 服务端要能区分不同文件，靠的是 file_uuid 而非文件名
    let uniq: std::collections::BTreeSet<_> = uuids.iter().collect();
    check!(
        uniq.len() == uuids.len(),
        "每个文件的 uuid 唯一（可作为不透明 handle 的基础）",
        format!("{} 个互不相同", uniq.len())
    );

    // --- READ：返回任意密文区间 ---
    let Some((first, _)) = omy_files.first() else {
        println!("\n没有可用文件");
        std::process::exit(1);
    };
    let total = std::fs::metadata(first).map(|m| m.len()).unwrap_or(0);
    let mid = total / 2;
    let slice = read_at(first, mid, 1024);
    let got_len = slice.as_ref().map(|s| s.len()).unwrap_or(0);
    let want_len = 1024usize.min(total.saturating_sub(mid) as usize);
    check!(
        got_len == want_len,
        "无密钥即可返回任意密文区间（READ）",
        format!("offset={mid} len={got_len}")
    );

    // --- 关键反证：服务端**确实**读不到明文 ---
    // 若这条没通过，说明我们不小心把明文暴露给了服务端
    let name_readable = omy_files.iter().any(|(_, head)| {
        // 尝试不带任何 KEK 打开
        omy_core::file::open(head, &[]).is_ok()
    });
    check!(
        !name_readable,
        "服务端**读不到**文件名与内容（零密钥是真的零）",
        "open(&[]) 全部失败，符合预期"
    );

    println!("\n{}", "=".repeat(62));
    println!("结果: {pass} 通过, {fail} 失败");
    if !fails.is_empty() {
        println!("失败项:");
        for f in &fails {
            println!("  - {f}");
        }
    }
    println!("{}", "=".repeat(62));
    if fail == 0 {
        println!("\n结论：LIST / STAT / READ 三种服务端操作全部不需要密钥。");
        println!("      共享方锁屏后可以继续共享——文档 §5.3 的「锁定即停服」");
        println!("      建立在错误前提上（它以为列文件要解密 header）。");
    }
    if fail > 0 {
        std::process::exit(1);
    }
}

fn read_head(p: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

fn read_at(p: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut f = std::fs::File::open(p)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}
