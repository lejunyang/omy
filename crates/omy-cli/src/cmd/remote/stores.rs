//! 把 WebDAV 与 Telegram 两种驱动收成一个具体类型，供文件命令无分支复用。
//!
//! # 为什么是枚举分发而不是 `dyn RemoteStore`
//!
//! [`RemoteStore`] 用的是原生 `async fn`（无 `async_trait` 装箱），所以它
//! **不是对象安全**的——拿不到 `Box<dyn RemoteStore>`。而文件命令（ls/upload/
//! download/copy/cache）里 `store.list(...)`、`store.read_range(...)` 这些调用
//! 若要按「是 WebDAV 还是 Telegram」各写一遍，就是把 GUI/CLI 那套「同一业务
//! 层两处实现」的漂移问题原样复制一遍。
//!
//! 枚举分发让两个驱动都通过 [`RemoteStore`] 同一个虚表（静态分发的 match），
//! 文件命令只面对 [`AnyStore`] 一种类型。新增驱动时只需在这里加一个变体。

use anyhow::{Result, anyhow, bail};
use omy_config::SavedPlace;
use omy_remote::store::Entry;
use omy_remote::telegram::TelegramStore;
use omy_remote::webdav::WebDavStore;
use omy_remote::{Capabilities, RemoteStore, UploadMediaHint};
use tokio::io::AsyncRead;

/// 一个已连接、可做文件操作的远程驱动。
#[derive(Debug)]
pub(crate) enum AnyStore {
    /// WebDAV。
    Dav(WebDavStore),
    /// Telegram（对话即目录）。
    Tg(TelegramStore),
}

impl RemoteStore for AnyStore {
    fn capabilities(&self) -> Capabilities {
        match self {
            Self::Dav(s) => s.capabilities(),
            Self::Tg(s) => s.capabilities(),
        }
    }

    async fn effective_capabilities(&self, dir_id: &str) -> omy_remote::Result<Capabilities> {
        match self {
            Self::Dav(s) => s.effective_capabilities(dir_id).await,
            Self::Tg(s) => s.effective_capabilities(dir_id).await,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Dav(s) => s.describe(),
            Self::Tg(s) => s.describe(),
        }
    }

    async fn list(&self, dir_id: &str) -> omy_remote::Result<Vec<Entry>> {
        match self {
            Self::Dav(s) => s.list(dir_id).await,
            Self::Tg(s) => s.list(dir_id).await,
        }
    }

    async fn read_range(&self, id: &str, offset: u64, len: u64) -> omy_remote::Result<Vec<u8>> {
        match self {
            Self::Dav(s) => s.read_range(id, offset, len).await,
            Self::Tg(s) => s.read_range(id, offset, len).await,
        }
    }

    async fn search(&self, dir_id: &str, query: &str, limit: usize) -> omy_remote::Result<Vec<Entry>> {
        match self {
            Self::Dav(s) => s.search(dir_id, query, limit).await,
            Self::Tg(s) => s.search(dir_id, query, limit).await,
        }
    }

    fn child_id(&self, dir_id: &str, name: &str) -> Option<String> {
        match self {
            Self::Dav(s) => s.child_id(dir_id, name),
            Self::Tg(s) => s.child_id(dir_id, name),
        }
    }

    async fn upload_media_hint(&self, id: &str) -> omy_remote::Result<Option<UploadMediaHint>> {
        match self {
            Self::Dav(s) => s.upload_media_hint(id).await,
            Self::Tg(s) => s.upload_media_hint(id).await,
        }
    }

    async fn write(&self, dir_id: &str, name: &str, data: &[u8]) -> omy_remote::Result<Entry> {
        match self {
            Self::Dav(s) => s.write(dir_id, name, data).await,
            Self::Tg(s) => s.write(dir_id, name, data).await,
        }
    }

    async fn write_stream(
        &self,
        dir_id: &str,
        name: &str,
        size: u64,
        reader: Box<dyn AsyncRead + Unpin + Send>,
    ) -> omy_remote::Result<Entry> {
        match self {
            Self::Dav(s) => s.write_stream(dir_id, name, size, reader).await,
            Self::Tg(s) => s.write_stream(dir_id, name, size, reader).await,
        }
    }

