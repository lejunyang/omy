//! 虚拟远程位置（收藏夹）命令。
//!
//! # 它是什么
//!
//! 虚拟位置不连服务器，只在本地存「对真实远程文件的引用」（收藏夹）。业务本体
//! 在 [`omy_remote::virtuals`]，GUI 与这里共用同一份模型/落盘/加解密；本文件只是
//! 把它包成脚本友好的命令行。
//!
//! # 与真实位置命令（remote list/ls/...）的关系
//!
//! 真实位置（WebDAV/Telegram）直接连服务器；虚拟位置只是本地收藏索引，**本文件
//! 的全部命令都不发网络请求**。命令放在 `remote virtual` 下而不是平铺在 `remote`，
//! 就是为了不让「给收藏夹加一条引用」和「给服务器加一个目录」混淆。
//!
//! # 无状态进程模型
//!
//! CLI 是一次性进程：每次命令都重新 `load()`。加密位置在新进程里默认是锁定态，
//! 所以任何要读/改树的命令都接受同一组 `--password-*`：没密码就报
//! `VIRTUAL_LOCKED`，有就现场解锁、操作完进程退出（内存随之销毁）。`unlock` 命令
//! 因此只做「校验密码对不对」，`lock` 在一次性进程里是空操作（进程本来就退出了），
//! 保留它们是为了与 GUI 命令树对齐与脚本对称。
//!
//! # 退出码
//!
//! 成功 0；用法错 2（clap）；密码错 3；其余虚拟操作失败 1。机器可读错误码在
//! `--json` 的 `error.code`，脚本应匹配它而不是中文文案。

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use omy_config::SavedPlace;
use omy_remote::virtuals::{
    self, Reference, Snapshot, SourceRef, VirtualCryptoError, VirtualRegistry,
};
use serde_json::{Value, json};

use crate::password::{PasswordSource, read_password};
use super::Ctx;

/// 虚拟位置子命令树。Rust 保留字不能当模块名，故模块叫 `virt`，
/// 但命令行上是 `remote virtual`（见本枚举上的 command(name)）。
#[derive(Debug, Subcommand)]
#[command(name = "virtual")]
pub enum Cmd {
    /// 位置管理：create/list/show/rename/remove
    #[command(subcommand)]
    Place(PlaceCmd),
    /// 虚拟文件夹：add/rename/remove
    #[command(subcommand)]
    Folder(FolderCmd),
    /// 引用（收藏条目）：add/remove/move/copy/list/browse
    #[command(subcommand)]
    Ref(RefCmd),
    /// 用独立密码加密一个虚拟位置
    Encrypt(PlacePwArgs),
    /// 校验位置密码是否正确（一次性进程，不持久解锁）
    Unlock(PlacePwArgs),
    /// 锁定一个虚拟位置（一次性进程内为空操作，与 GUI 对称）
    Lock(PlaceIdArgs),
    /// 取消加密：用密码解锁后写回明文收藏
    Decrypt(PlacePwArgs),
}

/// 带密码通道的位置命令（加密/解锁/取消加密）。
#[derive(Debug, Args)]
pub struct PlacePwArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

/// 只指一个位置 id 的命令（锁定）。
#[derive(Debug, Args)]
pub struct PlaceIdArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
}

#[derive(Debug, Subcommand)]
pub enum PlaceCmd {
    /// 新建一个空虚拟位置
    Create(CreateArgs),
    /// 列出所有虚拟位置
    List,
    /// 显示一个位置的详情
    Show(PlaceIdArgs),
    /// 重命名一个位置
    Rename(RenameArgs),
    /// 删除一个位置及其收藏（不碰真实远程文件）
    Remove(PlaceIdArgs),
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// 位置显示名
    pub name: String,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    /// 新名字
    pub name: String,
}

#[derive(Debug, Subcommand)]
pub enum FolderCmd {
    /// 在某位置下新建子文件夹
    Add(FolderAddArgs),
    /// 重命名一个文件夹
    Rename(FolderRenameArgs),
    /// 删除一个文件夹（连同整棵子树的引用）
    Remove(FolderRemoveArgs),
}

