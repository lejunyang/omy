//! `omy key`：密钥 slot 管理。
//!
//! # `key list` 为什么什么都不显示
//!
//! slot 区始终填满 8 个（真实 + 随机填充），设计上无法探测实际用了几个。
//! 因此 `key list` 只能告知「共 8 个 slot，内容不可探测」——
//! 这不是功能缺失，而是可否认性（决策 D-02）的直接结果。
//!
//! # 为什么 add 也要问「其余密码」
//!
//! 同一个原因。既然分不清空槽和别人的槽，就无法「挑个空位填进去」——
//! 猜错会静默覆盖掉另一个密码。所以这三个命令要求调用方声明**本次操作后
//! 应当能打开此文件的密码**：
//!
//! - `add`：解锁用的密码 + 新密码，都保留
//! - `change`：只保留新密码，解锁用的那个作废
//! - `remove`：只保留解锁用的密码，其余全部作废
//!
//! # 但「其余密码」不等于「文件上原有的全部密码」
//!
//! `add` / `change` 走原样搬运（core 的 `OtherSlots::Carry`）：没在命令里
//! 出现的槽会被逐字节保留，而不是填成随机。所以用户**不必**穷举这个文件上
//! 挂过的每一个密码——他不知道、也查不出来的那些（典型是恢复码）会自动
//! 留下。这解决了一个实测到的真缺陷：改一次密码就永久失去恢复码。
//!
//! 只有 `remove` 是清场。它的语义必须在提示里讲清楚：不是「删掉某一个
//! 密码」，而是「只留下我现在用的这个」，**包括用户可能设过的恢复码**。
//!
//! 代价是搬运只能无差别：想清掉某个特定协作者的密码，本命令做不到，
//! 只能整体 `remove` 或 `key reencrypt`。

use super::Ctx;
use crate::i18n::t;
use crate::password::{PasswordSource, read_password};
use anyhow::{Context as _, Result, bail};
use clap::{Args as ClapArgs, Subcommand};
use omy_core::crypto::Kek;
use serde_json::json;
use std::path::PathBuf;

/// `key` 的子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 添加密码 slot（需已知一个现有密码）
    Add(SlotArgs),
    /// 只保留当前密码，作废该文件上的其它密码
    Remove(SlotArgs),
    /// 显示 slot 占用情况
    List(ListArgs),
    /// 修改密码（旧密码作废）
    Change(SlotArgs),
    /// 重新加密：换掉文件密钥并重写载荷（可同时改密码）
    Reencrypt(SlotArgs),
    /// 生成恢复码并挂到文件上（忘记密码时的唯一退路）
    Recovery(RecoveryArgs),
    /// 用恢复码打开文件并设置新密码
    Restore(RestoreArgs),
    /// 管理设备密钥（用 Windows Hello 免密解锁）
    Device(DeviceArgs),
}

/// 设备密钥的操作。
#[derive(Debug, ClapArgs)]
pub struct DeviceArgs {
    /// 目标文件
    pub file: PathBuf,

    /// 做什么：`add` 挂上，`remove` 移除，`status` 查看
    #[arg(value_enum, default_value = "status")]
    pub action: DeviceAction,

    /// 从环境变量读取现有密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取现有密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取现有密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// 设备密钥的动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum DeviceAction {
    /// 挂上设备密钥。
    Add,
    /// 移除设备密钥（文件上的槽位与硬件里的密钥一起清）。
    Remove,
    /// 查看这台机器上有没有为这个库保管设备密钥。
    Status,
}

/// slot 操作的公共参数。
#[derive(Debug, ClapArgs)]
pub struct SlotArgs {
    /// 目标文件
    pub file: PathBuf,

    /// 从文件读取现有密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从环境变量读取现有密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从标准输入读取现有密码
    #[arg(long)]
    pub password_stdin: bool,

    /// 新密码来自文件（`add` / `change` 用）
    #[arg(long, value_name = "PATH")]
    pub new_password_file: Option<PathBuf>,

    /// 新密码来自环境变量（传变量名，`add` / `change` 用）
    #[arg(long, value_name = "VAR")]
    pub new_password_env: Option<String>,
}

/// `key recovery` 的参数。
#[derive(Debug, ClapArgs)]
pub struct RecoveryArgs {
    /// 目标文件
    pub file: PathBuf,

    /// 从文件读取现有密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从环境变量读取现有密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从标准输入读取现有密码
    #[arg(long)]
    pub password_stdin: bool,

    /// 把生成的恢复码写到文件而不是打印到终端
    ///
    /// 终端会留在历史记录、滚动缓冲区里，有时还会被终端复用器持久化。
    /// 需要程序化保存时用这个，但**文件本身就是一张明文纸条**，
    /// 请立刻转移到安全的地方。
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// `key restore` 的参数。
#[derive(Debug, ClapArgs)]
pub struct RestoreArgs {
    /// 目标文件
    pub file: PathBuf,

    /// 恢复码来自文件（一行 26 个词，空白分隔）
    #[arg(long, value_name = "PATH")]
    pub code_file: Option<PathBuf>,

    /// 恢复码来自环境变量（传变量名）
    #[arg(long, value_name = "VAR")]
    pub code_env: Option<String>,

    /// 新密码来自文件
    #[arg(long, value_name = "PATH")]
    pub new_password_file: Option<PathBuf>,

    /// 新密码来自环境变量（传变量名）
    #[arg(long, value_name = "VAR")]
    pub new_password_env: Option<String>,
}

/// `key list` 的参数。
#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// 目标文件
    pub file: PathBuf,

