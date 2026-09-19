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
        }
    }
}

impl From<WebDavStore> for PlaceStore {
    fn from(s: WebDavStore) -> Self {
        Self::WebDav(s)
    }
}

impl RemoteStore for PlaceStore {
    fn capabilities(&self) -> Capabilities {
        match self {
            Self::WebDav(s) => s.capabilities(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::WebDav(s) => s.describe(),
        }
    }

    async fn list(&self, dir_id: &str) -> Result<Vec<Entry>> {
        match self {
            Self::WebDav(s) => s.list(dir_id).await,
        }
    }

    async fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        match self {
            Self::WebDav(s) => s.read_range(id, offset, len).await,
        }
    }

    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> Result<Entry> {
        match self {
            Self::WebDav(s) => s.write(dir_id, name, data).await,
        }
    }

    async fn delete(&self, id: &str) -> Result<()> {
        match self {
            Self::WebDav(s) => s.delete(id).await,
        }
    }

    async fn rename(&self, id: &str, new_name: &str) -> Result<()> {
        match self {
            Self::WebDav(s) => s.rename(id, new_name).await,
        }
    }

    async fn create_dir(&self, parent_id: &str, name: &str) -> Result<Entry> {
        match self {
            Self::WebDav(s) => s.create_dir(parent_id, name).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webdav::WebDavConfig;

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
    }

    /// 只读位置的写操作要在发请求之前就被拒绝。
    ///
    /// 不这样会怎样：外壳把调用透传下去，能力校验只剩服务端那一道——
    /// 而那意味着先把数据发出去再被拒绝。
    #[test]
    fn readonly_writes_are_refused_through_the_wrapper() {
        let s = webdav(false);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("建运行时");
        assert!(rt.block_on(s.delete("/a")).is_err());
        assert!(rt.block_on(s.write("/", "a", b"1")).is_err());
        assert!(rt.block_on(s.rename("/a", "b")).is_err());
        assert!(rt.block_on(s.create_dir("/", "d")).is_err());
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
}
