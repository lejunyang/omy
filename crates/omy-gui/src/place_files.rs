//! 已打开的远程存储文件，与全局密文块缓存。
//!
//! # 为什么需要一张「打开句柄表」
//!
//! 远程浏览（[`crate::place_cmds`]）只列目录、读前几百字节识别文件，
//! 并不持有可播放的来源。而播放是**多次** Range 请求：播放器先取开头，
//! 再按拖动位置取中段、尾段。这些请求必须复用同一个 [`RemoteSource`]——
//! 它内部带着 1 MiB 对齐的密文块缓存，若每次请求都新建一个，缓存永远
//! 命中不了，seek 一次就重新拉一遍。
//!
//! 所以前端点开一个远程 `.omy` 时，后端构造好 `RemoteSource` 登记到这里，
//! 换回一个不透明 token；之后 `omystream://pfile/<token>` 的每次 Range
//! 请求都凭 token 取回同一个来源。
//!
//! # 缓存的只有密文
//!
//! [`RemoteSource`] 持有的头部字节、[`BlockCache`] 落盘的块，全部是
//! **加密的**。每次请求时 [`omy_core::file::open`] 用会话里的 KEK 当场
//! 重新推导 payload key，解出请求的那一段、随响应发走、立即丢弃——
//! 不在内存里长期持有 `OpenedFile`，磁盘上也只有密文。锁定时关闭句柄即可。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use omy_remote::cache::BlockCache;
use omy_remote::source::RemoteSource;
use omy_remote::webdav::WebDavStore;

/// 全局密文块缓存。
///
/// 所有远程位置、所有打开的文件共用同一个缓存根目录和同一个上限，
/// 这样「已用空间 / 上限」是全局口径，设置页里的数字才与磁盘真实占用一致。
#[derive(Default)]
pub struct RemoteCache {
    /// `None` 表示缓存目录无法建立，此时退化为「不缓存、每次走网络」，
    /// 而不是让整个远程功能不可用。
    inner: Mutex<Option<BlockCache>>,
}

impl RemoteCache {
    /// 按当前配置建立缓存根。
    #[must_use]
    pub fn from_config() -> Self {
        let cfg = omy_config::Config::load().unwrap_or_default();
        Self::from_root(Self::resolve_root(cfg.remote.cache_dir.clone()), cfg.remote.cache_limit)
    }

    /// 指定根目录与上限（测试与设置更新用）。
    fn from_root(root: Option<PathBuf>, limit: u64) -> Self {
        let cache = root.and_then(|r| match BlockCache::new(&r, limit) {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[omy] 远程缓存目录 {r:?} 无法建立，本次不缓存: {e}");
                None
            }
        });
        Self {
            inner: Mutex::new(cache),
        }
    }

    /// 取一份缓存句柄的克隆传给新建的 `RemoteSource`。
    ///
    /// `BlockCache` 的克隆共享同一根目录（LRU 靠文件 atime 维护），
    /// 所以多个来源能真正共享同一份磁盘缓存。
    #[must_use]
    pub fn snapshot(&self) -> Option<BlockCache> {
        self.inner.lock().ok()?.clone()
    }

    /// 当前缓存占用字节数。
    #[must_use]
    pub fn used(&self) -> u64 {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(BlockCache::used))
            .unwrap_or(0)
    }

    /// 配置的上限字节数（0 表示不限）。
    #[must_use]
    pub fn limit(&self) -> u64 {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.limit()))
            .unwrap_or(0)
    }

    /// 缓存根目录，供设置页「打开缓存目录」。
    #[must_use]
    pub fn root(&self) -> Option<PathBuf> {
        self.inner
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|c| c.root().to_path_buf()))
    }

    /// 清空全部缓存块，返回清空后的占用（应为 0）。
    pub fn clear(&self) -> u64 {
        if let Ok(g) = self.inner.lock() {
            if let Some(c) = g.as_ref() {
                // 清失败不致命：下次淘汰仍会回收，这里只如实返回当前占用
                let _ = c.clear();
            }
            return g.as_ref().map(BlockCache::used).unwrap_or(0);
        }
        0
    }

    /// 设置变更后重建：换根目录或调整上限。
    ///
    /// 换目录不搬运旧块——它们是密文，按新键也读不到，留着只会占空间，
    /// 但这里不主动删用户指定过的旧目录，交给「立即清理」处理。
    pub fn reload(&self, root: Option<PathBuf>, limit: u64) {
        if let Ok(mut g) = self.inner.lock() {
            *g = root.and_then(|r| BlockCache::new(&r, limit).ok());
        }
    }

    /// 解析远程块缓存根目录：用户自定义优先，否则落在应用缓存目录下的
    /// `remote/`。初始化与「设置保存后重建」共用这一处，避免两处算出
    /// 不同的目录（新增字段时两处都要改）。
    #[must_use]
    pub fn resolve_root(custom: Option<String>) -> Option<PathBuf> {
        custom
            .map(PathBuf::from)
            .or_else(omy_config::cache_dir)
            .map(|base| base.join("remote"))
    }
}

