//! `omy info`：查看文件信息。
//!
//! 不提供密码时显示的都是格式上本就明文的字段（见设计文档 02 号）。
//! 提供密码后额外显示原文件名等加密信息。
//!
//! # slot 数量恒为「未知」
//!
//! slot 区始终填满 8 个（真实 + 随机填充），实测 1 密码与 3 密码的文件
//! 字节分布无法区分。这是可否认性（决策 D-02）的基础，
//! **CLI 不得提供任何泄露 slot 数量的输出**。

use super::Ctx;
use crate::i18n::t;
use crate::output::{human_bytes, human_duration, thousands};
use crate::password::{PasswordSource, read_password};
use anyhow::{Context, Result};
use clap::Args as ClapArgs;
use omy_core::header::flags;
use serde_json::json;
use std::path::PathBuf;

/// `info` 的参数。
#[derive(Debug, ClapArgs)]
pub struct Args {
    /// 目标文件
    pub file: PathBuf,

    /// 提供密码以查看加密的元信息（原文件名等）
    #[arg(long)]
    pub with_password: bool,

    /// 从环境变量读取密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,

    /// 把缩略图导出到指定路径（需要密码）
    ///
    /// 用于确认自选的取帧时间点是否如预期。缩略图格式为 WebP。
    #[arg(long, value_name = "PATH")]
    pub extract_thumbnail: Option<PathBuf>,
}

/// 执行 `info`。
///
/// # Errors
///
/// 文件读取失败、非 omy 格式、或提供了密码但无匹配 slot 时返回错误。
pub fn run(ctx: &Ctx<'_>, a: &Args) -> Result<()> {
    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;

    let h = omy_core::file::peek_header(&data)?;

    let cipher = match h.cipher_id {
        omy_core::CipherId::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        omy_core::CipherId::Aes256Gcm => "AES-256-GCM",
    };
    let n_chunks = h.n_chunks();
    let ct_size = omy_core::payload::payload_ct_size(&h);

    // 分片探测：同目录下找 .omy.NNN
    let shard_info = probe_shards(&a.file);

    // 可选的密码解锁部分
    let mut filename: Option<String> = None;
    let mut unlocked = false;
    // 媒体信息只有解锁后才能读——它存在加密的 TLV 里
    let mut media: Option<omy_media::MediaMeta> = None;
    let mut has_moov = false;
    // 导出的缩略图字节数，用于 JSON 输出里回报结果
    let mut thumb_written: Option<(PathBuf, usize)> = None;
    if a.with_password
        || a.password_env.is_some()
        || a.password_file.is_some()
        || a.password_stdin
    {
        let src = PasswordSource {
            env: a.password_env.clone(),
            file: a.password_file.clone(),
            stdin: a.password_stdin,
        };
        let pw = read_password(&src, t("prompt.password"), false)?;
        let opened = omy_core::file::open_with_password(&data, &pw)?;
        unlocked = true;
        filename = opened.filename().ok();
        has_moov = opened.has_moov_cache();
        // 元信息读不出来不是错误：非媒体文件本来就没有，
        // 旧版本写的也可能解析不了。这是非 CRITICAL TLV，降级即可。
        media = opened
            .media_meta()
            .ok()
            .and_then(|b| omy_media::MediaMeta::from_json_bytes(&b).ok());

        // 导出缩略图。
        //
        // 这个开关存在的理由是「自选缩略图」需要能被验证：光看
        // has_thumbnail=true 无法判断取的是哪一帧——取帧时间点被忽略时
        // 照样有缩略图。把字节导出来才能确认选对了。
        if let Some(dest) = &a.extract_thumbnail {
            let bytes = opened.thumbnail().with_context(|| {
                format!("{} 没有缩略图，或解密失败", a.file.display())
            })?;
            std::fs::write(dest, &bytes)
                .with_context(|| format!("写入 {} 失败", dest.display()))?;
            thumb_written = Some((dest.clone(), bytes.len()));
        }
    } else if a.extract_thumbnail.is_some() {
        // 缩略图存在加密 TLV 里，没有密码根本读不出来。
        // 静默忽略会让用户以为导出成功了却找不到文件
        anyhow::bail!("导出缩略图需要密码，请加 --with-password 或 --password-env 等");
    }

    let human = render_human(
        &a.file,
        &h,
        cipher,
        n_chunks,
        ct_size,
        shard_info.as_deref(),
        filename.as_deref(),
        unlocked,
        media.as_ref(),
        has_moov,
    );

    let value = json!({
        "format": "OMYFILE",
        "version": format!("{}.{}", h.version_major, h.version_minor),
        "uuid": hex_uuid(&h.file_uuid),
        "header_len": h.header_len,
        "cipher": cipher_json_name(h.cipher_id),
        "kdf": {
            "algorithm": "argon2id",
            "m_kib": h.argon2_m_kib,
            "t": h.argon2_t,
            "p": h.argon2_p,
        },
        "chunk_size": h.chunk_size,
        "chunk_count": n_chunks,
        "compression": if h.has_flag(flags::COMPRESSED) {
            json!({ "algorithm": "zstd" })
        } else {
            json!(null)
        },
        "ciphertext_size": ct_size,
        "plaintext_size": h.plaintext_size,
        "has_thumbnail": h.has_flag(flags::HAS_THUMBNAIL),
        "filename_encrypted": h.has_flag(flags::FILENAME_ENCRYPTED),
        "extension_preserved": h.has_flag(flags::EXT_PRESERVED),
        "is_container": h.has_flag(flags::CONTAINER),
        "is_sharded": h.has_flag(flags::SHARDED),
        "is_transcoded": h.has_flag(flags::TRANSCODED),
        // 恒为 null：slot 数量在设计上不可探测
        "slot_count": serde_json::Value::Null,
        "filename": filename,
        // 未解锁时为 null，而不是 false——
        // 「不知道」与「没有」是两件事，脚本要能区分
        "has_moov_cache": if unlocked { json!(has_moov) } else { json!(null) },
        "media": media.as_ref().map_or(json!(null), |m| {
            json!({
                "container": m.container,
                "duration_ms": m.duration_ms,
                "bit_rate": m.bit_rate,
                "playback_tier": m.playback_tier.default,
                "tier_reason": m.playback_tier.reason,
                "video": m.video.as_ref().map(|v| json!({
                    "codec": v.codec,
                    "width": v.width,
                    "height": v.height,
                })),
                "audio_tracks": m.audio.len(),
                "subtitle_tracks": m.subtitles.len(),
            })
        }),
        // 导出结果：未导出时为 null，脚本据此判断有没有做这件事
        "thumbnail_extracted": thumb_written.as_ref().map_or(json!(null), |(p, n)| {
            json!({ "path": p.display().to_string(), "bytes": n })
        }),
    });

    ctx.out.result(&human, &value);
    Ok(())
}

