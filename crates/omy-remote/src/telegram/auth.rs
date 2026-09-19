//! 自己发原始 `auth.*` 请求，把 grammers 会 panic 的分支变成可返回的错误。
//!
//! # 为什么要绕过 grammers 的高层封装
//!
//! grammers 0.10 在若干「它认为不会发生」的分支上直接 `panic!` /
//! `unimplemented!()`。登录路径上至少有这四处（`client/auth.rs`）：
//!
//! ```text
//! auth.rs:270  SC::Success(_)         => panic!("should not have logged in yet")
//! auth.rs:271  SC::PaymentRequired(_) => unimplemented!()
//! auth.rs:283  SC::Success(_)         => panic!(...)      // DC 迁移后重试那条路径
//! auth.rs:284  SC::PaymentRequired(_) => unimplemented!()
//! ```
//!
//! **omy-gui 禁用 panic**，而这些分支真出现就是进程直接崩、不是返回错误。
//!
//! # 为什么不用 `catch_unwind` 兜
//!
//! 因为**根 `Cargo.toml` 里 `[profile.release] panic = "abort"`**，release 下
//! `catch_unwind` 根本不生效。靠它兜会写出一个「开发机上看着有效、release 里
//! 完全不起作用」的假兜底——那比没有兜底更危险，因为它会让人以为已经处理了。
//!
//! # 所以做法是：自己发请求、自己解 enum
//!
//! `tl::enums::auth::SentCode` 有三个 variant，其中 `PaymentRequired` 在
//! grammers 那边是 `unimplemented!()`，**在我们这边就是一个普通的 match 分支**。
//! 于是「进程崩」变成「界面报一条看得懂的错」。
//!
//! 这同时也是我们本来就需要的方向：精确控制分片 offset/limit 也必须走底层。

use crate::telegram::login::{CodeDelivery, CodeShape};

/// 发码请求的结果。
///
/// 与 `tl::enums::auth::SentCode` 的三个 variant 一一对应，**每一个都有归宿**，
/// 不存在「不该发生所以 panic」的分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendCodeOutcome {
    /// 正常发出，等用户输码。
    Sent(CodeShape),
    /// 服务端说「已经登录了」。
    ///
    /// grammers 在这里 `panic!("should not have logged in yet")`。
    /// 对我们来说这是个**正常可达**的状态：上一次登录其实成功了、只是我们的
    /// 登录态没存住，于是又来发码。正确反应是直接当成已登录继续，而不是崩。
    AlreadyAuthorized,
    /// 这个号码需要**付费**才能收验证码。
    ///
    /// grammers 在这里 `unimplemented!()`。Telegram 确实有需要付费的投递方式
    /// （某些地区 / 某些号段），所以它不是理论分支。
    ///
    /// 我们的界面没有支付流程，**也不打算有**——但那意味着要给用户一条看得懂的
    /// 说明（「这个号码需要付费短信，请改用扫码登录」），而不是让进程消失。
    /// 扫码路径不经过 `auth.sendCode`，所以这条建议是真的可行的出路。
    PaymentRequired,
}

impl SendCodeOutcome {
    /// 这个结果下，界面该不该引导用户改用扫码。
    ///
    /// 只有需要付费时为真。这是**唯一**一条我们不打算支持的投递方式，
    /// 而扫码恰好完全绕开它。
    #[must_use]
    pub const fn suggests_qr_instead(&self) -> bool {
        matches!(self, Self::PaymentRequired)
    }

    /// 这个结果是不是意味着「不用再输验证码了」。
    #[must_use]
    pub const fn already_done(&self) -> bool {
        matches!(self, Self::AlreadyAuthorized)
    }
}

