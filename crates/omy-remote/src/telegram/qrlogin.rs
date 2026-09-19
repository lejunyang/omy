//! 扫码登录的**网络那一半**：真正发 `auth.*` 请求、等手机端确认。
//!
//! 纯逻辑部分（token 怎么编码、过期/迁移该归成什么）在 [`crate::telegram::qr`]，
//! 那一层不碰网络、可单测。这一层负责把它接到真实的 MTProto 连接上。
//!
//! # 流程
//!
//! ```text
//! auth.exportLoginToken
//!   ├─ loginToken        → 编成 tg://login?token=… 渲染二维码，等 updateLoginToken
//!   │                      └─ 到期没等到 → **自动重新导出**，换一张图（不是错误）
//!   ├─ loginTokenMigrateTo → 切到该 DC 调 auth.importLoginToken（中间态，不是失败）
//!   └─ loginTokenSuccess   → 成功（若开了 2FA，服务端先回 SESSION_PASSWORD_NEEDED）
//! ```
//!
//! # 为什么做成「一次 `step()` 返回一个事件」
//!
//! 界面必须**看得见**每一步：换了第几张图、是不是在切数据中心、要不要输云密码。
//! 把整个流程塞进一个 `login()` 里只能在成功或失败时给一个结果，中间态全丢了——
//! 而中间态恰恰是这个流程里最容易被做错的部分（过期做成报错、迁移做成失败）。
//!
//! # 为什么不走 grammers 的高层封装
//!
//! 它压根没有扫码登录。而且登录路径上的若干分支是 `panic!` /
//! `unimplemented!()`，而 omy-gui 禁止 panic、release 下又是 `panic = "abort"`，
//! `catch_unwind` 救不了。详见 [`crate::telegram::auth`] 的模块文档。
//!
//! 唯一仍然复用 grammers 的是 2FA 的 SRP 计算（[`Client::check_password`]）：
//! 那段密码学自己重写风险更大。代价是它内部有几处 `unwrap`/`panic`，所以调用前
//! **必须**先把那些前提逐条验过，见 [`QrSession::submit_password`]。

use std::sync::Arc;

use grammers_client::{tl, Client, InvocationError, SignInError};
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::storages::MemorySession;
use grammers_session::updates::UpdatesLike;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::telegram::appid::AppId;
use crate::telegram::device::DeviceInfo;
use crate::telegram::qr::token_url;

/// 等一次 `updateLoginToken` 最多等多久（秒）。
///
/// 只是个兜底：正常情况下由令牌自己的过期时间决定何时换图。有它是因为
/// **服务端不保证一定会推这条更新**（网络抖动、连接被重建都可能让它丢），
/// 而没有兜底的话界面会永远停在一张早已过期的图上。
const UPDATE_WAIT_CAP_SECS: u64 = 60;

/// 令牌快到期时提前多少秒就主动换图。
///
/// 不卡着过期时刻才换：用户正要扫的那一瞬间图失效，他会扫到一张刚好作废的码，
/// 而界面上什么都没发生。留一点余量比精确更有用。
const REFRESH_MARGIN_SECS: i64 = 3;

/// 扫码登录过程中的一个事件。
///
/// 每一个都对应界面上一处**可见**的变化。刻意不带令牌字节以外的凭据：
/// `url` 是要渲染成二维码的东西，用完即弃。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QrEvent {
    /// 有一张（新的）二维码可以显示了。
    Token {
        /// 要渲染成二维码的内容（`tg://login?token=…`）。
        url: String,
        /// 这张图还有多少秒过期，界面据此倒计时。
        expires_in_secs: u32,
        /// 这是第几次换图。**0 表示首张**。
        ///
        /// 界面用它显示「已自动换一张（第 N 次）」。官方要求过期后自动重新
        /// 生成，但换图这件事必须让用户看见——静默替换会让盯着屏幕的人以为
        /// 卡住了，或者怀疑刚才扫的那张还算不算数。
        refresh_index: u32,
    },
    /// 正在切换数据中心。
    ///
    /// **中间态，不是失败。** 界面显示「正在切换数据中心…」并继续等。
    /// 做成失败会让一次本来会成功的登录被用户自己打断。
    Migrating {
        /// 目标数据中心编号。
        dc: i32,
    },
    /// 手机端确认了，但这个账号开了两步验证，还要云密码。
    ///
    /// 这是一个**事先无法预知**的子步：没开 2FA 的账号根本不会走到这里。
    /// 所以界面不能把它画成固定的「第 N 步」。
    NeedPassword {
        /// 服务端给的密码提示；没有则为 `None`，此时界面**整行不显示**，
        /// 不要放一个空的「提示：」。
        hint: Option<String>,
    },
    /// 登录成功。
    LoggedIn,
}

