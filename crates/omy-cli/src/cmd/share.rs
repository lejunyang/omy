//! `omy share`：局域网共享与访问。
//!
//! # 为什么是子命令组而不是单个命令
//!
//! 局域网功能天然有四个动作，参数差异很大：起服务、找设备、配对、
//! 连上去看。塞进一个命令会让参数互相冲突（`--serve` 和 `--connect`
//! 显然不能同时给），分开则每个命令的 `--help` 都是干净的。
//!
//! # 密钥边界
//!
//! `omy share serve` **不需要**共享文件的密码——它只把密文字节搬运给
//! 对方，解密在访问端完成（决策 DEC-16）。这是刻意的：共享方 vault
//! 锁定后共享仍然可用，而且共享进程被攻破也拿不到明文。
//!
//! 唯一需要的密码是**设备库**的密码，用来解开本机身份与已配对设备
//! 列表。两者不是一回事，帮助文本里要写清楚，否则用户会以为共享
//! 就等于把密码交出去。

use super::Ctx;
use anyhow::{Context as _, Result, bail};
use clap::{Args as ClapArgs, Subcommand};
use omy_net::store::{DeviceRecord, Store};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

/// `share` 的子命令。
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// 共享一个目录，等待已配对设备连接
    Serve(ServeArgs),
    /// 搜索局域网内正在共享的设备
    Discover(DiscoverArgs),
    /// 与另一台设备配对（双方都要执行）
    Pair(PairArgs),
    /// 连接已配对的设备，列出或取回文件
    Connect(ConnectArgs),
    /// 管理已配对设备
    #[command(subcommand)]
    Devices(DevicesCmd),
}

/// `share serve` 的参数。
#[derive(Debug, ClapArgs)]
pub struct ServeArgs {
    /// 要共享的目录
    pub dir: PathBuf,

    /// 广播给局域网的设备名，默认取设备库中的名字
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// 监听端口，0 表示由系统分配
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// 只监听本机回环，不对局域网开放（用于自测）
    #[arg(long)]
    pub local_only: bool,

    /// 不通过 mDNS 广播，对方需手动输入地址
    #[arg(long)]
    pub no_advertise: bool,

    /// 最大并发连接数
    #[arg(long, value_name = "N")]
    pub max_connections: Option<usize>,

    /// 设备库路径
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// `share discover` 的参数。
#[derive(Debug, ClapArgs)]
pub struct DiscoverArgs {
    /// 搜索时长（秒）
    #[arg(long, default_value_t = 5)]
    pub timeout: u64,

    /// 设备库路径，用于标注哪些设备已配对
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// `share pair` 的参数。
#[derive(Debug, ClapArgs)]
pub struct PairArgs {
    /// 对方地址（`IP:端口`）。发起方必填
    #[arg(value_name = "ADDR")]
    pub addr: Option<String>,

    /// 等待对方连入而不是主动连接
    #[arg(long, conflicts_with = "addr")]
    pub listen: bool,

    /// 监听端口，仅 `--listen` 时有效
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// 授权有效期（天），0 表示永久
    #[arg(long, default_value_t = 0, value_name = "DAYS")]
    pub expires_in: u32,

    /// 从文件读取配对码，避免交互输入
    ///
    /// 配对码与设备库密码是两个不同的值，所以要单独的开关——
    /// 复用 `--password-file` 会让两者混在一起。
    #[arg(long, value_name = "PATH")]
    pub pin_file: Option<PathBuf>,

    /// 设备库路径
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// `share connect` 的参数。
#[derive(Debug, ClapArgs)]
pub struct ConnectArgs {
    /// 对方指纹（16 位十六进制），用 `share discover` 或
    /// `share devices list` 查看
    #[arg(value_name = "FINGERPRINT")]
    pub fingerprint: String,

    /// 直接指定地址（`IP:端口`），跳过 mDNS 发现
    #[arg(long, value_name = "ADDR")]
    pub addr: Option<String>,

    /// 把远端文件取回到本地目录（不解密，仍是 .omy 密文）
    #[arg(long, value_name = "DIR")]
    pub fetch: Option<PathBuf>,

    /// 设备库路径
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// `share devices` 的子命令。
#[derive(Debug, Subcommand)]
pub enum DevicesCmd {
    /// 列出已配对设备
    List(DevicesListArgs),
    /// 吊销某台设备的授权
    Revoke(DevicesRevokeArgs),
    /// 清理已过期的授权
    Purge(DevicesListArgs),
}

/// `devices list` / `purge` 的参数。
#[derive(Debug, ClapArgs)]
pub struct DevicesListArgs {
    /// 设备库路径
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// `devices revoke` 的参数。
#[derive(Debug, ClapArgs)]
pub struct DevicesRevokeArgs {
    /// 要吊销的设备指纹（16 位十六进制，可用 `devices list` 查看）
    #[arg(value_name = "FINGERPRINT")]
    pub fingerprint: String,

