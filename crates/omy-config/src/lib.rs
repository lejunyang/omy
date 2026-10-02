//! omy 的配置定义与读写，CLI 与 GUI 共用。
//!
//! # 为什么要独立成一个 crate
//!
//! 原先只有 CLI 有配置（`omy-cli/src/config.rs`），GUI 把语言和主题存在
//! WebView 的 `localStorage` 里。这套组合撑不起设置页：
//!
//! - `localStorage` 在 WebView 内，**后端读不到**。而缓存上限、自动锁定
//!   这些要由 Rust 侧执行，前端存了也没用。
//! - 清缓存、换 WebView 就丢，用户会发现设置莫名回到默认。
//!
//! 而如果在 GUI 里另写一份配置，同一个键在两边可以有不同默认值，
//! 用户完全看不出为什么 CLI 和 GUI 行为不一致。所以提取到这里共用。
//!
//! # 便携优先：配置放在可执行文件旁边
//!
//! 见 [`config_path`]。这是与多数应用相反的选择，理由写在那里。
//!
//! # 写回时保留未知字段
//!
//! 见 [`save`]。这是本模块最容易被忽略、但后果最实际的一条。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

mod paths;
#[cfg(target_os = "android")]
pub use paths::set_android_dirs;
pub use paths::{cache_dir, config_path, data_dir, is_portable, log_dir, portable_root};

/// 配置读写错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 读写失败。
    #[error("配置文件 {path} {op}失败: {source}")]
    Io {
        /// 出错的路径。
        path: PathBuf,
        /// 正在做的事，用于让错误信息指得到具体步骤。
        op: &'static str,
        /// 底层错误。
        #[source]
        source: std::io::Error,
    },
    /// 解析失败。
    #[error("配置文件 {path} 解析失败: {source}")]
    Parse {
        /// 出错的路径。
        path: PathBuf,
        /// 底层错误。
        #[source]
        source: toml::de::Error,
    },
    /// 序列化失败。
    #[error("配置序列化失败: {0}")]
    Encode(#[from] toml::ser::Error),
    /// 找不到可用的配置目录。
    #[error("无法确定配置文件位置")]
    NoPath,
}

/// 结果别名。
pub type Result<T> = std::result::Result<T, Error>;

/// 顶层配置。所有字段都可缺省。
///
/// # 为什么不用 `deny_unknown_fields`
///
/// CLI 原先用了它，目的是让拼错的键报错而不是被静默忽略。但设置页要
/// **写回**，一旦拒绝未知字段，新版本写的键在旧版本里会直接让整个文件
/// 解析失败——用户升级又降级一次，配置就全没了。
///
/// 改为：解析时接受未知字段，但 [`load_checked`] 会把它们回报给调用方，
/// 由 CLI 决定是否提示。这样既保住了「拼错要能发现」，又不会让跨版本
/// 读写互相破坏。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 默认加密参数。
    pub defaults: Defaults,
    /// 压缩相关。
    pub compress: Compress,
    /// 扫描相关。
    pub scan: Scan,
    /// 共享相关。
    pub serve: Serve,
    /// 界面相关。
    pub ui: Ui,
    /// 安全相关。
    pub security: Security,
    /// 第三方密码管理器。
    pub password_managers: PasswordManagers,
    /// 远程位置与缓存。
    pub remote: Remote,
}

/// 默认加密参数。
///
/// 键名与 CLI 原有的 `[defaults]` 一致，不另起名字——GUI 设置页改的
/// 就是同一组值，用不同键名会让两边各存一份而互不生效。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// KDF 档位：mobile | interactive | moderate | sensitive
    pub kdf_profile: String,
    /// 加密算法：xchacha20 | aes256gcm
    pub cipher: String,
    /// 分块大小，如 `256K`
    pub chunk_size: String,
    /// 文件名模式：encrypt | keep-ext | plain
    pub name_mode: String,
    /// 原文件处理：keep | trash | delete
    pub original_action: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            kdf_profile: String::from("interactive"),
            cipher: String::from("xchacha20"),
            chunk_size: String::from("256K"),
            name_mode: String::from("encrypt"),
            original_action: String::from("keep"),
        }
    }
}

