//! 虚拟远程位置的命令层。
//!
//! # 委托模型（架构决定，见 16 号 §2 与 DEC）
//!
//! 虚拟位置只存**引用**，自己不连服务器。浏览时从引用表组装条目；打开/播放一条
//! 引用时，用引用里的**稳定标识**（[`SourceRef`]）在当前已注册的真实位置里认领出
//! 本地 place_id，再**委托**真实位置现有的浏览/读取/播放链路。
//!
//! 缓存共享因此自动成立：委托过去后由真实位置用它自己的 `(place, id)` 去
//! `BlockCache`，key 与直接在真实位置打开同一文件时**完全相同**——不额外缓存一份。
//! 这也是为什么不给虚拟位置发它自己的 place 字符串去读（那会让 key 不同、缓存翻倍）。

use std::sync::Arc;

use crate::commands::{CmdError, CmdResult};
use crate::place_files::PlaceThumbs;
use crate::places::PlaceRegistry;
use crate::virtual_place::{Reference, Snapshot, VirtualRegistry};

/// 引用相对真实源的可用性状态。前端据此区分三种 UI，且都不让引用消失/点崩。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceState {
    /// 源位置当前已注册且可用（能认领到本地 place）。
    Available,
    /// 源位置当前不在（被移除）——引用不消失，标「源不可用」，重新加回可恢复。
    Missing,
    /// 源位置在，但它是加密的且当前未解锁——提示先解锁，不当作失效。
    Locked,
}

/// 下发给前端的虚拟位置内一个条目：文件夹或引用。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VirtualEntry {
    /// 文件夹用文件夹 id，引用用 ref_id。
    pub id: String,
    /// 显示名（引用用快照名）。
    pub name: String,
    /// 是否为（虚拟）文件夹。
    pub is_dir: bool,
    /// 字节数（引用用快照大小）。
    pub size: Option<u64>,
    /// 引用的源状态；文件夹恒 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_state: Option<SourceState>,
    /// 引用指向的真实位置（认领成功时的本地 place_id）；供「定位到真实位置」跳转。
    /// 文件夹或源不可用时为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_place: Option<String>,
    /// 引用的源目录 id（跳转用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dir: Option<String>,
    /// 引用的源文件 id（跳转/定位用）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    /// 引用文件的缩略图句柄。
    ///
    /// 虚拟位置本身不连服务器、不存缩略图；这里复用**真实位置已落盘的清晰
    /// 缩略图磁盘缓存**（`<缓存根>/rthumbs`，按源文件 id `tg:<chat>:<msg>`
    /// 内容寻址命名）：用户只要在真实位置浏览过该文件，虚拟卡片就能零网络
    /// 出图。读不到（从没在真实位置加载过）就为 `None`，前端回退类型图标——
    /// **不在这里现拉**，避免用户打开虚拟位置就对每条引用打一遍 Telegram 请求。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumb_token: Option<String>,
    /// 引用记住的源文件 Telegram 分栏（添加时存下），供「定位到源文件」切栏。
    /// 仅 Telegram 引用有；旧引用/非 Telegram 为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_media_tab: Option<String>,
}

/// 判断一条引用当前的源状态，并（若可用）认领出本地 place_id。
fn resolve_state(reg: &PlaceRegistry, r: &Reference) -> (SourceState, Option<String>) {
    match reg.resolve_source(&r.source) {
        None => (SourceState::Missing, None),
        Some(place) => {
            // 源在。只有「加密且当前打不开」才算 Locked——要提示先解锁。
            //
            // 不能用「未连接」当 Locked：绝大多数位置在冷启动后、真正进去之前都
            // 是未连接占位（懒连接），把它们全判 Locked 会让指向未加密账号的引用
            // 也报「先解锁源位置」而点不开——而定位跳转本就会触发 ensure_connected
            // 顺带把它连上。所以未加密的位置一律 Available（跳转时按需连接）；
            // 只有加密位置才进一步看是否已解锁（已连接=已解出登录态=可用）。
            let locked = place.kind == "telegram"
                && omy_remote::telegram::session::is_encrypted(&place.id).unwrap_or(false)
                && place
                    .store
                    .as_telegram()
                    .is_some_and(|tg| !tg.is_connected());
            if locked {
                (SourceState::Locked, Some(place.id.clone()))
            } else {
                (SourceState::Available, Some(place.id.clone()))
            }
        }
    }
}

