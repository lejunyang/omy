//! 设备与共享相关的 Tauri 命令。
//!
//! 单独一个文件而不是塞进 `commands.rs`：那个文件已经 775 行，
//! 而这一组命令有自己的状态（设备库会话、配对任务、共享任务），
//! 混在一起会让两边都难读。

use crate::commands::{CmdError, CmdResult};
use crate::devices::{DeviceError, DeviceSession, DeviceStatus, PairedInfo};
use crate::lan::{DiscoveredDevice, PairPhase, PairTask, ShareStatus, ShareTask};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

/// 设备库会话句柄。
pub type SharedDevices = Arc<DeviceSession>;
/// 配对任务句柄。
pub type SharedPair = Arc<PairTask>;
/// 共享任务句柄。
pub type SharedShare = Arc<ShareTask>;

impl From<DeviceError> for CmdError {
    fn from(e: DeviceError) -> Self {
        Self::code(e.code())
    }
}

/// 设备库当前状态。
///
/// 界面据此决定显示「设置密码」还是「输入密码」，
/// 所以未打开时也必须能回答。
#[tauri::command]
pub fn device_status(devices: State<'_, SharedDevices>) -> DeviceStatus {
    devices.status()
}

/// 打开或创建设备库。
///
/// Argon2 派生要几十毫秒，放阻塞线程。
#[tauri::command]
pub async fn open_device_store(
    devices: State<'_, SharedDevices>,
    password: String,
) -> CmdResult<DeviceStatus> {
    if password.is_empty() {
        return Err(CmdError::code("empty_password"));
    }
    let handle: SharedDevices = Arc::clone(&devices);
    let name = crate::devices::default_device_name();

    tauri::async_runtime::spawn_blocking(move || handle.open(password.as_bytes(), &name))
        .await
        .map_err(|_| CmdError::code("internal"))?
        .map_err(CmdError::from)
}

/// 关闭设备库（抹掉内存里的身份）。
#[tauri::command]
pub fn close_device_store(devices: State<'_, SharedDevices>) {
    devices.close();
}

/// 已配对设备列表。
#[tauri::command]
pub fn paired_devices(devices: State<'_, SharedDevices>) -> Vec<PairedInfo> {
    devices.devices()
}

/// 改本机设备名。
///
/// 这个名字会**广播到局域网**，所以要提醒用户别写敏感内容。
/// 提醒放在界面上，这里只做校验。
#[tauri::command]
pub fn rename_device(devices: State<'_, SharedDevices>, name: String) -> CmdResult<DeviceStatus> {
    let clean = crate::devices::sanitize_device_name(&name);
    devices
        .mutate(|s| {
            s.set_device_name(&clean)
                .map_err(|_| DeviceError::BadDeviceName)
        })
        .map_err(CmdError::from)?;
    Ok(devices.status())
}

/// 吊销一台设备的授权。
///
/// 吊销后对方**下次连接**就会被拒绝。已经建立的连接不会中断——
/// 这一点要在界面上说清楚，否则用户以为点了按钮就立刻断开了。
#[tauri::command]
pub fn revoke_device(
    devices: State<'_, SharedDevices>,
    fingerprint: String,
) -> CmdResult<Vec<PairedInfo>> {
    let want =
        crate::devices::parse_fingerprint(&fingerprint).ok_or_else(|| CmdError::code("bad_fingerprint"))?;

    devices
        .mutate(|s| {
            let Some(pk) = s
                .find_by_fingerprint(&want)
                .map(|d| d.public_key.clone())
            else {
                return Err(DeviceError::NoSuchDevice);
            };
            if s.revoke(&pk) {
                Ok(())
            } else {
                Err(DeviceError::NoSuchDevice)
            }
        })
        .map_err(CmdError::from)?;

    Ok(devices.devices())
}

