//! 远程位置加密的密钥收集：把「会话里已解锁的 KEK」+「无库回退的机器密钥」
//! 汇成一份，供 Telegram 位置的 session 落盘/解锁使用。
//!
//! # 为什么单独放这里
//!
//! `omy-remote` 的 `place_secret`/`session` 不认识 GUI 的 `AppState`
//! （那会让底层反向依赖 GUI）。所以「从当前会话取出所有已解锁 KEK」这件事
//! 由 GUI 层做，底层只接一批 `Kek` / `SlotKey`。
//!
//! # 两个用途、两个入口
//!
//! - [`slot_keks`]：**落盘**时用——把当前会话所有已解锁凭据的 KEK 取出来，
//!   给新位置每把各建一个槽。于是用户当前输过的任一密码之后都能开这个位置。
//!   会话为空（没解锁任何库）时回退到机器密钥，保证位置仍能加密落盘。
//! - [`unlock_keks`]：**解锁**时用——同样是会话全部 KEK + 机器密钥，逐个去试
//!   位置的槽。机器密钥也带上，是为了让「无库回退」建的位置在无人解锁时也能
//!   自动开（等同旧行为）。

use omy_core::crypto::Kek;
use omy_remote::telegram::place_secret::SlotKey;
use omy_remote::telegram::session as tgsession;

use crate::commands::Shared;

/// 一把待登记的槽密钥：拥有 KEK 与它的类型/显示名（labels 供 UI/调试，不含秘密）。
///
/// 拥有所有权而非借用，是因为 KEK 来自会话锁内、要在锁释放后继续用；
/// `SlotKey` 借用它，见 [`as_slot_keys`]。
pub struct OwnedSlotKey {
    /// 包裹用的 KEK。
    pub kek: Kek,
    /// 凭据类型串（vault/device/recovery/portable/machine）。
    pub kind: String,
    /// 显示名。
    pub label: String,
}

/// 落盘用：会话里已解锁的全部 KEK；会话为空时回退机器密钥。
///
/// 返回空 `Vec` 表示「既没有已解锁的库、也没有可用凭据库」——那种情况下
/// 位置无法安全加密落盘，调用方应按「本机存不住登录态」处理（等同旧的
/// `NoProtector`）。
#[must_use]
pub fn slot_keks(state: &Shared) -> Vec<OwnedSlotKey> {
    let mut keys = session_slot_keys(state);
    if keys.is_empty() {
        // 无库回退：用机器密钥占一个槽，位置仍加密、解锁不需输密码
        if let Some(kek) = tgsession::machine_fallback_kek() {
            keys.push(OwnedSlotKey {
                kek,
                kind: String::from("machine"),
                label: String::from("machine"),
            });
        }
    }
    keys
}

/// 解锁用：会话全部 KEK + 机器密钥（后者让无库回退建的位置自动开）。
#[must_use]
pub fn unlock_keks(state: &Shared) -> Vec<Kek> {
    let mut keks: Vec<Kek> = state
        .with_session(|s| s.all_keks())
        .unwrap_or_default();
    if let Some(mk) = tgsession::machine_fallback_kek() {
        keks.push(mk);
    }
    keks
}

/// 会话里已解锁凭据的 KEK（不含机器密钥回退）。
fn session_slot_keys(state: &Shared) -> Vec<OwnedSlotKey> {
    // 会话密钥没有暴露每条的 label/kind，这里统一给 "vault"/"session"——
    // 槽的 kind/label 只用于显示，不影响解锁（解锁靠逐把 KEK 试）
    state
        .with_session(|s| s.all_keks())
        .unwrap_or_default()
        .into_iter()
        .map(|kek| OwnedSlotKey {
            kek,
            kind: String::from("vault"),
            label: String::from("session"),
        })
        .collect()
}

/// 把 [`OwnedSlotKey`] 借成 `SlotKey` 切片供底层建槽。
#[must_use]
pub fn as_slot_keys(owned: &[OwnedSlotKey]) -> Vec<SlotKey<'_>> {
    owned
        .iter()
        .map(|o| SlotKey { kek: &o.kek, kind: &o.kind, label: &o.label })
        .collect()
}

/// 落盘结果：区分「存好了」「本机存不住」两种，供登录收尾决定告诉用户什么。
pub enum SaveResult {
    /// 已加密落盘。
    Saved,
    /// 本机既没有已解锁的库、也没有凭据库——存不住，登录只在本次会话有效。
    /// 等同旧的 `NoProtector`：**绝不退回明文**。
    CannotPersist,
}

/// 把一份已抽取的登录态用 per-place 槽格式落到某账号文件。
///
/// keys = 会话已解锁 KEK（+ 无库回退机器密钥）。为空 ⇒ 本机存不住，返回
/// [`SaveResult::CannotPersist`]，**不落明文**。
///
/// # Errors
///
/// 建槽或写盘失败时返回底层 `SessionError`。
pub fn save_session_at(
    state: &Shared,
    saved: &tgsession::SavedSession,
    account: &str,
) -> Result<SaveResult, tgsession::SessionError> {
    let owned = slot_keks(state);
    if owned.is_empty() {
        return Ok(SaveResult::CannotPersist);
    }
    let keys = as_slot_keys(&owned);
    let path = tgsession::session_path_of(account)?;
    tgsession::save_with_slots(saved, &path, &keys)?;
    Ok(SaveResult::Saved)
}