/// 把服务端给的验证码类型映射成我们的 [`CodeShape`]。
///
/// # 为什么要有这一层
///
/// 一是**验证码不一定是数字**：`sentCodeTypeSmsWord` / `sentCodeTypeSmsPhrase`
/// 分别是一个单词和一个短句。输入框按「6 位数字」写死会让这两种情况根本没法输
/// ——数字键盘、长度限制、字符校验全都不对。
///
/// 二是让「发到哪里」这件事有唯一出处，见 [`CodeDelivery`]。
///
/// `length` 对 word/phrase 没有意义，此时取 0，界面据此不做长度限制。
#[must_use]
pub fn shape_of(kind: &SentCodeKind) -> CodeShape {
    match kind {
        SentCodeKind::App { length } => CodeShape {
            length: *length,
            via: CodeDelivery::App,
        },
        SentCodeKind::Sms { length } => CodeShape {
            length: *length,
            via: CodeDelivery::Sms,
        },
        SentCodeKind::Call { length } => CodeShape {
            length: *length,
            via: CodeDelivery::Call,
        },
        SentCodeKind::FlashCall | SentCodeKind::MissedCall { .. } => CodeShape {
            length: 0,
            via: CodeDelivery::FlashCall,
        },
        // 单词与短句：长度 0 表示「不要按位数限制输入」
        SentCodeKind::Word | SentCodeKind::Phrase => CodeShape {
            length: 0,
            via: CodeDelivery::Unknown,
        },
    }
}

/// 服务端可能给出的验证码投递类型。
///
/// 这是 `tl::enums::auth::SentCodeType` 的**我们关心的那部分**的镜像。
/// 做一层镜像而不是直接用 TL 类型，是为了让上面那些映射规则能在不依赖网络、
/// 不构造 TL 对象的情况下被单测覆盖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentCodeKind {
    /// 发到其他已登录的客户端。
    App { length: u8 },
    /// 短信。
    Sms { length: u8 },
    /// 电话语音播报。
    Call { length: u8 },
    /// 闪信：来电号码后几位就是码。
    FlashCall,
    /// 未接来电：号码后 N 位是码。
    MissedCall { length: u8 },
    /// 一个**单词**，不是数字。
    Word,
    /// 一个**短句**，不是数字。
    Phrase,
}

impl SentCodeKind {
    /// 这种码是不是纯数字。
    ///
    /// 界面据此决定：要不要用数字键盘、要不要按位数分格、要不要做数字校验。
    ///
    /// 不这样会怎样：按「纯数字」写死输入框，遇到单词或短句时用户**根本输不
    /// 进去**——数字键盘打不出字母，长度限制也不对。而这两种类型是官方协议
    /// 里真实存在的。
    #[must_use]
    pub const fn is_numeric(&self) -> bool {
        matches!(
            self,
            Self::App { .. } | Self::Sms { .. } | Self::Call { .. } | Self::MissedCall { .. }
        )
    }

