//! 登录状态机。
//!
//! 口径来自 `docs/research/appendix/telegram-remote-prototype.html` 第 10 节。
//! **这一层不碰网络**：它只决定「现在处于哪一步、界面该显示什么、下一步允许做
//! 什么」，真正的 RPC 由驱动发。
//!
//! # 为什么先做纯状态机
//!
//! 这些规则里最容易写错的几条，与协议细节无关，而与「把某个状态归错类」有关：
//!
//! - **「需要切换数据中心」必须是中间态。** 做成失败会让一次本来会成功的登录被
//!   用户自己打断——他看到「失败」就去点重试，而正确动作是继续等。
//! - **二维码过期不是错误。** 界面只换一张图，不报错、不清空进度。报错会让用户
//!   以为哪里坏了，而这是正常的 30 秒轮换。
//! - **验证码错误不能退回上一步。** 把手机号也清掉是常见的坏实现，用户得重填两遍。
//! - **`FLOOD_WAIT` 期间相关按钮必须禁用。** 让按钮可点然后必然失败等于骗用户，
//!   而限流下重试会把等待时间越点越长。
//! - **代理 scheme 不受支持是配置错误，不是网络故障。** 正确动作是改配置而不是
//!   重试；把它归成网络问题会让用户去查一个不存在的网络故障。
//!
//! 做成不依赖网络的状态机，才能在没有账号的条件下用单测逐条钉住。
//!
//! # 实测支撑
//!
//! 已在本机实测：直连数据中心超时、经 `socks5://` 约 3.3s 通；
//! `auth.exportLoginToken`（扫码第一步）在只有 api_id 的情况下就能拿到令牌。
//! 所以「连通性自检 → 扫码」这条路径的形状是有实测依据的。
//!
//! 仍**未**实测：手机号路径、2FA、以及登录成功之后的一切（需要能收验证码的账号）。

use crate::telegram::appid::{AppId, AppIdError, SessionMismatch};

/// 登录方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// 扫码：手机上用官方客户端扫一张二维码。
    ///
    /// 桌面端首选——不用在电脑上输手机号，也不经过短信。
    QrCode,
    /// 手机号 + 验证码。
    ///
    /// 移动端首选：同一台设备上扫自己显示的码是做不到的。
    PhoneCode,
    /// 复用本机 Telegram Desktop 的登录态（读 `tdata`）。
    ///
    /// **本期只占位，没有实现**，见 [`LoginMethod::is_implemented`]。
    ///
    /// 为什么占位而不实现：现成的 `grammpars` 有一条硬伤——它依赖
    /// `rusqlite` 且写死 `features = ["bundled"]`，没有开关可关，而 bundled
    /// 会去编译 SQLite 的 C 代码。本项目选 grammers 的核心理由之一正是整条
    /// 依赖树里没有 C 工具链（TDLib 就是因此被否决的），引它进来等于把刚
    /// 赶出门的 C 依赖从窗户请回来，Android 交叉编译也要重新验证。
    ///
    /// 那么为什么不自己写解析：解 `tdata` 要先派生出解开 auth key 的主密钥，
    /// 而这个派生函数的迭代次数在现有资料之间就对不上（一处记 100000，
    /// 公开资料记 400，另有旧格式用 4000）。这个常数错了不会报错，只会
    /// 静默解不开或解错——而本机没有真实 `tdata` 可供判定谁对。在拿到能
    /// 验证的样本之前写它，等于交付一段无法判断对错的密码学代码。
    ///
    /// 所以这里只留枚举位，**不留半成品解析代码、不留注释掉的依赖**。
    TdataImport,
}

impl LoginMethod {
    /// 这个方式现在能不能真的用。
    ///
    /// 有这个函数而不是「先不加那个枚举位」，是为了让占位不变成陷阱：
    /// 调用方想路由到 [`LoginMethod::TdataImport`] 时，必须先撞上这里返回的
    /// `false`。
    ///
    /// 不这样会怎样：界面把它当成一个正常选项渲染出来，用户点了之后什么也
    /// 不发生——既没有进度也没有错误，他会反复点，然后以为程序坏了。
    #[must_use]
    pub const fn is_implemented(&self) -> bool {
        match self {
            Self::QrCode | Self::PhoneCode => true,
            // 占位，理由见 variant 上的注释
            Self::TdataImport => false,
        }
    }
}