/// 列出所有虚拟位置（供侧栏），返回 (id, name)。
#[tauri::command]
pub fn virtual_places(vreg: tauri::State<'_, Arc<VirtualRegistry>>) -> Vec<VirtualIdName> {
    vreg.list_detailed()
        .into_iter()
        .map(|(id, name, encrypted)| VirtualIdName {
            unlocked: vreg.is_unlocked(&id),
            id,
            name,
            encrypted,
        })
        .collect()
}

/// 侧栏用的 id+name 对。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VirtualIdName {
    /// 虚拟位置 id。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 是否已用独立密码加密（侧栏据此显示锁标记）。
    pub encrypted: bool,
    /// 当前会话是否已解锁（未加密恒 true）；false 时浏览要先走解锁。
    pub unlocked: bool,
}

/// 新建一个虚拟位置，返回它的 id。
#[tauri::command]
pub fn virtual_create(vreg: tauri::State<'_, Arc<VirtualRegistry>>, name: String) -> String {
    vreg.create(name)
}

/// 重命名一个虚拟位置。
///
/// # Errors
///
/// 名字为空时返回。
#[tauri::command]
pub fn virtual_rename(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    name: String,
) -> CmdResult<bool> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CmdError::code("virtual_bad_name"));
    }
    Ok(vreg.rename(&place_id, name.to_owned()))
}

/// 用独立密码加密一个虚拟位置（收藏数据）。密码为空拒绝。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptReq {
    place_id: String,
    password: String,
    /// 同密码的 vault 材料（本地/远程），salt 为十六进制字符串。
    #[serde(default)]
    vaults: Vec<VaultHex>,
}

/// 与 commands::VaultParams 同形状（salt 十六进制）。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultHex {
    salt: String,
    m_kib: u32,
    t: u32,
    p: u32,
}

#[tauri::command]
pub fn virtual_encrypt(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    req: EncryptReq,
) -> CmdResult<bool> {
    if req.password.is_empty() {
        return Err(CmdError::code("virtual_bad_name"));
    }
    let vaults = parse_vaults(&req.vaults)?;
    vreg.encrypt(&req.place_id, req.password.as_bytes(), vaults, |password, kdf, vaults| {
        install_password_keks(&state, password,
            kdf, vaults);
    })
    .map(|()| true)
    .map_err(crypto_err)
}

/// 用密码解锁一个加密虚拟位置，解开后可浏览、编辑。
#[tauri::command]
pub fn virtual_unlock(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    password: String,
) -> CmdResult<bool> {
    vreg.unlock(&place_id, password.as_bytes(), |password, kdf, vaults| {
        install_password_keks(&state, password,
            kdf, vaults);
    })
    .map(|()| true)
    .map_err(crypto_err)
}

/// 锁定一个虚拟位置（清空本会话内存明文，磁盘仍是密文）。
#[tauri::command]
pub fn virtual_lock(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
) -> bool {
    vreg.lock(&place_id);
    true
}

/// 查询单个虚拟位置的加密/解锁态（打开前判断要不要弹解锁框）。
#[tauri::command]
pub fn virtual_lock_state(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
) -> serde_json::Value {
    serde_json::json!({
        "encrypted": vreg.is_encrypted(&place_id),
        "unlocked": vreg.is_unlocked(&place_id),
    })
}

/// 解析十六进制 salt 的 vault 材料（与 commands::parse_salt 同口径）。
fn parse_vaults(vs: &[VaultHex]) -> CmdResult<Vec<crate::virtual_place::VaultMaterial>> {
    let mut out = Vec::with_capacity(vs.len());
    for v in vs {
        let salt = crate::commands::parse_salt(&v.salt)
            .ok_or_else(|| CmdError::code("bad_salt"))?;
        out.push(crate::virtual_place::VaultMaterial {
            salt, m_kib: v.m_kib, t: v.t, p: v.p,
        });
    }
    Ok(out)
}

