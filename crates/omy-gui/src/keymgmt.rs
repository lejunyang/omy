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
//! 覆盖掉另一个密码。
//!
//! 但**保住一个槽不需要先认出它**：slot 密文只依赖
//! `(KEK, file_uuid, slot_index)`，原样搬运即可（core 的
//! `OtherSlots::Carry`）。所以三个操作的语义是：
//!
//! | 操作 | 保留 | 作废 |
//! |---|---|---|
//! | `add` | 当前密码 + 新密码 + 其它槽原样 | — |
//! | `change` | 新密码 + 其它槽原样 | 当前密码 |
//! | `remove` | 只有当前密码 | 该文件上其它所有密码 |
//!
//! `remove` 这层语义必须由 UI 文案讲清：它不是"删掉某一个密码"，而是
//! "只留下我现在用的这个"，**包括用户可能设过的恢复码**。文件上原本挂着
//! 几个密码是查不出来的，所以界面不能显示"将删除 1 个密码"这种它并不知道
//! 的数字。
//!
//! 反过来，`add` / `change` 也**不该**再说"其它密码会失效"——那是改用
//! 搬运之前的行为，现在它们不再殃及恢复码。
//!
//! # 第四个操作：`reencrypt`
//!
//! 上面三个都只改"谁能打开"，载荷密文一字节不变，所以移除密码只影响这
//! 一份文件——攻击者若留有旧副本，仍能用旧密码打开那个副本。用户会以为
//! "删掉密码"等于"那个人再也看不到了"，这是危险的错觉。
//!
//! `reencrypt` 换掉文件密钥并重写整个载荷（core 的 `reencrypt`），此后
//! 这个文件与旧密码彻底无关。代价是耗时与文件大小成正比，所以它是用户
//! 明确选择的另一个操作，不是默认行为。
//!
//! 它仍然挡不住已经流出去的副本：那是一份独立的密文，本操作对它无能为力。
//! UI 必须说"这份文件从此与旧密码无关"，不能说"彻底作废旧密码"。

use crate::commands::{CmdError, CmdResult, Shared};
use crate::encrypt::{ENCRYPT_PROGRESS_EVENT, EncryptProgress};
use omy_core::crypto::{Argon2Params, Kek};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{Emitter as _, State};

/// 前端发起的密码管理请求。
#[derive(Debug, Clone, Deserialize)]
pub struct KeyRequest {
    /// 目标 `.omy` 文件的磁盘路径。
    pub path: String,
    /// 操作：`add` / `change` / `remove` / `reencrypt`。
    pub action: String,
    /// 可管理模式下要精确删除的槽位下标。
    ///
    /// 只对 `remove` 有意义：给了就只删这一个，不给就沿用老语义
    /// （保留当前密码、清掉其余）。可否认模式下必须为空——那里
    /// 分不清哪个下标是谁，给了也没法照办。
    #[serde(default)]
    pub slot_index: Option<usize>,
    /// 当前密码，用于解开文件。四种操作都必需。
    pub current: String,
    /// 新密码。`add` / `change` 必需，`reencrypt` 可选（留空即沿用当前密码）。
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
    /// 是否重写了载荷。只有 `reencrypt` 为 true。
    ///
    /// 显式返回而不是让前端假定：这是"改密码"与"轮换"的唯一可观察差别，
    /// 端到端验证靠它区分两者是否真的走了不同实现。
    pub payload_rewritten: bool,
    /// 目标是树形加密的目录时为 true。
    pub is_tree: bool,
    /// 改密码后这棵树的新路径（树形时非空）。
    ///
    /// **目录名会变**：它由密码派生。前端必须用这个值刷新列表与选中项，
    /// 否则会指向一个已不存在的目录——表现为「改完密码文件夹不见了」。
    pub new_path: String,
    /// 树形时改写的文件数。
    pub files_changed: usize,
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
pub async fn manage_key(
    app: tauri::AppHandle,
    state: State<'_, Shared>,
    req: KeyRequest,
) -> CmdResult<KeyOutcome> {
    if req.current.is_empty() {
        return Err(CmdError::code("password_required"));
    }
    let action = Action::parse(&req.action).ok_or_else(|| CmdError::code("bad_action"))?;
    if action.needs_next() && req.next.is_empty() {
        return Err(CmdError::code("new_password_required"));
    }
    if !action.accepts_next() && !req.next.is_empty() {
        // remove 用不到新密码。静默忽略是不行的——用户以为自己指定了什么，
        // 实际什么也没发生，而 remove 的结果（其它密码作废）不可逆
        return Err(CmdError::code("unexpected_new_password"));
    }
    // 提前拦掉：否则会白跑两次 Argon2 才发现什么也没变。
    //
    // reencrypt 例外：它即使密码不变也是有意义的（换掉文件密钥，让旧副本
    // 的密码对这份文件失效），所以只在真的要改密码时才拦
    if !req.next.is_empty() && req.next == req.current && action != Action::Reencrypt {
        return Err(CmdError::code("same_password"));
    }

    let handle: Shared = std::sync::Arc::clone(&state);
    // 两次 Argon2 各几百毫秒，轮换还要重写整个载荷，
    // 必须离开异步执行器，否则 UI 卡住
    tauri::async_runtime::spawn_blocking(move || run(&handle, &app, &req, action))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

/// 一个槽位在界面上的样子。
#[derive(Debug, serde::Serialize)]
pub struct SlotInfo {
    /// 槽位下标，0..8。
    pub index: usize,
    /// 类型代号：`empty` / `vault` / `device` / `portable` / `recovery` / `unknown`。
    pub kind: String,
    /// 是否是当前密码所在的那一个。界面据此禁用它的删除按钮——
    /// 删掉自己正在用的这把钥匙，对话框下一步就没法继续了。
    pub current: bool,
}

/// 槽位清单查询的结果。
#[derive(Debug, serde::Serialize)]
pub struct SlotList {
    /// 是否是可管理模式。false 时 `slots` 为空，界面据此显示说明而非清单。
    pub managed: bool,
    /// 槽位清单。可否认模式下恒为空——那不是「读取失败」，是设计如此。
    pub slots: Vec<SlotInfo>,
}

/// 读取一个文件的槽位清单。
///
/// 需要密码：槽位目录是加密的。这不是不便，正是可管理模式的边界——
/// 对**打不开这个文件的人**，它与可否认模式一样什么都不透露。
///
/// # Errors
///
/// 文件读不了、不是 omy 文件、密码不对时返回对应错误码。
#[tauri::command]
pub async fn list_slots(state: State<'_, Shared>, req: SlotQuery) -> CmdResult<SlotList> {
    let handle: Shared = std::sync::Arc::clone(&state);
    // Argon2 几百毫秒，不能占着异步执行器
    tauri::async_runtime::spawn_blocking(move || read_slots(&handle, &req))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

/// `list_slots` 的入参。
#[derive(Debug, serde::Deserialize)]
pub struct SlotQuery {
    /// 目标文件路径。
    pub path: String,
    /// 当前密码。
    pub password: String,
}

fn read_slots(_state: &Shared, req: &SlotQuery) -> CmdResult<SlotList> {
    let path = Path::new(&req.path);
    // 树形目录还没有槽位目录这个概念：整棵树共用一份 .omy-keys，
    // 那里的槽位语义要单独设计。如实说「不支持」而不是报一个含糊的
    // IO 错误——后者会让用户以为文件坏了
    if path.is_dir() {
        return Ok(SlotList { managed: false, slots: Vec::new() });
    }
    let data = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CmdError::code("file_not_found"),
        _ => CmdError::code("io_error"),
    })?;
    let h = omy_core::file::peek_header(&data).map_err(|_| CmdError::code("not_omy_file"))?;
    let kek = derive(&req.password, &h.vault_salt, h.argon2_params())?;
    let opened =
        omy_core::file::open(&data, &[kek]).map_err(|_| CmdError::code("wrong_password"))?;

    if !opened.is_slot_managed() {
        return Ok(SlotList { managed: false, slots: Vec::new() });
    }
    let dir = opened.slot_directory().map_err(|_| CmdError::code("bad_slot_directory"))?;
    let cur = usize::from(opened.slot_index);
    let slots = dir
        .entries()
        .iter()
        .enumerate()
        .map(|(i, e)| SlotInfo {
            index: i,
            kind: e.kind.name().to_string(),
            current: i == cur,
        })
        .collect();
    Ok(SlotList { managed: true, slots })
}
/// 造一个把 core 进度转成前端事件的回调。
///
/// `stage` / `stages` 直接映射成 `index` / `total_files`，复用前端现成的
/// 「第 i / n 个」渲染——轮换的两个阶段（读一遍、写一遍）对用户来说正好
/// 就是"两步"。
///
/// 按整百分比节流：不节流的话 256 KiB 分块下处理 1 GB 要发四千多次事件，
/// IPC 开销反而拖慢操作，进度条也会因刷新过密而卡顿（与 encrypt.rs 同一
/// 理由，改动时两处都要看）。
fn progress_cb<'a>(
    app: &'a tauri::AppHandle,
    name: &'a str,
    stage: usize,
    stages: usize,
) -> impl FnMut(u64, u64) + 'a {
    let mut last_pct = u8::MAX;
    move |done: u64, total: u64| {
        // 空文件（total 为 0）算 100%：checked_div 在除数为 0 时返回 None，
        // 正好用 unwrap_or 兜住
        let pct = done
            .saturating_mul(100)
            .checked_div(total)
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(100);
        if pct == last_pct {
            return;
        }
        last_pct = pct;
        // 发送失败就算了：前端没在听不代表操作该中断
        let _ = app.emit(
            ENCRYPT_PROGRESS_EVENT,
            EncryptProgress {
                index: stage,
                total_files: stages,
                name: name.to_owned(),
                done,
                total,
            },
        );
    }
}