/// 二维码登录令牌的展示信息。
///
/// **不含令牌本体**：那串字节等同于一次登录凭据，界面只需要拿它渲染二维码，
/// 而状态机不需要持有它。让它流经这里只会多一处泄露面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QrToken {
    /// 还有多少秒过期。到点自动换一张，不报错。
    pub expires_in_secs: u32,
}

/// 验证码的形状，由服务端给出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeShape {
    /// 几位。**必须按服务端给的渲染**，写死 5 位会在其它发送方式下错位。
    pub length: u8,
    /// 码是怎么发的。
    pub via: CodeDelivery,
}

/// 验证码的送达方式。
///
/// 必须如实显示：写死「短信」会让用户在短信里白找——Telegram 经常发到
/// 已登录的官方客户端里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeDelivery {
    /// 发到已登录的官方客户端。
    App,
    /// 短信。
    Sms,
    /// 电话语音播报。
    Call,
    /// 服务端没说清楚。界面此时应当含糊地说「验证码已发送」而不是猜一种。
    Unknown,
}

/// 登录流程的当前状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginState {
    /// 还没开始。
    Idle,
    /// 正在自检能不能连上（直连或经代理）。
    CheckingConnectivity,
    /// 连不上，且需要用户配代理。
    ///
    /// **这不是故障，是一个待配置的前提**：界面用 warn 而不是 danger，
    /// 并且要就地展开代理表单，而不是把它藏在折叠的「高级设置」里——
    /// 用户在这一刻正需要它。
    NeedsProxy {
        /// 自检时遇到的具体问题，用于给出针对性的提示。
        reason: ConnectivityIssue,
    },
    /// 等用户选登录方式。
    AwaitingMethod,
    /// 二维码已显示，等手机端确认。
    AwaitingQrScan(QrToken),
    /// 正在切换数据中心。
    ///
    /// **中间态，不是失败。** 界面显示「正在切换数据中心…」并继续等；
    /// 做成失败会让一次本来会成功的登录被用户打断。
    SwitchingDc,
    /// 等用户输手机号。
    AwaitingPhone,
    /// 已请求验证码，等用户输入。
    AwaitingCode(CodeShape),
    /// 等用户输两步验证的云密码。
    AwaitingPassword {
        /// 服务端给的密码提示；没有则为 `None`。
        hint: Option<String>,
    },
    /// 被限流，等倒计时结束。
    ///
    /// 期间相关按钮必须禁用：可点然后必然失败等于骗用户，而限流下重试会把
    /// 等待时间越点越长。
    RateLimited {
        /// 还需等待的秒数。
        secs: u32,
        /// 等待结束后回到哪一步。
        ///
        /// 存着它才能「等完继续」而不是把用户扔回起点。
        resume: Box<LoginState>,
    },
    /// 登录成功。
    LoggedIn,
    /// 失败，且**不是**靠等待或重试能解决的。
    Failed(LoginError),
}

/// 连通性自检发现的问题。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectivityIssue {
    /// 直连超时。本机实测就是这种。
    DirectTimedOut,
    /// 用户填的代理地址不合法。
    ///
    /// 与「连不上」分开：这一种是**配置错误**，正确动作是改地址而不是等待。
    BadProxy(crate::telegram::proxy::ProxyError),
    /// 代理地址合法但连不上。
    ProxyUnreachable,
}

/// 登录失败的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    /// 应用身份有问题（含 `API_ID_PUBLISHED_FLOOD`）。
    #[error("{0}")]
    AppId(#[from] AppIdError),
    /// 已保存的登录态与当前 api_id 不匹配。
    #[error("{0}")]
    SessionMismatch(#[from] SessionMismatch),
    /// 手机号格式不对，或服务端说这个号不能用。
    #[error("这个手机号无法用于登录")]
    InvalidPhone,
    /// 云密码错了。
    ///
    /// **停在本步重输**，不退回上一步。
    #[error("两步验证密码不正确")]
    WrongPassword,
    /// 这个账号被封禁。
    #[error("该账号已被 Telegram 封禁")]
    Banned,
    /// 用户主动取消。
    #[error("已取消登录")]
    Cancelled,
    /// 网络层问题（不含「需要配代理」，那是 `NeedsProxy`）。
    #[error("网络错误: {0}")]
    Network(String),
}

impl LoginError {
    /// 界面该不该给「重试」。
    ///
    /// 只有网络错误值得重试。其余都是「改点什么再来」——给重试按钮等于
    /// 让用户反复点一件必然失败的事。
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::Network(_))
    }
}