/// 渲染人类可读输出。
#[allow(clippy::too_many_arguments)]
fn render_human(
    path: &std::path::Path,
    h: &omy_core::FixedHeader,
    cipher: &str,
    n_chunks: u64,
    ct_size: u64,
    shards: Option<&str>,
    filename: Option<&str>,
    unlocked: bool,
    media: Option<&omy_media::MediaMeta>,
    has_moov: bool,
) -> String {
    let mut s = String::new();
    let w = 14usize;

    let mut row = |k: &str, v: &str| {
        s.push_str(&format!("{k:<w$}{v}\n", w = w));
    };

    row(t("info.file"), &path.display().to_string());
    row(
        t("info.format"),
        &format!("OMYFILE v{}.{}", h.version_major, h.version_minor),
    );
    row(t("info.uuid"), &hex_uuid(&h.file_uuid));
    row(
        t("info.header_len"),
        &format!("{} B", thousands(u64::from(h.header_len))),
    );
    row(t("info.cipher"), cipher);
    row(
        t("info.kdf"),
        &format!(
            "Argon2id  m={} t={} p={}",
            human_bytes(u64::from(h.argon2_m_kib) * 1024),
            h.argon2_t,
            h.argon2_p
        ),
    );
    row(t("info.chunk_size"), &human_bytes(u64::from(h.chunk_size)));
    row(t("info.chunk_count"), &thousands(n_chunks));
    row(
        t("info.compression"),
        if h.has_flag(flags::COMPRESSED) {
            "zstd"
        } else {
            t("info.none")
        },
    );
    row(
        t("info.ciphertext_size"),
        &format!("{} ({} B)", human_bytes(ct_size), thousands(ct_size)),
    );
    row(
        t("info.plaintext_size"),
        &format!(
            "{} ({} B)",
            human_bytes(h.plaintext_size),
            thousands(h.plaintext_size)
        ),
    );
    row(
        t("info.thumbnail"),
        if h.has_flag(flags::HAS_THUMBNAIL) {
            t("info.yes")
        } else {
            t("info.no")
        },
    );
    row(
        t("info.filename"),
        match filename {
            Some(n) => n,
            None if h.has_flag(flags::FILENAME_ENCRYPTED) => t("info.filename_encrypted"),
            None => t("info.filename_plain"),
        },
    );
    if h.has_flag(flags::CONTAINER) {
        row(t("info.container"), t("info.yes"));
    }
    if let Some(sh) = shards {
        row(t("info.sharded"), sh);
    }
    // 关键：恒为「未知」，不泄露 slot 数量
    row(t("info.slots"), t("info.slots_unknown"));

    // 媒体信息：只有解锁后才有，因为它存在加密 TLV 里
    if let Some(m) = media {
        row(t("info.media_container"), &m.container);
        if let Some(ms) = m.duration_ms {
            row(t("info.media_duration"), &human_duration(ms));
        }
        if let Some(v) = &m.video {
            let res = match (v.width, v.height) {
                (Some(w2), Some(h2)) => format!("{} {w2}×{h2}", v.codec),
                _ => v.codec.clone(),
            };
            row(t("info.media_video"), &res);
        }
        if !m.audio.is_empty() {
            // 列出编码与语言，这是选轨的依据
            let list: Vec<String> = m
                .audio
                .iter()
                .map(|a| match (&a.lang, a.channels) {
                    (Some(l), Some(c)) => format!("{} {l} {c}ch", a.codec),
                    (Some(l), None) => format!("{} {l}", a.codec),
                    (None, Some(c)) => format!("{} {c}ch", a.codec),
                    (None, None) => a.codec.clone(),
                })
                .collect();
            row(t("info.media_audio"), &list.join(", "));
        }
        if !m.subtitles.is_empty() {
            let list: Vec<String> = m
                .subtitles
                .iter()
                .map(|x| match &x.lang {
                    Some(l) => format!("{} {l}", x.codec),
                    None => x.codec.clone(),
                })
                .collect();
            row(t("info.media_subtitles"), &list.join(", "));
        }
        // 分级与理由一起显示：只给代号用户看不懂
        row(
            t("info.media_tier"),
            &format!("{} — {}", m.playback_tier.default, m.playback_tier.reason),
        );
        for alt in &m.playback_tier.alternatives {
            row("", &format!("↳ {} {}", alt.tier, alt.note));
        }
    }
    if unlocked && has_moov {
        row(t("info.moov_cache"), t("info.yes"));
    }

    if !unlocked {
        s.push('\n');
        s.push_str(t("info.hint_password"));
        s.push('\n');
    }
    s
}

