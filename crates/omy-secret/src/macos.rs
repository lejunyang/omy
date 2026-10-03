//! 设备密钥（macOS）：把一把密钥封进 Data Protection Keychain，由 Touch ID 把守。
//!
//! # 与 Windows 上的 `HelloProtector` 的关系
//!
//! 两者是同一档（需要本人在场）的不同平台实现：
//!
//! | | 后端 | 秘密存在哪 | 取用时的门禁 |
//! |---|---|---|---|
//! | Windows | TPM 2.0（CNG） | TPM 芯片里 | 系统强制 Windows Hello |
//! | macOS | Data Protection Keychain | 钥匙串，访问控制挂 Touch ID | 系统强制 Touch ID |
//!
//! 关键都是**门禁由系统在读取秘密的那一步强制**，而不是我们在代码里
//! 先弹个框、用户点了确定再去读一条本来不设防的记录——后者等于没门禁。
//!
//! # 为什么是 Data Protection Keychain 而不是登录钥匙串
//!
//! 传统登录钥匙串的访问控制只能写「某个 app 始终允许」，**挂不上 Touch ID**。
//! 能给一次读取挂 Touch ID 的，只有 `kSecUseDataProtectionKeychain = true`
//! 的现代钥匙串条目，配 `SecAccessControlCreateWithFlags(biometryAny)`。
//!
//! # 它对签名有硬性要求（真机实测）
//!
//! 未签名 / 临时签名（ad-hoc）的进程写 Data Protection Keychain，SecurityServer
//! 直接拒绝，错误码 `errSecMissingEntitlement (-34018)`。本机 secd 日志原话：
//!
//! > Client has neither com.apple.application-identifier nor
//! > com.apple.security.application-groups nor keychain-access-groups entitlements
//!
//! 临时签名即使把 `keychain-access-groups` 塞进 entitlements，也会被 AMFI
//! 以 SIGKILL 杀掉——这类受限制 entitlement 必须由 Apple 颁发的
//! Developer ID 签名 + 描述文件背书。所以：**正式可用的免密解锁要求
//! omy 以 Developer ID 签名成 .app 分发**。未签名的开发二进制会如实报
//! `NoBackend`，绝不悄悄退回一条不设防的普通钥匙串条目。
//!
//! # 可用性探测不弹窗
//!
//! `new()` 用 `LAContext.canEvaluatePolicy(biometrics)` 判断这台机器有没有
//! Touch ID、录没录指纹。这是纯本地查询，**不弹 Touch ID**，也不要求签名。
//! 真正读密钥那一步（`retrieve`）才会弹。
//!
//! # biometryAny 而不是 BiometryCurrentSet
//!
//! 用 `biometryAny`：用户之后增删指纹不会让这条密钥失效。Windows Hello 那边
//! 也是「只要是本人就行」，不绑具体哪根手指。
//!
//! # 它随时可能失效
//!
//! 换机器、清除 Touch ID、重装系统、换 Apple ID 钥匙串同步——任一发生，
//! 这条密钥就再也解不开。所以它**永远不能是唯一凭据**。

use crate::{Error, ProtectKey, Protector, Result, KEY_LEN};

/// Touch ID（Data Protection Keychain）保管的密钥。
#[derive(Debug)]
pub struct MacBiometricProtector {
    /// 钥匙串里的 service 名，与 CLI/GUI 约定一致。
    service: String,
}

impl MacBiometricProtector {
    /// 新建一个。
    ///
    /// 只做不弹窗的可用性探测（`LAContext.canEvaluatePolicy`）：这台 Mac 有没有
    /// Touch ID、录没录指纹。**不**去碰钥匙串——那一步才会弹 Touch ID，
    /// 而构造时机可能只是用户打开了设置页。
    ///
    /// # Errors
    ///
    /// 无 Touch ID 硬件 / 未录入 / 被锁定时返回对应的细分错误，
    /// 让界面能分别引导用户。
    pub fn new(service: &str) -> Result<Self> {
        imp::probe_biometrics()?;
        Ok(Self { service: service.to_owned() })
    }
}