/// 压缩配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Compress {
    /// 是否默认启用。
    pub enabled: bool,
    /// zstd 级别。
    pub level: i32,
}

impl Default for Compress {
    fn default() -> Self {
        Self { enabled: false, level: 3 }
    }
}

/// 扫描配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scan {
    /// 默认扫描路径。
    pub paths: Vec<String>,
    /// 最大递归深度。
    pub max_depth: usize,
}

impl Default for Scan {
    fn default() -> Self {
        Self { paths: Vec::new(), max_depth: 8 }
    }
}

/// 局域网共享配置。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Serve {
    /// 本机显示名。
    pub device_name: Option<String>,
    /// 默认有效期，如 `24h`。
    pub default_expire: Option<String>,
    /// 启动时自动开启共享。
    pub autostart: bool,
}

/// 界面配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// 语言：auto | zh-CN | en
    pub language: String,
    /// 主题：auto | dark | light
    pub theme: String,
    /// 默认视图：grid | list
    pub view: String,
    /// 启动时打开：last | home | 具体路径
    pub startup: String,
    /// 上次所在目录，`startup = "last"` 时使用。
    pub last_dir: Option<String>,
    /// 列表中是否显示缩略图。
    pub thumbnails: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            language: String::from("auto"),
            theme: String::from("auto"),
            view: String::from("grid"),
            startup: String::from("last"),
            last_dir: None,
            thumbnails: true,
        }
    }
}

/// 安全配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Security {
    /// 闲置多少秒后自动锁定；0 表示从不。
    ///
    /// 「闲置」的判定不能只看输入设备：用户看两小时加密电影时全程不碰
    /// 鼠标键盘，那不是闲置。播放中必须算活跃，否则会把正在看的片子锁掉。
    pub auto_lock_secs: u64,
    /// 切到后台（移动端切应用 / 桌面最小化）时**立即**锁定。
    ///
    /// 与 `auto_lock_secs` 是两条独立的策略：
    /// - 关闭（默认）时，切后台只等于「开始闲置」，回到闲置计时里结算，
    ///   **播放中不算闲置**——看片时切出去回消息不会被锁；
    /// - 开启时，切后台的瞬间就锁（播放中也锁）。这面向任务切换器截图会
    ///   暴露内容的强安全诉求，代价是回到前台总要重新解锁一次。
    pub lock_on_background: bool,
    /// 用外部程序打开后，关闭时立即清理临时明文。
    ///
    /// ⚠️ **当前没有任何代码读这个字段**，改它不会改变任何行为。
    ///
    /// 保留它是因为加密文件的「外部程序打开」链路尚未接上
    /// （`omy_core::fsatomic::TempPlaintext` 已就绪，缺的是 GUI 那一段）。
    /// 设置页里对应的开关已**移除**而不是禁用——摆一个点了没用的安全开关，
    /// 比没有这个开关更糟：用户会以为临时明文已经被擦掉了。
    ///
    /// 接上那条链路时**必须同时消费这个字段**，否则它会一直是个骗人的承诺。
    pub wipe_temp_plaintext: bool,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            // 默认不自动锁定：这个项目的解锁成本是一次 Argon2（约 180 ms
            // 起，高档位更久），默认开启会让「看两个文件就要重输一次密码」
            // 成为常态。让用户按自己的场景选。
            auto_lock_secs: 0,
            // 默认关闭：切后台按「闲置」处理，交给 auto_lock_secs 计时，
            // 播放中还会豁免；只有显式开启才在切后台瞬间立即锁定。
            lock_on_background: false,
            wipe_temp_plaintext: true,
        }
    }
}

