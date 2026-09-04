//! 基准：扫描时顺带解 header 补媒体字段，代价是多少。
//!
//! 跑法：`cargo run --release -p omy-core --example bench_scan_header [文件数]`
//!
//! # 为什么需要这个基准
//!
//! GUI 扫描时会为每个已解锁文件解一次 header，用来填缩略图标记、
//! kind、时长等字段。这条路径的性能**无法用断言防住**——读多少字节
//! 都能得到正确结果，只是快慢不同。实测过一次变异（把两段读改回固定
//! 1 MiB 前缀）：端到端 14 条断言全部通过，而 10000 个文件的耗时从
//! 0.9 s 涨到 5.5 s，冲破文档 §9 的「10000 文件 < 5s」。
//!
//! 所以改动 GUI 的 `read_header_area` 或扫描路径后，跑一次这个基准。
//!
//! # 实测结论（本机，10000 个文件）
//!
//! | 读法 | 小文件 | 4 MiB 文件 |
//! |---|---|---|
//! | 固定 1 MiB 前缀 | 0.9 s | **5.5 s 超预算** |
//! | 按 `header_len` 两段读 | 0.9 s | 0.9 s |
//!
//! 小文件量不出差别——1 MiB 的读会被文件实际长度截断。只有用几 MB 的
//! 真实媒体文件才暴露，这也是为什么基准里必须有大文件那一组。

use std::time::Instant;

/// 每个文件读 header 的两种方式，用来对比。
#[derive(Clone, Copy)]
enum ReadMode {
    /// 固定前缀（旧写法）。
    Fixed(usize),
    /// 先取 `header_len` 再按需读（现写法）。
    TwoStage,
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);

    // 缩略图载荷用算式生成 6 KB（实测真实缩略图就是这个量级）。
    // omy-core 不依赖 image（那是 omy-media 的事，且它是 LGPL），
    // 这里只需要「一段合适大小的字节」——量的是解 TLV 的耗时，
    // 与内容是不是真图片无关。
    //
    // 不写 vec![0x5A; 6306] 这类字面量：AGENTS.md 记过它会在 .rodata
    // 里留下长串同值字节，本机安全软件据此把测试二进制删掉。
    let thumb: Vec<u8> = (0..6306).map(|i: usize| u8::try_from(i.wrapping_mul(37) % 251).unwrap_or(0)).collect();

    // 直接用固定密钥材料造 KEK，跳过 Argon2。
    //
    // 这正是要量的东西：KEK 在解锁时**每密码只派生一次**，扫描 10000 个
    // 文件不会重复付那个成本。把 Argon2 算进来会把结论完全带偏。
    let key = omy_core::crypto::SecretKey::from_bytes(core::array::from_fn(|i| {
        u8::try_from(i).unwrap_or(0).wrapping_mul(7) ^ 0x3D
    }));
    // Kek 有意不实现 Clone（密钥卫生），所以造一个切片反复借用
    let keks = [omy_core::crypto::Kek::from_key(key)];
    let salt = [0x11u8; 16];

    // 两组语料：小文件量不出前缀大小的差别，大文件才是真实图库的样子
    let small = build_corpus("omy-bench-small", n, 16, &thumb, &keks, &salt);
    let big = build_corpus("omy-bench-big", 200, 4 << 20, &thumb, &keks, &salt);

    for (label, dir, cnt) in [("小文件", &small, n), ("4 MiB 文件", &big, 200usize)] {
        println!("\n=== {label} x{cnt} ===");
        for mode in [
            ReadMode::Fixed(32 * 1024),
            ReadMode::Fixed(1 << 20),
            ReadMode::TwoStage,
        ] {
            let (hit, elapsed, maxlen) = measure(dir, cnt, mode, &keks);
            let per_ms = elapsed.as_secs_f64() / cnt as f64 * 1000.0;
            // 10000 个文件的总秒数：per_ms 毫秒 x 10000 个 / 1000
            let total_10k_s = per_ms * 10.0;
            let name = match mode {
                ReadMode::Fixed(c) => format!("固定 {} KiB", c.saturating_div(1024)),
                ReadMode::TwoStage => String::from("两段读"),
            };
            let verdict = if total_10k_s < 5.0 { "预算内" } else { "**超预算**" };
            println!(
                "  {name:<14} {hit}/{cnt} 命中  {per_ms:.3} ms/文件  \
                 推算 10000 个 {total_10k_s:.2} s  {verdict}"
            );
            if matches!(mode, ReadMode::TwoStage) {
                println!("                 （实际 header_len 最大 {maxlen} 字节）");
            }
        }
    }

    let _ = std::fs::remove_dir_all(&small);
    let _ = std::fs::remove_dir_all(&big);
}