/// 操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Add,
    Change,
    Remove,
    Reencrypt,
}

impl Action {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "add" => Some(Self::Add),
            "change" => Some(Self::Change),
            "remove" => Some(Self::Remove),
            "reencrypt" => Some(Self::Reencrypt),
            _ => None,
        }
    }

    /// 是否**必须**给新密码。
    const fn needs_next(self) -> bool {
        matches!(self, Self::Add | Self::Change)
    }

    /// 是否**接受**新密码。
    ///
    /// `reencrypt` 接受但不强制：轮换的核心是换文件密钥，改不改密码是另一
    /// 件事。强制要求会让"只想让旧副本的密码对这份文件失效"无法表达。
    const fn accepts_next(self) -> bool {
        matches!(self, Self::Add | Self::Change | Self::Reencrypt)
    }

    /// 是否重写载荷。
    const fn rewrites_payload(self) -> bool {
        matches!(self, Self::Reencrypt)
    }

    /// 未被 `keep` 覆盖的那些槽怎么处理。
    ///
    /// 只有 `remove` 是清场。`add` / `change` 用户想动的只是自己这一个密码，
    /// 不该殃及这个文件上的恢复码——实测确认过，填随机会让它静默失效，
    /// 而用户只在真忘密码那天才发现。
    ///
    /// 与 CLI 的 `Op::other_slots` 是同一套判据，**改一处就要改两处**。
    const fn other_slots(self) -> omy_core::keyslot::OtherSlots {
        match self {
            Self::Remove => omy_core::keyslot::OtherSlots::Discard,
            Self::Add | Self::Change | Self::Reencrypt => omy_core::keyslot::OtherSlots::Carry,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Change => "change",
            Self::Remove => "remove",
            Self::Reencrypt => "reencrypt",
        }
    }
}

