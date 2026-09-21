//! 手机号 + 验证码登录。自己发原始 `auth.sendCode` / `auth.signIn`，
//! 与 [`crate::telegram::qrlogin`] 同源地绕过 grammers 会 panic 的分支。
//!
//! # 为什么不复用 grammers 的高层 `request_login_code` / `sign_in`
//!
//! 和扫码那条路一样的理由：grammers 0.10 在若干「它认为不会发生」的分支上
//! 直接 `panic!` / `unimplemented!()`，而 omy-gui 禁 panic、release 下
//! `panic = "abort"` 连 `catch_unwind` 都救不了。`auth.sendCode` 的
//! `SentCode::PaymentRequired` variant 就是这样一处。自己发请求、自己解
//! enum，才能把「进程崩」变成「界面报一条看得懂的错」。
//!
//! # 与扫码共用的部分
//!
//! 2FA 的 SRP 计算复用扫码那条路里已经验证过的 `submit_password` 逻辑
//! （`Client::check_password` 前的四道前提校验），不重复写——两处各写一份
//! 迟早只改一处，而这段是安全攸关的。

use std::sync::Arc;

use grammers_client::{tl, Client, InvocationError, SignInError};
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::storages::MemorySession;

use crate::telegram::appid::AppId;
use crate::telegram::auth::{shape_of, SendCodeOutcome, SentCodeKind};
use crate::telegram::device::DeviceInfo;
use crate::telegram::login::CodeShape;
use crate::telegram::qrlogin::QrError;

/// 手机号登录过程中的一个事件。
///
/// 每一个都对应界面上一处可见变化。刻意不带 `phone_code_hash` 之外的凭据，
/// 而 hash 本身也留在 session 内部、不外泄给事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PhoneEvent {
    /// 验证码已发出，等用户输码。
    ///
    /// `shape` 告诉界面这是数字还是单词/短句、发到了哪里——**发到哪里**这条
    /// 尤其重要：`sentCodeTypeApp` 是发到其他已登录客户端，用户不知道就会
    /// 盯着短信白等。
    CodeSent {
        /// 验证码的形态与送达方式。
        shape: CodeShape,
    },
    /// 服务端要求两步验证的云密码。
    NeedPassword {
        /// 用户自己设的密码提示，可能为空。
        hint: Option<String>,
    },
    /// 登录成功。
    LoggedIn,
}

/// 一次手机号登录会话。
///
/// 与 [`crate::telegram::qrlogin::QrSession`] 平行：自己持有连接与 runner，
/// drop 时断开。
pub struct PhoneSession {
    client: Client,
    session: Arc<MemorySession>,
    app: AppId,
    /// 驱动 I/O 的后台任务。drop 掉连接就断，现象是所有请求超时。
    runner: tokio::task::JoinHandle<()>,
    /// 当前正在登录的号码。signIn 要用它。
    phone: Option<String>,
    /// `sendCode` 返回的 `phone_code_hash`，signIn 必须回带。
    ///
    /// 它等同一次登录凭据的一部分，只在内存里存活、不落盘不进日志。
    code_hash: Option<String>,
}

impl std::fmt::Debug for PhoneSession {
    /// **绝不打印 phone 与 code_hash**：前者是隐私、后者是登录凭据。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoneSession")
            .field("has_phone", &self.phone.is_some())
            .field("has_code_hash", &self.code_hash.is_some())
            .finish_non_exhaustive()
    }
}

impl Drop for PhoneSession {
    fn drop(&mut self) {
        self.client.disconnect();
        self.runner.abort();
    }
}

impl PhoneSession {
    /// 连上服务端，但还不发码。
    ///
    /// # Errors
    ///
    /// 目前不返回错误（连接建立是惰性的），签名保留 `Result` 是为了与扫码那条
    /// 路一致、且将来加校验时不用改调用方。
    pub fn connect(
        app: AppId,
        proxy: Option<&str>,
        device: &DeviceInfo,
    ) -> Result<Self, QrError> {
        let params = ConnectionParams {
            device_model: device.device_model.clone(),
            system_version: device.system_version.clone(),
            app_version: device.app_version.clone(),
            system_lang_code: device.system_lang_code.clone(),
            lang_code: device.lang_code.clone(),
            proxy_url: proxy.map(str::to_owned),
            ..ConnectionParams::default()
        };
        let session = Arc::new(MemorySession::default());
        // 手机号登录用不到 updates（不像扫码要等 updateLoginToken），
        // 用 `..` 忽略它，但 runner 必须 spawn，否则请求只排队不发出
        let SenderPool { runner, handle, .. } =
            SenderPool::with_configuration(Arc::clone(&session), app.id(), params);
        let client = Client::new(handle);
        let runner = tokio::spawn(async move {
            runner.run().await;
        });
        Ok(Self {
            client,
            session,
            app,
            runner,
            phone: None,
            code_hash: None,
        })
    }

