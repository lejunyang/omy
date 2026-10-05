//! 已配对设备与本机身份的持久化。
//!
//! # 存的东西里有私钥，所以整个文件必须加密
//!
//! 需要落盘的有两类：
//!
//! | 内容 | 泄露后果 |
//! |---|---|
//! | 本机静态**私钥** | 攻击者可冒充本设备连接他人 |
//! | 已配对设备列表 | 暴露"这台机器和哪些设备配过对"的关系图 |
//!
//! 第一条决定了不能明文存。既然项目本身就是加密工具，直接复用
//! `omy_core` 的文件格式——不另造一套密钥管理，也让这份文件享受
//! 同样的两级 KDF 与 AEAD 保护。
//!
//! 落盘走 [`omy_core::fsatomic::write_atomic`]：写配对表的时刻正好是
//! 用户刚配对完的时刻，此时崩溃或断电若留下半个文件，用户会失去
//! 全部已配对设备且不知情。原子写保证要么是旧的完整内容，要么是新的。
//!
//! # 为什么不用 JSON
//!
//! 与 `omy_core::container` 同样的理由：手写紧凑二进制，解析时每个
//! 长度字段都立刻对照剩余字节校验，不给 `Vec::with_capacity` 传入
//! 未经检查的数字。这份文件虽然来自本地磁盘而非网络，但磁盘上的
//! 字节同样可能被篡改或损坏。

use crate::error::{NetError, Result};
use crate::handshake::PairedDevice;
use std::path::{Path, PathBuf};

/// 存储文件的格式版本。
///
/// 与线路协议版本分开：存储格式的演进节奏和协议不同，
/// 混用一个版本号会导致"改了存储格式就得升协议版本"。
const STORE_VERSION: u8 = 1;

/// 单个设备记录允许的最大字节数。
///
/// 公钥 32 + 名字最多 63 + 时间戳 8 + 长度前缀若干，128 足够。
/// 设一个紧的上限，让畸形数据尽早失败。
const MAX_RECORD: usize = 128;

/// 一台已配对设备的完整记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// 对方静态公钥，Noise IK 用它。
    pub public_key: Vec<u8>,
    /// 对方设备名。**由对方自称，不可信**，仅用于界面显示。
    pub name: String,
    /// 配对时间（Unix 秒）。
    pub paired_at: u64,
    /// 会话到期时间（Unix 秒）。`0` 表示永不过期。
    ///
    /// 到期后需要重新配对。默认 24 小时（文档 §5.3）。
    pub expires_at: u64,
}

impl DeviceRecord {
    /// 从刚完成的配对创建记录。
    #[must_use]
    pub fn from_pairing(d: &PairedDevice, valid_for: Option<std::time::Duration>) -> Self {
        let now = now_secs();
        let expires_at = valid_for.map_or(0, |dur| now.saturating_add(dur.as_secs()));
        Self {
            public_key: d.public_key.clone(),
            name: d.name.clone(),
            paired_at: now,
            expires_at,
        }
    }

    /// 设备指纹。
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 8] {
        crate::discovery::fingerprint(&self.public_key)
    }

    /// 在给定时刻是否已过期。
    ///
    /// 显式传入时刻而不是内部取当前时间：这样测试可以验证过期逻辑
    /// 本身，而不必真的等上 24 小时或去改系统时钟。
    #[must_use]
    pub fn is_expired_at(&self, now: u64) -> bool {
        self.expires_at != 0 && now >= self.expires_at
    }

    /// 现在是否已过期。
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.is_expired_at(now_secs())
    }
}

/// 本机身份与已配对设备的集合。
pub struct Store {
    /// 本机静态密钥对。
    keypair: crate::channel::StaticKeypair,
    /// 本机设备名，会广播给局域网。
    device_name: String,
    /// 已配对设备，按指纹索引。
    devices: Vec<DeviceRecord>,
}

impl Store {
    /// 新建一份身份（首次运行时）。
    ///
    /// # Errors
    /// 设备名非法或随机源不可用时返回错误。
    pub fn create(device_name: &str) -> Result<Self> {
        crate::discovery::validate_device_name(device_name)?;
        Ok(Self {
            keypair: crate::channel::StaticKeypair::generate()?,
            device_name: device_name.to_owned(),
            devices: Vec::new(),
        })
    }