/// 扫码登录会话。
///
/// 持有连接与内存 session。登录成功后由调用方**立刻**把 session 落盘
/// （见 [`crate::telegram::session::save`]）——不要等整趟流程结束，中途任何
/// 一步失败都会让这次已经在服务端生效的登录白费。
pub struct QrSession {
    client: Client,
    session: Arc<MemorySession>,
    app: AppId,
    /// 服务端推来的更新流。扫码确认就是靠这里的 `updateLoginToken`。
    updates: UnboundedReceiver<UpdatesLike>,
    /// 驱动 I/O 的后台任务。
    ///
    /// 必须留着：它一旦被 drop，整个连接就断了，而现象是所有请求都超时。
    runner: tokio::task::JoinHandle<()>,
    /// 当前那张二维码对应的令牌。
    ///
    /// 只在内存里存活到下次换图为止。它等同一次登录凭据，不落盘也不进日志。
    pending: Option<Vec<u8>>,
    /// 已经换过几张图。
    refreshes: u32,
}

impl std::fmt::Debug for QrSession {
    /// **绝不打印 `pending`**：那是登录凭据。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QrSession")
            .field("has_pending_token", &self.pending.is_some())
            .field("refreshes", &self.refreshes)
            .finish_non_exhaustive()
    }
}

impl Drop for QrSession {
    /// 断开连接并停掉 runner。
    ///
    /// 不这样会怎样：用户关掉登录窗口后那条连接仍然挂着，而 Telegram 对同一
    /// 账号的并发连接数有限制——反复开关几次登录页就会开始收到 429。
    fn drop(&mut self) {
        self.client.disconnect();
        self.runner.abort();
    }
}

/// 扫码登录过程中可能出的问题。
#[derive(Debug, thiserror::Error)]
pub enum QrError {
    /// 底层调用失败。
    #[error("{0}")]
    Invocation(String),
    /// 被服务端限流。
    ///
    /// 与普通失败分开：它会自动恢复，界面该显示倒计时而不是重试按钮——
    /// 限流下重试只会把等待时间越点越长。
    #[error("请求过于频繁，请等待 {secs} 秒")]
    FloodWait {
        /// 服务端要求的等待秒数。
        secs: u32,
    },
    /// 内置 api_id 被服务端封了。
    ///
    /// **重试无用**，唯一出路是让用户填自己的 api_id。
    #[error("内置应用身份已被 Telegram 限制，请在设置里填写自己的 api_id")]
    ApiIdPublishedFlood,
    /// 云密码不对。**停在本步重输**，不退回上一步。
    #[error("两步验证密码不正确")]
    WrongPassword,
    /// 这个账号还没注册过。
    ///
    /// 第三方应用**不能**注册新账号（官方明确限制），所以这不是我们能帮上忙的
    /// 事，只能如实说明要先用官方客户端注册。
    #[error("该号码尚未注册，请先用官方 Telegram 客户端注册账号")]
    SignUpRequired,
    /// 服务端给的 2FA 参数我们处理不了。
    ///
    /// 存在这一条是因为 grammers 在同样的情形下是 `panic!`，而 omy-gui 禁止
    /// panic、release 下 `panic = "abort"` 连 `catch_unwind` 都救不了。
    /// 宁可报一条看得懂的错，也不能让进程消失。
    #[error("无法处理服务端给出的两步验证参数，请更新 omy 后重试")]
    UnsupportedPasswordAlgo,
    /// 连接断了（runner 停了）。
    #[error("与 Telegram 的连接已断开")]
    Disconnected,
}

