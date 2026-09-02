//! 密码管理：给已加密文件增删改密码。
//!
//! # 为什么单独一个模块
//!
//! 它与 `encrypt.rs` 的区别不只是"改 vs 建"：密码管理**完全不碰载荷**，
//! 只重写 384 字节 slot 区和 32 字节头部 MAC（core 的 `keyslot`）。放进
//! encrypt.rs 会让人以为它也要走那条读写整个文件的路径。
//!
//! # 界面上这三个操作的真实语义
//!
//! 格式上无法探测哪个 slot 是空的——未使用的槽填的是随机字节，与真实槽
//! 在字节层面不可区分（可否认性）。所以不能"找个空位填进去"，猜错会静默
//! 覆盖掉另一个密码。于是三个操作实际都是**重新声明这个文件的密码集合**：
//!
//! | 操作 | 保留 | 作废 |
//! |---|---|---|
//! | `add` | 当前密码 + 新密码 | — |
//! | `change` | 只有新密码 | 当前密码 |
//! | `remove` | 只有当前密码 | 该文件上其它所有密码 |
//!
//! `remove` 这层语义必须由 UI 文案讲清：它不是"删掉某一个密码"，而是
//! "只留下我现在用的这个"。文件上原本挂着几个密码是查不出来的，所以
//! 界面不能显示"将删除 1 个密码"这种它并不知道的数字。
//!
//! # 一个必须传达给用户的局限
//!
//! 移除密码只影响这一份文件。攻击者若留有旧副本，仍能用旧密码打开那个
//! 副本——真正的密钥轮换要重新加密载荷，是另一个操作。UI 不说这句的话，
//! 用户会以为"删掉密码"等于"那个人再也看不到了"，这是危险的错觉。

use crate::commands::{CmdError, CmdResult, Shared};
use omy_core::crypto::{Argon2Params, Kek};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::State;

/// 前端发起的密码管理请求。
#[derive(Debug, Clone, Deserialize)]
pub struct KeyRequest {
    /// 目标 `.omy` 文件的磁盘路径。
    pub path: String,
    /// 操作：`add` / `change` / `remove`。
    pub action: String,
    /// 当前密码，用于解开文件。三种操作都必需。
    pub current: String,
    /// 新密码，`add` / `change` 用。
    #[serde(default)]
    pub next: String,
}

/// 操作结果。
#[derive(Debug, Clone, Serialize)]
pub struct KeyOutcome {
    /// 实际执行的操作，回显给前端做断言。
    pub action: String,
    /// 改写后生效的密码数量。
    pub slots_in_use: usize,
    /// 恒为 false：本操作从不重写载荷。
    ///
    /// 显式返回而不是让前端假定——万一将来有人把实现改成"解密再加密"，
    /// 端到端验证会立刻发现这个字段变了。
    pub payload_rewritten: bool,
}

/// 给一个已加密文件增删改密码。
///
/// # Errors
///
/// - `password_required`：当前密码为空
/// - `new_password_required`：`add` / `change` 没给新密码
/// - `same_password`：新旧密码相同
/// - `bad_action`：未知操作名
/// - `wrong_password`：当前密码打不开这个文件
/// - `file_not_found` / `io_error`：读写失败
/// - `corrupted`：文件头部被篡改（MAC 不匹配）
/// - `internal`：线程调度失败
#[tauri::command]
pub async fn manage_key(state: State<'_, Shared>, req: KeyRequest) -> CmdResult<KeyOutcome> {
    if req.current.is_empty() {
        return Err(CmdError::code("password_required"));
    }
    let action = Action::parse(&req.action).ok_or_else(|| CmdError::code("bad_action"))?;
    if action.needs_next() {
        if req.next.is_empty() {
            return Err(CmdError::code("new_password_required"));
        }
        // 提前拦掉：否则会白跑两次 Argon2 才发现什么也没变
        if req.next == req.current {
            return Err(CmdError::code("same_password"));
        }
    } else if !req.next.is_empty() {
        // remove 用不到新密码。静默忽略是不行的——用户以为自己指定了什么，
        // 实际什么也没发生，而 remove 的结果（其它密码作废）不可逆
        return Err(CmdError::code("unexpected_new_password"));
    }

    let handle: Shared = std::sync::Arc::clone(&state);
    // 两次 Argon2 各几百毫秒，必须离开异步执行器，否则 UI 卡住
    tauri::async_runtime::spawn_blocking(move || run(&handle, &req, action))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

/// 操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Add,
    Change,
    Remove,
}

impl Action {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "add" => Some(Self::Add),
            "change" => Some(Self::Change),
            "remove" => Some(Self::Remove),
            _ => None,
        }
    }

    const fn needs_next(self) -> bool {
        matches!(self, Self::Add | Self::Change)
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Change => "change",
            Self::Remove => "remove",
        }
    }
}

