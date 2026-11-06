//! 远程位置的 Tauri 命令。
//!
//! # 列目录时顺带识别加密文件
//!
//! 这是格式设计带来的便宜：识别一个 `.omy` 文件只需读**开头 480 字节**
//! （见 `omy_core::scan` 的模块文档），载荷完全不碰。放到远程就是一次
//! Range 请求——扫 100 个文件只下约 48 KB，而不是把文件拉下来。
//!
//! # 三种状态必须分开
//!
//! 已解锁 / 是 omy 但当前密码打不开 / **没能探测成功**（网络失败）。
//! 第三种混进第二种是最糟的结果：用户会以为自己记错密码而反复尝试，
//! 真正的问题却是网络。

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tauri::Emitter;

use crate::commands::{CmdError, CmdResult, Shared};
use crate::decrypt::{DECRYPT_PROGRESS_EVENT, DecryptProgress};
use crate::place_files::{OpenPlaceFile, PlaceContainers, PlaceFiles, PlaceThumbs, RemoteCache};
use crate::places::{PlaceInfo, PlaceRegistry};
use omy_core::crypto::Kek;
use omy_remote::source::RemoteSource;
use omy_remote::webdav::WebDavConfig;
use omy_remote::{Error as RemoteError, PlaceStore, RemoteStore};

/// 远程目录里的一项，已附带识别结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteEntry {
    /// 条目 id（在该位置内的路径）。
    pub id: String,
    /// 显示名（未解密的名字）。
    pub name: String,
    /// 是否为目录。
    pub is_dir: bool,
    /// 字节数。
    pub size: Option<u64>,
    /// 上传/发送时间（Unix 秒）。Telegram 媒体用消息日期，供列表视图显示。
    /// WebDAV 若服务端不给则为 None。
    pub mtime: Option<i64>,
    /// 是否为 omy 加密文件。
    pub is_encrypted: bool,
    /// 是否已用当前会话的密钥解开。
    pub unlocked: bool,
    /// 解开后的真实文件名。
    pub real_name: Option<String>,
    /// 明文大小。
    pub plaintext_size: Option<u64>,
    /// 探测是否失败（网络原因）。
    ///
    /// 与 `unlocked == false` 是**两回事**：那个是「密码不对」，
    /// 这个是「根本没读到」。界面必须用不同的图标和文案，
    /// 否则用户会去反复试密码而不是检查网络。
    pub probe_failed: bool,
    /// 列表缩略图令牌：仅当已解锁且文件头里确实带缩略图时为 `Some`，
    /// 前端据此请求 `omystream://pthumb/<token>`；否则回退类型图标。
    pub thumb_token: Option<String>,
    /// 是否仍在后台识别中（边扫边出的骨架态）。
    ///
    /// 列目录（PROPFIND）很快、逐文件读头部识别较慢；前端先拿到一屏带
    /// `probing=true` 的骨架立即渲染，后台每识别完一个就用事件推一条最终
    /// 条目（`probing=false`）就地替换。该态与「未能读取」「锁定」都不同，
    /// 必须单独成态，不能让用户对着还没出结果的条目猜密码。
    pub probing: bool,
    /// 所在文件分栏（仅 Telegram，来自 omy_remote Entry.media_tab）。
    /// 供前端「在文件中显示」切到正确分栏；非 Telegram 为 `None`。
    pub media_tab: Option<String>,
}

