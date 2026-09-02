//! 应用状态：会话密钥、扫描结果、已打开的文件。
//!
//! # 设计要点：解锁状态是数据，不是视图
//!
//! `docs/research/08-ui-ux-design.md` §10 从原型验证中得出一条结论：
//! 锁定态**不能**用「对 DOM 做可逆的样式覆盖」实现。原因是有些文件
//! 本来就因为密码不匹配而处于锁定态，若解锁时统一重设为「正常」，
//! 这些文件会被错误地显示成已解锁——这是信息泄露。
//!
//! 所以这里把每个文件的 `unlocked` 做成**数据字段**（来自 core 的
//! [`UnlockOutcome`]），前端只做无状态渲染。锁定时后端直接把
//! 文件列表清空并抹掉密钥，前端拿到的就是空列表，没有「还原」一说。
//!
//! # 为什么明文不进 State
//!
//! 这里只保存密钥与元信息，**不缓存任何解密后的明文**。
//! 明文只在协议处理器里按需产生、随响应发走、立即丢弃。
//! 用户要求「缓存放内存」（决策 D-21）指的是解密块缓存，
//! 那是 `omy-core` 内部的事，不是 GUI 层持有整个文件。

use omy_core::scan::{ScanHit, UnlockOutcome};
use omy_core::session::SessionKeys;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 一个已被识别的 `.omy` 文件在 UI 中的表示。
///
/// 字段刻意做成扁平的可序列化结构，而不是直接把 [`ScanHit`] 丢给前端：
/// `ScanHit` 里含 `FixedHeader`，其中有盐、slot 等密码学材料，
/// 没有任何理由送进 WebView。
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileEntry {
    /// 稳定标识，前端用它请求内容。
    pub id: String,
    /// 磁盘路径（用于「在文件管理器中显示」等操作）。
    pub path: String,
    /// 展示用名称：解锁则为真实文件名，否则为占位。
    pub name: String,
    /// **是否已解锁**——渲染的唯一依据。
    pub unlocked: bool,
    /// 明文大小；锁定时为 `None`，前端显示「密码未解锁」。
    ///
    /// 不能在锁定时返回密文大小：文档 §10 指出文件大小配合数量
    /// 仍可推断库内容规模，属于信息泄露。
    pub size: Option<u64>,
    /// 磁盘上的密文大小，任何时候都可见（它本来就藏不住）。
    pub encrypted_size: u64,
    /// 媒体类别，决定用哪个预览器。锁定时为 `None`。
    pub kind: Option<String>,
    /// MIME，供 `<video>` / `<img>` 使用。锁定时为 `None`。
    pub mime: Option<String>,
    /// 播放分级 `p1` / `p2` / `p3`，对应 UI 的 ⚡🔄🐌。
    pub tier: Option<String>,
    /// 时长（毫秒），音视频才有。
    pub duration_ms: Option<u64>,
    /// 画面尺寸。
    pub width: Option<u32>,
    /// 画面尺寸。
    pub height: Option<u32>,
    /// 是否带加密缩略图。
    pub has_thumbnail: bool,
    /// 该图片格式 WebView 认不认；`true` 表示需要先转码再显示。
    ///
    /// 前端据此决定请求原图还是转码版本。不能让前端自己按后缀猜——
    /// 判定规则会随浏览器支持情况变化，散在两处早晚会不一致。
    pub needs_transcode: bool,
    /// 命中的凭据名称，用于状态栏显示「N 个密码已解锁」。
    pub credential: Option<String>,
    /// 是否是目录容器（一整个文件夹被打包进单个 .omy）。
    ///
    /// 前端据此把它显示成文件夹图标、双击进入而不是尝试预览——
    /// 容器的载荷是多个文件拼接，当成单个文件预览只会得到一堆乱码。
    /// 锁定时恒为 `false`：这个事实本身也属于内容信息。
    pub is_container: bool,
}