impl LoginState {
    /// 是不是终态。
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::LoggedIn | Self::Failed(_))
    }

    /// 界面上「继续 / 提交」这类主按钮该不该可点。
    ///
    /// 限流期间一律不可点。等待态下可点会让用户把等待时间越点越长，
    /// 而且他会以为是自己点得不对。
    #[must_use]
    pub const fn accepts_input(&self) -> bool {
        matches!(
            self,
            Self::AwaitingMethod
                | Self::AwaitingPhone
                | Self::AwaitingCode(_)
                | Self::AwaitingPassword { .. }
        )
    }

    /// 是不是一个「正在进行、用户只需要等」的中间态。
    ///
    /// 界面据此显示进度而不是错误。`SwitchingDc` 归在这里是关键一条。
    #[must_use]
    pub const fn is_in_progress(&self) -> bool {
        matches!(
            self,
            Self::CheckingConnectivity | Self::AwaitingQrScan(_) | Self::SwitchingDc
        )
    }

    /// 界面该不该给「重试」按钮。
    ///
    /// 限流态**不给**：它会自动恢复，给了只会让用户越点越糟。
    #[must_use]
    pub const fn offers_retry(&self) -> bool {
        match self {
            Self::Failed(e) => e.retryable(),
            _ => false,
        }
    }
}

/// 登录流程。
///
/// 纯内存、纯同步：不持有连接也不起线程，因此每条规则都能被单测钉住。
#[derive(Debug)]
pub struct LoginFlow {
    state: LoginState,
    app: AppId,
    /// 已经换过几张二维码。
    ///
    /// 只用于诊断（「换了 20 张还没扫」通常意味着用户根本没看到那张图）。
    /// 不用来限制次数：过期换图是正常行为，不该因为换得多就中断登录。
    qr_refreshes: u32,
}

impl LoginFlow {
    /// 用给定的应用身份开始一个流程。
    #[must_use]
    pub fn new(app: AppId) -> Self {
        Self {
            state: LoginState::Idle,
            app,
            qr_refreshes: 0,
        }
    }

    /// 当前状态。
    #[must_use]
    pub fn state(&self) -> &LoginState {
        &self.state
    }

    /// 本次流程用的应用身份。
    #[must_use]
    pub fn app(&self) -> &AppId {
        &self.app
    }

    /// 换过几张二维码。
    #[must_use]
    pub const fn qr_refreshes(&self) -> u32 {
        self.qr_refreshes
    }

    /// 开始连通性自检。
    pub fn begin(&mut self) {
        self.state = LoginState::CheckingConnectivity;
    }

    /// 自检通过，可以选登录方式了。
    pub fn connectivity_ok(&mut self) {
        self.state = LoginState::AwaitingMethod;
    }

    /// 自检没通过，需要用户配代理。
    pub fn needs_proxy(&mut self, reason: ConnectivityIssue) {
        self.state = LoginState::NeedsProxy { reason };
    }

    /// 二维码已就绪（或换了新的一张）。
    ///
    /// **过期换图走这同一个入口**，所以不会经过任何错误态——界面只看到图变了。
    pub fn qr_ready(&mut self, token: QrToken) {
        if matches!(self.state, LoginState::AwaitingQrScan(_)) {
            self.qr_refreshes += 1;
        }
        self.state = LoginState::AwaitingQrScan(token);
    }

    /// 服务端要求切换数据中心。
    ///
    /// **这不是失败**：转入中间态继续等。
    pub fn switching_dc(&mut self) {
        self.state = LoginState::SwitchingDc;
    }

    /// 用户选了手机号登录，等他输号码。
    pub fn awaiting_phone(&mut self) {
        self.state = LoginState::AwaitingPhone;
    }

    /// 验证码已发出，等用户输入。
    pub fn code_sent(&mut self, shape: CodeShape) {
        self.state = LoginState::AwaitingCode(shape);
    }

