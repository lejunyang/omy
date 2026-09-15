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

use omy_remote::webdav::{Vendor, WebDavConfig, WebDavStore};
use omy_remote::{Capabilities, RemoteStore};

/// 一个已注册的远程位置。
pub struct Place {
    /// 进程内标识，前端用它指代。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 驱动类型，目前只有 `webdav`。
    pub kind: String,
    /// 驱动实例。
    pub store: Arc<WebDavStore>,
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
        let store = Arc::new(WebDavStore::new(cfg)?);
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
            kind: String::from("webdav"),
            store,
        });
        if let (Ok(mut m), Ok(mut o)) = (self.places.lock(), self.order.lock()) {
            m.insert(id.clone(), place);
            o.push(id.clone());
        }
        Ok(id)
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
}
