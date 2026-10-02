//! 虚拟远程位置（收藏夹）：本地配置的「只存引用」的位置，**CLI 与 GUI 共用这一份**。
//!
//! # 为什么抽出来
//!
//! 「一个收藏夹位置怎么存、引用怎么组织、位置级密码怎么加密收藏数据」这三件事，
//! GUI 在侧栏做、CLI 在命令行做。两份实现各写一份的话，同一个 `virtual-places.json`
//! 格式、同一把位置密码的派生口径会在某次改动后漂移——CLI 加的收藏 GUI 认不出，
//! 或 GUI 加密的位置 CLI 解不开，而两边单看各自都「正常」。
//!
//! 这里收**两端都用得到**的纯业务：数据模型、文件夹/引用树操作、注册表与磁盘
//! 持久化、位置级加密/解锁/锁定/取消加密。GUI 特有的部分（把引用委托给真实位置
//! 去读、缩略图句柄、会话密钥池）仍留在 GUI 命令层，通过 [`VirtualRegistry`] 暴露的
//! `on_key` 回调接进来，本模块不碰 Tauri 与会话状态。
//!
//! # 为什么引用要记「稳定标识」而不是本地 place_id
//!
//! 真实位置的本地 id（`p3`/`nas` 之类）是进程/配置内序号：「从列表移除再重新加回」
//! 同一个账号，本地 id 会变。若引用记的是本地 id，源被移除再加回后就认不出「原来
//! 指向的是这个账号」，引用变成永久死链。
//!
//! 所以引用记的是**跨移除/重加仍稳定**的标识（[`SourceRef`]）：
//! - Telegram：服务端 `user_id`（公开数字 id，登录去重也用它）；
//! - WebDAV：服务器 URL + 账号（能唯一定位那个远程）。
//!
//! 加上对话/目录 id 与文件/消息 id，构成三层引用 key。打开时按这个稳定标识在当前
//! 已注册的真实位置里**重新认领**出本地 place_id，再委托它读——本地 id 变了也不影响。
//!
//! # 明文 / 密文边界
//!
//! 虚拟远程本身也可加密（复用位置级密码槽，与真实位置同一套）。加密的是**引用
//! 数据**（它指向了哪些账号的哪些文件，本身是隐私）。持久化结构与真实位置的
//! session 一样，是一份「可被 seal 的 JSON blob」——加密时用位置密码派生的密钥
//! seal 它，解锁时 unseal。userid/账号/URL 一类**在未加密时**明文可读（同真实位置
//! 加密口径：名称、userid 明文，便于锁定态下仍能显示与去重）。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个 vault 的派生材料（salt + Argon2 参数）。
///
/// 这是「同一密码在不同 salt 下各派生一把 KEK」所需的公开材料——salt 不是秘密
/// （.omy 文件头里本就明文带着），不存任何密码或密钥。
///
/// **CLI 与 GUI 共用这一份定义**：早先 GUI 的 `vault_reg` 与 `virtual_place` 各写了一份
/// 同形状结构，靠注释互相提醒；抽到这里后两边都是同一个类型，序列化形状天然一致。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct VaultMaterial {
    /// Argon2 salt（16 字节，明文，不是秘密）。
    pub salt: [u8; 16],
    /// Argon2 内存档位（KiB）。
    pub m_kib: u32,
    /// Argon2 时间迭代次数。
    pub t: u32,
    /// Argon2 并行度。
    pub p: u32,
}

impl VaultMaterial {
    /// salt 的十六进制，作去重/持久化键。
    #[must_use]
    pub fn salt_hex(&self) -> String {
        self.salt.iter().map(|b| format!("{b:02x}")).collect()
    }
}

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
    /// 驱动类型，与 `SavedPlace::kind` 一致（`telegram` / `webdav`）。
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
/// **不含 thumb_token**（那是会话内句柄、重启即失效）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// 显示名（未解密的名字）。
    pub name: String,
    /// 字节数；未知为 `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// 该文件所在的 Telegram 文件分栏（media/file/link/audio/gif）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_tab: Option<String>,
}

/// 一条引用：指向某个真实远程里的一个文件/媒体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// 引用在虚拟位置内的稳定 id（uuid 串）。
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
/// 这份 JSON 就是「可被加密的 blob」：未加密时明文落盘，加密时用位置密码派生的
/// 密钥 seal。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualPlace {
    /// 稳定 id（跨重启不变）。
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

