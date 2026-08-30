//! 远端共享的 Tauri 命令。

use crate::commands::{CmdError, CmdResult};
use crate::devices::DeviceSession;
use crate::remote::{PeerMeta, RemoteFile, RemoteSession};
use crate::state::AppState;
use std::sync::Arc;
use tauri::State;

type SharedRemote = Arc<RemoteSession>;
type SharedDevices = Arc<DeviceSession>;
type Shared = Arc<AppState>;

/// 连接一台已配对设备。
///
/// `addr` 留空时自动在局域网里找。
#[tauri::command]
pub async fn remote_connect(
    remote: State<'_, SharedRemote>,
    devices: State<'_, SharedDevices>,
    fingerprint: String,
    addr: Option<String>,
) -> CmdResult<PeerMeta> {
    let r: SharedRemote = Arc::clone(&remote);
    let d: SharedDevices = Arc::clone(&devices);
    Ok(crate::remote::connect(r, d, &fingerprint, addr).await?)
}

/// 断开当前远端连接。
#[tauri::command]
pub async fn remote_disconnect(remote: State<'_, SharedRemote>) -> CmdResult<()> {
    remote.disconnect().await;
    Ok(())
}

/// 拉取远端文件列表。
///
/// 每次都重新拉：对方可能加了新文件，也可能撤了共享。
/// 缓存列表会让用户看到已经不存在的条目，点开才发现取不到。
#[tauri::command]
pub async fn remote_list(
    remote: State<'_, SharedRemote>,
    state: State<'_, Shared>,
) -> CmdResult<Vec<RemoteFile>> {
    Ok(crate::remote::refresh(&remote, &state).await?)
}

/// 当前连接状态。
#[derive(Debug, serde::Serialize)]
pub struct RemoteStatus {
    /// 是否已连接。
    pub connected: bool,
    /// 对端信息。
    pub peer: Option<PeerMeta>,
}

/// 查询连接状态。
#[tauri::command]
pub fn remote_status(remote: State<'_, SharedRemote>) -> RemoteStatus {
    RemoteStatus {
        connected: remote.is_connected(),
        peer: remote.peer(),
    }
}

/// 用新解锁的密码重新解析远端列表。
///
/// 用户在远端视图里输密码后调用：不需要重新拉列表（头部已经在手上），
/// 只要拿新密钥再解一遍就行。省掉一次网络往返。
#[tauri::command]
pub async fn remote_relock(
    remote: State<'_, SharedRemote>,
    state: State<'_, Shared>,
) -> CmdResult<Vec<RemoteFile>> {
    if !remote.is_connected() {
        return Err(CmdError::code("not_connected"));
    }
    Ok(crate::remote::refresh(&remote, &state).await?)
}

/// 远端文件里出现过的 vault 参数。
///
/// # 为什么远端要单独有这个
///
/// 本地版 `vault_params_of` 是扫一个**目录**，读每个文件的头部取 salt。
/// 远端没有目录可扫——但我们手上已经有全部文件的头部了（LIST 时随
/// 条目一起返回），所以直接从内存里取，一次网络往返都不需要。
///
/// 去重是必需的：一个共享目录里可能混着用不同密码加密的文件，
/// 每个 vault 的 salt 和 Argon2 参数都不一样，逐个派生才能全部解开。
#[tauri::command]
pub fn remote_vaults(remote: State<'_, SharedRemote>) -> Vec<crate::commands::VaultParams> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for f in remote.list() {
        let Ok(h) = omy_core::file::peek_header(&f.header) else {
            continue;
        };
        let salt = crate::commands::hex_of(&h.vault_salt);
        if seen.insert(salt.clone()) {
            out.push(crate::commands::VaultParams {
                salt,
                m_kib: h.argon2_m_kib,
                t: h.argon2_t,
                p: h.argon2_p,
            });
        }
    }
    out
}
