//! 远程位置（WebDAV）命令。
//!
//! # 边界
//!
//! 本阶段只做 **WebDAV 纵向切片**：位置注册/浏览/上传/下载/跨位置复制/缓存管理。
//! Telegram 位置的长连接、二维码登录属于另一套，CLI 本阶段不碰——
//! 遇到 Telegram 记录会明确报错而不是静默跳过。
//!
//! # 与 GUI 共用什么
//!
//! 位置的配置落盘、密码信封的封/解、厂商串都在
//! [`omy_remote::placebook`]，GUI 与 CLI 同一份：命令行加的位置 GUI 认得出，
//! GUI 加的位置命令行解得了密码。
//!
//! # 退出码
//!
//! 成功为 0；参数用法错误由 clap 报 2；其余远程操作失败统一为 1（`GENERAL_ERROR`），
//! 错误细节在 stderr / `--json` 的 `error.message` 里。这样脚本可按退出码区分
//! 「用法错」与「跑起来了但失败了」，不必解析中文文案。

pub mod cache;
pub mod files;
pub mod qrterm;
mod stores;
pub mod telegram;

use std::path::PathBuf;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use omy_config::{Config, SavedPlace};
use omy_remote::{placebook, RemoteStore};
use serde_json::{Value, json};

use crate::password::{PasswordSource, read_password};
use omy_remote::webdav::WebDavStore;
use super::Ctx;

/// 远程位置子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 列出已注册的远程位置
    List,
    /// 显示一个位置的连接信息（不含密码）
    Show(ShowArgs),
    /// 添加一个 WebDAV 位置并保存到配置
    AddWebdav(AddWebdavArgs),
    /// 移除一个位置（只删注册，不删云端文件）
    Remove(NameArgs),
    /// 重命名一个位置（只改本地显示名，不碰服务端）
    Rename(RenameArgs),
    /// 列出远程目录内容
    Ls(files::LsArgs),
    /// 上传本地文件到远程目录
    Upload(files::UploadArgs),
    /// 下载远程文件到本地
    Download(files::DownloadArgs),
    /// 在两个位置之间复制远程文件
    Copy(files::CopyArgs),
    /// Telegram 账号扫码登录与位置管理
    #[command(subcommand)]
    Telegram(telegram::Cmd),
    /// 密文块缓存管理
    #[command(subcommand)]
    Cache(cache::Cmd),
}

/// `omy remote show <位置>` / `remove <位置>`。
#[derive(Debug, Args)]
pub struct NameArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
}

/// `omy remote show <位置>`。
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
}

/// `omy remote rename <位置> <新名字>`。
#[derive(Debug, Args)]
pub struct RenameArgs {
    /// 位置 id（如 p1）或显示名
    pub place: String,
    /// 新的显示名
    pub name: String,
}

/// `omy remote add-webdav <名字> --url ...`。
#[derive(Debug, Args)]
pub struct AddWebdavArgs {
    /// 位置显示名（可重复，仅作本地标签）
    pub name: String,
    /// WebDAV 根地址，如 https://dav.example.com/dav
    #[arg(long, value_name = "URL")]
    pub url: String,
    /// 用户名（匿名访问留空）
    #[arg(long, value_name = "USER")]
    pub user: Option<String>,
    /// 只允许读取（默认可写）
    #[arg(long)]
    pub read_only: bool,
    /// 服务端厂商：generic 或 nextcloud
    #[arg(long, value_name = "NAME", default_value = "generic")]
    pub vendor: String,
    /// 匿名访问（不存用户名/密码；与任何 --password-* 互斥）
    #[arg(long)]
    pub anonymous: bool,

    /// 从环境变量读取密码（传变量名，不是值）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// 从文件读取密码首行
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,
    /// 从标准输入读取密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// 分发。
pub fn run(ctx: &Ctx, cmd: &Cmd) -> Result<()> {
    match cmd {
        Cmd::List => list(ctx),
        Cmd::Show(a) => show(ctx, a),
        Cmd::AddWebdav(a) => add_webdav(ctx, a),
        Cmd::Remove(a) => remove(ctx, a),
        Cmd::Rename(a) => rename(ctx, a),
        Cmd::Ls(a) => files::ls(ctx, a),
        Cmd::Upload(a) => files::upload(ctx, a),
        Cmd::Download(a) => files::download(ctx, a),
        Cmd::Copy(a) => files::copy(ctx, a),
        Cmd::Telegram(c) => telegram::run(ctx, c),
        Cmd::Cache(c) => cache::run(ctx, c),
    }
}

/// 建一个 tokio runtime（与 share 命令同一模式：只在需要网络的命令里建）。
fn rt() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("无法创建异步运行时")
}

