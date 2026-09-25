//! 远程位置：注册、连接与浏览。
//!
//! # 与 `remote.rs` 的分工
//!
//! `remote.rs` 是**局域网远端**：对端是另一台运行 omy 的设备，走 Noise
//! 信道，共享是一份平铺清单。
//!
//! 本模块是**远程存储位置**：WebDAV 服务器、NAS、被中转出来的云盘。
//! 它有真实的目录层级，能进能出，与本地目录同构——所以前端复用同一套
//! 浏览界面，只靠能力位图决定哪些操作可用。
//!
//! 两者不合并，是因为寻址方式和信任模型完全不同：一个用 handle 且对端
//! 是已配对设备，一个用路径且服务端不可信。
//!
//! # 凭据不进 WebView
//!
//! 前端只拿到 `place_id` 这种不透明标识。密码、令牌一律留在后端——
//! 它们进了 WebView 就等于多一处泄露面，而前端拿它们也没有用途。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use omy_remote::telegram::TelegramStore;
use omy_remote::webdav::{Vendor, WebDavConfig, WebDavStore};
use omy_remote::{Capabilities, PlaceStore, RemoteStore};

/// 一个已注册的远程位置。
pub struct Place {
    /// 进程内标识，前端用它指代。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 驱动类型，与 [`PlaceStore::kind`] 一致，也是配置里存的那个字符串。
    pub kind: String,
    /// 这个位置用的代理地址（目前只有 Telegram 用）。
    ///
    /// 存在 `Place` 上而不是只存在于那次连接调用里：重启后要靠它重连，
    /// 而本机直连 Telegram 数据中心是超时的。
    pub proxy: Option<String>,
    /// Telegram 账号的服务端 user id（仅 Telegram 有，其余为 `None`）。
    ///
    /// 登录去重的判据：昵称会重会改，只有它在服务端唯一。从配置恢复出来的
    /// 未连接占位也带着它，重启后不必重连就能判重。
    pub user_id: Option<i64>,
    /// 驱动实例。
    ///
    /// 类型是 [`PlaceStore`] 而不是某个具体驱动：这里写死成 `Arc<WebDavStore>`
    /// 的话，整个 GUI 层就单态化到了 WebDAV，加第二个 provider 只能另拉一条
    /// 平行的命令链路——那正是 AGENTS.md「同一逻辑不允许两处实现」要拦的。
    /// **新增 provider 时要改的是 `PlaceStore` 那个枚举，不是这里。**
    pub store: Arc<PlaceStore>,
}

/// 下发给前端的位置信息。
///
/// 刻意不含 url / 用户名 / 密码：前端不需要它们，而带出去只是多一处
/// 泄露面。要编辑时由后端按 id 回填。
#[derive(Debug, Clone, serde::Serialize)]
pub struct PlaceInfo {
    /// 进程内标识。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 驱动类型。
    pub kind: String,
    /// 能力位图。前端据此决定哪些操作出现。
    pub caps: Capabilities,
    /// Telegram 位置是否已用 omy 密码加密（侧栏据此显示锁标识）。
    ///
    /// 只对 Telegram 位置有意义、其余为 `None`（序列化为缺省字段）。读的是
    /// 本地 session 文件头，不连网；读不到按「未加密」处理，不挡列表。放在
    /// 列表里一次性给出，免得前端对每个位置各发一次异步查询（侧栏首帧就要
    /// 显示锁，逐个查会先无锁、再跳一下）。
    #[serde(skip_serializing_if = "Option::is_none", rename = "encrypted")]
    pub tg_encrypted: Option<bool>,
}

/// 远程位置注册表。
#[derive(Default)]
pub struct PlaceRegistry {
    places: Mutex<HashMap<String, Arc<Place>>>,
    /// 注册顺序，用于让列表顺序稳定。
    ///
    /// HashMap 的遍历顺序每次都不同，直接用它会让侧栏的位置顺序乱跳。
    order: Mutex<Vec<String>>,
    /// 下一个位置 id 的序号。**只增不减。**
    ///
    /// 不用 `order.len() + 1` 算：那在「加两个 → 删第一个 → 再加一个」之后
    /// 会重新发出一个已经用过的 id。而 session 文件按位置 id 命名，
    /// 重发意味着新账号会读到（或覆盖掉）上一个账号的登录态——
    /// 现象是「加了个新账号，点进去却是刚移除那个人的对话」，
    /// 或者「加了新账号，另一个账号莫名其妙要重新登录」，两边都不报错。
    ///
    /// 做成计数器而不是「扫一遍看哪个没被占用」，是因为「没被占用」
    /// 依赖当下的内存与磁盘状态，而那两者都可能刚好为空；
    /// 单调递增则不依赖任何外部事实。
    next_seq: Mutex<usize>,
}