/// 把 grammers 的调用错误翻成我们的。
///
/// `FLOOD_WAIT` 与 `API_ID_PUBLISHED_FLOOD` 必须单独拎出来：前者要显示倒计时、
/// 后者要给「填自己的 api_id」按钮。混进笼统的失败里，界面只能显示「登录失败」，
/// 而这两种情况各自都有明确且不同的出路。
fn map_err(e: &InvocationError) -> QrError {
    if let InvocationError::Rpc(r) = e {
        if r.name.starts_with("FLOOD_WAIT") {
            return QrError::FloodWait {
                secs: r.value.unwrap_or(0),
            };
        }
        if r.name == "API_ID_PUBLISHED_FLOOD" {
            return QrError::ApiIdPublishedFlood;
        }
        // 只带错误名与代码，**不带 message**：服务端的原文里可能回显请求参数
        return QrError::Invocation(format!("{} ({})", r.name, r.code));
    }
    QrError::Invocation(e.to_string())
}

impl QrSession {
    /// 建连接。
    ///
    /// `proxy` 必须是已经过 [`crate::telegram::proxy::normalize`] 的地址——
    /// grammers 只认 `socks5://`，把用户填的原样递进来会收到一句
    /// 「proxy scheme not supported」。
    ///
    /// # Errors
    ///
    /// 目前不会失败（连接是懒建的，第一次请求才真的连）。保留 `Result` 是因为
    /// 调用方本来就要处理错误，将来这里加校验时签名不用变。
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
        let SenderPool {
            runner,
            handle,
            updates,
        } = SenderPool::with_configuration(Arc::clone(&session), app.id(), params);
        let client = Client::new(handle);
        // runner 必须 spawn 起来，否则所有请求都只是排进队列、永远不会发出去
        let runner = tokio::spawn(async move {
            runner.run().await;
        });
        Ok(Self {
            client,
            session,
            app,
            updates,
            runner,
            pending: None,
            refreshes: 0,
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

    /// 推进一步，返回这一步产生的事件。
    ///
    /// 调用方循环调它直到拿到 [`QrEvent::LoggedIn`] 或 [`QrEvent::NeedPassword`]。
    ///
    /// # Errors
    ///
    /// 网络失败、限流、api_id 被封时返回。**令牌过期不在此列**——那会静默换一
    /// 张新图并返回一个 `Token` 事件，因为它本来就不是错误。
    pub async fn step(&mut self) -> Result<QrEvent, QrError> {
        // 手上没有令牌（首次，或上一张已作废）→ 导出一张
        let Some(token) = self.pending.clone() else {
            return self.export().await;
        };

        // 有令牌 → 等手机端确认。等到了就再导出一次取结果，等不到就换图。
        if self.wait_for_confirmation().await? {
            // 这一次导出返回的会是 Success（或需要云密码）
            return self.export().await;
        }
        // 没等到：这张过期了，**自动**换一张。计数 +1 让界面能显示换过几次
        drop(token);
        self.pending = None;
        self.refreshes = self.refreshes.saturating_add(1);
        self.export().await
    }

    /// 导出一个登录令牌，并按服务端的回应决定下一步。
    async fn export(&mut self) -> Result<QrEvent, QrError> {
        let req = tl::functions::auth::ExportLoginToken {
            api_id: self.app.id(),
            api_hash: String::from(self.app.hash()),
            // 不排除任何已登录用户：这个参数是给「多账号客户端切换账号」用的，
            // omy 一次只管一个账号
            except_ids: Vec::new(),
        };
        let res = match self.client.invoke(&req).await {
            Ok(r) => r,
            // 服务端说「该换个数据中心了」也可能以 RPC 303 的形式出现，
            // 而不是 loginTokenMigrateTo。两条路都要走通，否则在需要迁移的
            // 账号上登录会随机失败
            Err(InvocationError::Rpc(ref r)) if r.code == 303 => {
                let dc = i32::try_from(r.value.unwrap_or(0)).unwrap_or(0);
                return self.migrate_home_dc(dc).await;
            }
            Err(e) => {
                // 服务端在这里回「需要云密码」，意味着手机端已经确认过了。
                // 这条路径是真实存在的：确认与我们取结果之间有时间差
                if e.is("SESSION_PASSWORD_NEEDED") {
                    return self.ask_password().await;
                }
                return Err(map_err(&e));
            }
        };
        self.handle_login_token(res).await
    }

    /// 处理 `auth.LoginToken` 的三个 variant。
    ///
    /// 三个都有归宿，不存在「不该发生所以 panic」的分支——这正是我们绕开
    /// grammers 高层封装的原因。
    async fn handle_login_token(
        &mut self,
        res: tl::enums::auth::LoginToken,
    ) -> Result<QrEvent, QrError> {
        match res {
            tl::enums::auth::LoginToken::Token(t) => {
                let url = token_url(&t.token);
                self.pending = Some(t.token);
                Ok(QrEvent::Token {
                    url,
                    expires_in_secs: remaining_secs(t.expires),
                    refresh_index: self.refreshes,
                })
            }
            tl::enums::auth::LoginToken::MigrateTo(m) => {
                self.import_into_dc(m.dc_id, m.token).await
            }
            tl::enums::auth::LoginToken::Success(_) => Ok(QrEvent::LoggedIn),
        }
    }

    /// 服务端以 RPC 303 要求换数据中心：切过去，然后重新导出。
    async fn migrate_home_dc(&mut self, dc: i32) -> Result<QrEvent, QrError> {
        use grammers_session::Session as _;
        self.session
            .set_home_dc_id(dc)
            .await
            .map_err(|e| QrError::Invocation(e.to_string()))?;
        // 旧连接留着没用，且会占一个连接配额
        self.client.disconnect();
        Ok(QrEvent::Migrating { dc })
    }

    /// `loginTokenMigrateTo`：去目标数据中心 `auth.importLoginToken`。
    ///
    /// 这是**中间态**。返回 `Migrating` 让界面显示「正在切换数据中心…」，
    /// 而不是显示失败——做成失败会让一次本来会成功的登录被用户点掉。
    async fn import_into_dc(&mut self, dc: i32, token: Vec<u8>) -> Result<QrEvent, QrError> {
        use grammers_session::Session as _;

        let req = tl::functions::auth::ImportLoginToken { token };
        let res = match self.client.invoke_in_dc(dc, &req).await {
            Ok(r) => r,
            Err(e) => {
                if e.is("SESSION_PASSWORD_NEEDED") {
                    return self.ask_password().await;
                }
                return Err(map_err(&e));
            }
        };
        // 迁移成功后主数据中心就是它了。不改的话之后每次请求还是发往旧 DC，
        // 而那边并不认这次登录——现象是「登录成功了但立刻又说未登录」
        self.session
            .set_home_dc_id(dc)
            .await
            .map_err(|e| QrError::Invocation(e.to_string()))?;
        self.pending = None;
        match res {
            tl::enums::auth::LoginToken::Success(_) => Ok(QrEvent::LoggedIn),
            // 迁移之后又给了一个新令牌：正常，继续显示新图
            tl::enums::auth::LoginToken::Token(t) => {
                let url = token_url(&t.token);
                self.pending = Some(t.token);
                Ok(QrEvent::Token {
                    url,
                    expires_in_secs: remaining_secs(t.expires),
                    refresh_index: self.refreshes,
                })
            }
            // 连环迁移：如实报出来继续走，而不是当成失败
            tl::enums::auth::LoginToken::MigrateTo(m) => Ok(QrEvent::Migrating { dc: m.dc_id }),
        }
    }

    /// 等一条 `updateLoginToken`。
    ///
    /// 返回 `true` 表示手机端确认了，`false` 表示这张图该换了。
    ///
    /// # 为什么要有超时兜底
    ///
    /// 服务端**不保证**一定会把这条更新推到。连接被重建、网络抖动都可能让它丢，
    /// 而丢了之后界面会永远停在一张早已过期的死图上——用户反复扫，什么也不发生。
    async fn wait_for_confirmation(&mut self) -> Result<bool, QrError> {
        let cap = std::time::Duration::from_secs(UPDATE_WAIT_CAP_SECS);
        let deadline = tokio::time::Instant::now() + cap;
        loop {
            let recv = tokio::time::timeout_at(deadline, self.updates.recv()).await;
            match recv {
                // 超时：没等到确认，该换图了
                Err(_) => return Ok(false),
                // 更新通道关了 = runner 没了 = 连接断了
                Ok(None) => return Err(QrError::Disconnected),
                Ok(Some(u)) => {
                    if contains_login_token(&u) {
                        return Ok(true);
                    }
                    // 连接被重建过，可能漏掉了那条更新。当成「该重新取一次结果」
                    // 而不是继续傻等——重新导出一次是幂等的，代价只是一次请求
                    if matches!(u, UpdatesLike::ConnectionClosed) {
                        return Ok(true);
                    }
                    // 其余更新与登录无关，继续等
                }
            }
        }
    }

    /// 取云密码参数并转成界面要的提示。
    async fn ask_password(&mut self) -> Result<QrEvent, QrError> {
        let pwd = self.password_info().await?;
        Ok(QrEvent::NeedPassword { hint: pwd.hint })
    }

    /// 拉一次 `account.getPassword`。
    async fn password_info(&self) -> Result<tl::types::account::Password, QrError> {
        self.client
            .invoke(&tl::functions::account::GetPassword {})
            .await
            .map(Into::into)
            .map_err(|e| map_err(&e))
    }

    /// 提交两步验证的云密码。
    ///
    /// # 为什么这里要先做一堆看似多余的校验
    ///
    /// 因为下面调用的 `Client::check_password` 内部有四处会崩：
    ///
    /// ```text
    /// current_algo.clone().unwrap()        // current_algo 为 None 时
    /// srp_b.clone().unwrap() / srp_id...   // 同上（都是 flags.2 字段）
    /// PasswordKdfAlgo::Unknown => panic!   // 不认识的 KDF
    /// check_p_and_g 失败两次 => panic!     // 参数不合法
    /// ```
    ///
    /// omy-gui 禁止 panic，而 release 下 `panic = "abort"` 让 `catch_unwind`
    /// 完全失效——也就是说撞上任何一条都是**进程直接消失**，用户看到程序闪退
    /// 而不是一条说明。所以只能在调用前把这些前提逐条验掉，让那些分支不可达。
    ///
    /// # Errors
    ///
    /// 密码不对、服务端参数无法处理、网络失败时返回。**密码不对时停在本步**，
    /// 调用方不要退回上一步。
    pub async fn submit_password(&mut self, password: &str) -> Result<QrEvent, QrError> {
        let info = self.password_info().await?;

        // ① flags.2 那组字段必须都在。它们在 has_password 为真时才出现，
        //    而我们走到这里正是因为服务端说要密码——但不能因此就假设它们在
        let (Some(algo), Some(_srp_b), Some(_srp_id)) =
            (info.current_algo.clone(), info.srp_b.clone(), info.srp_id)
        else {
            return Err(QrError::UnsupportedPasswordAlgo);
        };

        // ② KDF 必须是我们认识的那一种
        let tl::enums::PasswordKdfAlgo::Sha256Sha256Pbkdf2Hmacsha512iter100000Sha256ModPow(a) =
            algo
        else {
            return Err(QrError::UnsupportedPasswordAlgo);
        };

        // ③ g 必须落在 check_p_and_g 认识的范围内。
        //    超出这个范围时它自己就是 `panic!("Unexpected g parameter")`，
        //    所以这一条要在调它**之前**判，不能指望它返回 false
        if !(2..=7).contains(&a.g) {
            return Err(QrError::UnsupportedPasswordAlgo);
        }
        // ④ 参数本身必须合法。不合法时 grammers 会重取一次、仍不合法就 panic
        if !grammers_crypto::two_factor_auth::check_p_and_g(&a.p, &a.g) {
            return Err(QrError::UnsupportedPasswordAlgo);
        }

        // 前提都满足了，SRP 那段密码学交给 grammers——自己重写风险更大
        let token = grammers_client::client::PasswordToken::new(info);
        match self.client.check_password(token, password).await {
            Ok(_) => Ok(QrEvent::LoggedIn),
            // 停在本步重输，不退回上一步
            Err(SignInError::InvalidPassword(_)) => Err(QrError::WrongPassword),
            Err(SignInError::SignUpRequired) => Err(QrError::SignUpRequired),
            Err(SignInError::PasswordRequired(_)) => Err(QrError::WrongPassword),
            Err(SignInError::InvalidCode) => Err(QrError::UnsupportedPasswordAlgo),
            Err(SignInError::Other(e)) => Err(map_err(&e)),
        }
    }

    /// 服务端认不认这份登录态。
    ///
    /// 登录成功后用它自证，而不是「没报错就算成功」——auth key 绑定是服务端行为，
    /// 本地没报错不代表那边真的认。
    ///
    /// # Errors
    ///
    /// 网络失败时返回。
    pub async fn is_authorized(&self) -> Result<bool, QrError> {
        self.client.is_authorized().await.map_err(|e| map_err(&e))
    }
}

/// 一批更新里有没有 `updateLoginToken`。
///
/// 这条更新**没有任何字段**（`updateLoginToken#564fe691 = Update;`），
/// 它的全部含义就是「去再取一次结果」。
fn contains_login_token(u: &UpdatesLike) -> bool {
    let UpdatesLike::Updates(updates) = u else {
        return false;
    };
    let is_it = |x: &tl::enums::Update| matches!(x, tl::enums::Update::LoginToken);
    match updates {
        tl::enums::Updates::Updates(x) => x.updates.iter().any(is_it),
        tl::enums::Updates::Combined(x) => x.updates.iter().any(is_it),
        tl::enums::Updates::UpdateShort(x) => is_it(&x.update),
        _ => false,
    }
}

/// 把服务端给的绝对过期时刻换算成「还有多少秒」。
///
/// 服务端给的是 Unix 时间戳，而界面要显示倒计时。已经过期时返回 0 而不是负数
/// ——界面拿负数会显示「-3 秒后刷新」。
fn remaining_secs(expires_at: i32) -> u32 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0i64, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let left = i64::from(expires_at) - now - REFRESH_MARGIN_SECS;
    u32::try_from(left.max(0)).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只有 `updateLoginToken` 才算「手机端确认了」。
    ///
    /// 不这样会怎样：把任意一条更新都当成确认，就会在用户还没扫的时候去取结果，
    /// 白发一次请求（Telegram 有限流）；反过来漏认它，则是扫了码却毫无反应，
    /// 一直等到二维码过期换图——用户会以为扫码功能坏了。
    #[test]
    fn only_the_login_token_update_counts_as_confirmation() {
        let yes = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![tl::enums::Update::LoginToken],
            users: Vec::new(),
            chats: Vec::new(),
            date: 0,
            seq: 0,
        }));
        assert!(contains_login_token(&yes));

        // 一条无关的更新不能被当成确认
        let no = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![tl::enums::Update::DeleteMessages(
                tl::types::UpdateDeleteMessages {
                    messages: vec![1],
                    pts: 1,
                    pts_count: 1,
                },
            )],
            users: Vec::new(),
            chats: Vec::new(),
            date: 0,
            seq: 0,
        }));
        assert!(!contains_login_token(&no));
    }

    /// 混在一批更新里也要认出来。
    ///
    /// 不这样会怎样：只看第一条的话，当服务端把它和别的更新打包在一起下发时
    /// 就会漏掉——而那时用户明明已经扫了，界面却一直在换图。
    #[test]
    fn login_token_is_found_among_other_updates() {
        let mixed = UpdatesLike::Updates(tl::enums::Updates::Combined(tl::types::UpdatesCombined {
            updates: vec![
                tl::enums::Update::DeleteMessages(tl::types::UpdateDeleteMessages {
                    messages: vec![7],
                    pts: 2,
                    pts_count: 1,
                }),
                tl::enums::Update::LoginToken,
            ],
            users: Vec::new(),
            chats: Vec::new(),
            date: 0,
            seq_start: 0,
            seq: 0,
        }));
        assert!(contains_login_token(&mixed), "打包下发时也必须认出来");
    }

    /// `updateShort` 形式（单条更新）同样要认。
    #[test]
    fn short_form_updates_are_handled() {
        let short = UpdatesLike::Updates(tl::enums::Updates::UpdateShort(tl::types::UpdateShort {
            update: tl::enums::Update::LoginToken,
            date: 0,
        }));
        assert!(contains_login_token(&short));
    }

    /// 非 `Updates` 类的东西不能被误判成确认。
    ///
    /// 不这样会怎样：`ConnectionClosed` 之类的内部信号若被当成「已确认」，
    /// 会在断线重连时白跑一次取结果。断线该走它自己那条分支，语义不同。
    #[test]
    fn non_update_signals_are_not_confirmations() {
        assert!(!contains_login_token(&UpdatesLike::ConnectionClosed));
        assert!(!contains_login_token(&UpdatesLike::MalformedUpdates));
    }

    /// 已过期的令牌要报 0 秒，不能报负数。
    ///
    /// 不这样会怎样：界面会显示「-3 秒后自动刷新」。而且负数一旦参与倒计时
    /// 计算，很容易变成一个永远走不完的循环。
    #[test]
    fn expired_tokens_report_zero_not_negative() {
        assert_eq!(remaining_secs(0), 0, "1970 年的时间戳早就过期了");
        assert_eq!(remaining_secs(i32::MIN), 0);
    }

    /// 未过期的令牌要留出提前量。
    ///
    /// 不这样会怎样：卡着过期时刻才换图，用户正要扫的那一瞬间图失效——他扫到
    /// 一张刚好作废的码，而界面上什么都没发生。
    #[test]
    fn unexpired_tokens_reserve_a_margin() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0i64, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        let in_30s = i32::try_from(now + 30).unwrap_or(i32::MAX);
        let left = remaining_secs(in_30s);
        // 提前量必须真的被扣掉，否则这个常量等于没写
        assert!(
            left <= 30 - u32::try_from(REFRESH_MARGIN_SECS).unwrap_or(0),
            "应当扣掉提前量，实际 {left}"
        );
        assert!(left >= 20, "也不该扣得离谱，实际 {left}");
    }

    /// `FLOOD_WAIT` 必须被单独识别出来并带上秒数。
    ///
    /// 不这样会怎样：归进笼统的「登录失败」，界面就会给一个重试按钮——而限流
    /// 下重试会把等待时间越点越长，用户越急越点、越点越久。
    #[test]
    fn flood_wait_is_classified_with_its_seconds() {
        let e = InvocationError::Rpc(grammers_mtsender::RpcError {
            code: 420,
            name: String::from("FLOOD_WAIT_41"),
            value: Some(41),
            caused_by: None,
        });
        match map_err(&e) {
            QrError::FloodWait { secs } => assert_eq!(secs, 41, "秒数要如实带出来"),
            other => panic!("应当识别为限流，实际 {other:?}"),
        }
    }

    /// `API_ID_PUBLISHED_FLOOD` 要能与普通失败区分开。
    ///
    /// 不这样会怎样：这是唯一一种「重试永远无用、且 omy 自己修不了」的失败，
    /// 出路只有让用户填自己的 api_id。混进普通失败里，用户会一直点重试。
    #[test]
    fn published_flood_is_its_own_case() {
        let e = InvocationError::Rpc(grammers_mtsender::RpcError {
            code: 406,
            name: String::from("API_ID_PUBLISHED_FLOOD"),
            value: None,
            caused_by: None,
        });
        assert!(matches!(map_err(&e), QrError::ApiIdPublishedFlood));
    }

    /// 错误信息里不能回显服务端原文。
    ///
    /// 不这样会怎样：服务端的 message 里可能带上请求参数，而扫码请求的参数里
    /// 有 api_hash——那是凭据，一旦进了错误信息就会被写进日志或截图发出来。
    #[test]
    fn error_text_does_not_echo_server_message() {
        let e = InvocationError::Rpc(grammers_mtsender::RpcError {
            code: 400,
            name: String::from("SOMETHING_INVALID"),
            value: None,
            caused_by: None,
        });
        let text = map_err(&e).to_string();
        assert!(text.contains("SOMETHING_INVALID"), "错误名要留着便于排查");
        assert!(
            !text.contains(crate::telegram::appid::BUILTIN_API_HASH),
            "错误信息里绝不能出现 api_hash"
        );
    }
}