/// 搜索局域网设备。
///
/// mDNS 浏览是阻塞的（`mdns-sd` 自己管线程），放阻塞线程。
#[tauri::command]
pub async fn discover_devices(
    devices: State<'_, SharedDevices>,
    timeout_secs: u64,
) -> CmdResult<Vec<DiscoveredDevice>> {
    // 上限 30 秒：更久的话用户会以为界面卡死了，
    // 而 mDNS 通常两三秒就能收齐同网段的响应
    let secs = timeout_secs.clamp(1, 30);
    let handle: SharedDevices = Arc::clone(&devices);

    tauri::async_runtime::spawn_blocking(move || {
        crate::lan::discover(&handle, Duration::from_secs(secs))
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
    .map_err(CmdError::from)
}

/// 开始等待对方连入完成配对，立刻返回配对码。
#[tauri::command]
pub async fn pair_listen(
    devices: State<'_, SharedDevices>,
    pair: State<'_, SharedPair>,
    port: u16,
    expires_days: u32,
) -> CmdResult<PairPhase> {
    crate::lan::pair_listen_start(
        Arc::clone(&pair),
        Arc::clone(&devices),
        port,
        expires_days,
    )
    .await
    .map_err(CmdError::from)
}

/// 主动连接对方完成配对。
#[tauri::command]
pub async fn pair_with(
    devices: State<'_, SharedDevices>,
    pair: State<'_, SharedPair>,
    addr: String,
    pin: String,
    expires_days: u32,
) -> CmdResult<PairPhase> {
    crate::lan::pair_connect(
        Arc::clone(&pair),
        Arc::clone(&devices),
        addr,
        pin,
        expires_days,
    )
    .await
    .map_err(CmdError::from)
}

/// 查询配对进展。前端轮询这个。
#[tauri::command]
pub fn pair_status(pair: State<'_, SharedPair>) -> PairPhase {
    pair.phase()
}

/// 取消正在进行的配对。
///
/// 配对码一旦显示过就该能立刻作废。
#[tauri::command]
pub fn pair_cancel(pair: State<'_, SharedPair>) {
    pair.cancel();
}

/// 开始共享一个目录。
///
/// **不需要文件密码**：服务端只搬运密文（决策 DEC-16）。
#[tauri::command]
pub async fn start_share(
    devices: State<'_, SharedDevices>,
    share: State<'_, SharedShare>,
    dir: String,
    port: u16,
    advertise: bool,
) -> CmdResult<ShareStatus> {
    crate::lan::share_start(
        Arc::clone(&share),
        Arc::clone(&devices),
        dir,
        port,
        advertise,
    )
    .await
    .map_err(CmdError::from)
}

/// 停止共享。
///
/// 返回 `Result` 不是因为它会失败，而是 Tauri 的约束：async 命令
/// 只要带引用参数（这里是 `State`）就必须返回 `Result`。
#[tauri::command]
pub async fn stop_share(share: State<'_, SharedShare>) -> CmdResult<ShareStatus> {
    let task = Arc::clone(&share);
    Ok(crate::lan::share_stop(&task).await)
}

/// 共享服务当前状态。
#[tauri::command]
pub fn share_status(share: State<'_, SharedShare>) -> ShareStatus {
    share.status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_error_maps_to_translation_key() {
        let e: CmdError = DeviceError::WrongPassword.into();
        assert_eq!(e.code, "store_wrong_password");
        // 后端不拼中文：界面可能是英文的
        let j = serde_json::to_string(&e).unwrap();
        assert!(!j.contains("密码"));
    }

    #[test]
    fn all_device_errors_have_codes() {
        for e in [
            DeviceError::NoStorePath,
            DeviceError::WrongPassword,
            DeviceError::BadDeviceName,
            DeviceError::SaveFailed,
            DeviceError::NotOpen,
            DeviceError::NoSuchDevice,
            DeviceError::PairFailed,
            DeviceError::ConnectFailed,
            DeviceError::Internal,
        ] {
            let c: CmdError = e.into();
            assert!(!c.code.is_empty());
        }
    }
}
