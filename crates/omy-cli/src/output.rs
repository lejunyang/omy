//! 输出层：统一处理人类可读输出、JSON 输出与退出码。
//!
//! # 为什么单独一层
//!
//! 每个子命令都要支持 `--json`、`-q`、`--no-color`。若各自处理，很快就会
//! 出现「某个命令忘了支持 --json」或「JSON 模式下仍打印了进度文字污染 stdout」
//! 这类问题。集中在此处，子命令只调用语义化方法。
//!
//! # stdout 与 stderr 的分工
//!
//! - **stdout**：仅结果数据（JSON、`cat` 的明文、表格）
//! - **stderr**：进度、警告、提示
//!
//! 这样 `omy cat x.omy | mpv -` 与 `omy info --json x.omy | jq` 都不会被
//! 提示文字污染。

use omy_core::error::ExitCode;
use serde_json::{Value, json};
use std::io::{IsTerminal, Write};

/// 输出模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// 人类可读。
    Human,
    /// JSON，供脚本消费。
    Json,
}

/// 输出上下文。
#[derive(Debug, Clone)]
pub struct Out {
    format: Format,
    quiet: bool,
    verbose: u8,
    color: bool,
}

impl Out {
    /// 构造输出上下文。
    ///
    /// `no_color` 为真，或 stdout 非终端，或设置了 `NO_COLOR` 环境变量时禁用彩色。
    /// 遵循 <https://no-color.org/> 约定。
    #[must_use]
    pub fn new(format: Format, quiet: bool, verbose: u8, no_color: bool) -> Self {
        let color = !no_color
            && std::env::var_os("NO_COLOR").is_none()
            && std::io::stdout().is_terminal();
        Self { format, quiet, verbose, color }
    }

    /// 是否 JSON 模式。
    #[must_use]
    pub const fn is_json(&self) -> bool {
        matches!(self.format, Format::Json)
    }

    /// 详细级别。
    #[must_use]
    pub const fn verbose(&self) -> u8 {
        self.verbose
    }

    /// 输出结果数据到 stdout。
    ///
    /// JSON 模式下打印 `value`；人类模式下打印 `human`。
    pub fn result(&self, human: &str, value: &Value) {
        if self.is_json() {
            match serde_json::to_string_pretty(value) {
                Ok(s) => println!("{s}"),
                // 序列化自身失败属于程序缺陷，但不该 panic
                Err(e) => eprintln!("内部错误：JSON 序列化失败: {e}"),
            }
        } else if !human.is_empty() {
            println!("{human}");
        }
    }

    /// 输出一行**纯人类可读**文本到 stdout，JSON 模式下静默。
    ///
    /// 与 `result` 的区别：`result` 在 JSON 模式下会打印它的 `value`，
    /// 适合「一个命令产出一份结构化结果」。而逐行列表（doctor 的每项
    /// 检查、list 的每个文件…）在 JSON 模式下应当**只**由末尾那次
    /// `result` 汇总输出，中间每行不能各自打印。
    ///
    /// 曾经这些地方调 `result(text, &json!(null))`，于是 JSON 模式下
    /// 每行吐一个 `null`，最后才是真正的对象——整体不是合法 JSON。
    /// 症状很隐蔽：人类模式完全正常，只有写脚本的人会撞上。
    pub fn line(&self, human: &str) {
        if !self.is_json() && !human.is_empty() {
            println!("{human}");
        }
    }

    /// 是否应显示进度反馈。
    ///
    /// 单独给一个方法而不是暴露 `quiet` 字段：进度的开关条件
    /// （非 JSON 且非 quiet）与 `info` 完全一致，各调用点自己拼这个
    /// 判断迟早会漏掉一个，导致 JSON 输出被进度条污染。
    #[must_use]
    pub const fn wants_progress(&self) -> bool {
        !self.quiet && !self.is_json()
    }

    /// 提示信息，走 stderr。JSON 模式与 quiet 下静默。
    pub fn info(&self, msg: &str) {
        if !self.quiet && !self.is_json() {
            eprintln!("{msg}");
        }
    }

    /// 仅在 `-v` 时输出。
    pub fn detail(&self, msg: &str) {
        if self.verbose > 0 && !self.quiet && !self.is_json() {
            eprintln!("{msg}");
        }
    }

    /// 仅在 `-vv` 时输出，用于逐项的细粒度进展。
    ///
    /// 与 `detail` 分级的意义：批量处理上千个文件时，
    /// `-v` 给阶段性摘要，`-vv` 给逐个文件的明细。
    /// 两者混在一个级别会让 `-v` 在大批量下不可读。
    pub fn trace(&self, msg: &str) {
        if self.verbose >= 2 && !self.quiet && !self.is_json() {
            eprintln!("{msg}");
        }
    }