    /// 从环境变量读取密码（传变量名）
    ///
    /// 只对可管理模式有用：那时槽位目录是加密的，读它要先能打开文件。
    /// 可否认模式下给密码也换不来任何信息——槽位设计上就不可探测。
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// 执行 `key` 子命令。
///
/// # Errors
///
/// 文件读取失败、密码不匹配、写回失败时返回错误。
pub fn run(ctx: &Ctx<'_>, c: &Cmd) -> Result<()> {
    match c {
        Cmd::List(a) => list(ctx, a),
        Cmd::Add(a) => modify(ctx, a, Op::Add),
        Cmd::Remove(a) => modify(ctx, a, Op::Remove),
        Cmd::Change(a) => modify(ctx, a, Op::Change),
        Cmd::Reencrypt(a) => modify(ctx, a, Op::Reencrypt),
        Cmd::Recovery(a) => recovery(ctx, a),
        Cmd::Restore(a) => restore(ctx, a),
        Cmd::Device(a) => device(ctx, a),
    }
}

/// 生成恢复码并挂到文件上。
///
/// # 为什么恢复码必须当场显示、且只显示这一次
///
/// 它是从随机熵新生成的，我们**不保存明文**——保存了就等于在磁盘上留了
/// 一张万能钥匙。所以用户错过这一次就只能重新生成一份（旧的随之作废）。
///
/// # Errors
///
/// 读写失败、现有密码不正确、槽位已满时返回错误。
fn recovery(ctx: &Ctx<'_>, a: &RecoveryArgs) -> Result<()> {
    if a.file.is_dir() {
        return recovery_tree(ctx, a);
    }
    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;
    let h = omy_core::file::peek_header(&data)?;

    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let old = read_password(&src, t("prompt.password"), false)?;
    let old_kek = Kek::from_password(&old, &h.vault_salt, h.argon2_params())?;
    // 尽早验密码：让用户看完一长串恢复码再被告知「密码不对」很糟。
    // 顺带留着 opened——可管理模式要从它读槽位目录
    let opened = omy_core::file::open(&data, &[old_kek.duplicate()])?;

    let code = omy_core::recovery::RecoveryCode::generate();
    let reco_kek = code.to_kek(&h.vault_salt);

    // keep 里必须同时有当前密码与恢复码：只放恢复码的话，这条命令就成了
    // 「把密码换成恢复码」，用户的日常密码会当场失效
    let out = if opened.is_slot_managed() {
        // 可管理模式：找一个真正空闲的槽，并把类型如实记成 recovery。
        //
        // 不记的话目录会说那个槽是空的，下次 add 就会拿它去放新密码，
        // 恢复码静默消失——这种不一致正是 CRITICAL 标志要防的
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut dir = opened.slot_directory()?;
        let free = dir.first_free().ok_or_else(|| {
            anyhow::anyhow!("8 个槽位已全部占用，放不下恢复码。先用 key remove 腾出一个。")
        })?;
        let mut plans: Vec<SlotPlan> =
            (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();
        *plans.get_mut(free).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
            SlotPlan::Write(reco_kek.duplicate());
        dir.set(free, SlotEntry::of(SlotKind::Recovery))?;
        ctx.out.detail(&format!("恢复码写入 slot {free}，已记入槽位目录"));
        omy_core::keyslot::rewrite_slots_managed(
            &data,
            &[old_kek.duplicate()],
            &plans,
            &dir,
        )?
    } else {
        let keep = vec![old_kek.duplicate(), reco_kek.duplicate()];
        omy_core::keyslot::rewrite_slots(
            &data,
            &[old_kek.duplicate()],
            &keep,
            // 搬运而非清场：这个文件上可能还挂着别人的密码，
            // 「加一个恢复码」不该顺手把它们抹了
            omy_core::keyslot::OtherSlots::Carry,
        )?
    };
    let keep = vec![old_kek, reco_kek];

    // 写回前自证：恢复码真的能打开新文件。顺序不能反——先写回再发现
    // 恢复码无效，用户会拿着一张废纸以为自己有了兜底
    verify_reopenable(&out.bytes, &keep)?;

    if out.may_have_evicted {
        ctx.out.warn(
            "恢复码占用的槽位上原本可能挂着别的密码，若有则已失效。\
             这是格式限制：无法探测哪个槽位是空的。",
        );
    }

    omy_core::fsatomic::write_atomic(&a.file, &out.bytes)
        .with_context(|| format!("写回 {} 失败", a.file.display()))?;

    let phrase = code.to_phrase();
    if let Some(p) = &a.out {
        std::fs::write(p, format!("{phrase}\n"))
            .with_context(|| format!("写入 {} 失败", p.display()))?;
        ctx.out.warn(&format!(
            "恢复码已写入 {}。该文件是明文，请立即转移到安全的地方并删除原件。",
            p.display()
        ));
    }

    // 三条警告都要给，且必须在显示恢复码之后——放前面会被一长串词冲到
    // 屏幕外面
    let human = format!(
        "已为 {} 生成恢复码。\n\n\
         {}\n\n\
         ⚠️  这串词只显示这一次，我们不保存它的明文。\n\
         ⚠️  它是整个 vault 的强度下限——谁拿到这张纸，谁就能打开这个文件。\n\
         ⚠️  用它打开文件后，文件列表不会自动显形（恢复码不参与目录扫描），\n\
             请用 `omy key restore` 设置一个新密码。",
        a.file.display(),
        wrap_words(&phrase),
    );
    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "operation": "recovery",
            // 恢复码本身**不**进 JSON：--json 的输出常被重定向到文件或
            // 管道进日志，那等于把万能钥匙写进了一个谁也没在保护的地方。
            // 需要程序化保存的用 --out，它至少是用户显式指定的路径
            "words": omy_core::recovery::WORD_COUNT,
            "written_to": a.out.as_ref().map(|p| p.display().to_string()),
        }),
    );
    Ok(())
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
/// 恢复码解析失败（会指出第几个词可疑）、打不开文件、写回失败时返回错误。
fn restore(ctx: &Ctx<'_>, a: &RestoreArgs) -> Result<()> {
    if a.file.is_dir() {
        return restore_tree(ctx, a);
    }
    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;
    let h = omy_core::file::peek_header(&data)?;

    let csrc = PasswordSource {
        env: a.code_env.clone(),
        file: a.code_file.clone(),
        stdin: false,
    };
    let raw = read_password(&csrc, "恢复码（26 个词，空格分隔）", false)?;
    let phrase = String::from_utf8_lossy(&raw).into_owned();

    // 解析错误要原样透出：core 的 ParseError 会指出「第 7 个词不在词表，
    // 是不是 academic」，把它压成「恢复码无效」等于扔掉最有用的信息
    let code = omy_core::recovery::RecoveryCode::from_phrase(&phrase)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let reco_kek = code.to_kek(&h.vault_salt);

    // 校验和过了不代表这份恢复码属于这个文件——它只证明「没抄错」。
    // 必须真去解一次，否则用户会拿着另一个库的恢复码反复困惑
    let opened = omy_core::file::open(&data, &[reco_kek.duplicate()]).context(
        "这份恢复码打不开该文件。校验和是对的，说明没抄错，\
         但它可能属于另一个库",
    )?;
    let managed = opened.is_slot_managed();
    let reco_slot = usize::from(opened.slot_index);
    let slot_dir = if managed { opened.slot_directory().ok() } else { None };
    ctx.out.detail("恢复码有效，已解开文件");

    let nsrc = PasswordSource {
        env: a.new_password_env.clone(),
        file: a.new_password_file.clone(),
        stdin: false,
    };
    let pw = read_password(&nsrc, t("prompt.new_password"), nsrc.is_interactive())?;
    let new_kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;

    // keep 同时保留新密码与恢复码：用户刚经历过一次「忘了密码」，
    // 这时把他唯一的兜底抽掉是最坏的时机
    let keep = vec![new_kek.duplicate(), reco_kek.duplicate()];
    let out = if let Some(mut dir) = slot_dir {
        // 可管理模式：语义与可否认模式一致——作废其它日常密码、保住
        // 恢复码——但精确到槽。
        //
        // 不能直接走 rewrite_slots：它把 keep 顺序写到 slot 0、1，会盖掉
        // 恰好在那儿的恢复码，而目录还显示它在。也不能只往空槽塞新密码：
        // 那样旧密码原封不动还能用，而可否认模式下它是会失效的——同一个
        // 命令在两种模式下语义相反，比原缺陷更糟。用户用 restore 正是
        // 因为旧密码忘了或可能已泄露。
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut plans: Vec<SlotPlan> =
            (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();
        let mut cleared = 0usize;
        for i in 0..omy_core::header::SLOT_COUNT {
            // 恢复码一律留着：用户刚经历过一次「忘了密码」，
            // 这时抽掉他唯一的兜底是最坏的时机
            if dir.get(i).is_some_and(|e| e.kind == SlotKind::Recovery) {
                continue;
            }
            if dir.get(i).is_some_and(|e| e.kind.is_occupied()) {
                cleared = cleared.saturating_add(1);
            }
            *plans.get_mut(i).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? = SlotPlan::Clear;
            dir.set(i, SlotEntry::empty())?;
        }
        // 清完再找空位，这样新密码优先落在刚腾出来的槽上
        let free = dir.first_free().ok_or_else(|| {
            anyhow::anyhow!("8 个槽位都被恢复码占着，放不下新密码")
        })?;
        *plans.get_mut(free).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
            SlotPlan::Write(new_kek.duplicate());
        dir.set(free, SlotEntry::of(SlotKind::Vault))?;
        if cleared > 0 {
            ctx.out
                .warn(&format!("该文件上原有的 {cleared} 个日常密码已作废（恢复码保留）"));
        }
        ctx.out
            .detail(&format!("新密码写入 slot {free}，恢复码仍在 slot {reco_slot}"));
        omy_core::keyslot::rewrite_slots_managed(&data, &[reco_kek.duplicate()], &plans, &dir)?
    } else {
        omy_core::keyslot::rewrite_slots(
            &data,
            &[reco_kek],
            &keep,
            omy_core::keyslot::OtherSlots::Carry,
        )?
    };
    verify_reopenable(&out.bytes, &keep)?;
    omy_core::fsatomic::write_atomic(&a.file, &out.bytes)
        .with_context(|| format!("写回 {} 失败", a.file.display()))?;

    let human = format!(
        "已用恢复码重设 {} 的密码。\n\n\
         恢复码        仍然有效，请继续保管好\n\
         生效密码数    {}\n\
         载荷          未改动（仅重写 slot 区与头部 MAC）",
        a.file.display(),
        out.slot_used,
    );
    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "operation": "restore",
            "slots_in_use": out.slot_used,
            "recovery_still_valid": true,
        }),
    );
    Ok(())
}

