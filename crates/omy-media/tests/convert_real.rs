//! 转换跑在真实 FFmpeg 与真实视频上的验证。
//!
//! 单元测试只断言参数字符串长什么样，那证明不了产物能不能用。这里造一个
//! 真视频、真转一遍、再用 ffprobe 回读确认——用的是**与生成不同的路径**，
//! 这是「文件存在且非空」之外唯一能说明问题的检查。

use std::path::{Path, PathBuf};

/// 造一个几秒的测试视频。
///
/// 用 FFmpeg 自己的 testsrc 生成，不往仓库里塞二进制样本。
/// 需要视频编码器，所以没有完整版 FFmpeg 时会失败——调用方负责跳过。
fn make_sample(dir: &Path, name: &str, extra: &[&str]) -> Option<PathBuf> {
    let out = dir.join(name);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "testsrc=size=320x240:rate=15:duration=2".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        "sine=frequency=440:duration=2".into(),
    ];
    args.extend(extra.iter().map(|s| (*s).to_owned()));
    args.push(out.to_string_lossy().into_owned());

    let os: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    let r = omy_media::ffprobe::run_with_timeout(omy_media::ffprobe::Tool::Ffmpeg, &os, None).ok()?;
    if !r.success() || !out.is_file() {
        eprintln!("造样本失败: {}", r.stderr);
        return None;
    }
    Some(out)
}

/// 回读产物，返回 (容器, 视频编码, 音频编码)。
fn read_back(p: &Path) -> Option<(String, Option<String>, Option<String>)> {
    let info = omy_media::probe::probe(omy_media::probe::Source::Path(p)).ok()?;
    Some((
        info.container.clone(),
        info.video.first().map(|v| v.codec.clone()),
        info.audio.first().map(|a| a.codec.clone()),
    ))
}

/// MOV(PCM) → MP4：音轨必须被正确处理，不能悄悄丢掉。
///
/// 这是整条链上最容易让用户吃亏的一步：产物能播、画面正常，只是没声音。
/// 有 AAC 编码器时应转码保留，没有时才允许丢——而丢的时候界面必须已经
/// 告诉过用户。
#[test]
fn pcm_mov_to_mp4_keeps_audio_when_aac_available() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg 来造样本");
        return;
    }

    let dir = std::env::temp_dir().join("omy-convert-test-pcm");
    let _ = std::fs::create_dir_all(&dir);

    // 造一个 PCM 音轨的 MOV —— 正是原型里举的那个例子
    let Some(src) = make_sample(
        &dir,
        "src.mov",
        &["-c:v", "libx264", "-c:a", "pcm_s16le", "-t", "2"],
    ) else {
        eprintln!("跳过：造不出样本");
        return;
    };

    let (_, _, src_audio) = read_back(&src).expect("样本探测失败");
    assert_eq!(src_audio.as_deref(), Some("pcm_s16le"), "样本音轨不是 PCM");

    // 有 AAC：应当转码保留
    let plan = omy_media::convert::plan_audio(
        omy_media::convert::Container::Mp4,
        src_audio.as_deref(),
        caps.can_encode_aac(),
    );
    assert_eq!(
        plan,
        omy_media::convert::AudioPlan::ToAac,
        "有 AAC 编码器时不该丢音轨"
    );

    let out = dir.join("out.mp4");
    let r = omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: plan,
            subtitles: omy_media::convert::SubtitlePlan::None,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    assert!(r.size > 0);

    // 回读确认：容器变了、视频没被重编码、音轨还在且已是 AAC
    let (container, v, a) = read_back(&out).expect("产物探测失败");
    assert!(container.contains("mp4"), "容器不是 mp4: {container}");
    assert_eq!(v.as_deref(), Some("h264"), "视频编码被改了，应当是 copy");
    assert_eq!(a.as_deref(), Some("aac"), "音轨没了或不是 AAC");

    let _ = std::fs::remove_dir_all(&dir);
}