/// 一个已打开、可按需解密播放的远程文件。
///
/// 只保留服务一次 Range 请求所需的东西：完整头部（每次重新 `open`）、
/// 接好缓存与网络的来源、解密内容的 MIME。位置 id 与路径在构造
/// `RemoteSource` 时已固化进缓存键，这里不重复持有；真实文件名、明文大小、
/// 预览类别在打开命令的返回值里给前端，播放阶段用不到。
pub struct OpenPlaceFile {
    /// 完整文件头（密文）。每次请求据此重新 `open`，不长期持有 payload key。
    pub header: Vec<u8>,
    /// 接好缓存与网络的密文来源。
    pub source: RemoteSource<WebDavStore>,
    /// 解密后内容的 MIME。
    pub mime: String,
}

/// 已打开远程文件的句柄表。
#[derive(Default)]
pub struct PlaceFiles {
    files: Mutex<HashMap<String, Arc<OpenPlaceFile>>>,
    seq: Mutex<u64>,
}impl PlaceFiles {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一个打开的文件，返回不透明 token。
    pub fn insert(&self, f: OpenPlaceFile) -> Option<String> {
        let mut seq = self.seq.lock().ok()?;
        *seq += 1;
        let token = format!("pf{seq}");
        // token 自增后再拿文件表锁插入，保证「拿到的 token 必已登记」
        let mut files = self.files.lock().ok()?;
        files.insert(token.clone(), Arc::new(f));
        Some(token)
    }

    /// 按 token 取打开的文件。
    #[must_use]
    pub fn get(&self, token: &str) -> Option<Arc<OpenPlaceFile>> {
        self.files.lock().ok()?.get(token).cloned()
    }

    /// 关闭单个文件（播放结束）。
    pub fn remove(&self, token: &str) {
        if let Ok(mut m) = self.files.lock() {
            m.remove(token);
        }
    }

    /// 关闭全部文件（锁定时调用）。
    ///
    /// 句柄里虽只有密文，但锁定的语义是「从现在起什么都读不到」，
    /// 留着一批可凭 token 直接 Range 的来源与该语义不符。
    ///
    /// 自增序号不清零：token 只在本次进程生命周期内有效，清零可能让
    /// 锁定前后的两个文件复用同一 token。
    pub fn clear(&self) {
        if let Ok(mut m) = self.files.lock() {
            m.clear();
        }
    }
}

/// 远程缩略图句柄表（只存文件头，不存正文来源）。
///
/// 列表里每个媒体文件都要显示缩略图，但为每张图都构造一个带网络来源的
/// [`OpenPlaceFile`] 既浪费也无必要——缩略图 TLV 就在**文件头**里，
/// `browse` 列目录时已经把完整头部取回来了。这里登记「token → 完整头部」，
/// `omystream://pthumb/<token>` 凭头部当场 `open` 出缩略图，零额外网络请求。
///
/// 与 [`PlaceFiles`] 分开：缩略图只服务当前这一屏列表，刷新目录即清空，
/// 不与播放句柄（要跨多次 Range 请求、锁定才清）混在同一张表里。
#[derive(Default)]
pub struct PlaceThumbs {
    thumbs: Mutex<HashMap<String, Vec<u8>>>,
    seq: Mutex<u64>,
}

impl PlaceThumbs {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一份完整文件头用于取缩略图，返回不透明 token。
    pub fn insert(&self, header: Vec<u8>) -> Option<String> {
        let mut seq = self.seq.lock().ok()?;
        *seq += 1;
        let token = format!("pt{seq}");
        let mut thumbs = self.thumbs.lock().ok()?;
        thumbs.insert(token.clone(), header);
        Some(token)
    }

    /// 按 token 取文件头字节。
    #[must_use]
    pub fn get(&self, token: &str) -> Option<Vec<u8>> {
        self.thumbs.lock().ok()?.get(token).cloned()
    }

