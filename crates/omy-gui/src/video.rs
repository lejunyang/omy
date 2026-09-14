//! 视频处理对话框的后端：能力探测、源信息、整文件转换。
//!
//! # 这些命令只作用于「未加密的本地文件」
//!
//! 视频处理发生在**加密之前**，此时文件还是磁盘上的明文。所以这里既不
//! 需要密钥，也不产生新的明文——与 `protocol.rs` 那条「按需解密」的路径
//! 完全不同。
//!
//! 但路径仍然必须走 `PlainRegistry` 的 token，理由与 `plain.rs` 相同：
//! 若接受前端传来的任意路径字符串，一段注入的脚本就能让我们去读、去
//! 转码、甚至去覆盖任意文件。token 只对「被浏览过的目录里真实存在的
//! 文件」发放。
//!
//! # 为什么转换产物落在源文件旁边
//!
//! 不放系统临时目录：那里可能在另一个磁盘上，几 GB 的视频跨盘写完再由
//! 加密流程读一遍，白白多一次全量拷贝。放在源文件同目录则通常同盘。
//!
//! 产物是**中间文件**，加密完成后由调用方删除。命名带 `.omytmp-` 前缀
//! 以便识别，万一进程崩溃留下残留，用户也能看出这是什么。

use crate::commands::{CmdError, CmdResult, Shared};
use omy_media::convert::{AudioPlan, Container, ConvertOptions, VideoEncode};
use std::path::PathBuf;

/// 返回给前端的能力集。
///
/// 直接转发 `omy_media::Capabilities`，不再包一层——字段含义完全一样，
/// 中间再定义一个同形结构只会带来两处要同步维护的定义。
pub type Caps = omy_media::Capabilities;

/// 视频源信息，供对话框展示。
#[derive(Debug, serde::Serialize)]
pub struct VideoInfo {
    /// 容器名（ffprobe 的原始多值串，如 `mov,mp4,m4a,...`）。
    pub container: String,
    /// 容器可读名。
    pub container_long: Option<String>,
    /// 时长（毫秒）。
    pub duration_ms: Option<u64>,
    /// 文件字节数。
    pub size: u64,
    /// 总码率。
    pub bit_rate: Option<u64>,
    /// 主视频轨编码，如 `h264`。
    pub video_codec: Option<String>,
    /// 宽。
    pub width: Option<u32>,
    /// 高。
    pub height: Option<u32>,
    /// 帧率字面值，如 `30000/1001`。
    pub frame_rate: Option<String>,
    /// profile。
    pub profile: Option<String>,
    /// 主音轨编码，如 `pcm_s16le`。
    pub audio_codec: Option<String>,
    /// 声道布局。
    pub audio_layout: Option<String>,
    /// 采样率。
    pub sample_rate: Option<u32>,
    /// 播放分级：`p1` / `p2` / `p3`。
    pub tier: String,
    /// 分级原因，**已是可读文本**。
    ///
    /// 这里破例转发后端拼好的字符串，与 `commands.rs` 顶部「后端不拼文案」
    /// 的约定不同。理由是这段文本由 `tier.rs` 按具体轨道内容动态生成
    /// （「DTS 音轨无法封入 MP4」之类），要改成翻译键就得把每种组合都
    /// 定义成一个键，而组合是开放的。这个例外只影响展示，不影响任何判断。
    pub tier_reason: Option<String>,
    /// 字幕轨数量。
    pub subtitle_count: usize,
    /// 转封装到 MP4 时音轨会怎样：`copy` / `to_aac` / `drop`。
    ///
    /// 这个判断放在后端算，而不是让前端按编码名猜：它依赖「容器能装下
    /// 哪些编码」与「有没有 AAC 编码器」两件事，前端重复实现一遍迟早
    /// 和后端不一致，而不一致的后果是界面说会保留、实际丢了音轨。
    pub mp4_audio_plan: String,
}

/// 把 `AudioPlan` 转成前端用的字符串。
const fn plan_name(p: AudioPlan) -> &'static str {
    match p {
        AudioPlan::Copy => "copy",
        AudioPlan::ToAac => "to_aac",
        AudioPlan::Drop => "drop",
    }
}

