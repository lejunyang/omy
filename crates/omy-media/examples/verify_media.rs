//! 用**真实媒体文件**和**真实 ffprobe** 独立验证 omy-media。
//!
//! # 为什么必须有这个
//!
//! 单元测试用的是手写的 JSON 样本。那只能验证「给定这段 JSON 能否正确
//! 解析」，无法验证「真实 ffprobe 的输出是否真的长这样」。两者同源时，
//! 我对格式的误解会**同时**存在于样本和实现里——测试全绿，真实文件却
//! 失败。omy-cli 的 `cat --range` off-by-one 正是这么漏过去的：
//! 断言写的是错的期望值，恰好匹配错的实现。
//!
//! 因此这里的每个断言都基于**人工按文档 §5.2 判定的期望**，
//! 而不是"实现算出什么就认什么"。
//!
//! 运行：`cargo run --release --example verify_media`
//! 前置：`pwsh -File spikes/make-media-fixtures.ps1`

use omy_media::{
    ffprobe, meta, mp4, prepare,
    probe::{self, Source},
    thumbnail,
    tier::{self, PlaybackTier},
};
use std::path::{Path, PathBuf};

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
        let ok = got == want;
        if ok {
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
    // 从 target/release/examples 回到仓库根
    let mut d = std::env::current_exe().unwrap_or_default();
    for _ in 0..3 {
        d.pop();
    }
    let p = d.join("spikes").join("fixtures").join("media");
    if p.is_dir() {
        return p;
    }
    // 回退到当前工作目录
    PathBuf::from("spikes").join("fixtures").join("media")
}

