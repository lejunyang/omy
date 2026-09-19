//! [`PlaceStore`]：把各 provider 收成一个具体类型，供 GUI 持有。
//!
//! # 为什么需要它——GUI 层其实单态化到了 WebDAV
//!
//! [`RemoteStore`] 虽然是 trait，但 GUI 一侧把类型写死成了 `WebDavStore`：
//! `Place.store` 是 `Arc<WebDavStore>`、`OpenPlaceFile.source` 是
//! `RemoteSource<WebDavStore>`、若干函数签名直接收 `&WebDavStore`。
//! 也就是说抽象在 `omy-remote` 里成立，到了应用层就塌回一个实现上。
//!
//! 不打开这一层，加第二个 provider 只剩两条路：把它的代码塞进 `WebDavStore`，
//! 或者在 GUI 里拉一条平行的命令链路。后者正是 AGENTS.md「同一逻辑不允许两处
//! 实现」要拦的东西——两条链路的能力校验、错误映射、缓存键迟早走岔。
//!
//! # 为什么是 enum 而不是 `Arc<dyn RemoteStore>`
//!
//! [`RemoteStore`] 用的是 `#[allow(async_fn_in_trait)]` 的原生 async fn，
//! **不是 dyn-safe 的**。改成 `dyn` 要么让每个方法返回 `BoxFuture`、要么引入
//! `async-trait`：前者把签名搞得很难读，后者给每次调用加一次堆分配，而且是为
//! 一个「可预见的将来只有个位数实现」的场景付这个代价。
//!
//! enum 的代价是明确的、可写进注释的：**新增 provider 要改这里**。用 `dyn` 则
//! 是把这份成本换成了运行期的间接调用与一个新依赖。
//!
//! # 新增 provider 时要改的三处
//!
//! 1. 本文件的枚举成员与下面 `impl RemoteStore` 里的每个 `match`；
//! 2. [`crate::Capabilities`] 里对应的构造器（能力要如实声明，别套用现成的）；
//! 3. GUI 的位置注册与配置里的 `kind` 字符串。
//!
//! 漏掉第 1 处编译器会报错（`match` 不穷尽），漏掉第 2、3 处不会——所以那两处
//! 各自的注释里也标了这条。

use crate::store::{Entry, RemoteStore};
use crate::telegram::TelegramStore;
use crate::webdav::WebDavStore;
use crate::{Capabilities, Result};

/// 一个远程位置的驱动实例。
///
/// 转发给具体 provider，自己不含任何逻辑。**这里不要写「哪个 provider 该怎样」
/// 的判断**：那种判断会与 provider 自己的实现形成第二处真相，而症状是「改了
/// 驱动却没生效」。
#[derive(Debug)]
pub enum PlaceStore {
    /// WebDAV / NAS / 被中转出来的云盘。
    WebDav(WebDavStore),
    /// Telegram：对话即目录。
    ///
    /// 这是第一个**能力随目录而变**的 provider——同一个账号里，自己的收藏夹
    /// 可写、别人的频道只读。以前这一位上放的是个假驱动（`VaryingForTest`），
    /// 因为那时没有真的；现在有了，那个假的已经删掉：用一个与产品不同源的
    /// 东西去验证产品，通过了也说明不了产品对。
    Telegram(TelegramStore),
}

impl PlaceStore {
    /// 驱动类型的稳定标识，与配置里的 `kind` 字段、前端显示用的图标对应。
    ///
    /// 返回 `&'static str` 而不是枚举：它要被写进配置文件并读回来，
    /// 是持久化格式的一部分，**改动这些字面量等于改配置格式**。
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::WebDav(_) => "webdav",
            Self::Telegram(_) => "telegram",
        }
    }

    /// 取 WebDAV 驱动；不是 WebDAV 位置时返回 `None`。
    ///
    /// 留这个口子是为了 WebDAV **特有**的东西（回填编辑表单要读 `config()`）。
    /// 凡是 `RemoteStore` 已经表达的能力都不要走这里——那等于绕过抽象，
    /// 加第二个 provider 时又会漏掉一处。
    #[must_use]
    pub const fn as_webdav(&self) -> Option<&WebDavStore> {
        match self {
            Self::WebDav(s) => Some(s),
            Self::Telegram(_) => None,
        }
    }
}

impl From<WebDavStore> for PlaceStore {
    fn from(s: WebDavStore) -> Self {
        Self::WebDav(s)
    }
}

impl From<TelegramStore> for PlaceStore {
    fn from(s: TelegramStore) -> Self {
        Self::Telegram(s)
    }
}

impl RemoteStore for PlaceStore {
    fn capabilities(&self) -> Capabilities {
        match self {
            Self::WebDav(s) => s.capabilities(),
            Self::Telegram(s) => s.capabilities(),
        }
    }

