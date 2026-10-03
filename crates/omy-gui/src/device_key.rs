//! 设备密钥：用 Windows Hello 免密解锁。
//!
//! # 它在这一层要做三件事
//!
//! 1. 告诉界面「这个库能不能用、挂没挂」——决定要不要显示那个按钮
//! 2. 挂载（需要先验一次密码）
//! 3. 免密解锁：从硬件取出密钥，装进会话
//!
//! # 为什么挂载要先验密码
//!
//! 不验的话，任何能碰到这台机器的人都可以给别人的文件挂一把自己的设备
//! 密钥，此后凭指纹就能打开——那等于「物理接触 = 访问权」。
//!
//! # 装入会话用累加语义
//!
//! 走 `add_kek` 而不是 `insert_kek`：用户按了指纹之后，之前手动输入的
//! 密码不该消失。后者只要 label 撞上就会静默挤掉一条，而那条可能正是
//! 他刚输的另一个库的密码。

use crate::commands::{CmdError, CmdResult, Shared};
use omy_core::crypto::Kek;
use omy_secret::Protector as _;
use std::sync::Arc;
use tauri::State;

/// 服务名，与 CLI 保持一致——否则 CLI 挂的设备密钥 GUI 找不到。
const DEVICE_SERVICE: &str = "omy";

/// 一个目录里所有库的 vault_salt。
///
/// 前端给的是目录而不是盐，与解锁路径保持一致：让前端自己去拼盐会多一层
/// 没人验证的转换，而且它得先知道「同一个目录里可能有多个库」这件事。
///
/// 返回**全部**而不是第一个。实测踩过：用户的「文档」里有两个不同盐的库，
/// 只看第一个的话，设备密钥挂在第二个库上时界面查到「未启用」，Hello 按钮
/// 直接不出现——而用户明明挂过，只能以为功能坏了。随手加密几个文件就会
/// 形成这种目录，它一点都不罕见。
fn salts_of_dir(dir: &str) -> CmdResult<Vec<[u8; 16]>> {
    let vaults = crate::commands::vault_params_of(dir.to_owned())?;
    let salts: Vec<[u8; 16]> = vaults
        .iter()
        .filter_map(|v| crate::commands::parse_salt(&v.salt))
        .collect();
    if salts.is_empty() {
        return Err(CmdError::code("no_vault_found"));
    }
    Ok(salts)
}

/// 设备密钥在这台机器上的状态。
#[derive(Debug, serde::Serialize)]
pub struct DeviceKeyStatus {
    /// 这台机器支不支持（有 TPM、Hello 可用）。
    ///
    /// false 时界面不该显示任何设备密钥入口——摆一个点了就报错的按钮
    /// 比没有更糟。
    pub available: bool,
    /// 不支持的原因，给界面显示。available 为 true 时为空串。
    pub reason: String,
    /// 这个库有没有挂过设备密钥。
    pub enrolled: bool,
}

/// 查询设备密钥状态。
///
/// **不会弹 Hello**：用户只是想看看状态，为此弹窗很突兀。代价是
/// `enrolled` 的判断只看「有没有那份密文」，不验证它还能不能解开——
/// 清除 TPM 之后这里仍会报 true，直到真去解锁才失败。
///
/// 这个取舍是有意的：让状态查询免打扰，比让它绝对准确更要紧。真正的
/// 失败路径（解锁时）已经有明确的错误提示。
///
/// # Errors
///
/// 盐格式不对时返回 `bad_salt`。
#[tauri::command]
pub async fn device_key_status(dir: String) -> CmdResult<DeviceKeyStatus> {
    let salts = salts_of_dir(&dir)?;
    let p = match omy_secret::device_protector(DEVICE_SERVICE) {
        Ok(p) => p,
        Err(e) => {
            return Ok(DeviceKeyStatus {
                available: false,
                reason: e.to_string(),
                enrolled: false,
            });
        }
    };
    // 任一库挂了就算这个位置启用了。只看第一个的话，挂在第二个库上的
    // 设备密钥会被当成没挂，Hello 按钮直接不出现——而用户明明挂过
    let enrolled = salts
        .iter()
        .any(|s| p.has(&omy_core::devicekey::slot_id(s)));
    Ok(DeviceKeyStatus { available: true, reason: String::new(), enrolled })
}