/// faststart 必须真的把 moov 移到前面。
///
/// 不这样断言会怎样：参数加了但没生效时，文件照样能播，只有「边下边播」
/// 才会卡——而那是最难复现的场景。这里用 mp4 模块直接检查盒子顺序。
#[test]
fn faststart_actually_moves_moov_to_front() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg");
        return;
    }

    let dir = std::env::temp_dir().join("omy-convert-test-fs");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample(&dir, "src.mkv", &["-c:v", "libx264", "-c:a", "aac", "-t", "2"])
    else {
        eprintln!("跳过：造不出样本");
        return;
    };

    let out = dir.join("out.mp4");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: omy_media::convert::SubtitlePlan::None,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    let data = std::fs::read(&out).expect("读不出产物");
    assert!(
        omy_media::mp4::is_faststart(&data).unwrap_or(false),
        "moov 没有被移到前面，faststart 实际没生效"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 产物必须能被 omy 自己的探测认出来，而不只是「文件非空」。
///
/// 转封装之后如果播放分级反而变差，这个功能就没有意义了。
#[test]
fn remuxed_output_is_probeable_and_not_worse() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg");
        return;
    }

    let dir = std::env::temp_dir().join("omy-convert-test-tier");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample(&dir, "src.mkv", &["-c:v", "libx264", "-c:a", "aac", "-t", "2"])
    else {
        eprintln!("跳过：造不出样本");
        return;
    };

    let before = omy_media::probe::probe(omy_media::probe::Source::Path(&src)).expect("源探测失败");
    let tier_before = omy_media::tier::classify(&before).tier;

    let out = dir.join("out.mp4");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: omy_media::convert::SubtitlePlan::None,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    let after = omy_media::probe::probe(omy_media::probe::Source::Path(&out)).expect("产物探测失败");
    let tier_after = omy_media::tier::classify(&after).tier;

    // H.264 + AAC 的 MKV 是 P2，转成 MP4 之后应当是 P1
    assert!(
        tier_after <= tier_before,
        "转封装后分级反而变差：{tier_before:?} -> {tier_after:?}"
    );
    assert_eq!(
        tier_after,
        omy_media::PlaybackTier::P1,
        "H.264+AAC 的 MP4 应当是 P1 直通"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ==================== 字幕 ====================

/// 造一个带文本字幕的样本。
///
/// 字幕来自临时 .srt 文件，`sub_codec` 决定它以什么编码写进容器
/// （`srt` 保持 SubRip，`ass` 转成 ASS）。
fn make_sample_with_subs(dir: &Path, name: &str, sub_codec: &str, container: &str) -> Option<PathBuf> {
    let srt = dir.join("sub.srt");
    std::fs::write(
        &srt,
        "1\n00:00:00,000 --> 00:00:01,000\nhello\n\n2\n00:00:01,000 --> 00:00:02,000\nworld\n",
    )
    .ok()?;

    let out = dir.join(name);
    let args: Vec<String> = vec![
        "-hide_banner".into(), "-loglevel".into(), "error".into(), "-y".into(),
        "-f".into(), "lavfi".into(), "-i".into(),
        "testsrc=size=320x240:rate=15:duration=2".into(),
        "-f".into(), "lavfi".into(), "-i".into(), "sine=frequency=440:duration=2".into(),
        "-i".into(), srt.to_string_lossy().into_owned(),
        "-map".into(), "0:v".into(), "-map".into(), "1:a".into(), "-map".into(), "2:s".into(),
        "-c:v".into(), "libx264".into(), "-c:a".into(), "aac".into(),
        "-c:s".into(), sub_codec.to_owned(),
        "-t".into(), "2".into(), "-f".into(), container.to_owned(),
        out.to_string_lossy().into_owned(),
    ];
    let os: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    let r = omy_media::ffprobe::run_with_timeout(omy_media::ffprobe::Tool::Ffmpeg, &os, None).ok()?;
    if !r.success() || !out.is_file() {
        eprintln!("造带字幕样本失败: {}", r.stderr);
        return None;
    }
    Some(out)
}

/// 读出产物里每条字幕轨的编码。
fn subtitle_codecs(p: &Path) -> Vec<String> {
    omy_media::probe::probe(omy_media::probe::Source::Path(p))
        .map(|i| i.subtitles.iter().map(|s| s.codec.clone()).collect())
        .unwrap_or_default()
}

/// SRT → MP4：字幕必须还在，且已转成 mov_text。
///
/// 不这样断言会怎样：`-c:s` 写成 srt 时 FFmpeg 报
/// "codec not currently supported in container" 整个转换失败；而漏掉
/// `-map 0:s?` 时字幕被静默丢掉——产物能播、画面正常，只是字幕没了。
/// 两种都要真转一遍才看得出来。
#[test]
fn srt_survives_into_mp4_as_mov_text() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg 来造样本");
        return;
    }
    let dir = std::env::temp_dir().join("omy-sub-srt-mp4");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample_with_subs(&dir, "src.mkv", "srt", "matroska") else {
        eprintln!("跳过：造不出样本");
        return;
    };
    assert_eq!(subtitle_codecs(&src), vec!["subrip"], "样本字幕不是 subrip");

    let plan = omy_media::convert::plan_subtitles(
        omy_media::convert::Container::Mp4,
        &["subrip".to_owned()],
        true,
    );
    assert_eq!(plan, omy_media::convert::SubtitlePlan::ToMovText);

    let out = dir.join("out.mp4");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: plan,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    assert_eq!(
        subtitle_codecs(&out),
        vec!["mov_text"],
        "字幕没了或编码不对——MP4 只认 mov_text"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ASS → MKV：原样保留，样式不丢。
///
/// 这是「想保样式就选 MKV」那句界面建议的依据。若 MKV 也把 ass 转掉了，
/// 那句建议就是在骗用户，而用户要到打开字幕才发现特效没了。
#[test]
fn ass_keeps_its_codec_in_mkv() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg");
        return;
    }
    let dir = std::env::temp_dir().join("omy-sub-ass-mkv");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample_with_subs(&dir, "src.mkv", "ass", "matroska") else {
        eprintln!("跳过：造不出样本");
        return;
    };
    assert_eq!(subtitle_codecs(&src), vec!["ass"], "样本字幕不是 ass");

    let plan = omy_media::convert::plan_subtitles(
        omy_media::convert::Container::Mkv,
        &["ass".to_owned()],
        true,
    );
    assert_eq!(plan, omy_media::convert::SubtitlePlan::Copy);

    let out = dir.join("out.mkv");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mkv,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: plan,
            faststart: false,
            video_encode: None,
        },
    )
    .expect("转换失败");

    assert_eq!(subtitle_codecs(&out), vec!["ass"], "MKV 里的 ASS 被改动了");
    let _ = std::fs::remove_dir_all(&dir);
}

