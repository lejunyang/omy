//! 第三方密码管理器的 GUI 命令层。
//!
//! provider 返回的密码从不进入 WebView：查询后保存在 [`PasswordManagerState`]
//! 的短期缓存，前端只看到随机临时 handle 与显示字段；选择后由 Rust 直接调用现有
//! 解锁入口。锁定、重新查询和取消都应清掉缓存。

use crate::commands::{CmdError, CmdResult, Shared};
#[cfg(not(target_os = "android"))]
use omy_password_manager::PasswordManager as _;
#[cfg(not(target_os = "android"))]
use omy_password_manager::keepassxc::{
    Association, Client, DEFAULT_NAMESPACE, ProxyTransport, discover_proxy,
};
use omy_password_manager::{Credential, CredentialSummary};
#[cfg(not(target_os = "android"))]
use omy_secret::Protector as _;
use std::collections::HashMap;
#[cfg(not(target_os = "android"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;
use uuid::Uuid;
#[cfg(not(target_os = "android"))]
use zeroize::Zeroizing;

#[cfg(target_os = "android")]
mod android {
    use tauri::plugin::PluginHandle;

    const IDENTIFIER: &str = "org.omy.app";
    const CLASS: &str = "PasswordManagerPlugin";

    #[derive(serde::Deserialize)]
    pub struct AndroidCredential {
        pub id: String,
        pub secret: String,
    }

    #[derive(serde::Deserialize)]
    pub struct AndroidStatus {
        pub available: bool,
        pub reason: String,
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct SaveArgs<'a> {
        label: &'a str,
        secret: &'a str,
    }

    pub struct Plugin<R: tauri::Runtime>(PluginHandle<R>);

    impl<R: tauri::Runtime> Plugin<R> {
        fn status(&self) -> Result<AndroidStatus, String> {
            self.0
                .run_mobile_plugin("status", ())
                .map_err(|e| e.to_string())
        }

        fn select(&self) -> Result<AndroidCredential, String> {
            self.0
                .run_mobile_plugin("selectPassword", ())
                .map_err(|e| e.to_string())
        }

        fn save(&self, label: &str, secret: &str) -> Result<String, String> {
            #[derive(serde::Deserialize)]
            struct Saved {
                id: String,
            }
            self.0
                .run_mobile_plugin::<Saved>("savePassword", SaveArgs { label, secret })
                .map(|r| r.id)
                .map_err(|e| e.to_string())
        }
    }

    pub fn init<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
        tauri::plugin::Builder::new("omy-password-manager")
            .setup(|app, api| {
                let handle = api.register_android_plugin(IDENTIFIER, CLASS)?;
                tauri::Manager::manage(app, Plugin(handle));
                Ok(())
            })
            .build()
    }

    fn plugin(app: &tauri::AppHandle) -> Result<tauri::State<'_, Plugin<tauri::Wry>>, String> {
        use tauri::Manager as _;
        app.try_state::<Plugin<tauri::Wry>>()
            .ok_or_else(|| String::from("Android Credential Manager 插件未注册"))
    }

    pub fn status(app: &tauri::AppHandle) -> Result<AndroidStatus, String> {
        plugin(app)?.status()
    }

    pub fn select(app: &tauri::AppHandle) -> Result<AndroidCredential, String> {
        plugin(app)?.select()
    }

    pub fn save(app: &tauri::AppHandle, label: &str, secret: &str) -> Result<String, String> {
        plugin(app)?.save(label, secret)
    }
}

#[cfg(target_os = "android")]
pub use android::init;

#[cfg(not(target_os = "android"))]
const ASSOCIATION_SERVICE: &str = "omy-password-manager";
#[cfg(not(target_os = "android"))]
const ASSOCIATION_PREFIX: &str = "keepassxc-association-";

/// 候选秘密最长只在内存中保留五分钟。
const CANDIDATE_TTL: Duration = Duration::from_secs(5 * 60);