/// 用设备密钥解锁，把 KEK 装进会话。
///
/// 会弹 Hello。
///
/// # Errors
///
/// - `device_key_unavailable`：这台机器没有 TPM 或 Hello
/// - `device_key_not_enrolled`：这个库没挂过
/// - `user_cancelled`：用户在 Hello 弹窗上取消了
#[tauri::command]
pub async fn device_key_unlock(
    state: State<'_, Shared>,
    dir: String,
) -> CmdResult<crate::commands::UnlockResult> {
    let salts = salts_of_dir(&dir)?;

    let handle: Shared = Arc::clone(&state);
    // Hello 会阻塞等用户确认，绝不能占着异步执行器——那会让整个界面僵住
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let p = omy_secret::device_protector(DEVICE_SERVICE)
            .map_err(|e| CmdError::with(
                "device_key_unavailable",
                serde_json::json!({ "detail": e.to_string() }),
            ))?;
        // 先挑出真正挂过的那些库，再去解封。
        //
        // 顺序很重要：先 has() 后 retrieve()，因为 retrieve 会弹 Hello。
        // 反过来的话，一个混着三个库的目录会连弹三次，其中两次注定失败
        let mounted: Vec<[u8; 16]> = salts
            .iter()
            .copied()
            .filter(|s| p.has(&omy_core::devicekey::slot_id(s)))
            .collect();
        if mounted.is_empty() {
            return Err(CmdError::code("device_key_not_enrolled"));
        }

        // 一次 Hello 确认解封所有挂过的库。
        //
        // 硬件那边每个库各有一把密钥，但门禁是按进程的会话给的，所以
        // 用户只需确认一次
        let mut keks: Vec<([u8; 16], Kek)> = Vec::new();
        for salt in &mounted {
            let id = omy_core::devicekey::slot_id(salt);
            let secret = p.retrieve(&id).map_err(|e| match e {
                omy_secret::Error::NotFound => CmdError::code("device_key_not_enrolled"),
                omy_secret::Error::UserCancelled => CmdError::code("user_cancelled"),
                other => CmdError::with(
                    "device_key_failed",
                    serde_json::json!({ "detail": other.to_string() }),
                ),
            })?;
            keks.push((*salt, omy_core::devicekey::kek_from_secret(&secret, salt)));
        }

        // with_session 返回 Option：锁中毒时为 None。
        // 那时如实报 internal，不能假装装入成功——界面会显示已解锁
        // 而实际会话是空的
        handle
            .with_session(|s| {
                // 累加而非替换：手动输入的密码不该因为按了指纹而消失
                let mut added = 0usize;
                for (salt, kek) in keks {
                    if s.add_kek(
                        "这台设备",
                        omy_core::session::CredentialKind::Device,
                        &salt,
                        kek,
                    ) {
                        added += 1;
                    }
                }
                (s.len(), added > 0)
            })
            .ok_or_else(|| CmdError::code("internal"))
    })
    .await
    .map_err(|_| CmdError::code("internal"))??;

    let (total, added) = outcome;
    Ok(crate::commands::UnlockResult { credentials: total, vaults_unlocked: 1, added })
}

/// 给一个库挂上设备密钥。
///
/// 需要先验一次密码。之后每次解锁只要 Hello。
///
/// # Errors
///
/// - `device_key_unavailable`：这台机器不支持
/// - `wrong_password`：密码打不开那个样本文件
#[tauri::command]
pub async fn device_key_enroll(
    state: State<'_, Shared>,
    path: String,
    password: String,
) -> CmdResult<DeviceKeyEnrollResult> {
    if password.is_empty() {
        return Err(CmdError::code("empty_password"));
    }
    let handle: Shared = Arc::clone(&state);
    tauri::async_runtime::spawn_blocking(move || enroll(&handle, &path, &password))
        .await
        .map_err(|_| CmdError::code("internal"))?
}

/// 挂载结果。
#[derive(Debug, serde::Serialize)]
pub struct DeviceKeyEnrollResult {
    /// 改写了几个文件（树形时 > 1）。
    pub files_changed: usize,
    /// 挂载后这个文件上有几把有效钥匙。
    pub slots_in_use: usize,
}