    /// 设备库路径
    #[arg(long, value_name = "PATH")]
    pub store: Option<PathBuf>,

    /// 从环境变量读取设备库密码（传变量名）
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,

    /// 从文件读取设备库密码
    #[arg(long, value_name = "PATH")]
    pub password_file: Option<PathBuf>,

    /// 从标准输入读取设备库密码
    #[arg(long)]
    pub password_stdin: bool,
}

/// 执行 `share`。
///
/// # Errors
///
/// 网络、设备库或参数错误时返回。
pub fn run(ctx: &Ctx<'_>, c: &Cmd) -> Result<()> {
    match c {
        Cmd::Serve(a) => run_serve(ctx, a),
        Cmd::Discover(a) => run_discover(ctx, a),
        Cmd::Pair(a) => run_pair(ctx, a),
        Cmd::Connect(a) => run_connect(ctx, a),
        Cmd::Devices(d) => run_devices(ctx, d),
    }
}

/// 把三个独立开关组成密码来源。
///
/// 与其他子命令用同一组开关名（`--password-stdin` 等），
/// 用户不必为局域网功能记第二套。
fn pw_source(
    env: Option<String>,
    file: Option<PathBuf>,
    stdin: bool,
) -> crate::password::PasswordSource {
    crate::password::PasswordSource { env, file, stdin }
}

/// 读设备库密码。
///
/// 提示语不带冒号——`read_password` 自己会补 `": "`，
/// 再带一个就成了 `密码: : `。
///
/// 设备库密码支持与文件密码相同的非交互通道（`--password-stdin` 等），
/// 否则 `share serve` 没法写进开机自启脚本。
fn ask_store_password(src: &crate::password::PasswordSource, prompt: &str) -> Result<Vec<u8>> {
    crate::password::read_password(src, prompt, false)
}

/// 建一个 tokio runtime。
///
/// CLI 整体是同步的，只有网络命令需要异步。在命令内部建 runtime 比
/// 把整个 `main` 改成 `#[tokio::main]` 好：加解密命令不该为了一个用
/// 不到的运行时付出启动开销，也不该被迫处理 async 传染。
fn rt() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("无法创建异步运行时")
}

/// 打开或创建设备库。
///
/// 密码用途与文件密码**不同**，提示语要说清楚，否则用户会输错。
fn open_store(
    ctx: &Ctx<'_>,
    path: Option<&std::path::Path>,
    src: &crate::password::PasswordSource,
) -> Result<(Store, PathBuf)> {
    let p = match path {
        Some(p) => p.to_path_buf(),
        None => {
            // 显式指定了 --store 就不动；默认路径才做升级迁移，且赶在
            // exists() 之前——否则新位置缺库会被当成「首次使用」重建身份。
            omy_net::store::migrate_legacy_if_needed();
            omy_net::store::default_path().context("无法确定设备库默认路径")?
        }
    };

    if p.exists() {
        let pw = ask_store_password(src, "设备库密码（不是文件密码）")?;
        let s = Store::load(&p, &pw)
            .with_context(|| format!("无法打开设备库 {}", p.display()))?;
        Ok((s, p))
    } else {
        ctx.out
            .info("首次使用，正在创建设备库。这个密码只用于保护本机身份与已配对设备列表，与加密文件的密码无关。");
        let name = default_device_name();
        // 创建时要确认：设备库密码打错了会导致所有已配对设备失效，
        // 且没有找回手段
        let pw = crate::password::read_password(src, "设置设备库密码", true)?;
        let s = Store::create(&name).context("创建设备库失败")?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        s.save(&p, &pw)
            .with_context(|| format!("无法保存设备库 {}", p.display()))?;
        ctx.out.info(&format!(
            "已创建设备库：{}（设备名「{name}」，可用 --name 修改）",
            p.display()
        ));
        Ok((s, p))
    }
}

/// 取一个像样的默认设备名。
fn default_device_name() -> String {
    // 主机名最贴近用户认知——他们在文件管理器里看到的就是这个
    let raw = hostname_or_default();
    // 设备名有长度与字符限制，先归一化再交给校验
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(32)
        .collect();
    if cleaned.trim().is_empty() {
        "omy 设备".to_owned()
    } else {
        cleaned.trim().to_owned()
    }
}

/// 读主机名，失败时给个兜底。
fn hostname_or_default() -> String {
    // 不为了主机名引入一个依赖：环境变量在两个平台上都可用，
    // 拿不到就用兜底名，这不是关键路径
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "omy 设备".to_owned())
}

/// 执行 `share serve`。
fn run_serve(ctx: &Ctx<'_>, a: &ServeArgs) -> Result<()> {
    if !a.dir.is_dir() {
        bail!("{} 不是目录", a.dir.display());
    }

    let src = pw_source(
        a.password_env.clone(),
        a.password_file.clone(),
        a.password_stdin,
    );
    let (mut store, store_path) = open_store(ctx, a.store.as_deref(), &src)?;
    if let Some(n) = a.name.as_deref() {
        store.set_device_name(n).context("设备名不合法")?;
        // 改了名字要存回去，否则下次启动又变回原来的
        let pw = ask_store_password(&src, "确认设备库密码以保存新设备名")?;
        store.save(&store_path, &pw).ok();
    }

    if store.active_devices().is_empty() {
        ctx.out.warn(
            "尚未配对任何设备，没有人能连进来。先在两台设备上分别运行 `omy share pair`。",
        );
    }

    let share = omy_net::serve::Share::from_dir(&a.dir)
        .with_context(|| format!("无法扫描 {}", a.dir.display()))?;
    let n_files = share.len();
    if n_files == 0 {
        ctx.out
            .warn(&format!("{} 中没有 .omy 文件，对方会看到空列表", a.dir.display()));
    }

    let bind = if a.local_only {
        std::net::SocketAddr::from(([127, 0, 0, 1], a.port))
    } else {
        std::net::SocketAddr::from(([0, 0, 0, 0], a.port))
    };

    let advertise = if a.no_advertise {
        None
    } else {
        Some(store.device_name().to_owned())
    };

    let cfg = omy_net::server_loop::ServeConfig {
        bind,
        advertise_as: advertise,
        max_connections: a
            .max_connections
            .unwrap_or(omy_net::server_loop::MAX_CONNECTIONS),
        ..omy_net::server_loop::ServeConfig::default()
    };

    // serve 需要同时持有 store（查授权）与 keypair（握手），
    // 而 StaticKeypair 有意不实现 Clone（私钥不该被随手复制）。
    // 这里从字节显式重建一份，是唯一需要复制它的地方
    let keypair = omy_net::channel::StaticKeypair::from_parts(
        store.public_key().to_vec(),
        store.keypair().private_bytes().to_vec(),
    );
    // 连接事件的回调要求 'static，而 ctx.out 是借用——不能直接塞进去。
    // 用通道把事件送回本函数打印：这样输出仍然走统一的 Out（尊重
    // --json / --quiet），也不必给 Out 加 Arc
    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
    let rt = rt()?;

    rt.block_on(async move {
        let running = omy_net::server_loop::serve(
            cfg,
            std::sync::Arc::new(omy_net::serve::Server::new(share)),
            std::sync::Arc::new(store),
            std::sync::Arc::new(keypair),
            move |ev| {
                // 发送失败只意味着主循环已退出，忽略即可
                let _ = ev_tx.send(ev);
            },
        )
        .await
        .context("启动共享服务失败")?;

        let addr = running.local_addr();
        ctx.out.result(
            &format!("正在共享 {n_files} 个文件，监听 {addr}\n按 Ctrl-C 停止"),
            &json!({ "listening": addr.to_string(), "files": n_files }),
        );

        // 一边打印事件一边等 Ctrl-C。
        // 用户按下时要干净地注销 mDNS 广播，否则别的设备会在列表里
        // 看到一个连不上的幽灵条目
        loop {
            tokio::select! {
                ev = ev_rx.recv() => {
                    match ev {
                        Some(e) => log_conn_event(ctx.out, &e),
                        None => break,
                    }
                }
                _ = tokio::signal::ctrl_c() => break,
            }
        }

        ctx.out.info("\n正在停止…");
        running.shutdown().await.context("停止服务失败")?;
        ctx.out.info("已停止");
        Ok::<(), anyhow::Error>(())
    })
}

/// 把连接事件写成人能看懂的一行。
fn log_conn_event(out: &crate::output::Out, ev: &omy_net::server_loop::ConnOutcome) {
    use omy_net::server_loop::ConnOutcome as C;
    match ev {
        C::Served { peer, requests } => {
            out.detail(&format!("{} 断开，共 {requests} 个请求", hex8(peer)));
        }
        C::Unauthorized { peer } => {
            // 这条要显眼：说明有设备主动尝试访问但没有授权，
            // 用户可能想知道，也可能正是他忘了配对的那台
            out.warn(&format!(
                "拒绝了未配对设备 {}（如果是你的设备，请先运行 `omy share pair`）",
                hex8(peer)
            ));
        }
        C::HandshakeFailed => out.detail("一个连接握手失败"),
        C::Rejected => out.warn("连接数已达上限，拒绝了一个新连接"),
    }
}

/// 指纹转十六进制。
fn hex8(fp: &[u8; 8]) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

/// 执行 `share discover`。
fn run_discover(ctx: &Ctx<'_>, a: &DiscoverArgs) -> Result<()> {
    // 发现本身不需要设备库。但有它就能标注"这台我认识"，
    // 而这恰恰是用户最想知道的信息。
    //
    // 只在用户显式给了 --store 时才问密码：为了看一眼局域网有什么
    // 就强制输密码，会让这个命令变得难用
    let known = match a.store.as_deref() {
        Some(p) => {
            let pw = ask_store_password(
                &pw_source(
                    a.password_env.clone(),
                    a.password_file.clone(),
                    a.password_stdin,
                ),
                "设备库密码",
            )?;
            Some(
                Store::load(p, &pw)
                    .with_context(|| format!("无法打开设备库 {}", p.display()))?,
            )
        }
        None => None,
    };

    // browse 是同步的（mdns-sd 自己管线程），不需要 runtime
    ctx.out
        .detail(&format!("正在搜索局域网，等待 {} 秒…", a.timeout));
    let found = omy_net::discovery::browse(Duration::from_secs(a.timeout))
        .context("mDNS 搜索失败")?;

    if found.is_empty() {
        ctx.out.info(&format!(
            "{} 秒内没有发现共享设备。确认对方已运行 `omy share serve` 且在同一网段。",
            a.timeout
        ));
        return Ok(());
    }

    let mut rows = Vec::new();
    for d in &found {
        let fp = d.fingerprint_hex();
        // 已配对判断走**指纹**：广播里没有完整公钥，
        // 而且就算有也不能信（任何人都能广播任意内容）
        let paired = known
            .as_ref()
            .and_then(|s| s.find_by_fingerprint(&d.fingerprint))
            .map(|rec| if rec.is_expired() { "已过期" } else { "已配对" });
        let mark = match (paired, d.is_compatible()) {
            (_, false) => "版本不兼容",
            (Some(m), true) => m,
            (None, true) => "未配对",
        };
        let addr = d
            .preferred_addr()
            .map_or_else(|| "(无地址)".to_owned(), |ip| format!("{ip}:{}", d.port));

        ctx.out
            .line(&format!("{:<24} {addr:<22} {fp}  {mark}", d.name));
        rows.push(json!({
            "name": d.name,
            "addr": addr,
            "fingerprint": fp,
            "status": mark,
            "compatible": d.is_compatible(),
        }));
    }

    if ctx.out.is_json() {
        ctx.out
            .result("", &json!({ "devices": rows, "count": rows.len() }));
    } else {
        ctx.out.info(&format!("\n发现 {} 台设备", rows.len()));
        if known.is_none() {
            ctx.out
                .detail("（加 --store 可标注哪些设备已配对）");
        }
        ctx.out
            .detail("设备名来自对方广播，不可信；请以指纹为准。");
    }
    Ok(())
}

/// 执行 `share pair`。
fn run_pair(ctx: &Ctx<'_>, a: &PairArgs) -> Result<()> {
    let src = pw_source(
        a.password_env.clone(),
        a.password_file.clone(),
        a.password_stdin,
    );
    let (mut store, store_path) = open_store(ctx, a.store.as_deref(), &src)?;
    let rt = rt()?;

    let learned = if a.listen {
        let pin = omy_net::pairing::generate_pin();
        ctx.out.result(
            &format!(
                "配对码：{pin}\n\
                 请在另一台设备上运行：omy share pair <本机IP>:<端口>\n\
                 然后输入上面的配对码。"
            ),
            &json!({ "pin": pin }),
        );
        rt.block_on(pair_listen(ctx, &store, &pin, a.port))?
    } else {
        let Some(addr) = a.addr.as_deref() else {
            bail!("需要指定对方地址（如 192.168.1.5:5000），或用 --listen 等待对方连入");
        };
        // 配对码不是密码，但同样不该回显、不该进 shell 历史，
        // 所以复用同一条输入通道。
        //
        // 用单独的 --pin-file 而不是复用 --password-file：这是两个
        // 不同的值（长期的设备库密码 vs 一次性配对码），混用会让人
        // 搞不清文件里该放哪个
        let pin_src = crate::password::PasswordSource {
            file: a.pin_file.clone(),
            ..crate::password::PasswordSource::default()
        };
        let pin = crate::password::read_password(&pin_src, "输入对方显示的配对码", false)?;
        let pin = String::from_utf8(pin).context("配对码必须是文本")?;
        let pin = pin.trim().to_owned();
        omy_net::pairing::validate_pin(&pin).context("配对码格式不对（应为 6 位数字）")?;
        rt.block_on(pair_connect(&store, &pin, addr))?
    };

    let expires = if a.expires_in == 0 {
        None
    } else {
        Some(Duration::from_secs(u64::from(a.expires_in) * 86400))
    };
    store.upsert(DeviceRecord::from_pairing(&learned, expires));

    let pw = ask_store_password(&src, "再次输入设备库密码以保存")?;
    store
        .save(&store_path, &pw)
        .context("保存设备库失败")?;

    let fp = hex8(&learned.fingerprint());
    ctx.out.result(
        &format!("已与「{}」配对，对方指纹 {fp}", learned.name),
        &json!({ "paired_with": learned.name, "fingerprint": fp }),
    );
    ctx.out.info(&format!(
        "本机指纹 {}。请核对两台设备显示的指纹是否一致——不一致说明有人在中间。",
        hex8(&store.fingerprint())
    ));
    Ok(())
}

/// 等待对方连入完成配对。
async fn pair_listen(
    ctx: &Ctx<'_>,
    store: &Store,
    pin: &str,
    port: u16,
) -> Result<omy_net::handshake::PairedDevice> {
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .context("无法监听配对端口")?;
    let addr = listener.local_addr().context("无法取得监听地址")?;
    ctx.out.info(&format!("正在 {addr} 等待配对…"));

    // 配对必须限时：一直挂着等于把 PIN 的有效期无限延长，
    // 而 PIN 只有 6 位数字，时间越久被在线猜中的机会越大
    let accepted = tokio::time::timeout(Duration::from_secs(120), listener.accept())
        .await
        .map_err(|_| anyhow::anyhow!("配对超时（2 分钟）。配对码已作废，请重新开始"))?;
    let (mut sock, _) = accepted.context("接受连接失败")?;

    omy_net::handshake::pair_over(
        &mut sock,
        pin,
        omy_net::pairing::Role::Responder,
        store.keypair(),
        store.device_name(),
    )
    .await
    .context("配对失败。最可能的原因是配对码输错了")
}

/// 主动连接对方完成配对。
async fn pair_connect(
    store: &Store,
    pin: &str,
    addr: &str,
) -> Result<omy_net::handshake::PairedDevice> {
    let mut sock = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("连接 {addr} 超时"))?
    .with_context(|| format!("无法连接 {addr}"))?;