/// 给整棵树挂一份恢复码。
///
/// # 为什么整棵树共用一份，而不是每个文件各一份
///
/// 恢复码的用途是「密码忘了，把东西拿回来」。每个文件各一份意味着
/// 用户要抄 N 张纸，而且丢一张就少一个文件——这与它的用途相悖。
///
/// 共用一份之所以可行，是因为目录名改用了两层结构：随机目录密钥加密
/// 目录名，每把钥匙各包一份放在边车里。恢复码只是「又一把钥匙」。
///
/// # Errors
///
/// 不是密文树、密码不对、部分文件改写失败时返回错误。
fn recovery_tree(ctx: &Ctx<'_>, a: &RecoveryArgs) -> Result<()> {
    // 目录没有头部，从树里任意一个密文文件取 vault_salt 与 KDF 参数
    let Some(sample) = omy_core::tree::find_any_file(&a.file) else {
        bail!(
            "{} 看起来不是树形加密的目录（里面找不到任何 .omy 文件）",
            a.file.display()
        );
    };
    let head = read_prefix(&sample, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;

    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let old = read_password(&src, t("prompt.password"), false)?;
    let old_kek = Kek::from_password(&old, &h.vault_salt, h.argon2_params())?;

    // 尽早验密码：让用户看完一长串恢复码再被告知「密码不对」很糟。
    // 拿样本文件试，成本是一次小文件读取
    let sample_data = std::fs::read(&sample)
        .with_context(|| format!("读取 {} 失败", sample.display()))?;
    omy_core::file::open(&sample_data, &[old_kek.duplicate()])
        .context("现有密码不正确（用树里的一个文件验证过）")?;

    let code = omy_core::recovery::RecoveryCode::generate();
    let reco_kek = code.to_kek(&h.vault_salt);

    // keep 必须同时含当前密码与恢复码，理由同单文件：只放恢复码的话
    // 这条命令就成了「把密码换成恢复码」
    let keep = vec![old_kek.duplicate(), reco_kek];
    let rep = omy_core::tree::rekey_tree(
        &a.file,
        &[old_kek],
        &keep,
        &h.vault_salt,
        h.cipher_id,
        // Carry：这棵树上可能还挂着别人的密码，「加一个恢复码」
        // 不该顺手把它们抹了
        omy_core::keyslot::OtherSlots::Carry,
    )?;

    // 部分失败要如实报告：那些文件上没挂上恢复码，而用户以为整棵树都有了
    if !rep.is_complete() {
        for (path, why) in &rep.failed {
            ctx.out.warn(&format!("  {} — {why}", path.display()));
        }
        bail!(
            "{} 个文件没能挂上恢复码（共 {} 个）。已挂上的那些是有效的，\n\
             修掉上面的原因后重跑即可给剩下的补上。",
            rep.failed.len(),
            rep.changed.saturating_add(rep.failed.len()),
        );
    }

    let phrase = code.to_phrase();
    if let Some(p) = &a.out {
        std::fs::write(p, format!("{phrase}\n"))
            .with_context(|| format!("写入 {} 失败", p.display()))?;
        ctx.out.warn(&format!(
            "恢复码已写入 {}。该文件是明文，请立即转移到安全的地方并删除原件。",
            p.display()
        ));
    }

    let human = format!(
        "已为 {} 整棵树生成恢复码（{} 个文件）。\n\n\
         {}\n\n\
         ⚠️  这串词只显示这一次，我们不保存它的明文。\n\
         ⚠️  它是整棵树的强度下限——谁拿到这张纸，谁就能打开全部内容。\n\
         ⚠️  每个密文目录里的 .omy-keys 是解开目录名的钥匙，删掉它\n\
             那个目录的名字就再也解不开了。",
        a.file.display(),
        rep.changed,
        wrap_words(&phrase),
    );
    ctx.out.result(
        &human,
        &json!({
            "path": a.file.display().to_string(),
            "mode": "tree",
            "operation": "recovery",
            "files_changed": rep.changed,
            // 恢复码本身不进 JSON，理由同单文件
            "words": omy_core::recovery::WORD_COUNT,
            "written_to": a.out.as_ref().map(|p| p.display().to_string()),
        }),
    );
    Ok(())
}

/// 用恢复码打开整棵树并设置新密码。
///
/// # Errors
///
/// 恢复码解析失败、打不开这棵树、部分文件改写失败时返回错误。
fn restore_tree(ctx: &Ctx<'_>, a: &RestoreArgs) -> Result<()> {
    let Some(sample) = omy_core::tree::find_any_file(&a.file) else {
        bail!(
            "{} 看起来不是树形加密的目录（里面找不到任何 .omy 文件）",
            a.file.display()
        );
    };
    let head = read_prefix(&sample, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;

    let csrc = PasswordSource {
        env: a.code_env.clone(),
        file: a.code_file.clone(),
        stdin: false,
    };
    let raw = read_password(&csrc, "恢复码（26 个词，空格分隔）", false)?;
    let phrase = String::from_utf8_lossy(&raw).into_owned();
    // 解析错误原样透出：core 会指出「第 7 个词不在词表，是不是 academic」，
    // 压成一句「恢复码无效」等于把最有用的信息扔掉
    let code = omy_core::recovery::RecoveryCode::from_phrase(&phrase)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let reco_kek = code.to_kek(&h.vault_salt);

    // 校验和过了只说明「没抄错」，不说明「属于这棵树」。真去解一次
    let sample_data = std::fs::read(&sample)
        .with_context(|| format!("读取 {} 失败", sample.display()))?;
    omy_core::file::open(&sample_data, &[reco_kek.duplicate()]).context(
        "这串词本身没有抄错，但它打不开这棵树——可能属于另一个库，或属于更早生成的一份",
    )?;

    let nsrc = PasswordSource {
        env: a.new_password_env.clone(),
        file: a.new_password_file.clone(),
        stdin: false,
    };
    let pw = read_password(&nsrc, t("prompt.new_password"), nsrc.is_interactive())?;
    let new_kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;

    // keep 保留恢复码：用到它就意味着密码已经忘过一次，这时抽掉唯一的
    // 兜底是最坏的时机
    let keep = vec![new_kek, reco_kek.duplicate()];
    let rep = omy_core::tree::rekey_tree(
        &a.file,
        &[reco_kek],
        &keep,
        &h.vault_salt,
        h.cipher_id,
        omy_core::keyslot::OtherSlots::Carry,
    )?;

    if !rep.is_complete() {
        for (path, why) in &rep.failed {
            ctx.out.warn(&format!("  {} — {why}", path.display()));
        }
        bail!(
            "{} 个文件没能设上新密码（共 {} 个）。恢复码对它们仍然有效，\n\
             修掉上面的原因后重跑即可。",
            rep.failed.len(),
            rep.changed.saturating_add(rep.failed.len()),
        );
    }

    let human = format!(
        "已用恢复码重设 {} 的密码（{} 个文件）。\n\n\
         恢复码      仍然有效\n\
         目录名      未改变（由随机目录密钥加密，与密码无关）\n\
         载荷        未改动（仅重写 slot 区与边车）",
        a.file.display(),
        rep.changed,
    );
    ctx.out.result(
        &human,
        &json!({
            "path": a.file.display().to_string(),
            "mode": "tree",
            "operation": "restore",
            "files_changed": rep.changed,
            "recovery_still_valid": true,
        }),
    );
    Ok(())
}

/// 把恢复码按每行 4 词排版，并加上行号。
///
/// 26 个词排成一行没法抄——用户会数不清抄到哪个了。加行号让他能对照着
/// 逐行核对，这正是「第 7 个词有问题」这类提示能被用上的前提。
fn wrap_words(phrase: &str) -> String {
    let words: Vec<&str> = phrase.split_whitespace().collect();
    words
        .chunks(4)
        .enumerate()
        .map(|(row, chunk)| {
            let start = row.saturating_mul(4).saturating_add(1);
            format!("  {start:>2}. {}", chunk.join("  "))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// slot 操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Remove,
    Change,
    Reencrypt,
}

impl Op {
    /// 是否**必须**输入一个新密码。
    ///
    /// `reencrypt` 不在其中：轮换的核心是换掉文件密钥，换不换密码是另一
    /// 件事。强制要求新密码会让「我只想让旧副本的密码失效、密码不变」
    /// 这个正当需求无法表达。
    const fn needs_new_password(self) -> bool {
        matches!(self, Self::Add | Self::Change)
    }

    /// 是否**接受**新密码（可选传入）。
    const fn accepts_new_password(self) -> bool {
        matches!(self, Self::Add | Self::Change | Self::Reencrypt)
    }

    /// 本次要不要去读新密码。
    ///
    /// 单文件与目录两条路径都用它。之前各写各的，目录那条漏了「显式给了
    /// 新密码来源」这一支，于是 `key reencrypt <目录> --new-password-file`
    /// 的新密码被**静默忽略**：命令报成功，密码其实没换。
    ///
    /// reencrypt 只在用户显式给了来源时才读，不主动追问——轮换本身不要求
    /// 改密码，追问会让「只想换文件密钥」的用户以为必须换。
    fn wants_new_password(self, a: &SlotArgs) -> bool {
        self.needs_new_password()
            || (self.accepts_new_password()
                && (a.new_password_file.is_some() || a.new_password_env.is_some()))
    }

    /// 是否重写载荷。
    ///
    /// 决定两件事：要不要警告耗时、输出里的 `payload_rewritten` 取值。
    const fn rewrites_payload(self) -> bool {
        matches!(self, Self::Reencrypt)
    }

    /// 未被 `keep` 覆盖的那些槽怎么处理。
    ///
    /// 只有 `remove` 是清场。`add` / `change` 用户想动的只是自己这一个
    /// 密码，不该殃及这个文件上的恢复码——实测确认过，填随机会让它静默
    /// 失效，而用户只在真忘密码那天才发现。
    ///
    /// 把它做成 `Op` 的方法而不是在调用点现写：调用点有单文件和树形两处，
    /// 分开写迟早出现「单文件保住了、目录里没保住」这种不一致。
    const fn other_slots(self) -> omy_core::keyslot::OtherSlots {
        match self {
            Self::Remove => omy_core::keyslot::OtherSlots::Discard,
            Self::Add | Self::Change | Self::Reencrypt => omy_core::keyslot::OtherSlots::Carry,
        }
    }

    /// 命令名，用于输出。
    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Change => "change",
            Self::Reencrypt => "reencrypt",
        }
    }
}

fn list(ctx: &Ctx<'_>, a: &ListArgs) -> Result<()> {
    // 目录走树形分支。不先判断的话 read_prefix 会返回一个含糊的 IO
    // 错误，用户看不出这是「该用树形方式」
    if a.file.is_dir() {
        return list_tree(ctx, a);
    }
    let head = read_prefix(&a.file, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;
    let managed = h.has_flag(omy_core::header::flags::SLOT_DIRECTORY);

    // 可管理模式且给了密码：列出每个槽位的类型。
    //
    // 需要密码不是不便，正是设计要的：目录是 ENCRYPTED 的，读它要先有
    // FEK。对打不开这个文件的人，两种模式保护的东西一样多。
    if managed && (a.password_env.is_some() || a.password_file.is_some() || a.password_stdin) {
        return list_managed(ctx, a, &h);
    }

    let (note, used) = if managed {
        (
            "这个文件是可管理模式：槽位类型可以列出，但需要密码——\n\
             槽位目录是加密的，读它要先能打开这个文件。\n\
             加上 --password-env / --password-file 再试一次。",
            t("info.slots_unknown"),
        )
    } else {
        (
            "slot 区始终填满，真实 slot 与随机填充在字节层面无法区分。\n\
             这是可否认性设计的基础：无法判断该文件配置了几个密码。",
            t("info.slots_unknown"),
        )
    };

    let human = format!(
        "文件        {}\n槽位模式    {}\nSlot 总数   {}\nSlot 占用   {}\n\n{}",
        a.file.display(),
        if managed { "可管理（managed）" } else { "可否认（deniable）" },
        h.slot_count,
        used,
        note,
    );

    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "slot_mode": if managed { "managed" } else { "deniable" },
            "slot_total": h.slot_count,
            // 可否认模式下恒为 null：设计上不可探测。
            // 可管理模式下没给密码也是 null，加上密码才有值
            "slot_used": serde_json::Value::Null,
        }),
    );
    Ok(())
}