fn enroll(state: &Shared, path: &str, password: &str) -> CmdResult<DeviceKeyEnrollResult> {
    let target = std::path::Path::new(path);
    // 目录也支持：树形的每个文件共用同一个 vault_salt，
    // 所以一把设备密钥覆盖整棵树
    let sample = if target.is_dir() {
        omy_core::tree::find_any_file(target)
            .ok_or_else(|| CmdError::code("not_encrypted_tree"))?
    } else {
        target.to_path_buf()
    };
    let data = std::fs::read(&sample).map_err(|_| CmdError::code("io_error"))?;
    let header = omy_core::file::peek_header(&data).map_err(|_| CmdError::code("not_omy_file"))?;

    let cur = Kek::from_password(password.as_bytes(), &header.vault_salt, header.argon2_params())
        .map_err(|_| CmdError::code("kdf_failed"))?;
    // 真去开一次。from_password 只派生不校验，不验的话会先造好硬件密钥
    // 才发现密码不对——白弹一次 Hello，TPM 里还留下一把用不到的密钥。
    // CLI 那边踩过这个坑
    omy_core::file::open(&data, &[cur.duplicate()])
        .map_err(|_| CmdError::code("wrong_password"))?;

    let p = omy_secret::device_protector(DEVICE_SERVICE).map_err(|e| {
        CmdError::with(
            "device_key_unavailable",
            serde_json::json!({ "detail": e.to_string() }),
        )
    })?;
    let id = omy_core::devicekey::slot_id(&header.vault_salt);
    // 已有就复用，避免重复弹创建确认
    let secret = match p.retrieve(&id) {
        Ok(k) => k,
        Err(omy_secret::Error::NotFound) => {
            let k = omy_secret::random_key();
            p.store(&id, &k).map_err(|e| {
                CmdError::with(
                    "device_key_store_failed",
                    serde_json::json!({ "detail": e.to_string() }),
                )
            })?;
            k
        }
        Err(omy_secret::Error::UserCancelled) => return Err(CmdError::code("user_cancelled")),
        Err(e) => {
            return Err(CmdError::with(
                "device_key_failed",
                serde_json::json!({ "detail": e.to_string() }),
            ));
        }
    };
    let dev_kek = omy_core::devicekey::kek_from_secret(&secret, &header.vault_salt);

    if target.is_dir() {
        let keep = vec![cur.duplicate(), dev_kek];
        let rep = omy_core::tree::rekey_tree(
            target,
            &[cur],
            &keep,
            &header.vault_salt,
            header.cipher_id,
            omy_core::keyslot::OtherSlots::Carry,
        )
        .map_err(|_| CmdError::code("rewrite_failed"))?;
        let _ = state;
        return Ok(DeviceKeyEnrollResult {
            files_changed: rep.changed,
            slots_in_use: 0,
        });
    }

    let opened = omy_core::file::open(&data, &[cur.duplicate()])
        .map_err(|_| CmdError::code("wrong_password"))?;
    let keep = vec![cur.duplicate(), dev_kek.duplicate()];
    let out = if opened.is_slot_managed() {
        // 可管理模式：类型如实记成 device，否则用户在清单里
        // 分不出哪个是指纹解锁
        use omy_core::keyslot::SlotPlan;
        use omy_core::slotdir::{SlotEntry, SlotKind};
        let mut dir = opened
            .slot_directory()
            .map_err(|_| CmdError::code("bad_slot_directory"))?;
        let free = dir.first_free().ok_or_else(|| CmdError::code("slots_full"))?;
        let mut plans: Vec<SlotPlan> = (0..omy_core::header::SLOT_COUNT)
            .map(|_| SlotPlan::Keep)
            .collect();
        *plans.get_mut(free).ok_or_else(|| CmdError::code("internal"))? =
            SlotPlan::Write(dev_kek);
        dir.set(free, SlotEntry::of(SlotKind::Device))
            .map_err(|_| CmdError::code("internal"))?;
        omy_core::keyslot::rewrite_slots_managed(&data, &[cur.duplicate()], &plans, &dir)
            .map_err(|_| CmdError::code("rewrite_failed"))?
    } else {
        omy_core::keyslot::rewrite_slots(
            &data,
            &[cur.duplicate()],
            &keep,
            // Carry：这个文件上可能还挂着恢复码，加设备密钥不该抹掉它
            omy_core::keyslot::OtherSlots::Carry,
        )
        .map_err(|_| CmdError::code("rewrite_failed"))?
    };

    // 写回前自证每把保留的钥匙都能开。顺序不能反——先覆盖再发现打不开，
    // 用户就同时失去了文件和访问权
    for k in &keep {
        omy_core::file::open(&out.bytes, &[k.duplicate()])
            .map_err(|_| CmdError::code("rewrite_verify_failed"))?;
    }
    omy_core::fsatomic::write_atomic(target, &out.bytes)
        .map_err(|_| CmdError::code("io_error"))?;

    let _ = state;
    Ok(DeviceKeyEnrollResult {
        files_changed: 1,
        slots_in_use: out.slot_used,
    })
}

/// 移除这个库的设备密钥。
///
/// 只清硬件里那把密钥，不动文件：没了钥匙那个槽位就是一段随机字节，
/// 与空槽不可区分，留着不影响任何事。要精确清掉它得先解开文件，
/// 反而多一次 Hello 确认。
///
/// # Errors
///
/// 这台机器不支持时返回 `device_key_unavailable`。
#[tauri::command]
pub async fn device_key_forget(dir: String) -> CmdResult<()> {
    let salts = salts_of_dir(&dir)?;
    tauri::async_runtime::spawn_blocking(move || {
        let p = omy_secret::device_protector(DEVICE_SERVICE).map_err(|e| {
            CmdError::with(
                "device_key_unavailable",
                serde_json::json!({ "detail": e.to_string() }),
            )
        })?;
        // 清掉这个目录里所有库的设备密钥。
        //
        // 「关闭免密解锁」在用户看来是对这个位置说的，只清第一个库会
        // 留下一个他以为已经关掉、实际还在的密钥
        for salt in &salts {
            let id = omy_core::devicekey::slot_id(salt);
            p.delete(&id).map_err(|e| {
                CmdError::with(
                    "device_key_failed",
                    serde_json::json!({ "detail": e.to_string() }),
                )
            })?;
        }
        Ok(())
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}