/// 现场密码的 Argon2 派生材料（明文，salt 不是秘密）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KdfMaterial {
    /// Argon2 salt。
    pub salt: [u8; 16],
    /// Argon2 内存档位（KiB）。
    pub m_kib: u32,
    /// Argon2 时间迭代次数。
    pub t: u32,
    /// Argon2 并行度。
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
#[derive(Clone)]
struct UnlockState {
    kdf: KdfMaterial,
    key: Option<omy_secret::ProtectKey>,
    /// 加密时登记的 vault 材料；解锁后命令层据此为每个 vault 重派生同密码 KEK。
    vaults: Vec<VaultMaterial>,
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    vaults: Vec<VaultMaterial>,
}

/// 虚拟位置加解密相关操作的错误。
///
/// 变体同时服务 GUI（映射成 Tauri 错误码）与 CLI（映射成稳定退出码），所以这里
/// 直接带 `code()`/`exit_code()`，两端各取所需，不再各写一张映射表。
#[derive(Debug)]
pub enum VirtualCryptoError {
    /// 密码不对 / 信封认证失败。
    WrongPassword,
    /// 位置不存在。
    NoSuchPlace,
    /// 已经是目标态（对已加密位置再加密）。
    Already,
    /// 对未加密位置做了只对加密位置有意义的操作（解锁/锁定/取消加密）。
    NotEncrypted,
    /// 序列化/写盘等其它失败。
    Other(String),
}

impl std::fmt::Display for VirtualCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "密码不正确"),
            Self::NoSuchPlace => write!(f, "虚拟位置不存在"),
            Self::Already => write!(f, "已经加密"),
            Self::NotEncrypted => write!(f, "位置未加密"),
            Self::Other(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for VirtualCryptoError {}

impl VirtualCryptoError {
    /// 稳定的机器可读错误码（CLI `--json` 输出契约，脚本会匹配，勿改名）。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::WrongPassword => "VIRTUAL_WRONG_PASSWORD",
            Self::NoSuchPlace => "VIRTUAL_NO_SUCH_PLACE",
            Self::Already => "VIRTUAL_ALREADY_ENCRYPTED",
            Self::NotEncrypted => "VIRTUAL_NOT_ENCRYPTED",
            Self::Other(_) => "VIRTUAL_ENCRYPT_FAILED",
        }
    }

    /// 映射到 CLI 退出码：密码错复用全局 3，其余 1。
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::WrongPassword => 3,
            _ => 1,
        }
    }
}

/// 所有虚拟位置的注册表 + 磁盘持久化。
///
/// 落盘位置默认复用便携目录体系（`data_dir()`，与 session/config 同源），文件名
/// `virtual-places.json`。测试与 CLI 脚本可用 [`VirtualRegistry::with_path`] 注入
/// 隔离路径，不碰真实数据目录。
///
/// 线程安全：内部 `Mutex`。GUI 以 `Arc<VirtualRegistry>` 持有，命令层共享。
#[derive(Default)]
pub struct VirtualRegistry {
    places: std::sync::Mutex<Vec<VirtualPlace>>,
    /// 下一个虚拟位置 id 的序号，只增不减（同真实位置的理由：id 进持久化，
    /// 重发会让两个虚拟位置指向同一份数据）。
    next_seq: std::sync::Mutex<u64>,
    /// 加密位置的解锁态。进程重启后 key 复位为 None，必须重新输密码。
    unlock: std::sync::Mutex<std::collections::HashMap<String, UnlockState>>,
    /// 落盘路径。`None` 表示纯内存、永不读写磁盘（测试用）。
    store: Option<PathBuf>,
}

impl VirtualRegistry {
    /// 空注册表，落盘路径取默认数据目录下的 `virtual-places.json`。
    #[must_use]
    pub fn new() -> Self {
        let store = omy_config::data_dir().map(|d| d.join("virtual-places.json"));
        Self {
            places: std::sync::Mutex::new(Vec::new()),
            next_seq: std::sync::Mutex::new(0),
            unlock: std::sync::Mutex::new(std::collections::HashMap::new()),
            store,
        }
    }

