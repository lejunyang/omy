//! 扫码登录：把 `auth.exportLoginToken` 那套流程里**不碰网络的部分**做成可测的。
//!
//! # 为什么扫码是主推路径
//!
//! 不是因为它更时髦，而是因为手机号路径有一个**绕不过去**的可用性问题：
//! 账号只要还有其他活跃 session，Telegram 就优先把验证码发到应用内而不是发
//! 短信，而 `force_sms` 在 Layer 100+ 已被移除、无法强制走短信。也就是说
//! 「已经在用 Telegram 的人」默认都收不到短信，得去另一个客户端取码。
//!
//! 扫码本来就在那个客户端上操作，**不存在「码发到哪去了」这个问题**。
//! 这是真实踩到的坑之后得出的结论，不是预设偏好。
//!
//! # 流程
//!
//! ```text
//! auth.exportLoginToken
//!   ├─ loginToken        → 编码成 tg://login?token=<base64url> → 渲染二维码
//!   │                      ├─ 过期 → 自动重新导出、换一张图（不是错误）
//!   │                      └─ 手机端确认 → 再次 exportLoginToken 取结果
//!   ├─ loginTokenMigrateTo → 切到该 DC 后调 auth.importLoginToken
//!   └─ loginTokenSuccess   → 成功（若开了 2FA，仍要走云密码）
//! ```
//!
//! # 这一层刻意不发请求
//!
//! 发请求的部分在 GUI 那一侧（要用 grammers 的 client）。这里只做三件纯逻辑：
//! token 怎么编成二维码内容、过期该怎么反应、迁移 DC 该怎么反应。
//! 它们恰好是最容易写错、且不需要账号就能钉死的部分。

use crate::telegram::login::QrToken;

/// 二维码里要放的内容。
///
/// # 为什么是 `tg://login?token=` 而不是别的
///
/// 官方客户端只认这个 scheme。**token 必须用 base64**url**（`-_`，且去掉尾部
/// `=` 填充）**，不是标准 base64——标准 base64 里的 `+` 和 `/` 在 URL 里有别的
/// 含义，扫出来的链接会被解析坏，而现象是「官方客户端说二维码无效」，
/// 完全指不到编码这一步。
#[must_use]
pub fn token_url(token: &[u8]) -> String {
    format!("tg://login?token={}", base64url_nopad(token))
}

/// base64url 编码，不带填充。
///
/// 自己写而不是引一个 crate：只有这一处用，且规则简单到不值得多一个依赖
/// （依赖越少，Android 交叉编译越不容易出事）。
#[must_use]
pub fn base64url_nopad(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        // 取三个字节拼成 24 位，再切成四个 6 位
        let b0 = *chunk.first().unwrap_or(&0);
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        let idx = [
            (n >> 18) & 0x3F,
            (n >> 12) & 0x3F,
            (n >> 6) & 0x3F,
            n & 0x3F,
        ];
        // 不足三字节时少输出对应的字符，而不是补 '='：
        // 官方要求的是无填充形式
        let keep = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for i in idx.iter().take(keep) {
            // 索引恒在 0..64，表长 64；用 get 而不是下标是因为
            // omy-gui 禁止切片索引，这一层也保持同样习惯
            if let Some(c) = T.get(*i as usize) {
                out.push(char::from(*c));
            }
        }
    }
    out
}

/// 二维码的模块矩阵：`size` 行 `size` 列，`true` 表示黑格。
///
/// # 为什么给矩阵而不是图片
///
/// 界面自己用格子画（原型 `telegram-remote-prototype.html` 就是这么画的）。
/// 生成 PNG 再塞进 WebView 要多一次图像编码和一次 base64，而且缩放时会糊——
/// 二维码最怕的就是边缘模糊导致扫不出来。矩阵交给 CSS 画则任何尺寸都是锐利的。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct QrMatrix {
    /// 边长（模块数）。
    pub size: usize,
    /// 逐行展开的模块，长度为 `size * size`。
    pub modules: Vec<bool>,
}

/// 把二维码内容编成模块矩阵。
///
/// # Errors
///
/// 内容长得放不进任何版本的二维码时返回错误说明。登录令牌只有几十字节，
/// 正常情况下不会发生；有这个分支是因为**不能 panic**（这一层服务于 GUI）。
pub fn encode_matrix(content: &str) -> Result<QrMatrix, String> {
    // 纠错级别选 L：登录二维码是屏幕上显示、近距离扫，不存在污损和磨损，
    // 而级别越高模块越密、在小尺寸下越难扫。官方客户端显示的也是低纠错级别。
    let code = qrcode::QrCode::with_error_correction_level(content, qrcode::EcLevel::L)
        .map_err(|e| format!("无法生成二维码：{e}"))?;
    let size = code.width();
    let modules = code
        .into_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Ok(QrMatrix { size, modules })
}

