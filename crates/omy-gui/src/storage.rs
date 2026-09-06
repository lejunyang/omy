//! 安卓的全盘存储访问权限。
//!
//! # 为什么需要它
//!
//! 安卓应用默认只能读写自己的沙箱目录。omy 是文件管理器形态的应用，
//! 用户要加密的文件在相册、下载、微信收到的文件里，全都在沙箱外——
//! 没有这个权限，侧栏只能列出两个空目录，应用基本没用。
//!
//! # 为什么是 MANAGE_EXTERNAL_STORAGE 而不是 SAF
//!
//! omy 的核心（[`omy_core::source::BlockSource`] 的 seek 读、
//! `fsatomic` 的「写 tmp → fsync → rename → fsync 目录」原子写、
//! `tree`/`pack` 的目录遍历）全都建立在 `std::fs` 的**路径**语义上。
//! SAF 给的是 `content://` URI：rename 之后 URI 会变、已授权权限失效，
//! 恰好打掉原子写依赖的前提。而 MANAGE_EXTERNAL_STORAGE 明确保留直接
//! 文件路径访问，现有代码零改动可用。
//!
//! 另外 targetSdk 30 以上 `ACTION_OPEN_DOCUMENT_TREE` 根本不允许选择
//! 内部存储根目录，「浏览整机」这个目标用 SAF 做不到。
//!
//! # 状态不缓存
//!
//! 用户随时可能去系统设置里把开关关掉，应用无法收到通知。所以每次都
//! 实查，不在 Rust 侧存一份「已授权」的副本——那份副本会在用户关掉权限
//! 后继续说「有权限」，然后所有文件操作以 EACCES 失败，且错误信息完全
//! 指不到真正的原因。

/// 一个存储卷。
///
/// 由 Kotlin 侧的 `StorageManager.getStorageVolumes()` 得来；Rust 侧猜不出
/// SD 卡的挂载点（形如 `/storage/A1B2-C3D4`，卷 ID 系统随机分配）。
///
/// 仅安卓编译：桌面端的磁盘根由 [`crate::browse`] 自己枚举，这里留着
/// 只会是 dead_code。
#[cfg(target_os = "android")]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageVolume {
    /// 卷根目录的绝对路径。
    pub path: String,
    /// 是否可移除（SD 卡、U 盘）。
    pub removable: bool,
    /// 是否为主存储（即 `/storage/emulated/0`）。
    pub primary: bool,
    /// 系统给出的本地化卷名，主存储为 `None`（前端有自己的翻译键）。
    ///
    /// `serde(default)` 是必须的：Kotlin 侧 `JSObject.put(key, null)` 实际上
    /// 会把这个键**删掉**而不是置为 JSON null，字段因此可能整个缺失。
    /// 不写 default 的话反序列化直接失败，整个侧栏拿不到卷列表。
    #[serde(default)]
    pub label: Option<String>,
}

/// 存储卷列表的返回包装。
#[cfg(target_os = "android")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct VolumeList {
    volumes: Vec<StorageVolume>,
}

/// 存储访问权限的当前状态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StorageAccess {
    /// 能否读写共享存储。
    pub granted: bool,
    /// 走的是哪条申请路径。
    ///
    /// `all-files` 为 API 30+ 的 `MANAGE_EXTERNAL_STORAGE`（跳设置页），
    /// `legacy` 为 API 29 及以下的运行时权限（标准弹窗），
    /// `not-applicable` 为非安卓平台。
    ///
    /// 前端需要区分：两条路径的交互差别很大，`all-files` 会离开应用，
    /// 回来时要重新查询状态。
    pub mode: String,
}

impl StorageAccess {
    /// 非安卓平台的状态：桌面端本来就能读写整个文件系统。
    ///
    /// 安卓上不编译：那里状态一律来自 Kotlin 侧实查，留着会是 dead_code
    /// warning。用 cfg 而不是 allow(dead_code)——后者会顺带盖住将来
    /// 真正写错造成的未使用告警。
    #[cfg(any(not(target_os = "android"), test))]
    #[must_use]
    fn not_applicable() -> Self {
        Self {
            granted: true,
            mode: String::from("not-applicable"),
        }
    }
}

#[cfg(target_os = "android")]
mod imp {
    use super::StorageAccess;
    use tauri::plugin::PluginHandle;

    /// Kotlin 侧插件类所在的包名。
    ///
    /// 与 `tauri.conf.json` 里的 identifier 对应。插件类放在 app 模块内
    /// （而不是独立的 Gradle 模块）：它只有权限判定这一件事，独立模块
    /// 要多一份 build.gradle.kts、一份 ACL 定义和一份 manifest，
    /// 维护成本远超收益。
    const PLUGIN_IDENTIFIER: &str = "org.omy.app";
    /// Kotlin 侧的插件类名。
    const PLUGIN_CLASS: &str = "StoragePlugin";

    /// 插件句柄。存进 Tauri 的 state，命令通过它调 Kotlin。
    pub struct StoragePlugin<R: tauri::Runtime>(PluginHandle<R>);

    impl<R: tauri::Runtime> StoragePlugin<R> {
        /// 查询当前授权状态。
        pub fn check(&self) -> Result<StorageAccess, String> {
            self.0
                .run_mobile_plugin::<StorageAccess>("checkAccess", ())
                .map_err(|e| e.to_string())
        }