    /// 必须转发，不能用 trait 的默认实现。
    ///
    /// 默认实现返回位置级能力，而这个外壳一旦包住一个「能力随目录而变」的驱动，
    /// 不转发就等于把目录级的收窄整个吞掉——症状是进只读对话后删除菜单照样是
    /// 亮的，点了才报错。**新增 provider 时这一支也要跟着加。**
    async fn effective_capabilities(&self, dir_id: &str) -> Result<Capabilities> {
        match self {
            Self::WebDav(s) => s.effective_capabilities(dir_id).await,
            Self::Telegram(s) => s.effective_capabilities(dir_id).await,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::WebDav(s) => s.describe(),
            Self::Telegram(s) => s.describe(),
        }
    }

    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>> {
        match self {
            Self::WebDav(s) => s.list(dir_id).await,
            Self::Telegram(s) => s.list(dir_id).await,
        }
    }

    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        match self {
            Self::WebDav(s) => s.read_range(id, offset, len).await,
            Self::Telegram(s) => s.read_range(id, offset, len).await,
        }
    }

    /// 必须转发：不转发的话 Telegram 的服务端搜索会落回 trait 默认实现，
    /// 变成「支持搜索但一搜就报不支持」——而能力位图说它支持。
    async fn search(&self, dir_id: &str, query: &str, limit: usize) -> Result<Vec<Entry>> {
        match self {
            Self::WebDav(s) => s.search(dir_id, query, limit).await,
            Self::Telegram(s) => s.search(dir_id, query, limit).await,
        }
    }

    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> Result<Entry> {
        match self {
            Self::WebDav(s) => s.write(dir_id, name, data).await,
            Self::Telegram(s) => s.write(dir_id, name, data).await,
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        match self {
            Self::WebDav(s) => s.delete(id).await,
            Self::Telegram(s) => s.delete(id).await,
        }
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
        match self {
            Self::WebDav(s) => s.rename(id, new_name).await,
            Self::Telegram(s) => s.rename(id, new_name).await,
        }
    }

    async fn create_dir(&self, parent_id: &str, name: &str) -> Result<Entry> {
        match self {
            Self::WebDav(s) => s.create_dir(parent_id, name).await,
            Self::Telegram(s) => s.create_dir(parent_id, name).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webdav::WebDavConfig;

    /// 造一个**能力随目录而变**的真实驱动，用来测「外壳有没有转发」。
    ///
    /// 以前这里有一个手写的假驱动，那是在 Telegram 驱动还不存在时的临时替代。
    /// 现在 `TelegramStore` 就是真的按对话给能力，于是那份假实现成了同一逻辑的
    /// 第二处实现——AGENTS.md 明禁，且它还会慢慢和真实行为漂移，
    /// 到时候测过的东西和跑的东西就不是一回事了。
    ///
    /// 仍然需要**某个**能力随目录而变的驱动：WebDAV 各目录能力一致，
    /// 转发与落回 trait 默认实现的结果完全相同，缺陷从外部根本看不出来。
    fn varying() -> PlaceStore {
        use crate::telegram::{Conversation, TelegramStore};
        PlaceStore::Telegram(TelegramStore::with_conversations(vec![
            Conversation {
                chat: 1,
                title: String::from("收藏夹"),
                can_send: true,
                can_delete: true,
                broadcast: false,
            },
            Conversation {
                chat: 2,
                title: String::from("别人的频道"),
                can_send: false,
                can_delete: false,
                broadcast: true,
            },
        ]))
    }

    /// 可写的那个目录 id（自己的收藏夹）。
    const WRITABLE_DIR: &str = "tg:1";
    /// 只读的那个目录 id（别人的频道）。
    const READONLY_DIR: &str = "tg:2";

    fn webdav(writable: bool) -> PlaceStore {
        PlaceStore::WebDav(
            WebDavStore::new(WebDavConfig {
                base_url: String::from("https://dav.example.com/dav"),
                username: String::from("u"),
                // 用一个不会偶然出现在 URL 或结构体名里的串，
                // 否则下面那条「不泄露密码」的断言会因为撞上 https 里的字母而
                // 假通过或假失败
                password: String::from("pw-CANARY-7f31"),
                writable,
                ..WebDavConfig::default()
            })
            .expect("构造 WebDAV 驱动"),
        )
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时")
    }

    /// 转发必须保真，不能在这一层改写能力。
    ///
    /// 不这样会怎样：外壳与驱动各说一套，界面读到的能力与真实行为不符——
    /// 而 `caps.rs` 已经写明能力位图是唯一真相，这里再加工一次就是第二处真相。
    #[test]
    fn capabilities_are_forwarded_verbatim() {
        let ro = webdav(false);
        let rw = webdav(true);
        assert!(!ro.capabilities().any_write(), "只读位置不能被外壳改成可写");
        assert!(rw.capabilities().any_write());
        // 与直接问驱动的结果必须一致
        let direct = ro.as_webdav().expect("是 WebDAV").capabilities();
        assert_eq!(ro.capabilities(), direct, "外壳不能加工能力位图");
    }

    /// `kind` 是持久化格式的一部分，不能随手改。
    ///
    /// 不这样会怎样：配置里存着 `webdav`，改了这个字面量之后读回来对不上，
    /// 用户的已保存位置会变成一条恢复不出来的记录——而这不会有任何报错。
    #[test]
    fn kind_strings_are_stable() {
        assert_eq!(webdav(false).kind(), "webdav");
        // Telegram 的 kind 同样是持久化格式：配置里存着它，改了就恢复不出来
        assert_eq!(varying().kind(), "telegram");
    }

    /// 只读位置的写操作要在发请求之前就被拒绝。
    ///
    /// 不这样会怎样：外壳把调用透传下去，能力校验只剩服务端那一道——
    /// 而那意味着先把数据发出去再被拒绝。
    #[test]
    fn readonly_writes_are_refused_through_the_wrapper() {
        let s = webdav(false);
        let r = rt();
        assert!(r.block_on(s.delete("/a")).is_err());
        assert!(r.block_on(s.write("/", "a", b"1")).is_err());
        assert!(r.block_on(s.rename("/a", "b")).is_err());
        assert!(r.block_on(s.create_dir("/", "d")).is_err());
    }

    /// Debug 输出不能带出密码。
    ///
    /// 不这样会怎样：一次 `dbg!` 或错误日志就把用户的网盘密码写进日志文件。
    /// `WebDavStore` 自己有这道保证，包一层之后必须仍然成立——派生 Debug 的
    /// 外壳会调用成员的 Debug，所以这条是在验证「没人给 WebDavStore 换成
    /// 派生实现」。
    #[test]
    fn debug_does_not_leak_password() {
        let s = webdav(true);
        let text = format!("{s:?}");
        assert!(
            !text.contains("pw-CANARY-7f31"),
            "Debug 里不能出现密码：{text}"
        );
    }

    /// 外壳不能在有效能力这条路径上改写能力（WebDAV 各目录能力相同）。
    #[test]
    fn effective_capabilities_are_not_rewritten() {
        let ro = webdav(false);
        let eff = rt()
            .block_on(ro.effective_capabilities("/some/dir"))
            .expect("WebDAV 的默认实现不发请求");
        assert_eq!(eff, ro.capabilities(), "外壳不能在这条路径上改写能力");
        assert!(!eff.any_write(), "只读位置的有效能力也必须没有写");
    }

    /// **外壳必须把 `effective_capabilities` 转发给驱动**。
    ///
    /// 不这样会怎样：外壳落回 trait 的默认实现，把目录级的收窄整个吞掉——
    /// 进别人的只读频道后「删除」「上传」照样是亮的，用户点了才收到服务端拒绝。
    /// 而写操作在点下去之前就该知道做不做得到。
    ///
    /// 这条断言必须用一个**能力随目录而变**的驱动才有效：WebDAV 各目录一致，
    /// 转发与不转发结果相同，从外部区分不了（上一版就因此是个测不到的缺口）。
    #[test]
    fn effective_capabilities_are_forwarded_per_dir() {
        let s = varying();
        let r = rt();

        // 位置级是上界：它说「这个位置能写」
        assert!(s.capabilities().any_write(), "位置级上界应当是可写的");

        let writable = r
            .block_on(s.effective_capabilities(WRITABLE_DIR))
            .expect("取有效能力");
        let readonly = r
            .block_on(s.effective_capabilities(READONLY_DIR))
            .expect("取有效能力");

        // 要害：两个目录必须给出**不同**的结果。外壳一旦不转发，两者都会等于
        // 位置级能力，这条断言立刻失败
        assert_ne!(
            writable, readonly,
            "同一位置的两个目录必须能给出不同的有效能力——相同说明外壳没把调用\
             转发给驱动，目录级的收窄被吞掉了"
        );
        assert!(writable.write, "自己的收藏夹应当可写");
        assert!(writable.delete);
        assert!(!readonly.any_write(), "别人的频道不能有任何写能力");
        assert!(readonly.read, "只读频道仍然能看");
        assert!(readonly.search, "搜索是读能力，只读频道也该有");

        // 只读那一份必须真的窄于位置级上界，而不是被原样透传
        assert_ne!(
            readonly,
            s.capabilities(),
            "只读目录的有效能力若等于位置级上界，等于收窄没生效"
        );
    }
}