    async fn write_stream_with_hint(
        &self,
        dir_id: &str,
        name: &str,
        size: u64,
        reader: Box<dyn AsyncRead + Unpin + Send>,
        hint: Option<&UploadMediaHint>,
    ) -> omy_remote::Result<Entry> {
        match self {
            Self::Dav(s) => {
                s.write_stream_with_hint(dir_id, name, size, reader, hint).await
            }
            Self::Tg(s) => {
                s.write_stream_with_hint(dir_id, name, size, reader, hint).await
            }
        }
    }

    async fn delete(&self, id: &str) -> omy_remote::Result<()> {
        match self {
            Self::Dav(s) => s.delete(id).await,
            Self::Tg(s) => s.delete(id).await,
        }
    }

    async fn rename(&self, id: &str, new_name: &str) -> omy_remote::Result<()> {
        match self {
            Self::Dav(s) => s.rename(id, new_name).await,
            Self::Tg(s) => s.rename(id, new_name).await,
        }
    }

    async fn create_dir(&self, parent_id: &str, name: &str) -> omy_remote::Result<Entry> {
        match self {
            Self::Dav(s) => s.create_dir(parent_id, name).await,
            Self::Tg(s) => s.create_dir(parent_id, name).await,
        }
    }
}

/// 从一条配置记录连接出一个可用的驱动。
///
/// WebDAV 不发网络请求（构造即得，探活由调用方决定要不要列一次根目录）；
/// Telegram 必须真的连上去——它是长连接，断开就没有任何文件操作可做。
///
/// # Errors
///
/// 类型不认识、WebDAV 地址非法、或 Telegram 连不上 / 没有登录态时返回。
pub(crate) async fn connect_store(sp: &SavedPlace) -> Result<AnyStore> {
    match sp.kind.as_str() {
        "webdav" => connect_webdav(sp),
        "telegram" => connect_telegram(sp).await,
        other => bail!("位置 {} 是不支持的类型 {other:?}", sp.id),
    }
}

/// 构造 WebDAV 驱动。与旧 `build_store` 同语义：密码解不开要明确报「需重新登录」。
fn connect_webdav(sp: &SavedPlace) -> Result<AnyStore> {
    let key = omy_remote::placebook::protect_key();
    let Some((wcfg, need_login)) = omy_remote::placebook::saved_to_webdav(sp, key.as_ref()) else {
        bail!("位置 {} 不是 WebDAV 记录", sp.id);
    };
    if need_login {
        bail!(
            "位置 {} 的密码无法从本机凭据库解开（换过机器或凭据库不可用），请重新添加",
            sp.id
        );
    }
    if wcfg.base_url.is_empty() {
        bail!("位置 {} 缺少 URL", sp.id);
    }
    let store = WebDavStore::new(wcfg).map_err(|e| anyhow!("构造 WebDAV 客户端失败: {e}"))?;
    Ok(AnyStore::Dav(store))
}

/// 用已保存的 Telegram 登录态连上去。
///
/// 代理默认取系统代理（与 GUI 首次探测同口径）；CLI 不在本切片里给文件命令
/// 加 `--proxy`，需要时可先 `telegram login --proxy`。
async fn connect_telegram(sp: &SavedPlace) -> Result<AnyStore> {
    use omy_remote::telegram::{AppId, DeviceInfo, connect};

    let app = AppId::builtin();
    let device = DeviceInfo::current();
    let proxy = omy_remote::telegram::proxy::detect_system_proxy().map(|p| p.as_str().to_owned());

    let conn = connect::connect_saved(&app, &device, proxy.as_deref(), &sp.id)
        .await
        .map_err(|e| anyhow!("连接 Telegram 位置 {} 失败：{e}", sp.id))?;
    Ok(AnyStore::Tg(TelegramStore::from_connection(
        conn.client,
        conn.runner,
    )))
}