    /// 验证码错了。
    ///
    /// **停在本步**，保留验证码的形状（几位、怎么发的），不退回输手机号。
    /// 退回去是常见的坏实现：用户得把号码重填一遍。
    ///
    /// 返回是否真的停在了本步；不在输验证码这一步时不做任何改动。
    pub fn code_rejected(&mut self) -> bool {
        matches!(self.state, LoginState::AwaitingCode(_))
    }

    /// 还需要两步验证的云密码。
    pub fn needs_password(&mut self, hint: Option<String>) {
        self.state = LoginState::AwaitingPassword { hint };
    }

    /// 云密码错了。
    ///
    /// 同样**停在本步重输**，并保留服务端给的提示。
    pub fn password_rejected(&mut self) -> bool {
        matches!(self.state, LoginState::AwaitingPassword { .. })
    }

    /// 被限流，转入等待态。
    ///
    /// 记下「等完回到哪一步」，这样倒计时结束能继续，而不是把用户扔回起点。
    /// 已经在等待态时只更新秒数，不把等待态自己套进 resume 里——那会让
    /// 嵌套越来越深，且 resume 指向另一个等待态毫无意义。
    pub fn rate_limited(&mut self, secs: u32) {
        let resume = match std::mem::replace(&mut self.state, LoginState::Idle) {
            LoginState::RateLimited { resume, .. } => resume,
            other => Box::new(other),
        };
        self.state = LoginState::RateLimited { secs, resume };
    }

    /// 时间推进 `secs` 秒。倒计时走完则回到被打断的那一步。
    ///
    /// 取外部传入的流逝秒数而不是自己读时钟，是为了让这条规则能被确定性地测到。
    ///
    /// 返回是否已恢复。
    pub fn tick(&mut self, secs: u32) -> bool {
        // 先判断再取走。用 `mem::replace` 无条件取出状态，再在 else 分支里
        // 「打算放回去」是一个真实踩过的坑：那个分支拿不到原值，状态就被悄悄
        // 重置成 Idle 了——现象是「登录页突然回到第一步」，而 tick 是每秒都在
        // 调的，几乎必然发生。单测 tick_outside_rate_limit_preserves_state
        // 就是为它写的，写出来的第一版正是这样错的。
        if !matches!(self.state, LoginState::RateLimited { .. }) {
            return false;
        }
        let LoginState::RateLimited { secs: left, resume } =
            std::mem::replace(&mut self.state, LoginState::Idle)
        else {
            // 上面刚判断过，这里到不了；不 panic 是因为 omy 里不允许 panic
            return false;
        };
        let rest = left.saturating_sub(secs);
        if rest == 0 {
            self.state = *resume;
            true
        } else {
            self.state = LoginState::RateLimited { secs: rest, resume };
            false
        }
    }

    /// 登录成功。
    pub fn logged_in(&mut self) {
        self.state = LoginState::LoggedIn;
    }

    /// 失败，进终态。
    pub fn fail(&mut self, err: LoginError) {
        self.state = LoginState::Failed(err);
    }