/// 第三方密码管理器配置。
///
/// 这里只保存非秘密元数据。恢复 KeePassXC 授权所需的 association key 交给
/// `omy-secret` 的系统凭据库；把它也写进 TOML 会让任意能读配置的进程继承
/// 用户已经批准过的数据库访问权。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PasswordManagers {
    /// KeePassXC-Browser provider。
    pub keepassxc: KeePassXc,
}

/// KeePassXC provider 配置。
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeePassXc {
    /// 用户显式选择的 `keepassxc-proxy`；为空时自动探测。
    pub proxy_path: Option<String>,
    /// 已配对数据库的非秘密索引。
    pub associations: Vec<KeePassXcAssociation>,
}

/// 一个已配对 KeePassXC 数据库的非秘密部分。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeePassXcAssociation {
    /// 数据库 root UUID 的哈希。
    pub database_hash: String,
    /// 用户在 KeePassXC 确认窗口中输入的关联名。
    pub id: String,
}

/// 远程位置与缓存配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Remote {
    /// 密文缓存上限，字节；0 表示不限制。
    pub cache_limit: u64,
    /// 缓存目录；为空则用 [`cache_dir`]。
    pub cache_dir: Option<String>,
    /// 退出时清空缓存。
    pub clear_cache_on_exit: bool,
    /// 仅在 Wi-Fi 下缓存（移动端）。
    pub cache_wifi_only: bool,
    /// 扫描并发请求数。
    ///
    /// 这不是性能调参，是**风控边界**：调高容易被服务端限流甚至封禁。
    pub scan_concurrency: usize,
    /// 扫描时是否只看 `.omy` 扩展名。
    ///
    /// 为 `false` 时能识别伪装文件，但远程每个文件都要多一次请求。
    pub scan_omy_only: bool,
    /// Telegram 全局代理模式：`system` 自动读取当前系统代理，`manual` 使用固定地址。
    pub telegram_proxy_mode: String,
    /// Telegram 全局手动代理。仅 `telegram_proxy_mode = "manual"` 时生效。
    pub telegram_proxy: String,
    /// 已保存的远程位置。
    ///
    /// 密码不在这里——它经 `omy-secret` 加密后放在 [`SavedPlace::secret`]，
    /// 解开需要本机凭据库里的密钥。配置文件本身被拷走也用不了。
    #[serde(default)]
    pub places: Vec<SavedPlace>,
    /// 用户自己的 Telegram `api_id`；`None` 表示用内置那一份。
    ///
    /// # 为什么要给这个口子
    ///
    /// 内置的是 Telegram Desktop 的 2040（DEC-22）。它一旦触发
    /// `API_ID_PUBLISHED_FLOOD`，**所有用户同时连不上，而且没有任何自救
    /// 办法**——只能等我们发新版本。让用户能填自己申请的那一对，
    /// 就是这条逃生口。
    #[serde(default)]
    pub telegram_api_id: Option<i32>,
    /// 与 [`Self::telegram_api_id`] 配对的 `api_hash`（密文信封）。
    ///
    /// **它是凭据，不是配置项**，所以和 [`SavedPlace::secret`] 一样经
    /// `omy-secret` 加密后存放，不明文躺在配置文件里。
    ///
    /// 两个字段要么都有要么都没有：只填一半时按「没填」处理并报错，
    /// 而不是静默回落到内置值——那会让用户以为自己那对生效了。
    #[serde(default)]
    pub telegram_api_hash: Option<String>,
}