/// 查询这台机器上 FFmpeg 的实际能力。
///
/// `refresh` 为真时绕过缓存重新探测，供「我换了 FFmpeg，重新检测」使用。
///
/// # Errors
///
/// 不返回错误：探测失败时返回一个全为空的能力集，前端据此显示「不可用」
/// 即可。把「没装 FFmpeg」当成错误会逼前端写 try/catch 去处理一个
/// 完全正常的状态。
#[tauri::command]
pub async fn video_capabilities(refresh: bool) -> CmdResult<Caps> {
    let caps = tauri::async_runtime::spawn_blocking(move || {
        if refresh {
            omy_media::caps::refresh()
        } else {
            omy_media::capabilities().clone()
        }
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;
    Ok(caps)
}

/// 探测一个未加密的本地视频。
///
/// # Errors
///
/// - `unknown_file`：token 未登记或文件已不在
/// - `ffmpeg_unavailable`：没有 ffprobe
/// - `not_media`：不是媒体文件
/// - `probe_failed`：探测进程失败
#[tauri::command]
pub async fn video_info(state: tauri::State<'_, Shared>, token: String) -> CmdResult<VideoInfo> {
    let Some(path) = state.plain.resolve(&token) else {
        return Err(CmdError::code("unknown_file"));
    };
    if !path.is_file() {
        return Err(CmdError::code("unknown_file"));
    }

    tauri::async_runtime::spawn_blocking(move || {
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let info = omy_media::probe::probe(omy_media::probe::Source::Path(&path)).map_err(|e| {
            match e {
                omy_media::MediaError::FfmpegUnavailable { .. } => {
                    CmdError::code("ffmpeg_unavailable")
                }
                omy_media::MediaError::NotMedia { .. } => CmdError::code("not_media"),
                _ => CmdError::code("probe_failed"),
            }
        })?;

        let verdict = omy_media::tier::classify(&info);
        let v = info.primary_video();
        let a = info.audio.first();

        // 音轨计划按 MP4 算：那是转封装的默认目标，也是唯一会丢音轨的情况
        let caps = omy_media::capabilities();
        let plan = omy_media::convert::plan_audio(
            Container::Mp4,
            a.map(|x| x.codec.as_str()),
            caps.can_encode_aac(),
        );

        Ok(VideoInfo {
            container: info.container.clone(),
            container_long: info.container_long.clone(),
            duration_ms: info.duration_ms,
            size,
            bit_rate: info.bit_rate,
            video_codec: v.map(|x| x.codec.clone()),
            width: v.and_then(|x| x.width),
            height: v.and_then(|x| x.height),
            frame_rate: v.and_then(|x| x.frame_rate.clone()),
            profile: v.and_then(|x| x.profile.clone()),
            audio_codec: a.map(|x| x.codec.clone()),
            audio_layout: a.and_then(|x| x.channel_layout.clone()),
            sample_rate: a.and_then(|x| x.sample_rate),
            tier: format!("{:?}", verdict.tier).to_lowercase(),
            tier_reason: Some(verdict.reason.clone()),
            subtitle_count: info.subtitles.len(),
            mp4_audio_plan: plan_name(plan).to_owned(),
        })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 转换请求。
#[derive(Debug, serde::Deserialize)]
pub struct ConvertRequest {
    /// 源文件的 plain token。
    pub token: String,
    /// 目标容器：`mp4` / `mov` / `mkv`。
    pub container: String,
    /// 是否 faststart。
    pub faststart: bool,
    /// 重新编码参数。`None` 表示只转封装。
    pub encode: Option<EncodeRequest>,
}

/// 重新编码参数。
#[derive(Debug, serde::Deserialize)]
pub struct EncodeRequest {
    /// 编码器名，必须来自 `video_capabilities` 返回的列表。
    pub encoder: String,
    /// CRF。
    pub crf: u8,
    /// 目标高度，`None` 保持原样。
    pub height: Option<u32>,
    /// 目标帧率，`None` 保持原样。
    pub fps: Option<u32>,
}

/// 转换结果。
#[derive(Debug, serde::Serialize)]
pub struct ConvertResult {
    /// 产物路径。加密流程拿它当输入。
    pub path: String,
    /// 产物的 plain token。
    ///
    /// 必须回给前端：预览产物、以及之后删除它都要用 token 而不是路径。
    /// 只给路径的话前端就得把路径传回来，那正是 token 机制要避免的事。
    pub token: Option<String>,
    /// 产物字节数。
    pub size: u64,
    /// 源文件字节数，供界面显示「省了多少」。
    pub original_size: u64,
    /// FFmpeg 的警告。
    pub warnings: Vec<String>,
    /// 音轨实际怎么处理的。
    pub audio_plan: String,
}

/// 执行转换。
///
/// # Errors
///
/// - `unknown_file`：token 未登记
/// - `ffmpeg_unavailable` / `encoder_unavailable`：能力不足
/// - `bad_container`：容器名不认得
/// - `convert_failed`：FFmpeg 失败
#[tauri::command]
pub async fn convert_video(
    state: tauri::State<'_, Shared>,
    req: ConvertRequest,
) -> CmdResult<ConvertResult> {
    let Some(src) = state.plain.resolve(&req.token) else {
        return Err(CmdError::code("unknown_file"));
    };
    if !src.is_file() {
        return Err(CmdError::code("unknown_file"));
    }

    let container = match req.container.as_str() {
        "mp4" => Container::Mp4,
        "mov" => Container::Mov,
        "mkv" => Container::Mkv,
        _ => return Err(CmdError::code("bad_container")),
    };

    // 在真正开工前校验能力。
    //
    // 这一步不能省：前端的按钮状态是上一次探测的结果，用户可能在对话框
    // 开着的时候换掉了 FFmpeg。等 FFmpeg 自己报 "Unknown encoder" 的话，
    // 一是错误信息对用户没有意义，二是对大文件而言那已经是几分钟之后。
    let caps = omy_media::capabilities();
    if !caps.has_ffmpeg {
        return Err(CmdError::code("ffmpeg_unavailable"));
    }
    if let Some(e) = &req.encode {
        if !caps.video_encoders.iter().any(|x| x == &e.encoder) {
            return Err(CmdError::with(
                "encoder_unavailable",
                serde_json::json!({ "encoder": e.encoder }),
            ));
        }
    }

    let handle: Shared = std::sync::Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || {
        let original_size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);

        // 音轨计划要按**实际的**源音轨算，不能信前端传过来的
        let audio_codec = omy_media::probe::probe(omy_media::probe::Source::Path(&src))
            .ok()
            .and_then(|i| i.audio.first().map(|a| a.codec.clone()));
        let plan = omy_media::convert::plan_audio(
            container,
            audio_codec.as_deref(),
            caps.can_encode_aac(),
        );

        let out = temp_output(&src, container.extension());

        let opts = ConvertOptions {
            container,
            audio: plan,
            faststart: req.faststart,
            video_encode: req.encode.as_ref().map(|e| VideoEncode {
                encoder: e.encoder.clone(),
                crf: e.crf,
                height: e.height,
                fps: e.fps,
            }),
        };

        let r = omy_media::convert::convert(&src, &out, &opts)
            .map_err(|_| CmdError::code("convert_failed"))?;

        // 产物要登记进 token 表，否则前端无法预览它，加密流程也拿不到。
        // 不登记的话这个文件对前端等于不存在
        let token = handle.plain.register(&r.path);

        Ok(ConvertResult {
            path: r.path.to_string_lossy().into_owned(),
            token,
            size: r.size,
            original_size,
            warnings: r.warnings,
            audio_plan: plan_name(plan).to_owned(),
        })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 给转换产物起一个不会撞车的临时路径。
///
/// 放在源文件同目录（理由见模块注释）。加时间戳是因为用户可能对同一个
/// 文件试几种设置，固定名字会让第二次覆盖第一次——而第一次的产物此时
/// 可能正在被预览。
fn temp_output(src: &std::path::Path, ext: &str) -> PathBuf {
    let dir = src.parent().map_or_else(std::env::temp_dir, PathBuf::from);
    let stem = src
        .file_stem()
        .map_or_else(|| String::from("video"), |s| s.to_string_lossy().into_owned());
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    dir.join(format!(".omytmp-{stem}-{ts}.{ext}"))
}

/// 删除转换产生的中间文件。
///
/// 加密完成或用户取消后调用。只接受 token，且只删我们自己生成的
/// `.omytmp-` 文件——否则这就成了一个「删除任意文件」的接口。
///
/// # Errors
///
/// - `unknown_file`：token 未登记
/// - `not_temp_file`：不是我们生成的中间文件
#[tauri::command]
pub async fn discard_converted(
    state: tauri::State<'_, Shared>,
    token: String,
) -> CmdResult<()> {
    let Some(path) = state.plain.resolve(&token) else {
        return Err(CmdError::code("unknown_file"));
    };
    // 前缀校验是这个命令的全部安全性所在。
    //
    // 没有它，任何拿得到 token 的脚本都能删掉用户浏览过的任意文件——
    // 而 token 在浏览目录时就已经发给前端了
    let is_temp = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(".omytmp-"));
    if !is_temp {
        return Err(CmdError::code("not_temp_file"));
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时文件名必须带 `.omytmp-` 前缀，否则 `discard_converted` 拒绝删它。
    ///
    /// 不这样断言会怎样：两处对「什么是中间文件」的定义会悄悄分叉，
    /// 结果是转换产物删不掉，在用户的视频目录里越积越多。
    #[test]
    fn temp_output_is_recognizable_as_temp() {
        let p = temp_output(std::path::Path::new("C:/v/movie.mov"), "mp4");
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".omytmp-"), "前缀不对: {name}");
        assert!(name.ends_with(".mp4"), "扩展名不对: {name}");
        assert!(name.contains("movie"), "看不出源文件是谁: {name}");
        // 与源文件同目录，避免跨盘拷贝
        assert_eq!(p.parent(), Some(std::path::Path::new("C:/v")));
    }

    /// 同一个源连续转两次不能撞名。
    ///
    /// 不这样断言会怎样：用户试完 1080p 又试 720p，第二次会覆盖第一次，
    /// 而第一个产物此时可能正被预览播放着。
    #[test]
    fn repeated_conversions_do_not_collide() {
        let a = temp_output(std::path::Path::new("C:/v/movie.mov"), "mp4");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = temp_output(std::path::Path::new("C:/v/movie.mov"), "mp4");
        assert_ne!(a, b, "两次转换产生了同一个路径");
    }

    /// 计划名与枚举一一对应，前端按这几个字符串分支。
    #[test]
    fn plan_names_are_stable() {
        assert_eq!(plan_name(AudioPlan::Copy), "copy");
        assert_eq!(plan_name(AudioPlan::ToAac), "to_aac");
        assert_eq!(plan_name(AudioPlan::Drop), "drop");
    }
}