struct CandidateCache {
    entries: HashMap<String, Credential>,
    expires_at: Option<Instant>,
}

/// 密码管理器候选的进程内缓存。
pub struct PasswordManagerState {
    cache: Mutex<CandidateCache>,
}

impl PasswordManagerState {
    /// 新建空状态。
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(CandidateCache {
                entries: HashMap::new(),
                expires_at: None,
            }),
        }
    }

    fn replace(&self, entries: Vec<Credential>) -> CmdResult<Vec<CredentialSummary>> {
        let mut cache = self.cache.lock().map_err(|_| CmdError::code("internal"))?;
        // 先清旧值再装新值：CredentialSecret 的 Drop 会清零。若不断开两轮查询，
        // 用户只看见最新列表，旧秘密却会一直驻留到进程退出。
        cache.entries.clear();
        cache.expires_at = (!entries.is_empty()).then(|| Instant::now() + CANDIDATE_TTL);
        let mut summaries = Vec::with_capacity(entries.len());
        for entry in entries {
            // provider UUID 是跨会话稳定标识，不应暴露给 WebView。随机 handle
            // 只在这一轮选择中有效，也避免页面把旧 UUID 留下来反复调用。
            let handle = loop {
                let candidate = Uuid::new_v4().to_string();
                if !cache.entries.contains_key(&candidate) {
                    break candidate;
                }
            };
            let mut summary = CredentialSummary::from(&entry);
            summary.id.clone_from(&handle);
            cache.entries.insert(handle, entry);
            summaries.push(summary);
        }
        Ok(summaries)
    }

    pub(crate) fn take(&self, id: &str) -> CmdResult<Credential> {
        let mut cache = self.cache.lock().map_err(|_| CmdError::code("internal"))?;
        if cache
            .expires_at
            .is_none_or(|deadline| Instant::now() >= deadline)
        {
            cache.entries.clear();
            cache.expires_at = None;
            return Err(CmdError::code("password_manager_selection_expired"));
        }
        let selected = cache
            .entries
            .remove(id)
            .ok_or_else(|| CmdError::code("password_manager_selection_expired"))?;
        // 一次选择消费整批候选，而不只删所选项。否则同一轮查询返回的其它
        // 明文密码还会留在内存里，并可继续通过页面曾见过的 handle 调用。
        cache.entries.clear();
        cache.expires_at = None;
        Ok(selected)
    }

    /// 清空所有候选秘密。
    pub fn clear(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.entries.clear();
            cache.expires_at = None;
        }
    }
}

/// 前端展示的 KeePassXC 状态。
#[derive(Debug, serde::Serialize)]
pub struct PasswordManagerStatus {
    /// provider 标识。
    pub provider: &'static str,
    /// 本机是否找到 proxy。
    pub installed: bool,
    /// 是否已连接到运行中的 KeePassXC。
    pub running: bool,
    /// 当前数据库是否已经打开。
    pub database_open: bool,
    /// 当前数据库是否已经与 omy 关联。
    pub associated: bool,
    /// 当前数据库 hash；它不是秘密。
    pub database_hash: Option<String>,
    /// KeePassXC 版本。
    pub version: Option<String>,
    /// 供诊断显示的非秘密说明。
    pub detail: Option<String>,
}

/// 查询桌面密码管理器状态。Android 的显式 Credential Manager 状态由原生
/// provider 命令覆盖；这里先如实报告桌面 KeePassXC 不适用。
#[tauri::command]
pub async fn password_manager_status(app: tauri::AppHandle) -> CmdResult<PasswordManagerStatus> {
    #[cfg(target_os = "android")]
    {
        let status = android::status(&app).map_err(|e| {
            CmdError::with(
                "password_manager_unavailable",
                serde_json::json!({ "detail": e }),
            )
        })?;
        return Ok(PasswordManagerStatus {
            provider: "android_credential_manager",
            installed: status.available,
            running: status.available,
            database_open: status.available,
            associated: status.available,
            database_hash: None,
            version: None,
            detail: (!status.reason.is_empty()).then_some(status.reason),
        });
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        tauri::async_runtime::spawn_blocking(status_desktop)
            .await
            .map_err(|_| CmdError::code("internal"))?
    }
}

