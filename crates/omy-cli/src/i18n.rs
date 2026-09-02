//! 本地化。对应决策 D-29，首期简体中文 + 英文。
//!
//! # 语言检测顺序
//!
//! 与设计文档 [09 号 §8](../../docs/research/09-cli-design.md) 一致：
//!
//! ```text
//! OMY_LANG 环境变量
//!   → 配置文件 [ui].language
//!   → 系统 locale
//!   → 英语兜底
//! ```
//!
//! # 为什么 JSON 的 code 不翻译
//!
//! `--json` 输出里 `code` 恒为英文常量（如 `WRONG_PASSWORD`），只有 `message`
//! 会翻译。脚本必须匹配 `code`——若翻译了 code，用户换个语言脚本就崩。

use std::sync::OnceLock;

/// 支持的界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 简体中文。
    ZhCn,
    /// 英语，兜底语言。
    En,
}

impl Lang {
    /// BCP 47 风格标签。
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::En => "en",
        }
    }

    /// 从 locale 字符串识别。
    ///
    /// 只要以 `zh` 开头就归为简体中文——首期不区分繁简，
    /// 繁体用户看到简体仍可用，比退回英文更好。
    #[must_use]
    pub fn from_locale(s: &str) -> Option<Self> {
        let low = s.to_ascii_lowercase();
        if low.starts_with("zh") {
            Some(Self::ZhCn)
        } else if low.starts_with("en") {
            Some(Self::En)
        } else {
            None
        }
    }
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// 初始化语言。重复调用无效果，首次调用决定全局语言。
///
/// `configured` 来自配置文件，可为 `None`。
pub fn init(configured: Option<&str>) {
    let lang = detect(configured);
    let _ = LANG.set(lang);
}

/// 当前语言。未初始化时按系统 locale 惰性判断。
#[must_use]
pub fn current() -> Lang {
    *LANG.get_or_init(|| detect(None))
}

/// 按优先级检测语言。
fn detect(configured: Option<&str>) -> Lang {
    // 1. OMY_LANG 优先级最高，便于临时覆盖与测试
    if let Ok(v) = std::env::var("OMY_LANG") {
        if let Some(l) = Lang::from_locale(&v) {
            return l;
        }
    }
    // 2. 配置文件；"auto" 表示继续往下探测
    if let Some(c) = configured {
        if !c.eq_ignore_ascii_case("auto") {
            if let Some(l) = Lang::from_locale(c) {
                return l;
            }
        }
    }
    // 3. 系统 locale
    if let Some(sys) = sys_locale::get_locale() {
        if let Some(l) = Lang::from_locale(&sys) {
            return l;
        }
    }
    // 4. 英语兜底
    Lang::En
}

/// 取一条文案。
///
/// 用宏生成的查表函数，缺失的 key 返回 key 本身而非 panic——
/// 少一条翻译不该让程序崩溃。
#[must_use]
pub fn t(key: &str) -> &'static str {
    let lang = current();
    lookup(lang, key).unwrap_or_else(|| leak_fallback(key))
}

/// 缺失文案时的兜底：返回 key 本身。
///
/// 这些字符串只在开发期出现（漏翻译），数量有限，泄漏可接受。
fn leak_fallback(key: &str) -> &'static str {
    // 避免无限泄漏：只对已知的少量 key 泄漏一次
    static MISSING: OnceLock<std::sync::Mutex<Vec<&'static str>>> = OnceLock::new();
    let store = MISSING.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    if let Ok(mut v) = store.lock() {
        if let Some(found) = v.iter().find(|s| **s == key) {
            return found;
        }
        let leaked: &'static str = Box::leak(key.to_owned().into_boxed_str());
        v.push(leaked);
        return leaked;
    }
    "<missing>"
}