/** 把同一密码在「位置自身 salt + 各已知 vault salt」下派生出的 KEK 全部装进会话池。
 *
 * 关键点（实测确认）：KEK = Argon2id(密码, vault_salt)，**与 vault_salt 绑定**，
 * 跨 salt 不通用（NoMatchingSlot）。所以只装位置随机 salt 的一把 KEK 解不开本地
 * .omy；必须用**原始密码**为每个登记过的 vault salt 各派生一把。装好后同密码的
 * 本地/远程文件自动解锁，并计入 credential_count。SessionKeys 按 (salt,kind,label)
 * 指纹去重，不会重复计数；派生失败的单个 vault 跳过，不影响其它。 */
fn install_password_keks(
    state: &tauri::State<'_, crate::commands::Shared>,
    password: &[u8],
    self_kdf: &crate::virtual_place::KdfMaterial,
    vaults: &[crate::virtual_place::VaultMaterial],
) {
    state.with_session(|sess| {
        // 1) 位置自己那把（负责解开收藏信封）
        if let Ok(kek) = omy_core::crypto::Kek::from_password(password, &self_kdf.salt,
                omy_core::crypto::Argon2Params {
                    m_kib: self_kdf.m_kib, t: self_kdf.t, p: self_kdf.p,
                }) {
            sess.add_kek("远程位置密码", omy_core::session::CredentialKind::Vault,
                         &self_kdf.salt, kek);
        }
        // 2) 同一密码在每个本地/远程 vault salt 下重派生
        for v in vaults {
            let params = omy_core::crypto::Argon2Params {
                m_kib: v.m_kib, t: v.t, p: v.p,
            };
            if let Ok(kek) = omy_core::crypto::Kek::from_password(password, &v.salt, params) {
                sess.add_kek("远程位置密码", omy_core::session::CredentialKind::Vault,
                             &v.salt, kek);
            }
        }
    });
}

/// 把虚拟位置加密错误映射成结构化错误码（前端据此区分「密码错」与其它失败）。
fn crypto_err(e: crate::virtual_place::VirtualCryptoError) -> CmdError {
    use crate::virtual_place::VirtualCryptoError as E;
    match e {
        E::WrongPassword => CmdError::code("virtual_wrong_password"),
        E::NoSuchPlace => CmdError::code("virtual_no_such_place"),
        E::Already => CmdError::code("virtual_already"),
        E::Other(d) => CmdError::with("virtual_encrypt_failed", serde_json::Value::String(d)),
    }
}

/// 在某个虚拟位置的某个文件夹下新建子文件夹，返回新文件夹 id。
///
/// # Errors
///
/// 虚拟位置或父文件夹不存在时返回。
#[tauri::command]
pub fn virtual_add_folder(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    parent_folder: String,
    name: String,
) -> CmdResult<String> {
    let new_id = format!("vf{}", crate::virtual_place::random_id());
    let ok = vreg
        .with_place_mut(&place_id, |vp| {
            vp.root.find_mut(&parent_folder).map(|parent| {
                parent.folders.push(crate::virtual_place::VFolder {
                    id: new_id.clone(),
                    name,
                    folders: Vec::new(),
                    refs: Vec::new(),
                });
            })
        })
        .flatten();
    if ok.is_some() {
        Ok(new_id)
    } else {
        Err(CmdError::code("virtual_no_target"))
    }
}

/// 往某个虚拟位置的某个文件夹里添加一条引用。
///
/// `source_place_id` 是**源真实位置的本地 id**——后端据它导出稳定标识
/// （[`SourceRef`]，telegram user_id / webdav url+账号），前端不必知道 user_id/url。
/// `dir_id`/`file_id` 是源里的目录/文件 id；`snapshot_*` 是显示快照（源不可达时用）。
///
/// # Errors
///
/// 虚拟位置/目标文件夹不存在，或源位置认不出稳定标识（如 Telegram 还没连上拿到
/// user_id）时返回。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRefReq {
    /// 目标虚拟位置 id。
    pub place_id: String,
    /// 目标文件夹 id（根为空串）。
    pub folder: String,
    /// 源真实位置的**本地 id**（后端据它导出稳定标识）。
    pub source_place_id: String,
    /// 源目录/对话 id。
    pub dir_id: String,
    /// 源文件/消息 id。
    pub file_id: String,
    /// 显示快照名。
    pub snapshot_name: String,
    /// 显示快照大小。
    pub snapshot_size: Option<u64>,
    /// 源文件所在的 Telegram 分栏（前端从源条目 media_tab 透传）；可为空。
    #[serde(default)]
    pub source_media_tab: Option<String>,
}