/// 一个持久化的远程位置。
///
/// # 为什么密码单独走信封而不整条记录加密
///
/// 名字、URL、用户名要在**没有密钥时**也能显示：Linux 上没有 Secret
/// Service、或用户换了机器时，界面仍应列出这些位置并提示「需要重新
/// 登录」，而不是一片空白让人以为配置丢了。
///
/// 所以只有密码是密文，其余明文。这也意味着 URL 和用户名在配置文件里
/// 可见——它们不是凭据，但确实暴露「你在用哪个 NAS」，这一点在
/// 设计文档里写明，不要让人误以为整条记录都被保护了。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedPlace {
    /// 稳定标识，跨重启不变。
    pub id: String,
    /// 显示名。
    pub name: String,
    /// 驱动类型，目前只有 `webdav`。
    pub kind: String,
    /// 服务地址。Telegram 位置不使用此字段，且空值不写入配置。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// 用户名，匿名时为空。
    #[serde(default)]
    pub username: String,
    /// 厂商标识，用于兼容性调整。
    #[serde(default)]
    pub vendor: String,
    /// 是否允许写入。
    #[serde(default)]
    pub writable: bool,
    /// 加密后的密码信封（`omy-secret` 的 `Envelope` 序列化结果）。
    ///
    /// 类型用 `toml::Value` 而不是具体结构：`omy-config` 不该依赖
    /// `omy-secret`（那会让 CLI 也被迫链上凭据库），而信封的字段将来
    /// 可能随版本增减，在这里放一个不透明值正好把两者解耦。
    ///
    /// 为 `None` 表示这个位置没有密码（匿名），或密钥已丢失需要重新登录。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<toml::Value>,
    /// Telegram 账号的服务端 user id，用于登录去重。
    ///
    /// 只有 Telegram 位置有；WebDAV 为 `None`。它是**公开的数字 id**、
    /// 不是凭据，可以明文存。判据必须用它而不是昵称：昵称会重复、会被用户
    /// 改掉，只有 user id 在服务端唯一，才能判断「这两个位置是不是同一个账号」。
    ///
    /// `#[serde(default)]`：老配置里没有这个字段，反序列化时回落为 `None`
    /// ——升级后已有位置照样读得出，只是暂时不带 user id，下次登录/连接
    /// 拿到后再补上。少了这个默认，老配置会直接反序列化失败、所有位置丢失。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
}

impl Default for Remote {
    fn default() -> Self {
        Self {
            // 2 GiB：够放下一部 4K 电影的可观片段，又不会让用户某天发现
            // 磁盘被吃掉几十 GB
            cache_limit: 2 * 1024 * 1024 * 1024,
            cache_dir: None,
            clear_cache_on_exit: false,
            cache_wifi_only: false,
            scan_concurrency: 8,
            scan_omy_only: true,
            // Telegram 默认在每次建连时读取当前系统代理；手动地址是显式覆盖。
            telegram_proxy_mode: String::from("system"),
            telegram_proxy: String::new(),
            places: Vec::new(),
            // 默认用内置 api_id。填自己那一对是逃生口，不是常规配置
            telegram_api_id: None,
            telegram_api_hash: None,
        }
    }
}

/// 加载结果，附带解析时发现的未知字段。
#[derive(Debug, Clone)]
pub struct Loaded {
    /// 解析出的配置。
    pub config: Config,
    /// 文件里存在、但本版本不认识的键（点分路径，如 `defaults.kdf_profil`）。
    ///
    /// 可能是拼写错误，也可能是更高版本写的。调用方自行决定提示与否——
    /// CLI 适合提醒（多半是手写配置时拼错了），GUI 不必打扰用户。
    pub unknown_keys: Vec<String>,
}

impl Config {
    /// 从默认位置加载。文件不存在时返回内置默认值。
    ///
    /// # Errors
    ///
    /// 文件存在但读取或解析失败时返回错误。
    pub fn load() -> Result<Self> {
        Ok(Self::load_checked()?.config)
    }