impl PlaceRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 分配一个**从未被用过**的位置 id。
    ///
    /// # 为什么不能用 `format!("p{}", len + 1)`
    ///
    /// 那种写法在「加两个 → 删第一个 → 再加一个」之后会重新发出 `p2`：
    /// 删完只剩一个，`len + 1` 又算回 2。撞 id 在多账号下是**致命**的——
    /// session 文件按位置 id 命名，两个位置拿到同一个 id 就会写同一份
    /// session，后来的那个把前一个的 auth key 覆盖掉，于是「加了新账号，
    /// 旧账号却登出了」；而侧栏上两个位置都好端端列着，不会有任何报错。
    ///
    /// # 为什么还要额外避开残留的 session 文件
    ///
    /// 「从列表移除」会**保留 session 文件**（那正是它与「删除账号」的区别），
    /// 磁盘上于是留下一个不属于任何位置的 `telegram-session-pN.json`。
    /// 计数器在同一次运行内不会退回去，但**重启后**它是从配置里的 id 推出来的，
    /// 而那个被移除的位置已经不在配置里了——于是新账号可能拿到那个 N，
    /// 然后读到上一个账号的登录态。所以两道都要有：计数器管进程内，
    /// 文件检查管跨重启。
    fn next_id(&self, taken: &HashMap<String, Arc<Place>>) -> String {
        let Ok(mut seq) = self.next_seq.lock() else {
            // 锁中毒时退回一个几乎不可能撞的名字，而不是硬塞一个可能重复的
            // ——宁可 id 难看，也不能让两个账号共用一份 session
            return format!("p{}", taken.len().saturating_add(1_000_000));
        };
        loop {
            *seq = seq.saturating_add(1);
            let candidate = format!("p{seq}");
            if taken.contains_key(&candidate) {
                continue;
            }
            // 残留的 session 文件同样算占用，理由见上
            let has_session = omy_remote::telegram::session::session_path_of(&candidate)
                .map(|p| p.exists())
                .unwrap_or(false);
            if !has_session {
                return candidate;
            }
        }
    }

    /// 把序号推到至少 `n`，保证之后发出的 id 都大于它。
    ///
    /// 恢复配置时用：配置里已经有 `p7` 的话，这次运行必须从 8 开始发，
    /// 否则新位置会撞上一个已经存在、且可能还带着 session 文件的 id。
    fn bump_seq_to(&self, n: usize) {
        if let Ok(mut seq) = self.next_seq.lock() {
            if *seq < n {
                *seq = n;
            }
        }
    }

    /// 从一个位置 id 里取出序号（`p7` → 7）。认不出就返回 `None`。
    fn seq_of(id: &str) -> Option<usize> {
        id.strip_prefix('p').and_then(|n| n.parse().ok())
    }

    /// 添加一个 WebDAV 位置，返回其 id。
    ///
    /// # Errors
    ///
    /// URL 非法或客户端构造失败时返回。
    pub fn add_webdav(&self, name: String, cfg: WebDavConfig) -> omy_remote::Result<String> {
        let store = Arc::new(PlaceStore::from(WebDavStore::new(cfg)?));
        // id 用递增序号而非名字：名字可以重复，也可以带斜杠之类
        // 会破坏后续拼接的字符
        let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) else {
            return Err(omy_remote::Error::Protocol(String::from("注册表锁失效")));
        };
        let id = self.next_id(&m);
        let place = Arc::new(Place {
            id: id.clone(),
            name,
            // 由驱动自己报类型，不在这里写字面量：两处各写一份迟早对不上，
            // 而对不上的后果是配置存进去读回来变成另一种驱动
            kind: String::from(store.kind()),
            proxy: None,
            user_id: None, // WebDAV 没有账号 user id
            store,
        });
        m.insert(id.clone(), place);
        o.push(id.clone());
        Ok(id)
    }

    /// 添加一个 Telegram 位置，返回其 id。
    ///
    /// `store` 由调用方建好（它需要一个已连接的客户端，而建连要走网络、
    /// 属于命令层的事）。注册表只管登记，不碰网络——把建连放进来会让
    /// 「添加一个位置」这个同步操作变成可能卡几十秒的操作。
    ///
    /// **可以调用多次**：每次都是一个独立账号，各自拿到自己的 id，
    /// 也就各自对应一份自己的 session 文件。
    ///
    /// # Errors
    ///
    /// 注册表锁失效时返回。
    pub fn add_telegram(
        &self,
        name: String,
        store: TelegramStore,
        proxy: Option<String>,
        user_id: Option<i64>,
    ) -> omy_remote::Result<String> {
        let store = Arc::new(PlaceStore::from(store));
        let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) else {
            return Err(omy_remote::Error::Protocol(String::from("注册表锁失效")));
        };
        let id = self.next_id(&m);
        let place = Arc::new(Place {
            id: id.clone(),
            name,
            kind: String::from(store.kind()),
            proxy,
            user_id,
            store,
        });
        m.insert(id.clone(), place);
        o.push(id.clone());
        Ok(id)
    }

    /// 用**指定的 id** 添加一个 Telegram 位置。
    ///
    /// 给「把未连接的占位换成真连接」用：必须保住原来那个 id，
    /// 否则前端手里的 id 会失效，表现是刚点进去就报「找不到该远程位置」——
    /// 而位置明明就在那儿。
    pub fn add_telegram_with_id(
        &self,
        id: String,
        name: String,
        store: TelegramStore,
        proxy: Option<String>,
        user_id: Option<i64>,
    ) {
        let store = Arc::new(PlaceStore::from(store));
        let place = Arc::new(Place {
            id: id.clone(),
            name,
            kind: String::from(store.kind()),
            proxy,
            user_id,
            store,
        });
        if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
            if m.insert(id.clone(), place).is_none() {
                o.push(id);
            }
        }
    }

    /// 已注册的**全部** Telegram 位置 id，按注册顺序。
    ///
    /// # 为什么是复数
    ///
    /// 早先这里是 `telegram_id()`，注释写着「只会有一个：一个 omy 同时只持有
    /// 一份 Telegram 登录态」。那个前提**已经不成立**——用户要能加任意多个
    /// 账号。而单例版本正是「点添加也显示已登录」的根因：添加流程先查有没有
    /// 现成的，一查到就直接复用，于是第二个账号根本没有机会登录。
    ///
    /// 想判断「这个账号是不是已经加过了」不能再靠「有没有 Telegram 位置」，
    /// 要按账号自己的标识去比。
    #[must_use]
    pub fn telegram_ids(&self) -> Vec<String> {
        let (Ok(m), Ok(o)) = (self.places.lock(), self.order.lock()) else {
            return Vec::new();
        };
        o.iter()
            .filter_map(|id| m.get(id))
            .filter(|p| p.kind == "telegram")
            .map(|p| p.id.clone())
            .collect()
    }

    /// 改一个位置的显示名。
    ///
    /// **只动本地显示名，不碰服务端**：Telegram 上的昵称是账号自己的属性，
    /// 用户在 omy 里给位置起的名字只是本地标签。真去改服务端 profile
    /// 完全超出「给这个位置起个名」的预期，属于越权。
    ///
    /// 返回是否真的改到了（位置不存在时为 `false`）。
    pub fn rename(&self, id: &str, name: String) -> bool {
        let Ok(mut m) = self.places.lock() else {
            return false;
        };
        let Some(old) = m.get(id) else {
            return false;
        };
        // Place 里其余字段照搬。这里不能用 Arc::make_mut：store 是
        // Arc<PlaceStore> 且没有 Clone，硬拆会把已建立的连接弄丢——
        // 表现是改个名字之后位置突然「未连接」
        let replaced = Arc::new(Place {
            id: old.id.clone(),
            name,
            kind: old.kind.clone(),
            proxy: old.proxy.clone(),
            user_id: old.user_id, // 改名不动账号身份
            store: Arc::clone(&old.store),
        });
        m.insert(String::from(id), replaced);
        true
    }


    /// 按 id 取位置。
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<Place>> {
        self.places.lock().ok()?.get(id).cloned()
    }

    /// 找一个 user id 相同的**已有** Telegram 位置，返回它的 id。
    ///
    /// 登录去重的查询点：判据是 user id 而非昵称——昵称会重、会改，只有
    /// user id 在服务端唯一。带 user id 的占位（从配置恢复的）也算数，
    /// 所以重启后不必先连上就能判重。
    ///
    /// `exclude` 用来排除某个位置：命中后要把新登录并进已有位置，扫码路径
    /// 会先把占位建出来（PENDING → placeConnect），查重时要把那个刚建的
    /// 占位本身排除掉，否则会「自己命中自己」。
    #[must_use]
    pub fn find_telegram_by_user(&self, user_id: i64, exclude: Option<&str>) -> Option<String> {
        let (m, o) = (self.places.lock().ok()?, self.order.lock().ok()?);
        // 按注册顺序找，命中最早那个：稳定、可预期
        o.iter()
            .filter(|id| exclude != Some(id.as_str()))
            .filter_map(|id| m.get(id))
            .find(|p| p.kind == "telegram" && p.user_id == Some(user_id))
            .map(|p| p.id.clone())
    }

    /// 按**稳定标识**在当前已注册的真实位置里认领出对应的 `Place`。
    ///
    /// 这是虚拟远程「引用委托」的关键一步：引用记的是稳定标识（Telegram user_id /
    /// WebDAV url+账号），而不是会变的本地 place_id；打开引用时用这个把它解析回
    /// 当前的本地位置，再委托它读——**源被移除再加回后本地 id 变了也能重新认领**。
    ///
    /// Telegram 认 user_id，WebDAV 认 (url, username)。认不到（源没加、或还没连上
    /// 拿到 user_id）返回 `None`，调用方据此显示「源不可用」而不是崩。
    #[must_use]
    pub fn resolve_source(&self, src: &crate::virtual_place::SourceRef) -> Option<Arc<Place>> {
        let (m, o) = (self.places.lock().ok()?, self.order.lock().ok()?);
        o.iter()
            .filter_map(|id| m.get(id))
            .find(|p| Self::place_source(p).is_some_and(|ps| ps.matches(src)))
            .cloned()
    }

    /// 从一个真实位置导出它的稳定标识（[`SourceRef`]），供 `resolve_source` 与
    /// 前端「添加到虚拟远程」构造引用时共用同一份口径——不让「怎么算同一个源」
    /// 散在两处。取不到（缺 user_id / 非 telegram、webdav）返回 `None`。
    #[must_use]
    pub fn place_source(p: &Place) -> Option<crate::virtual_place::SourceRef> {
        match p.kind.as_str() {
            "telegram" => p.user_id.map(crate::virtual_place::SourceRef::telegram),
            "webdav" => p.store.as_webdav().map(|w| {
                let c = w.config();
                crate::virtual_place::SourceRef::webdav(c.base_url.clone(), c.username.clone())
            }),
            _ => None,
        }
    }

    /// 把一个已有 Telegram 位置的连接与 user id 就地换新。
    ///
    /// 登录去重命中已有账号时用：这次登录产生的是更新鲜的登录态，用它替换
    /// 已有位置的 store，比留着旧连接合理。名字与代理沿用已有的——用户之前
    /// 给这个账号起的名字不该被一次重复登录冲掉。
    ///
    /// 返回是否换成功（位置不存在或不是 Telegram 时为 `false`）。
    pub fn update_telegram_connection(
        &self,
        id: &str,
        store: TelegramStore,
        user_id: Option<i64>,
    ) -> bool {
        let store = Arc::new(PlaceStore::from(store));
        let Ok(mut m) = self.places.lock() else {
            return false;
        };
        let Some(old) = m.get(id) else {
            return false;
        };
        if old.kind != "telegram" {
            return false;
        }
        let replaced = Arc::new(Place {
            id: old.id.clone(),
            name: old.name.clone(),
            kind: old.kind.clone(),
            proxy: old.proxy.clone(),
            // user id 用新拿到的；正常与旧的相同（因为是靠它命中的），
            // 但占位此前可能没有 user id，这里正好补上
            user_id: user_id.or(old.user_id),
            store,
        });
        m.insert(String::from(id), replaced);
        true
    }

    /// 移除一个位置。
    pub fn remove(&self, id: &str) {
        if let Ok(mut m) = self.places.lock() {
            m.remove(id);
        }
        if let Ok(mut o) = self.order.lock() {
            o.retain(|x| x != id);
        }
    }

    /// 已有 Telegram 位置用的代理地址（任取一个）。
    ///
    /// 新建登录拿它当默认值：用户已经用这个地址连通过一次，比系统代理更
    /// 可信。没有 Telegram 位置或那个位置没配代理时返回 `None`。
    #[must_use]
    pub fn telegram_proxy(&self) -> Option<String> {
        let m = self.places.lock().ok()?;
        m.values()
            .find(|p| p.kind == "telegram" && p.proxy.is_some())
            .and_then(|p| p.proxy.clone())
    }

    /// 列出全部位置，顺序稳定。
    #[must_use]
    pub fn list(&self) -> Vec<PlaceInfo> {
        let (Ok(m), Ok(o)) = (self.places.lock(), self.order.lock()) else {
            return Vec::new();
        };
        o.iter()
            .filter_map(|id| m.get(id))
            .map(|p| PlaceInfo {
                id: p.id.clone(),
                name: p.name.clone(),
                kind: p.kind.clone(),
                caps: p.store.capabilities(),
                tg_encrypted: if p.kind == "telegram" {
                    crate::telegram_cmds::telegram_session_encrypted(&p.id)
                } else {
                    None
                },
            })
            .collect()
    }
}