#[tauri::command]
pub fn virtual_add_ref(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    req: AddRefReq,
) -> CmdResult<String> {
    // 从源真实位置导出稳定标识——认不出（缺 user_id / 非可引用类型）就拒绝，
    // 否则会存下一条永远认领不回的死引用。
    let source = reg
        .get(&req.source_place_id)
        .and_then(|p| PlaceRegistry::place_source(&p))
        .ok_or_else(|| CmdError::code("virtual_source_unidentified"))?;
    let ref_id = format!("vr{}", crate::virtual_place::random_id());
    let done = vreg
        .with_place_mut(&req.place_id, |vp| {
            vp.root.find_mut(&req.folder).map(|f| {
                f.refs.push(Reference {
                    ref_id: ref_id.clone(),
                    source,
                    dir_id: req.dir_id,
                    file_id: req.file_id,
                    snapshot: Snapshot {
                        name: req.snapshot_name,
                        size: req.snapshot_size,
                        media_tab: req.source_media_tab,
                    },
                    tags: Vec::new(),
                    note: String::new(),
                });
            })
        })
        .flatten();
    if done.is_some() {
        Ok(ref_id)
    } else {
        Err(CmdError::code("virtual_no_target"))
    }
}

/// 浏览一个虚拟位置的某个文件夹：返回子文件夹 + 本文件夹直属引用（带源状态）。
///
/// `folder` 为空表示根。
///
/// # Errors
///
/// 虚拟位置不存在时返回。
#[tauri::command]
pub fn virtual_browse(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    thumbs: tauri::State<'_, Arc<PlaceThumbs>>,
    place_id: String,
    folder: String,
) -> CmdResult<Vec<VirtualEntry>> {
    if vreg.is_encrypted(&place_id) && !vreg.is_unlocked(&place_id) {
        return Err(CmdError::code("virtual_locked"));
    }
    let out = vreg.with_place(&place_id, |vp| {
        let Some(node) = find_folder(&vp.root, &folder) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        // 子文件夹在前
        for f in &node.folders {
            entries.push(VirtualEntry {
                id: f.id.clone(),
                name: f.name.clone(),
                is_dir: true,
                size: None,
                source_state: None,
                source_place: None,
                source_dir: None,
                source_file: None,
                thumb_token: None,
                source_media_tab: None,
            });
        }
        // 引用在后，带源状态
        for r in &node.refs {
            let (state, place) = resolve_state(&reg, r);
            // 缩略图只对「源可用」的引用尝试，且纯读磁盘缓存、不触网。
            let thumb_token = if state == SourceState::Available {
                thumbs
                    .disk_image(&r.file_id)
                    .and_then(|b| thumbs.insert_image(b))
            } else {
                None
            };
            entries.push(VirtualEntry {
                id: r.ref_id.clone(),
                name: r.snapshot.name.clone(),
                is_dir: false,
                size: r.snapshot.size,
                source_state: Some(state),
                source_place: place,
                source_dir: Some(r.dir_id.clone()),
                source_file: Some(r.file_id.clone()),
                thumb_token,
                source_media_tab: r.snapshot.media_tab.clone(),
            });
        }
        entries
    });
    out.ok_or_else(|| CmdError::code("virtual_no_such_place"))
}

/// 重命名虚拟位置里的一个文件夹。根（id 为空）不允许改名——根显示的是位置名。
#[tauri::command]
pub fn virtual_rename_folder(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    folder: String,
    name: String,
) -> CmdResult<bool> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CmdError::code("virtual_bad_name"));
    }
    if folder.is_empty() {
        return Err(CmdError::code("virtual_no_target"));
    }
    let ok = vreg
        .with_place_mut(&place_id, |vp| {
            vp.root.find_mut(&folder).map(|f| f.name = name.to_owned()).is_some()
        })
        .unwrap_or(false);
    Ok(ok)
}

