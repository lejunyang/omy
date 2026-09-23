//! 远程位置解锁用的密钥收集：把「会话里已解锁的 KEK」+「无库回退的机器密钥」
//! 汇成一份，供 Telegram 位置解锁（`connect_saved_with_keks`）与显式加密/取消
//! （`telegram_place_encrypt`/`decrypt`）使用。
//!
//! # 为什么单独放这里
//!
//! `omy-remote` 的 `place_secret`/`session` 不认识 GUI 的 `AppState`（那会让底层
//! 反向依赖 GUI）。所以「从当前会话取出所有已解锁 KEK」这件事由 GUI 层做，底层
//! 只接一批 `Kek`。
//!
//! # 为什么带上机器密钥
//!
//! 机器密钥也放进来，是为了让「无库回退」建的位置（用机器密钥占槽）在无人解锁
//! 时也能自动开、等同旧行为；同时它也是「用户没有任何 omy 密码时仍能加密位置」
//! 的那把钥匙。

use omy_core::crypto::Kek;
use omy_remote::telegram::session as tgsession;

use crate::commands::Shared;

/// 会话里已解锁的全部 KEK + 无库回退机器密钥。
///
/// 解锁加密位置时逐把去试；显式加密位置时用它们建槽（会话有密码就用密码、
/// 没有就用机器密钥）。返回可能为空——那说明既没解锁任何库、也没有凭据库，
/// 调用方据此拒绝加密或按「本机存不住」处理。
#[must_use]
pub fn unlock_keks(state: &Shared) -> Vec<Kek> {
    let mut keks: Vec<Kek> = state.with_session(|s| s.all_keks()).unwrap_or_default();
    if let Some(mk) = tgsession::machine_fallback_kek() {
        keks.push(mk);
    }
    keks
}