impl FileEntry {
    /// 由 core 的扫描命中构造 UI 条目。
    ///
    /// 媒体元信息要等真正打开文件才拿得到（在 TLV 里），
    /// 扫描阶段只填基础字段，媒体字段留空由 `enrich` 补。
    pub fn from_hit(id: String, hit: &ScanHit) -> Self {
        let (unlocked, name, size, credential) = match &hit.unlock {
            UnlockOutcome::Unlocked {
                filename,
                plaintext_size,
                credential,
                ..
            } => {
                // 文件名可能没加密（用户选了保留原名），
                // 也可能加密了但 TLV 读不出——两种都退回磁盘文件名。
                let display = filename.clone().unwrap_or_else(|| {
                    Path::new(&hit.path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| String::from("?"))
                });
                (
                    true,
                    display,
                    Some(*plaintext_size),
                    Some(credential.clone()),
                )
            }
            // 打不开：绝不透露任何内容信息。
            // 名称用固定占位而非磁盘文件名——磁盘文件名本身
            // 可能就是「机密报告.omy」这种泄露信息的名字。
            UnlockOutcome::Locked => (false, String::new(), None, None),
        };

        Self {
            id,
            path: hit.path.to_string_lossy().into_owned(),
            name,
            unlocked,
            size,
            encrypted_size: hit.file_size,
            kind: None,
            mime: None,
            tier: None,
            duration_ms: None,
            width: None,
            height: None,
            has_thumbnail: false,
            needs_transcode: false,
            credential,
            // 容器标志就在文件头的 flags 里，扫描时已经解析过头部，
            // 读它不需要再打开文件。
            //
            // 不能等 `enrich_file` 再填：那个只在预览时才调用，而
            // 「这是不是个文件夹」在**列表刚出来时**就要知道——
            // 否则用户双击会走进单文件预览，看到一堆拼接的字节。
            //
            // 锁定的文件恒为 false：`unlocked` 为假时前端不显示任何
            // 内容信息，「这是个文件夹」也属于内容信息
            is_container: unlocked
                && hit.header.has_flag(omy_core::header::flags::CONTAINER),
        }
    }
}

/// 全局应用状态。
pub struct AppState {
    inner: Mutex<Inner>,
    /// 未加密文件的访问登记表。
    ///
    /// 单独放在 `Mutex` 之外是有意的：协议层每次请求都要查它，
    /// 而那条路径上不需要碰会话密钥。共用一把锁会让预览请求
    /// 和 Argon2 派生互相等待——大目录里滚动缩略图时很明显。
    pub plain: crate::plain::PlainRegistry,
    /// 容器内文件的访问登记表。
    ///
    /// 与 `plain` 同理放在锁外：协议层每次请求容器内文件都要查它。
    /// 表里只有「所属容器 id + 载荷区间」，没有密钥也没有路径——
    /// 授权判断由协议层拿 id 回查会话完成，见 [`crate::citem`]。
    pub citem: crate::citem::ContainerRegistry,
}

/// 受锁保护的内部状态。
struct Inner {
    /// 已解锁的凭据。锁定时整体清空。
    session: SessionKeys,
    /// 当前列出的文件，按 id 索引。
    files: HashMap<String, FileEntry>,
    /// 显示顺序（`HashMap` 无序，前端要稳定顺序）。
    order: Vec<String>,
    /// 已加入浏览的目录。
    roots: Vec<PathBuf>,
    /// 界面语言，`zh-CN` 或 `en`。
    lang: String,
}

