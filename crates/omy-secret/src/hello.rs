//! 设备密钥：把一把密钥封进 TPM，并由 Windows Hello 把守。
//!
//! # 与 [`MachineProtector`](crate::MachineProtector) 的区别
//!
//! 两者都实现 [`Protector`](crate::Protector)，但保护强度差一个量级：
//!
//! | | 后端 | 私钥在哪 | 拷走磁盘 | 换台机器 |
//! |---|---|---|---|---|
//! | `MachineProtector` | 凭据管理器（DPAPI） | 内存/磁盘，软件保护 | 解不开 | 解不开 |
//! | `HelloProtector` | TPM 2.0 | **芯片里，物理上出不来** | 解不开 | 解不开 |
//!
//! 关键差别不在「能不能解开」，而在**攻击者拿到系统级权限之后**：DPAPI
//! 的主密钥在软件里，SYSTEM 可以掏出来；TPM 的私钥连 SYSTEM 也拿不到，
//! 它只能请求芯片代为解密——而那一步会被 Hello 门禁拦下。
//!
//! # 门禁是系统强制的，不是我们自己判断的
//!
//! 通过 `NCRYPT_UI_POLICY` 挂在密钥上，由 CNG 在每次私钥操作时强制。
//! 实测证据（见 `verify-device-key.ps1`）：带 `NCRYPT_SILENT_FLAG` 解封
//! 会被拒绝，错误码 `0x80090022`（`NTE_UI_REQUIRED`）。
//!
//! 这一条必须由系统来保证。如果改成我们在代码里「先弹个框问一下，
//! 用户点了确定再调解密」，攻击者直接调 `NCryptDecrypt` 就绕过去了——
//! 那种门禁形同虚设。
//!
//! # 什么时候弹窗
//!
//! 实测确认：**创建密钥时弹一次**（finalize 那一步），之后每次解封各弹
//! 一次。封装（公钥操作）不弹——公钥本来就不是秘密。
//!
//! # 它挡不住什么
//!
//! 挡不住**正在你机器上以你的身份运行的恶意程序**：它可以在你按下指纹
//! 之后的那个窗口里发起解封请求。所以设备密钥的定位是**省去每次输密码**,
//! 不是「比密码更安全」。界面上必须这么说，否则用户会因为开了它而把
//! 密码设得更弱，反而更糟。
//!
//! # 它随时可能失效
//!
//! 换机器、重装系统、清除 TPM、重置 Hello——任一发生，这把密钥就永久
//! 解不开了。所以**永远不能是唯一凭据**，必须始终保留至少一个密码槽。

use crate::{Error, ProtectKey, Protector, Result, KEY_LEN};

/// TPM + Windows Hello 保管的密钥。
///
/// 每个 `id` 对应 TPM 里的一把 RSA 密钥，以及配置目录下的一份密文。
/// 密文本身不敏感——没有那块 TPM 谁也解不开它。
#[derive(Debug)]
pub struct HelloProtector {
    /// 服务名，用于给 TPM 里的密钥命名，避免与别的应用撞名。
    service: String,
}

impl HelloProtector {
    /// 新建一个。
    ///
    /// # Errors
    ///
    /// 这台机器没有可用 TPM、或 CNG 打不开 Platform Crypto Provider 时
    /// 返回 [`Error::NoBackend`]。**不会**悄悄退回软件实现：那会让用户
    /// 以为密钥有硬件保护，而实际没有。
    pub fn new(service: &str) -> Result<Self> {
        let p = Self { service: service.to_owned() };
        imp::probe()?;
        Ok(p)
    }

    /// TPM 里的密钥名。
    fn key_name(&self, id: &str) -> String {
        format!("{}-{id}", self.service)
    }
}

impl Protector for HelloProtector {
    fn name(&self) -> &'static str {
        "Windows Hello（TPM）"
    }

    fn retrieve(&self, id: &str) -> Result<ProtectKey> {
        let ct = imp::read_blob(&self.service, id)?;
        imp::unwrap_key(&self.key_name(id), &ct)
    }

    fn store(&self, id: &str, key: &ProtectKey) -> Result<()> {
        let ct = imp::wrap_key(&self.key_name(id), key)?;
        imp::write_blob(&self.service, id, &ct)
    }

    fn delete(&self, id: &str) -> Result<()> {
        // 两样都要删。只删密文的话 TPM 里会留下一把再也用不到的密钥，
        // 而下次用同一个 id 时 create 会撞名失败
        imp::delete_key(&self.key_name(id))?;
        imp::remove_blob(&self.service, id)
    }

    /// 只看密文在不在，不去解封——解封会弹 Hello。
    ///
    /// 界面上「挂没挂」是个状态展示，为它弹一次指纹很突兀。代价是清除
    /// TPM 之后这里仍报 true，直到真解锁才失败；那条路径有明确提示。
    fn has(&self, id: &str) -> bool {
        imp::read_blob(&self.service, id).is_ok()
    }

    fn requires_user_presence(&self) -> bool {
        true
    }
}