/// 把远程错误映射为结构化错误码。
///
/// # 为什么 CDN 重定向要单拎一条
///
/// 它也是 `Unsupported`，落到通用的 `remote_unsupported` 上，用户看到的是
/// 「该位置不支持此操作」——既不说是哪个操作，也不说能怎么办，是个死胡同。
/// 而这件事对用户其实有明确的下一步（改用官方客户端下载），值得一句自己的话。
fn to_cmd_err(e: &RemoteError) -> CmdError {
    let code = match e {
        RemoteError::Unauthorized => "remote_unauthorized",
        RemoteError::Forbidden => "remote_forbidden",
        RemoteError::NotFound(_) => "remote_not_found",
        RemoteError::RateLimited => "remote_rate_limited",
        RemoteError::Unsupported(what) if *what == CDN_REDIRECT => "tg_cdn_unsupported",
        RemoteError::Unsupported(what) if *what == BROADCAST_NO_MESSAGES => {
            "tg_broadcast_no_messages"
        }
        RemoteError::Unsupported(_) => "remote_unsupported",
        RemoteError::Network(_) => "remote_network",
        _ => "remote_failed",
    };
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

/// `read_range` 撞到 CDN 重定向时给出的标记。
///
/// 与 `omy_remote::telegram::store` 里那个 `Unsupported` 的参数必须逐字相同。
/// 做成常量并由两边共用，是因为靠字面量对暗号的写法会在改动一侧时静默失效——
/// 而失效的表现只是「文案退回通用错误」，没有任何报错。
const CDN_REDIRECT: &str = omy_remote::telegram::store::CDN_REDIRECT;

/// 广播频道不提供消息视图时的标记。与 omy-remote 里那个共用同一个常量，
/// 理由同 [`CDN_REDIRECT`]：两边各写一份字面量会在改动一侧时静默失效。
const BROADCAST_NO_MESSAGES: &str = omy_remote::telegram::store::BROADCAST_NO_MESSAGES;

/// 添加一个 WebDAV 位置。
///
/// # Errors
///
/// URL 非法或客户端构造失败时返回。
#[tauri::command]
pub fn remote_place_add(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    name: String,
    url: String,
    username: String,
    password: String,
    vendor: String,
    writable: bool,
) -> CmdResult<String> {
    let cfg = WebDavConfig {
        base_url: url,
        username,
        password,
        vendor: omy_remote::parse_vendor(&vendor),
        writable,
        ..WebDavConfig::default()
    };
    let id = reg.add_webdav(name, cfg).map_err(|e| to_cmd_err(&e))?;
    crate::applog::info("place", &format!("新增 WebDAV 位置 id={id}"));
    // 立刻落盘：用户加完位置就可能直接关掉应用，等到退出再存会丢。
    //
    // 存不上不让整个添加失败——位置在本次会话里是可用的，只是重启后
    // 要重加。那比「加了半天说失败了、其实连接是好的」体验好。
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 远程位置未能保存：{e}");
    }
    Ok(id)
}

/// 列出已注册的远程位置。
#[tauri::command]
pub fn remote_place_list(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
) -> Vec<PlaceInfo> {
    let mut places = reg.list();
    // 已加密的 Telegram 位置：据当前会话 KEK 判断是开口锁（可免密进）还是闭合锁。
    // 未加密的保持 None，前端按「无锁标」渲染。
    for p in &mut places {
        if p.kind == "telegram" && p.tg_encrypted == Some(true) {
            p.tg_unlocked = Some(crate::telegram_cmds::telegram_session_unlocked(
                &p.id, &state,
            ));
        }
    }
    places
}

/// 本机凭据保护是否可用。
///
/// 设置页据此显示「密码已加密保存在本机」还是「这台机器无法安全保存
/// 密码，每次启动需重新输入」。
#[tauri::command]
#[must_use]
pub fn remote_secret_status() -> crate::places::SecretStatus {
    PlaceRegistry::secret_status()
}

/// 移除一个远程位置。
#[tauri::command]
pub fn remote_place_remove(reg: tauri::State<'_, Arc<PlaceRegistry>>, id: String) {
    crate::applog::info("place", &format!("移除位置 id={id}"));
    reg.remove(&id);
    // 同样立刻落盘，否则删掉的位置重启后又回来了
    if let Err(e) = reg.persist() {
        eprintln!("[omy] 远程位置未能保存：{e}");
    }
}

// 命令按主题拆在子模块里；这里统一再导出，`lib.rs` 注册命令与其他模块引用
// 时仍写 `place_cmds::xxx`。子模块里非 pub 的辅助项是 `pub(super)`，经这里的
// glob 再导出后兄弟模块用 `use super::*` 即可互相调用。
mod browse;
mod cache;
mod messages;
mod open;
mod transfers;
mod upload;