    /// 已知的固定长度；没有固定长度时为 `None`。
    ///
    /// 闪信、单词、短句都没有可用于分格输入的长度。
    #[must_use]
    pub const fn fixed_length(&self) -> Option<u8> {
        match self {
            Self::App { length } | Self::Sms { length } | Self::Call { length } => Some(*length),
            // 未接来电的 length 是「号码后几位」，不是可输入格数，所以也不给
            Self::MissedCall { .. } | Self::FlashCall | Self::Word | Self::Phrase => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 需要付费的分支必须是**可处理的结果**，而不是让进程崩。
    ///
    /// 不这样会怎样：grammers 在这条分支上是 `unimplemented!()`，撞上就是
    /// 进程直接消失，用户看到的是程序闪退而不是一条说明。而 release 下
    /// `panic = "abort"`，连 catch_unwind 都救不了——只能不让它有机会被撞到。
    #[test]
    fn payment_required_is_a_handled_outcome() {
        let o = SendCodeOutcome::PaymentRequired;
        assert!(
            o.suggests_qr_instead(),
            "没有支付流程时必须给出一条真实可行的出路，而扫码不经过 sendCode"
        );
        assert!(!o.already_done());
    }

    /// 「服务端说已经登录了」是正常可达状态，不是异常。
    ///
    /// 不这样会怎样：grammers 在这里 panic「should not have logged in yet」。
    /// 但这个状态很容易达到——上次登录其实成功了、只是登录态没存住，于是又来
    /// 发一次码。当成异常就是崩，当成已登录就能直接继续。
    #[test]
    fn already_authorized_is_not_an_error() {
        let o = SendCodeOutcome::AlreadyAuthorized;
        assert!(o.already_done(), "应当直接当成已登录继续，而不是再要验证码");
        assert!(!o.suggests_qr_instead());
    }

    /// 三个 variant 都有归宿，不存在「不该发生」的分支。
    ///
    /// 不这样会怎样：漏掉任何一个就退回 grammers 那种「它认为不会发生所以
    /// panic」的状态，而我们绕过它的全部意义就在于每个分支都有处理。
    #[test]
    fn every_outcome_is_classified() {
        for o in [
            SendCodeOutcome::Sent(CodeShape {
                length: 5,
                via: CodeDelivery::App,
            }),
            SendCodeOutcome::AlreadyAuthorized,
            SendCodeOutcome::PaymentRequired,
        ] {
            // 两个判断合起来必须能把三种结果区分开
            let tag = (o.already_done(), o.suggests_qr_instead());
            match &o {
                SendCodeOutcome::Sent(_) => assert_eq!(tag, (false, false)),
                SendCodeOutcome::AlreadyAuthorized => assert_eq!(tag, (true, false)),
                SendCodeOutcome::PaymentRequired => assert_eq!(tag, (false, true)),
            }
        }
    }

    /// 单词与短句必须被标成**非数字**，且不给固定长度。
    ///
    /// 不这样会怎样：输入框按纯数字写死，用户遇到单词码时根本输不进去——
    /// 数字键盘打不出字母。这两种类型是官方协议里真实存在的
    /// （sentCodeTypeSmsWord / sentCodeTypeSmsPhrase）。
    #[test]
    fn word_and_phrase_are_not_numeric() {
        for k in [SentCodeKind::Word, SentCodeKind::Phrase] {
            assert!(!k.is_numeric(), "{k:?} 不是数字码");
            assert!(k.fixed_length().is_none(), "{k:?} 不该有固定输入格数");
        }
    }

    /// 数字码要给出位数，供界面分格；闪信与未接来电不给。
    ///
    /// 不这样会怎样：把未接来电的 length（那是「号码后几位」）当成输入格数，
    /// 会画出错误数量的输入框。
    #[test]
    fn numeric_kinds_expose_their_length() {
        assert_eq!(SentCodeKind::App { length: 5 }.fixed_length(), Some(5));
        assert_eq!(SentCodeKind::Sms { length: 6 }.fixed_length(), Some(6));
        assert_eq!(SentCodeKind::Call { length: 6 }.fixed_length(), Some(6));
        // 未接来电是数字，但它的 length 不是输入格数
        let mc = SentCodeKind::MissedCall { length: 4 };
        assert!(mc.is_numeric());
        assert!(mc.fixed_length().is_none(), "不能把「号码后几位」当输入格数");
        // 闪信既不是可输入的数字格，也没有长度
        assert!(!SentCodeKind::FlashCall.is_numeric());
        assert!(SentCodeKind::FlashCall.fixed_length().is_none());
    }

    /// 送达方式要映射到正确的去处提示。
    ///
    /// 不这样会怎样：App 类型没映射到 CodeDelivery::App，就丢掉了「去其他已
    /// 登录客户端查看」那条提示——而那正是用户白等一小时的那个坑。
    #[test]
    fn delivery_mapping_preserves_where_to_look() {
        let app = shape_of(&SentCodeKind::App { length: 5 });
        assert_eq!(app.via, CodeDelivery::App);
        assert!(
            app.via.requires_other_client(),
            "必须保留「去其他客户端查看」这条提示"
        );
        assert_eq!(app.length, 5);

        assert_eq!(shape_of(&SentCodeKind::Sms { length: 6 }).via, CodeDelivery::Sms);
        assert_eq!(shape_of(&SentCodeKind::Call { length: 6 }).via, CodeDelivery::Call);
        assert_eq!(shape_of(&SentCodeKind::FlashCall).via, CodeDelivery::FlashCall);
        // 单词/短句没有对应的「去哪找」，用 Unknown 让界面含糊说明而不是猜一种
        assert_eq!(shape_of(&SentCodeKind::Word).length, 0, "不要按位数限制输入");
        assert_eq!(shape_of(&SentCodeKind::Phrase).length, 0);
    }
}
