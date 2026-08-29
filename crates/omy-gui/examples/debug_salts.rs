//! 排查：同一目录下各文件的 vault_salt 是否相同。
//!
//! GUI 的解锁流程假设「一个目录 = 一个库 = 同一个盐」，
//! 从任意一个文件读盐即可解开全部。若这个假设不成立，
//! 就必须改成逐文件派生——那会让解锁慢上几十倍。

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| String::from("spikes/fixtures/vault"));
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("读目录失败: {e}");
            std::process::exit(1);
        }
    };

    let mut salts: Vec<(String, String, u32, u32, u32)> = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        let Ok(h) = omy_core::file::peek_header(&bytes) else {
            continue;
        };
        let hex: String = h.vault_salt.iter().map(|b| format!("{b:02x}")).collect();
        salts.push((
            p.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            hex,
            h.argon2_m_kib,
            h.argon2_t,
            h.argon2_p,
        ));
    }

    println!("目录: {dir}");
    println!("{:<28} {:<34} {:>8} {:>3} {:>3}", "文件", "vault_salt", "m_kib", "t", "p");
    for (name, salt, m, t, p) in &salts {
        println!("{name:<28} {salt:<34} {m:>8} {t:>3} {p:>3}");
    }

    let uniq: std::collections::HashSet<&String> = salts.iter().map(|(_, s, ..)| s).collect();
    println!();
    println!("不同的盐: {} 个 / 共 {} 个文件", uniq.len(), salts.len());
    if uniq.len() == 1 {
        println!("→ 同目录共用一个盐，GUI 的「读一个文件的盐解开全部」成立");
    } else {
        println!("→ 每个文件盐都不同！GUI 必须逐文件派生 KEK");
        println!("  这意味着解锁 N 个文件要跑 N 次 Argon2");
    }
}
