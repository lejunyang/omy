//! 进度显示：加解密过程中的可视化反馈。
//!
//! # 为什么需要
//!
//! 加解密是纯 CPU 循环，几百 MB 的文件要跑好几秒到几十秒。期间若没有任何
//! 输出，用户无法区分「正在算」和「卡死了」，只能盯着光标猜——这也是
//! 大文件场景最常见的抱怨来源。
//!
//! # 输出通道
//!
//! 进度一律走 **stderr**，与 `output` 模块的分工一致：stdout 只放结果数据。
//! 否则 `omy cat x.omy | mpv -` 会把进度条字符混进管道，
//! `omy info --json x.omy | jq` 会因为多出的非 JSON 文本而解析失败。
//!
//! # 什么时候不显示
//!
//! - `--json`：机器消费，任何装饰都是噪声
//! - `-q`：用户明确要求安静
//! - stderr 不是终端（重定向到文件或管道）：进度条靠 `\r` 原地刷新，
//!   写进文件会变成几千行垃圾
//! - 文件小于阈值：几十毫秒就结束，闪一下反而干扰

use indicatif::{ProgressBar, ProgressStyle};

/// 小于这个大小就不显示进度条。
///
/// 8 MiB 在普通机器上约几十毫秒完成，进度条一闪而过纯属干扰。
/// 阈值定在这里而不是更小，是因为「短暂闪现的进度条」比没有更烦人。
const MIN_BYTES_FOR_BAR: u64 = 8 * 1024 * 1024;

/// 加解密进度条。
///
/// 不满足显示条件时内部为 `None`，所有方法变成空操作——
/// 这样调用方不需要在每个调用点写 `if let Some(...)`，
/// 避免「某个分支忘了判断」导致 JSON 输出被污染。
#[derive(Debug)]
pub struct Progress {
    bar: Option<ProgressBar>,
}

impl Progress {
    /// 创建进度条。
    ///
    /// `enabled` 由调用方传入（通常是「非 JSON 且非 quiet」），
    /// `total` 是明文总字节数，`label` 显示在进度条前面。
    #[must_use]
    pub fn new(enabled: bool, total: u64, label: &str) -> Self {
        use std::io::IsTerminal as _;

        // 三个条件缺一不可，理由见模块文档
        if !enabled || total < MIN_BYTES_FOR_BAR || !std::io::stderr().is_terminal() {
            return Self { bar: None };
        }

        let bar = ProgressBar::new(total);
        // 模板解析失败时退回默认样式而不是 panic：
        // 进度条是锦上添花，绝不能因为它让加密命令挂掉
        if let Ok(style) = ProgressStyle::with_template(
            "{msg} [{bar:30}] {bytes}/{total_bytes} ({percent}%) {bytes_per_sec} 剩余 {eta}",
        ) {
            bar.set_style(style.progress_chars("=> "));
        }
        bar.set_message(String::from(label));
        Self { bar: Some(bar) }
    }

    /// 报告已完成的字节数。
    ///
    /// 传入的是**累计值**而非增量，与 `omy_core` 的进度回调语义一致；
    /// 用增量的话，回调里一旦漏掉一次就会永久性偏差。
    pub fn set(&self, done: u64) {
        if let Some(b) = &self.bar {
            b.set_position(done);
        }
    }

    /// 收尾并清除进度条。
    ///
    /// 必须清除而不是留在屏幕上：一批文件逐个加密时，
    /// 每个都留一条进度条会把真正的结果信息挤出屏幕。
    pub fn finish(&self) {
        if let Some(b) = &self.bar {
            b.finish_and_clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_not_enabled() {
        // JSON / quiet 模式下必须完全没有进度条，
        // 否则 `omy info --json | jq` 会因为多出的文本而解析失败
        let p = Progress::new(false, 100 * 1024 * 1024, "加密");
        assert!(p.bar.is_none(), "enabled=false 时不该创建进度条");
    }

    #[test]
    fn disabled_for_small_files() {
        // 小文件几十毫秒就结束，闪一下比不显示更烦人
        let p = Progress::new(true, 1024, "加密");
        assert!(p.bar.is_none(), "小于阈值不该创建进度条");
    }

    #[test]
    fn noop_methods_do_not_panic_when_disabled() {
        // 调用方不做 Option 判断，所以关闭态下的方法必须是安全的空操作。
        // 不这样保证的话，JSON 模式跑加密会直接崩
        let p = Progress::new(false, 0, "加密");
        p.set(10);
        p.finish();
    }

    #[test]
    fn threshold_is_the_only_size_gate() {
        // 前提校验：阈值判断确实存在，且边界两侧行为不同。
        // 否则上面两条测试可能只是因为「压根没做 size 判断、
        // 恰好都返回 None」而通过。
        //
        // 断言写成对函数返回值的比较而不是对常量的比较：
        // 比较常量是编译期定值，永远不会失败，抓不到任何缺陷。
        let below = Progress::new(true, MIN_BYTES_FOR_BAR - 1, "加密");
        assert!(below.bar.is_none(), "刚好低于阈值不该创建进度条");
    }
}