#[cfg(not(target_os = "android"))]
fn status_desktop() -> CmdResult<PasswordManagerStatus> {
    let path = configured_proxy();
    let Some(path) = discover_proxy(path.as_deref()) else {
        return Ok(PasswordManagerStatus {
            provider: "keepassxc",
            installed: false,
            running: false,
            database_open: false,
            associated: false,
            database_hash: None,
            version: None,
            detail: Some(String::from("proxy_not_found")),
        });
    };
    let mut client = match ProxyTransport::spawn(&path).and_then(Client::connect) {
        Ok(client) => client,
        Err(e) => {
            return Ok(PasswordManagerStatus {
                provider: "keepassxc",
                installed: true,
                running: false,
                database_open: false,
                associated: false,
                database_hash: None,
                version: None,
                detail: Some(e.to_string()),
            });
        }
    };
    let known = load_associations()?;
    match client.database_info(&known, false) {
        Ok(info) => {
            // 不能只看配置里“曾经关联过”：用户可能已经在 KeePassXC 里撤销。
            // 真跑 test-associate，失败就让界面重新出现“连接”按钮。
            let associated = known
                .iter()
                .find(|a| a.database_hash == info.hash)
                .is_some_and(|a| client.resume(a.clone()).is_ok());
            Ok(PasswordManagerStatus {
                provider: "keepassxc",
                installed: true,
                running: true,
                database_open: true,
                associated,
                database_hash: Some(info.hash),
                version: Some(info.version),
                detail: None,
            })
        }
        Err(omy_password_manager::Error::Locked) => Ok(PasswordManagerStatus {
            provider: "keepassxc",
            installed: true,
            running: true,
            database_open: false,
            associated: false,
            database_hash: None,
            version: None,
            detail: Some(String::from("database_locked")),
        }),
        Err(e) => Ok(PasswordManagerStatus {
            provider: "keepassxc",
            installed: true,
            running: false,
            database_open: false,
            associated: false,
            database_hash: None,
            version: None,
            detail: Some(e.to_string()),
        }),
    }
}

/// 首次关联当前 KeePassXC 数据库。KeePassXC 会显示授权窗口。
#[tauri::command]
pub async fn password_manager_connect(app: tauri::AppHandle) -> CmdResult<PasswordManagerStatus> {
    #[cfg(target_os = "android")]
    {
        return password_manager_status(app).await;
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        tauri::async_runtime::spawn_blocking(|| {
            let mut client = connect_new()?;
            client.database_info(&[], true).map_err(pm_error)?;
            let association = client.associate().map_err(pm_error)?;
            save_association(&association)?;
            status_desktop()
        })
        .await
        .map_err(|_| CmdError::code("internal"))?
    }
}

/// 查询当前数据库中的 omy 同步密钥。
#[tauri::command]
pub async fn password_manager_list(
    app: tauri::AppHandle,
    managers: State<'_, Arc<PasswordManagerState>>,
) -> CmdResult<Vec<CredentialSummary>> {
    #[cfg(target_os = "android")]
    {
        let selected = android::select(&app).map_err(|e| {
            let code = if e.contains("user_cancelled") {
                "user_cancelled"
            } else if e.contains("password_manager_not_found") {
                "password_manager_not_found"
            } else {
                "password_manager_failed"
            };
            CmdError::with(code, serde_json::json!({ "detail": e }))
        })?;
        let entry = Credential {
            id: selected.id.clone(),
            name: selected.id.clone(),
            login: selected.id,
            group: String::new(),
            secret: omy_password_manager::CredentialSecret::new(selected.secret),
        };
        return managers.replace(vec![entry]);
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        let state = Arc::clone(&managers);
        tauri::async_runtime::spawn_blocking(move || {
            let mut client = connect_associated()?;
            let entries = client.get_logins(DEFAULT_NAMESPACE).map_err(pm_error)?;
            state.replace(entries)
        })
        .await
        .map_err(|_| CmdError::code("internal"))?
    }
}