    /// 警告。即便 quiet 也输出——用户需要知道风险。
    pub fn warn(&self, msg: &str) {
        if self.is_json() {
            return;
        }
        if self.color {
            eprintln!("\x1b[33m警告\x1b[0m: {msg}");
        } else {
            eprintln!("警告: {msg}");
        }
    }

    /// 成功提示。
    pub fn success(&self, msg: &str) {
        if self.quiet || self.is_json() {
            return;
        }
        if self.color {
            eprintln!("\x1b[32m✓\x1b[0m {msg}");
        } else {
            eprintln!("✓ {msg}");
        }
    }

    /// 交互式确认。
    ///
    /// `assume_yes` 为真时直接返回 `true`（对应 `--yes`）。
    /// 非终端且未指定 `--yes` 时返回 `false`——脚本里不应静默执行危险操作。
    #[must_use]
    pub fn confirm(&self, prompt: &str, assume_yes: bool) -> bool {
        if assume_yes {
            return true;
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("非交互环境中拒绝执行危险操作。确认无误可加 --yes。");
            return false;
        }
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        let a = line.trim().to_ascii_lowercase();
        a == "y" || a == "yes"
    }
}

/// 把错误渲染为输出并返回退出码。
///
/// JSON 模式输出 `{"error": {"code", "exit_code", "message"}}`；
/// `code` 恒为英文常量，供脚本匹配（见 i18n 模块说明）。
#[must_use]
pub fn report_error(out: &Out, err: &anyhow::Error) -> i32 {
    // 遍历整个错误链找 core 的结构化错误。
    //
    // 只查顶层是不够的：`.context()` 会把原错误包在下层，
    // 而退出码契约（密码错=3、损坏=4、缺片=6）是脚本判断的依据，
    // 一旦退化成通用的 1，调用方就无法区分「密码错」与「文件坏」。
    let found = err
        .chain()
        .find_map(|e| e.downcast_ref::<omy_core::Error>());
    let (code, exit) = match found {
        Some(e) => (e.code(), e.exit_code()),
        None => ("GENERAL_ERROR", ExitCode::General),
    };
    let exit_i32 = exit as i32;

    if out.is_json() {
        let v = json!({
            "error": {
                "code": code,
                "exit_code": exit_i32,
                "message": err.to_string(),
            }
        });
        match serde_json::to_string_pretty(&v) {
            Ok(s) => println!("{s}"),
            Err(_) => println!("{{\"error\":{{\"code\":\"{code}\",\"exit_code\":{exit_i32}}}}}"),
        }
    } else {
        eprintln!("错误: {err}");
        // 错误链默认只显示第一层原因：完整链条对普通用户是噪音。
        // -v 起显示全链，便于定位根因。
        let mut src = err.source();
        let mut depth = 0usize;
        while let Some(s) = src {
            if out.verbose() == 0 && depth >= 1 {
                eprintln!("  （更多原因请加 -v 查看）");
                break;
            }
            eprintln!("  原因: {s}");
            src = s.source();
            depth += 1;
        }
        // 针对常见错误补充可操作的提示
        if let Some(hint) = hint_for(code) {
            eprintln!("\n{hint}");
        }
    }
    exit_i32
}

/// 常见错误的补充提示。
fn hint_for(code: &str) -> Option<&'static str> {
    match code {
        "WRONG_PASSWORD" => Some(crate::i18n::t("err.wrong_password")),
        "CHUNK_AUTH_FAILED" | "HEADER_MAC_MISMATCH" | "CONTENT_HASH_MISMATCH" => {
            Some(crate::i18n::t("err.corrupted"))
        }
        "MISSING_SHARDS" => Some(crate::i18n::t("err.missing_shards")),
        _ => None,
    }
}

/// 人类可读的字节数。
///
/// 用 1024 进制并标注 KiB/MiB，避免与厂商的 1000 进制混淆。
#[must_use]
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut i = 0usize;
    while v >= 1024.0 && i + 1 < UNITS.len() {
        v /= 1024.0;
        i += 1;
    }
    let unit = UNITS.get(i).copied().unwrap_or("B");
    if v >= 100.0 {
        format!("{v:.0} {unit}")
    } else if v >= 10.0 {
        format!("{v:.1} {unit}")
    } else {
        format!("{v:.2} {unit}")
    }
}

/// 带千分位的整数，便于读大数字。
#[must_use]
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(char::from(*c));
    }
    out
}

