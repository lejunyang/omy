//! 远程位置的 Tauri 命令。
//!
//! # 列目录时顺带识别加密文件
//!
//! 这是格式设计带来的便宜：识别一个 `.omy` 文件只需读**开头 480 字节**
//! （见 `omy_core::scan` 的模块文档），载荷完全不碰。放到远程就是一次
//! Range 请求——扫 100 个文件只下约 48 KB，而不是把文件拉下来。
//!
//! # 三种状态必须分开
//!
//! 已解锁 / 是 omy 但当前密码打不开 / **没能探测成功**（网络失败）。
//! 第三种混进第二种是最糟的结果：用户会以为自己记错密码而反复尝试，
//! 真正的问题却是网络。

use std::sync::Arc;

use crate::commands::{CmdError, CmdResult};
use crate::places::{PlaceInfo, PlaceRegistry};
use omy_remote::webdav::WebDavConfig;
use omy_remote::{RemoteStore, Error as RemoteError};

/// 远程目录里的一项，已附带识别结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteEntry {
    /// 条目 id（在该位置内的路径）。
    pub id: String,
    /// 显示名（未解密的名字）。
    pub name: String,
    /// 是否为目录。
    pub is_dir: bool,
    /// 字节数。
    pub size: Option<u64>,
    /// 是否为 omy 加密文件。
    pub is_encrypted: bool,
    /// 是否已用当前会话的密钥解开。
    pub unlocked: bool,
    /// 解开后的真实文件名。
    pub real_name: Option<String>,
    /// 明文大小。
    pub plaintext_size: Option<u64>,
    /// 探测是否失败（网络原因）。
    ///
    /// 与 `unlocked == false` 是**两回事**：那个是「密码不对」，
    /// 这个是「根本没读到」。界面必须用不同的图标和文案，
    /// 否则用户会去反复试密码而不是检查网络。
    pub probe_failed: bool,
}

/// 把远程错误映射为结构化错误码。
fn to_cmd_err(e: &RemoteError) -> CmdError {
    let code = match e {
        RemoteError::Unauthorized => "remote_unauthorized",
        RemoteError::Forbidden => "remote_forbidden",
        RemoteError::NotFound(_) => "remote_not_found",
        RemoteError::RateLimited => "remote_rate_limited",
        RemoteError::Unsupported(_) => "remote_unsupported",
        RemoteError::Network(_) => "remote_network",
        _ => "remote_failed",
    };
    CmdError::with(code, serde_json::json!({ "detail": e.to_string() }))
}

/// 添加一个 WebDAV 位置。
///
/// # Errors
///
/// URL 非法或客户端构造失败时返回。
#[tauri::command]
pub fn remote_place_add(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    name: String,
    url: String,
    username: String,
    password: String,
    vendor: String,
    writable: bool,
) -> CmdResult<String> {
    let cfg = WebDavConfig {
        base_url: url,
        username,
        password,
        vendor: crate::places::parse_vendor(&vendor),
        writable,
        ..WebDavConfig::default()
    };
    reg.add_webdav(name, cfg).map_err(|e| to_cmd_err(&e))
}

/// 列出已注册的远程位置。
#[tauri::command]
pub fn remote_place_list(reg: tauri::State<'_, Arc<PlaceRegistry>>) -> Vec<PlaceInfo> {
    reg.list()
}

/// 移除一个远程位置。
#[tauri::command]
pub fn remote_place_remove(reg: tauri::State<'_, Arc<PlaceRegistry>>, id: String) {
    reg.remove(&id);
}

