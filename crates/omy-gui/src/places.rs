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
}

/// 远程位置注册表。
#[derive(Default)]
pub struct PlaceRegistry {
    places: Mutex<HashMap<String, Arc<Place>>>,
    /// 注册顺序，用于让列表顺序稳定。
    ///
    /// HashMap 的遍历顺序每次都不同，直接用它会让侧栏的位置顺序乱跳。
    order: Mutex<Vec<String>>,
}

impl PlaceRegistry {
    /// 空注册表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
        let id = {
            let Ok(o) = self.order.lock() else {
                return Err(omy_remote::Error::Protocol(String::from("注册表锁失效")));
            };
            format!("p{}", o.len() + 1)
        };
        let place = Arc::new(Place {
            id: id.clone(),
            name,
            // 由驱动自己报类型，不在这里写字面量：两处各写一份迟早对不上，
            // 而对不上的后果是配置存进去读回来变成另一种驱动
            kind: String::from(store.kind()),
            store,
        });
        if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
            m.insert(id.clone(), place);
            o.push(id.clone());
        }
        Ok(id)
    }

    /// 添加一个 Telegram 位置，返回其 id。
    ///
    /// `store` 由调用方建好（它需要一个已连接的客户端，而建连要走网络、
    /// 属于命令层的事）。注册表只管登记，不碰网络——把建连放进来会让
    /// 「添加一个位置」这个同步操作变成可能卡几十秒的操作。
    ///
    /// # Errors
    ///
    /// 注册表锁失效时返回。
    pub fn add_telegram(&self, name: String, store: TelegramStore) -> omy_remote::Result<String> {
        let store = Arc::new(PlaceStore::from(store));
        let id = {
            let Ok(o) = self.order.lock() else {
                return Err(omy_remote::Error::Protocol(String::from("注册表锁失效")));
            };
            format!("p{}", o.len() + 1)
        };
        let place = Arc::new(Place {
            id: id.clone(),
            name,
            kind: String::from(store.kind()),
            store,
        });
        if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
            m.insert(id.clone(), place);
            o.push(id.clone());
        }
        Ok(id)
    }

    /// 已注册的 Telegram 位置 id（若有）。
    ///
    /// 只会有一个：一个 omy 同时只持有一份 Telegram 登录态。查它是为了避免
    /// 重复添加——重复添加的表现是侧栏里出现两个一模一样的 Telegram，
    /// 而它们背后其实是同一个账号。
    #[must_use]
    pub fn telegram_id(&self) -> Option<String> {
        let (Ok(m), Ok(o)) = (self.places.lock(), self.order.lock()) else {
            return None;
        };
        o.iter()
            .filter_map(|id| m.get(id))
            .find(|p| p.kind == "telegram")
            .map(|p| p.id.clone())
    }

    /// 按 id 取位置。
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<Place>> {
        self.places.lock().ok()?.get(id).cloned()
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
        let (Ok(m), Ok(o)) = (self.places.lock(), self.order.lock()) else {
            return Err(String::from("注册表锁失效"));
        };

        let mut saved: Vec<omy_config::SavedPlace> = Vec::new();

        // Telegram：没有 url / username / 密码可存——凭据在单独加密的
        // session 文件里，这里只记「有这么一个位置」。
        //
        // 不能沿用下面 WebDAV 那条链路：它靠 as_webdav() 过滤，
        // Telegram 会被静默丢掉，表现是「加过的位置重启就没了」而且不报错。
        for p in o.iter().filter_map(|id| m.get(id)) {
            if p.kind == "telegram" {
                saved.push(omy_config::SavedPlace {
                    id: p.id.clone(),
                    name: p.name.clone(),
                    kind: p.kind.clone(),
                    url: String::new(),
                    username: String::new(),
                    vendor: String::new(),
                    // 能不能写由对话决定（effective_capabilities），
                    // 位置级这一位对 Telegram 没有意义，存 true 只是别把
                    // 整个位置钉死成只读
                    writable: true,
                    // 凭据不在这里：Telegram 的登录态由
                    // omy_remote::telegram::session 单独加密落盘
                    secret: None,
                });
            }
        }

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
                    key.as_ref()
                        .and_then(|k| omy_secret::seal(k, c.password.as_bytes()).ok())
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
                }
            })
            .collect();
        saved.extend(webdav_saved);

        let mut cfg = omy_config::Config::load().map_err(|e| e.to_string())?;
        cfg.remote.places = saved;
        cfg.save().map_err(|e| e.to_string())
    }

    /// 从配置恢复已保存的位置。
    ///
    /// 解不开密码的位置**照样恢复**，只是没有密码：用户会看到这个位置
    /// 还在、标着需要重新登录，而不是以为自己的配置丢了。
    ///
    /// 返回成功恢复的位置数与其中缺密码的个数。
    pub fn restore(&self, cfg: &omy_config::Remote) -> (usize, usize) {
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
}