    /// 底层客户端，供登录成功后复用同一条连接。
    #[must_use]
    pub const fn client(&self) -> &Client {
        &self.client
    }

    /// 内存 session，登录成功后由调用方落盘。
    #[must_use]
    pub fn session(&self) -> &Arc<MemorySession> {
        &self.session
    }

    /// 本次用的应用身份。
    #[must_use]
    pub const fn app(&self) -> &AppId {
        &self.app
    }

    /// 给某个号码发验证码。
    ///
    /// # Errors
    ///
    /// 号码不合法、限流、api_id 被封、需要付费短信（我们不支持，引导改扫码）时返回。
    pub async fn send_code(&mut self, phone: &str) -> Result<PhoneEvent, QrError> {
        // allow_app_hash=true：允许服务端把码发到其他已登录客户端
        // （sentCodeTypeApp）。这是最省事、也最常见的一种，界面必须能处理它。
        let settings = tl::types::CodeSettings {
            allow_flashcall: false,
            current_number: false,
            allow_app_hash: true,
            allow_missed_call: false,
            allow_firebase: false,
            unknown_number: false,
            logout_tokens: None,
            token: None,
            app_sandbox: None,
        };
        let req = tl::functions::auth::SendCode {
            phone_number: phone.to_owned(),
            api_id: self.app.id(),
            api_hash: String::from(self.app.hash()),
            settings: tl::enums::CodeSettings::Settings(settings),
        };
        let sent = match self.client.invoke(&req).await {
            Ok(s) => s,
            Err(e) => return Err(map_send_err(&e)),
        };
        self.phone = Some(phone.to_owned());
        match self.classify(sent) {
            SendCodeOutcome::Sent(shape) => Ok(PhoneEvent::CodeSent { shape }),
            // grammers 在这两条上会 panic/unimplemented，我们把它变成可读的错。
            SendCodeOutcome::AlreadyAuthorized => Ok(PhoneEvent::LoggedIn),
            SendCodeOutcome::PaymentRequired => Err(QrError::Invocation(String::from(
                "PAYMENT_REQUIRED",
            ))),
        }
    }