/// 服务名：TPM 里的密钥都挂在这个名下，避免与别的应用撞名。
const DEVICE_SERVICE: &str = "omy";

/// 管理设备密钥。
///
/// # 为什么它是 slot 而不是「记住密码」
///
/// 记住密码要把密码本身存起来，那等于把最高权限的凭据落盘。设备密钥
/// 存的是一把**随机 KEK**，它只能开这个库，泄露了也拿不到用户的密码
/// ——而用户往往在别处也用同一个密码。
///
/// # Errors
///
/// 这台机器没有 TPM、密码不对、文件读写失败时返回错误。
fn device(ctx: &Ctx<'_>, a: &DeviceArgs) -> Result<()> {
    // 目录也要支持：树形的每个文件都用同一个 vault_salt，
    // 所以设备密钥对整棵树是一把
    let sample = if a.file.is_dir() {
        omy_core::tree::find_any_file(&a.file).ok_or_else(|| {
            anyhow::anyhow!(
                "{} 看起来不是加密过的文件夹（里面找不到任何 .omy 文件）",
                a.file.display()
            )
        })?
    } else {
        a.file.clone()
    };
    let head = read_prefix(&sample, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;
    let id = omy_core::devicekey::slot_id(&h.vault_salt);

    match a.action {
        DeviceAction::Status => device_status(ctx, a, &id),
        DeviceAction::Add => device_add(ctx, a, &h, &sample, &id),
        DeviceAction::Remove => device_remove(ctx, a, &id),
    }
}

/// 造一个 Hello 保管器，并把「这台机器没有 TPM」翻译成人话。
fn hello_protector() -> Result<omy_secret::HelloProtector> {
    omy_secret::HelloProtector::new(DEVICE_SERVICE).map_err(|e| {
        anyhow::anyhow!(
            "这台机器上用不了设备密钥：{e}\n\n\
             设备密钥需要 TPM 2.0 与已配置的 Windows Hello。\n\
             目前只支持 Windows。"
        )
    })
}

fn device_status(ctx: &Ctx<'_>, a: &DeviceArgs, id: &str) -> Result<()> {
    // 探测本身不该弹 Hello——用户只是想看看状态。
    // 所以只问「有没有这条记录」，不去解封它
    let p = match hello_protector() {
        Ok(p) => p,
        Err(e) => {
            ctx.out.result(
                &format!("{e}"),
                &json!({
                    "path": a.file.display().to_string(),
                    "available": false,
                    "enrolled": false,
                }),
            );
            return Ok(());
        }
    };
    // retrieve 会弹 Hello，所以这里不能用它判断「有没有挂过」。
    // has() 由各后端覆盖成「不弹窗的存在性检查」：Windows 上只看密文文件在不在，
    // 钥匙串后端则回退到 retrieve（那些后端本来就不弹窗）。
    // 曾误写成 retrieve().is_ok()，导致 `omy key device status` 一跑就弹一次
    // 指纹确认——用户只是想看看状态。
    let enrolled = omy_secret::Protector::has(&p, id);
    let human = if enrolled {
        format!(
            "文件        {}\n设备密钥    已挂载（{}）\n\n\
             解锁时会要求 Windows Hello 确认。",
            a.file.display(),
            omy_secret::Protector::name(&p),
        )
    } else {
        format!(
            "文件        {}\n设备密钥    未挂载\n\n\
             用 `omy key device {} add` 挂上，之后解锁只需 Windows Hello。",
            a.file.display(),
            a.file.display(),
        )
    };
    ctx.out.result(
        &human,
        &json!({
            "path": a.file.display().to_string(),
            "available": true,
            "enrolled": enrolled,
        }),
    );
    Ok(())
}

fn device_add(
    ctx: &Ctx<'_>,
    a: &DeviceArgs,
    h: &omy_core::header::FixedHeader,
    sample: &std::path::Path,
    id: &str,
) -> Result<()> {
    let p = hello_protector()?;

    // 先验密码：挂设备密钥要求已知一个现有密码。
    //
    // 不能省。省掉的话任何人在这台机器上都能给别人的文件挂一把
    // 自己的设备密钥——那等于凭「能碰到这台机器」就获得了访问权
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let pw = read_password(&src, t("prompt.password"), false)?;
    let cur = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;

    // 真的开一次，别只派生。
    //
    // from_password 只是算出一把 KEK，不校验它对不对——密码错了要到
    // 后面 rewrite 时才暴露。而那时硬件密钥已经造好了：TPM 里留下一把
    // 永远用不到的密钥，用户还白按了一次指纹。实测确认过这一幕
    let sample_data = std::fs::read(sample)
        .with_context(|| format!("读取 {} 失败", sample.display()))?;
    omy_core::file::open(&sample_data, &[cur.duplicate()])
        .context("这个密码打不开该文件")?;
    ctx.out.detail("密码已验证");

    ctx.out.warn(
        "设备密钥不会让文件更安全，只是省去每次输密码。\n\
         它挡得住硬盘被偷和换机器解密，挡不住这台机器上正在运行的恶意程序。",
    );
    ctx.out.warn(
        "换机器、重装系统、清除 TPM 或重置 Hello 之后它会永久失效，\n\
         所以务必继续记住密码——设备密钥不是备份手段。",
    );
    if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    // 硬件里已经有就复用，没有才生成。复用时不会重复弹创建确认
    let secret = match omy_secret::Protector::retrieve(&p, id) {
        Ok(k) => {
            ctx.out.detail("复用这台机器上已保管的设备密钥");
            k
        }
        Err(omy_secret::Error::NotFound) => {
            let k = omy_secret::random_key();
            omy_secret::Protector::store(&p, id, &k)
                .map_err(|e| anyhow::anyhow!("交给 Windows Hello 保管失败：{e}"))?;
            ctx.out.detail("已生成设备密钥并交给 Windows Hello 保管");
            k
        }
        Err(e) => return Err(anyhow::anyhow!("读取设备密钥失败：{e}")),
    };
    let dev_kek = omy_core::devicekey::kek_from_secret(&secret, &h.vault_salt);

    // 挂到文件（或整棵树）上，语义与 key add 相同
    if a.file.is_dir() {
        let keep = vec![cur.duplicate(), dev_kek];
        let rep = omy_core::tree::rekey_tree(
            &a.file,
            &[cur],
            &keep,
            &h.vault_salt,
            h.cipher_id,
            omy_core::keyslot::OtherSlots::Carry,
        )?;
        ctx.out.result(
            &format!(
                "已给 {} 挂上设备密钥，处理 {} 个文件。\n\n\
                 之后解锁这个文件夹只需 Windows Hello。",
                a.file.display(),
                rep.changed,
            ),
            &json!({
                "path": a.file.display().to_string(),
                "files_changed": rep.changed,
                "enrolled": true,
            }),
        );
        return Ok(());
    }

    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;
    let opened = omy_core::file::open(&data, &[cur.duplicate()])?;
    let keep = vec![cur.duplicate(), dev_kek.duplicate()];

    let out = if opened.is_slot_managed() {
        // 可管理模式：类型如实记成 device，别记成日常密码——
        // 否则用户在清单里分不出哪个是指纹解锁
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut dir = opened.slot_directory()?;
        let free = dir
            .first_free()
            .ok_or_else(|| anyhow::anyhow!("8 个槽位已全部占用，先用 key remove 腾一个"))?;
        let mut plans: Vec<SlotPlan> =
            (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();
        *plans.get_mut(free).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
            SlotPlan::Write(dev_kek);
        dir.set(free, SlotEntry::of(SlotKind::Device))?;
        ctx.out.detail(&format!("设备密钥写入 slot {free}"));
        omy_core::keyslot::rewrite_slots_managed(&data, &[cur.duplicate()], &plans, &dir)?
    } else {
        omy_core::keyslot::rewrite_slots(
            &data,
            &[cur.duplicate()],
            &keep,
            // Carry：这个文件上可能还挂着恢复码，加设备密钥不该抹掉它
            omy_core::keyslot::OtherSlots::Carry,
        )?
    };

    verify_reopenable(&out.bytes, &keep)?;
    omy_core::fsatomic::write_atomic(&a.file, &out.bytes)
        .with_context(|| format!("写回 {} 失败", a.file.display()))?;

    ctx.out.result(
        &format!(
            "已给 {} 挂上设备密钥。\n\n\
             之后解锁只需 Windows Hello；密码仍然有效，请继续记住它。",
            a.file.display()
        ),
        &json!({
            "path": a.file.display().to_string(),
            "slots_in_use": out.slot_used,
            "enrolled": true,
        }),
    );
    Ok(())
}

fn device_remove(ctx: &Ctx<'_>, a: &DeviceArgs, id: &str) -> Result<()> {
    let p = hello_protector()?;

    // 只清硬件里那把密钥，不动文件。
    //
    // 这样做的理由：文件上那个槽位没了钥匙就是一段随机字节，与空槽
    // 不可区分，留着不影响任何事；而要精确清掉它得先解开文件，反而
    // 多一次 Hello 确认。可管理模式下用户想让清单也干净，可以再走
    // 一次 key remove
    ctx.out.warn(
        "移除后这台机器上的免密解锁失效，需要用密码打开。\n\
         文件本身不动，其它密码不受影响。",
    );
    if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    omy_secret::Protector::delete(&p, id)
        .map_err(|e| anyhow::anyhow!("移除设备密钥失败：{e}"))?;
    ctx.out.result(
        &format!(
            "已移除 {} 的设备密钥。\n\n\
             文件没有改动；那个槽位现在只是一段无人能用的随机字节。",
            a.file.display()
        ),
        &json!({
            "path": a.file.display().to_string(),
            "enrolled": false,
        }),
    );
    Ok(())
}

/// 列出一棵树的槽位。
///
/// 与单文件的差别：树的钥匙包裹在 `.omy-keys` 边车里，槽位目录也在那儿，
/// 不在文件头。但对用户来说这个区别不该存在——同一个命令对文件夹和
/// 文件应当给出同样形态的答案。
fn list_tree(ctx: &Ctx<'_>, a: &ListArgs) -> Result<()> {
    // 目录没有头部，从树里任意一个密文文件取 vault_salt 与 KDF 参数。
    // 与 recovery_tree 同一个范式
    let Some(sample) = omy_core::tree::find_any_file(&a.file) else {
        bail!(
            "{} 看起来不是树形加密的目录（里面找不到任何 .omy 文件）",
            a.file.display()
        );
    };
    let head = read_prefix(&sample, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;

    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    if src.env.is_none() && src.file.is_none() && !src.stdin {
        let human = format!(
            "文件夹      {}\nSlot 总数   {}\n\n\
             这是一个加密文件夹。要列出它的密码需要提供一个能打开它的密码——\n\
             槽位目录是加密的。加上 --password-env / --password-file 再试一次。",
            a.file.display(),
            h.slot_count,
        );
        ctx.out.result(
            &human,
            &json!({
                "path": a.file.display().to_string(),
                "kind": "tree",
                "slot_total": h.slot_count,
                "slot_used": serde_json::Value::Null,
            }),
        );
        return Ok(());
    }

    let pw = read_password(&src, t("prompt.password"), false)?;
    let kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;
    let info = omy_core::tree::tree_slots(&a.file, &[kek], &h.vault_salt, h.cipher_id)?;

    let Some(dir) = info.directory else {
        let human = format!(
            "文件夹      {}\n槽位模式    可否认（deniable）\nSlot 总数   {}\nSlot 占用   {}\n\n\
             这棵树用的是可否认模式，看不出配了几个密码——这正是它要的效果。\n\
             想要能看清槽位，需要在加密时选可管理模式。",
            a.file.display(),
            h.slot_count,
            t("info.slots_unknown"),
        );
        ctx.out.result(
            &human,
            &json!({
                "path": a.file.display().to_string(),
                "kind": "tree",
                "slot_mode": "deniable",
                "slot_total": h.slot_count,
                "slot_used": serde_json::Value::Null,
            }),
        );
        return Ok(());
    };

    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for (i, e) in dir.entries().iter().enumerate() {
        let mark = if i == info.current { "  ← 当前使用" } else { "" };
        lines.push(format!("  slot {i}  {}{mark}", kind_label(e.kind)));
        rows.push(json!({
            "index": i,
            "kind": e.kind.name(),
            "current": i == info.current,
        }));
    }

    let human = format!(
        "文件夹      {}\n槽位模式    可管理（managed）\nSlot 总数   {}\nSlot 占用   {}\n\n{}\n\n\
         整棵树共用这一份槽位表，每个密文子目录里都存了一份。",
        a.file.display(),
        h.slot_count,
        dir.used(),
        lines.join("\n"),
    );
    ctx.out.result(
        &human,
        &json!({
            "path": a.file.display().to_string(),
            "kind": "tree",
            "slot_mode": "managed",
            "slot_total": h.slot_count,
            "slot_used": dir.used(),
            "current": info.current,
            "slots": rows,
        }),
    );
    Ok(())
}

/// 列出可管理模式文件的槽位类型。
fn list_managed(ctx: &Ctx<'_>, a: &ListArgs, h: &omy_core::header::FixedHeader) -> Result<()> {
    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let pw = read_password(&src, t("prompt.password"), false)?;
    let kek = Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?;
    let opened = omy_core::file::open(&data, &[kek])?;
    let dir = opened.slot_directory()?;

    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for (i, e) in dir.entries().iter().enumerate() {
        let name = match e.kind {
            omy_core::slotdir::SlotKind::Empty => "空",
            omy_core::slotdir::SlotKind::Vault => "日常密码",
            omy_core::slotdir::SlotKind::Device => "设备密钥",
            omy_core::slotdir::SlotKind::Portable => "单文件密码",
            omy_core::slotdir::SlotKind::Recovery => "恢复码",
            // 不认识的类型如实说「未知」，不当成空——把未知当空会让
            // 新版本写的槽被旧版本覆盖掉
            omy_core::slotdir::SlotKind::Unknown(_) => "未知类型",
        };
        lines.push(format!("  slot {i}  {name}"));
        rows.push(json!({
            "index": i,
            "kind": e.kind.name(),
            "label_id": e.label_id,
        }));
    }

    let human = format!(
        "文件        {}\n槽位模式    可管理（managed）\nSlot 总数   {}\nSlot 占用   {}\n\n{}\n\n\
         可管理模式下能精确删除某个密码而保住其它的。代价是：**能打开这个\n\
         文件的人**可以看到这份清单——对打不开的人，与可否认模式一样什么\n\
         都看不出来。",
        a.file.display(),
        h.slot_count,
        dir.used(),
        lines.join("\n"),
    );

    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "slot_mode": "managed",
            "slot_total": h.slot_count,
            "slot_used": dir.used(),
            "slots": rows,
        }),
    );
    Ok(())
}

