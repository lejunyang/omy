//! P2 转封装的真实文件验证：真实 MKV + 真实 FFmpeg。
//!
//! # 为什么单元测试不够
//!
//! `mkv.rs` 与 `remux.rs` 的单测用的是**手写的 EBML 字节**和**参数字符串比对**。
//! 前者只能验证"给定这段字节能否正确解析"，无法验证"真实 muxer 产出的
//! MKV 是否真的长这样"；后者压根没跑过 FFmpeg。
//!
//! 缺陷 #6 和 #8 都是这么漏过去的：测试与实现同源，对格式的误解同时
//! 存在于样本和代码里，互相印证出一个错误的结论。
//!
//! 所以这里用真实素材走完整链路，且每个断言的期望值按 spike 实测结果
//! 与文档判定，而不是"实现算出什么就认什么"。
//!
//! 前置：
//! ```text
//! pwsh -File spikes/make-media-fixtures.ps1
//! pwsh -File spikes/make-seek-fixtures.ps1
//! ```

use omy_media::{ffprobe, mkv, mp4, probe, remux};
use std::path::PathBuf;

struct Checker {
    pass: usize,
    fail: Vec<String>,
}

impl Checker {
    fn new() -> Self {
        Self {
            pass: 0,
            fail: Vec::new(),
        }
    }
    fn check(&mut self, cond: bool, label: &str) {
        if cond {
            self.pass += 1;
            println!("  PASS  {label}");
        } else {
            self.fail.push(label.to_owned());
            println!("  FAIL  {label}");
        }
    }
    fn check_eq<T: std::fmt::Debug + PartialEq>(&mut self, got: T, want: T, label: &str) {
        if got == want {
            self.pass += 1;
            println!("  PASS  {label}");
        } else {
            self.fail
                .push(format!("{label}（期望 {want:?}，实际 {got:?}）"));
            println!("  FAIL  {label}  期望 {want:?}，实际 {got:?}");
        }
    }
}

fn fixture_dir() -> PathBuf {
    let mut d = std::env::current_exe().unwrap_or_default();
    for _ in 0..3 {
        d.pop();
    }
    let p = d.join("spikes").join("fixtures").join("media");
    if p.is_dir() {
        return p;
    }
    PathBuf::from("spikes").join("fixtures").join("media")
}

/// 用 ffprobe 确认一段 fMP4 真的可解析，返回 (时长秒, 流数)。
fn probe_bytes(data: &[u8]) -> Option<(f64, usize)> {
    let info = probe::probe(probe::Source::Bytes(data)).ok()?;
    let n = info.video.len() + info.audio.len() + info.subtitles.len();
    #[allow(clippy::cast_precision_loss)]
    let secs = info.duration_ms.unwrap_or(0) as f64 / 1000.0;
    Some((secs, n))
}