    omy_net::handshake::pair_over(
        &mut sock,
        pin,
        omy_net::pairing::Role::Initiator,
        store.keypair(),
        store.device_name(),
    )
    .await
    .context("配对失败。最可能的原因是配对码输错了")
}

/// 执行 `share connect`。
fn run_connect(ctx: &Ctx<'_>, a: &ConnectArgs) -> Result<()> {
    let src = pw_source(
        a.password_env.clone(),
        a.password_file.clone(),
        a.password_stdin,
    );
    let (store, _) = open_store(ctx, a.store.as_deref(), &src)?;

    // 按指纹在**本地设备库**里反查公钥，而不是相信网络上广播的东西。
    //
    // mDNS 的 TXT 记录只带指纹，不带完整公钥——这是有意的：任何人都
    // 能广播任意内容，如果从广播里取公钥，攻击者广播自己的公钥配上
    // 别人的名字就能冒充。指纹只用来"在我认识的设备里找到是哪一台"。
    let want = parse_fingerprint(&a.fingerprint)?;
    let Some(dev) = store.find_by_fingerprint(&want) else {
        bail!(
            "没有指纹为 {} 的已配对设备。\n\
             先运行 `omy share pair` 完成配对，或用 `omy share devices list` 查看已有设备。",
            hex8(&want)
        );
    };
    if dev.is_expired() {
        bail!("与「{}」的授权已过期，请重新配对", dev.name);
    }
    let peer_public = dev.public_key.clone();
    let peer_name = dev.name.clone();

    let rt = rt()?;
    rt.block_on(async {
        // 地址：优先用户显式给的，否则去局域网找
        let addr = match a.addr.as_deref() {
            Some(s) => s.to_owned(),
            None => {
                ctx.out.detail("正在搜索局域网…");
                let found = omy_net::discovery::browse(Duration::from_secs(3))?;
                let peer = found
                    .iter()
                    .find(|p| p.fingerprint == want)
                    .with_context(|| {
                        format!(
                            "局域网中没找到「{peer_name}」。\n\
                             确认对方已运行 `omy share serve` 且在同一网段，\
                             或用 --addr 手动指定地址。"
                        )
                    })?;
                if !peer.is_compatible() {
                    bail!(
                        "对方协议版本 {} 与本机不兼容，请升级两端到同一版本",
                        peer.version
                    );
                }
                let ip = peer
                    .preferred_addr()
                    .context("对方没有可用的 IP 地址")?;
                format!("{ip}:{}", peer.port)
            }
        };

        let sock = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(&addr),
        )
        .await
        .with_context(|| format!("连接 {addr} 超时"))?
        .with_context(|| format!("无法连接 {addr}"))?;

        let mut ch = omy_net::channel::Channel::connect(sock, store.keypair(), &peer_public)
            .await
            .context("加密握手失败。对方可能已吊销本机的授权")?;

        let entries = match ch.request(&omy_net::wire::Request::List).await? {
            omy_net::wire::Response::ListOk { entries } => entries,
            omy_net::wire::Response::Err { code, msg } => {
                bail!("对方返回错误 {}: {msg}", code.as_str())
            }
            other => bail!("对方返回了意外的响应: {other:?}"),
        };

        if entries.is_empty() {
            ctx.out.info(&format!("「{peer_name}」没有共享任何文件"));
            return Ok(());
        }

        // ⚠️ 远端**不会**给明文文件名：服务端零密钥，它自己也不知道
        // 文件叫什么（决策 D-30）。名字在 header 里加密，要解开得有
        // 对应文件的密码。这里只展示指纹与大小，取回后用
        // `omy info` / `omy decrypt` 才能看到真名
        let mut rows = Vec::new();
        for e in &entries {
            let id = short_handle(&e.handle);
            ctx.out
                .line(&format!("{id}  {:>10}", crate::output::human_bytes(e.size)));
            rows.push(json!({
                "id": id,
                "size": e.size,
                "header_len": e.header_len,
            }));
        }

        if let Some(dest) = a.fetch.as_deref() {
            std::fs::create_dir_all(dest)
                .with_context(|| format!("无法创建 {}", dest.display()))?;
            for e in &entries {
                let bytes = fetch_one(&mut ch, e).await?;
                // 文件名由**本地**生成，不用远端给的任何字符串——
                // 远端根本没给名字，就算给了也不该直接拿来落盘
                let name = format!("{}.omy", short_handle(&e.handle));
                let path = dest.join(&name);
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("无法写入 {}", path.display()))?;
                ctx.out.info(&format!(
                    "已取回 {} → {}",
                    crate::output::human_bytes(e.size),
                    path.display()
                ));
            }
            ctx.out
                .info("取回的仍是密文。用 `omy info <文件>` 查看，`omy decrypt` 解开。");
        }

        if ctx.out.is_json() {
            ctx.out.result(
                "",
                &json!({ "peer": peer_name, "files": rows, "count": rows.len() }),
            );
        } else {
            ctx.out.info(&format!(
                "\n「{peer_name}」共享了 {} 个文件（文件名已加密，需密码才能看到）",
                rows.len()
            ));
        }
        Ok(())
    })
}