    /// 从指定路径加载。
    ///
    /// 与 [`Config::load`] 的区别：这里**文件不存在也报错**。用户明确
    /// 指定的文件被静默忽略，会让人以为配置生效了。
    ///
    /// # Errors
    ///
    /// 读取或解析失败时返回错误。
    pub fn load_from(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::Io { path: path.to_path_buf(), op: "读取", source: e })?;
        parse(&text, path).map(|l| l.config)
    }

    /// 写回默认位置。
    ///
    /// # Errors
    ///
    /// 无法确定路径、序列化失败或写盘失败时返回错误。
    pub fn save(&self) -> Result<()> {
        let path = config_path().ok_or(Error::NoPath)?;
        self.save_to(&path)
    }

    /// 写回指定路径，**保留文件中本版本不认识的键**。
    ///
    /// # 为什么必须保留
    ///
    /// 直接把结构体序列化后整体覆盖，会把更高版本写入的键抹掉。用户在
    /// 新旧两个版本之间来回切换一次，新版本的设置就永久丢了——而且丢得
    /// 悄无声息，没有任何报错。
    ///
    /// 所以这里先把原文件解析成 TOML 表，只覆盖自己认识的键，其余原样留下。
    ///
    /// # 注释会丢失
    ///
    /// TOML 序列化不保留注释。手写过配置文件的用户写的注释，在设置页保存
    /// 一次之后就没有了。这一点无法避免（保留注释需要 format-preserving
    /// 的解析器），已在文档中写明。
    ///
    /// # Errors
    ///
    /// 序列化或写盘失败时返回错误。
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let mut table = match std::fs::read_to_string(path) {
            Ok(text) => text.parse::<toml::Table>().unwrap_or_default(),
            // 文件不存在是正常的首次保存；其它读取错误（权限等）也不该
            // 阻止写入——写盘本身会给出更准确的错误
            Err(_) => toml::Table::new(),
        };

        let mine = toml::Table::try_from(self)?;
        merge_into(&mut table, mine);

        let text = toml::to_string_pretty(&table)?;

        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| Error::Io { path: dir.to_path_buf(), op: "创建目录", source: e })?;
        }

        // 用 core 的原子写：配置写到一半断电会留下半截文件，
        // 下次启动直接解析失败——而用户完全不知道为什么应用起不来了
        omy_core::fsatomic::write_atomic(path, text.as_bytes()).map_err(|e| Error::Io {
            path: path.to_path_buf(),
            op: "写入",
            source: std::io::Error::other(e.to_string()),
        })
    }

    /// 加载并回报未知字段。
    ///
    /// # Errors
    ///
    /// 文件存在但读取或解析失败时返回错误。
    pub fn load_checked() -> Result<Loaded> {
        let Some(path) = config_path() else {
            return Ok(Loaded { config: Self::default(), unknown_keys: Vec::new() });
        };
        if !path.exists() {
            return Ok(Loaded { config: Self::default(), unknown_keys: Vec::new() });
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| Error::Io { path: path.clone(), op: "读取", source: e })?;
        parse(&text, &path)
    }
}

/// 解析文本并找出未知键。
fn parse(text: &str, path: &Path) -> Result<Loaded> {
    let config: Config = toml::from_str(text)
        .map_err(|e| Error::Parse { path: path.to_path_buf(), source: e })?;
    let unknown_keys = text
        .parse::<toml::Table>()
        .map(|raw| unknown_keys(&raw, &config))
        .unwrap_or_default();
    Ok(Loaded { config, unknown_keys })
}

/// 用 `src` 的键覆盖 `dst`，逐层合并而不是整表替换。
///
/// 整表替换会把同一个 section 下的未知键连带删掉——那正是要避免的。
fn merge_into(dst: &mut toml::Table, src: toml::Table) {
    for (k, v) in src {
        match (dst.get_mut(&k), v) {
            (Some(toml::Value::Table(old)), toml::Value::Table(new)) => merge_into(old, new),
            (_, v) => {
                dst.insert(k, v);
            }
        }
    }
}