/// 把毫秒渲染成 `1:23:45` / `2:05` 形式。
///
/// 不足一小时时省略小时位——`0:02:05` 读起来比 `2:05` 累赘。
/// 秒始终补零，因为 `2:5` 会被误读成 2 分 5 十秒。
#[must_use]
pub fn human_duration(ms: u64) -> String {
    let total = ms / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// 解析人类书写的大小，如 `256K`、`4M`、`1G`、`65536`。
///
/// # Errors
///
/// 格式无法识别或数值溢出时返回错误。
pub fn parse_size(s: &str) -> anyhow::Result<u64> {
    let t = s.trim();
    if t.is_empty() {
        anyhow::bail!("大小不能为空");
    }
    let (num_part, mult) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 1024u64),
        Some('m' | 'M') => (&t[..t.len() - 1], 1024 * 1024),
        Some('g' | 'G') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        Some('b' | 'B') => (&t[..t.len() - 1], 1),
        _ => (t, 1),
    };
    let num_part = num_part.trim();
    let base: u64 = num_part
        .parse()
        .map_err(|_| anyhow::anyhow!("无法解析大小 {s:?}，示例：256K / 4M / 1G / 65536"))?;
    base.checked_mul(mult)
        .ok_or_else(|| anyhow::anyhow!("大小 {s:?} 溢出"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_formatting() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.00 KiB");
        assert_eq!(human_bytes(1536), "1.50 KiB");
        assert_eq!(human_bytes(10 * 1024), "10.0 KiB");
        assert_eq!(human_bytes(100 * 1024), "100 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MiB");
        assert_eq!(human_bytes(894_238_720), "853 MiB");
    }

    #[test]
    fn thousands_separator() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1), "1");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(thousands(894_238_720), "894,238,720");
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("65536").unwrap(), 65536);
        assert_eq!(parse_size("256K").unwrap(), 262_144);
        assert_eq!(parse_size("256k").unwrap(), 262_144);
        assert_eq!(parse_size("4M").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("1G").unwrap(), 1024 * 1024 * 1024);
        assert_eq!(parse_size("512B").unwrap(), 512);
        assert_eq!(parse_size(" 4M ").unwrap(), 4 * 1024 * 1024);
        assert_eq!(parse_size("4095M").unwrap(), 4095 * 1024 * 1024);
    }

    #[test]
    fn size_parsing_rejects_bad_input() {
        for bad in ["", "abc", "4X", "-1", "1.5M", "M"] {
            assert!(parse_size(bad).is_err(), "{bad:?} 本应被拒绝");
        }
        // 溢出必须拒绝而非回绕
        assert!(parse_size("99999999999999999999G").is_err());
    }

    #[test]
    fn error_code_maps_to_exit_code() {
        let e = anyhow::Error::new(omy_core::Error::NoMatchingSlot);
        let out = Out::new(Format::Human, true, 0, true);
        let code = report_error(&out, &e);
        assert_eq!(code, 3, "密码错误应为退出码 3");

        let e2 = anyhow::Error::new(omy_core::Error::HeaderMacMismatch);
        assert_eq!(report_error(&out, &e2), 4, "损坏应为退出码 4");

        let e3 = anyhow::Error::new(omy_core::Error::MissingShards {
            missing: vec![1],
            total: 3,
        });
        assert_eq!(report_error(&out, &e3), 6, "缺片应为退出码 6");
    }

    #[test]
    fn generic_error_is_general_exit_code() {
        let e = anyhow::anyhow!("随便一个错误");
        let out = Out::new(Format::Human, true, 0, true);
        assert_eq!(report_error(&out, &e), 1);
    }

    #[test]
    fn exit_code_survives_context_wrapping() {
        // 端到端验证曾发现：经 .context() 包装后退出码退化成 1，
        // 导致脚本无法区分「缺片」与「一般错误」。这里锁定修复。
        use anyhow::Context as _;
        let out = Out::new(Format::Human, true, 0, true);

        let wrapped = Err::<(), _>(omy_core::Error::MissingShards {
            missing: vec![1, 2],
            total: 5,
        })
        .context("合并分片失败")
        .unwrap_err();
        assert_eq!(report_error(&out, &wrapped), 6, "缺片经包装后仍应为 6");

        // 多层包装同样要能穿透
        let deep = Err::<(), _>(omy_core::Error::NoMatchingSlot)
            .context("打开文件失败")
            .context("处理第 3 个文件时")
            .unwrap_err();
        assert_eq!(report_error(&out, &deep), 3, "多层包装后仍应为 3");
    }
}