/// 生成 256 bit 同步密钥并写入 KeePassXC，再放进候选缓存。
#[tauri::command]
pub async fn password_manager_generate(
    app: tauri::AppHandle,
    managers: State<'_, Arc<PasswordManagerState>>,
    label: String,
) -> CmdResult<CredentialSummary> {
    if label.trim().is_empty() {
        return Err(CmdError::code("password_manager_label_required"));
    }

    #[cfg(target_os = "android")]
    {
        let secret = omy_password_manager::generate_sync_secret();
        let id = android::save(&app, label.trim(), secret.expose()).map_err(|e| {
            let code = if e.contains("user_cancelled") {
                "user_cancelled"
            } else {
                "password_manager_failed"
            };
            CmdError::with(code, serde_json::json!({ "detail": e }))
        })?;
        let entry = Credential {
            id,
            name: label.trim().to_owned(),
            login: label.trim().to_owned(),
            group: String::new(),
            secret,
        };
        return managers
            .replace(vec![entry])?
            .pop()
            .ok_or_else(|| CmdError::code("internal"));
    }

    #[cfg(not(target_os = "android"))]
    {
        let _ = app;
        let state = Arc::clone(&managers);
        tauri::async_runtime::spawn_blocking(move || {
            let mut client = connect_associated()?;
            let secret = omy_password_manager::generate_sync_secret();
            let id = client
                .create(DEFAULT_NAMESPACE, label.trim(), &secret)
                .map_err(pm_error)?
                .ok_or_else(|| CmdError::code("password_manager_write_unverified"))?;
            let entry = Credential {
                id,
                name: String::from("credentials.omy.app"),
                login: label.trim().to_owned(),
                group: String::new(),
                secret,
            };
            state
                .replace(vec![entry])?
                .pop()
                .ok_or_else(|| CmdError::code("internal"))
        })
        .await
        .map_err(|_| CmdError::code("internal"))?
    }
}

/// 用后端缓存中的某个密码管理器秘密解锁本地目录。
#[tauri::command]
pub async fn password_manager_unlock(
    managers: State<'_, Arc<PasswordManagerState>>,
    state: State<'_, Shared>,
    vault_reg: State<'_, Arc<crate::vault_reg::VaultRegistry>>,
    dir: String,
    credential_id: String,
) -> CmdResult<crate::commands::UnlockResult> {
    let entry = managers.take(&credential_id)?;
    crate::commands::unlock_directory(state, vault_reg, dir, entry.secret.expose().to_owned()).await
}