/// 解析 16 位十六进制指纹。
fn parse_fingerprint(s: &str) -> Result<[u8; 8]> {
    let t = s.trim().replace([':', '-', ' '], "").to_ascii_lowercase();
    if t.len() != 16 || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("指纹应为 16 位十六进制字符，实际收到 {s:?}");
    }
    let mut out = [0u8; 8];
    for (i, slot) in out.iter_mut().enumerate() {
        let Some(pair) = t.get(i * 2..i * 2 + 2) else {
            bail!("指纹格式错误");
        };
        *slot = u8::from_str_radix(pair, 16).context("指纹含非十六进制字符")?;
    }
    Ok(out)
}

/// handle 的短标识，用于展示与本地文件名。
fn short_handle(h: &omy_net::wire::Handle) -> String {
    h.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// 取回一个远端文件的完整密文。
async fn fetch_one(
    ch: &mut omy_net::channel::Channel<tokio::net::TcpStream>,
    e: &omy_net::wire::Entry,
) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(usize::try_from(e.size).unwrap_or(0));
    while (buf.len() as u64) < e.size {
        let remain = e.size - buf.len() as u64;
        let want = u32::try_from(remain)
            .unwrap_or(omy_net::wire::MAX_READ_LEN)
            .min(omy_net::wire::MAX_READ_LEN);
        match ch
            .request(&omy_net::wire::Request::Read {
                handle: e.handle,
                offset: buf.len() as u64,
                len: want,
            })
            .await?
        {
            omy_net::wire::Response::ReadOk { data } if !data.is_empty() => {
                buf.extend_from_slice(&data);
            }
            omy_net::wire::Response::ReadOk { .. } => break,
            other => bail!("读取中断: {other:?}"),
        }
    }
    if (buf.len() as u64) != e.size {
        bail!(
            "取回不完整：期望 {} 字节，实际 {}",
            e.size,
            buf.len()
        );
    }
    Ok(buf)
}