/// ASS → MP4：按用户选择转成 mov_text，文字还在。
///
/// 样式丢失是已知代价（界面上写明了），但**文字必须保留**——
/// 若这里连文字都没了，那个选项就是在骗人。
#[test]
fn ass_into_mp4_keeps_the_text() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg");
        return;
    }
    let dir = std::env::temp_dir().join("omy-sub-ass-mp4");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample_with_subs(&dir, "src.mkv", "ass", "matroska") else {
        eprintln!("跳过：造不出样本");
        return;
    };

    let out = dir.join("out.mp4");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: omy_media::convert::SubtitlePlan::ToMovText,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    assert_eq!(
        subtitle_codecs(&out),
        vec!["mov_text"],
        "ASS 转 MP4 后字幕轨没了"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 丢弃字幕时产物里真的一条都没有。
///
/// 不这样断言会怎样：`-sn` 漏掉而 `-map 0:s?` 还在时，字幕会被照常写进去。
/// 用户明明选了「不要字幕」却还是有——这在隐私场景里是实质问题，
/// 字幕内容可能泄露影片信息。
#[test]
fn dropping_subtitles_really_removes_them() {
    let caps = omy_media::caps::refresh();
    if !caps.has_ffmpeg || !caps.can_reencode_video() {
        eprintln!("跳过：需要完整版 FFmpeg");
        return;
    }
    let dir = std::env::temp_dir().join("omy-sub-drop");
    let _ = std::fs::create_dir_all(&dir);

    let Some(src) = make_sample_with_subs(&dir, "src.mkv", "srt", "matroska") else {
        eprintln!("跳过：造不出样本");
        return;
    };
    assert!(!subtitle_codecs(&src).is_empty(), "样本本来就没有字幕，测不出东西");

    let out = dir.join("out.mp4");
    omy_media::convert::convert(
        &src,
        &out,
        &omy_media::convert::ConvertOptions {
            container: omy_media::convert::Container::Mp4,
            audio: omy_media::convert::AudioPlan::Copy,
            subtitles: omy_media::convert::SubtitlePlan::Drop,
            faststart: true,
            video_encode: None,
        },
    )
    .expect("转换失败");

    assert!(
        subtitle_codecs(&out).is_empty(),
        "选了丢弃字幕，产物里却还有：{:?}",
        subtitle_codecs(&out)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