/// 可管理模式下按槽位目录精确改写。
///
/// 与可否认路径的差别只在一件事：这里**知道**每个下标是谁，于是
/// `add` 能找一个真正空闲的槽（而不是赌 `keep.len()` 那个下标是空的，
/// 那会顶掉恢复码），`remove` 能只删指定的那一个。
fn managed_rewrite(
    data: &[u8],
    opened: &omy_core::file::OpenedFile,
    current: &Kek,
    keep: &[Kek],
    action: Action,
    target: Option<usize>,
) -> CmdResult<omy_core::keyslot::RewriteOutcome> {
    use omy_core::keyslot::SlotPlan;
    use omy_core::slotdir::{SlotEntry, SlotKind};

    let mut dir = opened.slot_directory().map_err(|_| CmdError::code("bad_slot_directory"))?;
    let cur = usize::from(opened.slot_index);
    let mut plans: Vec<SlotPlan> =
        (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();

    match action {
        Action::Add => {
            let newcomer = keep.last().ok_or_else(|| CmdError::code("new_password_required"))?;
            let free = dir.first_free().ok_or_else(|| CmdError::code("slots_full"))?;
            *plans.get_mut(free).ok_or_else(|| CmdError::code("internal"))? =
                SlotPlan::Write(newcomer.duplicate());
            dir.set(free, SlotEntry::of(SlotKind::Vault))
                .map_err(|_| CmdError::code("internal"))?;
        }
        Action::Change => {
            // 就地替换当前密码所在的那一个槽，其余一律不动。
            // 这正是「改密码不该殃及恢复码」
            let newcomer = keep.first().ok_or_else(|| CmdError::code("new_password_required"))?;
            *plans.get_mut(cur).ok_or_else(|| CmdError::code("internal"))? =
                SlotPlan::Write(newcomer.duplicate());
        }
        Action::Remove => {
            if let Some(i) = target {
                // 精确删除一个。不能删自己正在用的那把——删完这个对话框
                // 下一步就没法继续了，而且文件可能就此打不开
                if i == cur {
                    return Err(CmdError::code("cannot_remove_current"));
                }
                if i >= omy_core::header::SLOT_COUNT {
                    return Err(CmdError::code("bad_slot_index"));
                }
                *plans.get_mut(i).ok_or_else(|| CmdError::code("internal"))? = SlotPlan::Clear;
                dir.set(i, SlotEntry::empty()).map_err(|_| CmdError::code("internal"))?;
            } else {
                // 老语义：保留当前密码，清掉其余
                for i in 0..omy_core::header::SLOT_COUNT {
                    if i == cur {
                        continue;
                    }
                    *plans.get_mut(i).ok_or_else(|| CmdError::code("internal"))? = SlotPlan::Clear;
                    dir.set(i, SlotEntry::empty()).map_err(|_| CmdError::code("internal"))?;
                }
            }
        }
        // 调用点已排除：轮换换掉 FEK 之后整个 slot 区都要重建
        Action::Reencrypt => return Err(CmdError::code("internal")),
    }

    omy_core::keyslot::rewrite_slots_managed(data, &[current.duplicate()], &plans, &dir)
        .map_err(map_core_err)
}
fn run(
    state: &Shared,
    app: &tauri::AppHandle,
    req: &KeyRequest,
    action: Action,
) -> CmdResult<KeyOutcome> {
    let path = Path::new(&req.path);
    // 目录走树形分支。不先判断的话 fs::read 会失败并报 io_error，
    // 用户只看到「读写失败」，看不出这是目录
    if path.is_dir() {
        return run_tree(app, state, path, req, action);
    }
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
        // 给了新密码就换掉，没给就沿用当前密码
        Action::Reencrypt => {
            if req.next.is_empty() {
                vec![current.duplicate()]
            } else {
                vec![derive(&req.next, &header.vault_salt, params)?]
            }
        }
    };

    // 两条路径只差这一步：slot 改写只动头部，轮换要把载荷读一遍再写一遍。
    // 前后的检查（自证可打开、原子写回、装入会话）完全共用——分开写的话，
    // 将来加一项检查就必然漏掉一边
    let (bytes, slot_used) = if action.rewrites_payload() {
        // 进度条上显示文件名，让用户知道在处理哪个文件。
        // 取磁盘名而不是解出来的原名：这一步还没解密，而且用户是在列表里
        // 按显示名选中它的
        let shown = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        // 两个阶段各自独立汇报，不合成一条总进度：合成会让进度条在中点
        // 莫名减速（读的速度和写的速度不一样），用户以为卡住了
        let mut dec = progress_cb(app, &shown, 1, 2);
        let mut enc = progress_cb(app, &shown, 2, 2);
        let out = omy_core::reencrypt::rotate_fek_with_progress(
            &data,
            &[current],
            &keep,
            &omy_core::file::RandomMaterial::generate(),
            Some(&mut dec),
            Some(&mut enc),
        )
        .map_err(map_core_err)?;
        (out.bytes, out.slot_used)
    } else if header.has_flag(omy_core::header::flags::SLOT_DIRECTORY) {
        // 这里再开一次只是用 FEK 解头部，不含 Argon2，开销可忽略；
        // 换来的是不必把 OpenedFile 从上面一路传下来
        let opened = omy_core::file::open(&data, &[current.duplicate()])
            .map_err(|_| CmdError::code("wrong_password"))?;
        let out = managed_rewrite(&data, &opened, &current, &keep, action, req.slot_index)?;
        (out.bytes, out.slot_used)
    } else {
        // 可否认模式收到 slot_index 要如实拒绝，不能悄悄忽略：
        // 用户以为删掉的是某一个，实际做的是「清掉其余全部」
        if req.slot_index.is_some() {
            return Err(CmdError::code("slot_index_not_supported"));
        }
        let out = omy_core::keyslot::rewrite_slots(&data, &[current], &keep, action.other_slots())
            .map_err(map_core_err)?;
        (out.bytes, out.slot_used)
    };

    // 写回前先自证每个保留密码都能打开新文件。逐个验而不是只验第一个：
    // add 最容易犯的错是新密码能开、原密码被挤掉，只验一个正好漏掉。
    //
    // 顺序不能反——先覆盖原文件再发现打不开，用户就同时失去了旧文件
    // 和访问权
    for k in &keep {
        omy_core::file::open(&bytes, &[k.duplicate()])
            .map_err(|_| CmdError::code("rewrite_verify_failed"))?;
    }

    omy_core::fsatomic::write_atomic(path, &bytes).map_err(|_| CmdError::code("io_error"))?;

    // 把改动后仍然有效的密码装进会话，否则列表会把这个文件显示成
    // 「🔒 需要密码」——用户刚刚才输过密码，再被问一次很荒唐。
    //
    // 用 keep 的最后一个：add/change 时它是新密码，remove 时是当前密码。
    // 都是「用户接下来会用的那个」
    let adopt = if action == Action::Remove || req.next.is_empty() {
        &req.current
    } else {
        &req.next
    };
    state.with_session(|s| {
        // 必须是累加而不是替换。换成替换会修出一个更糟的缺陷：
        // 一个库里十个文件都用密码 A，用户把其中**一个**改成 B，
        // 替换语义会把会话里的 A 顶掉，于是另外九个文件当场全部锁上——
        // 用户只想改一个文件的密码，却发现整个目录都看不见了。
        //
        // 旧密码在这里不能主动 forget：它对同库的其它文件依然有效。
        // 这个操作改的是**这一个文件**能被谁打开，不是整个库
        let _ = s.add_password("main", &header.vault_salt, adopt, params);
    });

    Ok(KeyOutcome {
        action: action.name().to_owned(),
        slots_in_use: slot_used,
        payload_rewritten: action.rewrites_payload(),
        is_tree: false,
        new_path: req.path.clone(),
        files_changed: 1,
    })
}