    /// 本机公钥。
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        &self.keypair.public
    }

    /// 本机密钥对，用于建立信道。
    #[must_use]
    pub fn keypair(&self) -> &crate::channel::StaticKeypair {
        &self.keypair
    }

    /// 本机设备名。
    #[must_use]
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// 改设备名。
    ///
    /// # Errors
    /// 名字非法时返回错误。
    pub fn set_device_name(&mut self, name: &str) -> Result<()> {
        crate::discovery::validate_device_name(name)?;
        // clear + push_str 复用已有分配，避免丢弃旧 String 再分配新的
        self.device_name.clear();
        self.device_name.push_str(name);
        Ok(())
    }

    /// 本机指纹。
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 8] {
        self.keypair.fingerprint()
    }

    /// 全部已配对设备（含已过期的）。
    #[must_use]
    pub fn devices(&self) -> &[DeviceRecord] {
        &self.devices
    }

    /// 仍然有效的设备。
    #[must_use]
    pub fn active_devices(&self) -> Vec<&DeviceRecord> {
        let now = now_secs();
        self.devices.iter().filter(|d| !d.is_expired_at(now)).collect()
    }

    /// 添加或更新一台设备。
    ///
    /// 同一公钥重复配对时**覆盖**旧记录而非追加：重新配对的语义就是
    /// "刷新这台设备的授权"，留两条记录会让吊销时漏掉一条。
    pub fn upsert(&mut self, rec: DeviceRecord) {
        if let Some(slot) = self
            .devices
            .iter_mut()
            .find(|d| d.public_key == rec.public_key)
        {
            *slot = rec;
        } else {
            self.devices.push(rec);
        }
    }

    /// 按指纹查设备。
    #[must_use]
    pub fn find_by_fingerprint(&self, fp: &[u8; 8]) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| &d.fingerprint() == fp)
    }

    /// 按公钥查设备。
    ///
    /// 服务端在握手后**必须**用这个方法核对对方身份。握手成功只证明
    /// 对方持有某个私钥，不证明那是已配对的设备。
    #[must_use]
    pub fn find_by_public_key(&self, pk: &[u8]) -> Option<&DeviceRecord> {
        self.devices.iter().find(|d| d.public_key == pk)
    }

    /// 判断对方是否为**当前有效**的已配对设备。
    ///
    /// 这是服务端应当调用的唯一授权判断入口：同时检查"认识"与"没过期"。
    /// 分成两步调用容易漏掉过期检查。
    #[must_use]
    pub fn is_authorized(&self, public_key: &[u8]) -> bool {
        self.find_by_public_key(public_key)
            .is_some_and(|d| !d.is_expired())
    }

    /// 吊销一台设备。
    ///
    /// 返回是否确实移除了记录。
    pub fn revoke(&mut self, public_key: &[u8]) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| d.public_key != public_key);
        self.devices.len() != before
    }

    /// 吊销全部设备。
    pub fn revoke_all(&mut self) {
        self.devices.clear();
    }

    /// 清掉已过期的记录，返回清掉的条数。
    pub fn purge_expired(&mut self) -> usize {
        let now = now_secs();
        let before = self.devices.len();
        self.devices.retain(|d| !d.is_expired_at(now));
        before.saturating_sub(self.devices.len())
    }

    /// 复制一份，用于交给服务端主循环。
    ///
    /// # 为什么不 `derive(Clone)`
    ///
    /// `Store` 含**静态私钥**。让它随手可 clone，等于邀请调用方到处
    /// 复制私钥而不自知——每一份副本都是一处泄露面，而且 `Drop` 时
    /// 各自擦除的时机也不受控。
    ///
    /// 但服务端主循环确实需要独立持有一份（它要在另一个任务里查授权，
    /// 生命周期与调用方无关）。与其让调用方用 `from_parts` 手工拼一个
    /// ——那样很容易漏掉设备列表，症状是「已配对的设备连不上」——
    /// 不如给一个名字**明确说明用途**的方法。
    ///
    /// 名字里带 `for_serve` 是刻意的：看到调用点就知道这份副本要
    /// 交给谁，而不是一个语义模糊的 `clone()`。
    #[must_use]
    pub fn duplicate_for_serve(&self) -> Self {
        Self {
            keypair: crate::channel::StaticKeypair::from_parts(
                self.keypair.public.clone(),
                self.keypair.private_bytes().to_vec(),
            ),
            device_name: self.device_name.clone(),
            devices: self.devices.clone(),
        }
    }

    /// 编码为明文字节（随后会被加密）。
    fn encode(&self) -> Result<Vec<u8>> {
        let mut out = vec![STORE_VERSION];

        // 本机身份
        push_bytes(&mut out, &self.keypair.public)?;
        push_bytes(&mut out, self.keypair.private_bytes())?;
        push_bytes(&mut out, self.device_name.as_bytes())?;

        // 设备列表
        let n = u32::try_from(self.devices.len()).map_err(|_| NetError::MalformedFrame)?;
        out.extend_from_slice(&n.to_le_bytes());
        for d in &self.devices {
            push_bytes(&mut out, &d.public_key)?;
            push_bytes(&mut out, d.name.as_bytes())?;
            out.extend_from_slice(&d.paired_at.to_le_bytes());
            out.extend_from_slice(&d.expires_at.to_le_bytes());
        }
        Ok(out)
    }

    /// 从明文字节解码。
    fn decode(buf: &[u8]) -> Result<Self> {
        let mut r = Cursor { buf, pos: 0 };

        let ver = r.u8()?;
        if ver != STORE_VERSION {
            return Err(NetError::VersionMismatch {
                theirs: u16::from(ver),
                ours: u16::from(STORE_VERSION),
            });
        }

        let public = r.bytes()?;
        let private = r.bytes()?;
        if public.len() != 32 || private.len() != 32 {
            return Err(NetError::MalformedFrame);
        }
        let name_raw = r.bytes()?;
        let device_name =
            String::from_utf8(name_raw).map_err(|_| NetError::MalformedFrame)?;
        crate::discovery::validate_device_name(&device_name)
            .map_err(|_| NetError::MalformedFrame)?;

        let n = r.u32()?;
        // 不按 n 预分配：它来自文件内容。每轮的 bytes() 会自然限制真实
        // 条目数——声称有 40 亿条但只剩几字节的文件会在第一轮就失败
        let mut devices = Vec::new();
        for _ in 0..n {
            let public_key = r.bytes()?;
            if public_key.len() != 32 {
                return Err(NetError::MalformedFrame);
            }
            let nm = r.bytes()?;
            if nm.len() > crate::discovery::MAX_DEVICE_NAME {
                return Err(NetError::MalformedFrame);
            }
            let dev_name = String::from_utf8(nm).map_err(|_| NetError::MalformedFrame)?;
            crate::discovery::validate_device_name(&dev_name)
                .map_err(|_| NetError::MalformedFrame)?;
            let paired_at = r.u64()?;
            let expires_at = r.u64()?;
            devices.push(DeviceRecord {
                public_key,
                name: dev_name,
                paired_at,
                expires_at,
            });
        }
        r.finish()?;

        Ok(Self {
            keypair: crate::channel::StaticKeypair::from_parts(public, private),
            device_name,
            devices,
        })
    }

    /// 加密保存到文件。
    ///
    /// # Errors
    /// 编码、加密或写盘失败时返回错误。
    pub fn save(&self, path: &Path, password: &[u8]) -> Result<()> {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};

        let plain = self.encode()?;
        // 每次保存都换新 salt：这份文件会被反复重写，
        // 固定 salt 会让多个版本共享同一 KEK
        let mut salt = [0u8; 16];
        {
            use rand::RngCore as _;
            rand::thread_rng().fill_bytes(&mut salt);
        }
        let params = Argon2Params::INTERACTIVE;
        let kek = Kek::from_password(password, &salt, params)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        let opts = EncryptOptions {
            // 不存文件名：这份文件的名字是固定的，存了反而多一处冗余。
            // 更重要的是它不该被当成用户数据在库里列出来
            filename: None,
            argon2: params,
            ..EncryptOptions::default()
        };
        let enc = encrypt(&plain, &[kek], &salt, &opts, &RandomMaterial::generate())
            .map_err(|e| NetError::Discovery(e.to_string()))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // 原子写：写配对表的时刻正好是刚配对完的时刻，
        // 此时留下半个文件会让用户静默失去全部已配对设备
        omy_core::fsatomic::write_atomic(path, &enc.bytes)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        // 尽力收紧权限。失败不致命——文件本身是加密的，
        // 权限只是纵深防御的一层
        let _ = omy_core::fsatomic::restrict_permissions(path);
        Ok(())
    }

    /// 从文件解密加载。
    ///
    /// # Errors
    /// 文件不存在、密码错误或内容损坏时返回错误。
    pub fn load(path: &Path, password: &[u8]) -> Result<Self> {
        let data = std::fs::read(path)?;
        let opened = omy_core::file::open_with_password(&data, password)
            .map_err(|_| NetError::SessionInvalid)?;
        let plain = opened
            .decrypt_all(&data)
            .map_err(|_| NetError::SessionInvalid)?;
        Self::decode(&plain)
    }

    /// 加载已有身份，不存在则新建并保存。
    ///
    /// # Errors
    /// 读写失败、密码错误或内容损坏时返回错误。
    pub fn load_or_create(path: &Path, password: &[u8], device_name: &str) -> Result<Self> {
        if path.exists() {
            return Self::load(path, password);
        }
        let s = Self::create(device_name)?;
        s.save(path, password)?;
        Ok(s)
    }
}