/// 文案表。
///
/// 用一个宏同时生成两种语言的查表，避免两边 key 不同步——
/// 手写两个 match 很容易漏掉一边。
macro_rules! catalog {
    ($($key:literal => { zh: $zh:literal, en: $en:literal $(,)? }),* $(,)?) => {
        fn lookup(lang: Lang, key: &str) -> Option<&'static str> {
            match (lang, key) {
                $(
                    (Lang::ZhCn, $key) => Some($zh),
                    (Lang::En, $key) => Some($en),
                )*
                _ => None,
            }
        }

        /// 全部 key，供测试检查覆盖完整性。
        #[cfg(test)]
        const ALL_KEYS: &[&str] = &[$($key),*];
    };
}

catalog! {
    // ---- 通用 ----
    "app.about" => {
        zh: "omy 加密工具：文件加解密、扫描、分片与密钥管理",
        en: "omy encryption tool: encrypt, decrypt, scan, shard and manage keys",
    },
    "prompt.password" => { zh: "请输入密码", en: "Password" },
    "prompt.new_password" => { zh: "请输入新密码", en: "New password" },

    // ---- info 命令的字段名 ----
    "info.file" => { zh: "文件", en: "File" },
    "info.format" => { zh: "格式", en: "Format" },
    "info.uuid" => { zh: "UUID", en: "UUID" },
    "info.header_len" => { zh: "文件头长度", en: "Header length" },
    "info.cipher" => { zh: "加密算法", en: "Cipher" },
    "info.kdf" => { zh: "KDF", en: "KDF" },
    "info.chunk_size" => { zh: "分块大小", en: "Chunk size" },
    "info.chunk_count" => { zh: "块数量", en: "Chunk count" },
    "info.compression" => { zh: "压缩", en: "Compression" },
    "info.ciphertext_size" => { zh: "密文大小", en: "Ciphertext size" },
    "info.plaintext_size" => { zh: "明文大小", en: "Plaintext size" },
    "info.slots" => { zh: "Slot 占用", en: "Slots" },
    "info.slots_unknown" => {
        zh: "未知（设计上不可探测）",
        en: "unknown (undetectable by design)",
    },
    "info.thumbnail" => { zh: "缩略图", en: "Thumbnail" },
    "info.filename" => { zh: "文件名", en: "Filename" },
    "info.filename_encrypted" => { zh: "已加密", en: "encrypted" },
    "info.filename_plain" => { zh: "明文", en: "plaintext" },
    "info.sharded" => { zh: "分片", en: "Sharded" },
    "info.container" => { zh: "容器", en: "Container" },
    "info.hint_password" => {
        zh: "提示：提供密码可查看原文件名与媒体信息。",
        en: "Hint: provide a password to reveal the original filename and media info.",
    },
    "info.none" => { zh: "无", en: "none" },
    "info.yes" => { zh: "有", en: "yes" },
    "info.no" => { zh: "无", en: "no" },

    // ---- 媒体信息（解锁后才可见）----
    "info.media_container" => { zh: "媒体容器", en: "Media container" },
    "info.media_duration" => { zh: "时长", en: "Duration" },
    "info.media_video" => { zh: "视频", en: "Video" },
    "info.media_audio" => { zh: "音频", en: "Audio" },
    "info.media_subtitles" => { zh: "字幕", en: "Subtitles" },
    "info.media_tier" => { zh: "播放方式", en: "Playback" },
    "info.moov_cache" => { zh: "moov 缓存", en: "moov cache" },

    // ---- 操作结果 ----
    "ok.encrypted" => { zh: "已加密", en: "Encrypted" },
    "ok.decrypted" => { zh: "已解密", en: "Decrypted" },
    "ok.verified" => { zh: "校验通过", en: "Verified" },
    "ok.no_files" => { zh: "没有匹配的文件", en: "No matching files" },

    // ---- 扫描 ----
    "scan.examined" => { zh: "检查文件", en: "Files examined" },
    "scan.found" => { zh: "找到 omy 文件", en: "omy files found" },
    "scan.unlocked" => { zh: "成功解锁", en: "Unlocked" },
    "scan.locked" => { zh: "未匹配", en: "Locked" },
    "scan.elapsed" => { zh: "用时", en: "Elapsed" },
    "scan.memory_only" => {
        zh: "扫描结果仅在内存中，进程退出即消失，不落盘任何索引。",
        en: "Scan results live in memory only and vanish on exit; no index is written to disk.",
    },

    // ---- 危险操作确认 ----
    "warn.remove_slot" => {
        zh: "移除 slot 只修改当前文件。若该文件曾被复制、备份或同步到其它位置，\n那些副本仍可用被移除的密码打开。",
        en: "Removing a slot only affects this file. Copies made earlier can still be\nopened with the removed password.",
    },
    "warn.transcode" => {
        zh: "转码会改变文件内容：加密后保存的是转码产物，解密无法还原为原始文件。\n原始文件的哈希会记录在文件头中，但仅供核对。",
        en: "Transcoding changes the content: the encrypted result is the transcoded\nstream and cannot be restored to the original. The original hash is recorded\nin the header for reference only.",
    },
    "warn.delete_original" => {
        zh: "即将删除原始文件。请先确认加密文件可正常解密。",
        en: "The original file will be deleted. Verify the encrypted file first.",
    },
    // 与永久删除分开：回收站可还原，用同一句话会让用户高估风险，
    // 反过来把删除说成「可还原」则是更危险的误导
    "warn.trash_original" => {
        zh: "即将把原始文件移到回收站，之后仍可从回收站还原。",
        en: "The original file will be moved to the recycle bin and can be restored from there.",
    },
    "prompt.confirm" => { zh: "确认继续？[y/N] ", en: "Continue? [y/N] " },
    "msg.cancelled" => { zh: "已取消", en: "Cancelled" },

    // ---- 错误提示补充 ----
    "err.wrong_password" => {
        zh: "没有匹配的密码。请确认密码，或该文件是否用其它密码加密。",
        en: "No matching password. Check the password, or the file may use a different one.",
    },
    "err.corrupted" => {
        zh: "文件损坏或已被篡改（认证失败）。",
        en: "File is corrupted or has been tampered with (authentication failed).",
    },
    "err.missing_shards" => {
        zh: "缺少分片。用 --ignore-missing-shards 可输出可用部分。",
        en: "Missing shards. Use --ignore-missing-shards to output available parts.",
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_detection() {
        assert_eq!(Lang::from_locale("zh-CN"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_locale("zh_TW.UTF-8"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_locale("ZH"), Some(Lang::ZhCn));
        assert_eq!(Lang::from_locale("en-US"), Some(Lang::En));
        assert_eq!(Lang::from_locale("fr-FR"), None);
        assert_eq!(Lang::from_locale(""), None);
    }

    #[test]
    fn every_key_exists_in_both_languages() {
        // 这是加 catalog! 宏的目的：保证两种语言不会漏项
        for k in ALL_KEYS {
            assert!(
                lookup(Lang::ZhCn, k).is_some(),
                "简中缺少文案: {k}"
            );
            assert!(lookup(Lang::En, k).is_some(), "英文缺少文案: {k}");
        }
        assert!(ALL_KEYS.len() > 30, "文案数量异常少");
    }

    #[test]
    fn missing_key_returns_key_not_panic() {
        assert!(lookup(Lang::En, "no.such.key").is_none());
        // t() 对缺失 key 返回 key 本身
        let v = t("definitely.missing.key");
        assert_eq!(v, "definitely.missing.key");
        // 再次调用返回同一指针，不重复泄漏
        let v2 = t("definitely.missing.key");
        assert!(std::ptr::eq(v, v2));
    }

    #[test]
    fn tags_are_stable() {
        assert_eq!(Lang::ZhCn.tag(), "zh-CN");
        assert_eq!(Lang::En.tag(), "en");
    }

    #[test]
    fn configured_auto_falls_through() {
        // "auto" 不应被当作语言标签匹配失败而直接兜底英文，
        // 而是继续走系统 locale 检测
        let l = detect(Some("auto"));
        assert!(matches!(l, Lang::ZhCn | Lang::En));
        // 明确配置则生效
        assert_eq!(detect(Some("zh-CN")), Lang::ZhCn);
        assert_eq!(detect(Some("en")), Lang::En);
    }
}