/// 从配置里的字符串解析厂商。
///
/// 未知值回落到 `Generic` 而不是报错：配置可能是更高版本写的，
/// 为一个厂商名让整个位置用不了并不值得。
#[must_use]
pub fn parse_vendor(s: &str) -> Vendor {
    match s {
        "nextcloud" | "owncloud" => Vendor::Nextcloud,
        _ => Vendor::Generic,
    }
}

/// 厂商转回配置里的字符串。
///
/// 与 [`parse_vendor`] 是一对，**改一处必须改两处**，
/// 否则存进去的值读回来会变成 `Generic`。
#[must_use]
pub fn vendor_str(v: Vendor) -> &'static str {
    match v {
        Vendor::Nextcloud => "nextcloud",
        Vendor::Generic => "generic",
    }
}

/// 凭据在本机凭据库里的服务名。
const SECRET_SERVICE: &str = "omy-remote-places";

/// 所有远程位置共用的那一把密钥的 id。
///
/// 只用一把：每个位置一条钥匙串记录的话，删位置时漏清就会在用户的
/// 钥匙串里堆垃圾，而 Linux 的 Secret Service 对条目数也不友好。
const SECRET_KEY_ID: &str = "places-key-v1";

/// 持久化状态：这台机器上凭据能不能保护。
///
/// 界面需要区分这三种，因为给用户的话完全不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStatus {
    /// 凭据库可用，密码已加密保存。
    Protected,
    /// 没有可用的凭据库（典型：Linux 无 Secret Service）。
    ///
    /// 此时位置**仍会保存**，但密码不保存——下次要重新输入。
    /// 绝不退回明文存储。
    Unavailable,
}

impl PlaceRegistry {
    /// 取本机的保护密钥。
    ///
    /// 每次现取而不缓存：钥匙串可能中途被锁上，缓存会让我们用一把
    /// 已经无权使用的密钥，错误也就推迟到更难解释的地方才出现。
    fn protect_key() -> Option<omy_secret::ProtectKey> {
        let p = omy_secret::default_protector(SECRET_SERVICE).ok()?;
        p.retrieve_or_create(SECRET_KEY_ID).ok()
    }