    /// 界面该不该引导用户去填自己的 api_id。
    ///
    /// 只在「用内置身份撞上 `API_ID_PUBLISHED_FLOOD`」时为真。
    /// 这条自救路径必须存在：选内置官方 AppID 降低的是该错误的概率，
    /// 不是消掉它。
    #[must_use]
    pub fn suggests_custom_app_id(&self) -> bool {
        match &self.state {
            LoginState::Failed(LoginError::AppId(e)) => {
                e.suggests_custom_app_id(self.app.is_builtin())
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> LoginFlow {
        LoginFlow::new(AppId::builtin())
    }

    /// 「需要切换数据中心」必须是进行中，不能是失败。
    ///
    /// 不这样会怎样：界面显示「登录失败」，用户点了取消或重试，而这次登录
    /// 本来再等一两秒就成了——一个被我们自己打断的成功。
    #[test]
    fn switching_dc_is_progress_not_failure() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.qr_ready(QrToken { expires_in_secs: 30 });
        f.switching_dc();

        assert_eq!(f.state(), &LoginState::SwitchingDc);
        assert!(f.state().is_in_progress(), "必须是「正在进行」");
        assert!(!f.state().is_terminal(), "不能是终态");
        assert!(!f.state().offers_retry(), "不该给重试按钮");
        assert!(!f.state().accepts_input(), "此时不需要用户输入任何东西");
    }

    /// 二维码过期只换图，不经过任何错误态。
    ///
    /// 不这样会怎样：每 30 秒弹一次「登录失败」，用户以为坏了就放弃了——
    /// 而这只是正常的令牌轮换。
    #[test]
    fn qr_expiry_only_swaps_the_image() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.qr_ready(QrToken { expires_in_secs: 30 });
        assert_eq!(f.qr_refreshes(), 0, "第一张不算「换」");

        for i in 1..=3 {
            f.qr_ready(QrToken { expires_in_secs: 30 });
            assert_eq!(f.qr_refreshes(), i);
            assert!(
                matches!(f.state(), LoginState::AwaitingQrScan(_)),
                "换图后仍停在等扫码，不能变成失败"
            );
            assert!(!f.state().is_terminal());
        }
        // 换得多不该中断登录：过期换图是正常行为
        assert!(f.state().is_in_progress());
    }

    /// 验证码错了要停在本步，不能退回输手机号。
    ///
    /// 不这样会怎样：用户得把手机号重填一遍——这是很常见的坏实现，
    /// 而且他会怀疑是不是号码填错了。
    #[test]
    fn wrong_code_stays_on_the_code_step() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.awaiting_phone();
        let shape = CodeShape { length: 6, via: CodeDelivery::App };
        f.code_sent(shape);

        assert!(f.code_rejected(), "输错时应当确认「停在本步」");
        assert_eq!(
            f.state(),
            &LoginState::AwaitingCode(shape),
            "状态必须原样保留，含验证码位数与送达方式"
        );
        assert!(f.state().accepts_input(), "还要接着输");
        assert_ne!(f.state(), &LoginState::AwaitingPhone, "绝不能退回输手机号");
    }

    /// 云密码错了同样停在本步，且保留服务端给的提示。
    ///
    /// 不这样会怎样：提示丢了，用户失去唯一的线索（那句提示往往是他自己设的）。
    #[test]
    fn wrong_password_stays_and_keeps_the_hint() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.needs_password(Some(String::from("我的猫的名字")));

