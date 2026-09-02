//! `omy doctor`：环境自检。
//!
//! 报告本机能力，帮助用户理解为何某些功能不可用。
//! 只报告**实测**结果，不猜测——例如可用内存要真的查，
//! 而不是假定「一般机器都有 8 GB」。

use super::Ctx;
use crate::output::human_bytes;
use anyhow::Result;
use clap::Args as ClapArgs;
use omy_core::crypto::{Argon2Params, Kek};
use serde_json::json;
use std::time::Instant;

/// `doctor` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 同时做一次快速 KDF 实测
    #[arg(long, default_value_t = true)]
    pub probe_kdf: bool,
}

/// 一项检查的结果。
struct Check {
    ok: Option<bool>,
    name: String,
    detail: String,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { ok: Some(true), name: name.into(), detail: detail.into() }
    }
    fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { ok: Some(false), name: name.into(), detail: detail.into() }
    }
    fn note(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { ok: None, name: name.into(), detail: detail.into() }
    }
}

/// 媒体能力检查：FFmpeg / ffprobe 在不在，在的话报版本。
///
/// 必须真探测。缩略图与视频播放依赖外部 FFmpeg，装没装只有查了才知道，
/// 这正是用户跑 doctor 想问的问题。
fn media_check() -> Check {
    let probe = omy_media::ffprobe::has_ffprobe();
    let ff = omy_media::ffprobe::has_ffmpeg();
    match (probe, ff) {
        (true, true) => {
            // 报出版本号：用户排查「为什么这个视频抽不出帧」时，
            // 版本是第一个要问的东西
            let v = omy_media::ffprobe::version(omy_media::ffprobe::Tool::Ffprobe)
                .unwrap_or_else(|_| String::from("版本未知"));
            Check::pass("媒体预览（omy-media）", format!("ffprobe 与 ffmpeg 均可用（{v}）"))
        }
        // 分开报缺哪个：图片缩略图走纯 Rust 不需要 FFmpeg，
        // 只缺 ffmpeg 时视频抽帧不可用但探测仍可用，二者影响范围不同
        (true, false) => Check::warn(
            "媒体预览（omy-media）",
            "ffprobe 可用但缺 ffmpeg：可探测媒体信息，无法生成视频缩略图",
        ),
        (false, true) => Check::warn(
            "媒体预览（omy-media）",
            "ffmpeg 可用但缺 ffprobe：无法探测媒体信息与播放分级",
        ),
        (false, false) => Check::warn(
            "媒体预览（omy-media）",
            "未找到 ffprobe / ffmpeg：图片缩略图仍可用（纯 Rust），视频探测与抽帧不可用",
        ),
    }
}

/// 执行 `doctor`。
///
/// # Errors
///
/// KDF 实测失败时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let mut checks = Vec::new();

    // 平台
    checks.push(Check::note(
        "平台",
        format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
    ));

    // AES 硬件加速：直接影响该选哪个算法
    checks.push(aes_check());

    // 并行度
    let cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    checks.push(Check::note("CPU 并行度", format!("{cpus} 个逻辑核")));

    // 配置文件
    match crate::config::default_path() {
        Some(p) if p.exists() => {
            checks.push(Check::pass("配置文件", p.display().to_string()));
        }
        Some(p) => {
            checks.push(Check::note(
                "配置文件",
                format!("未创建（默认位置 {}）", p.display()),
            ));
        }
        None => checks.push(Check::warn("配置文件", "无法确定配置目录")),
    }

    // 语言
    checks.push(Check::note(
        "界面语言",
        format!(
            "{}（系统 locale: {}）",
            crate::i18n::current().tag(),
            sys_locale::get_locale().unwrap_or_else(|| String::from("未知"))
        ),
    ));

    // KDF 实测：这是唯一能判断各档位是否可用的方法
    if a.probe_kdf {
        let salt = [0x77u8; 16];
        for (label, params) in [
            ("mobile", Argon2Params::MOBILE),
            ("interactive", Argon2Params::INTERACTIVE),
        ] {
            let t = Instant::now();
            match Kek::from_password(b"doctor", &salt, params) {
                Ok(k) => {
                    std::hint::black_box(&k);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    checks.push(Check::pass(
                        format!("KDF {label}"),
                        format!(
                            "{ms:.0} ms（需 {}）",
                            human_bytes(u64::from(params.m_kib) * 1024)
                        ),
                    ));
                }
                Err(e) => {
                    checks.push(Check::warn(format!("KDF {label}"), e.to_string()));
                }
            }
        }
        // 高档位只报告内存需求，不实跑——sensitive 需要 1 GiB，
        // 在内存紧张的机器上实跑可能触发 OOM 杀死进程
        checks.push(Check::note(
            "KDF moderate / sensitive",
            format!(
                "未实测，分别需要 {} 与 {} 内存",
                human_bytes(u64::from(Argon2Params::MODERATE.m_kib) * 1024),
                human_bytes(u64::from(Argon2Params::SENSITIVE.m_kib) * 1024)
            ),
        ));
    }

    // 媒体能力：真去查 FFmpeg 在不在，而不是写死结论。
    //
    // 这里曾经硬编码「未实现」，在 omy-media 做好之后就成了假消息。
    // doctor 报假消息比没有 doctor 更糟：用户会放着可用的功能不用，
    // 或者跑去排查一个不存在的问题
    checks.push(media_check());

    // 局域网共享：能力已接入，这里只报告它依赖什么
    checks.push(Check::pass(
        "局域网共享（omy-net）",
        "可用：mDNS 发现 + SPAKE2 配对 + Noise IK 传输（omy share）",
    ));

    // 格式层面的已知偏差，不是本机环境问题，所以用 note 而不是 warn——
    // 用 ⚠ 会让用户以为自己机器缺了什么东西
    checks.push(Check::note(
        "XChaCha20-Poly1305",
        "未实现：--cipher xchacha20 实际使用 ChaCha20-Poly1305（12 字节 nonce）",
    ));

    // 渲染
    let mut rows = Vec::new();
    for c in &checks {
        let mark = match c.ok {
            Some(true) => "✓",
            Some(false) => "⚠",
            None => "·",
        };
        ctx.out.line(&format!("{mark} {:<26} {}", c.name, c.detail));
        rows.push(json!({
            "name": c.name,
            "status": match c.ok {
                Some(true) => "ok",
                Some(false) => "warn",
                None => "info",
            },
            "detail": c.detail,
        }));
    }

    if ctx.out.is_json() {
        ctx.out.result("", &json!({ "checks": rows }));
    }
    Ok(())
}

/// 检测 AES 硬件加速。
///
/// 这不是猜测：用编译期与运行期的 CPU 特性检测。
#[cfg(target_arch = "x86_64")]
fn aes_check() -> Check {
    if std::arch::is_x86_feature_detected!("aes") {
        Check::pass("AES 硬件加速", "AES-NI 可用，--cipher aes256gcm 会更快")
    } else {
        Check::note(
            "AES 硬件加速",
            "不可用，建议用默认的 chacha20（软件实现下更快）",
        )
    }
}

/// aarch64 上的 AES 扩展检测。
#[cfg(target_arch = "aarch64")]
fn aes_check() -> Check {
    // std 的 aarch64 特性检测在部分平台上不稳定，此处保守报告
    Check::note(
        "AES 硬件加速",
        "aarch64：多数现代芯片支持 AES 扩展，实际差异请用 omy bench 实测",
    )
}

/// 其它架构。
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn aes_check() -> Check {
    Check::note("AES 硬件加速", "当前架构未做检测，请用 omy bench 实测")
}