pub use browse::*;
pub use cache::*;
pub use messages::*;
pub use open::*;
pub use transfers::*;
pub use upload::*;

#[cfg(test)]
mod tests {

    /// CDN 重定向要有自己的错误码，不能落到通用的「不支持此操作」。
    ///
    /// 不这样会怎样：用户看到一句「该位置不支持此操作」——既不说是哪个操作，
    /// 也不说能怎么办，是个死胡同。而这件事其实有明确的下一步（改用官方客户端
    /// 下载），值得一句自己的话。
    ///
    /// 这条映射的失效方式很安静：两侧字面量只要有一处改动，CDN 错误就会退回
    /// 通用码，界面上只是文案变笼统，不会有任何报错。
    #[test]
    fn cdn_redirect_gets_its_own_error_code() {
        let e = to_cmd_err(&RemoteError::Unsupported(CDN_REDIRECT));
        assert_eq!(
            e.code, "tg_cdn_unsupported",
            "CDN 重定向应当有专门的错误码，实际是 {}",
            e.code
        );

        // 其他 Unsupported 仍然走通用码——否则这条特判就成了「所有不支持都
        // 说成 CDN」，那是另一种误导
        let other = to_cmd_err(&RemoteError::Unsupported("something else"));
        assert_eq!(other.code, "remote_unsupported");
    }

    /// 常量必须与 omy-remote 里那个逐字相同。
    ///
    /// 不这样会怎样：两边各写一份字面量，改一侧不会有编译错误，
    /// 上面那条断言却仍然通过（它用的是同一个常量）——测试全绿而功能已坏。
    /// 所以这里直接断言常量的**值**，让改动必须同时改到这里。
    #[test]
    fn cdn_marker_matches_the_remote_crate() {
        assert_eq!(CDN_REDIRECT, "telegram cdn redirect");
    }
    use super::*;

    /// 错误码要能区分认证、限流与网络，而不是一律「失败」。
    ///
    /// 不这样会怎样：界面只能显示「操作失败」，而这三种情况用户
    /// 该做的事完全不同——重新登录、等一会、检查网络。
    #[test]
    fn error_codes_are_distinguishable() {
        assert_eq!(
            to_cmd_err(&RemoteError::Unauthorized).code,
            "remote_unauthorized"
        );
        assert_eq!(
            to_cmd_err(&RemoteError::RateLimited).code,
            "remote_rate_limited"
        );
        assert_eq!(
            to_cmd_err(&RemoteError::Network(String::from("x"))).code,
            "remote_network"
        );
        assert_eq!(
            to_cmd_err(&RemoteError::NotFound(String::from("a"))).code,
            "remote_not_found"
        );
    }

    /// 序列化字段名是前端契约，改名等于改协议。
    ///
    /// 不这样会怎样：前端读 `probe_failed` 得到 undefined，
    /// 网络失败的条目会被当成「密码不对」显示，用户去反复试密码。
    #[test]
    fn entry_fields_are_stable() {
        let e = RemoteEntry {
            id: String::from("/a.omy"),
            name: String::from("a.omy"),
            is_dir: false,
            size: Some(1000),
            mtime: Some(1_700_000_000),
            is_encrypted: true,
            unlocked: false,
            real_name: None,
            plaintext_size: None,
            probe_failed: true,
            thumb_token: None,
            probing: false,
            media_tab: None,
        };
        let j = serde_json::to_value(&e).expect("序列化");
        for k in [
            "id",
            "name",
            "is_dir",
            "size",
            "mtime",
            "is_encrypted",
            "unlocked",
            "real_name",
            "plaintext_size",
            "probe_failed",
            "thumb_token",
            "probing",
            "media_tab",
        ] {
            assert!(j.get(k).is_some(), "字段 {k} 不能改名或缺失");
        }
    }

    /// 非 omy 数据必须返回 None，而不是误判成加密文件。
    ///
    /// 不这样会怎样：普通文件被标成加密文件，界面给出解密入口，
    /// 点下去必然失败。
    #[test]
    fn non_omy_bytes_are_not_encrypted() {
        let state: crate::commands::Shared = std::sync::Arc::new(crate::state::AppState::new());
        let junk = vec![0x41u8; 600];
        assert!(probe_omy(&junk, &state).is_none(), "随机数据不该被当成 omy");
        // 太短的数据同样不该误判
        assert!(probe_omy(b"OMY", &state).is_none());
    }