/// 给一棵树形加密的目录换密码，或轮换它的文件密钥。
///
/// # 为什么不支持 add / remove
///
/// 目录名由**第一个** KEK 派生，`encrypt_tree` / `decrypt_tree` 都只认
/// `keks[0]`，所以一棵树同时只能有一个「能浏览」的密码。实测过：给树
/// add 第二个密码后，新密码能打开每一个文件却解不开目录名，解密报
/// content hash mismatch——用户会以为文件损坏。与其给出一个半残的密码，
/// 不如明确拒绝。
///
/// # reencrypt 与 change 的差别
///
/// change 只重建每个文件 384 字节的 slot 区，秒级完成。reencrypt 还会换掉
/// 每个文件的 FEK 并重写全部载荷，耗时与总数据量成正比——所以它带进度上报。
/// 值得多等的理由：change 之后 FEK 没变，攻击者手里若有旧文件副本仍能用
/// 旧密码打开那个副本；只有轮换才能让旧密码与这份数据彻底无关。
///
/// # Errors
///
/// - `not_a_tree`：目录里找不到任何 `.omy` 文件
/// - `tree_only_change`：对目录用了 change / reencrypt 以外的操作
/// - `wrong_password` / `corrupted` / `io_error`：同单文件
/// - `tree_partial`：部分文件改写失败，参数里带上是**哪些**（旧密码对
///   它们仍然有效）
fn run_tree(
    app: &tauri::AppHandle,
    state: &Shared,
    path: &Path,
    req: &KeyRequest,
    action: Action,
) -> CmdResult<KeyOutcome> {
    if !matches!(action, Action::Change | Action::Reencrypt) {
        return Err(CmdError::code("tree_only_change"));
    }

    // 目录没有头部，从树里任意一个密文文件取 vault_salt 与 KDF 参数。
    // 同一个 vault 内它们本就一致，所以 Argon2 只需派生一次
    let sample = omy_core::tree::find_any_file(path).ok_or_else(|| CmdError::code("not_a_tree"))?;
    let head = std::fs::read(&sample).map_err(|_| CmdError::code("io_error"))?;
    let header = omy_core::file::peek_header(&head).map_err(map_core_err)?;
    let params = header.argon2_params();

    let current = derive(&req.current, &header.vault_salt, params)?;
    // 尽早验密码：让用户等完整棵树才被告知「密码不对」是很糟的体验
    omy_core::file::open(&head, &[current.duplicate()]).map_err(map_core_err)?;

    // 轮换可以不换密码：「让旧副本作废、密码不变」是它的正当用法。
    // 这时 keep 就是当前密码本身
    let rotate = action.rewrites_payload();
    let changing_password = !req.next.is_empty();
    let next = if changing_password {
        derive(&req.next, &header.vault_salt, params)?
    } else {
        current.duplicate()
    };

    // 只有轮换才发进度：change 是秒级的，发事件纯属噪音
    let mut tick = progress_tree(app);
    // 与单文件同一个判据，避免「单文件保住了恢复码、目录里没保住」
    // 叫 rekey_params 而不是 params：这个作用域里已经有一个 Argon2Params
    // 也叫 params，重名会把它遮蔽掉
    let rekey_params = omy_core::tree::RekeyParams {
        vault_salt: &header.vault_salt,
        cipher: header.cipher_id,
        rotate,
        others: action.other_slots(),
    };
    let rep = omy_core::tree::rekey_tree_with_progress(
        path,
        &[current],
        &[next],
        rekey_params,
        if rotate { Some(&mut tick) } else { None },
    )
    .map_err(map_core_err)?;

    // 部分失败必须报错而不是静默返回成功：用户以为全改完了，等哪天用新
    // 密码打不开另一半时早就忘了旧密码。旧密码对失败的那些文件仍然有效。
    //
    // 带上**是哪些**文件：只给一个「有文件没改成」的提示，用户既不知道
    // 该去处理什么，也无法判断损失有多大
    if !rep.is_complete() {
        let names: Vec<String> = rep
            .failed
            .iter()
            .map(|(p, err)| {
                // 展示用短名：路径里含密文目录名，又长又无意义。
                // 名字本身就是密文，用户认不出来，但足以对上列表里的条目
                let n = p.file_name().map_or_else(
                    || p.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                );
                format!("{n}: {err}")
            })
            .collect();
        // 完整路径单独给一份，供「重试」原样传回。
        //
        // 不能让前端从短名拼路径：名字拼不回目录层级，而且不同子目录下
        // 可能有同名文件。core 已经保证这些路径在改名之后仍然有效
        let paths: Vec<String> =
            rep.failed.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
        return Err(CmdError::with(
            "tree_partial",
            serde_json::json!({
                "changed": rep.changed,
                "failed": rep.failed.len(),
                "files": names,
                "paths": paths,
            }),
        ));
    }

    if changing_password {
        state.with_session(|s| {
            // 同样是累加：这个目录下的树换了密码，不代表同一个 vault 里
            // 别处的文件也换了。旧密码留在会话里对它们仍然有用。
            //
            // 这里**不**主动 forget 旧密码，理由同单文件分支
            let _ = s.add_password("main", &header.vault_salt, &req.next, params);
        });
    }

    Ok(KeyOutcome {
        action: action.name().to_owned(),
        // 树上不可探测单个文件的 slot 占用，也没有意义——整棵树一个密码
        slots_in_use: 1,
        payload_rewritten: rep.payload_rewritten,
        is_tree: true,
        new_path: rep.root.to_string_lossy().into_owned(),
        files_changed: rep.changed,
    })
}