    /// 落盘路径显式指定（测试 / CLI 脚本用隔离目录，不碰真实数据目录）。
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        let mut s = Self::new();
        s.store = Some(path);
        s
    }

    /// 纯内存注册表，永不读写磁盘（单测用）。
    #[must_use]
    pub fn new_in_memory() -> Self {
        let mut s = Self::new();
        s.store = None;
        s
    }

    /// 落盘路径。
    #[must_use]
    fn store_path(&self) -> Option<&Path> {
        self.store.as_deref()
    }

    /// 从磁盘载入。文件不存在当作空，不报错（首次运行）。
    ///
    /// # Errors
    ///
    /// 文件存在但读/解析失败时返回，让调用方决定提示——不静默吞掉，否则用户的
    /// 收藏会「莫名其妙没了」。
    pub fn load(&self) -> std::io::Result<()> {
        let Some(path) = self.store_path() else { return Ok(()) };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // 落盘是 StoredPlace（可能明文 root，也可能是加密信封 blob+kdf，没有 root）。
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
        let mut pending: std::collections::HashMap<String, UnlockState> =
            std::collections::HashMap::new();
        let list: Vec<VirtualPlace> = stored
            .into_iter()
            .map(|sp| {
                if sp.encrypted {
                    if let Some(kdf) = sp.kdf {
                        pending.insert(sp.id.clone(), UnlockState {
                            kdf,
                            key: None,
                            vaults: sp.vaults,
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
        let Some(path) = self.store_path() else { return Ok(()) };
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
                    id: vp.id,
                    name: vp.name,
                    encrypted: true,
                    root: None,
                    blob: Some(blob),
                    kdf: Some(st.kdf.clone()),
                    vaults: st.vaults.clone(),
                });
            } else {
                stored.push(StoredPlace {
                    id: vp.id,
                    name: vp.name,
                    encrypted: false,
                    root: Some(vp.root),
                    blob: None,
                    kdf: None,
                    vaults: Vec::new(),
                });
            }
        }
        let text = serde_json::to_string_pretty(&stored)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        omy_core::fsatomic::write_atomic(path, text.as_bytes())
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

    /// 列出所有虚拟位置的 (id, name)。
    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.places
            .lock()
            .map(|ps| ps.iter().map(|p| (p.id.clone(), p.name.clone())).collect())
            .unwrap_or_default()
    }

    /// 列出所有虚拟位置的 (id, name, encrypted)。
    #[must_use]
    pub fn list_detailed(&self) -> Vec<(String, String, bool)> {
        self.places
            .lock()
            .map(|ps| ps.iter().map(|p| (p.id.clone(), p.name.clone(), p.encrypted)).collect())
            .unwrap_or_default()
    }

    /// 当前所有加密位置的 KDF vault 材料（salt+Argon2 参数）。
    ///
    /// 启动 load() 之后调用，把每个加密虚拟位置自身的 salt 登记进全局 vault 表。
    #[must_use]
    pub fn encrypted_vault_materials(&self) -> Vec<VaultMaterial> {
        self.unlock
            .lock()
            .map(|u| {
                u.values()
                    .map(|st| VaultMaterial {
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
    /// unlock 并立即落盘。已经加密 / 位置不存在 / 密码为空都报错。
    ///
    /// `vaults` 是同密码的其它 vault 材料（本地/远程），随加密位置一起保存，解锁时
    /// 回装。`on_key` 是密钥派生成功后的回调：GUI 据此把位置自身与各 vault 的 KEK
    /// 装进会话密钥池；CLI 无会话池，传空闭包即可。
    ///
    /// # Errors
    ///
    /// 见 [`VirtualCryptoError`]。
    pub fn encrypt(
        &self,
        id: &str,
        password: &[u8],
        vaults: Vec<VaultMaterial>,
        on_key: impl FnOnce(&[u8], &KdfMaterial, &[VaultMaterial]),
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
            if let Ok(mut ps) = self.places.lock()
                && let Some(vp) = ps.iter_mut().find(|p| p.id == id) {
                    vp.encrypted = false;
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
        extra: &[VaultMaterial],
        on_key: impl FnOnce(&[u8], &KdfMaterial, &[VaultMaterial]),
    ) -> Result<(), VirtualCryptoError> {
        // 取 KDF（先 clone，避免持着 unlock 锁再去拿 places 锁）
        let kdf = {
            let u = self.unlock.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            u.get(id).map(|st| st.kdf.clone()).ok_or(VirtualCryptoError::NoSuchPlace)?
        };
        let key = derive_key(password, &kdf)?;
        // 信封来自磁盘：直接读 StoredPlace.blob
        let path = self
            .store_path()
            .ok_or_else(|| VirtualCryptoError::Other("no data dir".into()))?;
        let text = std::fs::read_to_string(path)
            .map_err(|e| VirtualCryptoError::Other(e.to_string()))?;
        let stored: Vec<StoredPlace> = serde_json::from_str(&text)
            .map_err(|_| VirtualCryptoError::WrongPassword)?;
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

    /// 取消加密：用密码解锁校验通过后，把位置写回明文树、丢弃信封与 KDF。
    ///
    /// 磁盘上的 `blob`/`kdf` 随之消失，收藏数据恢复为明文 `virtual-places.json`
    /// 里的一段。需要正确密码（先解锁校验），密码错保持现状不动。
    ///
    /// # Errors
    ///
    /// 位置不存在 / 未加密 / 密码错时报错。
    pub fn decrypt(
        &self,
        id: &str,
        password: &[u8],
    ) -> Result<(), VirtualCryptoError> {
        {
            let ps = self.places.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            let vp = ps.iter().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
            if !vp.encrypted {
                return Err(VirtualCryptoError::NotEncrypted);
            }
        }
        // 解锁（验密码 + 把明文树载入内存）；on_key 对 CLI 无意义，给空闭包
        self.unlock_with_extra(id, password, &[], |_, _, _| {})?;
        // 关掉加密标记；unlock 态随 save 的明文写盘一起丢弃
        {
            let mut ps = self.places.lock().map_err(|_| VirtualCryptoError::Other("lock".into()))?;
            let vp = ps.iter_mut().find(|p| p.id == id).ok_or(VirtualCryptoError::NoSuchPlace)?;
            vp.encrypted = false;
        }
        if let Ok(mut u) = self.unlock.lock() {
            u.remove(id);
        }
        self.save().map_err(|e| VirtualCryptoError::Other(e.to_string()))
    }

    /// 用会话里**已有的 KEK** 尝试自动解锁一个锁定的加密位置，无需用户再输密码。
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
        let Some(path) = self.store_path() else { return false };
        let Ok(text) = std::fs::read_to_string(path) else { return false };
        let Ok(stored) = serde_json::from_str::<Vec<StoredPlace>>(&text) else { return false };
        let Some(sp) = stored.into_iter().find(|p| p.id == id) else { return false };
        let Some(blob) = sp.blob else { return false };
        let Ok(env) = serde_json::from_str::<omy_secret::Envelope>(&blob) else { return false };

        // 逐把会话 KEK 试（纯试解逻辑抽到 unseal_tree_with_keks，便于单测）。
        let Some((key, root)) = unseal_tree_with_keks(&env, keks) else { return false };

        {
            // 开了：载入明文树
            if let Ok(mut ps) = self.places.lock()
                && let Some(vp) = ps.iter_mut().find(|p| p.id == id) {
                    vp.root = root;
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
        if let Ok(mut ps) = self.places.lock()
            && let Some(vp) = ps.iter_mut().find(|p| p.id == id)
                && vp.encrypted {
                    vp.root = VFolder::root();
                }
        if let Ok(mut u) = self.unlock.lock()
            && let Some(st) = u.get_mut(id) {
                st.key = None;
            }
    }
}

/// 用一批 KEK 尝试解开虚拟位置信封并反序列化出文件夹树。纯函数、不碰磁盘。
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
    use std::collections::HashSet;

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

    /// 测试用的隔离注册表：每个测试一个临时目录，绝不碰真实数据目录。
    fn test_reg() -> (VirtualRegistry, PathBuf) {
        let dir = std::env::temp_dir().join(format!("omy-virtuals-{}-{}", std::process::id(), random_id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("virtual-places.json");
        (VirtualRegistry::with_path(path), dir)
    }

    /// 稳定标识判等：Telegram 只看 user_id，本地 id/名字都不参与。
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

    /// 树操作：删引用、剪切移动、删文件夹（带子树）。
    #[test]
    fn ref_remove_move_and_subtree_count() {
        let mut vp = VirtualPlace::new("v1".into(), "收藏".into());
        vp.root.refs.push(make_ref("r0"));
        vp.root.folders.push(VFolder {
            id: "f1".into(), name: "一".into(), folders: Vec::new(),
            refs: vec![make_ref("r1")],
        });
        vp.root.find_mut("f1").unwrap().folders.push(VFolder {
            id: "f2".into(), name: "二".into(), folders: Vec::new(),
            refs: vec![make_ref("r2")],
        });

        assert_eq!(vp.root.subtree_ref_count("f1"), Some(2));
        assert_eq!(vp.root.subtree_ref_count("f2"), Some(1));

        let moved = vp.root.take_ref("r2").expect("找到 r2");
        assert_eq!(moved.ref_id, "r2");
        vp.root.find_mut("f2").unwrap().refs.is_empty();
        vp.root.refs.push(moved);
        assert!(vp.root.refs.iter().any(|r| r.ref_id == "r2"));
        assert_eq!(vp.root.subtree_ref_count("f1"), Some(1));

        assert!(vp.root.remove_ref("r0"));
        assert!(!vp.root.remove_ref("r0"));
        assert_eq!(vp.root.subtree_ref_count("f1"), Some(1));

        assert!(vp.root.take_subtree("f1").is_some());
        assert!(vp.root.find("f1").is_none());
        assert!(vp.root.find("f2").is_none());
        assert!(vp.root.take_subtree("").is_none());
    }

    /// 加密→落盘→重新 load→解锁往返：错误密码解不开，正确密码还原树。
    ///
    /// 这是 CLI 跨进程语义的核心保证：加密后落盘的是信封，新进程 load 后必须能
    /// 凭密码把树读回来。早先的内存版测试不碰磁盘，兜不住「信封没写对」这类错。
    #[test]
    fn encrypt_persists_and_reopens_in_new_registry() {
        let (reg, dir) = test_reg();
        let id = reg.create("私密".into());
        reg.with_place_mut(&id, |vp| vp.root.refs.push(make_ref("r1")));
        reg.encrypt(&id, b"hunter2", Vec::new(), |_, _, _| {}).expect("加密");
        assert!(reg.is_encrypted(&id));
        assert!(reg.is_unlocked(&id));

        // 新进程语义：另一个注册表从同一文件 load
        let reloaded = VirtualRegistry::with_path(dir.join("virtual-places.json"));
        reloaded.load().expect("load");
        assert!(reloaded.is_encrypted(&id));
        assert!(!reloaded.is_unlocked(&id), "新进程必须是锁定态");
        // 锁定态读不到引用
        reloaded.with_place(&id, |vp| assert!(vp.root.refs.is_empty()));

        // 错密码解不开
        assert!(matches!(
            reloaded.unlock_with_extra(&id, b"wrong", &[], |_, _, _| {}),
            Err(VirtualCryptoError::WrongPassword)
        ));
        // 对密码还原
        reloaded.unlock_with_extra(&id, b"hunter2", &[], |_, _, _| {}).expect("解锁");
        assert!(reloaded.is_unlocked(&id));
        reloaded.with_place(&id, |vp| {
            assert_eq!(vp.root.refs.len(), 1);
            assert_eq!(vp.root.refs[0].snapshot.name, "r1.bin");
        });
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 取消加密：加密→decrypt→重新 load 后是明文树、无信封。
    #[test]
    fn decrypt_removes_encryption() {
        let (reg, dir) = test_reg();
        let id = reg.create("私密".into());
        reg.with_place_mut(&id, |vp| vp.root.refs.push(make_ref("r9")));
        reg.encrypt(&id, b"pw", Vec::new(), |_, _, _| {}).expect("加密");

        // 错密码不能取消
        assert!(matches!(
            reg.decrypt(&id, b"nope"),
            Err(VirtualCryptoError::WrongPassword)
        ));
        // 对密码取消
        reg.decrypt(&id, b"pw").expect("取消加密");
        assert!(!reg.is_encrypted(&id));

        // 落盘里该位置是明文 root、无 blob
        let text = std::fs::read_to_string(dir.join("virtual-places.json")).unwrap();
        assert!(text.contains("r9.bin"), "明文树应在落盘里");
        assert!(!text.contains("\"blob\""), "取消加密后不应再留信封");

        // 新进程直接读到树，无需密码
        let reloaded = VirtualRegistry::with_path(dir.join("virtual-places.json"));
        reloaded.load().expect("load");
        reloaded.with_place(&id, |vp| assert_eq!(vp.root.refs.len(), 1));
        std::fs::remove_dir_all(&dir).ok();
    }

    /// lock_all 一键锁掉**所有**加密位置。
    #[test]
    fn lock_all_locks_every_encrypted_place() {
        let (reg, _dir) = test_reg();
        let a = reg.create("甲".into());
        let b = reg.create("乙".into());
        reg.with_place_mut(&a, |vp| vp.root.refs.push(make_ref("ra")));
        reg.with_place_mut(&b, |vp| vp.root.refs.push(make_ref("rb")));
        reg.encrypt(&a, b"pw1", Vec::new(), |_, _, _| {}).expect("加密甲");
        reg.encrypt(&b, b"pw2", Vec::new(), |_, _, _| {}).expect("加密乙");
        let plain = reg.create("明文".into());

        assert!(reg.is_unlocked(&a));
        assert!(reg.is_unlocked(&b));

        reg.lock_all();

        assert!(!reg.is_unlocked(&a), "加密甲必须被锁");
        assert!(!reg.is_unlocked(&b), "加密乙必须被锁");
        reg.with_place(&a, |vp| assert!(vp.root.refs.is_empty()));
        reg.with_place(&b, |vp| assert!(vp.root.refs.is_empty()));
        assert!(reg.is_encrypted(&a));
        assert!(reg.is_encrypted(&b));
        assert!(reg.is_unlocked(&plain));
    }

    /// 自动解锁的纯试解核心：对的 KEK 能解信封还原树，错 salt 的 KEK 不行。
    #[test]
    fn unseal_tree_with_keks_needs_matching_salt() {
        use omy_core::crypto::{Argon2Params, Kek};

        let pw = b"132";
        let weak = Argon2Params::TEST_WEAK;
        let own_salt = [5u8; 16];
        let other_salt = [6u8; 16];

        let kek = Kek::from_password(pw, &own_salt, weak).expect("kdf");
        let mut raw = [0u8; 32];
        raw.copy_from_slice(kek.as_key().as_bytes());
        let protect = zeroize::Zeroizing::new(raw);
        let mut tree = VFolder::root();
        tree.name = "收藏".to_string();
        let json = serde_json::to_vec(&tree).expect("ser");
        let env = omy_secret::seal(&protect, &json).expect("seal");

        let wrong = Kek::from_password(pw, &other_salt, weak).expect("kdf2");
        assert!(
            unseal_tree_with_keks(&env, std::slice::from_ref(&wrong)).is_none(),
            "跨 salt 的 KEK 不能解开本位置信封"
        );

        let (_, got) = unseal_tree_with_keks(&env, std::slice::from_ref(&kek))
            .expect("匹配 salt 的 KEK 应能自动解锁");
        assert_eq!(got.name, "收藏");
    }

    /// 虚拟位置重命名。
    #[test]
    fn registry_rename() {
        let (reg, _dir) = test_reg();
        let id = reg.create("旧名".into());
        assert!(reg.rename(&id, "新名".into()));
        assert_eq!(reg.list(), vec![(id.clone(), "新名".to_string())]);
        assert!(!reg.rename("nope", "x".into()));
    }

    /// 并发 create 不丢更新、id 不撞。
    ///
    /// 不这样会怎样：两个线程同时 create 拿到同一个 next_seq，两个虚拟位置共用一个
    /// id——后写的 save 把先写的整份树覆盖掉（数据静默丢失）。
    #[test]
    fn concurrent_create_does_not_collide() {
        use std::sync::Arc;
        use std::thread;
        let (reg, _dir) = test_reg();
        let reg = Arc::new(reg);
        let mut handles = Vec::new();
        for i in 0..16 {
            let r = Arc::clone(&reg);
            handles.push(thread::spawn(move || r.create(format!("并发{i}"))));
        }
        let mut ids: HashSet<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(ids.len(), 16, "16 次并发 create 必须得到 16 个不同 id");
        // 落盘后仍是 16 个
        reg.load().expect("reload");
        assert_eq!(reg.list().len(), 16, "落盘/重载后数量不丢: {:?}", ids);
        ids.clear();
        let _ = &ids;
    }

    /// id 序号在重载后继续递增，不复用已用号。
    #[test]
    fn seq_keeps_growing_across_reload() {
        let (reg, dir) = test_reg();
        let a = reg.create("甲".into());
        let b = reg.create("乙".into());
        assert_eq!((a.as_str(), b.as_str()), ("v1", "v2"));
        reg.remove(&a);
        // 重载后分配：不能因为 v1 空了就复用 v1
        let reloaded = VirtualRegistry::with_path(dir.join("virtual-places.json"));
        reloaded.load().unwrap();
        let c = reloaded.create("丙".into());
        assert_eq!(c, "v3", "重载后必须继续 v3，不能复用已删的 v1");
        std::fs::remove_dir_all(&dir).ok();
    }
}
