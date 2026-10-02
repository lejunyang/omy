//! 把二维码模块矩阵渲染成终端里可直接扫码的 Unicode / ASCII 文本。
//!
//! # 为什么是纯函数
//!
//! 「能不能用 Unicode 半角块」取决于 stderr 是不是真终端（管道、CI、
//! 重定向到文件时 `▀▄█` 在多数等宽字体下照样能显示，但为了让脚本输出
//! 干净可解析，非 TTY 一律回落到纯 ASCII）。这个判断**在调用现场做一次**，
//! 把结果作为 [`RenderMode`] 传进渲染函数——渲染函数本身不碰任何终端句柄，
//! 于是可以用一个手搓的小矩阵把每一格的映射钉死在单测里。
//!
//! # 安静区（quiet zone）
//!
//! 二维码四周必须留至少 4 个模块宽的空白才能被摄像头/对焦识别。这里渲染时
//! 主动补两格空白边框：否则用户在终端里紧贴着上一行文字扫，很容易扫不出来，
//! 而现象是「命令明明打印了码却扫不动」——指不到渲染这一层。

use omy_remote::telegram::QrMatrix;

/// 渲染模式。
///
/// 由调用现场根据「stderr 是不是终端」决定后传入，渲染函数不自己探测——
/// 这就是「TTY 状态可注入」的接缝：测试用任意模式验证输出形状，不依赖真终端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderMode {
    /// 上下半角块：两行模块压成一行，用 `█ ▀ ▄ 空格` 表示四种明暗组合。
    HalfBlocks,
    /// 纯 ASCII：每个深色模块打两个 `#`，浅色打两个空格。
    ///
    /// 非 TTY / 管道时用它——输出全是 ASCII，进日志、进脚本都不会因为
    /// Unicode 块字符对齐问题变成一团乱码。
    Ascii,
}

/// 按 stderr 是否为 TTY 选渲染模式。
///
/// 抽出成一个小函数而不是在登录流程里 `if`：把「什么算 TTY」这条规则收在一处，
/// 也便于测试断言两种输入各自落到哪个模式。
#[must_use]
pub fn choose_mode(stderr_is_tty: bool) -> RenderMode {
    if stderr_is_tty {
        RenderMode::HalfBlocks
    } else {
        RenderMode::Ascii
    }
}

/// 安静区宽度（模块数）。
///
/// QR 规范要求 4，终端里两格视觉留白通常已足够对焦；取两格是为了别把码
/// 撑得过大换了行。
const QUIET: usize = 2;

/// 把矩阵渲染成多行字符串（行间以 `\n` 连接，不含结尾换行）。
#[must_use]
pub fn render(matrix: &QrMatrix, mode: RenderMode) -> String {
    match mode {
        RenderMode::HalfBlocks => render_half_blocks(matrix),
        RenderMode::Ascii => render_ascii(matrix),
    }
}

/// 取某个模块的明暗；越界（用于补奇数行的最后一行下半）一律当浅色。
#[inline]
fn dark(m: &QrMatrix, r: usize, c: usize) -> bool {
    if r >= m.size || c >= m.size {
        return false;
    }
    // 已在上文做了越界检查，这里用 get 而非下标：本 crate 开了 indexing_slicing，
    // 下标会让 clippy 报「可能 panic」。
    *m.modules.get(r * m.size + c).unwrap_or(&false)
}

/// 半角块模式：两行模块压一行。
fn render_half_blocks(m: &QrMatrix) -> String {
    let size = m.size;
    let mut lines: Vec<String> = Vec::new();

    // 顶部安静区：两整行空白
    for _ in 0..QUIET {
        lines.push(" ".repeat(size + 2 * QUIET));
    }

    let mut r = 0;
    while r < size {
        let mut line = String::with_capacity(size + 2 * QUIET);
        for _ in 0..QUIET {
            line.push(' ');
        }
        for c in 0..size {
            let top = dark(m, r, c);
            let bottom = dark(m, r + 1, c);
            // 两格明暗组合 → 一个块字符。这是整个渲染最容易写错的映射，
            // 单测里逐格钉住。
            let ch = match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            };
            line.push(ch);
        }
        for _ in 0..QUIET {
            line.push(' ');
        }
        lines.push(line);
        r += 2;
    }

    // 底部安静区
    for _ in 0..QUIET {
        lines.push(" ".repeat(size + 2 * QUIET));
    }
    lines.join("\n")
}