/// 文件夹命令通用：位置 + （改名/删除时的）文件夹。
#[derive(Debug, Args)]
pub struct FolderTarget {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    /// 文件夹 id；根目录为空串（命令行用 `--root` 表示）
    #[arg(long)]
    pub folder: Option<String>,
    /// 指根目录
    #[arg(long)]
    pub root: bool,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct FolderAddArgs {
    #[command(flatten)]
    pub target: FolderTarget,
    /// 新文件夹名
    pub name: String,
}

#[derive(Debug, Args)]
pub struct FolderRenameArgs {
    #[command(flatten)]
    pub target: FolderTarget,
    /// 文件夹 id
    pub folder_id: String,
    /// 新名字
    pub name: String,
}

#[derive(Debug, Args)]
pub struct FolderRemoveArgs {
    #[command(flatten)]
    pub target: FolderTarget,
    /// 要删除的文件夹 id
    pub folder_id: String,
}

#[derive(Debug, Subcommand)]
pub enum RefCmd {
    /// 添加一条引用（收藏一个真实远程文件）
    Add(RefAddArgs),
    /// 删除一条引用（不碰真实文件）
    Remove(RefPlaceRefArgs),
    /// 把一条引用在同位置内移动到另一个文件夹
    Move(RefMoveArgs),
    /// 把引用复制到（可跨位置）另一个文件夹，每条生成新引用 id
    Copy(RefCopyArgs),
    /// 递归列出一个位置（或其文件夹）下的全部引用
    List(RefBrowseArgs),
    /// 浏览一个文件夹：列出它的直属子文件夹与引用（单层）
    Browse(RefBrowseArgs),
}

#[derive(Debug, Args)]
pub struct RefBrowseArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    /// 文件夹 id；省略表示根目录
    #[arg(long)]
    pub folder: Option<String>,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct RefAddArgs {
    /// 目标位置 id（如 v1）或显示名
    pub place: String,
    /// 目标文件夹 id；省略表示根目录
    #[arg(long)]
    pub folder: Option<String>,
    /// 源真实位置（如 p1）：从已保存配置导出稳定标识
    #[arg(long, value_name = "PLACE")]
    pub source_place: String,
    /// 源目录/对话 id（如 tg:-100）
    #[arg(long)]
    pub dir_id: String,
    /// 源文件/消息 id（如 tg:-100:5 或 WebDAV 绝对路径）
    #[arg(long)]
    pub file_id: String,
    /// 显示名（源不可达时也能列出）
    #[arg(long)]
    pub name: String,
    /// 字节数（可选）
    #[arg(long)]
    pub size: Option<u64>,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct RefPlaceRefArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    /// 引用 id
    pub r#ref: String,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct RefMoveArgs {
    /// 位置 id（如 v1）或显示名
    pub place: String,
    /// 要移动的引用 id
    pub r#ref: String,
    /// 目标文件夹 id；省略表示根目录
    #[arg(long)]
    pub to_folder: Option<String>,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

#[derive(Debug, Args)]
pub struct RefCopyArgs {
    /// 源位置 id（如 v1）或显示名
    pub from_place: String,
    /// 要复制的引用 id（可重复）
    #[arg(long, value_name = "REF", required = true)]
    pub ref_id: Vec<String>,
    /// 目标位置 id（如 v2）或显示名
    #[arg(long)]
    pub to_place: String,
    /// 目标文件夹 id；省略表示根目录
    #[arg(long)]
    pub to_folder: Option<String>,
    #[command(flatten)]
    pub pw: PasswordArgs,
}

/// 密码输入通道（与 add-webdav 同一组，禁止 --password 明文 argv）。
#[derive(Debug, Args, Default, Clone)]
pub struct PasswordArgs {
    /// 从环境变量读取密码（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 从文件读取密码首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,
}

impl From<&PasswordArgs> for PasswordSource {
    fn from(a: &PasswordArgs) -> Self {
        Self { env: a.password_env.clone(), file: a.password_file.clone(), stdin: a.password_stdin }
    }
}

/// 虚拟命令的结构化错误：机器可读 code + 退出码，`--json` 下原样输出。
#[derive(Debug)]
pub struct VErr {
    pub(crate) code: &'static str,
    pub(crate) exit: i32,
    pub(crate) message: String,
}

impl VErr {
    fn new(code: &'static str, exit: i32, message: impl Into<String>) -> Self {
        Self { code, exit, message: message.into() }
    }
    /// 位置不存在。
    fn no_place(id: &str) -> Self {
        Self::new("VIRTUAL_NO_SUCH_PLACE", 1, format!("虚拟位置不存在: {id:?}"))
    }
    /// 文件夹/引用目标不存在。
    fn no_target(what: &str) -> Self {
        Self::new("VIRTUAL_NO_TARGET", 1, format!("目标不存在（文件夹/引用）: {what}"))
    }
    /// 位置已加密但没给密码。
    fn locked() -> Self {
        Self::new("VIRTUAL_LOCKED", 1, "位置已加密且未提供密码（用 --password-stdin/--password-file/--password-env）")
    }
}

impl std::fmt::Display for VErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for VErr {}

/// 把业务层加密错误映射成 CLI 错误。
fn crypto(e: VirtualCryptoError) -> VErr {
    match e {
        VirtualCryptoError::WrongPassword => VErr::new("VIRTUAL_WRONG_PASSWORD", 3, "密码不正确"),
        VirtualCryptoError::NoSuchPlace => VErr::new("VIRTUAL_NO_SUCH_PLACE", 1, "虚拟位置不存在"),
        VirtualCryptoError::Already => VErr::new("VIRTUAL_ALREADY_ENCRYPTED", 1, "位置已经是加密态"),
        VirtualCryptoError::NotEncrypted => VErr::new("VIRTUAL_NOT_ENCRYPTED", 1, "位置未加密"),
        VirtualCryptoError::Other(d) => VErr::new("VIRTUAL_ENCRYPT_FAILED", 1, d),
    }
}

/// 分发。
pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    // 统一把 VErr 转成 anyhow，交给 report_error 下钻（输出 code/exit）。
    match run_inner(ctx, cmd) {
        Ok(()) => Ok(()),
        Err(v) => Err(anyhow!(v)),
    }
}

fn run_inner(ctx: &Ctx, cmd: &Cmd) -> std::result::Result<(), VErr> {
    let reg = VirtualRegistry::new();
    reg.load().map_err(|e| VErr::new("VIRTUAL_LOAD_FAILED", 1, e.to_string()))?;
    match cmd {
        Cmd::Place(c) => place(ctx, &reg, c),
        Cmd::Folder(c) => folder(ctx, &reg, c),
        Cmd::Ref(c) => reference(ctx, &reg, c),
        Cmd::Encrypt(a) => {
            let id = resolve(&reg, &a.place)?;
            let pw = read_password(&PasswordSource::from(&a.pw), "虚拟位置加密密码", true)
                .map_err(|e| VErr::new("VIRTUAL_PASSWORD_INPUT", 1, e.to_string()))?;
            reg.encrypt(&id, &pw, Vec::new(), |_, _, _| {})
                .map_err(crypto)?;
            out(ctx, "加密成功", &json!({ "id": id, "encrypted": true, "unlocked": true }));
            Ok(())
        }
        Cmd::Unlock(a) => {
            let id = resolve(&reg, &a.place)?;
            let pw = read_password(&PasswordSource::from(&a.pw), "虚拟位置密码", false)
                .map_err(|e| VErr::new("VIRTUAL_PASSWORD_INPUT", 1, e.to_string()))?;
            reg.unlock_with_extra(&id, &pw, &[], |_, _, _| {}).map_err(crypto)?;
            out(ctx, "密码正确，位置已解锁（本进程内生效）", &json!({ "id": id, "unlocked": true }));
            Ok(())
        }
        Cmd::Lock(a) => {
            let id = resolve(&reg, &a.place)?;
            reg.lock(&id);
            out(ctx, "已锁定（一次性进程退出后内存即清空）", &json!({ "id": id, "locked": true }));
            Ok(())
        }
        Cmd::Decrypt(a) => {
            let id = resolve(&reg, &a.place)?;
            let pw = read_password(&PasswordSource::from(&a.pw), "虚拟位置密码", false)
                .map_err(|e| VErr::new("VIRTUAL_PASSWORD_INPUT", 1, e.to_string()))?;
            reg.decrypt(&id, &pw).map_err(crypto)?;
            out(ctx, "已取消加密，收藏恢复为明文", &json!({ "id": id, "encrypted": false }));
            Ok(())
        }
    }
}

// ---- 位置 ----

fn place(ctx: &Ctx, reg: &VirtualRegistry, cmd: &PlaceCmd) -> std::result::Result<(), VErr> {
    match cmd {
        PlaceCmd::Create(a) => {
            let name = a.name.trim();
            if name.is_empty() {
                return Err(VErr::new("VIRTUAL_BAD_NAME", 1, "位置名不能为空"));
            }
            let id = reg.create(name.to_owned());
            out(ctx, &format!("已创建虚拟位置 {id}（{name}）"), &json!({ "id": id, "name": name }));
            Ok(())
        }
        PlaceCmd::List => {
            let rows: Vec<Value> = reg
                .list_detailed()
                .into_iter()
                .map(|(id, name, encrypted)| {
                    json!({ "id": id, "name": name, "encrypted": encrypted })
                })
                .collect();
            let human = if rows.is_empty() {
                String::from("（还没有虚拟位置，用 `omy remote virtual place create` 添加）")
            } else {
                rows.iter()
                    .map(|r| {
                        let lock = if r["encrypted"].as_bool() == Some(true) { "🔒" } else { " " };
                        format!("{lock} {}  {}", r["id"].as_str().unwrap_or(""), r["name"].as_str().unwrap_or(""))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            out(ctx, &human, &json!({ "places": rows }));
            Ok(())
        }
        PlaceCmd::Show(a) => {
            let id = resolve(reg, &a.place)?;
            reg.with_place(&id, |vp| {
                let refs = count_refs(&vp.root);
                out(
                    ctx,
                    &format!("{}  {}\n加密: {}\n引用总数: {}", vp.id, vp.name, if vp.encrypted { "是" } else { "否" }, refs),
                    &json!({
                        "id": vp.id,
                        "name": vp.name,
                        "encrypted": vp.encrypted,
                        "refs": refs,
                    }),
                );
            })
            .ok_or_else(|| VErr::no_place(&id))?;
            Ok(())
        }
        PlaceCmd::Rename(a) => {
            let id = resolve(reg, &a.place)?;
            let name = a.name.trim();
            if name.is_empty() {
                return Err(VErr::new("VIRTUAL_BAD_NAME", 1, "新名字不能为空"));
            }
            if !reg.rename(&id, name.to_owned()) {
                return Err(VErr::no_place(&id));
            }
            out(ctx, &format!("已把 {id} 改名为 {name}"), &json!({ "id": id, "name": name }));
            Ok(())
        }
        PlaceCmd::Remove(a) => {
            let id = resolve(reg, &a.place)?;
            if !ctx.out.confirm(format!("将删除虚拟位置 {id} 及其全部收藏。不碰真实远程文件。继续？[y/N] ").as_str(), ctx.assume_yes) {
                return Err(VErr::new("VIRTUAL_CANCELLED", 8, "已取消"));
            }
            if !reg.remove(&id) {
                return Err(VErr::no_place(&id));
            }
            out(ctx, &format!("已删除 {id}"), &json!({ "removed": id }));
            Ok(())
        }
    }
}

// ---- 文件夹 ----

fn folder(ctx: &Ctx, reg: &VirtualRegistry, cmd: &FolderCmd) -> std::result::Result<(), VErr> {
    match cmd {
        FolderCmd::Add(a) => {
            let id = resolve(reg, &a.target.place)?;
            unlock_if_needed(reg, &id, &a.target.pw)?;
            let parent = folder_id(&a.target.folder, a.target.root);
            if a.name.trim().is_empty() {
                return Err(VErr::new("VIRTUAL_BAD_NAME", 1, "文件夹名不能为空"));
            }
            let new_id = format!("vf{}", virtuals::random_id());
            let done = reg
                .with_place_mut(&id, |vp| {
                    vp.root.find_mut(&parent).map(|p| {
                        p.folders.push(virtuals::VFolder {
                            id: new_id.clone(),
                            name: a.name.trim().to_owned(),
                            folders: Vec::new(),
                            refs: Vec::new(),
                        });
                    })
                })
                .flatten();
            if done.is_some() {
                out(ctx, &format!("已建文件夹 {new_id}"), &json!({ "folder": new_id, "place": id }));
                Ok(())
            } else {
                Err(VErr::no_target(&format!("父文件夹 {parent:?}")))
            }
        }
        FolderCmd::Rename(a) => folder_rename(ctx, reg, a),
        FolderCmd::Remove(a) => folder_remove(ctx, reg, a),
    }
}

/// 文件夹 id：`--root` 为空串，否则用 `--folder`；都不给也视为根。
fn folder_id(folder: &Option<String>, root: bool) -> String {
    if root {
        String::new()
    } else {
        folder.clone().unwrap_or_default()
    }
}

fn folder_rename(ctx: &Ctx, reg: &VirtualRegistry, a: &FolderRenameArgs) -> std::result::Result<(), VErr> {
    let id = resolve(reg, &a.target.place)?;
    unlock_if_needed(reg, &id, &a.target.pw)?;
    if a.folder_id.is_empty() {
        return Err(VErr::no_target("根文件夹不能改名"));
    }
    let name = a.name.trim();
    if name.is_empty() {
        return Err(VErr::new("VIRTUAL_BAD_NAME", 1, "文件夹名不能为空"));
    }
    let ok = reg
        .with_place_mut(&id, |vp| {
            vp.root.find_mut(&a.folder_id).map(|f| f.name = name.to_owned()).is_some()
        })
        .unwrap_or(false);
    if ok {
        out(ctx, &format!("已把文件夹 {} 改名为 {name}", a.folder_id), &json!({ "folder": a.folder_id, "name": name }));
        Ok(())
    } else {
        Err(VErr::no_target(&a.folder_id))
    }
}

fn folder_remove(ctx: &Ctx, reg: &VirtualRegistry, a: &FolderRemoveArgs) -> std::result::Result<(), VErr> {
    let id = resolve(reg, &a.target.place)?;
    unlock_if_needed(reg, &id, &a.target.pw)?;
    if a.folder_id.is_empty() {
        return Err(VErr::no_target("不能删根文件夹"));
    }
    let n = reg
        .with_place_mut(&id, |vp| {
            let n = vp.root.subtree_ref_count(&a.folder_id).unwrap_or(0);
            vp.root.take_subtree(&a.folder_id).map(|_| n)
        })
        .flatten();
    match n {
        Some(n) => {
            out(ctx, &format!("已删文件夹 {}（连同 {n} 条引用）", a.folder_id), &json!({ "folder": a.folder_id, "removed_refs": n }));
            Ok(())
        }
        None => Err(VErr::no_target(&a.folder_id)),
    }
}

// ---- 引用 ----

fn reference(ctx: &Ctx, reg: &VirtualRegistry, cmd: &RefCmd) -> std::result::Result<(), VErr> {
    match cmd {
        RefCmd::Add(a) => ref_add(ctx, reg, a),
        RefCmd::Remove(a) => {
            let id = resolve(reg, &a.place)?;
            unlock_if_needed(reg, &id, &a.pw)?;
            let ok = reg.with_place_mut(&id, |vp| vp.root.remove_ref(&a.r#ref)).unwrap_or(false);
            if ok {
                out(ctx, &format!("已删除引用 {}", a.r#ref), &json!({ "removed": a.r#ref }));
                Ok(())
            } else {
                Err(VErr::no_target(&a.r#ref))
            }
        }
        RefCmd::Move(a) => {
            let id = resolve(reg, &a.place)?;
            unlock_if_needed(reg, &id, &a.pw)?;
            let dest = a.to_folder.clone().unwrap_or_default();
            let moved = reg
                .with_place_mut(&id, |vp| -> Option<String> {
                    vp.root.find(&dest)?;
                    let r = vp.root.take_ref(&a.r#ref)?;
                    vp.root.find_mut(&dest)?.refs.push(r);
                    Some(a.r#ref.clone())
                })
                .flatten();
            match moved {
                Some(r) => {
                    out(ctx, &format!("已移动引用 {r}"), &json!({ "ref": r, "folder": dest }));
                    Ok(())
                }
                None => Err(VErr::no_target(&a.r#ref)),
            }
        }
        RefCmd::Copy(a) => ref_copy(ctx, reg, a),
        RefCmd::List(a) => ref_list(ctx, reg, a, true),
        RefCmd::Browse(a) => ref_list(ctx, reg, a, false),
    }
}

fn ref_add(ctx: &Ctx, reg: &VirtualRegistry, a: &RefAddArgs) -> std::result::Result<(), VErr> {
    let id = resolve(reg, &a.place)?;
    unlock_if_needed(reg, &id, &a.pw)?;
    // 从已保存的真实位置配置导出稳定标识：Telegram 用 user_id，WebDAV 用 url+账号。
    let sp = find_saved_place(ctx, &a.source_place)
        .ok_or_else(|| VErr::new("VIRTUAL_NO_SUCH_SOURCE", 1, format!("源真实位置不存在: {:?}", a.source_place)))?;
    let source = saved_to_source(&sp).ok_or_else(|| {
        VErr::new(
            "VIRTUAL_SOURCE_UNIDENTIFIED",
            1,
            format!("无法从源位置 {} 导出稳定标识（Telegram 缺 user_id，或类型不可引用）", sp.id),
        )
    })?;
    let folder = a.folder.clone().unwrap_or_default();
    let ref_id = format!("vr{}", virtuals::random_id());
    let done = reg
        .with_place_mut(&id, |vp| {
            vp.root.find_mut(&folder).map(|f| {
                f.refs.push(Reference {
                    ref_id: ref_id.clone(),
                    source,
                    dir_id: a.dir_id.clone(),
                    file_id: a.file_id.clone(),
                    snapshot: Snapshot {
                        name: a.name.clone(),
                        size: a.size,
                        media_tab: None,
                    },
                    tags: Vec::new(),
                    note: String::new(),
                });
            })
        })
        .flatten();
    match done {
        Some(()) => {
            out(ctx, &format!("已加引用 {ref_id}"), &json!({ "ref": ref_id, "place": id }));
            Ok(())
        }
        None => Err(VErr::no_target(&format!("文件夹 {folder:?}"))),
    }
}

fn ref_copy(ctx: &Ctx, reg: &VirtualRegistry, a: &RefCopyArgs) -> std::result::Result<(), VErr> {
    let from = resolve(reg, &a.from_place)?;
    let to = resolve(reg, &a.to_place)?;
    unlock_if_needed(reg, &from, &a.pw)?;
    unlock_if_needed(reg, &to, &a.pw)?;
    // 先在源位置克隆（只读），避免持两把锁跨两个 with_place。
    let clones: Vec<Reference> = reg
        .with_place(&from, |vp| {
            let mut out = Vec::new();
            collect_refs(&vp.root, &mut out);
            out.into_iter().filter(|r| a.ref_id.contains(&r.ref_id)).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if clones.len() != a.ref_id.len() {
        let missing: Vec<&str> = a
            .ref_id
            .iter()
            .filter(|id| !clones.iter().any(|r| &r.ref_id == *id))
            .map(String::as_str)
            .collect();
        return Err(VErr::no_target(&missing.join(", ")));
    }
    let dest = a.to_folder.clone().unwrap_or_default();
    let n = clones.len();
    reg.with_place_mut(&to, |vp| {
        let Some(d) = vp.root.find_mut(&dest) else { return; };
        for mut r in clones {
            r.ref_id = format!("vr{}", virtuals::random_id());
            d.refs.push(r);
        }
    });
    out(ctx, &format!("已复制 {n} 条引用"), &json!({ "copied": n, "to": to }));
    Ok(())
}

/// 列出一个文件夹下的引用。`recursive` 为 true 时递归整棵子树（list），
/// false 时只列直属（browse）。
fn ref_list(ctx: &Ctx, reg: &VirtualRegistry, a: &RefBrowseArgs, recursive: bool) -> std::result::Result<(), VErr> {
    let id = resolve(reg, &a.place)?;
    unlock_if_needed(reg, &id, &a.pw)?;
    let folder = a.folder.clone().unwrap_or_default();
    let rows = reg
        .with_place(&id, |vp| {
            let mut rows: Vec<Value> = Vec::new();
            let Some(node) = vp.root.find(&folder) else { return rows };
            if !recursive {
                for f in &node.folders {
                    rows.push(json!({ "type": "dir", "id": f.id, "name": f.name }));
                }
                for r in &node.refs {
                    rows.push(ref_row(r, ""));
                }
            } else {
                collect_refs(node, &mut Vec::new());
                walk_refs(node, "", &mut rows);
            }
            rows
        })
        .ok_or_else(|| VErr::no_place(&id))?;
    let human = if rows.is_empty() {
        "（空）".to_string()
    } else {
        rows.iter()
            .map(|r| {
                let t = if r["type"] == "dir" { "📁" } else { "📄" };
                format!("{} {}  {}", t, r["id"].as_str().unwrap_or(""), r["name"].as_str().unwrap_or(""))
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    out(ctx, &human, &json!({ "place": id, "entries": rows }));
    Ok(())
}

/// 一条引用的 JSON 行。
fn ref_row(r: &Reference, path: &str) -> Value {
    json!({
        "type": "ref",
        "id": r.ref_id,
        "name": r.snapshot.name,
        "size": r.snapshot.size,
        "path": path,
        "source": {
            "kind": r.source.kind,
            "dir_id": r.dir_id,
            "file_id": r.file_id,
        },
    })
}

/// 递归收集整棵子树里的引用。
fn collect_refs(f: &virtuals::VFolder, out: &mut Vec<Reference>) {
    out.extend(f.refs.iter().cloned());
    for c in &f.folders {
        collect_refs(c, out);
    }
}

/// 递归遍历，产出带路径的引用行。
fn walk_refs(f: &virtuals::VFolder, prefix: &str, out: &mut Vec<Value>) {
    for r in &f.refs {
        out.push(ref_row(r, prefix));
    }
    for c in &f.folders {
        let p = if prefix.is_empty() { c.name.clone() } else { format!("{}/{}", prefix, c.name) };
        walk_refs(c, &p, out);
    }
}

// ---- 共用小工具 ----

/// 统计一个位置的引用总数。
fn count_refs(f: &virtuals::VFolder) -> usize {
    f.refs.len() + f.folders.iter().map(count_refs).sum::<usize>()
}

/// 解析位置 id：先按精确 id，再按显示名唯一匹配。
fn resolve(reg: &VirtualRegistry, needle: &str) -> std::result::Result<String, VErr> {
    let all = reg.list_detailed();
    if let Some((id, _, _)) = all.iter().find(|(id, _, _)| id == needle) {
        return Ok(id.clone());
    }
    let hits: Vec<&str> = all.iter().filter(|(_, n, _)| n == needle).map(|(id, _, _)| id.as_str()).collect();
    match hits.len() {
        0 => Err(VErr::no_place(needle)),
        1 => Ok(hits.into_iter().next().unwrap_or_default().to_string()),
        n => Err(VErr::new(
            "VIRTUAL_AMBIGUOUS_NAME",
            1,
            format!("名字 {needle:?} 对应 {n} 个位置，请改用 id：{}", hits.join(", ")),
        )),
    }
}

/// 加密位置在操作前解锁；未加密位置直接放行。
fn unlock_if_needed(reg: &VirtualRegistry, id: &str, pw: &PasswordArgs) -> std::result::Result<(), VErr> {
    if !reg.is_encrypted(id) {
        return Ok(());
    }
    if reg.is_unlocked(id) {
        return Ok(());
    }
    let src = PasswordSource::from(pw);
    if src.env.is_none() && src.file.is_none() && !src.stdin {
        return Err(VErr::locked());
    }
    let pw = read_password(&src, "虚拟位置密码", false)
        .map_err(|e| VErr::new("VIRTUAL_PASSWORD_INPUT", 1, e.to_string()))?;
    reg.unlock_with_extra(id, &pw, &[], |_, _, _| {}).map_err(crypto)?;
    Ok(())
}

/// 从已保存的真实位置配置导出稳定标识。
fn saved_to_source(sp: &SavedPlace) -> Option<SourceRef> {
    match sp.kind.as_str() {
        "telegram" => sp.user_id.map(SourceRef::telegram),
        "webdav" => Some(SourceRef::webdav(sp.url.clone(), sp.username.clone())),
        _ => None,
    }
}

/// 在 CLI 配置里找一个真实位置（id 精确优先，名字唯一其次）。
fn find_saved_place(ctx: &Ctx, needle: &str) -> Option<SavedPlace> {
    let places = &ctx.cfg.remote.places;
    if let Some(sp) = places.iter().find(|p| p.id == needle) {
        return Some(sp.clone());
    }
    let hits: Vec<&SavedPlace> = places.iter().filter(|p| p.name == needle).collect();
    match hits.len() {
        1 => hits.into_iter().next().cloned(),
        _ => None,
    }
}

/// 统一输出入口。
fn out(ctx: &Ctx, human: &str, value: &Value) {
    ctx.out.result(human, value);
}
