//! `omy-secret`：把一段密文**绑定到这台设备**，并为将来的生物识别留好位置。
//!
//! # 解决的问题
//!
//! 远程位置的 WebDAV 密码要能重启后继续用，就必须落盘。直接写进
//! `config.toml` 等于密码明文躺在磁盘上，任何读到那个文件的人都能拿走。
//!
//! 做法是**再加一层密钥**：真正的内容用 `omy-core` 的 AEAD 加密，而这把
//! 密钥交给操作系统的凭据库保管。攻击者光拷走配置文件没有用，还得在
//! 这台机器上、以这个用户的身份去问系统要密钥。
//!
//! # 为什么抽成 trait 而不是直接调 keyring
//!
//! 「用什么保护这把密钥」和「怎么用这把密钥加密」是两件事。把前者抽成
//! [`Protector`]，机器绑定与生物识别就只是两个实现：
//!
//! | 实现 | 保护强度 | 用户感知 |
//! |---|---|---|
//! | [`MachineProtector`] | 绑这台机器+这个用户 | 无感，开箱即用 |
//! | 生物识别（未实现） | 绑这台机器+**本人在场** | 每次解锁要按指纹/刷脸 |
//!
//! 两者的差别只在「取密钥时系统要不要先验证是本人」。Android Keystore
//! 的 `setUserAuthenticationRequired` 正是同一把密钥上的一个开关，
//! 所以它们本就该是同一套机制的两档，而不是两套代码。
//!
//! # 威胁模型：它挡什么、不挡什么
//!
//! **挡得住**：配置文件被拷走、磁盘被取下来挂到别的机器、备份泄露。
//! 这些场景下攻击者拿到的只是一段无法解开的密文。
//!
//! **挡不住**：攻击者能以你的身份在这台机器上运行代码。那时他可以
//! 直接问系统要密钥——这是操作系统凭据库的固有边界，不是本模块的缺陷。
//! 要防这个得靠生物识别档（要求本人在场）或主密码派生（密钥根本不落盘）。
//!
//! 所以这一层的定位是**把「明文密码躺在磁盘上」提升到「需要这台机器」**，
//! 不是万能保险箱。不要在文档或界面里把它说成后者。

// 不用 forbid：设备密钥要调 Windows CNG，那套 API 全是 unsafe。
// 改成 deny + 在 hello 模块里局部 allow，其余模块照旧一行 unsafe 都不许有。
// forbid 的问题是它连模块级 allow 都挡掉，等于逼人把 CNG 挪进独立 crate，
// 换不来任何实际安全性
#![deny(unsafe_code)]

use zeroize::Zeroizing;

mod envelope;
mod hello;
#[cfg(target_os = "macos")]
mod macos;
mod machine;

pub use envelope::{seal, unseal, Envelope};
pub use hello::HelloProtector;
#[cfg(target_os = "macos")]
pub use macos::MacBiometricProtector;
pub use machine::MachineProtector;

/// 密钥长度，与 `omy-core` 的对称密钥一致。
pub const KEY_LEN: usize = 32;

/// 一把用于保护其他内容的密钥。
///
/// 用 `Zeroizing` 包住：这东西离开作用域必须从内存里抹掉，
/// 否则它会留在被释放的堆内存里，直到那块内存被别的数据覆盖。
pub type ProtectKey = Zeroizing<[u8; KEY_LEN]>;

/// 密钥保护失败的原因。
///
/// 分类的意义在于**界面要给出不同的话**：没有可用后端时该告诉用户
/// 「这台机器上装个 gnome-keyring」，而用户取消生物识别时什么都不用说。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 这个平台/环境没有可用的凭据后端。
    ///
    /// 典型情形是 Linux 服务器没跑 Secret Service。此时**不能**悄悄
    /// 退回明文存储——那会让用户以为密码被保护着。
    #[error("当前环境没有可用的凭据存储：{0}")]
    NoBackend(String),
    /// 后端存在但这次操作失败（权限、D-Bus 断了、钥匙串被锁）。
    #[error("凭据存储操作失败：{0}")]
    Backend(String),
    /// 存过的密钥找不到了。
    ///
    /// 与 `Backend` 分开，是因为这多半意味着用户换了机器或清了钥匙串，
    /// 界面该提示「请重新登录这个位置」而不是报错。
    #[error("找不到已保存的密钥")]
    NotFound,
    /// 用户取消了身份验证（生物识别档）。
    #[error("用户取消了身份验证")]
    UserCancelled,
    /// 这台机器没有可用的生物识别硬件（macOS：没有配对 Touch ID）。
    ///
    /// 与 `NoBackend` 分开：前者是「这台 Mac 压根没指纹硬件」，
    /// 后者是「系统服务/签名环境用不了」。界面该分别说
    /// 「接一个 Touch ID 键盘」和「这版应用没签好名」。
    #[error("这台设备没有可用的生物识别硬件：{0}")]
    BiometricsUnavailable(String),
    /// 生物识别硬件可用，但系统里没录入任何指纹。
    ///
    /// 界面该引导用户去「系统设置 → 触控 ID」录一个，而不是让他重试。
    #[error("这台设备还没录入指纹：{0}")]
    BiometricsNotEnrolled(String),
    /// 连续验证失败过多，系统暂时锁定了生物识别。
    #[error("生物识别被暂时锁定：{0}")]
    BiometricsLockout(String),
    /// 密文损坏或被篡改，解不开。
    #[error("数据无法解密（可能已损坏或来自其他设备）")]
    Undecryptable,
    /// 序列化/IO 之类的杂项错误。
    #[error("{0}")]
    Other(String),
}