/// 可管理模式下按槽位目录精确改写。
///
/// 与可否认路径的差别只在一件事：这里**知道**每个下标是谁，于是
///
/// - `add` 找一个真正空闲的槽。可否认模式只能写在 `keep.len()` 那个下标
///   上、赌它原来是空的——实测过这会顶掉恢复码，而且无法避免。
/// - `remove` 能如实说出要清掉哪几个、其中有没有恢复码。可否认模式下
///   只能含糊地说「如果有的话」。
/// - `change` 就地替换当前密码所在的那一个槽，其余一律不动。
fn managed_rewrite(
    ctx: &Ctx<'_>,
    data: &[u8],
    opened: &omy_core::file::OpenedFile,
    unlock: &Kek,
    keep: &[Kek],
    op: Op,
) -> Result<omy_core::keyslot::RewriteOutcome> {
    use omy_core::keyslot::SlotPlan;
    use omy_core::slotdir::{SlotEntry, SlotKind};

    let mut dir = opened.slot_directory()?;
    // slot_index 是 u16（格式里就这么存的），这里要当下标用
    let cur = usize::from(opened.slot_index);

    let mut plans: Vec<SlotPlan> =
        (0..omy_core::header::SLOT_COUNT).map(|_| SlotPlan::Keep).collect();

    match op {
        Op::Add => {
            // keep = [当前密码, 新密码]
            let newcomer = keep.last().ok_or_else(|| anyhow::anyhow!("add 需要一个新密码"))?;
            let free = dir.first_free().ok_or_else(|| {
                anyhow::anyhow!(
                    "8 个槽位已全部占用。先用 key remove 腾出一个，\n\
                     或用 key list --password-env ... 看看哪些还在用。"
                )
            })?;
            *plans.get_mut(free).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
                SlotPlan::Write(newcomer.duplicate());
            dir.set(free, SlotEntry::of(SlotKind::Vault))?;
            ctx.out.detail(&format!("新密码写入 slot {free}（目录确认它是空的）"));
        }
        Op::Change => {
            // 就地替换：当前密码在哪个槽，新密码就写哪个槽。其余一律 Keep
            let newcomer = keep.first().ok_or_else(|| anyhow::anyhow!("change 需要一个新密码"))?;
            *plans.get_mut(cur).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
                SlotPlan::Write(newcomer.duplicate());
            ctx.out.detail(&format!("在 slot {cur} 上就地替换，其余槽位不动"));
        }
        Op::Remove => {
            let mut cleared = Vec::new();
            for i in 0..omy_core::header::SLOT_COUNT {
                if i == cur {
                    continue;
                }
                if dir.get(i).is_some_and(|e| e.kind.is_occupied()) {
                    cleared.push((i, dir.get(i).map_or(SlotKind::Empty, |e| e.kind)));
                }
                *plans.get_mut(i).ok_or_else(|| anyhow::anyhow!("槽位下标越界"))? =
                    SlotPlan::Clear;
                dir.set(i, SlotEntry::empty())?;
            }
            if cleared.is_empty() {
                ctx.out.detail("目录显示本来就只有当前密码，没有其它可清的");
            } else {
                for (i, kind) in &cleared {
                    ctx.out.warn(&format!("将清除 slot {i}（{}）", kind_label(*kind)));
                }
            }
        }
        // 调用点已排除：轮换换掉 FEK 之后整个 slot 区都要重建
        Op::Reencrypt => bail!("reencrypt 不走精确改写路径"),
    }

    Ok(omy_core::keyslot::rewrite_slots_managed(data, &[unlock.duplicate()], &plans, &dir)?)
}