fn main() {
    println!("=== omy-media 独立验证（真实文件 + 真实 ffprobe）===\n");

    if !ffprobe::has_ffprobe() {
        println!("环境缺少 ffprobe，无法进行独立验证。");
        println!("这本身是 omy-media 必须优雅处理的情况，但验证需要它。");
        std::process::exit(2);
    }
    match ffprobe::version(ffprobe::Tool::Ffprobe) {
        Ok(v) => println!("ffprobe: {v}"),
        Err(e) => println!("取版本失败: {e}"),
    }
    if let Some(p) = ffprobe::ffprobe_path() {
        println!("路径: {}", p.display());
    }

    let dir = fixture_dir();
    println!("素材目录: {}\n", dir.display());
    if !dir.is_dir() {
        println!("素材目录不存在。请先运行 spikes/make-media-fixtures.ps1");
        std::process::exit(2);
    }

    let mut c = Checker::new();

    // ---- 1. 各素材的分级判定 ----
    // 期望值来自文档 §5.2 的人工判定，不是"实现算出什么就认什么"
    println!("--- 1. 分级判定（期望值按文档 §5.2 人工判定）---");
    let cases: &[(&str, PlaybackTier, &str)] = &[
        ("h264_aac.mp4", PlaybackTier::P1, "MP4+H264+AAC 全原生"),
        (
            "h264_aac.mkv",
            PlaybackTier::P2,
            "MKV 容器不被 <video> 支持，但可 remux",
        ),
        ("vp9_opus.webm", PlaybackTier::P1, "合法 WebM 可直通"),
        (
            "mpeg4_mp3.avi",
            PlaybackTier::P3,
            "MPEG-4 ASP 编码两端都不支持",
        ),
        ("audio_only.mp3", PlaybackTier::P1, "纯音频 MP3"),
        ("h264_only.mp4", PlaybackTier::P1, "无音轨不构成障碍"),
    ];

    for (name, want_tier, why) in cases {
        let path = dir.join(name);
        if !path.is_file() {
            c.check(false, &format!("{name} 素材缺失"));
            continue;
        }
        match probe::probe(Source::Path(&path)) {
            Ok(info) => {
                let v = tier::classify(&info);
                c.check_eq(
                    v.tier,
                    *want_tier,
                    &format!("{name} → {} （{why}）", want_tier.as_str()),
                );
                println!(
                    "        容器={} 视频={:?} 音频={:?} 理由={}",
                    info.container,
                    info.video.first().map(|x| x.codec.as_str()),
                    info.audio.first().map(|x| x.codec.as_str()),
                    v.reason
                );
            }
            Err(e) => c.check(false, &format!("{name} 探测失败: {e}")),
        }
    }

    // ---- 2. MKV 与 WebM 的 format_name 确实相同 ----
    // 这是 tier 模块里最关键的一个假设，必须用真实文件证实。
    // 若这个前提不成立，那段区分逻辑就是多余的复杂度。
    println!("\n--- 2. 验证 MKV 与 WebM 共享 format_name（tier 逻辑的前提）---");
    let mkv = probe::probe(Source::Path(&dir.join("h264_aac.mkv")));
    let webm = probe::probe(Source::Path(&dir.join("vp9_opus.webm")));
    match (&mkv, &webm) {
        (Ok(m), Ok(w)) => {
            println!("        MKV  format_name = {}", m.container);
            println!("        WebM format_name = {}", w.container);
            c.check(
                m.container_is("webm") && w.container_is("webm"),
                "两者的 format_name 都含 webm（故不能只靠容器名判断）",
            );
            // 且分级必须不同
            c.check(
                tier::classify(m).tier != tier::classify(w).tier,
                "尽管容器名相同，分级必须不同",
            );
        }
        _ => c.check(false, "MKV/WebM 探测失败"),
    }

    // ---- 3. 带封面的 MP3 不被误判为视频 ----
    println!("\n--- 3. attached_pic 处理（真实世界最常见的误判源）---");
    let p = dir.join("with_cover.mp3");
    if p.is_file() {
        match probe::probe(Source::Path(&p)) {
            Ok(info) => {
                c.check(!info.has_video(), "带封面的 MP3 不应被判定为含视频");
                c.check(info.is_audio_only(), "应识别为纯音频");
                c.check_eq(
                    tier::classify(&info).tier,
                    PlaybackTier::P1,
                    "带封面 MP3 应为 P1",
                );
            }
            Err(e) => c.check(false, &format!("with_cover.mp3 探测失败: {e}")),
        }
    } else {
        c.check(false, "with_cover.mp3 素材缺失");
    }

    // ---- 4. 非媒体文件返回 NotMedia 而非崩溃 ----
    println!("\n--- 4. 非媒体文件的处理 ---");
    let txt = dir.join("not_media.txt");
    if txt.is_file() {
        match probe::probe(Source::Path(&txt)) {
            Ok(info) => c.check(
                false,
                &format!("文本文件竟被识别为媒体：{}", info.container),
            ),
            Err(e) => {
                c.check(e.is_degradable(), "文本文件应返回可降级的错误而非硬失败");
                c.check_eq(e.code(), "NOT_MEDIA", "错误码应为 NOT_MEDIA");
            }
        }
    }

    // ---- 5. 管道模式（加密文件必须走这条路）----
    // 加密文件的明文只在内存里，不能给 ffprobe 文件路径。
    // 这条路径若不通，整个加密媒体的探测就无法实现。
    println!("\n--- 5. 管道模式探测（加密文件的唯一可行路径）---");
    let mp4_path = dir.join("h264_aac.mp4");
    match std::fs::read(&mp4_path) {
        Ok(bytes) => {
            match probe::probe(Source::Bytes(&bytes)) {
                Ok(info) => {
                    c.check(info.container_is("mp4"), "管道模式能识别容器");
                    c.check(info.has_video(), "管道模式能识别视频轨");
                    c.check(
                        info.duration_ms.is_some_and(|d| d > 7000 && d < 9000),
                        "管道模式能得到约 8 秒的时长",
                    );
                    // 与路径模式的结果比对：两条路径应当一致
                    if let Ok(byname) = probe::probe(Source::Path(&mp4_path)) {
                        c.check_eq(
                            tier::classify(&info).tier,
                            tier::classify(&byname).tier,
                            "管道模式与路径模式的分级一致",
                        );
                    }
                }
                Err(e) => c.check(false, &format!("管道模式探测失败: {e}")),
            }

            // ---- 6. MP4 box 解析 ----
            println!("\n--- 6. MP4 box 解析与 moov 定位 ---");
            c.check(mp4::looks_like_mp4(&bytes), "识别出 MP4 家族");
            match mp4::top_level_boxes(&bytes) {
                Ok(boxes) => {
                    let names: Vec<String> = boxes.iter().map(mp4::BoxRange::kind_str).collect();
                    println!("        顶层 box: {}", names.join(" "));
                    c.check(names.iter().any(|n| n == "ftyp"), "含 ftyp");
                    c.check(names.iter().any(|n| n == "moov"), "含 moov");
                    c.check(names.iter().any(|n| n == "mdat"), "含 mdat");
                    // box 长度之和应当等于（或非常接近）文件大小。
                    // 这是检验解析正确性的独立手段：若某个 box 的长度算错，
                    // 累加值就会偏离。
                    let sum: u64 = boxes.iter().map(|b| b.size).sum();
                    c.check(
                        sum == bytes.len() as u64,
                        &format!("box 长度之和 {sum} 等于文件大小 {}", bytes.len()),
                    );
                }
                Err(e) => c.check(false, &format!("box 解析失败: {e}")),
            }
            match mp4::find_moov(&bytes) {
                Ok(Some(m)) => {
                    println!("        moov: offset={} size={}", m.offset, m.size);
                    // 用独立手段验证：该位置的 4 字节确实是 "moov"
                    let at = usize::try_from(m.offset).unwrap_or(0) + 4;
                    let tag = bytes.get(at..at + 4).unwrap_or(&[]);
                    c.check(tag == b"moov", "moov 偏移处的字节确实是 'moov'");
                }
                Ok(None) => c.check(false, "未找到 moov"),
                Err(e) => c.check(false, &format!("moov 定位失败: {e}")),
            }

            // ---- 7. 尾部 moov 的重排能力 ----
            // 这是让「只读头部」可行的前提：moov 前移后元数据就在头部了。
            println!("\n--- 7. 尾部 moov 重排（纯 Rust，不依赖 FFmpeg）---");
            match mp4::to_faststart(&bytes) {
                Ok(Some(re)) => {
                    c.check_eq(re.len(), bytes.len(), "重排后总长度不变（只搬动不增删）");
                    c.check(
                        mp4::is_faststart(&re).unwrap_or(false),
                        "重排后 moov 位于 mdat 之前",
                    );
                    // 独立验证：重排产物必须仍是 FFmpeg 认得的合法 MP4，
                    // 且能从中读出与原文件一致的信息。
                    // 若 stco 偏移没修对，这里会失败（实测过：报 Invalid NAL unit size）
                    match probe::probe(Source::Bytes(&re)) {
                        Ok(ri) => {
                            let orig = probe::probe(Source::Path(&mp4_path)).ok();
                            c.check(
                                ri.has_video() && ri.container_is("mp4"),
                                "重排产物仍是可探测的合法 MP4",
                            );
                            if let Some(o) = orig {
                                c.check_eq(
                                    ri.video.first().map(|v| v.codec.clone()),
                                    o.video.first().map(|v| v.codec.clone()),
                                    "重排前后视频编码一致",
                                );
                                // 时长一致是偏移修正正确的强证据
                                let close = match (ri.duration_ms, o.duration_ms) {
                                    (Some(a), Some(b)) => a.abs_diff(b) < 200,
                                    _ => false,
                                };
                                c.check(close, "重排前后时长一致（证明偏移修正正确）");
                            }
                        }
                        Err(e) => c.check(false, &format!("重排产物无法探测: {e}")),
                    }

                    // 现在头部就含元数据了，验证只读头部可行
                    let head_len = re.len().min(512 * 1024);
                    match probe::probe(Source::Bytes(&re[..head_len])) {
                        Ok(hi) => c.check(
                            hi.has_video(),
                            &format!("重排后仅前 {} KiB 即可识别视频轨", head_len / 1024),
                        ),
                        Err(e) => println!("        头部探测仍失败（{e}）"),
                    }
                }
                Ok(None) => println!("        本素材已是 faststart，跳过"),
                Err(e) => c.check(false, &format!("重排失败: {e}")),
            }

            // ---- 8. 视频抽帧 ----
            println!("\n--- 8. 视频抽帧生成缩略图 ---");
            if ffprobe::has_ffmpeg() {
                match thumbnail::from_video_bytes(bytes.clone(), 3.0, 320) {
                    Ok(t) => {
                        println!(
                            "        {}x{} {} 字节 {:?}",
                            t.width,
                            t.height,
                            t.bytes.len(),
                            t.format
                        );
                        c.check(
                            t.bytes.len() <= thumbnail::SIZE_LIMIT,
                            &format!("缩略图 {} 字节未超上限", t.bytes.len()),
                        );
                        c.check(t.width <= 320, "宽度不超过 max_edge");
                        // 独立验证：产出的字节必须真能解码成图片。
                        // 「命令成功」不等于「产物有效」。
                        c.check(
                            image_decodable(&t.bytes),
                            "抽出的帧确实是可解码的图片",
                        );
                    }
                    Err(e) => c.check(false, &format!("抽帧失败: {e}")),
                }

                // moov 在尾部的 MP4 无法直接管道抽帧（FFmpeg 报
                // "Cannot determine format after EOF"，stdout 为空）。
                // 实现会自动先 remux 成分片 MP4 再抽帧。
                // 这一项验证该自动处理真的生效——它覆盖了真实世界里
                // 大量录屏与相机直出的视频。
                println!("\n--- 8b. moov 在尾部时自动 remux 后抽帧 ---");
                match mp4::find_moov(&bytes) {
                    Ok(Some(m)) => {
                        let ratio = m.offset as f64 / bytes.len() as f64;
                        println!(
                            "        本素材 moov 在 {:.0}% 处（{}）",
                            ratio * 100.0,
                            if ratio > 0.5 { "尾部" } else { "前部" }
                        );
                        c.check(
                            !mp4::is_faststart(&bytes).unwrap_or(true),
                            "确认本素材不是 faststart 布局（前提）",
                        );
                        match thumbnail::from_video_bytes(bytes.clone(), 3.0, 320) {
                            Ok(t) => {
                                // 必须验证内容特征，不能只看长度非零——
                                // 中途正是因为把 stderr 当产物而误判过一次
                                c.check(
                                    image_decodable(&t.bytes),
                                    &format!(
                                        "尾部 moov 经自动 remux 后抽帧成功（{} 字节，内容已验证可解码）",
                                        t.bytes.len()
                                    ),
                                );
                            }
                            Err(e) => c.check(false, &format!("尾部 moov 抽帧失败: {e}")),
                        }
                    }
                    _ => println!("        跳过：未定位到 moov"),
                }

                // 与 faststart 版本对比：验证"产物有效性"判据在两种布局下都成立
                let fs = dir.join("h264_aac_faststart.mp4");
                if fs.is_file()
                    && let Ok(fb) = std::fs::read(&fs)
                {
                    match thumbnail::from_video_bytes(fb, 3.0, 320) {
                        Ok(t) => c.check(
                            image_decodable(&t.bytes),
                            &format!("faststart 版本同样能抽帧（{} 字节）", t.bytes.len()),
                        ),
                        Err(e) => c.check(false, &format!("faststart 抽帧失败: {e}")),
                    }
                }
            } else {
                println!("        跳过：缺少 ffmpeg");
            }
        }
        Err(e) => c.check(false, &format!("读取素材失败: {e}")),
    }

    // ---- 9. FLAC in MKV：容易判错的组合 ----
    println!("\n--- 9. MKV + FLAC（FLAC 原生可解但 MP4 兼容性差）---");
    let p = dir.join("h264_flac.mkv");
    if p.is_file() {
        match probe::probe(Source::Path(&p)) {
            Ok(info) => {
                let v = tier::classify(&info);
                println!(
                    "        音频={:?} 分级={} 理由={}",
                    info.audio.first().map(|a| a.codec.as_str()),
                    v.tier.as_str(),
                    v.reason
                );
                // 不硬断言等级：FLAC 在 MP4 中的支持状况有争议。
                // 但**不能是 P1**——MKV 容器本身就不被 <video> 支持。
                c.check(v.tier != PlaybackTier::P1, "MKV 容器不应被判为 P1");
            }
            Err(e) => c.check(false, &format!("h264_flac.mkv 探测失败: {e}")),
        }
    }

    // ---- 10. prepare 一站式产出媒体 TLV ----
    // 这是加密流程真正调用的入口，必须用真实文件验证。
    println!("\n--- 10. prepare：一次探测产出三个 TLV ---");
    verify_prepare(&dir, &mut c);

    // ---- 11. MediaMeta 的往返与内容 ----
    println!("\n--- 11. MediaMeta JSON 往返（真实探测结果）---");
    verify_meta(&dir, &mut c);

    // ---- 汇总 ----
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

/// 用真实文件验证 `prepare` 的产出。
fn verify_prepare(dir: &Path, c: &mut Checker) {
    let mp4_path = dir.join("h264_aac.mp4");
    let Ok(bytes) = std::fs::read(&mp4_path) else {
        c.check(false, "读取 h264_aac.mp4 失败");
        return;
    };

    let out = prepare::prepare(&bytes, &prepare::PrepareOptions::default());
    if !out.warnings.is_empty() {
        println!("        警告: {:?}", out.warnings);
    }

    c.check(out.media_meta.is_some(), "产出 media_meta");
    c.check(out.moov_cache.is_some(), "产出 moov_cache");
    c.check(out.thumbnail.is_some(), "产出 thumbnail");
    c.check(out.info.is_some(), "带回探测信息");
    c.check(out.warnings.is_empty(), "正常 MP4 不该有警告");

    // moov_cache 必须是**原始**字节，不是重排后的。
    // 这是最容易搞错的一点：若存了重排后的 moov，其 stco 偏移
    // 与实际载荷不符，播放时会解出乱码。
    if let (Some(cached), Ok(Some(r))) = (out.moov_cache.as_ref(), mp4::find_moov(&bytes)) {
        let s = usize::try_from(r.offset).unwrap_or(0);
        let e = s + usize::try_from(r.size).unwrap_or(0);
        c.check(
            bytes.get(s..e) == Some(cached.as_slice()),
            "moov_cache 与原文件对应区间逐字节相同（不是重排后的）",
        );
        c.check(
            cached.get(4..8) == Some(b"moov"),
            "moov_cache 开头是 'moov' 标签",
        );

        // 反证：重排后的 moov 与缓存必须不同，否则说明存错了
        if let Ok(Some(re)) = mp4::to_faststart(&bytes)
            && let Ok(Some(r2)) = mp4::find_moov(&re)
        {
            let s2 = usize::try_from(r2.offset).unwrap_or(0);
            let e2 = s2 + usize::try_from(r2.size).unwrap_or(0);
            let reordered = re.get(s2..e2).unwrap_or(&[]);
            c.check(
                reordered != cached.as_slice(),
                "重排后的 moov 与缓存内容不同（证明缓存取的是原始版本）",
            );
        }
    }

    // 缩略图必须是可解码的真图片
    if let Some(t) = &out.thumbnail {
        c.check(image_decodable(t), "prepare 产出的缩略图可解码");
        c.check(
            t.len() <= thumbnail::SIZE_LIMIT,
            &format!("缩略图 {} 字节未超 32 KiB 上限", t.len()),
        );
    }

    // 纯音频：不该有 moov，也不该因为抽不到帧而报警告
    if let Ok(ab) = std::fs::read(dir.join("audio_only.mp3")) {
        let ao = prepare::prepare(&ab, &prepare::PrepareOptions::default());
        c.check(ao.media_meta.is_some(), "纯音频也应产出 media_meta");
        c.check(ao.moov_cache.is_none(), "MP3 不该有 moov_cache");
        c.check(
            ao.warnings.is_empty(),
            &format!("纯音频不该有警告（抽帧应被跳过）：{:?}", ao.warnings),
        );
    }

    // 带封面的 MP3：封面是 attached_pic，不该被当成视频去抽帧
    if let Ok(cb) = std::fs::read(dir.join("with_cover.mp3")) {
        let co = prepare::prepare(&cb, &prepare::PrepareOptions::default());
        c.check(
            co.warnings.is_empty(),
            &format!("带封面 MP3 不该有警告：{:?}", co.warnings),
        );
    }

    // 非媒体文件：静默降级，不刷警告
    if let Ok(tb) = std::fs::read(dir.join("not_media.txt")) {
        let to = prepare::prepare(&tb, &prepare::PrepareOptions::default());
        c.check(to.is_empty(), "非媒体文件不该产出任何 TLV");
        c.check(
            to.warnings.is_empty(),
            &format!("非媒体文件不该产生警告：{:?}", to.warnings),
        );
    }

    // 全关时应完全不工作
    let none = prepare::prepare(&bytes, &prepare::PrepareOptions::none());
    c.check(none.is_empty(), "全部关闭时不产出任何 TLV");

    // 用户指定特定帧（需求确认项 B.2）
    let at = prepare::PrepareOptions {
        thumbnail: prepare::ThumbSource::VideoAt(5.0),
        ..prepare::PrepareOptions::default()
    };
    let ao = prepare::prepare(&bytes, &at);
    match &ao.thumbnail {
        Some(t) => c.check(image_decodable(t), "指定第 5 秒抽帧成功且可解码"),
        None => c.check(false, &format!("指定帧抽取失败：{:?}", ao.warnings)),
    }
}

/// 用真实探测结果验证 `MediaMeta`。
fn verify_meta(dir: &Path, c: &mut Checker) {
    // 用 MKV 双轨素材：字段最丰富
    let Ok(info) = probe::probe(Source::Path(&dir.join("h264_aac.mkv"))) else {
        c.check(false, "探测 h264_aac.mkv 失败");
        return;
    };

    let m = meta::MediaMeta::from_probe(&info);
    let Ok(json) = m.to_json_bytes() else {
        c.check(false, "MediaMeta 序列化失败");
        return;
    };
    println!("        JSON {} 字节", json.len());
    if let Ok(s) = std::str::from_utf8(&json) {
        let preview: String = s.chars().take(180).collect();
        println!("        {preview}");
    }

    match meta::MediaMeta::from_json_bytes(&json) {
        Ok(back) => {
            c.check(back == m, "MediaMeta 往返完全一致");
            // 与探测结果逐项核对，而非只看往返
            c.check_eq(
                back.container.as_str(),
                info.container.as_str(),
                "容器名一致",
            );
            c.check_eq(back.duration_ms, info.duration_ms, "时长一致");
            c.check_eq(
                back.video.as_ref().map(|v| v.codec.clone()),
                info.video.first().map(|v| v.codec.clone()),
                "视频编码一致",
            );
            c.check_eq(back.audio.len(), info.audio.len(), "音轨数量一致");
            c.check(
                back.tier() == Some(tier::classify(&info).tier),
                "分级能从 JSON 还原且与重新计算一致",
            );
            c.check(
                !back.playback_tier.reason.is_empty(),
                "分级理由非空（UI 要展示）",
            );
        }
        Err(e) => c.check(false, &format!("MediaMeta 反序列化失败: {e}")),
    }

    // 隐私：ffprobe 的 tags 不得进入 meta
    if let Ok(s) = String::from_utf8(json.clone()) {
        let low = s.to_lowercase();
        c.check(
            !low.contains("encoder") && !low.contains("lavf") && !low.contains("handler"),
            "meta 不含 ffprobe 的 encoder/handler 等 tag",
        );
    }

    // meta 应当足够小：10000 个文件都要常驻内存
    c.check(
        json.len() < 2048,
        &format!("meta 体积 {} 字节，应远小于 2 KiB", json.len()),
    );
}

/// 用独立手段确认字节是可解码的图片。
///
/// 检查两层：**魔数特征** + **能否真正解码**。
///
/// 只看长度非零是不够的——中途曾把 FFmpeg 的 844 字节错误文本
/// 当成 WebP 图片（因为用了 `2>&1` 把 stderr 混进产物，
/// 又只检查了长度）。魔数检查能立刻挡住这类误判。
fn image_decodable(bytes: &[u8]) -> bool {
    // 常见图片格式的魔数
    let is_webp = bytes.len() >= 12
        && bytes.get(..4) == Some(b"RIFF")
        && bytes.get(8..12) == Some(b"WEBP");
    let is_jpeg = bytes.get(..3) == Some(&[0xFF, 0xD8, 0xFF]);
    let is_png = bytes.get(..8) == Some(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    if !(is_webp || is_jpeg || is_png) {
        // 打出开头字节，便于判断到底收到了什么
        let head: Vec<String> = bytes
            .iter()
            .take(12)
            .map(|b| format!("{b:02x}"))
            .collect();
        println!("        魔数不匹配任何图片格式，前 12 字节: {}", head.join(" "));
        return false;
    }
    // 魔数对了还要真能解码
    thumbnail::from_image_bytes(bytes, 64, thumbnail::ThumbFormat::Jpeg).is_ok()
}

/// 让 `Path` 参数在未使用时不告警。
#[allow(dead_code)]
fn _unused(_: &Path) {}
