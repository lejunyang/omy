//! `omy key`：密钥 slot 管理。
//!
//! # `key list` 为什么什么都不显示
//!
//! slot 区始终填满 8 个（真实 + 随机填充），设计上无法探测实际用了几个。
//! 因此 `key list` 只能告知「共 8 个 slot，内容不可探测」——
//! 这不是功能缺失，而是可否认性（决策 D-02）的直接结果。

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
    /// 移除密码 slot
    Remove(SlotArgs),
    /// 显示 slot 占用情况
    List(ListArgs),
    /// 修改密码
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
/// 文件读取失败、密码不匹配，或操作尚未实现时返回错误。
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
    let kek = Kek::from_password(&old, &h.vault_salt, h.argon2_params())?;
    let opened = omy_core::file::open(&data, &[kek])?;
    ctx.out.detail(&format!("已用现有密码解开（slot {}）", opened.slot_index));

    if op == Op::Remove {
        // 这个警告非常重要：用户常误以为移除 slot 能让已泄露的副本失效
        ctx.out.warn(t("warn.remove_slot"));
        if !ctx.out.confirm(t("prompt.confirm"), ctx.assume_yes) {
            ctx.out.info(t("msg.cancelled"));
            return Ok(());
        }
    }

    // slot 区的原地改写需要 core 提供「保留 FEK、重建 slot 区、重算 header MAC」
    // 的能力。当前 core 只暴露了整体加密路径，尚未提供该操作。
    //
    // 不用「解密后重新加密」来假装实现：那会改变 file_uuid、base_nonce 与全部
    // 密文字节，等于生成一个新文件。对大文件是数小时的重写，且破坏了
    // 「slot 操作只改头部」的预期，也让已有备份的增量同步全部失效。
    bail!(
        "{} 尚未实现。\n\n\
         该操作需要在**不重写载荷**的前提下改写 slot 区并重算头部 MAC，\n\
         core 目前只暴露整体加密路径。用解密再加密来代替会改变 file_uuid\n\
         与全部密文字节（大文件相当于完全重写），因此没有这样做。\n\n\
         当前可行的替代：用新密码重新加密一份。",
        match op {
            Op::Add => "key add",
            Op::Remove => "key remove",
            Op::Change => "key change",
        }
    );
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
