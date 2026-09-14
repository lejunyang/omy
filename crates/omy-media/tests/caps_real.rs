//! 能力探测跑在真实 FFmpeg 上的验证。
//!
//! 与 `caps.rs` 里的单元测试分工不同：那些喂的是抄来的样本文本，保证解析
//! 规则本身对；这里跑**真实进程**，保证「样本没抄错、真实输出没变样」。
//!
//! 单元测试过而这里挂，说明真实 FFmpeg 的输出格式和我们以为的不一样——
//! 那正是最该被抓住的情况。

/// 对当前环境里的 FFmpeg 跑一次真实探测。
///
/// 没有 FFmpeg 时跳过而不是失败：CI 的某些任务确实不装它，
/// 而「没装 FFmpeg 会怎样」由单元测试覆盖。
#[test]
fn detects_real_ffmpeg_consistently() {
    let caps = omy_media::caps::refresh();

    if !caps.has_ffmpeg {
        eprintln!("跳过：本机没有 ffmpeg");
        return;
    }

    // 版本号必须拿得到。拿不到说明 -version 的输出格式变了，
    // 而 doctor 正靠它告诉用户「你现在用的是哪一份」
    assert!(
        caps.version.as_ref().is_some_and(|v| v.contains("ffmpeg")),
        "版本首行不含 ffmpeg 字样: {:?}",
        caps.version
    );

    // 不论哪个版本，MP4 muxer 都必须在——omy 的转封装全靠它。
    // 缺了它说明这份 FFmpeg 根本不适合给 omy 用
    assert!(
        caps.has_muxer("mp4"),
        "没有 mp4 muxer，muxers={:?}",
        caps.muxers
    );

    // 解析不能吐出垃圾名字。真实输出里混进图例行或空串时这里会炸，
    // 而单元测试用的是抄来的样本，抓不到「真实格式变了」
    for e in caps
        .video_encoders
        .iter()
        .chain(caps.audio_encoders.iter())
        .chain(caps.muxers.iter())
    {
        assert!(!e.is_empty(), "出现空名字");
        assert!(e != "=", "图例行被当成了名字");
        assert!(
            !e.contains(char::is_whitespace),
            "名字里有空白，说明列切分错了: {e:?}"
        );
    }

    // 自洽：报告有 libx264 就必须报告能重编码，反之亦然
    assert_eq!(
        caps.can_reencode_video(),
        !caps.video_encoders.is_empty(),
        "can_reencode_video 与 video_encoders 不一致"
    );

    eprintln!(
        "实测能力：视频编码器 {:?} / 音频 {:?} / 容器 {:?}",
        caps.video_encoders, caps.audio_encoders, caps.muxers
    );
}

/// 内置裁剪版必须被判定为「不能压缩」。
///
/// 这条只在 `OMY_TEST_BUILTIN_FFMPEG` 指向内置产物时跑，因为它断言的是
/// **那一份**的特征。CI 里构建完 FFmpeg 后设上这个变量即可。
///
/// 不这样断言会怎样：配方哪天顺手加进了编码器，压缩会悄悄变成可用，
/// 而 LGPL→GPL 的许可证后果没人注意到——这正是最难发现的一类回归。
#[test]
fn builtin_build_reports_no_video_encoder() {
    let Some(path) = std::env::var_os("OMY_TEST_BUILTIN_FFMPEG") else {
        eprintln!("跳过：未设 OMY_TEST_BUILTIN_FFMPEG");
        return;
    };
    if !std::path::Path::new(&path).is_file() {
        eprintln!("跳过：OMY_TEST_BUILTIN_FFMPEG 指向的文件不存在");
        return;
    }

    // 通过环境变量让 omy-media 用这一份。
    // SAFETY 说明：测试进程内单线程设置，且紧接着就读取。
    unsafe {
        std::env::set_var("OMY_FFMPEG", &path);
    }
    let caps = omy_media::caps::refresh();

    assert!(caps.has_ffmpeg, "指定的内置 ffmpeg 没被找到");
    assert!(
        caps.video_encoders.is_empty(),
        "内置裁剪版不应含视频编码器，实际: {:?}。\
         若这是有意添加的，请同步更新许可证文档（10 号）与配方说明",
        caps.video_encoders
    );
    assert!(
        !caps.can_encode_aac(),
        "内置裁剪版不应含 AAC 编码器，实际: {:?}",
        caps.audio_encoders
    );
    // 但转封装必须可用，否则内置这份就失去意义了
    assert!(caps.has_muxer("mp4"), "内置版必须能输出 MP4");
}