/// 找出文件里存在、但序列化本结构体时不会产生的键。
fn unknown_keys(raw: &toml::Table, cfg: &Config) -> Vec<String> {
    let Ok(known) = toml::Table::try_from(cfg) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    diff_keys(raw, &known, "", &mut out);
    out.sort();
    out
}

fn diff_keys(raw: &toml::Table, known: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in raw {
        let full = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match (known.get(k), v) {
            (None, _) => out.push(full),
            (Some(toml::Value::Table(kt)), toml::Value::Table(rt)) => {
                diff_keys(rt, kt, &full, out);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("omy_cfg_{}_{}", std::process::id(), name));
        std::fs::create_dir_all(&d).expect("建临时目录");
        d.join("config.toml")
    }

    /// 默认值必须与 CLI 原有的一致。
    ///
    /// 不这样会怎样：同一个键在 CLI 与 GUI 下默认值不同，用户看到两边
    /// 行为不一致却找不到原因——配置文件里根本没有这一项。
    #[test]
    fn defaults_match_cli_documented_values() {
        let c = Config::default();
        assert_eq!(c.defaults.kdf_profile, "interactive");
        assert_eq!(c.defaults.cipher, "xchacha20");
        assert_eq!(c.defaults.chunk_size, "256K");
        assert_eq!(c.defaults.name_mode, "encrypt");
        assert_eq!(c.defaults.original_action, "keep");
        assert!(!c.compress.enabled);
        assert_eq!(c.compress.level, 3);
        assert_eq!(c.scan.max_depth, 8);
        assert_eq!(c.ui.language, "auto");
    }

    /// 缺省片段要回落到默认值，不能把整组清零。
    #[test]
    fn partial_config_keeps_other_defaults() {
        let c: Config = toml::from_str("[compress]\nenabled = true\n").expect("应能解析");
        assert!(c.compress.enabled);
        assert_eq!(c.compress.level, 3, "未指定的项应回落默认");
        assert_eq!(c.defaults.cipher, "xchacha20");
    }

    /// 写回必须保留本版本不认识的键。
    ///
    /// 不这样会怎样：用户在新旧版本间切换一次，新版本写的设置被旧版本
    /// 静默抹掉，且没有任何报错。
    #[test]
    fn save_preserves_unknown_keys() {
        let p = tmp("unknown");
        std::fs::write(
            &p,
            "[defaults]\nkdf_profile = \"moderate\"\n\n\
             [future]\nsomething_new = 42\n\n\
             [ui]\nlanguage = \"en\"\nfuture_flag = true\n",
        )
        .expect("写初始文件");

        let mut c = Config::load_from(&p).expect("应能加载");
        assert_eq!(c.defaults.kdf_profile, "moderate");
        c.ui.language = String::from("zh-CN");
        c.save_to(&p).expect("应能保存");

        let text = std::fs::read_to_string(&p).expect("读回");
        assert!(text.contains("something_new"), "整个未知 section 不能丢");
        assert!(text.contains("future_flag"), "已知 section 里的未知键也不能丢");
        assert!(text.contains("zh-CN"), "改动要写进去");

        std::fs::remove_dir_all(p.parent().expect("父目录")).ok();
    }

    /// 未知键要被回报，以便 CLI 提示用户可能拼错了。
    #[test]
    fn unknown_keys_are_reported() {
        let raw = "[defaults]\nkdf_profil = \"mobile\"\n"
            .parse::<toml::Table>()
            .expect("解析原始表");
        let keys = unknown_keys(&raw, &Config::default());
        assert_eq!(keys, vec![String::from("defaults.kdf_profil")]);
    }

    /// 拼错的键不能让整个文件解析失败。
    ///
    /// 不这样会怎样：高版本写的新键会让低版本完全读不了配置，
    /// 用户的全部设置一次性失效。
    #[test]
    fn unknown_key_does_not_break_parsing() {
        let c: Config = toml::from_str("[defaults]\nkdf_profil = \"mobile\"\n")
            .expect("未知键不应导致解析失败");
        assert_eq!(c.defaults.kdf_profile, "interactive", "拼错的键不生效");
    }

    /// 往返一次后内容必须完全相同。
    #[test]
    fn roundtrip_is_stable() {
        let p = tmp("roundtrip");
        let mut c = Config::default();
        c.remote.cache_limit = 5 * 1024 * 1024 * 1024;
        c.security.auto_lock_secs = 600;
        c.ui.view = String::from("list");
        c.save_to(&p).expect("保存");

        let back = Config::load_from(&p).expect("加载");
        assert_eq!(back, c, "往返后应完全一致");

        std::fs::remove_dir_all(p.parent().expect("父目录")).ok();
    }

    /// 显式指定的文件不存在必须报错。
    #[test]
    fn explicit_missing_path_errors() {
        let p = std::env::temp_dir().join("omy_no_such_config_xyz.toml");
        assert!(Config::load_from(&p).is_err(), "显式指定的缺失文件应报错");
    }

    /// 安全默认值：不主动自动锁定。
    ///
    /// 自动锁定（按闲置时间）与切后台立即锁定默认都关闭，让用户按场景显式
    /// 开启；切后台默认只是「开始闲置」，仍受播放豁免保护。
    #[test]
    fn security_defaults_are_intentional() {
        let s = Security::default();
        assert_eq!(s.auto_lock_secs, 0, "默认不按时间自动锁定");
        assert!(!s.lock_on_background, "切后台默认按闲置处理，不立即锁");
        assert!(s.wipe_temp_plaintext);
    }

    /// KeePassXC 关联的 bearer key 绝不能出现在配置结构里。
    #[test]
    fn password_manager_config_contains_only_public_metadata() {
        let mut c = Config::default();
        c.password_managers.keepassxc.proxy_path = Some(String::from("/opt/KeePassXC/proxy"));
        c.password_managers.keepassxc.associations.push(KeePassXcAssociation {
            database_hash: String::from("db-hash"),
            id: String::from("omy-test"),
        });
        let text = toml::to_string(&c).expect("配置应可序列化");
        // 不这样会怎样：为了省一次 keyring 调用把关联 key 顺手塞进 TOML，
        // 任何读配置的进程都能继承用户授予 KeePassXC 的访问权。
        assert!(text.contains("database_hash"), "需要保留查找数据库的公开索引");
        assert!(text.contains("omy-test"), "需要保留用户可辨认的关联名");
        assert!(!text.contains("association_key"), "配置不得出现关联 bearer key");
    }

    /// 缓存默认 2 GiB，且并发默认 8。
    #[test]
    fn remote_defaults() {
        let r = Remote::default();
        assert_eq!(r.cache_limit, 2 * 1024 * 1024 * 1024);
        assert_eq!(r.scan_concurrency, 8);
        assert!(r.scan_omy_only, "默认只扫 .omy，避免远程逐个探测");
        assert_eq!(r.telegram_proxy_mode, "system", "Telegram 默认跟随系统代理");
        assert!(r.telegram_proxy.is_empty(), "自动模式不应携带手动代理地址");
    }

    /// 合并是逐层的，不能整表替换。
    #[test]
    fn merge_is_recursive() {
        let mut dst = "[ui]\nkeep_me = 1\nlanguage = \"en\"\n"
            .parse::<toml::Table>()
            .expect("解析 dst");
        let src = "[ui]\nlanguage = \"zh-CN\"\n".parse::<toml::Table>().expect("解析 src");
        merge_into(&mut dst, src);
        let ui = dst.get("ui").and_then(toml::Value::as_table).expect("ui 表");
        assert!(ui.contains_key("keep_me"), "同组里的其它键不能被整表替换掉");
        assert_eq!(ui.get("language").and_then(toml::Value::as_str), Some("zh-CN"));
    }
}