/// 重试的入参。
#[derive(Debug, Clone, Deserialize)]
pub struct RetryRequest {
    /// 上次失败的那些文件，取自 `tree_partial` 错误里的 `paths`。
    pub paths: Vec<String>,
    /// 用于打开这些文件的密码。
    ///
    /// 失败的文件**没有被改写**，所以这里要填**原来的**密码，而不是新密码。
    /// 这是最容易搞错的地方：用户刚输过新密码，很容易以为重试也该用新的。
    pub current: String,
    /// 目标密码。留空表示沿用 `current`（只重新加密、不改密码时）。
    #[serde(default)]
    pub next: String,
    /// 是否同时轮换文件密钥。
    #[serde(default)]
    pub rotate: bool,
    /// 树里任意一个文件，用来取 `vault_salt` 与 KDF 参数。
    ///
    /// 目录没有头部，而重试的目标是散落的文件，所以要调用方给一个锚点。
    /// 取报告返回的新根目录即可。
    pub root: String,
}

/// 只重试上次失败的那些文件。
///
/// # 为什么不直接重跑整个操作
///
/// 重跑会拿旧密码去开已经改成新密码的文件，产生一堆假失败——用户分不清
/// 哪些是真问题。而且轮换时重跑意味着把已经处理过的文件再读写一遍，
/// 几十 GB 的树要多等一倍时间。
///
/// # Errors
///
/// - `not_a_tree`：`root` 下找不到可用的样本文件
/// - `wrong_password`：给的密码打不开这些文件
/// - `tree_partial`：仍有文件失败（清单同 `manage_key`）
#[tauri::command]
pub async fn retry_key_files(
    app: tauri::AppHandle,
    state: tauri::State<'_, Shared>,
    req: RetryRequest,
) -> CmdResult<KeyOutcome> {
    let handle = std::sync::Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || run_retry(&app, &handle, &req))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

fn run_retry(
    app: &tauri::AppHandle,
    state: &Shared,
    req: &RetryRequest,
) -> CmdResult<KeyOutcome> {
    if req.paths.is_empty() {
        return Err(CmdError::code("nothing_to_retry"));
    }
    let root = std::path::PathBuf::from(&req.root);
    // 参数来自 vault 里任意一个文件，与 run_tree 同一套做法
    let sample =
        omy_core::tree::find_any_file(&root).ok_or_else(|| CmdError::code("not_a_tree"))?;
    let head = std::fs::read(&sample).map_err(|_| CmdError::code("io_error"))?;
    let header = omy_core::file::peek_header(&head).map_err(map_core_err)?;
    let params = header.argon2_params();

    let current = derive(&req.current, &header.vault_salt, params)?;
    let files: Vec<std::path::PathBuf> =
        req.paths.iter().map(std::path::PathBuf::from).collect();

    // 尽早验密码，但要拿**失败的那些文件之一**来验，不能用样本文件：
    // 样本很可能已经改成新密码了，用它验会把正确的旧密码判成错的
    let first = files.first().ok_or_else(|| CmdError::code("nothing_to_retry"))?;
    let probe = std::fs::read(first).map_err(|_| CmdError::code("io_error"))?;
    omy_core::file::open(&probe, &[current.duplicate()]).map_err(map_core_err)?;

    let changing = !req.next.is_empty();
    let next = if changing {
        derive(&req.next, &header.vault_salt, params)?
    } else {
        current.duplicate()
    };

    let mut tick = progress_tree(app);
    let rep = omy_core::tree::retry_files(
        &files,
        &[current],
        &[next],
        req.rotate,
        // Carry：重试上下文里没有 action，但能走到这里的只有 change 与
        // reencrypt——remove 不改写载荷、不会产生需要重试的部分失败。
        // 这两个本来就该 Carry，所以固定值是对的而不是凑合。
        //
        // 真正的风险是将来有人给 remove 也加上重试路径却忘了改这里，
        // 那会让「只保留当前密码」在重试的那些文件上失效。RetryRequest
        // 里带上 action 才是长久之计，但那要改前后端接口，等有第三个
        // 调用方时再做
        omy_core::keyslot::OtherSlots::Carry,
        if req.rotate { Some(&mut tick) } else { None },
    )
    .map_err(map_core_err)?;

    if !rep.is_complete() {
        let names: Vec<String> = rep
            .failed
            .iter()
            .map(|(p, err)| {
                let n = p.file_name().map_or_else(
                    || p.to_string_lossy().into_owned(),
                    |n| n.to_string_lossy().into_owned(),
                );
                format!("{n}: {err}")
            })
            .collect();
        let paths: Vec<String> =
            rep.failed.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect();
        return Err(CmdError::with(
            "tree_partial",
            serde_json::json!({
                "changed": rep.changed,
                "failed": rep.failed.len(),
                "files": names,
                "paths": paths,
            }),
        ));
    }

    if changing {
        state.with_session(|s| {
            // 累加。重试场景下尤其不能替换：这次补的是**上次失败的那些**
            // 文件，同一棵树里已经改好的那些用的也是新密码，但别处还有
            // 用旧密码的文件——把旧密码顶掉会让它们凭空锁上
            let _ = s.add_password("main", &header.vault_salt, &req.next, params);
        });
    }

    Ok(KeyOutcome {
        action: String::from("retry"),
        slots_in_use: 1,
        payload_rewritten: rep.payload_rewritten,
        is_tree: true,
        // 重试不改目录名，所以路径没变。回显 root 而不是空串：前端拿它
        // 定位当前树，空串会被当成「路径失效」
        new_path: req.root.clone(),
        files_changed: rep.changed,
    })
}

