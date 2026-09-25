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

}

/// 一个虚拟远程位置的持久化数据。
///
/// 这份 JSON 就是「可被加密的 blob」：未加密时明文落盘，加密时用位置级 PDK seal。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualPlace {
    /// 稳定 id（跨重启不变，前端用它指代这个虚拟位置）。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 文件夹 + 引用的树。
    pub root: VFolder,
}

impl VirtualPlace {
    /// 新建一个空虚拟位置。
    #[must_use]
    pub fn new(id: String, name: String) -> Self {
        Self { id, name, root: VFolder::root() }
    }
}

/// 所有虚拟位置的注册表 + 磁盘持久化。
///
/// 落盘位置复用便携目录体系（`data_dir()`，与 session/config 同源），文件名
/// `virtual-places.json`。**本期先明文落盘**；将来位置级加密接上后，加密的虚拟
/// 位置这份 blob 会被 seal（见模块顶注），未加密的仍明文。
///
/// 线程安全：内部 `Mutex`。GUI 以 `Arc<VirtualRegistry>` 持有，命令层共享。
#[derive(Default)]
pub struct VirtualRegistry {
    places: std::sync::Mutex<Vec<VirtualPlace>>,
    /// 下一个虚拟位置 id 的序号，只增不减（同 `PlaceRegistry` 的理由：id 进
    /// 持久化，重发会让两个虚拟位置指向同一份数据）。
    next_seq: std::sync::Mutex<u64>,
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
        let list: Vec<VirtualPlace> = serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // 恢复 next_seq：扫已有 id 的数字后缀取最大值 +1，避免重发已用 id
        let max = list
            .iter()
            .filter_map(|p| p.id.strip_prefix('v').and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        if let (Ok(mut ps), Ok(mut seq)) = (self.places.lock(), self.next_seq.lock()) {
            *ps = list;
            *seq = max;
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
        let text = serde_json::to_string_pretty(&list)
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

    /// 列出所有虚拟位置的 (id, name)（供侧栏）。
    #[must_use]
    pub fn list(&self) -> Vec<(String, String)> {
        self.places
            .lock()
            .map(|ps| ps.iter().map(|p| (p.id.clone(), p.name.clone())).collect())
            .unwrap_or_default()
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

    /// 删除一个虚拟位置。返回是否真的删了（不存在为 false）。落盘。
    pub fn remove(&self, id: &str) -> bool {
        let removed = {
            let Ok(mut ps) = self.places.lock() else { return false };
            let before = ps.len();
            ps.retain(|p| p.id != id);
            ps.len() != before
        };
        if removed {
            let _ = self.save();
        }
        removed
    }
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
}