impl AppState {
    /// 创建空状态（锁定态）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                session: SessionKeys::new(),
                files: HashMap::new(),
                order: Vec::new(),
                roots: Vec::new(),
                lang: detect_language(),
            }),
            plain: crate::plain::PlainRegistry::new(),
            citem: crate::citem::ContainerRegistry::new(),
        }
    }

    /// 在会话上执行操作。
    ///
    /// 用闭包而非返回 `MutexGuard`，避免调用方持锁期间做耗时操作
    /// （比如 Argon2 派生或磁盘扫描）把 UI 卡住。
    ///
    /// 锁中毒时返回 `None`：某个线程 panic 过，状态可能已不一致，
    /// 此时继续用这份数据不如让调用方明确处理。
    pub fn with_session<T>(&self, f: impl FnOnce(&mut SessionKeys) -> T) -> Option<T> {
        self.inner.lock().ok().map(|mut g| f(&mut g.session))
    }

    /// 是否有任何已解锁的凭据。
    pub fn is_unlocked(&self) -> bool {
        self.inner
            .lock()
            .map(|g| !g.session.is_empty())
            .unwrap_or(false)
    }

    /// 已解锁的凭据数量，状态栏显示用。
    pub fn credential_count(&self) -> usize {
        self.inner.lock().map(|g| g.session.len()).unwrap_or(0)
    }

    /// 替换文件列表。
    pub fn set_files(&self, entries: Vec<FileEntry>) {
        if let Ok(mut g) = self.inner.lock() {
            g.order = entries.iter().map(|e| e.id.clone()).collect();
            g.files = entries.into_iter().map(|e| (e.id.clone(), e)).collect();
        }
    }

    /// 按显示顺序取出所有条目。
    pub fn files(&self) -> Vec<FileEntry> {
        self.inner
            .lock()
            .map(|g| {
                g.order
                    .iter()
                    .filter_map(|id| g.files.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 按 id 取单个条目。
    pub fn file(&self, id: &str) -> Option<FileEntry> {
        self.inner.lock().ok().and_then(|g| g.files.get(id).cloned())
    }

    /// 就地更新某个条目（用于补充媒体元信息）。
    pub fn update_file(&self, id: &str, f: impl FnOnce(&mut FileEntry)) {
        if let Ok(mut g) = self.inner.lock()
            && let Some(e) = g.files.get_mut(id)
        {
            f(e);
        }
    }

    /// 记录一个浏览目录。
    pub fn add_root(&self, p: PathBuf) {
        if let Ok(mut g) = self.inner.lock()
            && !g.roots.contains(&p)
        {
            g.roots.push(p);
        }
    }

    /// 当前所有浏览目录。
    pub fn roots(&self) -> Vec<PathBuf> {
        self.inner.lock().map(|g| g.roots.clone()).unwrap_or_default()
    }

    /// 锁定：抹掉密钥并清空文件列表。
    ///
    /// 必须**同时**清空文件列表。只抹密钥的话，前端还留着上一次的
    /// 文件名和缩略图，锁定就成了假象。
    pub fn lock(&self) {
        if let Ok(mut g) = self.inner.lock() {
            g.session.lock();
            g.files.clear();
            g.order.clear();
        }
        // 明文 token 也一并作废。这些文件本来就以明文躺在磁盘上，
        // 清不清都拦不住直接去磁盘打开的人；但留着一批可用 token
        // 与「锁定后什么都看不到」的预期不符
        self.plain.clear();
        // 容器内文件的 token 同样作废。这一条比明文那条更要紧：
        // 上面的 files.clear() 已经让协议层回查 entry id 时查不到，
        // 但两道防线里任何一道单独成立都不该被当成可以省掉另一道
        self.citem.clear();
    }

    /// 当前界面语言。
    pub fn lang(&self) -> String {
        self.inner
            .lock()
            .map(|g| g.lang.clone())
            .unwrap_or_else(|_| String::from("en"))
    }

    /// 设置界面语言。
    pub fn set_lang(&self, lang: String) {
        if let Ok(mut g) = self.inner.lock() {
            g.lang = lang;
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// 探测系统语言，返回 `zh-CN` 或 `en`。
///
/// 按文档 §7.1 的顺序：系统 locale → 英语兜底。
/// （用户手动设置在前端持久化，启动时会覆盖这里的结果。）
///
/// 首期只有简中和英文（决策 D-29），所以任何中文变体
/// —— `zh-TW` / `zh-HK` / `zh-Hans` —— 都归到 `zh-CN`。
/// 这比回退到英文更合理：繁体用户读简体远比读英文容易。
#[must_use]
pub fn detect_language() -> String {
    let raw = sys_locale::get_locale().unwrap_or_else(|| String::from("en"));
    normalize_language(&raw)
}

/// 把系统 locale 归一化到支持的语言标签。
#[must_use]
pub fn normalize_language(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("zh") {
        String::from("zh-CN")
    } else {
        String::from("en")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_normalization() {
        // 所有中文变体都归到简中——首期只有简中和英文，
        // 让繁体用户读简体好过让他们读英文
        for z in ["zh", "zh-CN", "zh-TW", "zh-HK", "zh-Hans", "ZH_CN", "zh_SG"] {
            assert_eq!(normalize_language(z), "zh-CN", "{z} 应归为简中");
        }
        // 其余一律英文
        for e in ["en", "en-US", "ja-JP", "de", "fr-FR", "", "xx"] {
            assert_eq!(normalize_language(e), "en", "{e} 应归为英文");
        }
    }

    #[test]
    fn new_state_is_locked() {
        let s = AppState::new();
        assert!(!s.is_unlocked(), "新建状态必须是锁定的");
        assert_eq!(s.credential_count(), 0);
        assert!(s.files().is_empty());
    }

    #[test]
    fn lock_clears_files_not_just_keys() {
        // 这条断言守护一个真实的信息泄露：
        // 若锁定只抹密钥而不清文件列表，前端会继续显示
        // 上一次的真实文件名——锁定就成了假象
        let s = AppState::new();
        s.set_files(vec![FileEntry {
            id: String::from("a"),
            path: String::from("/x/a.omy"),
            name: String::from("机密报告.docx"),
            unlocked: true,
            size: Some(1024),
            encrypted_size: 2048,
            kind: Some(String::from("document")),
            mime: None,
            tier: None,
            duration_ms: None,
            width: None,
            height: None,
            has_thumbnail: false,
            needs_transcode: false,
            credential: Some(String::from("main")),
            // 设成 true 顺带验证容器标记也随锁定一起消失
            is_container: true,
        }]);
        assert_eq!(s.files().len(), 1);

        s.lock();

        assert!(s.files().is_empty(), "锁定后文件列表必须清空");
        assert!(s.file("a").is_none(), "锁定后不能再按 id 取到条目");
        assert!(!s.is_unlocked());
    }

    /// 锁定必须让容器内文件的 token 一起失效。
    ///
    /// 这些 token 指向的是**解密后**的载荷区间，锁定的语义就是
    /// 「这些都不该再读得到」。协议层还会拿 entry id 回查一次解锁状态，
    /// 但两道防线里任何一道都不该因为另一道存在而省掉。
    #[test]
    fn lock_clears_container_tokens() {
        let s = AppState::new();
        let t = s
            .citem
            .register(crate::citem::ContainerRef {
                entry_id: String::from("a"),
                inner_path: String::from("secret/plan.txt"),
                offset: 0,
                size: 10,
                mime: String::from("text/plain"),
            })
            .unwrap_or_default();
        assert!(s.citem.resolve(&t).is_some());

        s.lock();

        assert!(
            s.citem.resolve(&t).is_none(),
            "锁定后容器内文件的 token 必须失效"
        );
    }

    #[test]
    fn file_order_is_stable() {
        // HashMap 无序，必须靠 order 维持前端看到的顺序
        let s = AppState::new();
        let mk = |id: &str| FileEntry {
            id: id.to_owned(),
            path: format!("/x/{id}"),
            name: id.to_owned(),
            unlocked: true,
            size: Some(1),
            encrypted_size: 1,
            kind: None,
            mime: None,
            tier: None,
            duration_ms: None,
            width: None,
            height: None,
            has_thumbnail: false,
            needs_transcode: false,
            credential: None,
            is_container: false,
        };
        s.set_files(vec![mk("z"), mk("a"), mk("m")]);
        let names: Vec<String> = s.files().into_iter().map(|e| e.id).collect();
        assert_eq!(names, vec!["z", "a", "m"], "顺序必须与插入一致");
    }
}