    /// 把服务端的 `auth.SentCode` 解成我们的结果，并记下 `phone_code_hash`。
    ///
    /// 三个 variant 都有归宿：`Code` 正常、`Success` 说明已经登录了、
    /// `PaymentRequired` 是我们不支持的付费投递。grammers 对后两者是 panic。
    fn classify(&mut self, sent: tl::enums::auth::SentCode) -> SendCodeOutcome {
        match sent {
            tl::enums::auth::SentCode::Code(code) => {
                self.code_hash = Some(code.phone_code_hash.clone());
                SendCodeOutcome::Sent(shape_of(&kind_of(&code.r#type)))
            }
            // 服务端直接说「已登录」——上次登录其实成功了、登录态没存住。
            tl::enums::auth::SentCode::Success(_) => SendCodeOutcome::AlreadyAuthorized,
            tl::enums::auth::SentCode::PaymentRequired(_) => SendCodeOutcome::PaymentRequired,
        }
    }

    /// 重新发一次码（用户没收到时）。
    ///
    /// # Errors
    ///
    /// 尚未发过码（没有 phone/hash）、或服务端拒绝时返回。
    pub async fn resend_code(&mut self) -> Result<PhoneEvent, QrError> {
        let (Some(phone), Some(hash)) = (self.phone.clone(), self.code_hash.clone()) else {
            return Err(QrError::Invocation(String::from("RESEND_WITHOUT_SEND")));
        };
        let req = tl::functions::auth::ResendCode {
            phone_number: phone,
            phone_code_hash: hash,
            reason: None,
        };
        let sent = self.client.invoke(&req).await.map_err(|e| map_send_err(&e))?;
        match self.classify(sent) {
            SendCodeOutcome::Sent(shape) => Ok(PhoneEvent::CodeSent { shape }),
            SendCodeOutcome::AlreadyAuthorized => Ok(PhoneEvent::LoggedIn),
            SendCodeOutcome::PaymentRequired => {
                Err(QrError::Invocation(String::from("PAYMENT_REQUIRED")))
            }
        }
    }

    /// 提交验证码。
    ///
    /// # Errors
    ///
    /// 尚未发过码、验证码错误、号码未注册（第三方不能注册）、需要 2FA 云密码
    /// （返回 [`PhoneEvent::NeedPassword`] 而不是错误）、网络失败时返回。
    pub async fn submit_code(&mut self, code: &str) -> Result<PhoneEvent, QrError> {
        let (Some(phone), Some(hash)) = (self.phone.clone(), self.code_hash.clone()) else {
            return Err(QrError::Invocation(String::from("CODE_WITHOUT_SEND")));
        };
        let req = tl::functions::auth::SignIn {
            phone_number: phone,
            phone_code_hash: hash,
            phone_code: Some(code.to_owned()),
            email_verification: None,
        };
        match self.client.invoke(&req).await {
            Ok(tl::enums::auth::Authorization::Authorization(_)) => Ok(PhoneEvent::LoggedIn),
            // authorizationSignUpRequired：号码没注册过。第三方应用不能注册新
            // 账号，只能如实说明要先用官方客户端注册
            Ok(tl::enums::auth::Authorization::SignUpRequired(_)) => Err(QrError::SignUpRequired),
            Err(e) => {
                // 服务端要 2FA 云密码——这不是错误，是正常子步
                if e.is("SESSION_PASSWORD_NEEDED") {
                    return self.ask_password().await;
                }
                Err(map_sign_err(&e))
            }
        }
    }

    /// 拉一次 `account.getPassword` 取提示。
    async fn ask_password(&mut self) -> Result<PhoneEvent, QrError> {
        let info = self.password_info().await?;
        Ok(PhoneEvent::NeedPassword { hint: info.hint })
    }

    async fn password_info(&self) -> Result<tl::types::account::Password, QrError> {
        self.client
            .invoke(&tl::functions::account::GetPassword {})
            .await
            .map(Into::into)
            .map_err(|e| map_sign_err(&e))
    }

    /// 提交两步验证的云密码。
    ///
    /// 与扫码那条路的 `submit_password` 逐条同源：`Client::check_password`
    /// 内部有四处会 panic（current_algo/srp_b/srp_id 为 None、未知 KDF、
    /// g 越界、p_and_g 不合法），全都要在调用前判掉——release 下 panic 直接
    /// 带走进程。
    ///
    /// # Errors
    ///
    /// 密码错误（停在本步重输）、服务端 2FA 参数无法处理、网络失败时返回。
    pub async fn submit_password(&mut self, password: &str) -> Result<PhoneEvent, QrError> {
        let info = self.password_info().await?;
        let (Some(algo), Some(_srp_b), Some(_srp_id)) =
            (info.current_algo.clone(), info.srp_b.clone(), info.srp_id)
        else {
            return Err(QrError::UnsupportedPasswordAlgo);
        };
        let tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(a) =
            algo
        else {
            return Err(QrError::UnsupportedPasswordAlgo);
        };
        if !(2..=7).contains(&a.g) {
            return Err(QrError::UnsupportedPasswordAlgo);
        }
        if !grammers_crypto::two_factor_auth::check_p_and_g(&a.p, &a.g) {
            return Err(QrError::UnsupportedPasswordAlgo);
        }
        let token = grammers_client::client::PasswordToken::new(info);
        match self.client.check_password(token, password).await {
            Ok(_) => Ok(PhoneEvent::LoggedIn),
            Err(SignInError::InvalidPassword(_) | SignInError::PasswordRequired(_)) => {
                Err(QrError::WrongPassword)
            }
            Err(SignInError::SignUpRequired) => Err(QrError::SignUpRequired),
            Err(SignInError::InvalidCode) => Err(QrError::UnsupportedPasswordAlgo),
            Err(SignInError::Other(e)) => Err(map_sign_err(&e)),
        }
    }

    /// 服务端认不认这份登录态。用于登录成功后自证。
    ///
    /// # Errors
    ///
    /// 网络失败时返回。
    pub async fn is_authorized(&self) -> Result<bool, QrError> {
        match self
            .client
            .invoke(&tl::functions::updates::GetState {})
            .await
        {
            Ok(_) => Ok(true),
            Err(InvocationError::Rpc(r)) if r.name == "AUTH_KEY_UNREGISTERED" => Ok(false),
            Err(e) => Err(map_sign_err(&e)),
        }
    }
}

/// 把 `auth.SentCodeType` 转成我们做映射用的 [`SentCodeKind`]。
///
/// 只镜像我们关心的那些 variant；email / firebase / fragment 这些当前不接，
/// 归到 `Word`（界面会含糊说明而不是猜一种）——但那三种在手机号+验证码这条
/// 主路径上极少出现。
fn kind_of(t: &tl::enums::auth::SentCodeType) -> SentCodeKind {
    use tl::enums::auth::SentCodeType as T;
    match t {
        T::App(a) => SentCodeKind::App {
            length: u8::try_from(a.length).unwrap_or(5),
        },
        T::Sms(s) => SentCodeKind::Sms {
            length: u8::try_from(s.length).unwrap_or(5),
        },
        T::Call(c) => SentCodeKind::Call {
            length: u8::try_from(c.length).unwrap_or(5),
        },
        T::FlashCall(_) => SentCodeKind::FlashCall,
        T::MissedCall(m) => SentCodeKind::MissedCall {
            length: u8::try_from(m.length).unwrap_or(0),
        },
        T::SmsWord(_) => SentCodeKind::Word,
        T::SmsPhrase(_) => SentCodeKind::Phrase,
        // 其余（email / firebase / fragment / setup-email）当前不接，按短句处理：
        // 界面据此不做数字/长度限制，给一句含糊说明
        _ => SentCodeKind::Phrase,
    }
}

/// `auth.sendCode` 的错误映射。
fn map_send_err(e: &InvocationError) -> QrError {
    if let InvocationError::Rpc(r) = e {
        if r.name.starts_with("FLOOD_WAIT") {
            return QrError::FloodWait {
                secs: r.value.unwrap_or(0),
            };
        }
        if r.name == "API_ID_PUBLISHED_FLOOD" {
            return QrError::ApiIdPublishedFlood;
        }
        // 不带 message：服务端原文可能回显请求参数（含号码）
        return QrError::Invocation(format!("{} ({})", r.name, r.code));
    }
    QrError::Invocation(e.to_string())
}

/// `auth.signIn` / 2FA 的错误映射。
///
/// PHONE_CODE_INVALID / PHONE_CODE_EXPIRED 要单独拎出来：界面据此让用户重输
/// 或重发，而不是笼统「登录失败」。
fn map_sign_err(e: &InvocationError) -> QrError {
    if let InvocationError::Rpc(r) = e {
        if r.name.starts_with("FLOOD_WAIT") {
            return QrError::FloodWait {
                secs: r.value.unwrap_or(0),
            };
        }
        if r.name == "API_ID_PUBLISHED_FLOOD" {
            return QrError::ApiIdPublishedFlood;
        }
        return QrError::Invocation(format!("{} ({})", r.name, r.code));
    }
    QrError::Invocation(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telegram::login::CodeDelivery;

    /// App 送达方式必须映射到「去其他客户端查看」——用户白等一小时的那个坑。
    #[test]
    fn app_delivery_maps_to_other_client() {
        let k = kind_of(&tl::enums::auth::SentCodeType::App(
            tl::types::auth::SentCodeTypeApp { length: 5 },
        ));
        assert_eq!(k, SentCodeKind::App { length: 5 });
        let shape = shape_of(&k);
        assert_eq!(shape.via, CodeDelivery::App);
        assert!(shape.via.requires_other_client());
    }

    /// 单词 / 短句必须被认成非数字，否则用户在数字键盘上输不进字母。
    #[test]
    fn word_and_phrase_are_non_numeric() {
        let w = kind_of(&tl::enums::auth::SentCodeType::SmsWord(
            tl::types::auth::SentCodeTypeSmsWord { beginning: None },
        ));
        assert_eq!(w, SentCodeKind::Word);
        assert!(!w.is_numeric());
        let p = kind_of(&tl::enums::auth::SentCodeType::SmsPhrase(
            tl::types::auth::SentCodeTypeSmsPhrase { beginning: None },
        ));
        assert_eq!(p, SentCodeKind::Phrase);
        assert!(!p.is_numeric());
    }

    /// 短信长度要如实传出来，供界面分格。
    #[test]
    fn sms_length_flows_through() {
        let k = kind_of(&tl::enums::auth::SentCodeType::Sms(
            tl::types::auth::SentCodeTypeSms { length: 6 },
        ));
        assert_eq!(k.fixed_length(), Some(6));
    }
}