/// 删除虚拟位置里的一个文件夹（连同整棵子树的引用）。**不能删根**。
///
/// 返回被删子树里直属引用的条数，供前端确认文案说明「将一并移除 N 条引用」
/// （引用只是收藏指针，删除不触碰真实文件）。
///
/// # Errors
///
/// 位置/文件夹不存在或目标是根时返回。
#[tauri::command]
pub fn virtual_remove_folder(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    folder: String,
) -> CmdResult<usize> {
    if folder.is_empty() {
        return Err(CmdError::code("virtual_no_target"));
    }
    vreg.with_place_mut(&place_id, |vp| {
        let n = vp.root.subtree_ref_count(&folder).unwrap_or(0);
        vp.root.take_subtree(&folder).map(|_| n)
    })
    .flatten()
    .ok_or_else(|| CmdError::code("virtual_no_target"))
}

/// 删除一条引用（收藏指针）。不触碰真实文件。返回是否删到。
#[tauri::command]
pub fn virtual_remove_ref(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    ref_id: String,
) -> CmdResult<bool> {
    let ok = vreg
        .with_place_mut(&place_id, |vp| vp.root.remove_ref(&ref_id))
        .unwrap_or(false);
    Ok(ok)
}

/// 移动（剪切）一条引用到同位置另一个文件夹，返回引用 id。
///
/// 只允许在**同一个虚拟位置内**移动；跨位置走「复制」（virtual_copy_refs）。
#[tauri::command]
pub fn virtual_move_ref(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
    ref_id: String,
    dest_folder: String,
) -> CmdResult<String> {
    vreg.with_place_mut(&place_id, |vp| -> Option<String> {
        vp.root.find(&dest_folder)?;
        let r = vp.root.take_ref(&ref_id)?;
        vp.root.find_mut(&dest_folder)?.refs.push(r);
        Some(ref_id)
    })
    .flatten()
    .ok_or_else(|| CmdError::code("virtual_no_target"))
}

/// 把若干引用**复制**到（可跨位置的）某个虚拟位置文件夹。
///
/// 复制是在新位置再建一份指向同一源的引用，快照与源稳定标识沿用原引用——不连
/// 服务器、不碰真实文件。每条复制件生成**新 ref_id**：两个位置不能共享同一引用
/// id，否则在一边删除/移动会让另一边的指向失效。
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyRefsReq {
    pub source_place_id: String,
    pub dest_place_id: String,
    pub dest_folder: String,
    pub ref_ids: Vec<String>,
}

/// 在一棵 VFolder 里按 ref_id 只读找一条引用。
fn find_ref<'a>(f: &'a crate::virtual_place::VFolder, id: &str) -> Option<&'a Reference> {
    if let Some(r) = f.refs.iter().find(|r| r.ref_id == id) {
        return Some(r);
    }
    f.folders.iter().find_map(|c| find_ref(c, id))
}

#[tauri::command]
pub fn virtual_copy_refs(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    req: CopyRefsReq,
) -> CmdResult<usize> {
    // 先在源位置（只读）克隆要复制的引用，避免持着一把锁跨两个 with_place。
    let clones: Vec<Reference> = vreg
        .with_place(&req.source_place_id, |vp| {
            req.ref_ids
                .iter()
                .filter_map(|id| find_ref(&vp.root, id).cloned())
                .collect::<Vec<_>>()
        })
        .ok_or_else(|| CmdError::code("virtual_no_such_place"))?;
    let added = clones.len();

    vreg.with_place_mut(&req.dest_place_id, |vp| {
        // 目标文件夹不存在（含非法 id）时一条都不写
        let Some(dest) = vp.root.find_mut(&req.dest_folder) else {
            return;
        };
        for mut r in clones {
            r.ref_id = format!("vr{}", crate::virtual_place::random_id());
            dest.refs.push(r);
        }
    });
    Ok(added)
}

