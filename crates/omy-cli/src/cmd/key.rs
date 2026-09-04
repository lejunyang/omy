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
    /// 重新加密：换掉文件密钥并重写载荷（可同时改密码）
    Reencrypt(SlotArgs),
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
        Cmd::Reencrypt(a) => modify(ctx, a, Op::Reencrypt),
    }
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

    // 所有会作废其它密码的操作都要确认。这里的措辞必须点明「其它密码」，
    // 因为用户很容易以为 add 是纯增量、change 只影响自己那一个
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
        } else {
            ctx.out.warn(t("warn.remove_slot"));
            ctx.out.warn(match op {
                Op::Change => "修改后，原密码将无法再打开这个文件。",
                _ => "该文件上除当前密码之外的其它密码都会失效（如果有的话）。",
            });
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
    } else {
        let out = omy_core::keyslot::rewrite_slots(&data, &[unlock], &keep)?;
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
    // 树只能有一个密码，所以 add / remove 在这里没有意义。
    //
    // 目录名由**第一个** KEK 派生（encrypt_tree / decrypt_tree 都只认
    // keks[0]），多出来的密码只能打开文件、解不开目录名。实测的表现是
    // decrypt 报 content hash mismatch——用户会以为文件损坏了。与其给出
    // 一个半残的密码，不如直说不支持
    if matches!(op, Op::Add | Op::Remove) {
        bail!(
            "key {} 不支持目录：树形加密的目录名由密码派生，\n\
             一棵树同时只能有一个密码。多加的密码能打开文件却解不开目录名，\n\
             解密时会报「文件损坏」。\n\
             要换密码请用 key change。",
            op.name()
        );
    }

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
        // 上面已经逐一拒绝。不用 unreachable!：omy-cli 虽然没有 omy-gui
        // 那么严的禁用规则，但一个能被将来的改动触发的 panic 不值得留
        Op::Add | Op::Remove => {
            bail!("key {} 不支持目录", op.name())
        }
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
    let rep = omy_core::tree::rekey_tree_with_progress(
        &a.file,
        &[old_kek],
        &keep,
        &h.vault_salt,
        h.cipher_id,
        rotate,
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