/// 槽位类型的中文名。
const fn kind_label(k: omy_core::slotdir::SlotKind) -> &'static str {
    use omy_core::slotdir::SlotKind;
    match k {
        SlotKind::Empty => "空",
        SlotKind::Vault => "日常密码",
        SlotKind::Device => "设备密钥",
        SlotKind::Portable => "单文件密码",
        SlotKind::Recovery => "恢复码",
        SlotKind::Unknown(_) => "未知类型",
    }
}
fn modify(ctx: &Ctx<'_>, a: &SlotArgs, op: Op) -> Result<()> {
    // 目录走树形分支。不先判断的话，std::fs::read 会返回一个含糊的 IO
    // 错误（Windows 上是「拒绝访问」），用户看不出这是「该用树形方式」
    if a.file.is_dir() {
        return modify_tree(ctx, a, op);
    }
    let data = std::fs::read(&a.file)
        .with_context(|| format!("读取 {} 失败", a.file.display()))?;
    let h = omy_core::file::peek_header(&data)?;

    // 先验证现有密码——所有 slot 操作都要求已知一个有效密码
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let old = read_password(&src, t("prompt.password"), false)?;
    let old_kek = Kek::from_password(&old, &h.vault_salt, h.argon2_params())?;
    // 这一步只为尽早报「密码不对」，避免让用户白输一遍新密码。
    // 真正的解开与 MAC 校验在 rewrite_slots 里再做一次
    let opened = omy_core::file::open(&data, &[old_kek.duplicate()])?;
    ctx.out
        .detail(&format!("已用现有密码解开（slot {}）", opened.slot_index));

    // remove 不需要新密码。这三个子命令共用 SlotArgs，所以 clap 会照样
    // 接受 --new-password-*；静默忽略是不行的——用户以为自己指定了什么，
    // 实际什么也没发生，而结果（其它密码全废）是不可逆的
    if !op.accepts_new_password()
        && (a.new_password_file.is_some() || a.new_password_env.is_some())
    {
        bail!(
            "key {} 不接受 --new-password-file / --new-password-env：\
             它只保留当前密码，不设置新密码",
            op.name()
        );
    }

    // 新密码要确认两遍：打错了会得到一个自己也打不开的文件
    let new_kek = if op.wants_new_password(a) {
        let nsrc = PasswordSource {
            env: a.new_password_env.clone(),
            file: a.new_password_file.clone(),
            stdin: false,
        };
        let pw = read_password(&nsrc, t("prompt.new_password"), nsrc.is_interactive())?;
        if pw == old {
            bail!("新密码与现有密码相同，没有变化");
        }
        Some(Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?)
    } else {
        None
    };

    // 改写后要保留的 KEK 集合。顺序决定 slot 下标，但下标本身不可探测，
    // 所以顺序只影响 open 的命中位置，不影响可用性。
    //
    // 注意 change 的 keep 里**没有**解锁用的那个 KEK（旧密码要作废），
    // 所以解锁与保留必须分成两个变量传给 rewrite_slots，不能图省事拿
    // keep[0] 当解锁密钥用——change 会因此拿新密码去解原文件而必然失败
    let unlock = old_kek.duplicate();
    let keep: Vec<Kek> = match op {
        Op::Add => match new_kek {
            Some(k) => vec![old_kek, k],
            None => bail!("add 需要一个新密码"),
        },
        Op::Change => match new_kek {
            Some(k) => vec![k],
            None => bail!("change 需要一个新密码"),
        },
        Op::Remove => vec![old_kek],
        // 给了新密码就换掉，没给就沿用当前密码
        Op::Reencrypt => match new_kek {
            Some(k) => vec![k],
            None => vec![old_kek],
        },
    };

    // 只有真正会作废其它密码的操作才需要确认。
    //
    // change 现在走原样搬运（core 的 OtherSlots::Carry），不再殃及这个文件
    // 上的恢复码或别人的密码，所以它不该再摆出「其它密码会失效」的警告——
    // 那是过时且吓人的，会让用户不敢改密码
    if op != Op::Add {
        if op.rewrites_payload() {
            // 与 remove/change 的差异必须讲清，否则用户不知道为什么要
            // 多等这么久，也不知道这次做的到底是什么
            ctx.out.warn(
                "重新加密会换掉文件密钥并重写整个载荷，耗时与文件大小成正比。",
            );
            ctx.out.warn(
                "之后这个文件与旧密码彻底无关。但已经流出去的旧副本是独立的\
                 密文，本操作对它无能为力——它仍可用旧密码打开。",
            );
            ctx.out
                .warn("该文件上除保留密码之外的其它密码都会失效（如果有的话）。");
        } else if op == Op::Remove {
            ctx.out.warn(t("warn.remove_slot"));
            // 必须点名恢复码。「其它密码」这个说法太抽象——用户设恢复码时
            // 想的是「灾难兜底」，不会把它归到「其它密码」里，于是在这里
            // 一路确认下去，直到真忘密码那天才发现兜底早没了
            ctx.out.warn(
                "该文件上除当前密码之外的其它密码都会失效，\
                 **包括你可能设过的恢复码**。",
            );
        } else {
            // change：只换自己这一个
            ctx.out.warn("修改后，原密码将无法再打开这个文件。");
            ctx.out.warn(
                "该文件上的其它密码（如恢复码）不受影响，会原样保留。\
                 想清掉它们请用 key remove。",
            );
        }
        if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
            ctx.out.info(t("msg.cancelled"));
            return Ok(());
        }
    }

    // 两条路径只差中间这一步：slot 改写只动头部，轮换要重写整个载荷。
    // 前后的检查（验密码、确认、自证可打开、原子写回）完全共用——
    // 分成两个函数的话，将来加一项检查就必然漏掉一边
    let (bytes, slot_used) = if op.rewrites_payload() {
        let out = omy_core::reencrypt::rotate_fek(
            &data,
            &[unlock],
            &keep,
            &omy_core::file::RandomMaterial::generate(),
        )?;
        ctx.out
            .detail(&format!("已重写 {} 字节明文", out.plaintext_size));
        (out.bytes, out.slot_used)
    } else if opened.is_slot_managed() {
        // 可管理模式：按槽位目录精确改写，不必赌下标也不会无差别抹槽
        let out = managed_rewrite(ctx, &data, &opened, &unlock, &keep, op)?;
        (out.bytes, out.slot_used)
    } else {
        let out = omy_core::keyslot::rewrite_slots(&data, &[unlock], &keep, op.other_slots())?;
        if out.carried_opaque > 0 {
            // 不说「保留了 N 个密码」——那个数字把真实密码和随机填充算在
            // 一起（格式上本就不可区分），说出来就是在报一个我们并不知道
            // 的事实。只陈述做了什么
            ctx.out.detail("其它槽位已原样保留（若该文件设过恢复码，它仍然有效）");
        }
        if out.may_have_evicted {
            // add 看起来是纯增量，用户完全想不到它会顶掉一个自己看不见的
            // 槽。措辞只能说「可能」——那个下标上原来是真密码还是随机填充，
            // 我们确实不知道
            ctx.out.warn(
                "新密码占用的槽位上原本可能挂着别的密码（如恢复码），\
                 若有则已失效。这是格式限制：无法探测哪个槽位是空的。",
            );
            ctx.out
                .warn("如果这个文件设过恢复码，请重新生成并妥善保存。");
        }
        (out.bytes, out.slot_used)
    };

    // 写回前先自证新文件真的能用新密码打开。顺序很重要：一旦覆盖了原文件
    // 又发现打不开，用户就同时失去了旧文件和访问权
    verify_reopenable(&bytes, &keep)?;

    omy_core::fsatomic::write_atomic(&a.file, &bytes)
        .with_context(|| format!("写回 {} 失败", a.file.display()))?;

    let payload_note = if op.rewrites_payload() {
        "已重写（文件密钥已更换）"
    } else {
        "未改动（仅重写 slot 区与头部 MAC）"
    };
    let human = format!(
        "已更新 {}\n\n\
         操作        key {}\n\
         生效密码数  {}\n\
         载荷        {}",
        a.file.display(),
        op.name(),
        slot_used,
        payload_note,
    );
    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "operation": op.name(),
            "slots_in_use": slot_used,
            "payload_rewritten": op.rewrites_payload(),
        }),
    );
    Ok(())
}

