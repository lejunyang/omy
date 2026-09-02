//! 容器内单个文件的访问登记表。
//!
//! # 为什么需要它
//!
//! 容器（加密的文件夹）的载荷是**多个文件首尾相接**。想预览其中一个，
//! 就得知道它在明文载荷里的区间——而这个区间只有解开容器索引才知道。
//!
//! 协议层不能直接收「偏移 + 长度」当参数：那等于把「读这个容器里任意
//! 位置的任意长度」的能力交给 WebView 里的任何脚本，越过了索引这层
//! 约束。所以这里沿用 [`crate::plain`] 的做法——**只有列过的条目才有
//! token**，协议层只认 token。
//!
//! # 与 `/plain/` 的区别
//!
//! | | `/plain/<token>` | `/citem/<token>` |
//! |---|---|---|
//! | 目标 | 磁盘上的明文文件 | 容器载荷里的一段区间 |
//! | 需要密钥 | 否 | 是 |
//! | 每次请求的授权判断 | token 在表里 | token 在表里**且**所属容器仍解锁 |
//!
//! 最后一行是关键：token 里存的是所属容器的 **entry id**，不是密钥也
//! 不是路径。协议层每次请求都要拿这个 id 回查会话状态，锁定之后同一个
//! token 会立刻失效。登记时的解锁状态**不构成**后续请求的授权依据。
//!
//! # 锁定时必须清空
//!
//! 即使上面那道「回查解锁状态」已经足够，锁定时仍然要清表。理由与
//! `PlainRegistry::clear` 相同：留着一批 token 与「锁定后什么都看不到」
//! 的预期不符，而两道防线里任何一道单独成立都不该被当成可以省掉另一道。

use std::collections::HashMap;
use std::sync::Mutex;

/// 容器内一个文件的定位信息。
///
/// 刻意**不存**路径与密钥：路径由 entry id 在请求时回查，密钥在会话里。
/// 这样锁定后这张表里剩下的东西不足以读出任何内容。
#[derive(Debug, Clone)]
pub struct ContainerRef {
    /// 所属容器文件的 entry id。协议层用它回查「现在还解锁着吗」。
    pub entry_id: String,
    /// 相对容器根的路径，仅用于诊断与 MIME 推导时的可读性。
    pub inner_path: String,
    /// 在容器**明文载荷**中的起始偏移。
    pub offset: u64,
    /// 该文件的字节数。
    pub size: u64,
    /// MIME，按容器内的文件名后缀推导。
    pub mime: String,
}

/// 容器内文件的访问登记表。
#[derive(Default)]
pub struct ContainerRegistry {
    inner: Mutex<HashMap<String, ContainerRef>>,
}

impl ContainerRegistry {
    /// 新建空登记表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个容器内文件，返回访问 token。
    ///
    /// 同一（容器, 内部路径）重复登记得到**同一个** token——容器面板每次
    /// 刷新都会重新登记，不去重的话浏览几次就堆满一张表。
    pub fn register(&self, r: ContainerRef) -> Option<String> {
        let key = token_for(&r.entry_id, &r.inner_path);
        let mut g = self.inner.lock().ok()?;
        // 用 insert 而不是 or_insert：偏移和大小可能因为容器被重新加密
        // 而变化，此时旧值是错的，必须覆盖
        g.insert(key.clone(), r);
        Some(key)
    }

    /// 按 token 取回定位信息。
    pub fn resolve(&self, token: &str) -> Option<ContainerRef> {
        self.inner.lock().ok()?.get(token).cloned()
    }

    /// 清空登记表（锁定时调用）。
    pub fn clear(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.clear();
        }
    }

    /// 当前登记数量。只有测试用得上。
    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|g| g.len()).unwrap_or(0)
    }

    /// 是否为空。
    #[cfg(test)]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 由（容器 id, 内部路径）算出稳定 token。