/// 默认的存储路径。
///
/// 优先级从高到低：
///
/// 1. 环境变量 `OMY_DEVICE_STORE` 显式指定的路径；
/// 2. 便携模式：与配置文件同根，放在可执行文件旁的 `omy-data` 下
///    （该目录由 omy-config 实测可写后才会采用）；
/// 3. 系统配置目录下的 `omy/devices.omy`。
///
/// # 为什么要有环境变量这条路
///
/// 加这个开关的直接原因是一次真实的事故：端到端测试想用改写
/// `APPDATA` 的办法隔离测试环境，结果**没有隔离住**——
/// `dirs::config_dir()` 在 Windows 上走的是 `SHGetKnownFolderPath`
/// 系统调用，不看环境变量。测试身份于是被写进了开发者的真实配置目录。
///
/// 那次事故暴露的是一个更普遍的问题：这个路径此前**完全不可控**。
/// 除了测试，至少还有两种正当需求需要它可控：
///
/// - **便携模式**：把设备库放在 U 盘上随身带
/// - **多身份**：同一台机器上用不同身份连不同的设备组
///
/// 环境变量是这三者共同的、最小的解法。它优先级最高：便携根只是
/// 「没说放哪」时的默认，显式指定必须压过默认。
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    resolve_store_path(std::env::var("OMY_DEVICE_STORE").ok().as_deref())
}

/// 由给定的环境变量值决定设备库路径。
///
/// 抽成纯函数是为了可测：读进程环境变量的版本没法在
/// `unsafe_code = "forbid"` 的 crate 里测（`set_var` 是 unsafe），
/// 而且依赖进程全局状态的测试在并行执行下本就不可靠。
///
/// 便携根与系统配置目录也在这里注入，而不是直接写进决策核心：
/// [`resolve_store_path_with`] 拿不到任何进程状态，测试才能把三个
/// 优先级排错这种缺陷抓出来（见其测试）。
fn resolve_store_path(env_value: Option<&str>) -> Option<PathBuf> {
    resolve_store_path_with(
        env_value,
        omy_config::portable_root().as_deref(),
        dirs::config_dir().as_deref(),
    )
}

/// 生产环境**真正打开设备库前**调用一次：升级场景下把老系统位置的库
/// 一次性迁到便携位置。失败静默——迁移不是开库的前提，开库流程照常走。
///
/// # 为什么不放在 `default_path()` 里
///
/// `default_path()` 会被状态查询高频调用，测试也频繁问它。把「复制用户
/// 真实身份文件」这种副作用放进去，等于让每个测试二进制都把开发者的真实
/// 设备库拷进 `target/`——既是隐私事故，也让测试依赖本机环境。迁移只在
/// 要开库、要生成/加载身份那一刻才需要，故与路径解析显式分开。
///
/// 显式设了 `OMY_DEVICE_STORE` 时不迁移：用户指定的路径是权威位置，
/// 不替他搬东西。
pub fn migrate_legacy_if_needed() {
    // 空白环境变量视同未设，和 resolve_store_path_with 的判定保持一致
    let env = std::env::var("OMY_DEVICE_STORE").ok();
    if env.as_deref().is_some_and(|v| !v.trim().is_empty()) {
        return;
    }
    let Some(root) = omy_config::portable_root() else {
        return;
    };
    let new = root.join("devices.omy");
    let legacy = dirs::config_dir().map(|d| d.join("omy").join("devices.omy"));
    migrate_legacy_store(&new, legacy.as_deref());
}