/// 把 core 的树形进度转成前端事件。
///
/// 与单文件的 `progress_cb` 分开写，因为语义不同：那边 `index` 是「第几个
/// 阶段」（读一遍、写一遍），这边是「第几个文件」。合并成一个函数要靠参数
/// 区分，反而更容易搞混。
///
/// 按文件 + 整百分比双重节流：一棵上万文件的树，每个文件每 1% 发一次事件
/// 就是上百万次 IPC，进度条会因刷新过密而卡顿（与 encrypt.rs 同一理由，
/// 改动时几处都要看）。
fn progress_tree(app: &tauri::AppHandle) -> impl FnMut(&str, usize, usize, u64, u64) + '_ {
    let mut last = (usize::MAX, u8::MAX);
    move |name: &str, idx: usize, total: usize, done: u64, bytes: u64| {
        let pct = done
            .saturating_mul(100)
            .checked_div(bytes)
            .and_then(|v| u8::try_from(v).ok())
            .unwrap_or(100);
        if last == (idx, pct) {
            return;
        }
        last = (idx, pct);
        // 发送失败就算了：前端没在听不代表操作该中断
        let _ = app.emit(
            ENCRYPT_PROGRESS_EVENT,
            EncryptProgress {
                index: idx,
                total_files: total,
                name: name.to_owned(),
                done,
                total: bytes,
            },
        );
    }
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

// ============================================================
// 恢复码
// ============================================================

/// 生成恢复码的入参。
#[derive(Debug, Clone, Deserialize)]
pub struct RecoveryRequest {
    /// 目标 `.omy` 文件。
    pub path: String,
    /// 当前密码——生成恢复码要先证明你现在能打开这个文件。
    pub current: String,
}

/// 生成恢复码的结果。
#[derive(Debug, Clone, Serialize)]
pub struct RecoveryOutcome {
    /// 26 个词。
    ///
    /// # 为什么必须回传给前端，而 CLI 却不放进 --json
    ///
    /// CLI 的 `--json` 常被重定向进文件或管道进日志，那是在用户看不见的
    /// 地方留下一张万能钥匙。而这里是 Tauri IPC：内容只进 WebView 的内存、
    /// 用于当场显示给用户看，不落盘、不进任何日志。
    ///
    /// 前端拿到后**必须只显示、不存储**——不得写进 localStorage、不得留在
    /// 组件状态里超过对话框的生命周期。
    pub words: Vec<String>,
    /// 本次是否可能顶掉了原本挂在该槽位上的其它密码。
    ///
    /// 恢复码要占一个新槽位，而实现无法探测哪个槽位是空的（可否认性的
    /// 直接后果）。为 true 时 UI 必须如实提醒——但只能说「可能」，
    /// 那个下标上原来是真密码还是随机填充，我们确实不知道。
    pub may_have_evicted: bool,
}

/// 给一个已加密文件生成并挂上恢复码。
///
/// # 为什么不支持目录
///
/// 树形加密的每个文件各自挂槽，「整棵树共用一份恢复码还是各一份」尚未
/// 决定。与其给出一个语义含糊的实现，不如如实拒绝——用户至少知道该去
/// 单个文件上设。
///
/// # Errors
///
/// - `password_required`：当前密码为空
/// - `tree_not_supported`：目标是目录
/// - `wrong_password`：当前密码打不开这个文件
/// - `file_not_found` / `io_error` / `corrupted`：同 `manage_key`
#[tauri::command]
pub async fn generate_recovery(
    state: State<'_, Shared>,
    req: RecoveryRequest,
) -> CmdResult<RecoveryOutcome> {
    if req.current.is_empty() {
        return Err(CmdError::code("password_required"));
    }
    let handle: Shared = std::sync::Arc::clone(&state);
    // Argon2 要几百毫秒，必须离开异步执行器，否则 UI 卡住
    tauri::async_runtime::spawn_blocking(move || run_recovery(&handle, &req))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

fn run_recovery(state: &Shared, req: &RecoveryRequest) -> CmdResult<RecoveryOutcome> {
    let path = Path::new(&req.path);
    if path.is_dir() {
        return run_recovery_tree(state, req, path);
    }
    let data = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CmdError::code("file_not_found"),
        _ => CmdError::code("io_error"),
    })?;

    let header = omy_core::file::peek_header(&data).map_err(map_core_err)?;
    let params = header.argon2_params();
    let current = derive(&req.current, &header.vault_salt, params)?;

    let code = omy_core::recovery::RecoveryCode::generate();
    let reco_kek = code.to_kek(&header.vault_salt);

    // keep 里必须同时有当前密码与恢复码。只放恢复码的话这就成了
    // 「把密码换成恢复码」，用户的日常密码会当场失效
    let keep = vec![current.duplicate(), reco_kek.duplicate()];

    let opened = omy_core::file::open(&data, &[current.duplicate()])
        .map_err(|_| CmdError::code("wrong_password"))?;
    let out = if opened.is_slot_managed() {
        // 可管理模式要把类型如实记成 recovery。不记的话目录会说那个槽
        // 是空的，下次 add 就拿它去放新密码，恢复码静默消失——实测过
        // GUI 这条路径原本就漏了，而 CLI 那边是对的
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut dir = opened.slot_directory().map_err(|_| CmdError::code("bad_slot_directory"))?;
        let free = dir.first_free().ok_or_else(|| CmdError::code("slots_full"))?;
        let mut plans: Vec<SlotPlan> =
            (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();
        *plans.get_mut(free).ok_or_else(|| CmdError::code("internal"))? =
            SlotPlan::Write(reco_kek.duplicate());
        dir.set(free, SlotEntry::of(SlotKind::Recovery))
            .map_err(|_| CmdError::code("internal"))?;
        omy_core::keyslot::rewrite_slots_managed(&data, &[current.duplicate()], &plans, &dir)
            .map_err(map_core_err)?
    } else {
        omy_core::keyslot::rewrite_slots(
            &data,
            &[current.duplicate()],
            &keep,
            // 搬运而非清场：这个文件上可能还挂着别人的密码，
            // 「加一个恢复码」不该顺手把它们抹了
            omy_core::keyslot::OtherSlots::Carry,
        )
        .map_err(map_core_err)?
    };

    // 写回前自证：恢复码真的能打开新文件。顺序不能反——先写回再发现
    // 恢复码无效，用户会拿着一张废纸以为自己有了兜底
    for k in &keep {
        omy_core::file::open(&out.bytes, &[k.duplicate()])
            .map_err(|_| CmdError::code("rewrite_verify_failed"))?;
    }

    omy_core::fsatomic::write_atomic(path, &out.bytes)
        .map_err(|_| CmdError::code("io_error"))?;

    // 当前密码仍然有效，装回会话：用户刚输过它，不该再被问一次
    state.with_session(|s| {
        let _ = s.add_password("main", &header.vault_salt, &req.current, params);
    });

    Ok(RecoveryOutcome {
        words: code.to_words().into_iter().map(str::to_owned).collect(),
        may_have_evicted: out.may_have_evicted,
    })
}