/// 按 id 精确匹配，再按显示名唯一匹配，解析一个位置。
///
/// id 优先：显示名可以重复、可以改，id 才是稳定标识。名字撞了要如实报错，
/// 而不是随便挑一个——拿错位置去删/传文件是难以察觉的数据错误。
fn find_place(cfg: &Config, needle: &str) -> Result<SavedPlace> {
    let places = &cfg.remote.places;
    if let Some(sp) = places.iter().find(|p| p.id == needle) {
        return Ok(sp.clone());
    }
    let hits: Vec<&SavedPlace> = places.iter().filter(|p| p.name == needle).collect();
    match hits.len() {
        0 => bail!("找不到位置 {needle:?}（用 `omy remote list` 查看已注册位置）"),
        1 => Ok(hits
            .into_iter()
            .next()
            .expect("恰好一个命中")
            .clone()),
        n => {
            let ids: Vec<&str> = hits.iter().map(|p| p.id.as_str()).collect();
            bail!("位置名 {needle:?} 对应 {n} 个位置，请改用 id：{}", ids.join(", "))
        }
    }
}

/// 从一条配置记录连接出可用的驱动。
///
/// WebDAV 不发网络（构造即得）；Telegram 必须真的连上去，所以是异步的。
/// 统一返回 [`stores::AnyStore`]，文件命令不必区分两种驱动。
pub(crate) async fn connect_store(sp: &SavedPlace) -> Result<stores::AnyStore> {
    stores::connect_store(sp).await
}

/// 重读最新配置，只对 `places` 做本次那一处改动后写回。
///
/// **不能**拿启动时的配置快照整体覆盖：本进程启动后 GUI 可能又加了/删了位置，
/// 整体回写 `remote.places` 会把那段时间里 GUI 的改动整个冲掉（丢失更新）。
/// 这里每次都先从磁盘重读，再由闭包按 id 做本次操作（增/删/改一条）。
///
/// 仍存在「重读 → 写回」之间的极小 TOCTOU 窗口（两个进程同时改）；要彻底排除
/// 需要跨进程文件锁，本切片先把「启动快照覆盖」这个最大、最常见的丢失更新堵上。
fn mutate_places<F>(ctx: &Ctx, f: F) -> Result<()>
where
    F: FnOnce(&mut Vec<SavedPlace>) -> Result<()>,
{
    let mut fresh = match ctx.config_path {
        Some(p) => Config::load_from(p)
            .with_context(|| format!("重读配置 {} 失败", p.display()))?,
        None => Config::load().context("重读配置失败")?,
    };
    f(&mut fresh.remote.places)?;
    match ctx.config_path {
        Some(p) => fresh
            .save_to(p)
            .with_context(|| format!("写回配置 {} 失败", p.display()))?,
        None => fresh.save().context("写回配置失败")?,
    }
    Ok(())
}

/// 远程块缓存根目录：与 GUI 的 `RemoteCache::resolve_root` 同口径，
/// 否则 CLI 看到的缓存占用和 GUI 设置页对不上。
#[must_use]
fn cache_root(cfg: &Config) -> Option<PathBuf> {
    cfg.remote
        .cache_dir
        .clone()
        .map(PathBuf::from)
        .or_else(omy_config::cache_dir)
        .map(|base| base.join("remote"))
}

/// 把用户给的远程路径规整成 WebDAV 绝对路径（以 / 开头）。
pub(crate) fn abs(path: &str) -> String {
    let t = path.trim();
    if t.starts_with('/') {
        t.to_string()
    } else {
        format!("/{t}")
    }
}

/// 拆成 (父目录, 文件名)。根目录下的文件父目录为 ""。
pub(crate) fn parent_name(path: &str) -> (String, String) {
    let p = abs(path);
    match p.rfind('/') {
        Some(0) => (String::new(), p[1..].to_string()),
        Some(i) => (p[..i].to_string(), p[i + 1..].to_string()),
        None => (String::new(), p),
    }
}

// ---- 位置管理 ----