/// 执行 `share devices`。
fn run_devices(ctx: &Ctx<'_>, c: &DevicesCmd) -> Result<()> {
    match c {
        DevicesCmd::List(a) => {
            let src = pw_source(
                a.password_env.clone(),
                a.password_file.clone(),
                a.password_stdin,
            );
            let (store, _) = open_store(ctx, a.store.as_deref(), &src)?;
            let mut rows = Vec::new();
            for d in store.devices() {
                let fp = hex8(&d.fingerprint());
                let status = if d.is_expired() { "已过期" } else { "有效" };
                let exp = if d.expires_at == 0 {
                    "永久".to_owned()
                } else {
                    format_ts(d.expires_at)
                };
                ctx.out
                    .line(&format!("{fp}  {:<24} {status:<8} 到期 {exp}", d.name));
                rows.push(json!({
                    "fingerprint": fp,
                    "name": d.name,
                    "expired": d.is_expired(),
                    "expires_at": d.expires_at,
                }));
            }
            if ctx.out.is_json() {
                ctx.out
                    .result("", &json!({ "devices": rows, "count": rows.len() }));
            } else if rows.is_empty() {
                ctx.out.info("尚未配对任何设备");
            } else {
                ctx.out.info(&format!(
                    "\n本机指纹 {}（对方配对时应看到这个值）",
                    hex8(&store.fingerprint())
                ));
            }
            Ok(())
        }
        DevicesCmd::Revoke(a) => {
            let src = pw_source(
                a.password_env.clone(),
                a.password_file.clone(),
                a.password_stdin,
            );
            let (mut store, path) = open_store(ctx, a.store.as_deref(), &src)?;
            let want = a.fingerprint.trim().to_ascii_lowercase();
            let Some(d) = store
                .devices()
                .iter()
                .find(|d| hex8(&d.fingerprint()) == want)
                .cloned()
            else {
                bail!("没有指纹为 {want} 的已配对设备。用 `omy share devices list` 查看。");
            };
            let name = d.name.clone();
            if !store.revoke(&d.public_key) {
                bail!("吊销失败");
            }
            let pw = ask_store_password(&src, "再次输入设备库密码以保存")?;
            store.save(&path, &pw).context("保存设备库失败")?;
            ctx.out.result(
                &format!("已吊销「{name}」。该设备下次连接会被拒绝。"),
                &json!({ "revoked": want, "name": name }),
            );
            Ok(())
        }
        DevicesCmd::Purge(a) => {
            let src = pw_source(
                a.password_env.clone(),
                a.password_file.clone(),
                a.password_stdin,
            );
            let (mut store, path) = open_store(ctx, a.store.as_deref(), &src)?;
            let n = store.purge_expired();
            if n == 0 {
                ctx.out.info("没有已过期的授权");
                return Ok(());
            }
            let pw = ask_store_password(&src, "再次输入设备库密码以保存")?;
            store.save(&path, &pw).context("保存设备库失败")?;
            ctx.out.result(
                &format!("已清理 {n} 条过期授权"),
                &json!({ "purged": n }),
            );
            Ok(())
        }
    }
}

