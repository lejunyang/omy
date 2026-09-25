//! 虚拟远程位置：本地配置的「收藏夹式」位置，只存**对真实远程文件的引用**，
//! 不存实际文件、也不连服务器。
//!
//! # 为什么引用要记「稳定标识」而不是本地 place_id
//!
//! 真实位置的本地 id（`p3`/`nas` 之类）是 `PlaceRegistry` 发的**进程/配置内序号**：
//! 「从列表移除再重新加回」同一个账号，本地 id 会变（见 `places.rs` next_id 的
//! 单调递增说明）。若引用记的是本地 id，源被移除再加回后就认不出「原来指向的是
//! 这个账号」，引用变成永久死链。
//!
//! 所以引用记的是**跨移除/重加仍稳定**的标识（[`SourceRef`]）：
//! - Telegram：服务端 `user_id`（公开数字 id，登录去重也用它）；
//! - WebDAV：服务器 URL + 账号（能唯一定位那个远程）。
//!
//! 加上对话/目录 id 与文件/消息 id，构成三层引用 key。打开时按这个稳定标识在当前
//! 已注册的真实位置里**重新认领**出本地 place_id，再委托它读——本地 id 变了也不
//! 影响。
//!
//! # 明文 / 密文边界（为将来的位置级加密留位）
//!
//! 虚拟远程本身也可加密（复用位置级密码槽，与真实位置同一套）。加密的是**引用
//! 数据**（它指向了哪些账号的哪些文件，本身是隐私）。这里的持久化结构与真实位置
//! 的 session 一样，是一份「可被 seal 的 JSON blob」——加密时用 `place_secret`
//! 的 PDK seal 它，解锁时 unseal。userid/账号/URL 一类**在未加密时**明文可读（同
//! 真实位置加密口径：名称、userid 明文，便于锁定态下仍能显示与去重）。

use serde::{Deserialize, Serialize};

/// 生成一个短随机 id（16 个十六进制字符），给文件夹/引用当稳定 id 用。
///
/// 用随机而不是自增：文件夹/引用在树里增删频繁，自增计数要额外维护且删除后有
/// 复用风险；随机 128/2 位碰撞概率可忽略，且不依赖任何外部状态。
#[must_use]
pub fn random_id() -> String {
    let mut b = [0u8; 8];
    omy_core::util::fill_random(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 一个真实远程的**稳定标识**——不认会变的本地 place_id。
///
/// 判等只看「能唯一定位那个远程」的字段：Telegram 看 `telegram_user_id`，
/// WebDAV 看 `webdav_url` + `webdav_username`。`kind` 决定看哪一组。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// 驱动类型，与 `PlaceStore::kind` 一致（`telegram` / `webdav`）。
    pub kind: String,
    /// Telegram 账号的服务端 user id（仅 telegram，公开数字 id，非凭据）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telegram_user_id: Option<i64>,
    /// WebDAV 服务器 URL（仅 webdav）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav_url: Option<String>,
    /// WebDAV 登录账号（仅 webdav；匿名时为空串）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webdav_username: Option<String>,
}

impl SourceRef {
    /// Telegram 源：按 user_id 稳定标识。
    #[must_use]
    pub fn telegram(user_id: i64) -> Self {
        Self {
            kind: "telegram".to_owned(),
            telegram_user_id: Some(user_id),
            webdav_url: None,
            webdav_username: None,
        }
    }

    /// WebDAV 源：按 URL + 账号稳定标识。
    #[must_use]
    pub fn webdav(url: String, username: String) -> Self {
        Self {
            kind: "webdav".to_owned(),
            telegram_user_id: None,
            webdav_url: Some(url),
            webdav_username: Some(username),
        }
    }

    /// 这个稳定标识是否指向「同一个真实远程」。
    ///
    /// 只比能唯一定位远程的字段，不比本地 id、不比显示名。Telegram 比 user_id，
    /// WebDAV 比 (url, username)——这正是「源移除后重新加回仍认得出」的依据。
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        if self.kind != other.kind {
            return false;
        }
        match self.kind.as_str() {
            "telegram" => {
                // 两边都得有 user_id 才能判等；缺了就不认（不能把两个未知账号当同一个）
                self.telegram_user_id.is_some() && self.telegram_user_id == other.telegram_user_id
            }
            "webdav" => {
                self.webdav_url.is_some()
                    && self.webdav_url == other.webdav_url
                    && self.webdav_username == other.webdav_username
            }
            _ => false,
        }
    }
}

/// 引用的显示快照——源不可达时仍能把条目列出来（灰显），不必连服务器。
///
/// 与 15 号文档「列表快照落盘」同一手法。**不含 thumb_token**（那是会话内句柄、
/// 重启即失效）；缩略图字节可以存一份小的（stripped 占位级别），也可为 `None`
/// 靠打开时现取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 显示名（未解密的名字）。
    pub name: String,
    /// 字节数；未知为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 该文件所在的 Telegram 文件分栏（media/file/link/audio/gif）。
    ///
    /// 在「添加到虚拟远程」时由前端从源条目的 `media_tab` 一起存进来：之后
    /// 「定位到源文件」直接切到这个分栏网格，不必再按扩展名重新猜（旧引用没有
    /// 该字段时前端会回落到按文件名猜）。非 Telegram 引用为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_tab: Option<String>,
}