/// 忘记当前数据库的 omy 关联。不会删除任何密码条目。
#[tauri::command]
pub async fn password_manager_forget(
    managers: State<'_, Arc<PasswordManagerState>>,
) -> CmdResult<()> {
    managers.clear();
    #[cfg(target_os = "android")]
    {
        return Ok(());
    }

    #[cfg(not(target_os = "android"))]
    tauri::async_runtime::spawn_blocking(|| {
        let mut client = connect_new()?;
        let known = load_associations()?;
        let info = client.database_info(&known, false).map_err(pm_error)?;
        forget_association(&info.hash)
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 用户关闭选择器时立即清除其中的明文候选。
#[tauri::command]
pub fn password_manager_clear(managers: State<'_, Arc<PasswordManagerState>>) {
    managers.clear();
}

#[cfg(not(target_os = "android"))]
fn connect_new() -> CmdResult<Client<ProxyTransport>> {
    let configured = configured_proxy();
    Client::connect_proxy(configured.as_deref()).map_err(pm_error)
}

#[cfg(not(target_os = "android"))]
fn connect_associated() -> CmdResult<Client<ProxyTransport>> {
    let associations = load_associations()?;
    let mut client = connect_new()?;
    let info = client
        .database_info(&associations, true)
        .map_err(pm_error)?;
    let association = associations
        .into_iter()
        .find(|a| a.database_hash == info.hash)
        .ok_or_else(|| CmdError::code("password_manager_not_associated"))?;
    client.resume(association).map_err(pm_error)?;
    Ok(client)
}

#[cfg(not(target_os = "android"))]
fn configured_proxy() -> Option<PathBuf> {
    omy_config::Config::load()
        .ok()
        .and_then(|c| c.password_managers.keepassxc.proxy_path.map(PathBuf::from))
}

#[cfg(not(target_os = "android"))]
fn association_secret_id(database_hash: &str) -> String {
    format!("{ASSOCIATION_PREFIX}{database_hash}")
}

#[cfg(not(target_os = "android"))]
fn load_associations() -> CmdResult<Vec<Association>> {
    let cfg = omy_config::Config::load().map_err(|_| CmdError::code("config_read_failed"))?;
    if cfg.password_managers.keepassxc.associations.is_empty() {
        return Ok(Vec::new());
    }
    let protector = omy_secret::MachineProtector::new(ASSOCIATION_SERVICE)
        .map_err(|_| CmdError::code("password_manager_keyring_unavailable"))?;
    let mut out = Vec::new();
    for meta in cfg.password_managers.keepassxc.associations {
        let key = match protector.retrieve(&association_secret_id(&meta.database_hash)) {
            Ok(key) => key,
            Err(omy_secret::Error::NotFound) => continue,
            Err(_) => return Err(CmdError::code("password_manager_keyring_failed")),
        };
        out.push(Association::from_key_bytes(
            meta.database_hash,
            meta.id,
            &key,
        ));
    }
    Ok(out)
}

#[cfg(not(target_os = "android"))]
fn save_association(association: &Association) -> CmdResult<()> {
    let key = Zeroizing::new(association.key_bytes().map_err(pm_error)?);
    let protector = omy_secret::MachineProtector::new(ASSOCIATION_SERVICE)
        .map_err(|_| CmdError::code("password_manager_keyring_unavailable"))?;
    let secret_id = association_secret_id(&association.database_hash);
    // 覆盖已有数据库关联时先留住旧 key。配置写盘若失败，要把系统凭据库
    // 恢复成原值；直接 delete 会把一个原本可用的关联也破坏掉。
    let previous = match protector.retrieve(&secret_id) {
        Ok(previous) => Some(previous),
        Err(omy_secret::Error::NotFound) => None,
        Err(_) => return Err(CmdError::code("password_manager_keyring_failed")),
    };
    protector
        .store(&secret_id, &key)
        .map_err(|_| CmdError::code("password_manager_keyring_failed"))?;

    let mut cfg = omy_config::Config::load().map_err(|_| CmdError::code("config_read_failed"))?;
    cfg.password_managers
        .keepassxc
        .associations
        .retain(|a| a.database_hash != association.database_hash);
    cfg.password_managers
        .keepassxc
        .associations
        .push(omy_config::KeePassXcAssociation {
            database_hash: association.database_hash.clone(),
            id: association.id.clone(),
        });
    if cfg.save().is_err() {
        if let Some(previous) = previous {
            let _ = protector.store(&secret_id, &previous);
        } else {
            // 元数据没落盘时留下 bearer 只会变成再也找不到的授权；立即撤回。
            let _ = protector.delete(&secret_id);
        }
        return Err(CmdError::code("config_write_failed"));
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn forget_association(database_hash: &str) -> CmdResult<()> {
    let protector = omy_secret::MachineProtector::new(ASSOCIATION_SERVICE)
        .map_err(|_| CmdError::code("password_manager_keyring_unavailable"))?;
    protector
        .delete(&association_secret_id(database_hash))
        .map_err(|_| CmdError::code("password_manager_keyring_failed"))?;
    let mut cfg = omy_config::Config::load().map_err(|_| CmdError::code("config_read_failed"))?;
    cfg.password_managers
        .keepassxc
        .associations
        .retain(|a| a.database_hash != database_hash);
    cfg.save()
        .map_err(|_| CmdError::code("config_write_failed"))
}

fn pm_error(error: omy_password_manager::Error) -> CmdError {
    use omy_password_manager::Error as PmError;
    let detail = error.to_string();
    match error {
        PmError::Unavailable(_) | PmError::Transport(_) => CmdError::with(
            "password_manager_unavailable",
            serde_json::json!({ "detail": detail }),
        ),
        PmError::Locked => CmdError::code("password_manager_locked"),
        PmError::NotAssociated => CmdError::code("password_manager_not_associated"),
        PmError::UserCancelled => CmdError::code("user_cancelled"),
        PmError::NotFound => CmdError::code("password_manager_not_found"),
        PmError::Unsupported(_) => CmdError::with(
            "password_manager_unsupported",
            serde_json::json!({ "detail": detail }),
        ),
        PmError::Protocol(_) | PmError::Provider { .. } => CmdError::with(
            "password_manager_failed",
            serde_json::json!({ "detail": detail }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_candidates_drops_old_handles() {
        let state = PasswordManagerState::new();
        let entry = |id: &str| Credential {
            id: id.to_owned(),
            name: id.to_owned(),
            login: id.to_owned(),
            group: String::from("omy"),
            secret: omy_password_manager::CredentialSecret::new(format!("secret-{id}")),
        };
        let old = state.replace(vec![entry("provider-old")]).expect("first");
        let new = state.replace(vec![entry("provider-new")]).expect("second");
        // 不这样会怎样：用户刷新候选后，已经从界面消失的旧密码仍可凭旧 handle
        // 被注入脚本取用，并一直驻留到进程退出。
        assert!(state.take(&old[0].id).is_err(), "旧 handle 必须失效");
        assert_ne!(new[0].id, "provider-new", "不能把 provider UUID 暴露给页面");
        assert_eq!(
            state.take(&new[0].id).expect("new").secret.expose(),
            "secret-provider-new"
        );
    }

    #[test]
    fn selecting_one_candidate_invalidates_its_siblings() {
        let state = PasswordManagerState::new();
        let entry = |id: &str| Credential {
            id: id.to_owned(),
            name: id.to_owned(),
            login: id.to_owned(),
            group: String::from("omy"),
            secret: omy_password_manager::CredentialSecret::new(format!("secret-{id}")),
        };
        let summaries = state
            .replace(vec![entry("first"), entry("second")])
            .expect("replace");
        state.take(&summaries[0].id).expect("first selection");
        // 不这样会怎样：页面选过一项后，仍可用同一批列表中的其它 handle
        // 重复驱动后端，候选明文也不会按“一次性选择”立即清掉。
        assert!(state.take(&summaries[1].id).is_err());
    }

    #[test]
    fn expired_candidates_are_cleared_before_use() {
        let state = PasswordManagerState::new();
        let summaries = state
            .replace(vec![Credential {
                id: String::from("provider-id"),
                name: String::from("name"),
                login: String::from("login"),
                group: String::from("omy"),
                secret: omy_password_manager::CredentialSecret::new(String::from("secret")),
            }])
            .expect("replace");
        state.cache.lock().expect("cache").expires_at = Some(Instant::now());
        // 不这样会怎样：把选择器留在后台几小时后，页面里的旧 handle 仍能
        // 取回查询当时的明文密码，所谓“短期缓存”只是文档承诺。
        assert!(state.take(&summaries[0].id).is_err());
    }

    #[test]
    fn association_secret_id_is_stable_and_scoped() {
        // 不这样会怎样：两个数据库共用一个 keyring id，后关联的会覆盖前一个，
        // 重启后其中一个数据库必然无法恢复关联。
        assert_eq!(association_secret_id("abc"), association_secret_id("abc"));
        assert_ne!(association_secret_id("abc"), association_secret_id("def"));
    }
}