fn main() {
    println!("=== P2 转封装验证（真实 MKV + 真实 FFmpeg）===\n");

    if !ffprobe::has_ffmpeg() || !ffprobe::has_ffprobe() {
        println!("环境缺少 ffmpeg/ffprobe，无法验证 P2 路径。");
        std::process::exit(2);
    }

    let dir = fixture_dir();
    let seek_mkv = dir.join("seek_h264_aac.mkv");
    if !seek_mkv.is_file() {
        println!("找不到 {}", seek_mkv.display());
        println!("请先运行 spikes/make-seek-fixtures.ps1");
        std::process::exit(2);
    }

    let mut c = Checker::new();
    let data = std::fs::read(&seek_mkv).expect("读取素材失败");
    println!("素材: seek_h264_aac.mkv  {} 字节\n", data.len());

    // ---------- 1. EBML 解析 ----------
    println!("--- 1. EBML 结构解析 ---");
    let idx = match mkv::parse(&data) {
        Ok(i) => i,
        Err(e) => {
            println!("  FAIL  解析真实 MKV：{e}");
            println!("\n结果: 0 通过, 1 失败");
            std::process::exit(1);
        }
    };
    c.check(true, "能解析真实 MKV");
    // 素材是 60 秒、每秒一个 cluster
    c.check_eq(idx.clusters.len(), 60, "解析出 60 个 Cluster");
    c.check(idx.has_tracks, "识别出 Tracks 元素");
    c.check_eq(idx.timestamp_scale, 1_000_000, "TimestampScale 为 1ms");
    c.check(
        idx.duration_ms.is_some_and(|d| (59_000..=61_000).contains(&d)),
        &format!("时长约 60 秒（实际 {:?} ms）", idx.duration_ms),
    );

    // 头部必须止于第一个 Cluster
    c.check_eq(
        idx.header_len,
        idx.clusters[0].offset,
        "头部长度等于首个 Cluster 偏移",
    );
    // spike 实测该素材头部是 754 字节
    c.check(
        idx.header_len > 100 && idx.header_len < 10_000,
        &format!("头部大小合理（{} 字节）", idx.header_len),
    );

    // 时间戳必须单调递增——这是 cluster_for_time 二分查找的前提
    let mut monotonic = true;
    let mut prev = 0u64;
    for cl in &idx.clusters {
        if let Some(ts) = cl.timestamp_ms {
            if ts < prev {
                monotonic = false;
                break;
            }
            prev = ts;
        }
    }
    c.check(monotonic, "Cluster 时间戳单调递增");

    // 第 30 个 cluster 应当在 30 秒附近
    c.check(
        idx.clusters[30]
            .timestamp_ms
            .is_some_and(|t| (29_000..=31_000).contains(&t)),
        &format!(
            "第 30 个 Cluster 时间戳约 30s（实际 {:?}）",
            idx.clusters[30].timestamp_ms
        ),
    );

    // ---------- 2. 与 spike 的裸扫结果交叉验证 ----------
    println!("\n--- 2. 与裸字节扫描交叉验证（防止 EBML 解析跑偏）---");
    // spike 用扫 1F43B675 找到 60 个 cluster，偏移 754, 62694, 125399...
    // 正规解析必须得到相同结果，否则说明其中一个是错的
    let mut naive = Vec::new();
    let mut i = 0usize;
    while i + 4 <= data.len() {
        if data[i] == 0x1F && data[i + 1] == 0x43 && data[i + 2] == 0xB6 && data[i + 3] == 0x75 {
            naive.push(i as u64);
            i += 4;
        } else {
            i += 1;
        }
    }
    c.check_eq(naive.len(), idx.clusters.len(), "裸扫与 EBML 解析的 Cluster 数量一致");
    let offsets_match = naive
        .iter()
        .zip(idx.clusters.iter())
        .all(|(n, cl)| *n == cl.offset);
    c.check(offsets_match, "裸扫与 EBML 解析的 Cluster 偏移逐个一致");

    // ---------- 3. cluster_for_time ----------
    println!("\n--- 3. 时间→Cluster 定位 ---");
    c.check_eq(idx.cluster_for_time(0), Some(0), "0s → 第 0 个");
    // 30.5 秒应当落在第 30 个（向前取整，不能跳到 31）
    let at30 = idx.cluster_for_time(30_500);
    c.check_eq(at30, Some(30), "30.5s → 第 30 个（向前取整）");
    // 超出时长取最后一个
    c.check_eq(
        idx.cluster_for_time(999_999),
        Some(59),
        "超出时长 → 最后一个",
    );

    // ---------- 4. 裸 Cluster 必须失败 ----------
    println!("\n--- 4. 裸 Cluster（无头部）应当无法 demux ---");
    let (off, len) = idx.cluster_range(30, 2000).expect("应有区间");
    let region = &data[off as usize..(off + len) as usize];
    let bare = remux::remux_to_fmp4(region, &remux::RemuxOptions::default());
    c.check(
        bare.is_err(),
        "裸 Cluster remux 必须失败（缺 Tracks 元素）",
    );
    if let Err(e) = &bare {
        println!("      （错误信息：{}）", {
            let s = format!("{e}");
            s.chars().take(90).collect::<String>()
        });
    }

    // ---------- 5. 头部 + Cluster 拼接（P2 seek 的核心）----------
    println!("\n--- 5. 头部 + 中间 Cluster 拼接 remux（P2 seek 核心）---");
    let spliced = mkv::splice(&data, idx.header_len, region).expect("拼接失败");
    c.check_eq(
        spliced.len() as u64,
        idx.header_len + len,
        "拼接长度 = 头部 + 区间",
    );

    match remux::remux_to_fmp4(&spliced, &remux::RemuxOptions::default()) {
        Ok(out) => {
            c.check(true, "头部+Cluster 能成功 remux");
            c.check(
                mp4::looks_like_mp4(&out.data),
                "产物是有效 MP4 结构",
            );
            match remux::has_fragments(&out.data) {
                Ok(true) => c.check(true, "产物含 moof（是分片 MP4）"),
                _ => c.check(false, "产物含 moof（是分片 MP4）"),
            }
            // 关键：真能被解析，而不只是长得像
            match probe_bytes(&out.data) {
                Some((dur, streams)) => {
                    c.check(
                        (1.0..=5.0).contains(&dur),
                        &format!("产物时长约 2-3 秒（实际 {dur:.2}s）"),
                    );
                    c.check_eq(streams, 2, "产物含 2 条流（视频+音频）");
                }
                None => {
                    c.check(false, "产物可被 ffprobe 解析");
                }
            }
            if !out.warnings.is_empty() {
                println!("      （FFmpeg 警告：{:?}）", out.warnings);
            }
        }
        Err(e) => {
            c.check(false, &format!("头部+Cluster 能成功 remux（{e}）"));
        }
    }

    // ---------- 6. 不同位置的 seek 都要可用 ----------
    println!("\n--- 6. 多个 seek 点都能产出可播放片段 ---");
    for target_s in [0u64, 10, 25, 45, 59] {
        let idx_at = idx.cluster_for_time(target_s * 1000);
        let Some(ci) = idx_at else {
            c.check(false, &format!("{target_s}s 能定位到 Cluster"));
            continue;
        };
        let Some((o, l)) = idx.cluster_range(ci, 1000) else {
            c.check(false, &format!("{target_s}s 能算出字节区间"));
            continue;
        };
        let reg = &data[o as usize..(o + l) as usize];
        let sp = match mkv::splice(&data, idx.header_len, reg) {
            Ok(s) => s,
            Err(_) => {
                c.check(false, &format!("{target_s}s 能拼接"));
                continue;
            }
        };
        match remux::remux_to_fmp4(&sp, &remux::RemuxOptions::default()) {
            Ok(out) => {
                let ok = mp4::looks_like_mp4(&out.data)
                    && remux::has_fragments(&out.data).unwrap_or(false)
                    && probe_bytes(&out.data).is_some();
                c.check(
                    ok,
                    &format!(
                        "seek 到 {target_s}s → 可播放片段（{} 字节）",
                        out.data.len()
                    ),
                );
            }
            Err(e) => {
                c.check(false, &format!("seek 到 {target_s}s → 可播放片段（{e}）"));
            }
        }
    }

    // ---------- 7. init segment ----------
    println!("\n--- 7. init segment（MSE 必需）---");
    let head_only = mkv::splice(&data, idx.header_len, &[]).expect("拼接失败");
    // init segment 需要至少能读到 Tracks，用头部+第一个 cluster 更稳
    let (o0, l0) = idx.cluster_range(0, 0).expect("应有区间");
    let init_src = mkv::splice(&data, idx.header_len, &data[o0 as usize..(o0 + l0) as usize])
        .expect("拼接失败");
    match remux::init_segment(&init_src, &remux::RemuxOptions::default()) {
        Ok(out) => {
            c.check(true, "能产出 init segment");
            c.check(mp4::looks_like_mp4(&out.data), "init 是有效 MP4");
            // init segment 的定义就是**不含**媒体片段
            match remux::has_fragments(&out.data) {
                Ok(false) => c.check(true, "init segment 不含 moof（符合定义）"),
                Ok(true) => c.check(false, "init segment 不含 moof（符合定义）"),
                Err(_) => c.check(false, "init segment 的 box 结构可解析"),
            }
            // 应当远小于媒体片段
            c.check(
                out.data.len() < 10_000,
                &format!("init segment 很小（{} 字节）", out.data.len()),
            );
            // 必须含 moov（描述轨道），否则 MSE 无法初始化
            let boxes = mp4::top_level_boxes(&out.data).unwrap_or_default();
            let has_moov = boxes.iter().any(|b| b.kind == *b"moov");
            let has_ftyp = boxes.iter().any(|b| b.kind == *b"ftyp");
            c.check(has_ftyp && has_moov, "init segment 含 ftyp + moov");
        }
        Err(e) => {
            c.check(false, &format!("能产出 init segment（{e}）"));
        }
    }
    let _ = head_only;

    // ---------- 8. 轨道映射 ----------
    println!("\n--- 8. 轨道映射（文档 §5.4 的多轨道可选）---");
    let vonly = remux::RemuxOptions {
        map: vec!["0:v:0".into()],
        ..remux::RemuxOptions::default()
    };
    match remux::remux_to_fmp4(&spliced, &vonly) {
        Ok(out) => match probe_bytes(&out.data) {
            Some((_, n)) => c.check_eq(n, 1, "只映射视频轨时产物只有 1 条流"),
            None => c.check(false, "只映射视频轨的产物可解析"),
        },
        Err(e) => c.check(false, &format!("只映射视频轨能 remux（{e}）")),
    }

    // ---------- 9. 整文件 remux（对照）----------
    println!("\n--- 9. 整文件 remux（对照，应当保留完整内容）---");
    match remux::remux_to_fmp4(&data, &remux::RemuxOptions::default()) {
        Ok(out) => {
            c.check(true, "整文件能 remux");

            // 注意：这里**不能**用管道探测去核对时长。
            //
            // 实测对照（spikes/dbg-fmp4-duration.ps1）：
            //   fMP4 + 文件路径（可 seek）  → 60.02s ✅
            //   fMP4 + 管道（不可 seek）    → 2.04s  ❌
            //   非分片 MP4 + 管道           → 60.00s ✅
            //   真解码全片                  → 60.02s ✅
            //
            // 原因是 `empty_moov` 让 moov 不含总时长，时长分散在各个
            // moof 里，管道读不到末尾就只能报第一个 fragment 的时长。
            // 这是 fMP4 的固有特性，不是 remux 出了问题。
            //
            // 所以整片的完整性改用**结构证据**核对：moof 数量。
            // 素材是 60 秒、2 秒一片，应当有约 30 个以上 moof。
            let boxes = mp4::top_level_boxes(&out.data).unwrap_or_default();
            let moof_count = boxes.iter().filter(|b| b.kind == *b"moof").count();
            c.check(
                moof_count >= 30,
                &format!("整文件产出足够多的 fragment（{moof_count} 个 moof）"),
            );
            // 片段数量应当与时长相称：60 秒素材不该只有两三片
            c.check(
                moof_count > 10,
                &format!("fragment 数量与 60 秒时长相称（{moof_count} 个）"),
            );

            // remux 不重编码，尺寸应当接近原文件
            #[allow(clippy::cast_precision_loss)]
            let ratio = out.data.len() as f64 / data.len() as f64;
            c.check(
                (0.9..=1.15).contains(&ratio),
                &format!("remux 无明显膨胀（{:.1}%）", ratio * 100.0),
            );
        }
        Err(e) => c.check(false, &format!("整文件能 remux（{e}）")),
    }

    // ---------- 10. 非 MKV 输入 ----------
    println!("\n--- 10. 非 MKV 输入应当明确拒绝 ---");
    let mp4_path = dir.join("h264_aac.mp4");
    if mp4_path.is_file() {
        let mp4_data = std::fs::read(&mp4_path).unwrap_or_default();
        c.check(
            mkv::parse(&mp4_data).is_err(),
            "MP4 文件不该被当成 MKV 解析",
        );
        c.check(!mkv::looks_like_mkv(&mp4_data), "looks_like_mkv 正确排除 MP4");
    }
    let txt = dir.join("not_media.txt");
    if txt.is_file() {
        let t = std::fs::read(&txt).unwrap_or_default();
        c.check(mkv::parse(&t).is_err(), "文本文件不该被解析为 MKV");
    }

    // ---------- 11. 截断文件 ----------
    println!("\n--- 11. 截断文件（缺片场景）---");
    let cut = &data[..data.len() / 2];
    match mkv::parse(cut) {
        Ok(ci) => {
            c.check(
                ci.clusters.len() < idx.clusters.len(),
                &format!(
                    "截断后解析出较少 Cluster（{} < {}）",
                    ci.clusters.len(),
                    idx.clusters.len()
                ),
            );
            c.check(ci.has_tracks, "截断后仍能读到 Tracks（头部完整）");
            // 截断的前半部分仍应能 remux 出可播放内容
            if let Some((o, l)) = ci.cluster_range(0, 2000)
                && (o + l) as usize <= cut.len()
            {
                let reg = &cut[o as usize..(o + l) as usize];
                if let Ok(sp) = mkv::splice(cut, ci.header_len, reg) {
                    let ok = remux::remux_to_fmp4(&sp, &remux::RemuxOptions::default())
                        .is_ok_and(|out| probe_bytes(&out.data).is_some());
                    c.check(ok, "截断文件的可用部分仍能播放");
                }
            }
        }
        Err(e) => {
            println!("      （截断文件解析失败，可接受：{e}）");
        }
    }

    // ---------- 汇总 ----------
    println!("\n{}", "=".repeat(64));
    println!("结果: {} 通过, {} 失败", c.pass, c.fail.len());
    if !c.fail.is_empty() {
        println!("\n失败明细:");
        for f in &c.fail {
            println!("  - {f}");
        }
    }
    println!("{}", "=".repeat(64));

    if !c.fail.is_empty() {
        std::process::exit(1);
    }
}