/// ASCII 模式：一个模块两列宽。
fn render_ascii(m: &QrMatrix) -> String {
    let size = m.size;
    let mut lines: Vec<String> = Vec::new();

    let row_width = 2 * size + 2 * 2 * QUIET;
    for _ in 0..QUIET {
        lines.push(" ".repeat(row_width));
    }

    for r in 0..size {
        let mut line = String::with_capacity(row_width);
        for _ in 0..(2 * QUIET) {
            line.push(' ');
        }
        for c in 0..size {
            // 两列同色，保证方块在等宽字体下是正方形：一列 # 在终端里是窄的，
            // 两列才接近一个模块的宽高比
            if dark(m, r, c) {
                line.push_str("##");
            } else {
                line.push_str("  ");
            }
        }
        for _ in 0..(2 * QUIET) {
            line.push(' ');
        }
        lines.push(line);
    }

    for _ in 0..QUIET {
        lines.push(" ".repeat(row_width));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手搓一个矩阵：2x2，左上与右下为深色。
    fn mat_2x2() -> QrMatrix {
        QrMatrix {
            size: 2,
            // 行0: 暗 亮 ; 行1: 亮 暗
            modules: vec![true, false, false, true],
        }
    }

    /// 3x3 奇数边长：最后一行要配一个「浅色下半」，不能越界panic。
    fn mat_odd() -> QrMatrix {
        QrMatrix {
            size: 3,
            modules: vec![
                true, true, false, // r0
                false, true, true, // r1
                true, false, false, // r2（单独成一行，下半视作浅色）
            ],
        }
    }

    /// TTY 状态直接决定模式，且这条规则是注入的入口。
    ///
    /// 不这样会怎样：渲染函数自己去 `is_terminal()`，测试就没法在非 TTY 的
    /// 测试进程里断言「TTY 分支会产出半角块」——而那条分支正是用户扫码用的。
    #[test]
    fn choose_mode_follows_tty_flag() {
        assert_eq!(choose_mode(true), RenderMode::HalfBlocks);
        assert_eq!(choose_mode(false), RenderMode::Ascii);
    }

    /// 半角块四格映射必须逐格正确。
    ///
    /// 不这样会怎样：把 `(top,dark bottom,light)` 写成 `█` 之类，扫出来的码
    /// 明暗反了——用户对着一张「看起来是二维码」的图怎么扫都没反应，
    /// 而错误藏在字符映射里，肉眼几乎看不出来。
    #[test]
    fn half_block_quadrant_mapping_is_exact() {
        let out = render(&mat_2x2(), RenderMode::HalfBlocks);
        // 2x2 → 一行模块（r0/r1 压一起）。去掉两侧安静区后中间那行应是：
        //   c0: top=暗 bottom=亮 → ▀
        //   c1: top=亮 bottom=暗 → ▄
        let mid: Vec<&str> = out.lines().collect();
        // 顶部安静 2 行 + 中间 1 行 + 底部安静 2 行 = 5 行
        assert_eq!(mid.len(), 5, "应有上下各两格安静区，实际 {} 行", mid.len());
        let center = mid[2];
        let trimmed = center.trim();
        assert_eq!(trimmed, "▀▄", "2x2 中间行应恰好是 ▀▄，实际 {center:?}");
    }

    /// 奇数边长不能越界 panic，且最后一行按「下半全浅色」处理。
    #[test]
    fn odd_size_does_not_panic_and_pads_bottom() {
        // 不应 panic
        let out = render(&mat_odd(), RenderMode::HalfBlocks);
        let lines: Vec<&str> = out.lines().collect();
        // size=3 → r 从 0 起：(0,1) 一行，(2, 越界当亮) 一行 = 2 个模块行
        assert_eq!(lines.len(), 2 + 2 * QUIET);
    }

    /// ASCII 模式：深色模块变成 ##，浅色变空格。
    ///
    /// 不这样会怎样：非 TTY 回退分支写错，脚本里 `--json` 旁边那段二维码
    /// 会变成一片全黑或全白，用户复制走也没用。
    #[test]
    fn ascii_mode_marks_dark_modules() {
        let out = render(&mat_2x2(), RenderMode::Ascii);
        // 第 0 模块行：暗亮 → "##  " ; 第 1 行：亮暗 → "  ##"
        let lines: Vec<&str> = out.lines().collect();
        // 顶安静 2 + 2 模块行 + 底安静 2 = 6
        assert_eq!(lines.len(), 6);
        // 每行宽 = 2*size + 4*QUIET = 4 + 8 = 12
        let r0 = lines[2];
        let r1 = lines[3];
        assert_eq!(r0.len(), 12, "每行应等宽");
        // 去掉两侧各 4 个安静空格后，模块部分（4 个字符）应是 "##  " / "  ##"
        let mid0 = &r0[4..8];
        let mid1 = &r1[4..8];
        assert_eq!(mid0, "##  ", "第 0 行应暗亮 → '##  '，实际 {r0:?}");
        assert_eq!(mid1, "  ##", "第 1 行应亮暗 → '  ##'，实际 {r1:?}");
    }

    /// 两种模式都要带安静区，否则终端里贴着上一行扫不出来。
    #[test]
    fn quiet_zone_is_present_in_both_modes() {
        for mode in [RenderMode::HalfBlocks, RenderMode::Ascii] {
            let out = render(&mat_2x2(), mode);
            let lines: Vec<&str> = out.lines().collect();
            // 第一行必须是纯空白（顶部安静区）
            assert!(
                lines[0].trim().is_empty(),
                "{mode:?} 顶部应有安静区，实际首行 {:?}",
                lines[0]
            );
            assert!(
                lines[lines.len() - 1].trim().is_empty(),
                "{mode:?} 底部应有安静区"
            );
        }
    }

    /// 渲染真实 token：和 encode_matrix 对接，确认能产出非空多行。
    ///
    /// 不这样会怎样：渲染函数直接吃 QrMatrix 没问题，但登录流程喂给它的是
    /// `encode_matrix` 的产物，两者尺寸假设不一致（比如假定方阵）时要到运行期才暴露。
    #[test]
    fn renders_a_real_encoded_token() {
        let matrix = omy_remote::telegram::encode_matrix("tg://login?token=Zm9vYmFy").expect("应能编码");
        for mode in [RenderMode::HalfBlocks, RenderMode::Ascii] {
            let out = render(&matrix, mode);
            assert!(!out.is_empty());
            assert!(out.lines().count() >= 3, "二维码不该只有一两行");
        }
    }
}