/// 浏览远程目录，并尝试识别其中的加密文件。
///
/// # Errors
///
/// 位置不存在、网络失败或认证失败时返回。
#[tauri::command]
pub async fn remote_browse(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<RemoteEntry>> {
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;

    let items = place.store.list(&dir).await.map_err(|e| to_cmd_err(&e))?;

    let mut out = Vec::with_capacity(items.len());
    for it in items {
        let mut e = RemoteEntry {
            id: it.id.clone(),
            name: it.name.clone(),
            is_dir: it.is_dir,
            size: it.size,
            is_encrypted: false,
            unlocked: false,
            real_name: None,
            plaintext_size: None,
            probe_failed: false,
        };

        // 目录不必探测；太小的文件不可能是 omy（连头部都装不下）
        if it.is_dir || it.size.unwrap_or(0) < omy_core::scan::MIN_FILE_SIZE as u64 {
            out.push(e);
            continue;
        }

        // 只读前 480 字节：识别 + 试解锁全靠这一段，载荷完全不碰
        let probe = place
            .store
            .read_range(&it.id, 0, omy_core::scan::MIN_PROBE_SIZE as u64)
            .await;

        match probe {
            Ok(bytes) => {
                if let Some((unlocked, real, psize)) = probe_omy(&bytes, &state) {
                    e.is_encrypted = true;
                    e.unlocked = unlocked;
                    e.real_name = real;
                    e.plaintext_size = psize;
                }
            }
            // 读不到就如实标记，不要静默当成普通文件——那会让用户
            // 以为文件不是加密的，而实际只是这次没读到
            Err(_) => e.probe_failed = true,
        }
        out.push(e);
    }
    Ok(out)
}

/// 用会话里的密钥尝试识别并解开一段文件头。
///
/// 返回 `None` 表示不是 omy 文件。
fn probe_omy(
    bytes: &[u8],
    state: &crate::commands::Shared,
) -> Option<(bool, Option<String>, Option<u64>)> {
    let header = omy_core::file::peek_header(bytes).ok()?;

    // 能解析出头部就说明是 omy 文件，即便打不开
    let keks = state.with_session(|s| {
        s.all_for(&header.vault_salt)
            .into_iter()
            .map(|c| c.kek)
            .collect::<Vec<_>>()
    });

    let Some(keks) = keks else {
        return Some((false, None, None));
    };
    if keks.is_empty() {
        return Some((false, None, None));
    }

    // open 只访问 data[..header_len]，所以传头部字节就够——
    // 这正是远程不必下载整个文件也能显示真实文件名的原因
    match omy_core::file::open(bytes, &keks) {
        Ok(opened) => {
            let name = opened.filename().ok();
            Some((true, name, Some(opened.header.plaintext_size)))
        }
        // 解不开是正常情况：文件可能属于另一个密码集
        Err(_) => Some((false, None, None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 错误码要能区分认证、限流与网络，而不是一律「失败」。
    ///
    /// 不这样会怎样：界面只能显示「操作失败」，而这三种情况用户
    /// 该做的事完全不同——重新登录、等一会、检查网络。
    #[test]
    fn error_codes_are_distinguishable() {
        assert_eq!(to_cmd_err(&RemoteError::Unauthorized).code, "remote_unauthorized");
        assert_eq!(to_cmd_err(&RemoteError::RateLimited).code, "remote_rate_limited");
        assert_eq!(
            to_cmd_err(&RemoteError::Network(String::from("x"))).code,
            "remote_network"
        );
        assert_eq!(
            to_cmd_err(&RemoteError::NotFound(String::from("a"))).code,
            "remote_not_found"
        );
    }

    /// 序列化字段名是前端契约，改名等于改协议。
    ///
    /// 不这样会怎样：前端读 `probe_failed` 得到 undefined，
    /// 网络失败的条目会被当成「密码不对」显示，用户去反复试密码。
    #[test]
    fn entry_fields_are_stable() {
        let e = RemoteEntry {
            id: String::from("/a.omy"),
            name: String::from("a.omy"),
            is_dir: false,
            size: Some(1000),
            is_encrypted: true,
            unlocked: false,
            real_name: None,
            plaintext_size: None,
            probe_failed: true,
        };
        let j = serde_json::to_value(&e).expect("序列化");
        for k in [
            "id", "name", "is_dir", "size", "is_encrypted", "unlocked", "real_name",
            "plaintext_size", "probe_failed",
        ] {
            assert!(j.get(k).is_some(), "字段 {k} 不能改名或缺失");
        }
    }

    /// 非 omy 数据必须返回 None，而不是误判成加密文件。
    ///
    /// 不这样会怎样：普通文件被标成加密文件，界面给出解密入口，
    /// 点下去必然失败。
    #[test]
    fn non_omy_bytes_are_not_encrypted() {
        let state: crate::commands::Shared =
            std::sync::Arc::new(crate::state::AppState::new());
        let junk = vec![0x41u8; 600];
        assert!(probe_omy(&junk, &state).is_none(), "随机数据不该被当成 omy");
        // 太短的数据同样不该误判
        assert!(probe_omy(b"OMY", &state).is_none());
    }
}