    /// 本机凭据保护是否可用。
    #[must_use]
    pub fn secret_status() -> SecretStatus {
        if Self::protect_key().is_some() {
            SecretStatus::Protected
        } else {
            SecretStatus::Unavailable
        }
    }

    /// 把当前所有位置写进配置。
    ///
    /// 密码经 `omy-secret` 加密；拿不到保护密钥时**丢掉密码**而不是
    /// 明文写入——用户下次需要重新登录，但配置文件里不会有裸密码。
    ///
    /// # Errors
    ///
    /// 配置写盘失败时返回。
    pub fn persist(&self) -> Result<(), String> {
        let key = Self::protect_key();
        let saved = {
            let (Ok(m), Ok(o)) = (self.places.lock(), self.order.lock()) else {
                return Err(String::from("注册表锁失效"));
            };
            Self::assemble_saved_places(&m, &o, key.as_ref())
        };

        let mut cfg = omy_config::Config::load().map_err(|e| e.to_string())?;
        cfg.remote.places = saved;
        cfg.save().map_err(|e| e.to_string())
    }

    /// 把注册表里的全部位置组装成可持久化的记录。**纯逻辑，不碰磁盘。**
    ///
    /// # 为什么要从 persist 里抽出来
    ///
    /// `persist` 还要读写真实配置文件，测试碰不到它。于是「每一类位置都要被
    /// 存下来」这条只能靠测试重抄一遍转换来验，而抄出来的那份**改坏产品代码
    /// 也不会失败**——变异测试确认过：把 persist 里拼 Telegram 那一句换成
    /// `Vec::new()`，重抄版断言照样通过，而实际后果是所有 Telegram 位置
    /// 重启后凭空消失。
    ///
    /// 抽成纯函数之后，测试断言的就是产品真正用来组装的那条路径。
    fn assemble_saved_places(
        m: &HashMap<String, Arc<Place>>,
        o: &[String],
        key: Option<&omy_secret::ProtectKey>,
    ) -> Vec<omy_config::SavedPlace> {
        let mut saved: Vec<omy_config::SavedPlace> = Self::telegram_saved_places(m, o);

        // Telegram 那一段收在 telegram_saved_places 里，见其文档。
        // 下面是 WebDAV 那条链路。
        //
        // 原本这里写的是：没有 url / username / 密码可存——凭据在单独加密的
        // session 文件里（**按位置 id 一个账号一个文件**），这里只记
        // 「有这么一个位置、它叫什么」。
        //
        // 不能沿用下面 WebDAV 那条链路：它靠 as_webdav() 过滤，
        // Telegram 会被静默丢掉，表现是「加过的位置重启就没了」而且不报错。
        //
        // 这里**遍历全部** Telegram 位置而不是只存一个：早先的实现基于
        // 「只会有一个 Telegram」那个前提，多账号之后必须逐个存，
        // 否则重启后只剩一个账号，其余的连同它们的名字一起消失
        // （session 文件还在，但没有位置引用它们，等于登录态被孤立）。
        let webdav_saved: Vec<omy_config::SavedPlace> = o
            .iter()
            .filter_map(|id| m.get(id))
            // 其余非 WebDAV 的位置在这里被跳过，而不是存成一条半截记录：
            // `SavedPlace` 的 url / username / vendor 都是 WebDAV 专有字段，
            // 用空串凑出来的记录下次恢复会造出一个连不上的位置。
            // **新增 provider 时要在这里补一支**，而不是任它静默消失。
            .filter_map(|p| p.store.as_webdav().map(|w| (Arc::clone(p), w.config())))
            .map(|(p, c)| {
                // 没有密码就不必造信封；有密码但没有保护密钥时也不存，
                // 两种情况在配置里都表现为 secret 缺失，界面提示重新登录
                let secret = if c.password.is_empty() {
                    None
                } else {
                    key.and_then(|k| omy_secret::seal(k, c.password.as_bytes()).ok())
                        .and_then(|env| toml::Value::try_from(env).ok())
                };
                omy_config::SavedPlace {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    url: c.base_url.clone(),
                    username: c.username.clone(),
                    vendor: String::from(vendor_str(c.vendor)),
                    writable: c.writable,
                    secret,
                    user_id: None, // WebDAV 无账号 user id
                }
            })
            .collect();
        saved.extend(webdav_saved);
        saved
    }

    /// 把**全部** Telegram 位置转成可持久化的记录。
    ///
    /// # 为什么单独一个函数
    ///
    /// `persist` 要读真实配置文件，测试碰不到它——于是「多个账号都要存下来」
    /// 这条只能在测试里把转换重抄一遍，而抄出来的那份**改坏产品代码也不会
    /// 失败**。变异测试确认过：给循环加一个 `.take(1)`，重抄版断言照样通过。
    ///
    /// 抽出来之后测试直接调它，产品与测试走同一份代码。
    ///
    /// # 为什么不能沿用 WebDAV 那条链路
    ///
    /// 那条靠 `as_webdav()` 过滤，Telegram 会被静默丢掉，
    /// 表现是「加过的位置重启就没了」而且不报错。
    fn telegram_saved_places(
        m: &HashMap<String, Arc<Place>>,
        o: &[String],
    ) -> Vec<omy_config::SavedPlace> {
        o.iter()
            .filter_map(|id| m.get(id))
            .filter(|p| p.kind == "telegram")
            .map(|p| omy_config::SavedPlace {
                id: p.id.clone(),
                // 用户给这个账号起的名字。存它是多账号的必需项——两个
                // Telegram 位置只能靠名字区分，不存的话重启后会变成两个都叫
                // 「Telegram」，用户无从分辨哪个是哪个
                name: p.name.clone(),
                kind: p.kind.clone(),
                // 复用 url 字段存代理地址。
                //
                // 不新增字段：这个字段对 Telegram 本来就空着，而代理**不是
                // 凭据**（它是本机地址，不涉及账号），放明文没有问题。
                //
                // 必须存：本机直连 Telegram 数据中心是超时的，没有代理就连不上。
                // 不存的话重启后自动连必然超时，而超时要等很久，
                // 用户只看到界面卡住、看不出和代理有关。
                url: p.proxy.clone().unwrap_or_default(),
                username: String::new(),
                vendor: String::new(),
                // 能不能写由对话决定（effective_capabilities），位置级这一位
                // 对 Telegram 没有意义，存 true 只是别把整个位置钉死成只读
                writable: true,
                // 凭据不在这里：Telegram 的登录态由
                // omy_remote::telegram::session 按位置 id 单独加密落盘
                secret: None,
                // 账号 user id：登录去重的判据，公开数字 id、可明文存
                user_id: p.user_id,
            })
            .collect()
    }