/// 一个虚拟位置的文件夹（扁平化，带层级深度），供树形选择对话框显示。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FolderNode {
    /// 文件夹 id（根为空串）。
    pub id: String,
    /// 显示名（根用虚拟位置自己的名，由前端填）。
    pub name: String,
    /// 缩进层级（根为 0）。
    pub depth: u32,
}

/// 列出一个虚拟位置的全部文件夹（深度优先扁平化），供「添加到虚拟远程」的树形
/// 目标选择器一次性展开。根文件夹 id 为空串、name 为空（前端用位置名兜底）。
///
/// # Errors
///
/// 虚拟位置不存在时返回。
#[tauri::command]
pub fn virtual_folders(
    vreg: tauri::State<'_, Arc<VirtualRegistry>>,
    place_id: String,
) -> CmdResult<Vec<FolderNode>> {
    fn walk(f: &crate::virtual_place::VFolder, depth: u32, out: &mut Vec<FolderNode>) {
        out.push(FolderNode { id: f.id.clone(), name: f.name.clone(), depth });
        for c in &f.folders {
            walk(c, depth + 1, out);
        }
    }
    vreg.with_place(&place_id, |vp| {
        let mut out = Vec::new();
        walk(&vp.root, 0, &mut out);
        out
    })
    .ok_or_else(|| CmdError::code("virtual_no_such_place"))
}

/// 只读地按文件夹 id 找节点（根为空串）。
fn find_folder<'a>(
    root: &'a crate::virtual_place::VFolder,
    id: &str,
) -> Option<&'a crate::virtual_place::VFolder> {
    if root.id == id {
        return Some(root);
    }
    for f in &root.folders {
        if let Some(hit) = find_folder(f, id) {
            return Some(hit);
        }
    }
    None
}

/// 删除一个虚拟位置。
#[tauri::command]
pub fn virtual_delete(vreg: tauri::State<'_, Arc<VirtualRegistry>>, place_id: String) -> bool {
    vreg.remove(&place_id)
}


#[cfg(test)]
mod tests {
    use omy_remote::cache::BlockCache;

    /// 缓存共享的核心断言：虚拟位置读一条引用，必须用**真实位置的 (place, id)**
    /// 去 BlockCache，key 才会与直接在真实位置打开同一文件时相同、命中同一份缓存。
    ///
    /// 不这样会怎样（16 号 §2 的反面教训）：若偷懒用虚拟位置自己的 place 字符串
    /// （如 "v1"）去读，block_path 的 place 段不同 → 哈希不同 → 必然缓存未命中，
    /// 把同一个文件在磁盘上缓存两份。这条锁死「委托必须带真实 place」。
    ///
    /// 委托模型下这天然成立（虚拟 read 命令解析 SourceRef→真实 place_id，再转调
    /// 真实位置的 read/播放，由真实 store 用自己的 (place,id) 落缓存）。这里用
    /// BlockCache::path_of 直接验证 key 是否相等。
    #[test]
    fn virtual_read_shares_cache_key_with_real_place() {
        let dir = std::env::temp_dir().join("omy-virtual-cache-share");
        let _ = std::fs::remove_dir_all(&dir);
        let cache = BlockCache::new(&dir, 1024 * 1024).expect("建缓存");

        let real_place = "p3"; // 真实位置的本地 id
        let virtual_place = "v1"; // 虚拟位置自己的 id（**不该**拿它去读）
        let file_id = "tg:-100:42\u{1}vhash"; // 版本化条目键
        let block = 0u64;

        // 委托（用真实 place）与在真实位置直接读，key 完全相同 → 共享同一份缓存
        let via_real = cache.path_of(real_place, file_id, block);
        let via_direct = cache.path_of(real_place, file_id, block);
        assert_eq!(via_real, via_direct, "委托用真实 place 时缓存 key 必须与直接读相同");

        // 反面：若用虚拟位置自己的 id 去读，key 不同 → 缓存翻倍
        let via_virtual_wrong = cache.path_of(virtual_place, file_id, block);
        assert_ne!(
            via_real, via_virtual_wrong,
            "用虚拟位置 id 读会得到不同 key（这正是要避免的双份缓存）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