///
/// 两段之间插 `\0`：直接拼接的话 `("ab", "c")` 与 `("a", "bc")` 会算出
/// 同一个 token，而 `\0` 不可能出现在两段中的任何一段里。
///
/// 用哈希而不是把路径放进 URL：容器内的文件名是**解密出来的内容**，
/// 让它出现在 URL 里等于写进 WebView 的网络面板。
fn token_for(entry_id: &str, inner_path: &str) -> String {
    let mut buf = Vec::with_capacity(entry_id.len().saturating_add(inner_path.len()).saturating_add(1));
    buf.extend_from_slice(entry_id.as_bytes());
    buf.push(0);
    buf.extend_from_slice(inner_path.as_bytes());
    let h = omy_core::util::blake2b_256(&buf);
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk(entry: &str, path: &str, offset: u64, size: u64) -> ContainerRef {
        ContainerRef {
            entry_id: String::from(entry),
            inner_path: String::from(path),
            offset,
            size,
            mime: String::from("text/plain"),
        }
    }

    #[test]
    fn token_is_stable_for_same_item() {
        // 前端会缓存 URL，同一条目必须始终得到同一个 token
        let a = token_for("e1", "a/b.txt");
        let b = token_for("e1", "a/b.txt");
        assert_eq!(a, b);
    }

    #[test]
    fn token_differs_across_containers_and_paths() {
        assert_ne!(token_for("e1", "a.txt"), token_for("e2", "a.txt"));
        assert_ne!(token_for("e1", "a.txt"), token_for("e1", "b.txt"));
    }

    /// 分隔符缺失会让不同条目撞成同一个 token。
    ///
    /// 若把两段直接拼起来哈希，`("ab","c")` 与 `("a","bc")` 得到同一个
    /// token——一个容器里的文件会被另一个容器的 URL 读到。
    #[test]
    fn token_is_not_ambiguous_across_field_boundary() {
        assert_ne!(token_for("ab", "c"), token_for("a", "bc"));
    }

    #[test]
    fn token_does_not_leak_inner_name() {
        // 容器内的文件名是解密出来的内容，不能出现在 URL 里
        let t = token_for("e1", "工资表/2026-机密.xlsx");
        assert!(!t.contains("机密"));
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn registry_roundtrip() {
        let r = ContainerRegistry::new();
        let t = r.register(mk("e1", "a.txt", 10, 20)).unwrap_or_default();
        let got = r.resolve(&t).expect("刚登记的 token 必须能解析");
        assert_eq!(got.entry_id, "e1");
        assert_eq!(got.offset, 10);
        assert_eq!(got.size, 20);
    }

    #[test]
    fn registry_rejects_unknown_token() {
        // 这条守护的是「构造 token 读容器任意位置」
        let r = ContainerRegistry::new();
        assert!(r.resolve("deadbeef").is_none());
        assert!(r.resolve("").is_none());
    }

    #[test]
    fn re_registering_does_not_grow_table() {
        let r = ContainerRegistry::new();
        for _ in 0..50 {
            let _ = r.register(mk("e1", "a.txt", 0, 1));
        }
        assert_eq!(r.len(), 1, "同一条目不应重复占用");
    }

    /// 重新登记必须覆盖偏移，不能保留旧值。
    ///
    /// 容器被重新加密后，同名文件在载荷里的位置会变。若沿用旧偏移，
    /// 预览出来的是**另一个文件的字节**——而且不会报错，只是内容不对。
    #[test]
    fn re_registering_updates_offset() {
        let r = ContainerRegistry::new();
        let t = r.register(mk("e1", "a.txt", 0, 10)).unwrap_or_default();
        let _ = r.register(mk("e1", "a.txt", 999, 20));
        let got = r.resolve(&t).expect("token 不变");
        assert_eq!(got.offset, 999, "偏移必须更新为最新值");
        assert_eq!(got.size, 20);
    }

    #[test]
    fn clear_empties_registry() {
        let r = ContainerRegistry::new();
        let _ = r.register(mk("e1", "a.txt", 0, 1));
        assert!(!r.is_empty());
        r.clear();
        assert!(r.is_empty(), "锁定后不应残留可用 token");
    }
}