/// 造一组加密文件，返回所在目录。
fn build_corpus(
    name: &str,
    count: usize,
    payload_len: usize,
    thumb: &[u8],
    keks: &[omy_core::crypto::Kek],
    salt: &[u8; 16],
) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return dir;
    }
    let payload: Vec<u8> = (0..payload_len)
        .map(|i: usize| u8::try_from(i.wrapping_mul(31) % 251).unwrap_or(0))
        .collect();
    println!("正在造 {count} 个文件（载荷 {payload_len} 字节）……");
    let t = Instant::now();
    for i in 0..count {
        let opts = omy_core::file::EncryptOptions {
            filename: Some(format!("f{i}.png")),
            thumbnail: Some(thumb.to_vec()),
            media_meta: Some(br#"{"container":"png_pipe"}"#.to_vec()),
            ..omy_core::file::EncryptOptions::default()
        };
        let rnd = omy_core::file::RandomMaterial::generate();
        if let Ok(enc) = omy_core::file::encrypt(&payload, keks, salt, &opts, &rnd) {
            let _ = std::fs::write(dir.join(format!("f{i:05}.omy")), &enc.bytes);
        }
        // 预热：顺手读一遍，避免后面第一轮量到的是冷启动磁盘 IO
        let _ = std::fs::read(dir.join(format!("f{i:05}.omy")));
    }
    println!("  建库耗时 {:?}", t.elapsed());
    dir
}

/// 按指定读法遍历一组文件，返回（命中数, 耗时, 见到的最大 `header_len`）。
fn measure(
    dir: &std::path::Path,
    count: usize,
    mode: ReadMode,
    keks: &[omy_core::crypto::Kek],
) -> (usize, std::time::Duration, usize) {
    let t = Instant::now();
    let mut hit = 0usize;
    let mut maxlen = 0usize;
    for i in 0..count {
        let p = dir.join(format!("f{i:05}.omy"));
        let bytes = match mode {
            ReadMode::Fixed(cap) => read_prefix(&p, cap).unwrap_or_default(),
            ReadMode::TwoStage => {
                let (b, hl) = read_header_area(&p);
                maxlen = maxlen.max(hl);
                b
            }
        };
        if omy_core::file::open(&bytes, keks).is_ok_and(|o| o.thumbnail().is_ok()) {
            hit = hit.saturating_add(1);
        }
    }
    (hit, t.elapsed(), maxlen)
}

/// 先读固定头取出 `header_len`，再按需读那么多。
///
/// 与 GUI 里 `read_header_area` 同一套逻辑；这里额外把 `header_len`
/// 返回出来，好确认实际头部到底多大（用来判断固定前缀是不是在白读）。
fn read_header_area(p: &std::path::Path) -> (Vec<u8>, usize) {
    let off = omy_core::header::HEADER_LEN_FIELD_OFFSET;
    let head = read_prefix(p, off.saturating_add(4)).unwrap_or_default();
    let Some(field) = head
        .get(off..off.saturating_add(4))
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
    else {
        return (head, 0);
    };
    let claimed = u32::from_le_bytes(field) as usize;
    // header_len 来自尚未验证的内容，用文件实际长度兜住，
    // 否则一个声称 4 GiB 的坏文件会让进程被 OOM 杀掉
    let actual = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let cap = claimed.min(usize::try_from(actual).unwrap_or(usize::MAX));
    (read_prefix(p, cap).unwrap_or_default(), claimed)
}

/// 只读文件前 n 字节。
fn read_prefix(p: &std::path::Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let f = std::fs::File::open(p)?;
    let mut buf = Vec::new();
    // take + read_to_end：`Read::read` 不保证填满缓冲区，单次 read 会短读
    f.take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}