/// 服务端对 `auth.exportLoginToken` 的三种回应，去掉了凭据本体。
///
/// **不携带 token 字节**：那串东西等同于一次登录凭据，让它流经状态机只会多
/// 一处泄露面。界面拿它渲染二维码，用完即弃。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrStep {
    /// 拿到了新 token，可以显示二维码。
    Shown(QrToken),
    /// 需要换到别的数据中心。
    ///
    /// **这是中间态，不是失败**，见 [`QrOutcome`]。
    MigrateTo {
        /// 目标数据中心编号。
        dc: i32,
    },
    /// 手机端已确认，登录成功。
    Success,
    /// 这张码过期了。
    Expired,
    /// 成功了但账号开了两步验证，还要云密码。
    NeedsPassword,
}

/// 收到某一步之后该做什么。
///
/// 把「该做什么」算成数据而不是直接写进 if-else，是为了让这几条最容易搞错的
/// 归类能被单测钉住。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QrOutcome {
    /// 显示这张二维码，等手机端扫。
    Display(QrToken),
    /// **自动**重新导出 token 并换一张图。
    ///
    /// 官方明确要求过期后自动重新生成。让二维码停在那里变成死图，用户会一直扫
    /// 一张永远不会成功的图——而界面上什么错误都没有。
    Refresh,
    /// 切数据中心，然后调 `auth.importLoginToken`。
    ///
    /// 界面显示「正在切换数据中心…」并继续等。
    Migrate {
        /// 目标数据中心。
        dc: i32,
    },
    /// 登录完成。
    Done,
    /// 还要云密码。
    AskPassword,
}

impl QrOutcome {
    /// 这一步用户需不需要做什么。
    ///
    /// 为真的只有「要输云密码」。其余都是程序自己该继续推进的事——
    /// 界面在这些状态下不该显示任何要求操作的提示，更不该显示错误。
    #[must_use]
    pub const fn needs_user_action(&self) -> bool {
        matches!(self, Self::AskPassword)
    }

    /// 这一步是不是「出错了」。
    ///
    /// **恒为 false**：扫码流程里没有一步是错误。过期是正常轮换、迁移是正常
    /// 协商。有这个方法是为了让「不许把它们显示成错误」这条规则在类型层面
    /// 有个落点，而不是散在界面代码里靠记性。
    #[must_use]
    pub const fn is_error(&self) -> bool {
        false
    }
}