        assert!(f.password_rejected());
        assert_eq!(
            f.state(),
            &LoginState::AwaitingPassword {
                hint: Some(String::from("我的猫的名字"))
            },
            "提示必须保留"
        );
        assert!(f.state().accepts_input());
    }

    /// 验证码的位数与送达方式必须按服务端给的来。
    ///
    /// 不这样会怎样：写死 5 位会在其它发送方式下错位；写死「短信」会让用户
    /// 在短信里白找——Telegram 经常发到已登录的官方客户端里。
    #[test]
    fn code_shape_comes_from_the_server() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        for (len, via) in [
            (5u8, CodeDelivery::Sms),
            (6, CodeDelivery::App),
            (6, CodeDelivery::Call),
            (5, CodeDelivery::Unknown),
        ] {
            f.code_sent(CodeShape { length: len, via });
            let LoginState::AwaitingCode(s) = f.state() else {
                panic!("应当停在输验证码");
            };
            assert_eq!(s.length, len);
            assert_eq!(s.via, via);
        }
    }

    /// 限流期间不接受输入，倒计时走完回到被打断的那一步。
    ///
    /// 不这样会怎样：按钮可点然后必然失败等于骗用户，而限流下重试会把等待
    /// 时间越点越长；而若不记住「回到哪一步」，等完会把用户扔回起点重来。
    #[test]
    fn rate_limit_disables_input_and_resumes_where_it_stopped() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.awaiting_phone();
        f.rate_limited(30);

        assert!(!f.state().accepts_input(), "等待期间不该接受输入");
        assert!(!f.state().offers_retry(), "自动恢复的事不该给重试按钮");
        assert!(!f.state().is_terminal());

        assert!(!f.tick(10), "还没到点");
        assert!(
            matches!(f.state(), LoginState::RateLimited { secs: 20, .. }),
            "倒计时要递减，界面才能显示剩余秒数：{:?}",
            f.state()
        );
        assert!(f.tick(20), "到点应当恢复");
        assert_eq!(f.state(), &LoginState::AwaitingPhone, "要回到被打断的那一步");
        assert!(f.state().accepts_input());
    }

    /// 连续限流不该把等待态套进等待态。
    ///
    /// 不这样会怎样：resume 链越套越深，最后恢复到一个中间的等待态上，
    /// 表现是「等完了又在等」。
    #[test]
    fn repeated_rate_limits_do_not_nest() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.awaiting_phone();
        f.rate_limited(10);
        f.rate_limited(60);

        let LoginState::RateLimited { secs, resume } = f.state() else {
            panic!("应当在等待态");
        };
        assert_eq!(*secs, 60, "秒数应当被更新");
        assert_eq!(
            **resume,
            LoginState::AwaitingPhone,
            "resume 必须还是那一步，不能是另一个等待态"
        );
        assert!(f.tick(60));
        assert_eq!(f.state(), &LoginState::AwaitingPhone);
    }

    /// 不在等待态时 tick 什么也不做，且不能把状态弄丢。
    ///
    /// 不这样会怎样：实现里用 `mem::replace` 取状态，忘了放回去就会把用户的
    /// 进度悄悄重置成 Idle——而现象是「登录页突然回到第一步」。
    #[test]
    fn tick_outside_rate_limit_preserves_state() {
        let mut f = flow();
        f.begin();
        f.connectivity_ok();
        f.awaiting_phone();
        assert!(!f.tick(5));
        assert_eq!(f.state(), &LoginState::AwaitingPhone, "状态不能被弄丢");
    }

    /// 直连不通是「待配置」，不是故障，且要能区分三种成因。
    ///
    /// 不这样会怎样：本机实测直连就是超时，若把它当故障用红色报错，
    /// 每个用户第一次登录都会以为程序坏了——而他只需要填个代理。
    /// 三种成因也必须分开：地址写错了要改地址，连不上要查代理本身。
    #[test]
    fn needs_proxy_is_a_precondition_not_a_fault() {
        let mut f = flow();
        f.begin();
        assert!(f.state().is_in_progress(), "自检期间是进行中");

        f.needs_proxy(ConnectivityIssue::DirectTimedOut);
        assert_eq!(
            f.state(),
            &LoginState::NeedsProxy {
                reason: ConnectivityIssue::DirectTimedOut
            }
        );
        assert!(!f.state().is_terminal(), "不是终态：填了代理就能继续");
        assert!(!f.state().offers_retry(), "该做的是填代理，不是重试");

        // 配置错误与「连不上」是两回事
        f.needs_proxy(ConnectivityIssue::BadProxy(
            crate::telegram::proxy::ProxyError::MissingPort,
        ));
        assert!(matches!(
            f.state(),
            LoginState::NeedsProxy {
                reason: ConnectivityIssue::BadProxy(_)
            }
        ));
        f.needs_proxy(ConnectivityIssue::ProxyUnreachable);
        assert!(matches!(
            f.state(),
            LoginState::NeedsProxy {
                reason: ConnectivityIssue::ProxyUnreachable
            }
        ));
    }

    /// 只有网络错误给重试，其余都不给。
    ///
    /// 不这样会怎样：给密码错误、账号封禁配一个重试按钮，用户会反复点一件
    /// 必然失败的事，而真正该做的（改密码 / 换账号）没人告诉他。
    #[test]
    fn only_network_failures_offer_retry() {
        let mut f = flow();
        f.fail(LoginError::Network(String::from("timeout")));
        assert!(f.state().offers_retry());
        assert!(f.state().is_terminal());

        for e in [
            LoginError::WrongPassword,
            LoginError::InvalidPhone,
            LoginError::Banned,
            LoginError::Cancelled,
            LoginError::AppId(AppIdError::PublishedFlood),
        ] {
            f.fail(e.clone());
            assert!(!f.state().offers_retry(), "{e:?} 不该给重试按钮");
        }
    }

    /// 撞上 `API_ID_PUBLISHED_FLOOD` 时要引导用户填自己的 api_id。
    ///
    /// 不这样会怎样：用户永远卡在登录页，而出路（填一对自己的 api_id）就在
    /// 设置里。选内置官方 AppID 降低的是这个错误的概率，不是消掉它，
    /// 所以这条自救路径必须存在。
    #[test]
    fn published_flood_guides_to_custom_app_id() {
        let mut f = LoginFlow::new(AppId::builtin());
        f.fail(LoginError::AppId(AppIdError::PublishedFlood));
        assert!(f.suggests_custom_app_id(), "用内置值撞上限流要引导用户去填自己的");

        // 用户已经在用自己的那对时不该再劝他改——那会让他两对之间换来换去
        let mine = AppId::custom(4242, "0123456789abcdef0123456789abcdef").expect("合法");
        let mut g = LoginFlow::new(mine);
        g.fail(LoginError::AppId(AppIdError::PublishedFlood));
        assert!(!g.suggests_custom_app_id());

        // 别的失败原因不触发这条引导
        let mut h = LoginFlow::new(AppId::builtin());
        h.fail(LoginError::WrongPassword);
        assert!(!h.suggests_custom_app_id());
    }

    /// 换了 api_id 导致的失败要能与普通登录失败区分开。
    ///
    /// 不这样会怎样：只报「登录已失效」，用户会以为账号出了问题，
    /// 跑去官方客户端查会话、怀疑被盗号，而真正原因是他刚改了一个数字。
    #[test]
    fn session_mismatch_failure_explains_itself() {
        let mut f = flow();
        let mismatch = SessionMismatch {
            saved_api_id: 2040,
            current_api_id: 999,
        };
        f.fail(LoginError::SessionMismatch(mismatch));

        let LoginState::Failed(e) = f.state() else {
            panic!("应当是失败态");
        };
        let text = e.to_string();
        assert!(text.contains("2040") && text.contains("999"));
        assert!(text.contains("与账号本身无关"), "必须澄清不是账号的问题");
        assert!(!e.retryable(), "重试无用，要重新登录");
    }

    /// 进行中的状态一律不接受输入。
    ///
    /// 不这样会怎样：界面在「正在切换数据中心」时还让用户点提交，
    /// 点下去必然失败，而他会以为自己操作错了。
    #[test]
    fn in_progress_states_never_accept_input() {
        let mut f = flow();
        for setup in [
            (|f: &mut LoginFlow| f.begin()) as fn(&mut LoginFlow),
            |f: &mut LoginFlow| f.qr_ready(QrToken { expires_in_secs: 30 }),
            |f: &mut LoginFlow| f.switching_dc(),
        ] {
            setup(&mut f);
            assert!(f.state().is_in_progress(), "{:?} 应当是进行中", f.state());
            assert!(
                !f.state().accepts_input(),
                "{:?} 不该接受输入",
                f.state()
            );
        }
    }

    /// 需要用户输入的状态一律接受输入，且都不是终态。
    #[test]
    fn input_states_accept_input_and_are_not_terminal() {
        let mut f = flow();
        for setup in [
            (|f: &mut LoginFlow| f.connectivity_ok()) as fn(&mut LoginFlow),
            |f: &mut LoginFlow| f.awaiting_phone(),
            |f: &mut LoginFlow| f.code_sent(CodeShape { length: 5, via: CodeDelivery::Sms }),
            |f: &mut LoginFlow| f.needs_password(None),
        ] {
            setup(&mut f);
            assert!(f.state().accepts_input(), "{:?} 应当接受输入", f.state());
            assert!(!f.state().is_terminal());
            assert!(!f.state().is_in_progress());
        }
    }

    /// 占位的登录方式必须被明确标成「还不能用」。
    ///
    /// 不这样会怎样：界面把 tdata 导入当成正常选项渲染出来，用户点了之后
    /// 什么也不发生——没有进度也没有错误，他会反复点然后以为程序坏了。
    /// 而这个枚举位存在的意义只是「将来会有」，不是「现在能用」。
    #[test]
    fn placeholder_method_is_marked_unimplemented() {
        assert!(LoginMethod::QrCode.is_implemented());
        assert!(LoginMethod::PhoneCode.is_implemented());
        assert!(
            !LoginMethod::TdataImport.is_implemented(),
            "本期没有实现 tdata 导入，不能让它看起来可用"
        );
    }

    /// 登录成功是终态，且不再接受输入。
    #[test]
    fn logged_in_is_terminal() {
        let mut f = flow();
        f.logged_in();
        assert_eq!(f.state(), &LoginState::LoggedIn);
        assert!(f.state().is_terminal());
        assert!(!f.state().accepts_input());
        assert!(!f.state().offers_retry());
    }
}