    /// 记录每次 `read_range` 的假存储，用来数「到底发了几次请求」。
    ///
    /// 缓存这类改动只有数请求次数才验得出来：断言「返回的字节对不对」的话，
    /// 一个完全不接缓存的实现照样全绿。
    struct CountingStore {
        data: Vec<u8>,
        calls: std::sync::Mutex<u32>,
    }

    impl CountingStore {
        fn new(data: Vec<u8>) -> Self {
            Self {
                data,
                calls: std::sync::Mutex::new(0),
            }
        }
        fn calls(&self) -> u32 {
            self.calls.lock().map(|c| *c).unwrap_or(u32::MAX)
        }
    }

    impl omy_remote::RemoteStore for CountingStore {
        fn capabilities(&self) -> omy_remote::Capabilities {
            omy_remote::Capabilities::read_only()
        }
        fn describe(&self) -> String {
            String::from("counting")
        }
        async fn list(&self, _dir_id: &str) -> Result<Vec<omy_remote::store::Entry>, RemoteError> {
            Ok(Vec::new())
        }
        async fn read_range(
            &self,
            _id: &str,
            offset: u64,
            len: u64,
        ) -> Result<Vec<u8>, RemoteError> {
            if let Ok(mut c) = self.calls.lock() {
                *c = c.saturating_add(1);
            }
            let s = usize::try_from(offset).unwrap_or(usize::MAX);
            let e = usize::try_from(offset.saturating_add(len)).unwrap_or(usize::MAX);
            Ok(self
                .data
                .get(s..e.min(self.data.len()))
                .unwrap_or(&[])
                .to_vec())
        }
    }

    /// 造一个临时缓存目录，返回（缓存, 目录路径）。
    fn temp_cache(tag: &str) -> (omy_remote::cache::BlockCache, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("omy_hdrcache_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let c = match omy_remote::cache::BlockCache::new(&dir, 0) {
            Ok(c) => c,
            Err(e) => unreachable!("建缓存失败：{e}"),
        };
        (c, dir)
    }

    fn rt() -> tokio::runtime::Runtime {
        match tokio::runtime::Builder::new_current_thread().build() {
            Ok(r) => r,
            Err(e) => unreachable!("建运行时失败：{e}"),
        }
    }

