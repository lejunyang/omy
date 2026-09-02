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
//! 猜错会静默覆盖掉另一个密码。所以这三个命令的真实语义都是
//! **重新声明这个文件的密码集合**：
//!
//! - `add`：解锁用的密码 + 新密码，都保留
//! - `change`：只保留新密码，解锁用的那个作废
//! - `remove`：只保留解锁用的密码，其余全部作废
//!
//! `remove` 这个语义必须在提示里讲清楚：它不是「删掉某一个密码」，而是
//! 「只留下我现在用的这个」。用户以为删掉的是别人那个，实际效果一样，
//! 但如果文件上原本挂着 3 个密码，另外 2 个会一起失效。

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

/// `key list` 的参数。
#[derive(Debug, ClapArgs)]
pub struct ListArgs {
    /// 目标文件
    pub file: PathBuf,
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
    }
}

/// slot 操作类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Add,
    Remove,
    Change,
}

impl Op {
    /// 是否需要输入一个新密码。
    const fn needs_new_password(self) -> bool {
        matches!(self, Self::Add | Self::Change)
    }

    /// 命令名，用于输出。
    const fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Change => "change",
        }
    }
}

fn list(ctx: &Ctx<'_>, a: &ListArgs) -> Result<()> {
    let head = read_prefix(&a.file, omy_core::scan::MIN_PROBE_SIZE)?;
    let h = omy_core::file::peek_header(&head)?;

    let human = format!(
        "文件        {}\nSlot 总数   {}\nSlot 占用   {}\n\n\
         slot 区始终填满，真实 slot 与随机填充在字节层面无法区分。\n\
         这是可否认性设计的基础：无法判断该文件配置了几个密码。",
        a.file.display(),
        h.slot_count,
        t("info.slots_unknown")
    );

    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "slot_total": h.slot_count,
            // 恒为 null：设计上不可探测
            "slot_used": serde_json::Value::Null,
        }),
    );
    Ok(())
}

fn modify(ctx: &Ctx<'_>, a: &SlotArgs, op: Op) -> Result<()> {
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

    // 新密码要确认两遍：打错了会得到一个自己也打不开的文件
    let new_kek = if op.needs_new_password() {
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
    };

    // 所有会作废其它密码的操作都要确认。这里的措辞必须点明「其它密码」，
    // 因为用户很容易以为 add 是纯增量、change 只影响自己那一个
    if op != Op::Add {
        ctx.out.warn(t("warn.remove_slot"));
        ctx.out.warn(match op {
            Op::Change => "修改后，原密码将无法再打开这个文件。",
            _ => "该文件上除当前密码之外的其它密码都会失效（如果有的话）。",
        });
        if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
            ctx.out.info(t("msg.cancelled"));
            return Ok(());
        }
    }

    let outcome = omy_core::keyslot::rewrite_slots(&data, &[unlock], &keep)?;

    // 写回前先自证新文件真的能用新密码打开。顺序很重要：一旦覆盖了原文件
    // 又发现打不开，用户就同时失去了旧文件和访问权
    verify_reopenable(&outcome.bytes, &keep)?;

    omy_core::fsatomic::write_atomic(&a.file, &outcome.bytes)
        .with_context(|| format!("写回 {} 失败", a.file.display()))?;

    let human = format!(
        "已更新 {}\n\n\
         操作        key {}\n\
         生效密码数  {}\n\
         载荷        未改动（仅重写 slot 区与头部 MAC）",
        a.file.display(),
        op.name(),
        outcome.slot_used,
    );
    ctx.out.result(
        &human,
        &json!({
            "file": a.file.display().to_string(),
            "operation": op.name(),
            "slots_in_use": outcome.slot_used,
            "payload_rewritten": false,
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
    }
}