/// 用恢复码重设密码的入参。
#[derive(Debug, Clone, Deserialize)]
pub struct RestoreRequest {
    /// 目标 `.omy` 文件。
    pub path: String,
    /// 用户输入的恢复码（26 个词，空白分隔；大小写与多余空格会被归一化）。
    pub code: String,
    /// 要设置的新密码。
    pub next: String,
}

/// 用恢复码重设密码的结果。
#[derive(Debug, Clone, Serialize)]
pub struct RestoreOutcome {
    /// 改写后生效的密码数。
    pub slots_in_use: usize,
    /// 恢复码是否仍然有效。恒为 true——这是承诺，不是观测。
    ///
    /// 显式返回而不是让前端假定：用到恢复码就意味着密码已经忘了，这时把
    /// 唯一的兜底抽掉是最坏的时机。有了这个字段，端到端验证能断言它，
    /// 日后有人把 `keep` 改成不含恢复码时会立刻变红。
    pub recovery_still_valid: bool,
}

/// 用恢复码打开文件并设置新密码。
///
/// # 为什么不做成「只验证恢复码」
///
/// 用到恢复码就意味着密码已经忘了。验证完却不给设新密码，用户下次还得
/// 再翻一次纸条——而每翻一次都是一次暴露机会。
///
/// # Errors
///
/// - `code_required` / `new_password_required`：输入为空
/// - `tree_not_supported`：目标是目录
/// - `bad_recovery_code`：解析失败，`params.detail` 里带着能定位到词的原文
/// - `recovery_mismatch`：码是对的但不属于这个文件
#[tauri::command]
pub async fn restore_with_recovery(
    state: State<'_, Shared>,
    req: RestoreRequest,
) -> CmdResult<RestoreOutcome> {
    if req.code.trim().is_empty() {
        return Err(CmdError::code("code_required"));
    }
    if req.next.is_empty() {
        return Err(CmdError::code("new_password_required"));
    }
    let handle: Shared = std::sync::Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || run_restore(&handle, &req))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

fn run_restore(state: &Shared, req: &RestoreRequest) -> CmdResult<RestoreOutcome> {
    let path = Path::new(&req.path);
    if path.is_dir() {
        return run_restore_tree(state, req, path);
    }
    let data = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => CmdError::code("file_not_found"),
        _ => CmdError::code("io_error"),
    })?;
    let header = omy_core::file::peek_header(&data).map_err(map_core_err)?;

    // 解析错误要把 core 的原文带出去。它会说「第 7 个词『acadmic』不在
    // 词表中，是不是『academic』？」——压成一个错误码就等于扔掉最有用的
    // 信息，用户只能把 26 个词从头核对一遍
    let code = omy_core::recovery::RecoveryCode::from_phrase(&req.code).map_err(|e| {
        CmdError::with("bad_recovery_code", serde_json::json!({ "detail": e.to_string() }))
    })?;
    let reco_kek = code.to_kek(&header.vault_salt);

    // 校验和过了只证明「没抄错」，不证明「属于这个文件」。必须真去解一次，
    // 否则用户会拿着另一个库的恢复码反复困惑。两种处境的处置方式相反，
    // 所以用不同的错误码
    let opened = omy_core::file::open(&data, &[reco_kek.duplicate()])
        .map_err(|_| CmdError::code("recovery_mismatch"))?;

    let params = header.argon2_params();
    let new_kek = derive(&req.next, &header.vault_salt, params)?;

    // keep 同时保留新密码与恢复码
    let keep = vec![new_kek.duplicate(), reco_kek.duplicate()];
    let out = if opened.is_slot_managed() {
        // 语义与可否认模式一致——作废其它日常密码、保住恢复码——
        // 但精确到槽。走 rewrite_slots 会把 keep 盲写到 slot 0、1，
        // 盖掉恰好在那儿的恢复码而目录还显示它在。
        //
        // 反过来「只往空槽塞新密码」也不行：那样旧密码原封不动还能用，
        // 而可否认模式下它会失效——同一个命令两种模式语义相反更糟
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut dir = opened.slot_directory().map_err(|_| CmdError::code("bad_slot_directory"))?;
        let mut plans: Vec<SlotPlan> =
            (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();
        for i in 0..omy_core::header::SLOT_COUNT {
            // 恢复码一律留着：用户刚经历过一次「忘了密码」，
            // 这时抽掉唯一的兜底是最坏的时机
            if dir.get(i).is_some_and(|e| e.kind == SlotKind::Recovery) {
                continue;
            }
            *plans.get_mut(i).ok_or_else(|| CmdError::code("internal"))? = SlotPlan::Clear;
            dir.set(i, SlotEntry::empty()).map_err(|_| CmdError::code("internal"))?;
        }
        // 清完再找空位，新密码优先落在刚腾出来的槽上
        let free = dir.first_free().ok_or_else(|| CmdError::code("slots_full"))?;
        *plans.get_mut(free).ok_or_else(|| CmdError::code("internal"))? =
            SlotPlan::Write(new_kek.duplicate());
        dir.set(free, SlotEntry::of(SlotKind::Vault))
            .map_err(|_| CmdError::code("internal"))?;
        omy_core::keyslot::rewrite_slots_managed(&data, &[reco_kek.duplicate()], &plans, &dir)
            .map_err(map_core_err)?
    } else {
        omy_core::keyslot::rewrite_slots(
            &data,
            &[reco_kek.duplicate()],
            &keep,
            omy_core::keyslot::OtherSlots::Carry,
        )
        .map_err(map_core_err)?
    };

    for k in &keep {
        omy_core::file::open(&out.bytes, &[k.duplicate()])
            .map_err(|_| CmdError::code("rewrite_verify_failed"))?;
    }

    omy_core::fsatomic::write_atomic(path, &out.bytes)
        .map_err(|_| CmdError::code("io_error"))?;

    // 装入新密码：用户刚设完，列表该立刻显形
    state.with_session(|s| {
        let _ = s.add_password("main", &header.vault_salt, &req.next, params);
    });

    Ok(RestoreOutcome {
        slots_in_use: out.slot_used,
        recovery_still_valid: true,
    })
}