fn cipher_json_name(c: omy_core::CipherId) -> &'static str {
    match c {
        omy_core::CipherId::ChaCha20Poly1305 => "chacha20-poly1305",
        omy_core::CipherId::Aes256Gcm => "aes-256-gcm",
    }
}

/// UUID 的标准连字符形式。
fn hex_uuid(u: &[u8; 16]) -> String {
    let h: Vec<String> = u.iter().map(|b| format!("{b:02x}")).collect();
    let j = h.join("");
    // 8-4-4-4-12
    match (j.get(..8), j.get(8..12), j.get(12..16), j.get(16..20), j.get(20..)) {
        (Some(a), Some(b), Some(c), Some(d), Some(e)) => format!("{a}-{b}-{c}-{d}-{e}"),
        _ => j,
    }
}

/// 探测同目录的分片，返回如 `3 片（找到 3/3）`。
fn probe_shards(main: &std::path::Path) -> Option<String> {
    let dir = main.parent()?;
    let stem = main.file_name()?.to_string_lossy().to_string();
    let mut found = 0usize;
    for i in 0..1000u32 {
        let p = dir.join(format!("{stem}.{i:03}"));
        if p.exists() {
            found += 1;
        } else if found > 0 {
            break;
        }
    }
    if found == 0 {
        None
    } else {
        Some(format!("{found} 片"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_formatting() {
        let u = [
            0x7f, 0x3a, 0x2b, 0x91, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa,
            0xbb, 0xcc,
        ];
        assert_eq!(hex_uuid(&u), "7f3a2b91-1122-3344-5566-778899aabbcc");
    }

    #[test]
    fn uuid_all_zero() {
        assert_eq!(
            hex_uuid(&[0u8; 16]),
            "00000000-0000-0000-0000-000000000000"
        );
    }
}