/// 一次性把老系统位置的设备库迁到便携位置。
///
/// 只做「便携位置没有库、老位置有库」这一种情况下的复制；其余情况原样
/// 返回便携路径。老库**永远不删**——这是复制不是移动，出问题老库还在，
/// 下次启动再试。
///
/// # 为什么要在升级时迁一次
///
/// 便携根是上一次提交才纳入默认路径的。此前设备身份一直在系统配置目录；
/// 升级后 exe 恰好放在可写目录，程序会去便携位置读一个**不存在**的新库，
/// 于是生成一份空白身份——用户所有已配对设备当场失效，而老库静静躺在
/// %APPDATA% 里没人知道。
///
/// # 并发
///
/// 两个进程同时升级启动时都会走到这里。先复制到同目录临时文件再改名：
/// 改名失败说明临时文件没就位（目标已被别的进程迁好，或磁盘/权限问题），
/// 此时若便携位置已有文件就用它，没有就回退到老路径。绝不用自己手里的
/// 副本去覆盖一个可能已经被对端写过的便携库。
fn migrate_legacy_store(new: &Path, legacy: Option<&Path>) -> PathBuf {
    // 便携位置已有库：可能是上次迁好了，也可能用户刚配对完正在用。
    // 任何情况下都不覆盖。
    if new.exists() {
        return new.to_path_buf();
    }
    let Some(legacy) = legacy else {
        return new.to_path_buf();
    };
    if !legacy.is_file() {
        return new.to_path_buf();
    }
    let tmp = new.with_file_name(format!(
        ".{}.migrating",
        new.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("devices.omy")
    ));
    if std::fs::copy(legacy, &tmp).is_err() {
        std::fs::remove_file(&tmp).ok();
        // 回退：这次用老库，下次启动再试。绝不能返回一个还不存在的便携路径
        // ——那会让程序在新位置生成空白身份，老身份被永久搁置。
        return legacy.to_path_buf();
    }
    if std::fs::rename(&tmp, new).is_ok() {
        return new.to_path_buf();
    }
    std::fs::remove_file(&tmp).ok();
    if new.exists() {
        // 别的进程已经迁好：直接用它
        new.to_path_buf()
    } else {
        legacy.to_path_buf()
    }
}

/// 路径决策的纯函数核心：环境变量覆盖 → 便携根 → 系统配置目录。
///
/// `portable_root` 传 `None` 表示 exe 旁目录不可写（或移动端），
/// `config_dir` 传 `None` 表示系统也给不出配置目录。
fn resolve_store_path_with(
    env_value: Option<&str>,
    portable_root: Option<&Path>,
    config_dir: Option<&Path>,
) -> Option<PathBuf> {
    // 空白值当没设：误设成空串时若照单全收，设备库会写到当前工作目录，
    // 位置随启动方式漂移，用户根本找不到自己的身份文件
    if let Some(p) = env_value.filter(|p| !p.trim().is_empty()) {
        return Some(PathBuf::from(p));
    }
    // 便携模式：设备库与 config.toml 同根（exe 旁 omy-data）。
    // portable_root 只在实测可写时才返回 Some，这里不必重复探测。
    // 文件名放根下而不是 data/ 子目录：非便携态它与 config.toml 本就
    // 同在 omy/ 一级，便携态保持这层镜像。
    if let Some(root) = portable_root {
        return Some(root.join("devices.omy"));
    }
    config_dir.map(|d| d.join("omy").join("devices.omy"))
}

/// 当前 Unix 秒。
///
/// 系统时间早于纪元时返回 0——那种情况下过期判断本就没有意义，
/// 但绝不能 panic。
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn push_bytes(out: &mut Vec<u8>, v: &[u8]) -> Result<()> {
    let len = u16::try_from(v.len()).map_err(|_| NetError::MalformedFrame)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(v);
    Ok(())
}

