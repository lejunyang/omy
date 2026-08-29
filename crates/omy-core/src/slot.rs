//! Key Slot Area 的读写。
//!
//! 8 个固定 slot，每个 48 字节（32 字节被包裹的 FEK + 16 字节 tag）。
//!
//! # 可否认性依赖两条不可妥协的规则
//!
//! 1. **槽数固定为 8**：1 个密码和 3 个密码的文件字节长度完全相同。
//! 2. **未使用的槽填充随机字节**，不得填零。否则「哪些槽在用」直接可见，
//!    进而暴露该文件有几个密码。
//!
//! 验证结果见 `docs/research/appendix/verification-report.md` §3：
//! slot 区香农熵 7.491 bits/byte，落在同长度纯随机基线区间内。

use crate::crypto::{CipherId, Fek, Kek, SecretKey, ZERO_NONCE};
use crate::error::{Error, Result};
use crate::header::{SLOT_AREA_LEN, SLOT_COUNT, SLOT_LEN};
use crate::util::Writer;

/// 构建 Key Slot Area。
///
/// `keks` 中第 `i` 个 KEK 占用第 `i` 个 slot，其余槽填随机字节。
///
/// # Errors
///
/// - [`Error::TooManySlots`]：KEK 数量超过 8
/// - AEAD 加密失败
pub fn build_slot_area(
    keks: &[Kek],
    fek: &Fek,
    file_uuid: &[u8; 16],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    if keks.len() > SLOT_COUNT {
        return Err(Error::TooManySlots { got: keks.len(), max: SLOT_COUNT });
    }

    let mut w = Writer::with_capacity(SLOT_AREA_LEN);
    for (i, kek) in keks.iter().enumerate() {
        // i < SLOT_COUNT = 8，转换必然成功
        let idx = u16::try_from(i).map_err(|_| Error::TooManySlots {
            got: keks.len(),
            max: SLOT_COUNT,
        })?;
        let wrap_key = kek.derive_slot_key(file_uuid, idx);
        let wrapped = cipher.encrypt(&wrap_key, &ZERO_NONCE, fek.as_key().as_bytes(), &[])?;
        debug_assert_eq!(wrapped.len(), SLOT_LEN, "包裹后的 slot 必须是 48 字节");
        w.bytes(&wrapped);
    }

    // 剩余槽填随机字节——这是可否认性的关键，绝不能改成填零
    let used = keks.len().saturating_mul(SLOT_LEN);
    w.random(SLOT_AREA_LEN.saturating_sub(used));

    let out = w.into_vec();
    debug_assert_eq!(out.len(), SLOT_AREA_LEN, "slot 区必须恰好 384 字节");
    Ok(out)
}

/// 尝试用一组候选 KEK 解包出 FEK。
///
/// 对每个 KEK 尝试每个 slot。返回 `(FEK, kek_index, slot_index)`。
///
/// # 性能
///
/// 每次尝试只做一次 HKDF（约 13 µs）加一次 AEAD 解密，**不涉及 Argon2**。
/// 因此扫描大量文件是可行的：10,000 文件 × 8 槽在秒级完成。
/// 前提是调用方已缓存 `Kek`——若在此处从密码重新派生，性能会劣化约 13,700 倍。
///
/// # Errors
///
/// 所有组合都失败时返回 [`Error::NoMatchingSlot`]。这**不代表文件损坏**，
/// 也可能只是该文件不属于当前解锁的任何密码，面向用户的文案必须涵盖两种可能。
pub fn unwrap_fek(
    slot_area: &[u8],
    keks: &[Kek],
    file_uuid: &[u8; 16],
    cipher: CipherId,
) -> Result<(Fek, usize, u16)> {
    if slot_area.len() < SLOT_AREA_LEN {
        return Err(Error::Truncated {
            context: "key slot area",
            need: SLOT_AREA_LEN,
            got: slot_area.len(),
        });
    }

    for (ki, kek) in keks.iter().enumerate() {
        for si in 0..SLOT_COUNT {
            let start = si.saturating_mul(SLOT_LEN);
            let end = start.saturating_add(SLOT_LEN);
            let Some(slot) = slot_area.get(start..end) else { continue };

            let idx = match u16::try_from(si) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let wrap_key = kek.derive_slot_key(file_uuid, idx);
            if let Some(pt) = cipher.decrypt(&wrap_key, &ZERO_NONCE, slot, &[])? {
                if pt.len() != crate::crypto::KEY_LEN {
                    // 认证已通过却长度不对：理论上不可能，保守跳过
                    continue;
                }
                let mut raw = [0u8; crate::crypto::KEY_LEN];
                let Some(src) = pt.get(..crate::crypto::KEY_LEN) else { continue };
                raw.copy_from_slice(src);
                return Ok((Fek::from_key(SecretKey::from_bytes(raw)), ki, idx));
            }
        }
    }
    Err(Error::NoMatchingSlot)
}