/// 本模块的结果类型。
pub type Result<T> = std::result::Result<T, Error>;

/// 保管一把密钥的后端。
///
/// # 实现者须知
///
/// - `retrieve` 找不到时返回 [`Error::NotFound`]，**不要**返回一把新密钥：
///   那会让上层用错误的密钥去解旧数据，得到「数据损坏」这种误导性错误。
/// - 需要用户交互的实现（生物识别），用户取消要返回 [`Error::UserCancelled`]，
///   不要当成失败——界面据此区分「出错了」和「你自己取消的」。
pub trait Protector: Send + Sync {
    /// 这个后端的名字，用于日志与设置页显示。
    fn name(&self) -> &'static str;

    /// 取出已保管的密钥。
    ///
    /// # Errors
    ///
    /// 没存过返回 [`Error::NotFound`]；后端不可用返回其他错误。
    fn retrieve(&self, id: &str) -> Result<ProtectKey>;

    /// 保管一把密钥，覆盖同 id 的旧值。
    ///
    /// # Errors
    ///
    /// 后端不可用或写入失败时返回。
    fn store(&self, id: &str, key: &ProtectKey) -> Result<()>;

    /// 删除已保管的密钥。已经不存在时应当成功返回。
    ///
    /// # Errors
    ///
    /// 后端不可用时返回。
    fn delete(&self, id: &str) -> Result<()>;

    /// 取出，没有就生成一把并存好。
    ///
    /// 这是最常用的入口：首次使用时建密钥，之后每次拿到同一把。
    ///
    /// # Errors
    ///
    /// 后端不可用，或生成后存不进去时返回。
    fn retrieve_or_create(&self, id: &str) -> Result<ProtectKey> {
        match self.retrieve(id) {
            Ok(k) => Ok(k),
            Err(Error::NotFound) => {
                let key = random_key();
                self.store(id, &key)?;
                Ok(key)
            }
            Err(e) => Err(e),
        }
    }

    /// 有没有保管过这个 id 的密钥。
    ///
    /// # 为什么不用 `retrieve().is_ok()` 代替
    ///
    /// 那会弹出生物识别提示框。界面上「有没有挂过」是个状态展示，
    /// 为它弹一次指纹确认很突兀——用户只是打开了设置页。
    ///
    /// 默认实现退回 `retrieve`，因为对不需要用户在场的后端（机器绑定档）
    /// 两者没有区别。要求在场的后端**应当覆盖它**，用不弹窗的方式判断。
    ///
    /// 代价是它只回答「那份密文在不在」，不保证还解得开：清除 TPM 之后
    /// 这里仍会返回 true，直到真去 `retrieve` 才失败。这个取舍是有意的——
    /// 让状态查询免打扰比让它绝对准确更要紧，真正的失败路径有明确提示。
    fn has(&self, id: &str) -> bool {
        self.retrieve(id).is_ok()
    }

    /// 这个后端是否要求用户当场验证身份（指纹/人脸/PIN）。
    ///
    /// 机器绑定档是 `false`，生物识别档是 `true`。界面据此决定要不要
    /// 提示「解锁时需要验证指纹」。
    fn requires_user_presence(&self) -> bool {
        false
    }
}

/// 生成一把随机密钥。
#[must_use]
pub fn random_key() -> ProtectKey {
    use rand_core::RngCore;
    let mut k = [0u8; KEY_LEN];
    rand_core::OsRng.fill_bytes(&mut k);
    Zeroizing::new(k)
}

// 让 Box<dyn Protector> 自己也是 Protector：上层拿到统一选择器返回的
// Box 后，可以毫无差别地调 trait 方法，不必每次先 `&*` 解引用。
impl Protector for Box<dyn Protector> {
    fn name(&self) -> &'static str {
        (**self).name()
    }
    fn retrieve(&self, id: &str) -> Result<ProtectKey> {
        (**self).retrieve(id)
    }
    fn store(&self, id: &str, key: &ProtectKey) -> Result<()> {
        (**self).store(id, key)
    }
    fn delete(&self, id: &str) -> Result<()> {
        (**self).delete(id)
    }
    fn has(&self, id: &str) -> bool {
        (**self).has(id)
    }
    fn requires_user_presence(&self) -> bool {
        (**self).requires_user_presence()
    }
}