impl Protector for MacBiometricProtector {
    fn name(&self) -> &'static str {
        "Touch ID（Data Protection Keychain）"
    }

    fn retrieve(&self, id: &str) -> Result<ProtectKey> {
        imp::fetch(&self.service, id)
    }

    fn store(&self, id: &str, key: &ProtectKey) -> Result<()> {
        imp::store(&self.service, id, key)
    }

    fn delete(&self, id: &str) -> Result<()> {
        imp::remove(&self.service, id)
    }

    /// 只查条目在不在，不回读数据、不带 prompt——**不弹 Touch ID**。
    ///
    /// 实测（spike）：带 `kSecReturnAttributes`、不带 `kSecReturnData`、不带
    /// `kSecUseOperationPrompt` 的查询，SecurityServer 直接返回在/不在，
    /// 不会走到生物识别那一步。
    fn has(&self, id: &str) -> bool {
        imp::exists(&self.service, id)
    }

    fn requires_user_presence(&self) -> bool {
        true
    }
}

// Security 框架是 C API，这里集中 unsafe。crate 其余部分仍 deny(unsafe_code)。
#[allow(unsafe_code)]
mod imp {
    use super::{Error, ProtectKey, Result, KEY_LEN};
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::data::CFData;
    use core_foundation::string::CFString;
    use std::os::raw::c_void;
    use zeroize::Zeroizing;

    type CFTypeRef = *const c_void;
    type CFMutableDictionaryRef = *mut c_void;
    type OSStatus = i32;