    /// 同一个文件头读第二次**不能**再发网络请求。
    ///
    /// 不这样会怎样：这正是用户报的「每次进来都要重新加载」。实测识别一个
    /// 头部约 1.5 秒且第二次一样慢，十几个文件就是十几秒，每次进对话重来
    /// 一遍。原先 `fetch_full_header` 直连 `store.read_range`，绕过了播放
    /// 路径在用的 `BlockCache`。
    ///
    /// 这条断言数的是**请求次数**而不是返回内容：只断言「读回来的字节对」
    /// 的话，一个把缓存整个摘掉的实现照样通过——变异测试确认过，那样的断言
    /// 等于没写。
    #[test]
    fn a_second_header_read_does_not_hit_the_network() {
        let store = CountingStore::new(vec![0x7Au8; 4096]);
        let (cache, dir) = temp_cache("hit");
        let r = rt();

        let first = r
            .block_on(fetch_full_header(
                &store,
                "/a.omy",
                4096,
                Some(&cache),
                "p1",
                None,
            ))
            .unwrap_or_default();
        let after_first = store.calls();
        assert!(after_first > 0, "首次必须真的发请求");
        assert!(!first.is_empty(), "首次要读到内容");

        let second = r
            .block_on(fetch_full_header(
                &store,
                "/a.omy",
                4096,
                Some(&cache),
                "p1",
                None,
            ))
            .unwrap_or_default();
        assert_eq!(
            store.calls(),
            after_first,
            "第二次读同一个头部不该再发请求——缓存没接上"
        );
        assert_eq!(second, first, "缓存命中返回的内容必须与首次一致");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 文件被覆盖更新（长度变了）后，**不能**返回旧头部。
    ///
    /// 不这样会怎样：缓存键若不含 size，云端换了文件之后界面仍然显示上一版的
    /// 识别结果——文件名、是否加密、明文大小全是旧的，而且因为一直命中缓存
    /// 永远不会自愈。
    #[test]
    fn a_changed_size_invalidates_the_cached_header() {
        let old = CountingStore::new(vec![0x11u8; 4096]);
        let new = CountingStore::new(vec![0x22u8; 8192]);
        let (cache, dir) = temp_cache("size");
        let r = rt();

        let a = r
            .block_on(fetch_full_header(
                &old,
                "/a.omy",
                4096,
                Some(&cache),
                "p1",
                None,
            ))
            .unwrap_or_default();
        // 同一个 id、同一个位置，但文件长度变了
        let b = r
            .block_on(fetch_full_header(
                &new,
                "/a.omy",
                8192,
                Some(&cache),
                "p1",
                None,
            ))
            .unwrap_or_default();

        assert!(new.calls() > 0, "长度变了必须重新请求，不能吃旧缓存");
        assert_ne!(a, b, "返回的必须是新文件的头部");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 不同位置的同名条目不能互相串。
    ///
    /// 不这样会怎样：两个 Telegram 账号里各有一条 `tg:123:456`，缓存键若不含
    /// 位置 id，A 账号会显示 B 账号那个文件的识别结果——而两边看起来都「正常」，
    /// 没有任何报错。
    #[test]
    fn different_places_do_not_share_header_cache() {
        let a_store = CountingStore::new(vec![0x33u8; 4096]);
        let b_store = CountingStore::new(vec![0x44u8; 4096]);
        let (cache, dir) = temp_cache("place");
        let r = rt();

        let a = r
            .block_on(fetch_full_header(
                &a_store,
                "tg:1:2",
                4096,
                Some(&cache),
                "p1",
                None,
            ))
            .unwrap_or_default();
        let b = r
            .block_on(fetch_full_header(
                &b_store,
                "tg:1:2",
                4096,
                Some(&cache),
                "p2",
                None,
            ))
            .unwrap_or_default();

        assert!(
            b_store.calls() > 0,
            "另一个位置必须自己去取，不能命中前一个的缓存"
        );
        assert_ne!(a, b, "不同位置的同名条目内容不能串");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 头部键与正文块键不能撞。
    ///
    /// 不这样会怎样：头部只有几百字节、正文块是 1 MiB，两者共用一个键的话，
    /// 播放时会把一段 480 B 的头部当成第 0 个正文块拿去解密，得到认证失败——
    /// 而那个症状指向密钥，完全指不到缓存键。
    #[test]
    fn header_keys_never_collide_with_payload_keys() {
        use omy_remote::header_cache_key as key;
        let k = key("/a.omy", 4096);
        // 正文键的两种形态（见 RemoteSource）：`<id>\u{1}<hex哈希>` 与
        // `<id>\u{1}plain<size>`。头部键必须与它们都不同
        assert_ne!(k, "/a.omy\u{1}plain4096");
        assert!(k.contains("\u{1}hdr"), "头部键要有自己的前缀：{k}");
        // size 必须真的进键里，否则覆盖更新后旧头部不会失效
        assert_ne!(
            key("/a.omy", 4096),
            key("/a.omy", 8192),
            "长度不同必须得到不同的键"
        );
    }

    /// 没有缓存时要照常工作（缓存目录建不起来是既有的降级路径）。
    ///
    /// 不这样会怎样：把「没有缓存」写成错误的话，一台缓存目录不可写的机器上
    /// 整个远程浏览都会瘫掉，而它本来只该慢一点。
    #[test]
    fn header_read_works_without_a_cache() {
        let store = CountingStore::new(vec![0x55u8; 4096]);
        let r = rt();
        let out = r
            .block_on(fetch_full_header(&store, "/a.omy", 4096, None, "", None))
            .unwrap_or_default();
        assert!(!out.is_empty(), "没有缓存也要能读到头部");
        assert!(store.calls() > 0);
    }
}