/// Unix 时间戳转可读日期。
fn format_ts(ts: u64) -> String {
    // 不引入 chrono 这种重依赖：这里只需要一个粗略可读的日期。
    // 精确到天足够——授权有效期本来就是按天设置的
    let (y, m, d) = civil_from_days(ts / 86400);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 天数转公历日期（Howard Hinnant 的 civil_from_days 算法）。
///
/// 全程用 `u64`：Unix 时间戳非负，不需要处理 1970 年之前的日期，
/// 去掉负数分支就没有符号丢失与截断的问题。
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    // 把纪元挪到 0000-03-01，让闰年规则变成简单的周期
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex8_formats_all_bytes() {
        assert_eq!(hex8(&[0, 1, 2, 3, 0xAB, 0xCD, 0xEF, 0xFF]), "00010203abcdefff");
        // 高位为 0 的字节必须补零，否则指纹长度不定、无法比对
        assert_eq!(hex8(&[0x0A; 8]).len(), 16);
    }

    #[test]
    fn parse_fingerprint_accepts_common_forms() {
        let want = [0x00, 0x11, 0x22, 0x33, 0xAA, 0xBB, 0xCC, 0xDD];
        assert_eq!(parse_fingerprint("001122 33aabbccdd").unwrap(), want);
        assert_eq!(parse_fingerprint("00112233AABBCCDD").unwrap(), want);
        assert_eq!(parse_fingerprint("  00112233aabbccdd  ").unwrap(), want);
        // 常见的分隔写法（用户可能从界面复制带冒号的形式）
        assert_eq!(
            parse_fingerprint("00:11:22:33:aa:bb:cc:dd").unwrap(),
            want
        );
    }

    #[test]
    fn parse_fingerprint_rejects_bad_input() {
        assert!(parse_fingerprint("").is_err());
        assert!(parse_fingerprint("00112233aabbccd").is_err(), "少一位");
        assert!(parse_fingerprint("00112233aabbccdde").is_err(), "多一位");
        assert!(parse_fingerprint("00112233aabbccgg").is_err(), "非十六进制");
        assert!(parse_fingerprint("../../etc/passwd").is_err());
    }

    /// 指纹解析必须与 `hex8` 互为逆运算。
    ///
    /// 两者不一致的话，`devices list` 显示的指纹会没法用在
    /// `connect` 上——这是最容易被忽略又最影响使用的一类错误。
    #[test]
    fn fingerprint_roundtrips_with_hex8() {
        for seed in [0u8, 1, 0x7F, 0x80, 0xFF] {
            let fp = [seed; 8];
            let s = hex8(&fp);
            assert_eq!(parse_fingerprint(&s).unwrap(), fp, "hex8 与解析应互逆");
        }
        let mixed = [0x0A, 0x00, 0xFF, 0x10, 0x01, 0xF0, 0x5A, 0xA5];
        assert_eq!(parse_fingerprint(&hex8(&mixed)).unwrap(), mixed);
    }

    /// 本地落盘的文件名只由 handle 生成，不含任何远端字符串。
    ///
    /// 这是路径穿越的根本性防御：不是过滤危险字符，而是**根本不用**
    /// 远端提供的名字。handle 是 16 字节不透明标识，转成十六进制后
    /// 必然只含 0-9a-f。
    #[test]
    fn short_handle_is_always_safe() {
        for pat in [0u8, 0xFF, 0x2F, 0x5C, 0x3A] {
            let h: omy_net::wire::Handle = [pat; 16];
            let s = short_handle(&h);
            assert!(
                s.chars().all(|c| c.is_ascii_hexdigit()),
                "{s:?} 应只含十六进制字符"
            );
            assert!(!s.contains('/') && !s.contains('\\') && !s.contains(".."));
            assert_eq!(s.len(), 12, "取 6 字节应得 12 个字符");
        }
    }

    #[test]
    fn short_handle_distinguishes_files() {
        let a: omy_net::wire::Handle = [1; 16];
        let mut b: omy_net::wire::Handle = [1; 16];
        b[5] = 2;
        assert_ne!(
            short_handle(&a),
            short_handle(&b),
            "前 6 字节不同的 handle 必须得到不同标识，否则取回时会互相覆盖"
        );
    }

    #[test]
    fn device_name_default_is_valid() {
        let n = default_device_name();
        assert!(!n.is_empty());
        assert!(!n.chars().any(char::is_control));
        // 必须能通过 net 侧的校验，否则首次创建设备库就会失败
        assert!(
            omy_net::discovery::validate_device_name(&n).is_ok(),
            "默认设备名 {n:?} 未通过校验"
        );
    }

    /// 日期换算容易写错，用多个已知基准点锁定。
    ///
    /// 特别是闰年与世纪边界：2000 是闰年（能被 400 整除），
    /// 1900 不是。算错的话到期日会差一天，用户看到的过期时间就不对。
    #[test]
    fn civil_from_days_matches_known_dates() {
        // 每个值都可用 `date -u -d @<秒数>` 核对
        assert_eq!(civil_from_days(0), (1970, 1, 1), "Unix 纪元");
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(31), (1970, 2, 1), "跨月");
        assert_eq!(civil_from_days(364), (1970, 12, 31));
        assert_eq!(civil_from_days(365), (1971, 1, 1), "跨年");
        // 1972 是闰年，2 月 29 日存在
        assert_eq!(civil_from_days(789), (1972, 2, 29), "闰日");
        assert_eq!(civil_from_days(790), (1972, 3, 1));
        // 2000-03-01：世纪闰年边界
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
        // 2000-02-29 必须存在（能被 400 整除，是闰年）
        assert_eq!(civil_from_days(11016), (2000, 2, 29), "世纪闰年");
        // 2024-01-01
        assert_eq!(civil_from_days(19723), (2024, 1, 1));
    }

    #[test]
    fn format_ts_pads_and_divides() {
        assert_eq!(format_ts(0), "1970-01-01");
        // 同一天内的不同秒数应给出同一天
        assert_eq!(format_ts(86399), "1970-01-01");
        assert_eq!(format_ts(86400), "1970-01-02");
        // 月份与日必须补零，否则列宽会跳动
        assert_eq!(format_ts(31 * 86400), "1970-02-01");
    }
}