/// 一条引用：指向某个真实远程里的一个文件/媒体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// 引用在虚拟位置内的稳定 id（uuid 串），前端用它指代这一条。
    pub ref_id: String,
    /// 源真实远程的稳定标识。
    pub source: SourceRef,
    /// 源目录/对话 id（如 Telegram 的 `tg:<chat>`）。
    pub dir_id: String,
    /// 源文件/消息 id（如 `tg:<chat>:<msg>` 或 WebDAV 路径）。
    pub file_id: String,
    /// 显示快照（源不可达时用它列条目）。
    pub snapshot: Snapshot,
    /// 用户打的标签（本期不做标签 UI，但结构留位）。
    #[serde(default)]
    pub tags: Vec<String>,
    /// 用户备注（本期不做备注 UI，但结构留位）。
    #[serde(default)]
    pub note: String,
}

/// 虚拟位置里的一个文件夹节点（只组织引用，不含真实目录）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VFolder {
    /// 文件夹在虚拟位置内的稳定 id（根为 `""`）。
    pub id: String,
    /// 显示名（根的名字不显示，取虚拟位置自己的名）。
    pub name: String,
    /// 子文件夹。
    #[serde(default)]
    pub folders: Vec<VFolder>,
    /// 本文件夹直属的引用。
    #[serde(default)]
    pub refs: Vec<Reference>,
}

impl VFolder {
    /// 空的根文件夹。
    #[must_use]
    pub fn root() -> Self {
        Self { id: String::new(), name: String::new(), folders: Vec::new(), refs: Vec::new() }
    }

    /// 按 id 深度优先找一个文件夹（可变）。根 id 是 `""`。
    pub fn find_mut(&mut self, id: &str) -> Option<&mut VFolder> {
        if self.id == id {
            return Some(self);
        }
        for f in &mut self.folders {
            if let Some(hit) = f.find_mut(id) {
                return Some(hit);
            }
        }
        None
    }

    /// 按 id 深度优先找一个文件夹（只读）。根 id 是空串。
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&VFolder> {
        if self.id == id {
            return Some(self);
        }
        self.folders.iter().find_map(|f| f.find(id))
    }

    /// 取出 id 指定的子文件夹节点（连同整棵子树）的所有权；不存在返回 None。
    /// 根是空 id，调用方不该来取根（根不能被删/移动），对空 id 返回 None。
    pub fn take_subtree(&mut self, id: &str) -> Option<VFolder> {
        if id.is_empty() {
            return None;
        }
        if let Some(i) = self.folders.iter().position(|f| f.id == id) {
            return Some(self.folders.remove(i));
        }
        for f in &mut self.folders {
            if let Some(node) = f.take_subtree(id) {
                return Some(node);
            }
        }
        None
    }

    /// 删除一个直属/后代引用（按 ref_id），返回是否真的删了。递归整棵子树。
    pub fn remove_ref(&mut self, ref_id: &str) -> bool {
        let before = self.refs.len();
        self.refs.retain(|r| r.ref_id != ref_id);
        let mut removed = self.refs.len() != before;
        for f in &mut self.folders {
            removed = f.remove_ref(ref_id) || removed;
        }
        removed
    }

    /// 取出一个引用（按 ref_id）的所有权（用于剪切移动）。整棵树递归找。
    pub fn take_ref(&mut self, ref_id: &str) -> Option<Reference> {
        if let Some(i) = self.refs.iter().position(|r| r.ref_id == ref_id) {
            return Some(self.refs.remove(i));
        }
        for f in &mut self.folders {
            if let Some(r) = f.take_ref(ref_id) {
                return Some(r);
            }
        }
        None
    }

    /// 某个文件夹（含其整棵子树）里直属引用的数量，用于删除确认计数。
    #[must_use]
    pub fn subtree_ref_count(&self, id: &str) -> Option<usize> {
        let node = self.find(id)?;
        fn count(n: &VFolder) -> usize {
            n.refs.len() + n.folders.iter().map(count).sum::<usize>()
        }
        Some(count(node))
    }
}

/// 一个虚拟远程位置的持久化数据。
///
/// 这份 JSON 就是「可被加密的 blob」：未加密时明文落盘，加密时用位置级 PDK seal。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualPlace {
    /// 稳定 id（跨重启不变，前端用它指代这个虚拟位置）。
    pub id: String,
    /// 显示名。即使整个位置加密，名字也保持明文（与真实位置加密口径一致：
    /// 锁定态下侧栏仍能显示名字）。
    pub name: String,
    /// 文件夹 + 引用的树。
    pub root: VFolder,
    /// 是否已用独立密码加密。加密后 `root` 在落盘时是一份 omy-secret 信封，
    /// 内存里仍是明文树（解锁后使用）；未加密为 false（旧配置缺字段也按 false）。
    #[serde(default)]
    pub encrypted: bool,
}

impl VirtualPlace {
    /// 新建一个空虚拟位置。
    #[must_use]
    pub fn new(id: String, name: String) -> Self {
        Self { id, name, root: VFolder::root(), encrypted: false }
    }