    /// 从配置恢复已保存的位置。
    ///
    /// 解不开密码的位置**照样恢复**，只是没有密码：用户会看到这个位置
    /// 还在、标着需要重新登录，而不是以为自己的配置丢了。
    ///
    /// 返回成功恢复的位置数与其中缺密码的个数。
    pub fn restore(&self, cfg: &omy_config::Remote) -> (usize, usize) {
        // 先把序号推过配置里所有已存在的 id。
        //
        // 不推的话，这次运行发出的第一个 id 会从 1 开始，直接撞上恢复出来的
        // p1——而 p1 的 session 文件就在磁盘上，新账号会读到别人的登录态。
        for sp in &cfg.places {
            if let Some(n) = Self::seq_of(&sp.id) {
                self.bump_seq_to(n);
            }
        }
        let key = Self::protect_key();
        if key.is_none() {
            eprintln!("[omy] 恢复远程位置：取不到本机保护密钥，所有密码都将为空");
        }
        let mut total = 0usize;
        let mut need_login = 0usize;

        for sp in &cfg.places {
            // Telegram 恢复成一个**未连接**的占位。
            //
            // 不在这里建连：建连要走网络，可能超时几十秒，而 restore 跑在
            // 应用启动路径上——放这儿会让整个界面卡在启动那一刻，
            // 而用户看不出是在等网络。真正的连接推迟到用户点开它时。
            if sp.kind == "telegram" {
                let place = Arc::new(Place {
                    id: sp.id.clone(),
                    name: sp.name.clone(),
                    kind: sp.kind.clone(),
                    // 代理存在 url 字段里（见 persist 处的说明）
                    proxy: Some(sp.url.clone()).filter(|s| !s.is_empty()),
                    // 从配置带回 user id：老配置没有这个字段时为 None（见
                    // SavedPlace.user_id 的 serde default），重启后判重就少了
                    // 这一个账号，等它下次连接再补上
                    user_id: sp.user_id,
                    store: Arc::new(PlaceStore::from(TelegramStore::new())),
                });
                if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
                    if m.insert(sp.id.clone(), place).is_none() {
                        o.push(sp.id.clone());
                    }
                    total += 1;
                }
                continue;
            }

            let password = sp
                .secret
                .as_ref()
                .and_then(|v| match v.clone().try_into::<omy_secret::Envelope>() {
                    Ok(env) => Some(env),
                    Err(e) => {
                        eprintln!("[omy] 位置 {} 的凭据信封无法解析：{e}", sp.name);
                        None
                    }
                })
                .and_then(|env| {
                    key.as_ref().and_then(|k| match omy_secret::unseal(k, &env) {
                        Ok(pt) => Some(pt),
                        Err(e) => {
                            eprintln!("[omy] 位置 {} 的凭据解密失败：{e}", sp.name);
                            None
                        }
                    })
                })
                .and_then(|pt| String::from_utf8(pt.to_vec()).ok())
                .unwrap_or_default();

            // 有密文却解不开（换了机器、清了钥匙串）要单独计数：
            // 这与「本来就是匿名位置」完全不同，界面给的提示也不同
            if sp.secret.is_some() && password.is_empty() {
                need_login += 1;
            }

            let wcfg = WebDavConfig {
                base_url: sp.url.clone(),
                username: sp.username.clone(),
                password,
                writable: sp.writable,
                vendor: parse_vendor(&sp.vendor),
                ..WebDavConfig::default()
            };
            let Ok(store) = WebDavStore::new(wcfg) else {
                // URL 坏了就跳过这一条，不要让整个恢复流程失败——
                // 其余位置还是好的
                continue;
            };
            let place = Arc::new(Place {
                id: sp.id.clone(),
                name: sp.name.clone(),
                kind: sp.kind.clone(),
                proxy: None,
                user_id: None, // WebDAV 无账号 user id
                store: Arc::new(PlaceStore::from(store)),
            });
            if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
                // 恢复时保留原 id：前端可能存了「上次打开的位置」
                if m.insert(sp.id.clone(), place).is_none() {
                    o.push(sp.id.clone());
                }
                total += 1;
            }
        }
        (total, need_login)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(writable: bool) -> WebDavConfig {
        WebDavConfig {
            base_url: String::from("https://dav.example.com/dav"),
            username: String::from("u"),
            password: String::from("secret-pw"),
            writable,
            ..WebDavConfig::default()
        }
    }

    /// 下发给前端的信息里不能带凭据。
    ///
    /// 不这样会怎样：密码进了 WebView，任何一个 XSS 或调试面板就能拿到
    /// 用户的网盘账号——而前端根本不需要它。
    #[test]
    fn place_info_hides_credentials() {
        let r = PlaceRegistry::new();
        let id = r.add_webdav(String::from("NAS"), cfg(true)).expect("添加");
        let list = r.list();
        let j = serde_json::to_string(&list).expect("序列化");
        assert!(!j.contains("secret-pw"), "不能带出密码");
        assert!(!j.contains("dav.example.com"), "连 URL 也不必给前端");
        assert!(j.contains(&id));
        assert!(j.contains("NAS"));
    }

    /// 能力位图要跟着实际配置走。
    ///
    /// 不这样会怎样：只读位置被前端当成可写，用户点了删除才失败。
    #[test]
    fn caps_follow_config() {
        let r = PlaceRegistry::new();
        let ro = r.add_webdav(String::from("只读"), cfg(false)).expect("添加");
        let rw = r.add_webdav(String::from("可写"), cfg(true)).expect("添加");
        let list = r.list();
        let get = |id: &str| list.iter().find(|p| p.id == id).expect("应存在").caps;
        assert!(!get(&ro).any_write(), "只读位置不能有写能力");
        assert!(get(&rw).any_write());
        assert!(!get(&rw).random_write, "WebDAV 没有部分写");
    }

    /// 列表顺序必须稳定。
    ///
    /// 不这样会怎样：HashMap 的遍历顺序每次都不同，侧栏里的位置会
    /// 每次刷新都换位置，用户点错的概率显著上升。
    #[test]
    fn list_order_is_stable() {
        let r = PlaceRegistry::new();
        for n in ["a", "b", "c", "d"] {
            r.add_webdav(String::from(n), cfg(false)).expect("添加");
        }
        let first: Vec<String> = r.list().into_iter().map(|p| p.name).collect();
        assert_eq!(first, vec!["a", "b", "c", "d"]);
        for _ in 0..5 {
            let again: Vec<String> = r.list().into_iter().map(|p| p.name).collect();
            assert_eq!(first, again, "多次列举顺序必须一致");
        }
    }

    /// 移除后不能再取到，且顺序表也要清干净。
    ///
    /// 不这样会怎样：order 里留下孤儿 id，list 会静默跳过它，
    /// 但每次注册新位置时序号还在增长，行为逐渐难以理解。
    #[test]
    fn remove_cleans_both_maps() {
        let r = PlaceRegistry::new();
        let a = r.add_webdav(String::from("a"), cfg(false)).expect("添加");
        let b = r.add_webdav(String::from("b"), cfg(false)).expect("添加");
        r.remove(&a);
        assert!(r.get(&a).is_none());
        assert!(r.get(&b).is_some());
        let names: Vec<String> = r.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["b"]);
    }

    /// 未知厂商名回落到 Generic，而不是让位置用不了。
    #[test]
    fn unknown_vendor_falls_back() {
        assert_eq!(parse_vendor("nextcloud"), Vendor::Nextcloud);
        assert_eq!(parse_vendor("owncloud"), Vendor::Nextcloud);
        assert_eq!(parse_vendor("generic"), Vendor::Generic);
        assert_eq!(parse_vendor("某个未来才有的厂商"), Vendor::Generic);
    }

    /// 厂商的写出与读回必须互为逆运算。
    ///
    /// 不这样会怎样：存 Nextcloud 读回 Generic，针对该厂商的兼容处理
    /// 静默失效——位置还能用，只是某些请求方式退回通用路径，很难察觉。
    #[test]
    fn vendor_roundtrips() {
        for v in [Vendor::Nextcloud, Vendor::Generic] {
            assert_eq!(parse_vendor(vendor_str(v)), v, "{v:?} 往返后变了");
        }
    }

    /// 持久化再恢复，位置的各字段与密码都要回来。
    ///
    /// 环境没有可用凭据库时（CI 容器常见）跳过密码断言，但仍验证
    /// 其余字段——因为那正是「没有密钥也要能列出位置」的要求。
    #[test]
    fn saved_place_roundtrips_through_config() {
        let r = PlaceRegistry::new();
        r.add_webdav(String::from("我的NAS"), cfg(true)).expect("添加");

        // 不碰真实配置文件：手工走一遍 persist 用的那套转换
        let key = PlaceRegistry::protect_key();
        let saved: Vec<omy_config::SavedPlace> = r
            .list()
            .iter()
            .filter_map(|info| r.get(&info.id))
            .map(|p| {
                let c = p.store.as_webdav().expect("夹具里都是 WebDAV").config();
                let secret = key
                    .as_ref()
                    .and_then(|k| omy_secret::seal(k, c.password.as_bytes()).ok())
                    .and_then(|env| toml::Value::try_from(env).ok());
                omy_config::SavedPlace {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    url: c.base_url.clone(),
                    username: c.username.clone(),
                    vendor: String::from(vendor_str(c.vendor)),
                    writable: c.writable,
                    secret,
                    user_id: None,
                }
            })
            .collect();

        let remote = omy_config::Remote {
            places: saved,
            ..omy_config::Remote::default()
        };
        let restored = PlaceRegistry::new();
        let (n, _) = restored.restore(&remote);
        assert_eq!(n, 1, "应恢复一个位置");

        let got = restored.list();
        let first = got.first().expect("应有一个");
        assert_eq!(first.name, "我的NAS");
        assert!(first.caps.any_write(), "可写标志必须跟着恢复");

        let place = restored.get(&first.id).expect("取回");
        let c = place.store.as_webdav().expect("应恢复成 WebDAV 驱动").config();
        assert_eq!(c.base_url, "https://dav.example.com/dav");
        assert_eq!(c.username, "u");
        if key.is_some() {
            assert_eq!(c.password, "secret-pw", "密码必须能解回来");
        } else {
            eprintln!("跳过密码断言：当前环境没有可用的凭据后端");
        }
    }

    /// 解不开密码时，位置照样要恢复出来，只是没有密码。
    ///
    /// 不这样会怎样：用户换了机器或清了钥匙串，打开应用发现远程位置
    /// 全空了，会以为配置损坏——而实际上只需要重新输一次密码。
    #[test]
    fn undecryptable_secret_still_restores_place() {
        let remote = omy_config::Remote {
            places: vec![omy_config::SavedPlace {
                id: String::from("p1"),
                name: String::from("换过机器的NAS"),
                kind: String::from("webdav"),
                url: String::from("https://dav.example.com/dav"),
                username: String::from("u"),
                vendor: String::from("generic"),
                writable: false,
                // 一个解不开的信封：字段合法但密钥对不上
                secret: toml::Value::try_from(omy_secret::Envelope {
                    v: 1,
                    n: String::from("000102030405060708090a0b"),
                    c: String::from("deadbeef"),
                })
                .ok(),
                user_id: None,
            }],
            ..omy_config::Remote::default()
        };

        let r = PlaceRegistry::new();
        let (n, need_login) = r.restore(&remote);
        assert_eq!(n, 1, "位置必须恢复出来");
        assert_eq!(need_login, 1, "必须被计为需要重新登录");
        let place = r.get("p1").expect("应存在");
        let c = place.store.as_webdav().expect("应是 WebDAV 驱动").config();
        assert!(c.password.is_empty(), "密码应为空");
        assert_eq!(c.base_url, "https://dav.example.com/dav");
    }

    /// URL 损坏的条目跳过，不能让整个恢复流程失败。
    ///
    /// 不这样会怎样：配置里一条坏记录会让所有远程位置都消失。
    #[test]
    fn broken_entry_does_not_abort_restore() {
        let mk = |id: &str, url: &str| omy_config::SavedPlace {
            id: String::from(id),
            name: String::from(id),
            kind: String::from("webdav"),
            url: String::from(url),
            username: String::new(),
            vendor: String::from("generic"),
            writable: false,
            secret: None,
            user_id: None,
        };
        let remote = omy_config::Remote {
            places: vec![
                mk("bad", "not a url at all"),
                mk("good", "https://dav.example.com/dav"),
            ],
            ..omy_config::Remote::default()
        };
        let r = PlaceRegistry::new();
        let (n, _) = r.restore(&remote);
        assert_eq!(n, 1, "坏的跳过，好的要恢复");
        assert!(r.get("good").is_some());
    }

    /// 序列化成 TOML 后，配置里绝不能出现明文密码。
    ///
    /// 不这样会怎样：这是整个 omy-secret 存在的唯一理由。哪怕只是某次
    /// 重构把 password 字段也加进了 SavedPlace，密码就明文躺在磁盘上，
    /// 而功能表现上毫无异常——没有这条断言根本发现不了。
    #[test]
    fn persisted_config_has_no_plaintext_password() {
        let Some(key) = PlaceRegistry::protect_key() else {
            eprintln!("跳过：当前环境没有可用的凭据后端");
            return;
        };
        let env = omy_secret::seal(&key, b"secret-pw").expect("加密");
        let sp = omy_config::SavedPlace {
            id: String::from("p1"),
            name: String::from("NAS"),
            kind: String::from("webdav"),
            url: String::from("https://dav.example.com/dav"),
            username: String::from("u"),
            vendor: String::from("generic"),
            writable: true,
            secret: toml::Value::try_from(env).ok(),
            user_id: None,
        };
        let remote = omy_config::Remote {
            places: vec![sp],
            ..omy_config::Remote::default()
        };
        let text = toml::to_string(&remote).expect("序列化");
        assert!(
            !text.contains("secret-pw"),
            "配置里出现了明文密码：\n{text}"
        );
    }

    /// 造一个 Telegram 位置（未连接占位，够用来验注册表逻辑）。
    fn add_tg(r: &PlaceRegistry, name: &str) -> String {
        r.add_telegram(String::from(name), TelegramStore::new(), None, None)
            .expect("添加 Telegram")
    }

    /// **两个 Telegram 位置能共存，且各自拿到不同的 id。**
    ///
    /// 不这样会怎样：这是用户报的那条「点添加也显示已登录」的根因。
    /// 旧实现认定「只会有一个 Telegram」，第二次添加会复用第一个，
    /// 于是第二个账号根本没机会登录。
    ///
    /// id 必须不同这一半尤其要紧：session 文件按位置 id 命名，
    /// id 相同就等于两个账号共用一份 session。
    #[test]
    fn two_telegram_places_coexist_with_distinct_ids() {
        let r = PlaceRegistry::new();
        let a = add_tg(&r, "工作号");
        let b = add_tg(&r, "私人号");

        assert_ne!(a, b, "两个 Telegram 位置必须拿到不同的 id");
        assert_eq!(r.telegram_ids(), vec![a.clone(), b.clone()], "两个都要在");
        // 名字也要各自保留，否则侧栏上两个都叫 Telegram，用户无从分辨
        let names: Vec<String> = r.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["工作号", "私人号"]);
    }

    /// **位置 id 绝不能被重复发放**，哪怕中间删过位置。
    ///
    /// 不这样会怎样：`format!("p{}", len + 1)` 在「加两个 → 删第一个 →
    /// 再加一个」之后会再发一次 `p2`。而 session 文件按位置 id 命名，
    /// 于是新账号会写进旧账号那份 session，把它的 auth key 覆盖掉——
    /// 现象是「加了新账号，另一个账号莫名其妙要重新登录」，
    /// 而侧栏上两个位置都好端端列着，没有任何报错。
    ///
    /// 这条断言直接比 id 本身，不是「有两个位置」——后者抓不到这个缺陷。
    #[test]
    fn place_ids_are_never_reused_after_removal() {
        let r = PlaceRegistry::new();
        let a = add_tg(&r, "一号");
        let b = add_tg(&r, "二号");
        r.remove(&a);
        let c = add_tg(&r, "三号");

        assert_ne!(c, b, "新位置不能拿到仍在使用的 id");
        assert_ne!(c, a, "也不该回收刚删掉那个 id：它的 session 文件可能还在");

        // 再删再加一轮，确认不是碰巧
        r.remove(&b);
        let d = add_tg(&r, "四号");
        assert_ne!(d, c, "第二轮同样不能撞");
        assert_ne!(d, b);
    }

    /// 删掉一个 Telegram 位置，另一个必须原封不动。
    ///
    /// 不这样会怎样：用户删掉账号 B，结果 A 也不见了或变成未连接——
    /// 而他完全不知道为什么，只会觉得这个功能不敢碰。
    #[test]
    fn removing_one_telegram_place_leaves_the_other() {
        let r = PlaceRegistry::new();
        let a = add_tg(&r, "留下的");
        let b = add_tg(&r, "删掉的");
        r.remove(&b);

        assert!(r.get(&b).is_none(), "被删的必须没了");
        let left = r.get(&a).expect("另一个必须还在");
        assert_eq!(left.name, "留下的", "另一个的名字不能被动过");
        assert_eq!(r.telegram_ids(), vec![a], "只剩一个 Telegram");
    }

    /// 改名只动显示名，其余一律不动。
    ///
    /// 不这样会怎样：改名时若重建了 store，已经建立的连接会被丢掉——
    /// 表现是「改了个名字，位置突然显示未连接」，而用户完全联系不起来。
    /// 位置 id 更不能变：它是 session 文件名的来源，变了等于换了个账号。
    #[test]
    fn renaming_changes_only_the_display_name() {
        let r = PlaceRegistry::new();
        let id = add_tg(&r, "原名");
        let before = r.get(&id).expect("先取一次");
        let store_before = Arc::as_ptr(&before.store);

        assert!(r.rename(&id, String::from("新名字")), "改名应当成功");

        let after = r.get(&id).expect("改完还在");
        assert_eq!(after.name, "新名字");
        assert_eq!(after.id, id, "位置 id 不能变——它决定了 session 文件名");
        assert_eq!(after.kind, "telegram");
        assert!(
            std::ptr::eq(Arc::as_ptr(&after.store), store_before),
            "必须复用同一个 store，重建会把已建立的连接弄丢"
        );
    }

    /// 给不存在的位置改名要如实返回 false，不能静默成功。
    ///
    /// 不这样会怎样：界面以为改成功了、刷新后发现没变，用户会反复试。
    #[test]
    fn renaming_a_missing_place_reports_failure() {
        let r = PlaceRegistry::new();
        assert!(!r.rename("p404", String::from("x")), "不存在的位置不该改成功");
    }

    /// **多个 Telegram 位置必须都能存进配置、都能恢复回来。**
    ///
    /// 不这样会怎样：旧的 persist 基于「只有一个 Telegram」的前提，
    /// 多账号时只会存下一个。重启后其余账号连同名字一起消失——而它们的
    /// session 文件还躺在磁盘上，成了没有位置引用的孤儿：用户既看不到
    /// 那个账号，也不知道本机还留着它的登录态。
    #[test]
    fn all_telegram_places_survive_persist_and_restore() {
        let r = PlaceRegistry::new();
        let a = add_tg(&r, "工作号");
        let b = add_tg(&r, "私人号");

        // 断言 persist 真正用来组装记录的那条路径（assemble_saved_places），
        // 而不是在测试里重抄一遍转换。抄一遍的话，无论是把循环改坏
        // （.take(1)）还是让 persist 根本不拼 Telegram 那一段，
        // 这条断言都不会失败——两种变异都实测确认过。
        let saved = match (r.places.lock(), r.order.lock()) {
            (Ok(m), Ok(o)) => PlaceRegistry::assemble_saved_places(&m, &o, None),
            // 锁取不到时退化成空列表，让下面那条断言去报「少了两条」：
            // omy-gui 全局禁用 panic，测试代码也不给它开口子
            _ => Vec::new(),
        };
        assert_eq!(saved.len(), 2, "两个 Telegram 位置都要被存下来");
        assert!(
            saved.iter().all(|s| s.kind == "telegram"),
            "这两条都该是 Telegram 记录"
        );

        let remote = omy_config::Remote {
            places: saved,
            ..omy_config::Remote::default()
        };
        let restored = PlaceRegistry::new();
        let (n, _) = restored.restore(&remote);
        assert_eq!(n, 2, "两个都要恢复出来");

        // id 必须原样保留：它是 session 文件名的来源，变了就找不回登录态
        assert_eq!(restored.telegram_ids(), vec![a, b], "id 必须原样恢复");
        let names: Vec<String> = restored.list().into_iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["工作号", "私人号"], "各自的名字也要回来");
    }

    /// 恢复出来的多个 Telegram 位置，id 不能互相覆盖。
    ///
    /// 不这样会怎样：restore 用 `m.insert(id, ...)`，两条记录若 id 相同
    /// 后一条会把前一条挤掉，于是「配置里有两个账号，启动后只剩一个」。
    /// 这条守住配置层面那道。
    #[test]
    fn restoring_keeps_each_telegram_place_separate() {
        let mk = |id: &str, name: &str| omy_config::SavedPlace {
            id: String::from(id),
            name: String::from(name),
            kind: String::from("telegram"),
            url: String::new(),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: None,
        };
        let remote = omy_config::Remote {
            places: vec![mk("p1", "甲"), mk("p2", "乙"), mk("p3", "丙")],
            ..omy_config::Remote::default()
        };
        let r = PlaceRegistry::new();
        let (n, _) = r.restore(&remote);
        assert_eq!(n, 3);
        assert_eq!(r.telegram_ids(), vec!["p1", "p2", "p3"]);
        // 恢复之后再加一个，不能撞上已有的任何一个 id
        let fresh = add_tg(&r, "丁");
        assert!(
            !["p1", "p2", "p3"].contains(&fresh.as_str()),
            "新位置拿到了已被占用的 id：{fresh}"
        );
    }

    /// 带 user id 造一个 Telegram 位置。
    fn add_tg_uid(r: &PlaceRegistry, name: &str, uid: i64) -> String {
        r.add_telegram(String::from(name), TelegramStore::new(), None, Some(uid))
            .expect("添加 Telegram")
    }

    /// **同一个 user id 能被查到，用于登录去重。**
    ///
    /// 不这样会怎样：去重靠这条查询判断「这个账号加过没有」。查不到就会
    /// 每次重复登录都新建一个同账号位置——正是这次要修的缺陷。
    #[test]
    fn find_telegram_by_user_hits_same_account() {
        let r = PlaceRegistry::new();
        let a = add_tg_uid(&r, "工作号", 111);
        add_tg_uid(&r, "私人号", 222);
        assert_eq!(
            r.find_telegram_by_user(111, None).as_deref(),
            Some(a.as_str()),
            "同一个 user id 必须命中它对应的位置"
        );
        assert!(
            r.find_telegram_by_user(999, None).is_none(),
            "没加过的 user id 不该命中任何位置"
        );
    }

    /// **exclude 能把某个位置排除掉。**
    ///
    /// 不这样会怎样：扫码路径会先建占位再查重，若不排除占位本身，就会
    /// 「自己命中自己」，把一次全新登录误判成重复。
    #[test]
    fn find_telegram_by_user_respects_exclude() {
        let r = PlaceRegistry::new();
        let only = add_tg_uid(&r, "唯一账号", 111);
        assert!(
            r.find_telegram_by_user(111, Some(&only)).is_none(),
            "排除唯一持有者后不该再命中"
        );
    }

    /// **不同 user id 各自建位置，去重不误伤多账号。**
    ///
    /// 不这样会怎样：去重做过头会把两个不同账号也并成一个，多账号直接失效。
    #[test]
    fn distinct_users_are_not_deduped() {
        let r = PlaceRegistry::new();
        let a = add_tg_uid(&r, "甲", 111);
        let b = add_tg_uid(&r, "乙", 222);
        assert_ne!(a, b);
        assert_eq!(r.find_telegram_by_user(111, None).as_deref(), Some(a.as_str()));
        assert_eq!(r.find_telegram_by_user(222, None).as_deref(), Some(b.as_str()));
    }

    /// **命中后覆盖连接：位置 id 与名字不变，user id 补上。**
    ///
    /// 不这样会怎样：去重命中已有账号时，要用更新鲜的登录态换掉旧连接，
    /// 但不能把用户起的名字冲掉、也不能换 id（换 id 等于换 session 文件）。
    #[test]
    fn update_connection_keeps_id_and_name() {
        let r = PlaceRegistry::new();
        let id = add_tg(&r, "我的账号"); // 占位此前没有 user id（老配置恢复的）
        assert!(
            r.update_telegram_connection(&id, TelegramStore::new(), Some(111)),
            "对已有 Telegram 位置换连接应成功"
        );
        let p = r.get(&id).expect("位置还在");
        assert_eq!(p.name, "我的账号", "名字不能被换连接冲掉");
        assert_eq!(p.user_id, Some(111), "占位应补上 user id");
        assert_eq!(r.find_telegram_by_user(111, None).as_deref(), Some(id.as_str()));
    }

    /// **user id 能持久化并在恢复后读回，用于重启后判重。**
    ///
    /// 不这样会怎样：user id 不落盘的话，重启后所有位置都「没有 user id」，
    /// 下次重复登录又会新建——去重只在本次运行内有效，形同虚设。
    #[test]
    fn user_id_round_trips_through_persist() {
        let r = PlaceRegistry::new();
        let id = add_tg_uid(&r, "账号", 12345);
        // 走产品真正的组装路径 assemble_saved_places（见其文档），不重抄
        let saved = {
            let (m, o) = (r.places.lock().unwrap(), r.order.lock().unwrap());
            PlaceRegistry::assemble_saved_places(&m, &o, None)
        };
        let sp = saved.iter().find(|s| s.id == id).expect("应存下这个位置");
        assert_eq!(sp.user_id, Some(12345), "user id 必须被存进 SavedPlace");

        let remote = omy_config::Remote {
            places: saved,
            ..omy_config::Remote::default()
        };
        let r2 = PlaceRegistry::new();
        r2.restore(&remote);
        assert_eq!(
            r2.find_telegram_by_user(12345, None).as_deref(),
            Some(id.as_str()),
            "恢复后必须还能按 user id 查到"
        );
    }

    /// **老配置没有 user_id 字段时，反序列化不崩、位置照常恢复。**
    ///
    /// 不这样会怎样：给 SavedPlace 加字段若没配 serde default，老用户升级后
    /// 整份 remote.places 反序列化失败，所有远程位置一起消失——这是最恶劣的
    /// 升级回归，而且不报错，用户只看到「我的位置全没了」。
    #[test]
    fn old_config_without_user_id_still_loads() {
        let old = r#"
[[places]]
id = "p1"
name = "老账号"
kind = "telegram"
url = ""
username = ""
vendor = ""
writable = true
"#;
        let remote: omy_config::Remote =
            toml::from_str(old).expect("老配置必须仍能反序列化");
        assert_eq!(remote.places.len(), 1, "位置不能丢");
        assert_eq!(remote.places[0].user_id, None, "缺字段应回落为 None");

        let r = PlaceRegistry::new();
        let (n, _) = r.restore(&remote);
        assert_eq!(n, 1, "位置必须恢复出来");
        assert!(r.get("p1").is_some(), "p1 应存在");
    }
}
