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
/// 打开位置对应的存储驱动。
///
/// `password` 仅 Telegram per-place 加密位置需要：本进程现场输入的位置密码会在
/// [`connect_with_password`] 里派成 KEK，与机器回退 KEK 一起试槽。未加密（默认）
/// 位置传 `None` 即可——机器回退就能开。
///
/// WebDAV 不发网络请求（构造即得，探活由调用方决定要不要列一次根目录）；
/// Telegram 必须真的连上去——它是长连接，断开就没有任何文件操作可做。
///
/// # Errors
///
/// 类型不认识、WebDAV 地址非法、或 Telegram 连不上 / 没有登录态 / 位置锁着
/// （缺位置密码）时返回。
/// 打开一个位置的存储。`password` 是 Telegram 位置的 per-place 密码（可选；
/// WebDAV 不用）。`config_path` 是 CLI `--config` 指定的配置文件，Telegram 建连的
/// 应用身份与代理都从它解析（见 [`connect_telegram`]），不退回默认配置。
pub(crate) async fn connect_store(
    sp: &SavedPlace,
    password: Option<&[u8]>,
    config_path: Option<&std::path::Path>,
) -> Result<AnyStore> {
    match sp.kind.as_str() {
        "webdav" => connect_webdav(sp),
        // Telegram 位置需要联网：先确认本机能拿到登录态，拿不到就提前报错。
        "telegram" => connect_telegram(sp, password, config_path).await,
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
/// `config_path` 是 CLI `--config` 指定的文件：建连用的应用身份（api_id/api_hash）
/// 与代理都从它解析（`load_at`），而不是退回默认配置——位置清单来自 `--config`
/// 文件，登录态信封绑的也是这份 api_id，两者必须同源。
///
/// `password` 是本进程现场拿到的位置密码（per-place 加密位置才需要）。
/// 它与机器回退 KEK 一起在 [`connect_with_password`] 里拼成钥匙集合——这正是
/// 「CLI 能用当前进程解锁的 KEK 打开 per-place 加密 session」的落点：CLI 是
/// 一次性进程、没有 GUI 那样的长期会话，KEK 全靠本次现场密码现派。
async fn connect_telegram(
    sp: &SavedPlace,
    password: Option<&[u8]>,
    config_path: Option<&std::path::Path>,
) -> Result<AnyStore> {
    use omy_remote::telegram::{AppIdChoice, TelegramConnectionContext, connect};

    // 与登录/重连同源：从 --config 指定文件解析实际 api_id 与代理（load_at）。
    // 以前这里 load(.., None) 退回默认配置——自定义 api_id 登录的 session 会
    // SessionMismatch，配置里的 manual 代理也被绕过。
    let ctx = TelegramConnectionContext::load_at(config_path, AppIdChoice::FromConfig, None)?;
    let conn = connect::connect_with_password(ctx.app(), ctx.device(), ctx.proxy(), &sp.id, password)
        .await
        .map_err(|e| map_connect_err(&sp.id, password.is_some(), e))?;
    Ok(AnyStore::Tg(TelegramStore::from_connection(
        conn.client,
        conn.runner,
    )))
}

/// 把连接错误翻成**结构化**错误（机器可读 code + 退出码），`--json` 下原样输出。
///
/// 以前这里返回带 `[tg_xxx]` 的 anyhow 字符串，但 report_error 下钻不到字符串里的
/// 结构化码，`--json error.code` 会退化成 GENERAL_ERROR。现在返回 [`RemoteConnErr`]，
/// 经 `anyhow!(e)` 装箱后仍能在错误链里被下钻到。
fn map_connect_err(
    id: &str,
    password_supplied: bool,
    e: omy_remote::telegram::connect::ConnectError,
) -> super::RemoteConnErr {
    use omy_remote::telegram::connect::ConnectError;
    use super::RemoteConnErr;
    match e {
        // 位置被 per-place 加密锁着、又没解开它：分两种。
        // - 用户给过密码还解不开 → 密码错（退出 3，与文件密码错同口径）；
        // - 没给密码 → 锁定态，提示用密码通道，而不是含糊地引去重新扫码。
        ConnectError::Locked => {
            if password_supplied {
                RemoteConnErr::new("TG_PLACE_WRONG_PASSWORD", 3, format!("位置 {id} 密码不正确"))
            } else {
                RemoteConnErr::new(
                    "TG_PLACE_LOCKED",
                    1,
                    format!(
                        "位置 {id} 已加密但当前密码解不开它。\n\
                         \n  请通过密码通道提供该位置密码：\n\
                         \n    --password-stdin        从管道读取\
                         \n    --password-file <路径>  从文件读取\
                         \n    --password-env <变量名> 从环境变量读取"
                    ),
                )
            }
        }
        ConnectError::NoSession => RemoteConnErr::new(
            "TG_NO_SESSION",
            1,
            format!("位置 {id} 没有可用的 Telegram 登录态，请先扫码登录（omy remote telegram login {id}）"),
        ),
        ConnectError::Unauthorized => RemoteConnErr::new(
            "TG_SESSION_EXPIRED",
            1,
            format!("位置 {id} 的登录态已失效，请重新登录（omy remote telegram login {id}）"),
        ),
        ConnectError::ConnectNoProxy(d) => RemoteConnErr::new(
            "TG_CONNECT_NO_PROXY",
            1,
            format!("直连 Telegram 失败且未配置代理（检查网络或在 config 里配 manual 代理）：{d}"),
        ),
        ConnectError::Session(d) => RemoteConnErr::new(
            "TG_SESSION_UNREADABLE",
            1,
            format!("位置 {id} 的登录态读取失败：{d}"),
        ),
        ConnectError::Connect(d) => RemoteConnErr::new(
            "TG_CONNECT_FAILED",
            1,
            format!("连接 Telegram 位置 {id} 失败：{d}"),
        ),
    }
}