        /// 申请授权。
        ///
        /// API 30+ 会跳到系统设置页，用户可能过很久才回来，也可能直接
        /// 不回来。Kotlin 侧在 activity 返回时重新实查状态再 resolve，
        /// 所以这里拿到的结果总是真实的。
        pub fn request(&self) -> Result<StorageAccess, String> {
            self.0
                .run_mobile_plugin::<StorageAccess>("requestAccess", ())
                .map_err(|e| e.to_string())
        }

        /// 列出所有已挂载的存储卷。
        pub fn volumes(&self) -> Result<Vec<super::StorageVolume>, String> {
            self.0
                .run_mobile_plugin::<super::VolumeList>("storageVolumes", ())
                .map(|v| v.volumes)
                .map_err(|e| e.to_string())
        }
    }

    /// 注册插件。在 `tauri::Builder::plugin` 里调用。
    pub fn init<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
        tauri::plugin::Builder::new("omy-storage")
            .setup(|app, api| {
                let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, PLUGIN_CLASS)?;
                tauri::Manager::manage(app, StoragePlugin(handle));
                Ok(())
            })
            .build()
    }

    /// 取插件句柄并查询状态。
    ///
    /// 插件注册失败时（Kotlin 类找不到、JNI 出错）state 里没有句柄。
    /// 此时**不能**假装有权限：那会让侧栏列出整机路径，然后每个都
    /// EACCES。报 false 更接近真相。
    pub fn check(app: &tauri::AppHandle) -> Result<StorageAccess, String> {
        use tauri::Manager as _;
        app.try_state::<StoragePlugin<tauri::Wry>>()
            .ok_or_else(|| String::from("存储权限插件未注册"))?
            .check()
    }

    /// 取插件句柄并申请授权。
    pub fn request(app: &tauri::AppHandle) -> Result<StorageAccess, String> {
        use tauri::Manager as _;
        app.try_state::<StoragePlugin<tauri::Wry>>()
            .ok_or_else(|| String::from("存储权限插件未注册"))?
            .request()
    }

    /// 取插件句柄并列举存储卷。
    ///
    /// 失败时返回空列表而不是错误：调用方 [`super::android_volume_roots`]
    /// 是给侧栏用的，拿不到卷时应当退回沙箱目录，而不是让整个侧栏报错。
    pub fn volumes(app: &tauri::AppHandle) -> Vec<super::StorageVolume> {
        use tauri::Manager as _;
        app.try_state::<StoragePlugin<tauri::Wry>>()
            .and_then(|p| p.volumes().ok())
            .unwrap_or_default()
    }
}

#[cfg(target_os = "android")]
pub use imp::init;

/// 当前是否已拿到全盘访问（供后端内部判断，不经过命令层）。
///
/// 供 [`crate::browse::list_places`] 决定列沙箱目录还是列整机卷。
/// 查询失败按未授权处理：宁可少列几个入口，也不要列出一堆点进去
/// 就 EACCES 的路径——后者用户完全看不出问题在权限上。
#[cfg(target_os = "android")]
#[must_use]
pub fn is_granted(app: &tauri::AppHandle) -> bool {
    imp::check(app).map(|s| s.granted).unwrap_or(false)
}

/// 已挂载的存储卷（供后端内部使用）。
#[cfg(target_os = "android")]
#[must_use]
pub fn volumes(app: &tauri::AppHandle) -> Vec<StorageVolume> {
    imp::volumes(app)
}

/// 查询存储访问权限状态。
///
/// 非安卓平台恒为已授权：桌面端进程本来就有文件系统访问权限，
/// 前端不需要为此分叉。
#[tauri::command]
pub fn storage_access(app: tauri::AppHandle) -> Result<StorageAccess, crate::commands::CmdError> {
    #[cfg(target_os = "android")]
    {
        imp::check(&app).map_err(|e| {
            crate::commands::CmdError::with(
                "storage_permission_query_failed",
                serde_json::json!({ "detail": e }),
            )
        })
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(StorageAccess::not_applicable())
    }
}

/// 申请存储访问权限。
///
/// 安卓 API 30+ 上这会把用户送去系统设置页；返回的状态是用户操作完
/// **重新实查**的结果，不是 activity 的 resultCode——设置页返回时
/// resultCode 恒为 CANCELED，哪怕用户刚打开了开关。
#[tauri::command]
pub fn request_storage_access(
    app: tauri::AppHandle,
) -> Result<StorageAccess, crate::commands::CmdError> {
    #[cfg(target_os = "android")]
    {
        imp::request(&app).map_err(|e| {
            crate::commands::CmdError::with(
                "storage_permission_request_failed",
                serde_json::json!({ "detail": e }),
            )
        })
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        Ok(StorageAccess::not_applicable())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 桌面端必须报「已授权」。
    ///
    /// 不这样会怎样：前端见 granted=false 就弹授权引导，
    /// 而桌面端根本没有可申请的东西，用户会卡在一个点了没反应的按钮上。
    #[test]
    fn desktop_reports_granted() {
        let s = StorageAccess::not_applicable();
        assert!(s.granted);
        assert_eq!(s.mode, "not-applicable");
    }

    /// mode 参与前端分支判断，序列化后的字段名不能变。
    ///
    /// 不这样会怎样：改成 camelCase 或换字段名时测试仍然通过，
    /// 但前端读到 undefined，授权引导永远不显示。
    #[test]
    fn serializes_expected_fields() {
        let json = serde_json::to_value(StorageAccess::not_applicable())
            .expect("StorageAccess 应当可序列化");
        assert_eq!(json["granted"], serde_json::json!(true));
        assert_eq!(json["mode"], serde_json::json!("not-applicable"));
    }
}