/// 确认改写后的文件真能用保留下来的每个密码打开。
///
/// 逐个验，而不是只验第一个：`add` 最容易犯的错就是新密码能开、原密码
/// 被挤掉了，只验一个的话正好漏掉。
fn verify_reopenable(bytes: &[u8], keep: &[Kek]) -> Result<()> {
    for (i, k) in keep.iter().enumerate() {
        if omy_core::file::open(bytes, &[k.duplicate()]).is_err() {
            bail!(
                "改写后的文件无法用第 {} 个保留密码打开，已放弃写回（原文件未改动）",
                i.saturating_add(1)
            );
        }
    }
    Ok(())
}

fn read_prefix(p: &std::path::Path, n: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)
        .with_context(|| format!("打开 {} 失败", p.display()))?;
    let mut buf = vec![0u8; n];
    let mut got = 0usize;
    while got < n {
        let Some(s) = buf.get_mut(got..) else { break };
        match f.read(s) {
            Ok(0) => break,
            Ok(k) => got = got.saturating_add(k),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// 给一棵树形加密的目录换密码。
///
/// # 与单文件的差别
///
/// 目录没有头部，所以 `vault_salt` 与 KDF 参数要从树里任意一个 `.omy`
/// 文件取——同一个 vault 内它们本就一致（与 `decrypt` 的树形分支同一套
/// 做法）。取到之后 Argon2 只派生一次，整棵树复用。
///
/// `reencrypt` 会把每个文件的载荷读一遍、用新 FEK 写一遍，耗时与总数据量
/// 成正比，所以带进度输出。中途失败不会毁数据：每个文件各自原子写回，
/// 任一时刻每个文件要么是完整的旧密文、要么是完整的新密文。
fn modify_tree(ctx: &Ctx<'_>, a: &SlotArgs, op: Op) -> Result<()> {
    // add / remove 现在对目录也成立。
    //
    // 以前禁掉它们，是因为目录名由 keks[0] 派生，多出来的密码能打开
    // 每个文件却解不开目录名——解密时报「文件损坏」。改用两层结构
    // （随机目录密钥 + 每把钥匙包一份放边车）之后，任一把钥匙都能
    // 解开目录名，这个限制不存在了。
    //
    // remove 仍然危险，但危险之处与单文件相同（清掉其它全部密码），
    // 不需要在这里特殊拦截

    // 目录没有头部，从树里任意一个密文文件取 vault_salt 与 KDF 参数
    let Some(sample) = omy_core::tree::find_any_file(&a.file) else {
        bail!(
            "{} 看起来不是树形加密的目录（里面找不到任何 .omy 文件）",
            a.file.display()
        );
    };
    let head = read_prefix(&sample, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;

    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let old = read_password(&src, t("prompt.password"), false)?;
    let old_kek = Kek::from_password(&old, &h.vault_salt, h.argon2_params())?;

    // 尽早验密码：让用户白输一遍新密码再报「旧密码不对」是很糟的体验。
    // 拿样本文件试，成本是一次小文件读取
    let sample_data = std::fs::read(&sample)
        .with_context(|| format!("读取 {} 失败", sample.display()))?;
    omy_core::file::open(&sample_data, &[old_kek.duplicate()])
        .context("现有密码不正确（用树里的一个文件验证过）")?;

    if !op.accepts_new_password()
        && (a.new_password_file.is_some() || a.new_password_env.is_some())
    {
        bail!(
            "key {} 不接受 --new-password-file / --new-password-env：\
             它只保留当前密码，不设置新密码",
            op.name()
        );
    }

    let new_kek = if op.wants_new_password(a) {
        let nsrc = PasswordSource {
            env: a.new_password_env.clone(),
            file: a.new_password_file.clone(),
            stdin: false,
        };
        let pw = read_password(&nsrc, t("prompt.new_password"), nsrc.is_interactive())?;
        if pw == old {
            bail!("新密码与现有密码相同，没有变化");
        }
        Some(Kek::from_password(&pw, &h.vault_salt, h.argon2_params())?)
    } else {
        None
    };

    // 在 new_kek 被移进 keep 之前先记下来：轮换可以不换密码，
    // 提示的措辞和要不要预告目录改名都取决于这一点
    let changing_password = new_kek.is_some();
    let keep: Vec<Kek> = match op {
        Op::Change => match new_kek {
            Some(k) => vec![k],
            None => bail!("change 需要一个新密码"),
        },
        // 轮换可以不换密码：「让旧副本作废」是它的正当用法之一
        Op::Reencrypt => match new_kek {
            Some(k) => vec![k],
            None => vec![old_kek.duplicate()],
        },
        // add：原密码 + 新密码都留着。
        //
        // 目录名改用两层结构之后这才有意义——以前多出来的密码能打开
        // 每个文件却解不开目录名，解密时报「文件损坏」
        Op::Add => match new_kek {
            Some(k) => vec![old_kek.duplicate(), k],
            None => bail!("add 需要一个新密码"),
        },
        // remove：只留当前密码，其它全部作废（含恢复码）
        Op::Remove => vec![old_kek.duplicate()],
    };

    let rotate = op.rewrites_payload();
        if rotate {
        ctx.out
            .warn("重新加密会换掉每个文件的密钥并重写全部载荷，耗时与总数据量成正比。");
        ctx.out.warn(
            "之后这棵树与旧密钥彻底无关。但已经流出去的旧副本是独立的密文，\
             本操作对它无能为力——它仍可用旧密码打开。",
        );
    }
    if changing_password {
        ctx.out.warn("修改后，原密码将无法再打开这棵树里的任何文件。");
        // 目录名会变这件事必须提前说：用户回到文件管理器发现文件夹
        // 「不见了」是很吓人的，而它只是改了名
        ctx.out
            .warn("目录名由密码派生，换密码后整棵树的目录名都会变（内容不变）。");
    } else {
        // 不换密码时目录名不变，得说清楚，否则用户会怀疑操作没生效
        ctx.out.warn("密码不变，所以目录名保持原样，只有文件内容被重新加密。");
    }
    if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
        ctx.out.info(t("msg.cancelled"));
        return Ok(());
    }

    // 只有轮换才显示进度：改密码是秒级的，刷一堆进度行反而是噪音
    let mut last = 0usize;
    let mut tick = |name: &str, idx: usize, total: usize, _done: u64, _bytes: u64| {
        // 按文件节流。不节流的话每个文件会刷上百行（字节进度每变一次
        // 就来一次），把终端刷满，真正有用的信息反而被顶走
        if idx == last {
            return;
        }
        last = idx;
        ctx.out.detail(&format!("[{idx}/{total}] {name}"));
    };
    // 与单文件同一个判据：只有 remove 清场，其余保住认不出来的槽位。
    // 两处若分开写，迟早出现「单文件保住了恢复码、目录里没保住」
    let params = omy_core::tree::RekeyParams {
        vault_salt: &h.vault_salt,
        cipher: h.cipher_id,
        rotate,
        others: op.other_slots(),
    };
    let rep = omy_core::tree::rekey_tree_with_progress(
        &a.file,
        &[old_kek],
        &keep,
        params,
        if rotate { Some(&mut tick) } else { None },
    )?;

    // 部分失败要显式报出来，并且退出码不能是 0——用户以为全改完了，
    // 等哪天用新密码打不开另一半时早忘了旧密码
    if !rep.is_complete() {
        for (p, err) in &rep.failed {
            ctx.out.warn(&format!("  {} : {}", p.display(), err));
        }
        // 建议「逐个补」而不是「重跑整棵树」：重跑会拿旧密码去开已经改成
        // 新密码的文件，报一堆假失败，用户分不清哪些是真问题；轮换时还要把
        // 已处理过的文件再读写一遍。上面打印的路径在改名之后仍然有效，
        // 可以直接复制粘贴
        bail!(
            "{} 个文件已改，{} 个失败。失败的那些文件没有被改动，旧密码依然有效。\n\
             对上面列出的每个路径单独执行一次本命令即可补齐（路径已是改名后的新位置）。",
            rep.changed,
            rep.failed.len()
        );
    }

    let mut human = format!(
        "{} 完成\n目录        {}\n改写文件    {}\n重命名目录  {}\n新路径      {}",
        op.name(),
        a.file.display(),
        rep.changed,
        rep.dirs_renamed,
        rep.root.display()
    );
    if rep.payload_rewritten {
        human.push_str(&format!("\n重写明文    {} 字节", rep.bytes_rewritten));
    }
    ctx.out.result(
        &human,
        &json!({
            "path": a.file.display().to_string(),
            "mode": "tree",
            "action": op.name(),
            "files_changed": rep.changed,
            "dirs_renamed": rep.dirs_renamed,
            "new_root": rep.root.display().to_string(),
            "payload_rewritten": rep.payload_rewritten,
            "bytes_rewritten": rep.bytes_rewritten,
        }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_add_and_change_need_new_password() {
        assert!(Op::Add.needs_new_password());
        assert!(Op::Change.needs_new_password());
        assert!(!Op::Remove.needs_new_password(), "remove 不该问新密码");
    }

    #[test]
    fn op_names_match_subcommands() {
        assert_eq!(Op::Add.name(), "add");
        assert_eq!(Op::Remove.name(), "remove");
        assert_eq!(Op::Change.name(), "change");
        assert_eq!(Op::Reencrypt.name(), "reencrypt");
    }

    #[test]
    fn only_reencrypt_rewrites_payload() {
        // 不这样会怎样：这个判定同时决定「要不要警告耗时」和输出里的
        // payload_rewritten。若 remove 也被算成重写载荷，JSON 消费方会以为
        // 每次改密码都动了载荷，增量备份策略就会做错决定
        assert!(Op::Reencrypt.rewrites_payload());
        for op in [Op::Add, Op::Remove, Op::Change] {
            assert!(!op.rewrites_payload(), "{} 不该重写载荷", op.name());
        }
    }

    #[test]
    fn reencrypt_accepts_but_does_not_require_new_password() {
        // 轮换的核心是换文件密钥，改密码是可选的。强制要求会让
        // 「只想让旧副本的密码失效、密码不变」无法表达
        assert!(Op::Reencrypt.accepts_new_password());
        assert!(!Op::Reencrypt.needs_new_password());
        // remove 则连接受都不该接受
        assert!(!Op::Remove.accepts_new_password());
    }
}