#[cfg(target_os = "windows")]
// CNG 是 C API，每一次调用都得 unsafe。范围限制在这个模块里：
// crate 其余部分仍然 deny(unsafe_code)
#[allow(unsafe_code)]
mod imp {
    use super::{Error, ProtectKey, Result, KEY_LEN};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Security::Cryptography::{
        NCryptCreatePersistedKey, NCryptDecrypt, NCryptDeleteKey, NCryptEncrypt,
        NCryptFinalizeKey, NCryptFreeObject, NCryptOpenKey, NCryptOpenStorageProvider,
        NCryptSetProperty, BCRYPT_OAEP_PADDING_INFO, BCRYPT_SHA256_ALGORITHM, CERT_KEY_SPEC,
        NCRYPT_FLAGS, NCRYPT_KEY_HANDLE, NCRYPT_PAD_OAEP_FLAG, NCRYPT_PROV_HANDLE,
        NCRYPT_UI_POLICY,
    };
    use zeroize::Zeroizing;

    /// `NCRYPT_UI_PROTECT_KEY_FLAG`：每次用这把私钥都要用户确认。
    const UI_PROTECT_KEY: u32 = 1;

    /// 密钥不存在时 CNG 返回的错误码（`NTE_BAD_KEYSET`）。
    ///
    /// 必须单独认出来：它表示「没存过」，是正常情况，混进通用失败里
    /// 会让首次使用报错。
    const NTE_BAD_KEYSET: i32 = -2146893802; // 0x80090016

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(core::iter::once(0)).collect()
    }

    fn open_provider() -> Result<NCRYPT_PROV_HANDLE> {
        let mut prov = NCRYPT_PROV_HANDLE::default();
        // Platform Crypto Provider 就是 TPM。不能退回软件 KSP——
        // 那里的私钥可以导出，「绑定这台机器」当场不成立
        unsafe { NCryptOpenStorageProvider(&mut prov, w!("Microsoft Platform Crypto Provider"), 0) }
            .map_err(|e| {
                Error::NoBackend(format!("这台机器没有可用的 TPM（0x{:08X}）", e.code().0))
            })?;
        Ok(prov)
    }

    /// 探测 TPM 是否真的可用。
    ///
    /// 只打开 provider，不造密钥：造密钥会弹 Hello，而探测阶段弹窗
    /// 会让用户莫名其妙——他还没开启这个功能。
    pub(super) fn probe() -> Result<()> {
        let prov = open_provider()?;
        unsafe { NCryptFreeObject(prov.into()) }.ok();
        Ok(())
    }

    fn oaep() -> BCRYPT_OAEP_PADDING_INFO {
        BCRYPT_OAEP_PADDING_INFO {
            pszAlgId: BCRYPT_SHA256_ALGORITHM,
            pbLabel: std::ptr::null_mut(),
            cbLabel: 0,
        }
    }

    /// 打开已有密钥；不存在时返回 [`Error::NotFound`]。
    fn open_key(prov: NCRYPT_PROV_HANDLE, name: &str) -> Result<NCRYPT_KEY_HANDLE> {
        let n = wide(name);
        let mut key = NCRYPT_KEY_HANDLE::default();
        unsafe {
            NCryptOpenKey(prov, &mut key, PCWSTR(n.as_ptr()), CERT_KEY_SPEC(0), NCRYPT_FLAGS(0))
        }
        .map_err(|e| {
            if e.code().0 == NTE_BAD_KEYSET {
                Error::NotFound
            } else {
                Error::Backend(format!("打开 TPM 密钥失败（0x{:08X}）", e.code().0))
            }
        })?;
        Ok(key)
    }

    /// 造一把新的、挂了 Hello 门禁的 TPM 密钥。
    fn create_key(prov: NCRYPT_PROV_HANDLE, name: &str) -> Result<NCRYPT_KEY_HANDLE> {
        let n = wide(name);
        let mut key = NCRYPT_KEY_HANDLE::default();
        unsafe {
            NCryptCreatePersistedKey(
                prov,
                &mut key,
                w!("RSA"),
                PCWSTR(n.as_ptr()),
                CERT_KEY_SPEC(0),
                NCRYPT_FLAGS(0),
            )
        }
        .map_err(|e| Error::Backend(format!("创建 TPM 密钥失败（0x{:08X}）", e.code().0)))?;

        // UI Policy 必须在 finalize **之前**设：finalize 之后属性就锁定了，
        // 那时再设不会报错但也不生效——门禁静默失效是最糟的失败方式
        let policy = NCRYPT_UI_POLICY {
            dwVersion: 1,
            dwFlags: UI_PROTECT_KEY,
            pszCreationTitle: w!("omy 设备密钥"),
            pszFriendlyName: w!("omy"),
            pszDescription: w!("解锁 omy 加密文件"),
        };
        let bytes = unsafe {
            std::slice::from_raw_parts(
                std::ptr::addr_of!(policy).cast::<u8>(),
                core::mem::size_of::<NCRYPT_UI_POLICY>(),
            )
        };
        unsafe { NCryptSetProperty(key.into(), w!("UI Policy"), bytes, NCRYPT_FLAGS(0)) }
            .map_err(|e| Error::Backend(format!("设置门禁失败（0x{:08X}）", e.code().0)))?;

        // 这一步会弹一次 Hello（创建期确认）
        unsafe { NCryptFinalizeKey(key, NCRYPT_FLAGS(0)) }.map_err(|e| {
            Error::Backend(format!("完成 TPM 密钥创建失败（0x{:08X}）", e.code().0))
        })?;
        Ok(key)
    }

    /// 用 TPM 公钥封装一把密钥。公钥操作，不弹窗。
    pub(super) fn wrap_key(name: &str, key: &ProtectKey) -> Result<Vec<u8>> {
        let prov = open_provider()?;
        // 已有就复用，没有才造。复用时不会重复弹创建确认
        let h = match open_key(prov, name) {
            Ok(h) => h,
            Err(Error::NotFound) => create_key(prov, name)?,
            Err(e) => return Err(e),
        };
        let pad = oaep();
        let mut need = 0u32;
        let r = (|| -> Result<Vec<u8>> {
            unsafe {
                NCryptEncrypt(
                    h,
                    Some(key.as_slice()),
                    Some(std::ptr::addr_of!(pad).cast()),
                    None,
                    &mut need,
                    NCRYPT_PAD_OAEP_FLAG,
                )
            }
            .map_err(|e| Error::Backend(format!("封装失败（0x{:08X}）", e.code().0)))?;
            let mut ct = vec![0u8; need as usize];
            unsafe {
                NCryptEncrypt(
                    h,
                    Some(key.as_slice()),
                    Some(std::ptr::addr_of!(pad).cast()),
                    Some(&mut ct),
                    &mut need,
                    NCRYPT_PAD_OAEP_FLAG,
                )
            }
            .map_err(|e| Error::Backend(format!("封装失败（0x{:08X}）", e.code().0)))?;
            ct.truncate(need as usize);
            Ok(ct)
        })();
        unsafe { NCryptFreeObject(h.into()) }.ok();
        unsafe { NCryptFreeObject(prov.into()) }.ok();
        r
    }

    /// 让 TPM 解封。私钥操作，会弹 Hello。
    pub(super) fn unwrap_key(name: &str, ct: &[u8]) -> Result<ProtectKey> {
        let prov = open_provider()?;
        let h = open_key(prov, name)?;
        let pad = oaep();
        let r = (|| -> Result<ProtectKey> {
            let mut need = 0u32;
            // 不加 SILENT：那会让门禁把我们自己拒掉。
            // 反过来说，SILENT 被拒正是门禁生效的证据（实测 0x80090022）
            unsafe {
                NCryptDecrypt(
                    h,
                    Some(ct),
                    Some(std::ptr::addr_of!(pad).cast()),
                    None,
                    &mut need,
                    NCRYPT_PAD_OAEP_FLAG,
                )
            }
            .map_err(map_decrypt_err)?;
            let mut pt = Zeroizing::new(vec![0u8; need as usize]);
            unsafe {
                NCryptDecrypt(
                    h,
                    Some(ct),
                    Some(std::ptr::addr_of!(pad).cast()),
                    Some(&mut pt),
                    &mut need,
                    NCRYPT_PAD_OAEP_FLAG,
                )
            }
            .map_err(map_decrypt_err)?;

            let bytes: [u8; KEY_LEN] = pt
                .get(..need as usize)
                .and_then(|s| s.try_into().ok())
                // 长度不对说明这份密文不是我们写的，或者格式变了。
                // 不能当成「密钥错误」——那会让用户以为是 Hello 的问题
                // 长度不对说明这份密文不是我们写的，或格式变了。归到 Backend 而不是
                // Undecryptable：后者在界面上会说「密钥不对」，会把用户往
                // 「是不是 Hello 出问题了」的方向带
                .ok_or_else(|| Error::Backend("TPM 解出的密钥长度不对".into()))?;
            Ok(Zeroizing::new(bytes))
        })();
        unsafe { NCryptFreeObject(h.into()) }.ok();
        unsafe { NCryptFreeObject(prov.into()) }.ok();
        r
    }

    /// 解封失败的分类。
    ///
    /// 用户取消要和真失败分开：前者界面什么都不用说，后者要给出原因。
    fn map_decrypt_err(e: windows::core::Error) -> Error {
        // 0x800704C7 = ERROR_CANCELLED，用户在 Hello 弹窗上点了取消
        const CANCELLED: i32 = -2147023673;
        // 0x80090022 = NTE_UI_REQUIRED，需要 UI 但当前不允许弹
        const UI_REQUIRED: i32 = -2146893790;
        match e.code().0 {
            CANCELLED => Error::UserCancelled,
            UI_REQUIRED => Error::Backend(
                "需要 Windows Hello 确认，但当前环境弹不出提示框".into(),
            ),
            other => Error::Backend(format!("TPM 解封失败（0x{other:08X}）")),
        }
    }

    pub(super) fn delete_key(name: &str) -> Result<()> {
        let prov = open_provider()?;
        let r = match open_key(prov, name) {
            Ok(h) => unsafe { NCryptDeleteKey(h, 0) }
                .map_err(|e| Error::Backend(format!("删除 TPM 密钥失败（0x{:08X}）", e.code().0))),
            // 已经不在了就算成功：delete 是幂等的
            Err(Error::NotFound) => Ok(()),
            Err(e) => Err(e),
        };
        unsafe { NCryptFreeObject(prov.into()) }.ok();
        r
    }

    // ---- 密文落盘 ----
    //
    // 密文本身不敏感：没有这块 TPM 谁也解不开。所以放普通配置目录即可，
    // 不需要再套一层保护——那只会把「哪一层才是真正的保护」搞模糊。

    fn blob_path(service: &str, id: &str) -> Result<std::path::PathBuf> {
        let base = omy_config::data_dir()
            .ok_or_else(|| Error::Other("找不到数据目录".into()))?
            .join("device-keys");
        std::fs::create_dir_all(&base).map_err(|e| Error::Other(e.to_string()))?;
        // id 可能含斜杠等字符，转成十六进制再当文件名
        let safe: String = id.bytes().map(|b| format!("{b:02x}")).collect();
        Ok(base.join(format!("{service}-{safe}.tpm")))
    }

    pub(super) fn read_blob(service: &str, id: &str) -> Result<Vec<u8>> {
        match std::fs::read(blob_path(service, id)?) {
            Ok(v) => Ok(v),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::NotFound),
            Err(e) => Err(Error::Other(e.to_string())),
        }
    }

    pub(super) fn write_blob(service: &str, id: &str, ct: &[u8]) -> Result<()> {
        std::fs::write(blob_path(service, id)?, ct).map_err(|e| Error::Other(e.to_string()))
    }

    pub(super) fn remove_blob(service: &str, id: &str) -> Result<()> {
        match std::fs::remove_file(blob_path(service, id)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Other(e.to_string())),
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{Error, ProtectKey, Result};

    /// 非 Windows 平台如实说不支持。
    ///
    /// 不退回软件实现：那会让界面显示「已启用硬件保护」而实际没有——
    /// 用户据此判断风险，说错比不说更糟。
    fn unsupported<T>() -> Result<T> {
        Err(Error::NoBackend(
            "设备密钥目前只支持 Windows（需要 TPM 2.0）".into(),
        ))
    }

    pub(super) fn probe() -> Result<()> {
        unsupported()
    }
    pub(super) fn wrap_key(_name: &str, _key: &ProtectKey) -> Result<Vec<u8>> {
        unsupported()
    }
    pub(super) fn unwrap_key(_name: &str, _ct: &[u8]) -> Result<ProtectKey> {
        unsupported()
    }
    pub(super) fn delete_key(_name: &str) -> Result<()> {
        unsupported()
    }
    pub(super) fn read_blob(_service: &str, _id: &str) -> Result<Vec<u8>> {
        unsupported()
    }
    pub(super) fn write_blob(_service: &str, _id: &str, _ct: &[u8]) -> Result<()> {
        unsupported()
    }
    pub(super) fn remove_blob(_service: &str, _id: &str) -> Result<()> {
        unsupported()
    }
}