fn run(state: &Shared, req: &KeyRequest, action: Action) -> CmdResult<KeyOutcome> {
    let path = Path::new(&req.path);
    let data = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CmdError::code("file_not_found"),
        _ => CmdError::code("io_error"),
    })?;

    let header = omy_core::file::peek_header(&data).map_err(map_core_err)?;
    let params = header.argon2_params();

    let current = derive(&req.current, &header.vault_salt, params)?;

    // 解锁集合与保留集合是两回事：change 的保留集合里只有新密码，
    // 拿它去解原文件必然失败。分开传，不要图省事复用第一个
    let keep = match action {
        Action::Add => vec![current.duplicate(), derive(&req.next, &header.vault_salt, params)?],
        Action::Change => vec![derive(&req.next, &header.vault_salt, params)?],
        Action::Remove => vec![current.duplicate()],
    };

    let outcome = omy_core::keyslot::rewrite_slots(&data, &[current], &keep).map_err(map_core_err)?;

    // 写回前先自证每个保留密码都能打开新文件。逐个验而不是只验第一个：
    // add 最容易犯的错是新密码能开、原密码被挤掉，只验一个正好漏掉。
    //
    // 顺序不能反——先覆盖原文件再发现打不开，用户就同时失去了旧文件
    // 和访问权
    for k in &keep {
        omy_core::file::open(&outcome.bytes, &[k.duplicate()])
            .map_err(|_| CmdError::code("rewrite_verify_failed"))?;
    }

    omy_core::fsatomic::write_atomic(path, &outcome.bytes)
        .map_err(|_| CmdError::code("io_error"))?;

    // 把改动后仍然有效的密码装进会话，否则列表会把这个文件显示成
    // 「🔒 需要密码」——用户刚刚才输过密码，再被问一次很荒唐。
    //
    // 用 keep 的最后一个：add/change 时它是新密码，remove 时是当前密码。
    // 都是「用户接下来会用的那个」
    let adopt = match action {
        Action::Remove => &req.current,
        _ => &req.next,
    };
    state.with_session(|s| {
        // 与 unlock / encrypt 用同一个 label，否则同一个密码会被算成
        // 两条凭据，状态栏显示的数量就不对了
        let _ = s.unlock_password("main", &header.vault_salt, adopt, params);
    });

    Ok(KeyOutcome {
        action: action.name().to_owned(),
        slots_in_use: outcome.slot_used,
        payload_rewritten: false,
    })
}

fn derive(password: &str, salt: &[u8; 16], params: Argon2Params) -> CmdResult<Kek> {
    Kek::from_password(password.as_bytes(), salt, params)
        .map_err(|_| CmdError::code("kdf_failed"))
}

/// 把 core 的错误映射成前端认识的码。
///
/// 不能一律返回 `internal`：「密码不对」「不是加密文件」「文件坏了」对用户
/// 是三种完全不同的处境——重试一次、选错了文件、去找备份。
///
/// 分类跟随 core 自己的 `ExitCode` 口径（`error.rs`），只把 `BadMagic`
/// 单独拆出来：core 把它算作 Corrupted，但在「选文件」这个场景里，它
/// 几乎总是「用户选了个不是 .omy 的文件」，报「文件已损坏」会让人以为
/// 数据出了问题。新增分支时两处都要看。
fn map_core_err(e: omy_core::Error) -> CmdError {
    use omy_core::Error as E;
    match e {
        E::NoMatchingSlot => CmdError::code("wrong_password"),
        E::BadMagic { .. } => CmdError::code("not_encrypted"),
        E::UnsupportedVersion { .. } | E::UnknownCriticalTlv { .. } => {
            CmdError::code("incompatible_version")
        }
        E::HeaderMacMismatch
        | E::ChunkAuthFailed { .. }
        | E::MalformedHeader { .. }
        | E::MalformedTlv { .. }
        | E::Truncated { .. }
        | E::ContentHashMismatch => CmdError::code("corrupted"),
        E::Io(_) => CmdError::code("io_error"),
        _ => CmdError::code("internal"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_parsing_rejects_unknown() {
        assert_eq!(Action::parse("add"), Some(Action::Add));
        assert_eq!(Action::parse("change"), Some(Action::Change));
        assert_eq!(Action::parse("remove"), Some(Action::Remove));
        // 不这样会怎样：拼错的动作名若被当成某个默认操作执行，
        // 用户可能在想改密码时把其它密码全删了
        assert_eq!(Action::parse("delete"), None);
        assert_eq!(Action::parse(""), None);
        assert_eq!(Action::parse("ADD"), None, "大小写不同应视为未知");
    }

    #[test]
    fn only_add_and_change_need_new_password() {
        assert!(Action::Add.needs_next());
        assert!(Action::Change.needs_next());
        // 不这样会怎样：remove 若也要求新密码，界面会强迫用户为一个
        // 不需要新密码的操作编一个密码出来
        assert!(!Action::Remove.needs_next());
    }

    /// 错误映射必须区分「密码不对」「不是加密文件」「文件损坏」。
    ///
    /// 不这样会怎样：三者都报 internal，用户不知道该重试、换文件还是找备份。
    #[test]
    fn core_errors_map_to_distinct_codes() {
        assert_eq!(map_core_err(omy_core::Error::NoMatchingSlot).code, "wrong_password");
        assert_eq!(map_core_err(omy_core::Error::HeaderMacMismatch).code, "corrupted");
        assert_eq!(
            map_core_err(omy_core::Error::BadMagic { got: [0; 8] }).code,
            "not_encrypted",
            "选错文件不该报「已损坏」"
        );
        assert_eq!(
            map_core_err(omy_core::Error::MalformedHeader { reason: "x" }).code,
            "corrupted"
        );
    }
}