/// `omy remote list`。
fn list(ctx: &Ctx) -> Result<()> {
    let rows: Vec<Value> = ctx
        .cfg
        .remote
        .places
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "kind": p.kind,
                "url": p.url,
                "username": p.username,
                "vendor": p.vendor,
                "writable": p.writable,
                "has_password": p.secret.is_some(),
            })
        })
        .collect();

    let human = if rows.is_empty() {
        String::from("（还没有远程位置，用 `omy remote add-webdav` 添加）")
    } else {
        rows.iter()
            .map(|r| {
                let id = r["id"].as_str().unwrap_or("");
                let name = r["name"].as_str().unwrap_or("");
                let kind = r["kind"].as_str().unwrap_or("");
                let w = if r["writable"].as_bool() == Some(true) { "rw" } else { "ro" };
                format!("{id:<6} {w:<3} {kind:<9} {name}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    ctx.out.result(&human, &json!({ "places": rows }));
    Ok(())
}

/// `omy remote show <位置>`。
fn show(ctx: &Ctx, a: &ShowArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    let value = json!({
        "id": sp.id,
        "name": sp.name,
        "kind": sp.kind,
        "url": sp.url,
        "username": sp.username,
        "vendor": sp.vendor,
        "writable": sp.writable,
        "has_password": sp.secret.is_some(),
        "user_id": sp.user_id,
    });
    let human = format!(
        "{}  {}\n 类型: {}\n 地址: {}\n 用户: {}\n 厂商: {}\n 权限: {}\n 已存密码: {}",
        sp.id,
        sp.name,
        sp.kind,
        sp.url,
        if sp.username.is_empty() { "(匿名)" } else { &sp.username },
        sp.vendor,
        if sp.writable { "读写" } else { "只读" },
        if sp.secret.is_some() { "是" } else { "否" },
    );
    ctx.out.result(&human, &value);
    Ok(())
}

/// `omy remote add-webdav`。
fn add_webdav(ctx: &Ctx, a: &AddWebdavArgs) -> Result<()> {
    // 密码绝不走 argv（旁路 L12）。匿名是显式、安全的空凭据路径：
    // 只有加了 --anonymous 才允许空密码；普通路径仍走 read_password 的非空约束，
    // 不能因为这里要支持匿名就把全局非空检查放宽（会削弱加密命令）。
    let src = PasswordSource {
        env: a.password_env.clone(),
        file: a.password_file.clone(),
        stdin: a.password_stdin,
    };
    let has_pw_src = src.env.is_some() || src.file.is_some() || src.stdin;
    if a.anonymous && has_pw_src {
        bail!("--anonymous 与 --password-stdin/--password-file/--password-env 互斥：匿名访问不应提供密码");
    }
    let password: String = if a.anonymous {
        // 匿名：不落密码，也不提示交互（非 TTY 也能直接加）
        String::new()
    } else {
        String::from_utf8(read_password(&src, "WebDAV 密码（不加 --anonymous 时必填）", false)?)?
    };

    let vendor = placebook::parse_vendor(&a.vendor);
    let wcfg = omy_remote::webdav::WebDavConfig {
        base_url: a.url.trim().to_string(),
        username: a.user.clone().unwrap_or_default(),
        password,
        writable: !a.read_only,
        vendor,
        ..Default::default()
    };

    // 先构造客户端：URL 非法在这里就报错，而不是写进配置后第一次浏览才炸。
    let store = WebDavStore::new(wcfg).map_err(|e| anyhow!("WebDAV 地址无效: {e}"))?;
    // 顺手探活：列一次根目录，URL/凭据错了立刻告诉用户，而不是默默存个连不上的位置。
    let rt = rt()?;
    rt.block_on(store.list(""))
        .map_err(|e| anyhow!("连接 WebDAV 失败（未保存位置）: {e}"))?;

    // id 分配与落盘都在「重读后的最新 places」上做，避免与这期间 GUI 新增的位置撞 id
    // 或被快照覆盖。
    let key = placebook::protect_key();
    let mut assigned_id = String::new();
    mutate_places(ctx, |places| {
        let existing: std::collections::HashSet<String> =
            places.iter().map(|p| p.id.clone()).collect();
        let blocked = |id: &str| {
            omy_remote::telegram::session::session_path_of(id)
                .map(|p| p.exists())
                .unwrap_or(false)
        };
        let id = placebook::allocate_place_id(&existing, &blocked);
        let saved = placebook::webdav_to_saved(&id, &a.name, store.config(), key.as_ref());
        places.push(saved);
        assigned_id = id;
        Ok(())
    })?;

    let human = format!("已添加位置 {assigned_id}（{}）", a.name);
    ctx.out.result(&human, &json!({ "id": assigned_id, "name": a.name }));
    Ok(())
}

/// `omy remote remove <位置>`。
fn remove(ctx: &Ctx, a: &NameArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    if !ctx.out.confirm(
        format!("将移除位置 {}（{}）。云端文件不受影响。继续？[y/N] ", sp.id, sp.name).as_str(),
        ctx.assume_yes,
    ) {
        bail!("已取消");
    }
    mutate_places(ctx, |places| {
        places.retain(|p| p.id != sp.id);
        Ok(())
    })?;
    let human = format!("已移除位置 {}（{}）", sp.id, sp.name);
    ctx.out.result(&human, &json!({ "removed": sp.id }));
    Ok(())
}

/// `omy remote rename <位置> <新名字>`。
fn rename(ctx: &Ctx, a: &RenameArgs) -> Result<()> {
    let sp = find_place(ctx.cfg, &a.place)?;
    mutate_places(ctx, |places| {
        for p in places.iter_mut() {
            if p.id == sp.id {
                p.name = a.name.clone();
            }
        }
        Ok(())
    })?;
    let human = format!("已把位置 {} 改名为 {}", sp.id, a.name);
    ctx.out.result(&human, &json!({ "id": sp.id, "name": a.name }));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `find_place` 必须 id 精确优先、名字唯一才接受、撞名报错。
    ///
    /// 不这样会怎样：名字撞了随便挑一个，删/传文件就会落到错误的位置上，
    /// 而操作全程「成功」——这是加密工具里最难发现的一类错。
    #[test]
    fn find_place_resolves_id_and_disambiguates_names() {
        let mut cfg = Config::default();
        cfg.remote.places = vec![
            SavedPlace {
                id: String::from("p1"),
                name: String::from("NAS"),
                kind: String::from("webdav"),
                url: String::from("https://a/"),
                username: String::new(),
                vendor: String::new(),
                writable: true,
                secret: None,
                user_id: None,
            },
            SavedPlace {
                id: String::from("p2"),
                name: String::from("NAS"),
                kind: String::from("webdav"),
                url: String::from("https://b/"),
                username: String::new(),
                vendor: String::new(),
                writable: true,
                secret: None,
                user_id: None,
            },
        ];
        // id 精确命中
        assert_eq!(find_place(&cfg, "p1").unwrap().url, "https://a/");
        // 撞名必须报错，不能随便挑
        assert!(find_place(&cfg, "NAS").is_err(), "撞名应当报错");
        // 不存在
        assert!(find_place(&cfg, "p99").is_err());
    }

    /// 非 webdav / 非 telegram 的未知类型记录要明确报错，而不是静默造出半截位置。
    ///
    /// 不这样会怎样：遇到不认识的 kind 静默跳过，用户以为连上了其实没连，
    /// 错误要到更后面才冒出来。这里只断言「未知类型被拒」这条纯校验分支——
    /// 真正连网由 stores::connect_store 在 runtime 里做，单测不碰网络。
    #[test]
    fn unknown_kind_is_rejected_locally() {
        let sp = SavedPlace {
            id: String::from("p1"),
            name: String::from("x"),
            kind: String::from("some-future-kind"),
            url: String::new(),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: None,
        };
        let rt = rt().expect("可建 runtime");
        let res = rt.block_on(connect_store(&sp));
        assert!(res.is_err(), "未知类型应当被拒");
        assert!(res.unwrap_err().to_string().contains("some-future-kind"));
    }

    fn dummy_place(id: &str, name: &str) -> SavedPlace {
        SavedPlace {
            id: id.to_string(),
            name: name.to_string(),
            kind: String::from("webdav"),
            url: String::from("https://x/"),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: None,
        }
    }

    /// 复现「启动快照整体覆盖」的丢失更新：磁盘上这期间 GUI 加了 p2，
    /// CLI 用启动快照（只有 p1）整体回写就会把 p2 冲掉。
    ///
    /// 不这样会怎样：两个客户端交替写配置，后写的那个把先写的位置静默删掉，
    /// 用户在 GUI 里看到的位置莫名消失，且没有任何报错。修复要求写前重读磁盘、
    /// 只按 id 改本次那一条——下面断言 p2 在删除 p1 后仍然存在。
    #[test]
    fn mutate_places_does_not_clobber_concurrent_add() {
        let dir = std::env::temp_dir().join(format!("omy_mutate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        // 磁盘上已有 p1 + p2（模拟 GUI 已加了 p2）
        let mut on_disk = Config::default();
        on_disk.remote.places = vec![dummy_place("p1", "old"), dummy_place("p2", "gui-added")];
        on_disk.save_to(&path).unwrap();

        // CLI 启动快照只看到 p1（p2 是启动之后 GUI 才加的）
        let mut stale = Config::default();
        stale.remote.places = vec![dummy_place("p1", "old")];

        let out = crate::output::Out::new(crate::output::Format::Human, false, 0, true);
        let ctx = super::Ctx {
            out: &out,
            cfg: &stale,
            assume_yes: true,
            config_path: Some(path.as_path()),
        };

        // 删除 p1。若用旧的「快照整体覆盖」，写回的就是 [p1] 删完后的空列表，p2 丢失；
        // 新实现重读磁盘后只删 p1，p2 应保留。
        mutate_places(&ctx, |places| {
            places.retain(|p| p.id != "p1");
            Ok(())
        })
        .unwrap();

        let after = Config::load_from(&path).unwrap();
        let ids: Vec<&str> = after.remote.places.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["p2"], "GUI 新增的 p2 不能被 CLI 的快照覆盖冲掉: {ids:?}");

        std::fs::remove_dir_all(&dir).ok();
    }
}