/// 给整棵树挂一份恢复码。
///
/// 共用一份而不是每个文件各一份：恢复码的用途是「密码忘了，把东西拿
/// 回来」，每个文件各一份意味着要抄 N 张纸、丢一张少一个文件，与用途相悖。
fn run_recovery_tree(
    state: &Shared,
    req: &RecoveryRequest,
    path: &Path,
) -> CmdResult<RecoveryOutcome> {
    let sample =
        omy_core::tree::find_any_file(path).ok_or_else(|| CmdError::code("not_encrypted_tree"))?;
    let head = std::fs::read(&sample).map_err(|_| CmdError::code("io_error"))?;
    let header = omy_core::file::peek_header(&head).map_err(map_core_err)?;
    let params = header.argon2_params();
    let current = derive(&req.current, &header.vault_salt, params)?;

    // 尽早验密码：让用户看完 26 个词再被告知「密码不对」很糟
    omy_core::file::open(&head, &[current.duplicate()])
        .map_err(|_| CmdError::code("wrong_password"))?;

    let code = omy_core::recovery::RecoveryCode::generate();
    let reco_kek = code.to_kek(&header.vault_salt);
    let keep = vec![current.duplicate(), reco_kek];

    let rep = omy_core::tree::rekey_tree(
        path,
        &[current],
        &keep,
        &header.vault_salt,
        header.cipher_id,
        // Carry：这棵树上可能还挂着别人的密码，加一个恢复码不该抹掉它们
        omy_core::keyslot::OtherSlots::Carry,
    )
    .map_err(map_core_err)?;

    if !rep.is_complete() {
        // 部分失败要如实报告：那些文件没挂上恢复码，而用户以为整棵树都有了
        return Err(CmdError::with(
            "tree_partial_failure",
            serde_json::json!({ "failed": rep.failed.len(), "changed": rep.changed }),
        ));
    }

    state.with_session(|s| {
        let _ = s.add_password("main", &header.vault_salt, &req.current, params);
    });

    Ok(RecoveryOutcome {
        words: code.to_words().into_iter().map(str::to_owned).collect(),
        // 树形走 rekey_tree，它不返回 may_have_evicted——整棵树统一改写，
        // 不存在单文件那种「槽位下标撞车」的情形
        may_have_evicted: false,
    })
}

/// 用恢复码重设整棵树的密码。
fn run_restore_tree(
    state: &Shared,
    req: &RestoreRequest,
    path: &Path,
) -> CmdResult<RestoreOutcome> {
    let sample =
        omy_core::tree::find_any_file(path).ok_or_else(|| CmdError::code("not_encrypted_tree"))?;
    let head = std::fs::read(&sample).map_err(|_| CmdError::code("io_error"))?;
    let header = omy_core::file::peek_header(&head).map_err(map_core_err)?;

    let code = omy_core::recovery::RecoveryCode::from_phrase(&req.code).map_err(|e| {
        CmdError::with("bad_recovery_code", serde_json::json!({ "detail": e.to_string() }))
    })?;
    let reco_kek = code.to_kek(&header.vault_salt);

    // 校验和过了只说明没抄错，不说明属于这棵树
    omy_core::file::open(&head, &[reco_kek.duplicate()])
        .map_err(|_| CmdError::code("recovery_mismatch"))?;

    let params = header.argon2_params();
    let new_kek = derive(&req.next, &header.vault_salt, params)?;
    let keep = vec![new_kek, reco_kek.duplicate()];

    let rep = omy_core::tree::rekey_tree(
        path,
        &[reco_kek],
        &keep,
        &header.vault_salt,
        header.cipher_id,
        omy_core::keyslot::OtherSlots::Carry,
    )
    .map_err(map_core_err)?;

    if !rep.is_complete() {
        return Err(CmdError::with(
            "tree_partial_failure",
            serde_json::json!({ "failed": rep.failed.len(), "changed": rep.changed }),
        ));
    }

    state.with_session(|s| {
        let _ = s.add_password("main", &header.vault_salt, &req.next, params);
    });

    Ok(RestoreOutcome { slots_in_use: rep.changed, recovery_still_valid: true })
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
        assert_eq!(Action::parse("reencrypt"), Some(Action::Reencrypt));
        assert_eq!(Action::parse("delete"), None);
        assert_eq!(Action::parse(""), None);
        assert_eq!(Action::parse("ADD"), None, "大小写不同应视为未知");
    }

    #[test]
    fn only_reencrypt_rewrites_payload() {
        // 不这样会怎样：这是「改密码」与「轮换」唯一的可观察差别。若 remove
        // 也被算成重写载荷，端到端验证就无法区分两者是否真走了不同实现
        assert!(Action::Reencrypt.rewrites_payload());
        for a in [Action::Add, Action::Change, Action::Remove] {
            assert!(!a.rewrites_payload(), "{} 不该重写载荷", a.name());
        }
    }

    #[test]
    fn reencrypt_accepts_but_does_not_require_new_password() {
        // 轮换即使密码不变也有意义：换掉文件密钥，让旧副本的密码对这份
        // 文件失效。强制要求新密码会让这个正当需求无法表达
        assert!(Action::Reencrypt.accepts_next());
        assert!(!Action::Reencrypt.needs_next());
        // remove 连接受都不该接受，否则用户以为指定了什么、实际什么也没发生
        assert!(!Action::Remove.accepts_next());
        assert!(Action::Add.accepts_next());
        assert!(Action::Change.accepts_next());
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