/// 决定收到某一步之后该做什么。
#[must_use]
pub const fn decide(step: QrStep) -> QrOutcome {
    match step {
        QrStep::Shown(t) => QrOutcome::Display(t),
        // 过期 → 自动换一张。**不是失败**
        QrStep::Expired => QrOutcome::Refresh,
        // 迁移 → 中间态，继续推进。**不是失败**
        QrStep::MigrateTo { dc } => QrOutcome::Migrate { dc },
        QrStep::Success => QrOutcome::Done,
        QrStep::NeedsPassword => QrOutcome::AskPassword,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// base64url 必须用 `-_` 且不带填充。
    ///
    /// 不这样会怎样：标准 base64 的 `+` `/` 在 URL 里有别的含义，官方客户端
    /// 扫出来会说「二维码无效」——而那个报错完全指不到编码这一步，
    /// 会让人去查二维码渲染、token 有效期、网络，全都查不到。
    #[test]
    fn base64url_uses_url_safe_alphabet_without_padding() {
        // 这三个字节在标准 base64 下会产出 '+' 与 '/'
        let tricky = [0xFBu8, 0xFF, 0xBF];
        let e = base64url_nopad(&tricky);
        assert!(!e.contains('+'), "不能出现 +：{e}");
        assert!(!e.contains('/'), "不能出现 /：{e}");
        assert!(!e.contains('='), "不能带填充：{e}");
        assert!(
            e.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "只能用 base64url 的字母表：{e}"
        );
    }

    /// 编码结果要与已知答案一致，长度也要对。
    ///
    /// 不这样会怎样：只断言"没有 + 和 /"的话，一个完全算错但恰好没产出这两个
    /// 字符的实现照样通过——而那种 token 扫出来一样是无效的。
    #[test]
    fn base64url_matches_known_vectors() {
        // RFC 4648 的经典向量，换成 url-safe 无填充形式
        assert_eq!(base64url_nopad(b""), "");
        assert_eq!(base64url_nopad(b"f"), "Zg");
        assert_eq!(base64url_nopad(b"fo"), "Zm8");
        assert_eq!(base64url_nopad(b"foo"), "Zm9v");
        assert_eq!(base64url_nopad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_nopad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_nopad(b"foobar"), "Zm9vYmFy");
        // 无填充形式的长度规则：每 3 字节 4 字符，余 1 字节出 2 字符、余 2 出 3
        for n in 0usize..40 {
            // 用算式生成而不是写长串重复字节：那种字面量会让本机安全软件
            // 把测试二进制删掉（AGENTS.md 记过这一条）
            let data: Vec<u8> = (0..n).map(|i| (i as u8).wrapping_mul(31) ^ 0x5B).collect();
            let want = n / 3 * 4
                + match n % 3 {
                    1 => 2,
                    2 => 3,
                    _ => 0,
                };
            assert_eq!(base64url_nopad(&data).len(), want, "n={n} 长度不对");
        }
    }

    /// 二维码内容必须是官方认的那个 scheme。
    ///
    /// 不这样会怎样：官方客户端只认 `tg://login?token=`，拼错前缀会让扫码
    /// 直接无反应，而界面这边毫无异常。
    #[test]
    fn token_url_has_the_official_shape() {
        let u = token_url(b"hello");
        assert!(u.starts_with("tg://login?token="), "{u}");
        assert_eq!(u, format!("tg://login?token={}", base64url_nopad(b"hello")));
        // token 本体不能被原样塞进 URL
        assert!(!u.contains("hello"), "token 应当是编码后的形式：{u}");
    }

    /// 过期必须触发**自动刷新**，而不是报错。
    ///
    /// 不这样会怎样：二维码停在那里变成死图，用户一直扫一张永远不会成功的图，
    /// 而界面上什么错误都没有——这是官方文档明确要求自动重新生成的地方。
    #[test]
    fn expiry_refreshes_instead_of_failing() {
        let o = decide(QrStep::Expired);
        assert_eq!(o, QrOutcome::Refresh);
        assert!(!o.is_error(), "过期不是错误");
        assert!(!o.needs_user_action(), "换图不需要用户做任何事");
    }

    /// 迁移数据中心是中间态，不是失败。
    ///
    /// 不这样会怎样：显示「登录失败」会让用户点取消或重试，而这次登录本来
    /// 再等一下就成了——一个被我们自己打断的成功。
    #[test]
    fn migrate_is_an_intermediate_step() {
        let o = decide(QrStep::MigrateTo { dc: 4 });
        assert_eq!(o, QrOutcome::Migrate { dc: 4 });
        assert!(!o.is_error(), "迁移不是错误");
        assert!(!o.needs_user_action(), "迁移不需要用户做任何事");
    }

    /// 只有「要云密码」才需要用户操作。
    ///
    /// 不这样会怎样：在显示二维码、换图、迁移这些阶段提示用户操作，
    /// 他会以为要点什么，而实际上该做的只是等。
    #[test]
    fn only_password_needs_the_user() {
        assert!(decide(QrStep::NeedsPassword).needs_user_action());
        for s in [
            QrStep::Shown(QrToken { expires_in_secs: 30 }),
            QrStep::Expired,
            QrStep::MigrateTo { dc: 2 },
            QrStep::Success,
        ] {
            assert!(
                !decide(s).needs_user_action(),
                "{s:?} 不该要求用户操作"
            );
        }
    }

    /// 扫码流程里没有一步该被当成错误。
    ///
    /// 不这样会怎样：把过期或迁移归成错误，界面就会弹重试——而重试在这两种
    /// 情况下都是错的动作（该做的是换图 / 继续等）。
    #[test]
    fn no_step_is_an_error() {
        for s in [
            QrStep::Shown(QrToken { expires_in_secs: 1 }),
            QrStep::Expired,
            QrStep::MigrateTo { dc: 5 },
            QrStep::Success,
            QrStep::NeedsPassword,
        ] {
            assert!(!decide(s).is_error(), "{s:?} 不该被当成错误");
        }
    }

    /// 成功就是成功，不要多问一步。
    #[test]
    fn success_is_done() {
        assert_eq!(decide(QrStep::Success), QrOutcome::Done);
    }

    /// 显示二维码时要把剩余秒数带上。
    ///
    /// 不这样会怎样：界面不知道什么时候该换图，只能靠猜一个固定间隔——
    /// 猜短了白发请求（Telegram 有限流），猜长了用户扫的是已经过期的图。
    #[test]
    fn display_carries_the_expiry() {
        let o = decide(QrStep::Shown(QrToken { expires_in_secs: 27 }));
        assert_eq!(o, QrOutcome::Display(QrToken { expires_in_secs: 27 }));
    }

    /// 矩阵必须是方阵，且长度与边长自洽。
    ///
    /// 不这样会怎样：前端按 `size` 逐行切 `modules`，两者对不上时画出来的是
    /// 一张错位的图——它看起来**像**二维码，但扫不出来。而「扫不出来」这个
    /// 现象会让人去查令牌编码、有效期、甚至相机，查不到渲染这一步。
    #[test]
    fn matrix_is_square_and_self_consistent() {
        let m = encode_matrix("tg://login?token=Zm9vYmFy").expect("应能生成");
        assert_eq!(
            m.modules.len(),
            m.size * m.size,
            "模块数必须正好是 size 的平方"
        );
        // 二维码最小版本是 21x21，且边长恒为奇数（4*version+17）
        assert!(m.size >= 21, "边长不该小于版本 1 的 21：{}", m.size);
        assert_eq!(m.size % 2, 1, "二维码边长恒为奇数：{}", m.size);
    }

    /// 三个定位角必须是实心的 7x7 回字。
    ///
    /// 不这样会怎样：这是唯一能在不引入解码器的前提下验证「画出来的真是二维码
    /// 而不是一堆随机格子」的结构特征。少了它，一个把所有模块都填成 false 的
    /// 实现照样能通过上面那条形状断言——而那是一张纯白的图。
    #[test]
    fn finder_patterns_are_present_in_three_corners() {
        let m = encode_matrix("tg://login?token=Zm9vYmFy").expect("应能生成");
        let at = |x: usize, y: usize| m.modules.get(y * m.size + x).copied().unwrap_or(false);
        // 定位角的结构：外圈 7x7 全黑边、中间 3x3 实心黑、之间一圈白
        let check_finder = |ox: usize, oy: usize, label: &str| {
            for i in 0..7 {
                assert!(at(ox + i, oy), "{label} 顶边第 {i} 格应为黑");
                assert!(at(ox + i, oy + 6), "{label} 底边第 {i} 格应为黑");
                assert!(at(ox, oy + i), "{label} 左边第 {i} 格应为黑");
                assert!(at(ox + 6, oy + i), "{label} 右边第 {i} 格应为黑");
            }
            // 中心 3x3 实心
            for dy in 2..5 {
                for dx in 2..5 {
                    assert!(at(ox + dx, oy + dy), "{label} 中心应为实心黑");
                }
            }
            // 隔离的那一圈白
            assert!(!at(ox + 1, oy + 1), "{label} 第二圈应为白");
            assert!(!at(ox + 5, oy + 5), "{label} 第二圈应为白");
        };
        check_finder(0, 0, "左上");
        check_finder(m.size - 7, 0, "右上");
        check_finder(0, m.size - 7, "左下");
    }

    /// 不同内容要画出不同的图。
    ///
    /// 不这样会怎样：换图时若矩阵没跟着变，用户扫到的永远是第一张（早已过期的）
    /// 令牌——而界面上动画放了、次数也加了，看起来一切正常。
    #[test]
    fn different_content_yields_a_different_matrix() {
        let a = encode_matrix("tg://login?token=AAAA").expect("应能生成");
        let b = encode_matrix("tg://login?token=BBBB").expect("应能生成");
        assert_ne!(a.modules, b.modules, "内容不同，图案必须不同");
    }

    /// 生成失败要返回错误而不是 panic。
    ///
    /// 不这样会怎样：这一层服务于 GUI，而 omy-gui 禁止 panic；release 下
    /// `panic = "abort"` 更是连 catch_unwind 都救不了，一次崩溃带走整个应用。
    #[test]
    fn oversized_content_errors_instead_of_panicking() {
        // 远超二维码容量上限（最高版本约 2953 字节）
        let huge: String = std::iter::repeat_n('A', 8000).collect();
        assert!(
            encode_matrix(&huge).is_err(),
            "放不下时要返回错误，不能崩"
        );
    }
}