/// 逐字段前进的读取器，全程边界检查。
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(NetError::MalformedFrame)?;
        let s = self.buf.get(self.pos..end).ok_or(NetError::MalformedFrame)?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(*self.take(1)?.first().ok_or(NetError::MalformedFrame)?)
    }
    fn u32(&mut self) -> Result<u32> {
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| NetError::MalformedFrame)?;
        Ok(u32::from_le_bytes(b))
    }
    fn u64(&mut self) -> Result<u64> {
        let b: [u8; 8] = self.take(8)?.try_into().map_err(|_| NetError::MalformedFrame)?;
        Ok(u64::from_le_bytes(b))
    }
    fn bytes(&mut self) -> Result<Vec<u8>> {
        let b: [u8; 2] = self.take(2)?.try_into().map_err(|_| NetError::MalformedFrame)?;
        let len = usize::from(u16::from_le_bytes(b));
        if len > MAX_RECORD {
            return Err(NetError::MalformedFrame);
        }
        Ok(self.take(len)?.to_vec())
    }
    fn finish(self) -> Result<()> {
        if self.pos == self.buf.len() {
            Ok(())
        } else {
            Err(NetError::MalformedFrame)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpfile(tag: &str) -> PathBuf {
        use rand::RngCore as _;
        let r = rand::thread_rng().next_u64();
        std::env::temp_dir().join(format!("omy-store-{tag}-{r:016x}.omy"))
    }

    fn dev(name: &str, pk: u8, expires_at: u64) -> DeviceRecord {
        DeviceRecord {
            public_key: vec![pk; 32],
            name: name.to_owned(),
            paired_at: 1000,
            expires_at,
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mut s = Store::create("我的电脑").expect("创建应成功");
        s.upsert(dev("设备甲", 1, 0));
        s.upsert(dev("设备乙", 2, 99999));

        let enc = s.encode().expect("编码应成功");
        let back = Store::decode(&enc).expect("解码应成功");

        assert_eq!(back.device_name(), "我的电脑");
        assert_eq!(back.public_key(), s.public_key(), "本机公钥应保持");
        assert_eq!(
            back.keypair().private_bytes(),
            s.keypair().private_bytes(),
            "本机私钥应保持——丢了就等于换了身份，所有已配对设备都连不上"
        );
        assert_eq!(back.devices().len(), 2);
        assert_eq!(back.devices()[0].name, "设备甲");
        assert_eq!(back.devices()[1].expires_at, 99999);
    }

    #[test]
    fn save_load_roundtrip() {        let p = tmpfile("roundtrip");
        let mut s = Store::create("台式机").expect("创建应成功");
        s.upsert(dev("笔记本", 7, 0));
        s.save(&p, b"store-password").expect("保存应成功");

        let back = Store::load(&p, b"store-password").expect("加载应成功");
        assert_eq!(back.device_name(), "台式机");
        assert_eq!(back.devices().len(), 1);
        assert_eq!(back.keypair().private_bytes(), s.keypair().private_bytes());
        let _ = std::fs::remove_file(&p);
    }

    /// 落盘的文件必须是加密的——这是整个模块存在的理由。
    #[test]
    fn saved_file_does_not_leak_private_key_or_names() {
        let p = tmpfile("noleak");
        let mut s = Store::create("秘密设备名").expect("创建应成功");
        s.upsert(dev("另一台秘密设备", 9, 0));
        s.save(&p, b"pw").expect("保存应成功");

        let raw = std::fs::read(&p).expect("读文件应成功");

        // 私钥不得出现在磁盘字节里
        let priv_bytes = s.keypair().private_bytes().to_vec();
        assert!(
            !raw.windows(priv_bytes.len()).any(|w| w == priv_bytes.as_slice()),
            "静态私钥绝不能明文落盘——泄露后攻击者可冒充本设备"
        );
        // 设备名同样不得出现
        for needle in ["秘密设备名", "另一台秘密设备"] {
            let n = needle.as_bytes();
            assert!(
                !raw.windows(n.len()).any(|w| w == n),
                "设备名不得明文落盘: {needle}"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn wrong_password_fails() {
        let p = tmpfile("wrongpw");
        let s = Store::create("x").expect("创建应成功");
        s.save(&p, b"right").expect("保存应成功");
        assert!(
            Store::load(&p, b"wrong").is_err(),
            "错误密码必须打不开，否则加密形同虚设"
        );
        assert!(Store::load(&p, b"right").is_ok(), "正确密码应能打开");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn load_or_create_is_idempotent() {
        let p = tmpfile("loadcreate");
        let a = Store::load_or_create(&p, b"pw", "首次").expect("首次应创建");
        let b = Store::load_or_create(&p, b"pw", "忽略").expect("再次应加载");
        assert_eq!(
            a.keypair().private_bytes(),
            b.keypair().private_bytes(),
            "第二次调用必须加载已有身份，而不是生成新的——否则每次启动都换身份"
        );
        assert_eq!(b.device_name(), "首次", "已存在时不应用新名字覆盖");
        let _ = std::fs::remove_file(&p);
    }

    /// 环境变量指定的路径要被采纳，空白值要退回默认。
    ///
    /// 这条测试来自一次真实事故：端到端测试改写 `APPDATA` 想隔离环境，
    /// 但 `dirs::config_dir()` 在 Windows 上走 `SHGetKnownFolderPath`
    /// 系统调用，根本不看环境变量，于是测试身份被写进了开发者的
    /// 真实配置目录。
    ///
    /// 这里测的是纯函数 [`resolve_store_path`] 而不是 [`default_path`]：
    /// 后者要读进程环境变量，而 `set_var` 在 Rust 2024 里是 unsafe，
    /// 本 crate 又是 `unsafe_code = "forbid"`。把「怎么决定路径」的
    /// 逻辑抽成纯函数，既守住了那条禁令，也让测试不必依赖进程全局状态
    /// ——后者在并行测试下本来就不可靠。
    #[test]
    fn env_value_overrides_store_path() {
        assert_eq!(
            resolve_store_path(Some("/tmp/custom/my-devices.omy")),
            Some(PathBuf::from("/tmp/custom/my-devices.omy"))
        );

        // 空白值要被忽略——否则误设成空串会让设备库写到当前工作目录，
        // 位置随启动方式漂移
        for blank in ["", "   ", "\t", "\n"] {
            let got = resolve_store_path(Some(blank));
            assert!(
                got.is_none_or(|p| p.ends_with("devices.omy")),
                "{blank:?} 应退回默认路径"
            );
        }
    }

    /// 显式环境变量必须压过便携根。
    ///
    /// 不这样会怎样：便携根只是「没说放哪」时的默认。用户设
    /// `OMY_DEVICE_STORE` 想把身份指到 U 盘（多身份 / 便携随身），
    /// 若便携根反过来赢，程序仍写到 exe 旁的 omy-data，U 盘里那份
    /// 身份根本用不上，机器上还平白多出一份新身份。
    #[test]
    fn explicit_env_beats_portable_root() {
        let got = resolve_store_path_with(
            Some("/tmp/usb/my-devices.omy"),
            Some(Path::new("/exe/dir/omy-data")),
            Some(Path::new("/home/u/.config")),
        );
        assert_eq!(
            got,
            Some(PathBuf::from("/tmp/usb/my-devices.omy")),
            "显式指定的路径必须被原样采用"
        );
    }

    /// 空白环境变量要被忽略，且**落到便携根**而不是继续掉到配置目录。
    ///
    /// 不这样会怎样：照单全收空白值，设备库会写到进程当前工作目录，
    /// 位置随启动方式漂移；忽略后若错误地跳过便携根，便携用户的设备库
    /// 就会散到系统目录里——U 盘拷走的文件夹里反而没有身份。
    #[test]
    fn blank_env_falls_through_to_portable_root() {
        for blank in ["", "   ", "\t"] {
            let got = resolve_store_path_with(
                Some(blank),
                Some(Path::new("/exe/dir/omy-data")),
                Some(Path::new("/home/u/.config")),
            );
            assert_eq!(
                got,
                Some(Path::new("/exe/dir/omy-data").join("devices.omy")),
                "{blank:?} 应被当作没设，并采用便携根"
            );
        }
    }

    /// 没有显式覆盖时，便携根要压过系统配置目录——这是便携模式的全部意义。
    ///
    /// 不这样会怎样：设备库仍写到 %APPDATA%，用户把 exe 连同加密文件拷到
    /// U 盘后，配对记录留在原机器上；在新机器上首次启动又生成一份新身份，
    /// 已经配对过的设备全部连不上。
    #[test]
    fn portable_root_beats_config_dir_without_override() {
        let got = resolve_store_path_with(
            None,
            Some(Path::new("/exe/dir/omy-data")),
            Some(Path::new("/home/u/.config")),
        );
        assert_eq!(
            got,
            Some(Path::new("/exe/dir/omy-data").join("devices.omy"))
        );
    }

    /// exe 旁目录不可写（装在 Program Files）时，必须回落到系统配置目录，
    /// 且形状是 `omy/devices.omy` 而不是直接散在配置目录根部。
    ///
    /// 不这样会怎样：漏掉 `join("omy")`，设备库会和用户其他几十个应用的
    /// 配置平铺在一起，想备份或清理时根本找不到它；换成系统数据目录则会
    /// 挪动老用户设备库的位置，升级后所有配对记录「凭空消失」。
    /// 两个根都给不出时如实返回 `None`，绝不退化成当前目录。
    #[test]
    fn fallback_lands_under_config_dir_omy() {
        let got = resolve_store_path_with(None, None, Some(Path::new("/home/u/.config")));
        assert_eq!(
            got,
            Some(Path::new("/home/u/.config").join("omy").join("devices.omy"))
        );
        assert_eq!(
            resolve_store_path_with(None, None, None),
            None,
            "两边都不可用时返回 None，而不是写到当前目录"
        );
    }

    /// 生产装配：[`default_path`] 把真实环境接到决策核心上，结果必须落在
    /// 三级之一里（便携根 omy-data 下，或系统配置目录的 omy/ 下）。
    ///
    /// 纯函数测试锁住了优先级与形状，这条只防「装配时接错了参数」——
    /// 比如把便携根错接到配置目录上，决策逻辑本身一行没改，行为却全变了。
    #[test]
    fn default_path_wires_to_one_of_the_three_tiers() {
        let Some(path) = default_path() else {
            return; // 极受限环境，跳过
        };
        assert!(path.ends_with("devices.omy"), "实得 {}", path.display());
        let s = path.to_string_lossy().replace('\\', "/");
        let in_portable = s.contains("omy-data/");
        let in_config = s.ends_with("omy/devices.omy");
        assert!(in_portable || in_config, "路径落在了意料之外的位置: {s}");
    }

    /// 迁移用的临时沙箱目录。
    fn migrate_sandbox(tag: &str) -> PathBuf {
        use rand::RngCore as _;
        let r = rand::thread_rng().next_u64();
        let d = std::env::temp_dir().join(format!("omy-migrate-{tag}-{r:016x}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// 便携位置已有库时，绝不能拿老库覆盖它。
    ///
    /// 不这样会怎样：两个进程同时升级，第二个进程迁完改名时把第一个进程
    /// 刚配对完写好的新库盖掉——用户上一秒配好的设备下一秒就连不上了，
    /// 而且回退到的是几分钟前的旧身份。
    #[test]
    fn existing_portable_store_is_never_overwritten() {
        let root = migrate_sandbox("exist");
        let new = root.join("devices.omy");
        std::fs::write(&new, b"portable-already-paired").unwrap();
        let legacy_parent = migrate_sandbox("exist-leg");
        let legacy = legacy_parent.join("devices.omy");
        std::fs::write(&legacy, b"old-identity").unwrap();

        let got = migrate_legacy_store(&new, Some(&legacy));
        assert_eq!(got, new, "已有库就直接用它");
        assert_eq!(
            std::fs::read(&new).unwrap(),
            b"portable-already-paired",
            "便携库内容一个字节都不能被老库换掉"
        );
        assert!(legacy.exists(), "老库不动");
    }

    /// 便携位置没有库、老位置有库：完整复制，老库保留。
    ///
    /// 不这样会怎样：漏拷或拷一半，密钥都解不开这个容器，升级后所有
    /// 配对设备失效；删了老库则迁移一旦有任何闪失，身份直接消失、
    /// 没有回退余地。
    #[test]
    fn copies_legacy_store_when_portable_absent() {
        let root = migrate_sandbox("copy");
        let new = root.join("devices.omy");
        let legacy_parent = migrate_sandbox("copy-leg");
        let legacy = legacy_parent.join("devices.omy");
        // 字节各不相同，拷错偏移能被测出来（仓库安全软件对长串同值字节敏感，
        // 故用算式生成而不是写 0xAA 重复）。全程 u8 算术：取 i 的低字节再乘/异或，
        // 等价于原来的 u32 截断，但不产生 clippy 标记的可能截断强转。
        let body: Vec<u8> =
            (0..400u32).map(|i| i.to_be_bytes()[3].wrapping_mul(7) ^ 0x3D).collect();
        std::fs::write(&legacy, &body).unwrap();

        let got = migrate_legacy_store(&new, Some(&legacy));
        assert_eq!(got, new, "迁成功后应使用便携路径");
        assert_eq!(std::fs::read(&new).unwrap(), body, "复制必须一字节不差");
        assert!(legacy.exists(), "老库保留：复制不是移动");
    }

    /// 复制失败必须回退到老路径，且不留半成品。
    ///
    /// 不这样会怎样：返回一个还不存在的便携路径，程序就会在那里生成空白
    /// 身份——老身份被永久搁置，用户以为配对记录丢了。临时文件不清理，
    /// 下次启动还可能和改名撞车。
    #[test]
    fn copy_failure_falls_back_to_legacy_path() {
        let base = migrate_sandbox("fail");
        // new 的父目录不存在：copy 到 tmp 必然失败（模拟磁盘/权限问题）
        let new = base.join("no-such-dir").join("devices.omy");
        let legacy_parent = migrate_sandbox("fail-leg");
        let legacy = legacy_parent.join("devices.omy");
        std::fs::write(&legacy, b"old-bytes").unwrap();

        let got = migrate_legacy_store(&new, Some(&legacy));
        assert_eq!(got, legacy, "失败时必须回退到老路径，本次会话还用老身份");
        assert!(!new.exists(), "回退后便携位置不得出现半成品");
        assert!(
            !base.join("no-such-dir").join(".devices.omy.migrating").exists(),
            "临时文件必须清掉"
        );
    }

    /// 并发语义：第二次调用看到便携库已就位，幂等返回且不回盖。
    ///
    /// 不这样会怎样：第二个进程启动时若无视便携库已存在、拿老库再迁一次，
    /// 用户在两台进程之间刚配对的设备会被旧身份覆盖。
    #[test]
    fn second_call_does_not_overwrite_fresh_state() {
        let root = migrate_sandbox("double");
        let new = root.join("devices.omy");
        let legacy_parent = migrate_sandbox("double-leg");
        let legacy = legacy_parent.join("devices.omy");
        std::fs::write(&legacy, b"old-bytes").unwrap();

        // 第一个进程完成迁移
        migrate_legacy_store(&new, Some(&legacy));
        assert_eq!(std::fs::read(&new).unwrap(), b"old-bytes");
        // 用户随后在便携库里配对了新设备，保存了新状态
        std::fs::write(&new, b"paired-new-device-state").unwrap();
        // 第二个进程（或本进程下次启动）再走一次同一逻辑
        let got = migrate_legacy_store(&new, Some(&legacy));
        assert_eq!(got, new);
        assert_eq!(
            std::fs::read(&new).unwrap(),
            b"paired-new-device-state",
            "第二次调用绝不能拿老库回盖新状态"
        );
    }

    /// 老库不存在：只是选好便携路径，不建文件、不报错。
    #[test]
    fn no_legacy_store_means_fresh_portable_path() {
        let root = migrate_sandbox("fresh");
        let new = root.join("devices.omy");
        let got = migrate_legacy_store(&new, Some(&root.join("nonexistent.omy")));
        assert_eq!(got, new);
        assert!(!new.exists(), "这里只决定路径，建库是首次打开时的事");
    }

    /// 交给服务端的副本必须**完整**：身份和设备列表都要带上。    ///
    /// 漏掉设备列表的话，服务端谁都不认识，所有已配对设备连上来都被
    /// 判为未授权。症状是「配对明明成功了却连不上」，而日志只会说
    /// `Unauthorized`——排查时很容易怀疑到握手或配对上去。
    #[test]
    fn duplicate_for_serve_is_complete() {
        let mut s = Store::create("原始设备").expect("创建应成功");
        s.upsert(dev("甲", 1, 0));
        s.upsert(dev("乙", 2, 99999));

        let copy = s.duplicate_for_serve();

        assert_eq!(copy.device_name(), "原始设备");
        assert_eq!(copy.public_key(), s.public_key());
        assert_eq!(
            copy.keypair().private_bytes(),
            s.keypair().private_bytes(),
            "私钥必须一致，否则对方用已配对的公钥握不上手"
        );
        assert_eq!(copy.devices().len(), 2, "设备列表不能漏");
        // 最关键的一条：副本必须能做出与原件相同的授权判断
        assert!(
            copy.is_authorized(&[1u8; 32]),
            "副本必须认识原件认识的设备"
        );
        assert!(!copy.is_authorized(&[9u8; 32]), "陌生设备仍应拒绝");
    }

    #[test]
    fn upsert_replaces_not_appends() {        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(dev("旧名字", 5, 100));
        s.upsert(dev("新名字", 5, 200));
        assert_eq!(s.devices().len(), 1, "同一公钥应覆盖而非追加");
        assert_eq!(s.devices()[0].name, "新名字");
        assert_eq!(s.devices()[0].expires_at, 200);
    }

    #[test]
    fn revoke_works() {
        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(dev("甲", 1, 0));
        s.upsert(dev("乙", 2, 0));
        assert!(s.revoke(&[1u8; 32]), "应确实移除");
        assert_eq!(s.devices().len(), 1);
        assert!(!s.revoke(&[1u8; 32]), "重复吊销应返回 false");
        s.revoke_all();
        assert_eq!(s.devices().len(), 0);
    }

    #[test]
    fn expiry_logic() {
        let never = dev("永久", 1, 0);
        assert!(!never.is_expired_at(u64::MAX), "expires_at=0 表示永不过期");

        let d = dev("限时", 2, 5000);
        assert!(!d.is_expired_at(4999), "到期前应有效");
        assert!(d.is_expired_at(5000), "到期时刻应判为过期");
        assert!(d.is_expired_at(5001), "过期后应无效");
    }

    #[test]
    fn active_devices_excludes_expired() {
        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(dev("有效", 1, 0));
        s.upsert(dev("过期", 2, 1)); // 1970 年就过期了
        assert_eq!(s.devices().len(), 2, "全部列表应含过期项");
        assert_eq!(s.active_devices().len(), 1, "有效列表应排除过期项");
        assert_eq!(s.active_devices()[0].name, "有效");
    }

    /// 授权判断必须同时检查"认识"与"没过期"。
    #[test]
    fn is_authorized_checks_both_identity_and_expiry() {
        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(dev("有效", 1, 0));
        s.upsert(dev("过期", 2, 1));

        assert!(s.is_authorized(&[1u8; 32]), "有效设备应授权");
        assert!(
            !s.is_authorized(&[2u8; 32]),
            "过期设备必须拒绝——只查'认识'会放过过期的"
        );
        assert!(!s.is_authorized(&[3u8; 32]), "陌生设备应拒绝");
    }

    #[test]
    fn purge_expired_counts() {
        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(dev("甲", 1, 0));
        s.upsert(dev("乙", 2, 1));
        s.upsert(dev("丙", 3, 1));
        assert_eq!(s.purge_expired(), 2, "应清掉两条过期记录");
        assert_eq!(s.devices().len(), 1);
    }

    #[test]
    fn from_pairing_sets_expiry() {
        let d = PairedDevice {
            public_key: vec![4u8; 32],
            name: "对方".into(),
        };
        let rec = DeviceRecord::from_pairing(&d, Some(std::time::Duration::from_secs(3600)));
        assert!(rec.expires_at > rec.paired_at, "应设置未来的到期时间");
        assert!(!rec.is_expired(), "刚创建不应过期");

        let forever = DeviceRecord::from_pairing(&d, None);
        assert_eq!(forever.expires_at, 0, "None 表示永不过期");
    }

    #[test]
    fn find_by_fingerprint_works() {
        let mut s = Store::create("x").expect("创建应成功");
        let rec = dev("甲", 1, 0);
        let fp = rec.fingerprint();
        s.upsert(rec);
        assert!(s.find_by_fingerprint(&fp).is_some());
        assert!(s.find_by_fingerprint(&[0xFF; 8]).is_none());
    }

    #[test]
    fn decode_rejects_malformed() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],                        // 空
            vec![STORE_VERSION],           // 只有版本
            vec![99, 1, 2, 3],             // 版本不对
            {
                let mut v = vec![STORE_VERSION];
                v.extend_from_slice(&16u16.to_le_bytes());
                v.extend_from_slice(&[0u8; 16]); // 公钥长度不是 32
                v
            },
        ];
        for c in cases {
            assert!(Store::decode(&c).is_err(), "畸形输入应拒绝: {c:?}");
        }
    }

    /// 声称有海量设备时必须拒绝，而不是尝试预分配。
    ///
    /// 用**空设备列表**的 store：此时 encode 的最后 4 字节正好是设备
    /// 计数，改它才真正改到了目标字段。若用有设备的 store，最后 4 字节
    /// 是末条记录的 `expires_at` 高位，改了根本测不到计数校验。
    #[test]
    fn decode_rejects_huge_count() {
        let s = Store::create("x").expect("创建应成功");
        assert!(s.devices().is_empty(), "本测试依赖空设备列表");
        let mut enc = s.encode().expect("编码应成功");

        // 先确认末 4 字节确实是计数 0
        let tail_start = enc.len().checked_sub(4).expect("编码应至少 4 字节");
        assert_eq!(
            enc.get(tail_start..),
            Some(&0u32.to_le_bytes()[..]),
            "空列表时末 4 字节应为计数 0；布局若变了本测试需同步更新"
        );

        let tail = enc.get_mut(tail_start..).expect("上一步已确认存在");
        tail.copy_from_slice(&u32::MAX.to_le_bytes());

        assert!(
            Store::decode(&enc).is_err(),
            "声称 40 亿条设备必须拒绝而非尝试分配"
        );
    }

    #[test]
    fn decode_rejects_trailing_garbage() {
        let s = Store::create("x").expect("创建应成功");
        let mut enc = s.encode().expect("编码应成功");
        enc.push(0xFF);
        assert!(Store::decode(&enc).is_err(), "尾部多余字节应拒绝");
    }

    #[test]
    fn decode_rejects_control_chars_in_name() {
        let mut s = Store::create("x").expect("创建应成功");
        s.upsert(DeviceRecord {
            public_key: vec![1u8; 32],
            name: "正常".into(),
            paired_at: 0,
            expires_at: 0,
        });
        let mut enc = s.encode().expect("编码应成功");

        // 必须确认真的定位到了目标。用 if let 静默跳过的话，
        // 哪天编码布局变了，这条测试会"通过"但什么都没测
        let pos = enc
            .windows(6)
            .position(|w| w == "正常".as_bytes())
            .expect("应能在编码中找到设备名");
        *enc.get_mut(pos).expect("上一步已确认位置有效") = 0x07;

        assert!(
            Store::decode(&enc).is_err(),
            "含控制字符的设备名应拒绝——可用于伪造界面显示"
        );
    }

    #[test]
    fn create_rejects_bad_name() {
        assert!(Store::create("").is_err());
        assert!(Store::create(&"x".repeat(100)).is_err());
        assert!(Store::create("含\u{0}空字符").is_err());
    }

    #[test]
    fn set_device_name_validates() {
        let mut s = Store::create("好名字").expect("创建应成功");
        assert!(s.set_device_name("新名字").is_ok());
        assert_eq!(s.device_name(), "新名字");
        assert!(s.set_device_name("").is_err());
        assert_eq!(s.device_name(), "新名字", "失败时不应改动");
    }
}