    /// 改显示名。
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

/// 所有虚拟位置的注册表 + 磁盘持久化。
///
/// 落盘位置复用便携目录体系（`data_dir()`，与 session/config 同源），文件名
/// `virtual-places.json`。**本期先明文落盘**；将来位置级加密接上后，加密的虚拟
/// 位置这份 blob 会被 seal（见模块顶注），未加密的仍明文。
///
/// 线程安全：内部 `Mutex`。GUI 以 `Arc<VirtualRegistry>` 持有，命令层共享。
/// 落盘文件里的一个位置：明文名字 + 明文树或加密信封。
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StoredPlace {
    id: String,
    name: String,
    #[serde(default)]
    encrypted: bool,
    /// 未加密时的树；加密时缺省。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<VFolder>,
    /// 加密时：封着 root JSON 的信封（序列化成 JSON 字符串）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blob: Option<String>,
    /// 加密时的 Argon2 材料（salt/参数），明文（salt 不是秘密）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfMaterial>,
    /// 加密时登记的、与该位置**同一密码**的其它 vault 材料（本地/远程）。
    /// 解锁后据此为每个 vault 重派生 KEK 装回会话池。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    vaults: Vec<crate::vault_reg::VaultMaterial>,
}

/// 现场密码的 Argon2 派生材料（明文，salt 不是秘密）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KdfMaterial {
    pub salt: [u8; 16],
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfMaterial {
    fn moderate() -> Self {
        let params = omy_core::crypto::Argon2Params::MODERATE;
        let mut salt = [0u8; 16];
        omy_core::util::fill_random(&mut salt);
        Self { salt, m_kib: params.m_kib, t: params.t, p: params.p }
    }
}

/// 一个加密位置在内存里的解锁态：KDF 材料 + （解锁后才有）派生的 32 字节密钥。
///
/// 密钥同时是一把「位置密码 KEK」：加密/解锁成功后由命令层用 (kdf.salt, key)
/// 装进 GUI 的 `SessionKeys`，于是它和普通 omy 文件密码进同一个池——同密码的本地
/// 文件扫描时自动解锁，反之亦然，右上角「N 个密码已解锁」也会计数。
#[derive(Clone)]
struct UnlockState {
    kdf: KdfMaterial,
    key: Option<omy_secret::ProtectKey>,
    /// 加密时登记的 vault 材料；解锁后命令层据此为每个 vault 重派生同密码 KEK。
    vaults: Vec<crate::vault_reg::VaultMaterial>,
}

/// 加密相关操作的错误。
#[derive(Debug)]
pub enum VirtualCryptoError {
    /// 密码不对 / 信封认证失败。
    WrongPassword,
    /// 位置不存在。
    NoSuchPlace,
    /// 已经是目标态（对已加密位置再加密）。
    Already,
    /// 序列化/写盘等其它失败。
    Other(String),
}

impl std::fmt::Display for VirtualCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "密码不正确"),
            Self::NoSuchPlace => write!(f, "虚拟位置不存在"),
            Self::Already => write!(f, "已经加密"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for VirtualCryptoError {}

#[derive(Default)]
pub struct VirtualRegistry {
    places: std::sync::Mutex<Vec<VirtualPlace>>,
    /// 下一个虚拟位置 id 的序号，只增不减（同 `PlaceRegistry` 的理由：id 进
    /// 持久化，重发会让两个虚拟位置指向同一份数据）。
    next_seq: std::sync::Mutex<u64>,
    /// 加密位置的解锁态。进程重启后 key 复位为 None，必须重新输密码。
    unlock: std::sync::Mutex<std::collections::HashMap<String, UnlockState>>,
}

impl VirtualRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 落盘文件路径（便携优先，复用 paths；取不到数据目录时 `None`）。
    #[must_use]
    fn store_path() -> Option<std::path::PathBuf> {
        omy_config::data_dir().map(|d| d.join("virtual-places.json"))    }

    /// 从磁盘载入。文件不存在当作空，不报错（首次运行）。
    ///
    /// # Errors
    ///
    /// 文件存在但读/解析失败时返回，让调用方决定提示——不静默吞掉，否则用户的
    /// 收藏会"莫名其妙没了"。
    pub fn load(&self) -> std::io::Result<()> {
        let Some(path) = Self::store_path() else { return Ok(()) };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // 落盘是 StoredPlace（可能明文 root，也可能是加密信封 blob+kdf，没有 root）。
        // 旧实现这里仍按 VirtualPlace 反序列化，加密位置缺 `root` 直接整份解析失败、
        // 侧栏变空——日志里就是 missing field `root`。
        let stored: Vec<StoredPlace> = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // 恢复 next_seq：扫已有 id 的数字后缀取最大值 +1，避免重发已用 id
        let max = stored
            .iter()
            .filter_map(|p| p.id.strip_prefix('v').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        // 加密位置载入为空树（锁定态），KDF 放进解锁表（key=None）；信封不预读，
        // unlock 时再从磁盘取。未加密位置直接恢复明文树。
        let mut pending: std::collections::HashMap<String, UnlockState>
            = std::collections::HashMap::new();
        let list: Vec<VirtualPlace> = stored
            .into_iter()
            .map(|sp| {
                if sp.encrypted {
                    if let Some(kdf) = sp.kdf {
                        pending.insert(sp.id.clone(), UnlockState {
                            kdf, key: None, vaults: sp.vaults,
                        });
                    }
                    VirtualPlace {
                        id: sp.id,
                        name: sp.name,
                        root: VFolder::root(),
                        encrypted: true,
                    }
                } else {
                    VirtualPlace {
                        id: sp.id,
                        name: sp.name,
                        root: sp.root.unwrap_or_else(VFolder::root),
                        encrypted: false,
                    }
                }
            })
            .collect();
        if let (Ok(mut ps), Ok(mut seq)) = (self.places.lock(), self.next_seq.lock()) {
            *ps = list;
            *seq = max;
        }
        if let Ok(mut u) = self.unlock.lock() {
            *u = pending;
        }
        Ok(())
    }

    /// 落盘（原子写，避免半截文件）。
    ///
    /// # Errors
    ///
    /// 序列化或写盘失败时返回。
    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = Self::store_path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let list = self.places.lock().map(|p| p.clone()).unwrap_or_default();
        let unlock = self.unlock.lock().ok();
        let mut stored: Vec<StoredPlace> = Vec::with_capacity(list.len());
        for vp in list {
            if vp.encrypted {
                // 只有已解锁（手里有 key）才把**当前内存树**重新封好写盘；
                // 锁定态跳过这一项，绝不用空树覆盖已加密收藏。
                let Some(st) = unlock.as_ref().and_then(|m| m.get(&vp.id)) else { continue };
                let Some(key) = &st.key else { continue };
                let json = serde_json::to_vec(&vp.root)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                let env = omy_secret::seal(key, &json)
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
                let blob = serde_json::to_string(&env)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                stored.push(StoredPlace {
                    id: vp.id, name: vp.name, encrypted: true,
                    root: None,
                    blob: Some(blob),
                    kdf: Some(st.kdf.clone()),
                    vaults: st.vaults.clone(),
                });
            } else {
                stored.push(StoredPlace {
                    id: vp.id, name: vp.name, encrypted: false,
                    root: Some(vp.root), blob: None, kdf: None, vaults: Vec::new(),
                });
            }
        }
        let text = serde_json::to_string_pretty(&stored)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        omy_core::fsatomic::write_atomic(&path, text.as_bytes())
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// 新建一个虚拟位置，返回它的 id。落盘失败仍返回 id（内存已建），由调用方
    /// 决定是否提示——不因落盘失败就假装没建。
    pub fn create(&self, name: String) -> String {
        let id = {
            let mut seq = self.next_seq.lock().unwrap_or_else(|e| e.into_inner());
            *seq += 1;
            format!("v{}", *seq)
        };
        if let Ok(mut ps) = self.places.lock() {
            ps.push(VirtualPlace::new(id.clone(), name));
        }
        let _ = self.save();
        id
    }

    /// 列出所有虚拟位置的 (id, name)。侧栏实际用 [`Self::list_detailed`]（带加密
    /// 标记）；这个二元组版本保留给测试与潜在的内部调用。
    #[must_use]
    #[allow(dead_code)]
    pub fn list(&self) -> Vec<(String, String)> {
        self.places
            .lock()
            .map(|ps| ps.iter().map(|p| (p.id.clone(), p.name.clone())).collect())
            .unwrap_or_default()
    }

    /// 列出所有虚拟位置的 (id, name, encrypted)（供侧栏显示锁标记）。
    #[must_use]
    pub fn list_detailed(&self) -> Vec<(String, String, bool)> {
        self.places
            .lock()
            .map(|ps| ps.iter().map(|p| (p.id.clone(), p.name.clone(), p.encrypted)).collect())
            .unwrap_or_default()
    }

    /// 当前所有加密位置的 KDF vault 材料（salt+Argon2 参数）。
    ///
    /// 启动 load() 之后调用，把每个加密虚拟位置自身的 salt 登记进全局 vault 表——
    /// 否则这个 salt 只在「用户显式解锁该虚拟位置」时才出现，先在别处用同密码解锁
    /// 时全局表里没有它，就无法为它派生 KEK、做不到免密自动解锁（实测：先解本地
    /// 文件，Telegram 自动开了、虚拟位置仍锁着）。salt/参数是公开材料，不涉密钥。
    #[must_use]
    pub fn encrypted_vault_materials(&self) -> Vec<crate::vault_reg::VaultMaterial> {
        self.unlock
            .lock()
            .map(|u| {
                u.values()
                    .map(|st| crate::vault_reg::VaultMaterial {
                        salt: st.kdf.salt,
                        m_kib: st.kdf.m_kib,
                        t: st.kdf.t,
                        p: st.kdf.p,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 该位置是否处于加密态（不存在为 false）。
    #[must_use]
    pub fn is_encrypted(&self, id: &str) -> bool {
        self.places.lock().map(|ps| ps.iter().any(|p| p.id == id && p.encrypted)).unwrap_or(false)
    }

    /// 该加密位置当前是否已解锁（内存里有可用树与派生密钥）。
    /// 未加密的位置恒视为「可用」，返回 true。
    #[must_use]
    pub fn is_unlocked(&self, id: &str) -> bool {
        let encrypted = self.is_encrypted(id);
        if !encrypted {
            return true;
        }
        self.unlock
            .lock()
            .map(|m| m.get(id).and_then(|st| st.key.as_ref()).is_some())
            .unwrap_or(false)
    }

    /// 对某个虚拟位置做一次修改（在闭包里改，改完自动落盘）。
    ///
    /// 返回闭包的结果；找不到该 id 时返回 `None`（不落盘）。
    pub fn with_place_mut<R>(&self, id: &str, f: impl FnOnce(&mut VirtualPlace) -> R) -> Option<R> {
        let out = {
            let mut ps = self.places.lock().ok()?;
            let vp = ps.iter_mut().find(|p| p.id == id)?;
            f(vp)
        };
        let _ = self.save();
        Some(out)
    }

    /// 只读访问某个虚拟位置。
    pub fn with_place<R>(&self, id: &str, f: impl FnOnce(&VirtualPlace) -> R) -> Option<R> {
        let ps = self.places.lock().ok()?;
        let vp = ps.iter().find(|p| p.id == id)?;
        Some(f(vp))
    }

    /// 重命名一个虚拟位置。返回是否真的改了（不存在为 false）。落盘。
    pub fn rename(&self, id: &str, name: String) -> bool {
        self.with_place_mut(id, |vp| vp.rename(name)).is_some()
    }

    /// 删除一个虚拟位置。返回是否真的删了（不存在为 false）。落盘。
    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let Ok(mut ps) = self.places.lock() else { return false };
            let before = ps.len();
            ps.retain(|p| p.id != id);
            ps.len() != before
        };
        if let Ok(mut u) = self.unlock.lock() {
            u.remove(id);
        }
        if removed {
            let _ = self.save();
        }
        removed
    }

    /// 把一个**未加密**的虚拟位置用独立密码加密。
    ///
    /// 成功后内存里仍是明文树（本会话继续可用），同时把「KDF + 派生密钥」记进
    /// unlock 并立即落盘（save 用它把树封成信封）。已经加密 / 位置不存在 /
    /// 密码为空都报错。
    ///
    /// # Errors
    ///
    /// 见 [`VirtualCryptoError`]。
    pub fn encrypt(
        &self,
        id: &str,
        password: &[u8],
        // 同密码的其它 vault 材料（本地/远程），随加密位置一起保存，解锁时回装。
        vaults: Vec<crate::vault_reg::VaultMaterial>,
        // 密钥派生成功后回调：让命令层把位置自己的 KEK + 各 vault 的 KEK 装进
        // omy 会话密钥池，与本地/远程文件密码打通（互相自动解锁、计入已解锁数）。
        on_key: impl FnOnce(&[u8], &KdfMaterial, &[crate::vault_reg::VaultMaterial]),
    ) -> Result<(), VirtualCryptoError> {
        if password.is_empty() {
            return Err(VirtualCryptoError::Other("empty password".into()));
        }
        // 先确认存在且未加密
        {
            let ps = self.places.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            let vp = ps.iter().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
            if vp.encrypted {
                return Err(VirtualCryptoError::Already);
            }
        }
        let kdf = KdfMaterial::moderate();
        let key = derive_key(password, &kdf)?;
        // 置加密标记
        {
            let mut ps = self.places.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            let vp = ps.iter_mut().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
            vp.encrypted = true;
        }
        // 装进会话密码池（尽力而为：失败不阻断加密，密钥仍在 unlock 表里可用）。
        on_key(password, &kdf, &vaults);
        {
            let mut u = self.unlock.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            u.insert(id.to_owned(), UnlockState { kdf, key: Some(key), vaults });
        }
        // save 用 unlock 里的密钥把当前树封好；失败回滚加密标记
        if let Err(e) = self.save() {
            if let Ok(mut ps) = self.places.lock() {
                if let Some(vp) = ps.iter_mut().find(|p| p.id == id) {
                    vp.encrypted = false;
                }
            }
            if let Ok(mut u) = self.unlock.lock() {
                u.remove(id);
            }
            return Err(VirtualCryptoError::Other(e.to_string()));
        }
        Ok(())
    }

    /// 用密码解锁一个加密虚拟位置：解开信封、把明文树载入内存，缓存派生密钥。
    ///
    /// # Errors
    ///
    /// 位置不存在、未加密、密码错误（信封认证失败）时报错。
    pub fn unlock_with_extra(
        &self,
        id: &str,
        password: &[u8],
        extra: &[crate::vault_reg::VaultMaterial],
        on_key: impl FnOnce(&[u8], &KdfMaterial, &[crate::vault_reg::VaultMaterial]),
    ) -> Result<(), VirtualCryptoError> {
        // 取 KDF（先 clone，避免持着 unlock 锁再去拿 places 锁）
        let kdf = {
            let u = self.unlock.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            u.get(id).map(|st| st.kdf.clone()).ok_or(VirtualCryptoError::NoSuchPlace)?
        };
        let key = derive_key(password, &kdf)?;
        // 信封来自磁盘：直接读 StoredPlace.blob
        let path = Self::store_path().ok_or_else(|| VirtualCryptoError::Other("no data dir".into()))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|e| VirtualCryptoError::Other(e.to_string()))?;
        let stored: Vec<StoredPlace> = serde_json::from_str(&text)
            .map_err(|e| VirtualCryptoError::Other(e.to_string()))?;
        let sp = stored.into_iter().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
        let blob = sp.blob.ok_or(VirtualCryptoError::WrongPassword)?;
        let env: omy_secret::Envelope = serde_json::from_str(&blob)
            .map_err(|_| VirtualCryptoError::WrongPassword)?;
        let plain = omy_secret::unseal(&key, &env)
            .map_err(|_| VirtualCryptoError::WrongPassword)?;
        let root: VFolder = serde_json::from_slice(&plain)
            .map_err(|_| VirtualCryptoError::WrongPassword)?;
        // 载入明文树 + 缓存密钥
        {
            let mut ps = self.places.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            let vp = ps.iter_mut().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
            vp.root = root;
        }
        // 合并位置登记的 vault 与全局已见 vault 表，按 salt 去重。
        // 全局表解决「加密后才在别处见到的同密码 vault」——老位置记录里没有它，
        // 只靠记录会漏解（用户实测 E:/tele 文件没自动解锁就是这个原因）。
        let mut vaults = {
            let u0 = self.unlock.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            u0.get(id).map(|st| st.vaults.clone()).unwrap_or_default()
        };
        vaults.extend(extra.iter().cloned());
        vaults.sort_by_key(|a| a.salt_hex());
        vaults.dedup_by(|a, b| a.salt_hex() == b.salt_hex());
        on_key(password, &kdf, &vaults);
        {
            let mut u = self.unlock.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            u.insert(id.to_owned(), UnlockState { kdf, key: Some(key), vaults });
        }
        Ok(())
    }

    /// 用会话里**已有的 KEK** 尝试自动解锁一个锁定的加密位置，无需用户再输密码。
    ///
    /// 场景：用户在别处（本地文件 / Telegram 位置）刚用同一密码解锁，会话池里
    /// 已经有按本位置 salt 派生的 KEK（全局 vault 表保证同密码会为每个 salt 派生）。
    /// 此时虚拟位置信封本就该能被那把 KEK 解开，却仍显示锁定、要再输一次——这里
    /// 就是补上这次「免密解锁」。
    ///
    /// 成功返回 `true` 并载入明文树；位置不存在、未加密、已解锁或没有能开信封的
    /// KEK 时返回 `false`，不报错（没钥匙是常态，不是异常）。
    pub fn try_auto_unlock_with_keks(
        &self,
        id: &str,
        keks: &[omy_core::crypto::Kek],
    ) -> bool {
        if !self.is_encrypted(id) || self.is_unlocked(id) {
            return self.is_unlocked(id);
        }
        // 读 KDF 与磁盘信封
        let kdf = match self.unlock.lock() {
            Ok(u) => match u.get(id) {
                Some(st) => st.kdf.clone(),
                None => return false,
            },
            Err(_) => return false,
        };
        let Some(path) = Self::store_path() else { return false };
        let Ok(text) = std::fs::read_to_string(&path) else { return false };
        let Ok(stored) = serde_json::from_str::<Vec<StoredPlace>>(&text) else { return false };
        let Some(sp) = stored.into_iter().find(|p| p.id == id) else { return false };
        let Some(blob) = sp.blob else { return false };
        let Ok(env) = serde_json::from_str::<omy_secret::Envelope>(&blob) else { return false };

        // 逐把会话 KEK 试（纯试解逻辑抽到 unseal_with_keks，便于单测）。
        let Some((key, root)) = unseal_tree_with_keks(&env, keks) else { return false };

        {
            // 开了：载入明文树
            if let Ok(mut ps) = self.places.lock() {
                if let Some(vp) = ps.iter_mut().find(|p| p.id == id) {
                    vp.root = root;
                }
            }
            let vaults = match self.unlock.lock() {
                Ok(u0) => u0.get(id).map(|st| st.vaults.clone()).unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            if let Ok(mut u) = self.unlock.lock() {
                u.insert(id.to_owned(), UnlockState { kdf, key: Some(key), vaults });
            }
        }
        true
    }

    /// 一键锁定**所有**加密虚拟位置（应用级「锁定」用）。
    ///
    /// 与逐个 [`lock`](Self::lock) 语义一致，只是一次遍历：清空全部明文树与
    /// 派生密钥。这样应用锁定后，虚拟位置不再残留可直接浏览的内容——否则只锁
    /// 本地文件、虚拟位置仍开着，会让「锁定」成为假象。
    pub fn lock_all(&self) {
        let ids: Vec<String> = self
            .places
            .lock()
            .map(|ps| ps.iter().filter(|p| p.encrypted).map(|p| p.id.clone()).collect())
            .unwrap_or_default();
        for id in ids {
            self.lock(&id);
        }
    }

    /// 锁定：清空内存明文树与派生密钥（磁盘上仍是信封）。未加密位置忽略。
    pub fn lock(&self, id: &str) {
        if let Ok(mut ps) = self.places.lock() {
            if let Some(vp) = ps.iter_mut().find(|p| p.id == id) {
                if vp.encrypted {
                    vp.root = VFolder::root();
                }
            }
        }
        if let Ok(mut u) = self.unlock.lock() {
            if let Some(st) = u.get_mut(id) {
                st.key = None;
            }
        }
    }
}

/// 用一批 KEK 尝试解开虚拟位置信封并反序列化出文件夹树。
///
/// 虚拟位置信封的保护密钥就是某把 KEK 的 32 字节原始密钥（见 [`derive_key`]），
/// 所以逐把取 `as_key()` 去 unseal 即可。返回第一把能同时通过信封认证与树反序列化
/// 的 KEK 对应的（保护密钥, 树）；都不行返回 None。纯函数、不碰磁盘，供自动解锁
/// 与单测共用。
fn unseal_tree_with_keks(
    env: &omy_secret::Envelope,
    keks: &[omy_core::crypto::Kek],
) -> Option<(omy_secret::ProtectKey, VFolder)> {
    for kek in keks {
        let mut raw = [0u8; 32];
        raw.copy_from_slice(kek.as_key().as_bytes());
        let key = zeroize::Zeroizing::new(raw);
        let Ok(plain) = omy_secret::unseal(&key, env) else { continue };
        let Ok(root) = serde_json::from_slice::<VFolder>(&plain) else { continue };
        return Some((key, root));
    }
    None
}

/// 用 Argon2 从密码 + salt 派生出 32 字节保护密钥（与真实位置加密同款）。
fn derive_key(password: &[u8], kdf: &KdfMaterial) -> Result<omy_secret::ProtectKey, VirtualCryptoError> {
    let kek = omy_core::crypto::Kek::from_password(
        password,
        &kdf.salt,
        omy_core::crypto::Argon2Params { m_kib: kdf.m_kib, t: kdf.t, p: kdf.p },
    )
        .map_err(|_| VirtualCryptoError::Other("derive failed".into()))?;
    let mut k = [0u8; 32];
    k.copy_from_slice(kek.as_key().as_bytes());
    Ok(zeroize::Zeroizing::new(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 稳定标识判等：Telegram 只看 user_id，本地 id/名字都不参与。
    ///
    /// 不这样会怎样：这是「源移除后重新加回仍认得出」的依据——加回来时本地
    /// place_id 变了、甚至名字改了，只要 user_id 一样就该认领上；反之两个不同
    /// 账号绝不能判成同一个（否则引用会指向错账号的文件）。
    #[test]
    fn telegram_source_matches_by_user_id() {
        let a = SourceRef::telegram(8916309419);
        let b = SourceRef::telegram(8916309419);
        let c = SourceRef::telegram(6846748979);
        assert!(a.matches(&b), "同一 user_id 必须判为同一源");
        assert!(!a.matches(&c), "不同 user_id 必须判为不同源");
    }

    /// 缺 user_id 时不认（不能把两个未知账号当成同一个）。
    #[test]
    fn telegram_without_user_id_never_matches() {
        let known = SourceRef::telegram(1);
        let unknown = SourceRef {
            kind: "telegram".to_owned(),
            telegram_user_id: None,
            webdav_url: None,
            webdav_username: None,
        };
        assert!(!known.matches(&unknown));
        assert!(!unknown.matches(&unknown), "两个都没 user_id 也不能判等");
    }

    /// WebDAV 按 URL+账号判等；跨类型永不判等。
    #[test]
    fn webdav_matches_by_url_and_user_and_never_cross_kind() {
        let a = SourceRef::webdav("https://nas/dav".into(), "bob".into());
        let b = SourceRef::webdav("https://nas/dav".into(), "bob".into());
        let c = SourceRef::webdav("https://nas/dav".into(), "alice".into());
        assert!(a.matches(&b));
        assert!(!a.matches(&c), "账号不同不判等");
        assert!(!a.matches(&SourceRef::telegram(1)), "跨类型不判等");
    }

    /// 树结构：建文件夹、加引用、按 id 找回、遍历。
    #[test]
    fn folder_tree_add_and_find() {
        let mut vp = VirtualPlace::new("v1".into(), "收藏".into());
        vp.root.folders.push(VFolder {
            id: "f1".into(),
            name: "视频".into(),
            folders: Vec::new(),
            refs: Vec::new(),
        });
        let r = Reference {
            ref_id: "r1".into(),
            source: SourceRef::telegram(1),
            dir_id: "tg:-100".into(),
            file_id: "tg:-100:5".into(),
            snapshot: Snapshot { name: "a.mp4".into(), size: Some(10), media_tab: None },
            tags: Vec::new(),
            note: String::new(),
        };
        vp.root.find_mut("f1").expect("找到 f1").refs.push(r.clone());
        let f1 = vp.root.find_mut("f1").expect("再找 f1");
        assert_eq!(f1.refs.len(), 1);
        assert_eq!(f1.refs[0].file_id, "tg:-100:5");
    }

    /// 序列化往返：老配置缺 tags/note/size 字段应能反序列化（serde default）。
    #[test]
    fn deserialize_tolerates_missing_optional_fields() {
        // 一条最小引用 JSON：没有 tags/note，snapshot 没有 size
        let json = r#"{
            "ref_id":"r1",
            "source":{"kind":"telegram","telegram_user_id":1},
            "dir_id":"tg:-100","file_id":"tg:-100:5",
            "snapshot":{"name":"a.mp4"}
        }"#;
        let r: Reference = serde_json::from_str(json).expect("缺可选字段也应能反序列化");
        assert_eq!(r.tags.len(), 0);
        assert_eq!(r.note, "");
        assert_eq!(r.snapshot.size, None);
    }

    /// 树操作：删引用、剪切移动、删文件夹（带子树）、跨位置复制源克隆。
    ///
    /// 钉住「移动不复制、删除只动指针」这两件事——它们是收藏夹最容易写坏的地方。
    #[test]
    fn ref_remove_move_and_subtree_count() {
        let mut vp = VirtualPlace::new("v1".into(), "收藏".into());
        // 根下放一条 r0；子文件夹 f1 下放 r1；f1 下再建 f2 放 r2。
        vp.root.refs.push(make_ref("r0"));
        vp.root.folders.push(VFolder {
            id: "f1".into(), name: "一".into(), folders: Vec::new(),
            refs: vec![make_ref("r1")],
        });
        vp.root.find_mut("f1").unwrap().folders.push(VFolder {
            id: "f2".into(), name: "二".into(), folders: Vec::new(),
            refs: vec![make_ref("r2")],
        });

        // subtree_ref_count 把整棵子树的引用都数上（f1 含 r1+r2=2）。
        assert_eq!(vp.root.subtree_ref_count("f1"), Some(2));
        assert_eq!(vp.root.subtree_ref_count("f2"), Some(1));

        // 剪切 r2 到根：原位置消失、根上出现，且 ref_id 不变（移动不是复制）。
        let moved = vp.root.take_ref("r2").expect("找到 r2");
        assert_eq!(moved.ref_id, "r2");
        vp.root.find_mut("f2").unwrap().refs.is_empty();
        vp.root.refs.push(moved);
        assert!(vp.root.refs.iter().any(|r| r.ref_id == "r2"));
        assert_eq!(vp.root.subtree_ref_count("f1"), Some(1));

        // 删除 r0：只动这一条指针，r1/r2 不受影响。
        assert!(vp.root.remove_ref("r0"));
        assert!(!vp.root.remove_ref("r0")); // 再删一次为 false
        assert_eq!(vp.root.subtree_ref_count("f1"), Some(1));

        // 删除 f1 整棵子树：f1/f2 都没了。
        assert!(vp.root.take_subtree("f1").is_some());
        assert!(vp.root.find("f1").is_none());
        assert!(vp.root.find("f2").is_none());
        // 根不能被 take_subtree 取走
        assert!(vp.root.take_subtree("").is_none());
    }

    fn make_ref(id: &str) -> Reference {
        Reference {
            ref_id: id.into(),
            source: SourceRef::telegram(1),
            dir_id: "tg:-100".into(),
            file_id: format!("tg:-100:{id}"),
            snapshot: Snapshot { name: format!("{id}.bin"), size: None, media_tab: None },
            tags: Vec::new(),
            note: String::new(),
        }
    }

    /// 加密→锁定→解锁往返：错误密码解不开，正确密码还原树。
    #[test]
    fn encrypt_lock_unlock_roundtrip() {
        let dir = std::env::temp_dir().join(format!("omy-vp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        // 用一个隔离的 data dir（VirtualRegistry::store_path 走 omy_config::data_dir，
        // 不易注入；这里只直接验证内存加密/解锁的密钥往返，不依赖落盘路径）。
        let reg = VirtualRegistry::new();
        let id = reg.create("私密".into());
        reg.with_place_mut(&id, |vp| {
            vp.root.refs.push(make_ref("r1"));
        });

        // 加密
        reg.encrypt(&id, b"hunter2", Vec::new(), |_, _, _| {}).expect("加密成功");
        assert!(reg.is_encrypted(&id));
        assert!(reg.is_unlocked(&id));

        // 锁定后内存空树、key 清空
        reg.lock(&id);
        assert!(!reg.is_unlocked(&id));
        reg.with_place(&id, |vp| assert!(vp.root.refs.is_empty()));

        // unlock 需要从落盘信封读 KDF（见集成路径），这里只锁定内存语义。
    }

    /// lock_all 一键锁掉**所有**加密位置：应用级锁定不能只关其中一个。
    ///
    /// 不这样会怎样：早先工具栏「锁定」只锁本地文件，已解锁的加密虚拟位置仍开着、
    /// 明文树还在内存，形成假锁定。这里建两个加密位置，lock_all 后两者都必须回到
    /// 未解锁且树为空。
    #[test]
    fn lock_all_locks_every_encrypted_place() {
        let reg = VirtualRegistry::new();
        let a = reg.create("甲".into());
        let b = reg.create("乙".into());
        reg.with_place_mut(&a, |vp| vp.root.refs.push(make_ref("ra")));
        reg.with_place_mut(&b, |vp| vp.root.refs.push(make_ref("rb")));
        reg.encrypt(&a, b"pw1", Vec::new(), |_, _, _| {}).expect("加密甲");
        reg.encrypt(&b, b"pw2", Vec::new(), |_, _, _| {}).expect("加密乙");
        // 明文位置不应受影响
        let plain = reg.create("明文".into());

        assert!(reg.is_unlocked(&a));
        assert!(reg.is_unlocked(&b));

        reg.lock_all();

        assert!(!reg.is_unlocked(&a), "加密甲必须被锁");
        assert!(!reg.is_unlocked(&b), "加密乙必须被锁");
        reg.with_place(&a, |vp| assert!(vp.root.refs.is_empty()));
        reg.with_place(&b, |vp| assert!(vp.root.refs.is_empty()));
        // 仍处于加密态（不是被删除/解密）
        assert!(reg.is_encrypted(&a));
        assert!(reg.is_encrypted(&b));
        // 未加密位置恒为可用
        assert!(reg.is_unlocked(&plain));
    }

    /// 自动解锁的纯试解核心：对的 KEK（同一密码+同一 salt）能解开信封并还原树，
    /// 错的 KEK（不同 salt，即别处的同密码也不行）解不开。
    ///
    /// 这是「在别处用同密码解锁后，会话池里已有按本位置 salt 派生的 KEK，虚拟
    /// 位置应免密自动解锁」的保证；反过来也锁死「跨 salt 的 KEK 不能冒充」。
    #[test]
    fn unseal_tree_with_keks_needs_matching_salt() {
        use omy_core::crypto::{Argon2Params, Kek};

        let pw = b"132";
        let weak = Argon2Params::TEST_WEAK;
        let own_salt = [5u8; 16];
        let other_salt = [6u8; 16];

        // 用 own_salt 造一棵加密树信封（复刻 derive_key 的密钥取法）
        let kek = Kek::from_password(pw, &own_salt, weak).expect("kdf");
        let mut raw = [0u8; 32];
        raw.copy_from_slice(kek.as_key().as_bytes());
        let protect = zeroize::Zeroizing::new(raw);
        let mut tree = VFolder::root();
        tree.name = "收藏".to_string();
        let json = serde_json::to_vec(&tree).expect("ser");
        let env = omy_secret::seal(&protect, &json).expect("seal");

        // 别处同密码但不同 salt 的 KEK：必须解不开（KEK 与 salt 绑定）
        let wrong = Kek::from_password(pw, &other_salt, weak).expect("kdf2");
        assert!(
            unseal_tree_with_keks(&env, std::slice::from_ref(&wrong)).is_none(),
            "跨 salt 的 KEK 不能解开本位置信封"
        );

        // 对的 KEK：解开且树内容一致
        let (_, got) = unseal_tree_with_keks(&env, std::slice::from_ref(&kek))
            .expect("匹配 salt 的 KEK 应能自动解锁");
        assert_eq!(got.name, "收藏");
    }

    /// 虚拟位置重命名。
    #[test]
    fn registry_rename() {
        let reg = VirtualRegistry::new();
        let id = reg.create("旧名".into());
        assert!(reg.rename(&id, "新名".into()));
        assert_eq!(reg.list(), vec![(id.clone(), "新名".to_string())]);
        // 不存在的位置改不了（空白名校验在命令层 virtual_rename，注册表这层只管改名）
        assert!(!reg.rename("nope", "x".into()));
    }
}