    // ---- Security / CoreFoundation 全局量与函数 ----
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFDictionaryCreate(
            allocator: *const c_void,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            num: isize,
            keyCallbacks: *const c_void,
            valueCallbacks: *const c_void,
        ) -> CFMutableDictionaryRef;
        fn CFDataGetLength(data: CFTypeRef) -> usize;
        fn CFDataGetBytePtr(data: CFTypeRef) -> *const u8;
    }

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecClass: CFTypeRef;
        static kSecClassGenericPassword: CFTypeRef;
        static kSecAttrService: CFTypeRef;
        static kSecAttrAccount: CFTypeRef;
        static kSecAttrAccessControl: CFTypeRef;
        static kSecValueData: CFTypeRef;
        static kSecAttrAccessible: CFTypeRef;
        static kSecAttrAccessibleWhenUnlockedThisDeviceOnly: CFTypeRef;
        static kSecUseDataProtectionKeychain: CFTypeRef;
        static kSecUseOperationPrompt: CFTypeRef;
        static kSecReturnData: CFTypeRef;
        static kSecReturnAttributes: CFTypeRef;
        static kSecMatchLimit: CFTypeRef;
        static kSecMatchLimitOne: CFTypeRef;

        fn SecAccessControlCreateWithFlags(
            allocator: *const c_void,
            accessibility: CFTypeRef,
            flags: u64,
            error: *mut CFTypeRef,
        ) -> CFTypeRef;

        fn SecItemAdd(attributes: CFMutableDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemCopyMatching(query: CFMutableDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
        fn SecItemDelete(query: CFMutableDictionaryRef) -> OSStatus;
    }

    // ---- LocalAuthentication：走 objc_msgSend 直调 LAContext ----
    //
    // 不引 objc2 全家桶：这里只用 LAContext 的一个方法，手写 FFI 更省依赖。
    type ObjcId = *mut c_void;
    type Sel = *const c_void;

    // LocalAuthentication 框架必须显式链接：否则 LAContext 这个类根本没被
    // 加载进进程，objc_getClass 只会返回 NULL。空块仅为把框架拉进来。
    #[link(name = "LocalAuthentication", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    // objc_msgSend 是个多态跳板，按实参形状声明多个原型是惯例；
    // clippy 的「冲突声明」警告在这里是预期的。
    #[allow(clashing_extern_declarations)]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_void) -> ObjcId;
        fn sel_registerName(name: *const c_void) -> Sel;
        #[link_name = "objc_msgSend"]
        fn msg0(obj: ObjcId, sel: Sel) -> ObjcId;
        #[link_name = "objc_msgSend"]
        fn msg_bool_err(obj: ObjcId, sel: Sel, policy: isize, err: *mut ObjcId) -> bool;
        #[link_name = "objc_msgSend"]
        fn msg_isize(obj: ObjcId, sel: Sel) -> isize;
    }

    fn cstr(s: &str) -> Vec<u8> {
        s.bytes().chain(std::iter::once(0)).collect()
    }

    /// LAPolicy.deviceOwnerAuthenticationWithBiometrics = 1。
    const POLICY_BIOMETRICS: isize = 1;

    /// LAError 码。新系统会加新码（本机实测无配件配对返回 -12），
    /// 认得出的细分，认不出的一律归「不可用」并把码带进消息。
    const LA_BIOMETRY_NOT_ENROLLED: isize = -7;

    /// 不弹窗地探测生物识别可用性。
    pub(super) fn probe_biometrics() -> Result<()> {
        let cls = unsafe { objc_getClass(cstr("LAContext").as_ptr() as *const c_void) };
        if cls.is_null() {
            return Err(Error::BiometricsUnavailable(
                "系统没有 LocalAuthentication".into(),
            ));
        }
        let sel_alloc = unsafe { sel_registerName(cstr("alloc").as_ptr() as *const c_void) };
        let sel_init = unsafe { sel_registerName(cstr("init").as_ptr() as *const c_void) };
        let sel_policy =
            unsafe { sel_registerName(cstr("canEvaluatePolicy:error:").as_ptr() as *const c_void) };
        let sel_code = unsafe { sel_registerName(cstr("code").as_ptr() as *const c_void) };

        let ctx = unsafe { msg0(msg0(cls, sel_alloc), sel_init) };
        if ctx.is_null() {
            return Err(Error::BiometricsUnavailable("建不起 LAContext".into()));
        }
        let mut err: ObjcId = std::ptr::null_mut();
        let ok = unsafe { msg_bool_err(ctx, sel_policy, POLICY_BIOMETRICS, &mut err) };
        if ok {
            return Ok(());
        }
        let code = if err.is_null() {
            0
        } else {
            unsafe { msg_isize(err, sel_code) }
        };
        let msg = format!("LAContext 错误码 {code}");
        match code {
            LA_BIOMETRY_NOT_ENROLLED => Err(Error::BiometricsNotEnrolled(msg)),
            // -6/-8 是历代 NotAvailable；-12 是新系统「配件未配对」。都算不可用。
            -6 | -8 | -12 => Err(Error::BiometricsUnavailable(msg)),
            // 历史上 TouchIDLockout 是 -8，与 NotAvailable 撞码；新系统应分得出。
            // 这里拿不准的码不擅自当可用，宁可让用户去系统设置确认。
            other => Err(Error::BiometricsUnavailable(format!("{other}（{msg}）"))),
        }
    }

    /// 建 Security 查询字典。
    fn query(service: &str, id: &str, extra: &[(CFTypeRef, CFTypeRef)]) -> CFMutableDictionaryRef {
        let svc = CFString::new(service);
        let acc = CFString::new(id);
        let tru = CFBoolean::true_value();
        let mut pairs: Vec<(CFTypeRef, CFTypeRef)> = Vec::with_capacity(6 + extra.len());
        pairs.push((unsafe { kSecClass }, unsafe { kSecClassGenericPassword }));
        pairs.push((unsafe { kSecAttrService }, svc.as_CFTypeRef()));
        pairs.push((unsafe { kSecAttrAccount }, acc.as_CFTypeRef()));
        pairs.push((
            unsafe { kSecUseDataProtectionKeychain },
            tru.as_CFTypeRef(),
        ));
        pairs.extend_from_slice(extra);
        let (keys, vals): (Vec<CFTypeRef>, Vec<CFTypeRef>) = pairs.into_iter().unzip();
        unsafe {
            CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                vals.as_ptr(),
                keys.len() as isize,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            )
        }
    }

    /// 把 OSStatus 翻成我们的错误分类。
    fn map_status(code: OSStatus) -> Result<()> {
        match code {
            0 => Ok(()),
            -25300 => Err(Error::NotFound),
            // 用户在 Touch ID 弹窗上点了「取消」
            -128 => Err(Error::UserCancelled),
            // 未签名 / 缺 keychain-access-groups entitlement。
            // 如实报「没有可用后端」，绝不退回不设防的普通钥匙串条目。
            -34018 => Err(Error::NoBackend(
                "Data Protection Keychain 不可用：这个应用没有 Developer ID 签名 \
                 或缺少 keychain-access-groups entitlement"
                    .into(),
            )),
            -25293 => Err(Error::Backend(
                "Touch ID 验证失败（未录入指纹、被锁定或权限被拒绝）".into(),
            )),
            other => Err(Error::Backend(format!("Keychain 操作失败（{other}）"))),
        }
    }

    /// 条目在不在（不回数据、不弹窗）。任何异常都按「不在」处理：
    /// 状态查询不该因为后端抖动而报错。
    pub(super) fn exists(service: &str, id: &str) -> bool {
        let tru = CFBoolean::true_value();
        let q = query(
            service,
            id,
            &[
                (unsafe { kSecReturnAttributes }, tru.as_CFTypeRef()),
                (unsafe { kSecMatchLimit }, unsafe { kSecMatchLimitOne }),
            ],
        );
        let mut res: CFTypeRef = std::ptr::null();
        let r = unsafe { SecItemCopyMatching(q, &mut res) };
        r == 0
    }

    /// 取密钥。带 operation prompt，会弹 Touch ID。
    pub(super) fn fetch(service: &str, id: &str) -> Result<ProtectKey> {
        let tru = CFBoolean::true_value();
        let prompt = CFString::new("验证 omy 是你本人");
        let q = query(
            service,
            id,
            &[
                (unsafe { kSecReturnData }, tru.as_CFTypeRef()),
                (unsafe { kSecUseOperationPrompt }, prompt.as_CFTypeRef()),
            ],
        );
        let mut data: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemCopyMatching(q, &mut data) };
        map_status(status)?;

        if data.is_null() {
            return Err(Error::Backend("Keychain 没返回数据".into()));
        }
        // data 是 CFDataRef。直接读长度与指针，避免版本间包装方法漂移。
        let len = unsafe { CFDataGetLength(data) };
        let ptr = unsafe { CFDataGetBytePtr(data) };
        let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
        let arr: [u8; KEY_LEN] = bytes
            .try_into()
            .map_err(|_| Error::Backend("钥匙串里的密钥长度不对".into()))?;
        Ok(Zeroizing::new(arr))
    }

    /// 存密钥。带访问控制：以后每次读取都要 Touch ID。
    pub(super) fn store(service: &str, id: &str, key: &ProtectKey) -> Result<()> {
        // 先造访问控制对象。失败说明系统不支持这个组合。
        let mut ac_err: CFTypeRef = std::ptr::null();
        let ac = unsafe {
            SecAccessControlCreateWithFlags(
                std::ptr::null(),
                kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
                BIOMETRY_ANY,
                &mut ac_err,
            )
        };
        if ac.is_null() {
            return Err(Error::Backend(
                "创建 Touch ID 访问控制失败（系统不支持或未签名）".into(),
            ));
        }

        // 已存在就先删，再按带访问控制的方式写——等于覆盖同 id 的旧值。
        let _ = remove(service, id);

        let val = CFData::from_buffer(key.as_slice());
        let attrs = query(
            service,
            id,
            &[
                (
                    unsafe { kSecAttrAccessible },
                    unsafe { kSecAttrAccessibleWhenUnlockedThisDeviceOnly },
                ),
                (unsafe { kSecAttrAccessControl }, ac),
                (unsafe { kSecValueData }, val.as_CFTypeRef()),
            ],
        );
        let mut res: CFTypeRef = std::ptr::null();
        let status = unsafe { SecItemAdd(attrs, &mut res) };
        map_status(status)
    }

    /// 删除。已经不在就算成功（幂等）。
    pub(super) fn remove(service: &str, id: &str) -> Result<()> {
        let q = query(service, id, &[]);
        let status = unsafe { SecItemDelete(q) };
        match status {
            0 => Ok(()),
            -25300 => Ok(()), // 本来就没有
            other => map_status(other).map(|_| ()),
        }
    }

    /// kSecAccessControlBiometryAny。
    const BIOMETRY_ANY: u64 = 1 << 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真机探针：LAContext 探测不能 panic，且在这台机器上应明确报
    /// 「无 Touch ID」而不是成功或含糊错误。
    ///
    /// 本机（Mac mini M4，无 Touch ID 配件）实测：code -12。
    /// 在有 Touch ID 的机器上这条会反过来返回 Ok——那是手工证据项。
    #[test]
    fn probe_reports_real_state() {
        match MacBiometricProtector::new("omy-probe") {
            Ok(_) => println!("本机 Touch ID 可用（探测通过）"),
            Err(Error::BiometricsUnavailable(m)) => println!("本机无 Touch ID：{m}"),
            Err(Error::BiometricsNotEnrolled(m)) => println!("本机未录入指纹：{m}"),
            Err(e) => panic!("探测返回了意料外的错误：{e:?}"),
        }
    }
}