    /// 刷新目录或锁定时清空：上一屏的缩略图 token 全部失效。
    pub fn clear(&self) {
        if let Ok(mut m) = self.thumbs.lock() {
            m.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 根目录为 `None`（无法定位缓存位置）时降级为无缓存，而不是让状态构造失败。
    ///
    /// 不这样会怎样：移动端取不到缓存目录时，整个远程功能在启动时不可用——
    /// 但缓存本就是可有可无的加速层，没它顶多每次都走网络。
    #[test]
    fn cache_degrades_when_root_unknown() {
        let cache = RemoteCache::from_root(None, 1024);
        assert!(cache.snapshot().is_none(), "无根目录时句柄应为 None");
        assert_eq!(cache.used(), 0, "无缓存时占用恒为 0");
        assert_eq!(cache.clear(), 0, "清空无缓存也不应报错");
        assert_eq!(cache.limit(), 0);
    }

    /// 正常临时目录下缓存可用：写入增长、上限如实、清空归零。
    #[test]
    fn cache_works_in_temp_dir() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("omy_place_cache_{}_{nanos}", std::process::id()));
        let cache = RemoteCache::from_root(Some(dir.clone()), 4096);
        let snap = cache.snapshot().expect("临时目录应能建缓存");
        assert_eq!(cache.used(), 0);
        snap.put("p", "/a", 0, &[1u8; 500]);
        assert!(cache.used() >= 500, "写入后占用应增长");
        assert_eq!(cache.limit(), 4096);
        cache.clear();
        assert_eq!(cache.used(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// token 必须唯一，关闭后取不到，锁定清空后全空。
    ///
    /// 不这样会怎样：两个文件拿到同一 token，第二个 Range 请求解到第一个
    /// 文件的数据，表现为播放串台且极难排查。
    #[test]
    fn place_files_tokens_are_unique_and_revocable() {
        let files = PlaceFiles::new();
        let t1 = files.insert(make_holder(10)).expect("token1");
        let t2 = files.insert(make_holder(20)).expect("token2");
        assert_ne!(t1, t2, "两次打开的 token 不能相同");
        assert!(files.get(&t1).is_some() && files.get(&t2).is_some());
        files.remove(&t1);
        assert!(files.get(&t1).is_none(), "关闭后取不到");
        assert!(files.get(&t2).is_some(), "另一个不受影响");
        files.clear();
        assert!(files.get(&t2).is_none(), "锁定清空后旧 token 全失效");
        // 清空后再打开不复用旧 token
        let t3 = files.insert(make_holder(30)).expect("token3");
        assert_ne!(t3, t2, "清空后 token 也不能复用");
    }

    /// 缩略图表：token 唯一可取，刷新 / 锁定清空后旧 token 全失效。
    ///
    /// 不这样会怎样：列表反复进出目录，文件头在句柄表里只增不减，且上一屏
    /// 的 pthumb token 还能取到内容——与「锁定后什么都读不到」相悖。
    #[test]
    fn place_thumbs_register_and_clear() {
        let t = PlaceThumbs::new();
        let a = t.insert(vec![1u8, 2, 3]).expect("token a");
        let b = t.insert(vec![4u8]).expect("token b");
        assert_ne!(a, b, "两次登记的 token 不能相同");
        assert_eq!(t.get(&a).as_deref(), Some(&[1u8, 2, 3][..]));
        assert_eq!(t.get(&b).as_deref(), Some(&[4u8][..]));
        t.clear();
        assert!(
            t.get(&a).is_none() && t.get(&b).is_none(),
            "刷新/锁定后旧缩略图 token 必须全失效"
        );
    }

    /// 造一个 holder。`RemoteSource::new` 只 `peek_header`、不发网络，
    /// 所以给它一段真实加密文件的头部即可在没有服务器的情况下构造成功。
    fn make_holder(tag: u64) -> OpenPlaceFile {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{encrypt, EncryptOptions, RandomMaterial};
        use omy_remote::webdav::{Vendor, WebDavConfig};

        let salt = [7u8; 16];
        let params = Argon2Params::TEST_WEAK;
        let kek = Kek::from_password(b"x", &salt, params).expect("KEK");
        let opts = EncryptOptions {
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(
            b"hello-remote",
            std::slice::from_ref(&kek),
            &salt,
            &opts,
            &RandomMaterial::generate(),
        )
        .expect("加密");

        let store = Arc::new(
            WebDavStore::new(WebDavConfig {
                base_url: String::from("http://127.0.0.1:1/"),
                vendor: Vendor::Generic,
                ..WebDavConfig::default()
            })
            .expect("store"),
        );
        // 独立运行时；holder 测试不真正读取，句柄随测试结束而丢弃
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let path = format!("/{tag}.omy");
        let source = RemoteSource::new(
            store,
            "p1",
            path,
            &enc.bytes,
            enc.bytes.len() as u64,
            None,
            rt.handle().clone(),
        )
        .expect("真实头部应能构造来源");

        OpenPlaceFile {
            header: enc.bytes,
            source,
            mime: String::from("text/plain; charset=utf-8"),
        }
    }
}