/// 挑一个当前环境可用的保护后端。
///
/// 目前只有机器绑定一档。将来加入生物识别后，这里按配置选择，
/// 而调用方的代码不用改——这正是抽出 [`Protector`] 的目的。
///
/// # Errors
///
/// 当前环境没有任何可用后端时返回 [`Error::NoBackend`]。
pub fn default_protector(service: &str) -> Result<Box<dyn Protector>> {
    let p = MachineProtector::new(service)?;
    Ok(Box::new(p))
}

/// 挑一个「需要本人在场」的设备密钥后端。
///
/// 这是 GUI 与 CLI 共用的**唯一**入口：上层不要再各自 `HelloProtector::new`，
/// 否则加新平台时两端容易各漏一处。
///
/// | 平台 | 后端 |
/// |---|---|
/// | Windows | TPM + Windows Hello（[`HelloProtector`]）|
/// | macOS | Data Protection Keychain + Touch ID（[`MacBiometricProtector`]）|
/// | 其它 | [`Error::NoBackend`]，明确说不支持，不退回软件档 |
///
/// # Errors
///
/// 这台机器没有对应硬件、未录入指纹、未签名、或平台不支持时返回。
pub fn device_protector(service: &str) -> Result<Box<dyn Protector>> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(HelloProtector::new(service)?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(MacBiometricProtector::new(service)?))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = service;
        Err(Error::NoBackend(
            "设备密钥（生物识别免密）目前只支持 Windows 与 macOS".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存版后端，用于测试上层逻辑而不碰真实钥匙串。
    #[derive(Default)]
    struct MemProtector {
        map: Mutex<HashMap<String, [u8; KEY_LEN]>>,
        /// 记录 store 被调用的次数，用于验证「第二次不该再建新密钥」。
        stores: Mutex<usize>,
    }

    impl Protector for MemProtector {
        fn name(&self) -> &'static str {
            "mem"
        }
        fn retrieve(&self, id: &str) -> Result<ProtectKey> {
            let m = self.map.lock().map_err(|_| Error::Other(String::from("锁")))?;
            m.get(id)
                .map(|k| Zeroizing::new(*k))
                .ok_or(Error::NotFound)
        }
        fn store(&self, id: &str, key: &ProtectKey) -> Result<()> {
            let mut m = self.map.lock().map_err(|_| Error::Other(String::from("锁")))?;
            m.insert(id.to_owned(), **key);
            if let Ok(mut n) = self.stores.lock() {
                *n += 1;
            }
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<()> {
            let mut m = self.map.lock().map_err(|_| Error::Other(String::from("锁")))?;
            m.remove(id);
            Ok(())
        }
    }

    /// `retrieve_or_create` 第二次必须返回同一把密钥。
    ///
    /// 不这样会怎样：每次启动都生成新密钥，于是上次加密的凭据全部解不开——
    /// 表现为「重启后远程位置要求重新登录」，而日志里一切正常。
    #[test]
    fn retrieve_or_create_is_stable() {
        let p = MemProtector::default();
        let a = p.retrieve_or_create("x").expect("首次");
        let b = p.retrieve_or_create("x").expect("再次");
        assert_eq!(*a, *b, "同一个 id 必须拿到同一把密钥");
        assert_eq!(*p.stores.lock().expect("锁"), 1, "第二次不该再写一次");
    }

    /// 不同 id 之间必须互相独立。
    ///
    /// 不这样会怎样：删掉一个远程位置会把别的位置的密钥一起废掉。
    #[test]
    fn ids_are_isolated() {
        let p = MemProtector::default();
        let a = p.retrieve_or_create("a").expect("a");
        let b = p.retrieve_or_create("b").expect("b");
        assert_ne!(*a, *b, "不同 id 不能共用一把密钥");
        p.delete("a").expect("删 a");
        assert!(matches!(p.retrieve("a"), Err(Error::NotFound)), "a 应已删除");
        assert_eq!(*p.retrieve("b").expect("b 还在"), *b, "删 a 不该影响 b");
    }

    /// 删除一个不存在的 id 应当成功，而不是报错。
    ///
    /// 不这样会怎样：移除位置时若密钥已被用户手动清掉，会多报一个
    /// 无意义的错误，甚至让移除流程中断。
    #[test]
    fn delete_is_idempotent() {
        let p = MemProtector::default();
        p.delete("never-existed").expect("删不存在的应当成功");
    }

    /// 随机密钥不能是全零，也不能每次一样。
    ///
    /// 不这样会怎样：RNG 若没真正初始化，所有设备会用同一把「随机」密钥，
    /// 机器绑定就完全失效了，而功能表现上毫无异常。
    #[test]
    fn random_key_is_random() {
        let a = random_key();
        let b = random_key();
        assert_ne!(*a, [0u8; KEY_LEN], "不能是全零");
        assert_ne!(*a, *b, "两次生成不能相同");
    }
}
