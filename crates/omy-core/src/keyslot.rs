//! slot 区的原地改写：增删改密码，不触碰载荷。
//!
//! # 为什么必须是原地改写
//!
//! 密码变更的正确成本是**毫秒级**（文档 03 §4.3）。用「解密后重新加密」来
//! 代替是错的，且错得不止是慢：
//!
//! - 会换掉 `file_uuid` 与 `base_nonce`，全部密文字节随之改变。对外部观察者
//!   而言这是一个全新文件——增量备份要重传整份，去重失效；
//! - 大文件是数小时的重写，中途失败就可能同时失去新旧两份；
//! - 破坏了「改密码只动头部」这一可验证的性质。
//!
//! 所以本模块只重建 384 字节的 slot 区并重算 32 字节头部 MAC，
//! 载荷密文与 `file_uuid` **逐字节不变**。
//!
//! # 为什么不能只覆写「那一个 slot」
//!
//! 每个 slot 的包裹密钥由 `(KEK, file_uuid, slot_index)` 唯一确定，所以
//! 一个 slot 的密文只在它所在的下标上有效。但**我们无法知道其余 7 个 slot
//! 里哪些是真实的**——这正是可否认性的设计目标（空槽填随机，字节层面不可
//! 区分）。因此：
//!
//! - `add` 无法「找一个空槽填进去」，因为分不清空槽和别人的槽。若猜错就会
//!   静默覆盖掉另一个密码，用户直到下次用那个密码才会发现，那时已无法恢复。
//! - 唯一诚实的做法是：**要求调用方提供本次操作后应当保留的全部密码**，
//!   据此整体重建 slot 区。
//!
//! 这个约束不是实现偷懒，而是格式的直接后果，必须如实反映在 UI 文案里：
//! 「加一个密码」实际语义是「重新声明这个文件的密码集合」。
//!
//! # 与「轮换 FEK」的区别
//!
//! 本模块**不换 FEK**。所以移除一个密码后，如果攻击者留有旧文件副本，他仍
//!能用旧密码打开那个副本——移除只影响这一份文件的未来。真正的密钥轮换必须
//! 重新加密载荷，属于另一个操作。这一点必须对用户讲清楚，否则会给人错误的
//! 安全感。

use crate::crypto::{CipherId, Fek, Kek, ZERO_NONCE, header_mac};
use crate::error::{Error, Result};
use crate::header::{
    FIXED_HEADER_LEN, FixedHeader, HEADER_MAC_LEN, SLOT_AREA_LEN, SLOT_AREA_OFFSET, SLOT_COUNT,
    SLOT_LEN, TLV_AREA_OFFSET,
};
use crate::util::Writer;

/// slot 区改写的结果。
#[derive(Debug)]
pub struct RewriteOutcome {
    /// 改写后的完整文件字节。
    pub bytes: Vec<u8>,
    /// 解开原文件时命中的 slot 下标，供调用方展示。
    pub opened_slot: u16,
    /// 改写后占用的 slot 数量（即传入的 KEK 个数）。
    pub slot_used: usize,
}

/// 用一组新的 KEK 重建 slot 区，保留原 FEK 与全部载荷。
///
/// `unlock` 用于解开现有文件（只需其中一个能命中）；`keep` 是改写后应当能打开
/// 该文件的**全部** KEK，按顺序占用 slot 0..n，其余槽填随机字节。
///
/// # 为什么 `keep` 为空要报错而不是「清空所有密码」
///
/// 没有任何 slot 的文件永远无法再打开，等同于销毁数据。这种操作如果真的需要，
/// 应该由调用方明确地删除文件，而不是通过一次「移除密码」意外达成。
///
/// # Errors
///
/// - [`Error::TooManySlots`]：`keep` 超过 8 个
/// - [`Error::NoMatchingSlot`]：`unlock` 里没有能解开该文件的 KEK
/// - [`Error::HeaderMacMismatch`]：原文件头部已被篡改
/// - [`Error::MalformedHeader`]：`keep` 为空，或文件头部字段非法
pub fn rewrite_slots(data: &[u8], unlock: &[Kek], keep: &[Kek]) -> Result<RewriteOutcome> {
    if keep.is_empty() {
        return Err(Error::MalformedHeader {
            reason: "refusing to leave a file with zero key slots; it could never be opened again",
        });
    }
    if keep.len() > SLOT_COUNT {
        return Err(Error::TooManySlots { got: keep.len(), max: SLOT_COUNT });
    }

    // 完整走一遍 open：它会验证头部 MAC。绝不能跳过——若头部已被篡改，
    // 我们会把篡改后的字段连同新 MAC 一起签进去，等于替攻击者背书。
    let opened = crate::file::open(data, unlock)?;
    let header = FixedHeader::parse(data)?;

    let slot_area = build_area(keep, opened.fek(), &header.file_uuid, header.cipher_id)?;

    // TLV 区与载荷原样搬运。这里刻意不做任何「顺手整理」：
    // 本操作的可验证性质就是「除 slot 区和 MAC 外一字节不变」
    let tlv_end = TLV_AREA_OFFSET.saturating_add(header.tlv_len as usize);
    let tlv_blob = data.get(TLV_AREA_OFFSET..tlv_end).ok_or(Error::Truncated {
        context: "tlv area",
        need: tlv_end,
        got: data.len(),
    })?;
    let fixed = data.get(..FIXED_HEADER_LEN).ok_or(Error::Truncated {
        context: "fixed header",
        need: FIXED_HEADER_LEN,
        got: data.len(),
    })?;
    let payload_start = tlv_end.saturating_add(HEADER_MAC_LEN);
    let payload = data.get(payload_start..).ok_or(Error::Truncated {
        context: "payload",
        need: payload_start,
        got: data.len(),
    })?;

    let mut covered = Writer::with_capacity(header.header_len as usize);
    covered.bytes(fixed).bytes(&slot_area).bytes(tlv_blob);
    let mac_key = opened.fek().derive_header_mac_key(&header.file_uuid);
    let mac = header_mac(&mac_key, covered.as_slice());

    let mut out =
        Writer::with_capacity((header.header_len as usize).saturating_add(payload.len()));
    out.bytes(covered.as_slice()).bytes(&mac).bytes(payload);
    let bytes = out.into_vec();

    debug_assert_eq!(bytes.len(), data.len(), "slot 改写不得改变文件长度");
    Ok(RewriteOutcome {
        bytes,
        opened_slot: opened.slot_index,
        slot_used: keep.len(),
    })
}

/// 构建 slot 区：前 n 个槽包裹同一个 FEK，其余填随机。
///
/// 与 [`crate::slot::build_slot_area`] 的区别仅在于这里接受已有的 `Fek`
/// 引用。逻辑保持一致：新增槽位或改变填充策略时两处都要改。
fn build_area(
    keks: &[Kek],
    fek: &Fek,
    file_uuid: &[u8; 16],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    let mut w = Writer::with_capacity(SLOT_AREA_LEN);
    for (i, kek) in keks.iter().enumerate() {
        let idx = u16::try_from(i)
            .map_err(|_| Error::TooManySlots { got: keks.len(), max: SLOT_COUNT })?;
        let wrap_key = kek.derive_slot_key(file_uuid, idx);
        let wrapped = cipher.encrypt(&wrap_key, &ZERO_NONCE, fek.as_key().as_bytes(), &[])?;
        w.bytes(&wrapped);
    }
    // 空槽必须是随机而非零：填零会让「用了几个密码」直接可见
    let used = keks.len().saturating_mul(SLOT_LEN);
    w.random(SLOT_AREA_LEN.saturating_sub(used));

    let out = w.into_vec();
    debug_assert_eq!(out.len(), SLOT_AREA_LEN, "slot 区必须恰好 384 字节");
    Ok(out)
}

/// slot 区在文件中的字节范围，供调用方做「只有这一段变了」的校验。
#[must_use]
pub const fn slot_area_range() -> (usize, usize) {
    (SLOT_AREA_OFFSET, SLOT_AREA_OFFSET + SLOT_AREA_LEN)
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::indexing_slicing, reason = "测试断言需要")]
mod tests {
    use super::*;
    use crate::crypto::{Argon2Params, SecretKey};
    use crate::file::{EncryptOptions, RandomMaterial, encrypt};

    fn kek(seed: u8) -> Kek {
        // 直接构造 KEK，避开 Argon2 的开销：本模块的逻辑与 KEK 怎么来的无关
        Kek::from_key(SecretKey::from_bytes(core::array::from_fn(|i| {
            (i as u8).wrapping_mul(31) ^ seed
        })))
    }

    fn sample(keks: &[Kek]) -> Vec<u8> {
        let plain: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let opts = EncryptOptions {
            argon2: Argon2Params { m_kib: 8, t: 1, p: 1 },
            ..Default::default()
        };
        encrypt(&plain, keks, &[7u8; 16], &opts, &RandomMaterial::generate())
            .unwrap()
            .bytes
    }

    /// 加了新密码后，新旧密码都要能打开。
    ///
    /// 不这样会怎样：只测新密码可用，就无法发现「重建 slot 区时把原密码
    /// 漏掉了」——而这正是 add 最容易出的错。
    #[test]
    fn add_keeps_both_passwords() {
        let a = kek(0x11);
        let b = kek(0x22);
        let data = sample(&[a.duplicate()]);

        let out = rewrite_slots(&data, &[a.duplicate()], &[a.duplicate(), b.duplicate()]).unwrap();

        assert!(crate::file::open(&out.bytes, &[a]).is_ok(), "原密码必须仍可用");
        assert!(crate::file::open(&out.bytes, &[b]).is_ok(), "新密码必须可用");
        assert_eq!(out.slot_used, 2);
    }

    /// 移除后被移除的密码必须真的打不开。
    ///
    /// 不这样会怎样：如果实现只是「把新集合写进前几个槽」而没有覆盖掉原来
    /// 更靠后的槽，被移除的密码依然能命中，remove 就是假的。
    #[test]
    fn removed_password_no_longer_opens() {
        let a = kek(0x33);
        let b = kek(0x44);
        let data = sample(&[a.duplicate(), b.duplicate()]);
        // 先确认两个都能开，否则后面的断言证明不了任何东西
        assert!(crate::file::open(&data, &[b.duplicate()]).is_ok());

        let out = rewrite_slots(&data, &[a.duplicate()], &[a.duplicate()]).unwrap();

        assert!(crate::file::open(&out.bytes, &[a]).is_ok(), "保留的密码应可用");
        let err = crate::file::open(&out.bytes, &[b]).unwrap_err();
        assert!(
            matches!(err, Error::NoMatchingSlot),
            "被移除的密码必须打不开，实际 {err:?}"
        );
    }

    /// 载荷与 file_uuid 必须逐字节不变——这是「不重写载荷」的可验证形式。
    ///
    /// 不这样会怎样：一旦有人图省事改成「解密再加密」，这条会立刻失败。
    #[test]
    fn payload_and_uuid_untouched() {
        let a = kek(0x55);
        let b = kek(0x66);
        let data = sample(&[a.duplicate()]);
        let out = rewrite_slots(&data, &[a.duplicate()], &[b]).unwrap();

        let h0 = FixedHeader::parse(&data).unwrap();
        let h1 = FixedHeader::parse(&out.bytes).unwrap();
        assert_eq!(h0.file_uuid, h1.file_uuid, "file_uuid 不得改变");
        assert_eq!(h0.base_nonce, h1.base_nonce, "base_nonce 不得改变");
        assert_eq!(data.len(), out.bytes.len(), "文件长度不得改变");

        let (lo, hi) = slot_area_range();
        assert_eq!(data[..lo], out.bytes[..lo], "固定头不得改变");
        let mac_end = h0.header_len as usize;
        let mac_start = mac_end - HEADER_MAC_LEN;
        assert_eq!(data[hi..mac_start], out.bytes[hi..mac_start], "TLV 区不得改变");
        assert_eq!(data[mac_end..], out.bytes[mac_end..], "载荷必须逐字节相同");
        assert_ne!(data[lo..hi], out.bytes[lo..hi], "slot 区应当变了");
        assert_ne!(
            data[mac_start..mac_end],
            out.bytes[mac_start..mac_end],
            "头部 MAC 应当随 slot 区一起变"
        );
    }

    /// 改写后明文仍能正确还原。
    ///
    /// 不这样会怎样：前面的断言只看结构，万一 FEK 包裹错了（例如包裹了
    /// 派生密钥而不是 FEK 本身），open 也可能通过，但解出来是垃圾。
    #[test]
    fn plaintext_still_decrypts() {
        let a = kek(0x77);
        let b = kek(0x88);
        let plain: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let data = sample(&[a.duplicate()]);

        let out = rewrite_slots(&data, &[a], &[b.duplicate()]).unwrap();
        let opened = crate::file::open(&out.bytes, &[b]).unwrap();
        assert_eq!(opened.decrypt_all(&out.bytes).unwrap(), plain);
    }

    /// 空集合必须被拒绝：留下 0 个 slot 等于销毁文件。
    #[test]
    fn empty_keep_is_rejected() {
        let a = kek(0x99);
        let data = sample(&[a.duplicate()]);
        assert!(rewrite_slots(&data, &[a], &[]).is_err(), "keep 为空必须报错");
    }

    /// 超过 8 个必须被拒绝，而不是静默丢掉多余的。
    #[test]
    fn too_many_slots_is_rejected() {
        let a = kek(0xAA);
        let data = sample(&[a.duplicate()]);
        let many: Vec<Kek> = (0..9).map(|i| kek(0xB0 + i)).collect();
        let err = rewrite_slots(&data, &[a], &many).unwrap_err();
        assert!(matches!(err, Error::TooManySlots { got: 9, max: 8 }), "实际 {err:?}");
    }

    /// 拿不到能解开的密码时必须失败，不能产出一个新 slot 区。
    #[test]
    fn wrong_unlock_password_fails() {
        let a = kek(0xCC);
        let wrong = kek(0xDD);
        let data = sample(&[a]);
        let err = rewrite_slots(&data, &[wrong.duplicate()], &[wrong]).unwrap_err();
        assert!(matches!(err, Error::NoMatchingSlot), "实际 {err:?}");
    }

    /// 头部被篡改的文件必须拒绝改写。
    ///
    /// 不这样会怎样：若跳过 MAC 校验直接重建，我们会给篡改后的头部算一个
    /// 新的有效 MAC，把攻击者的修改「洗白」成合法文件。
    #[test]
    fn tampered_header_is_rejected() {
        let a = kek(0xEE);
        let mut data = sample(&[a.duplicate()]);
        // 改动固定头里的 plaintext_size（偏移 64，见 header 布局），
        // 它被 MAC 覆盖，因此必须被检出
        data[64] ^= 0x01;
        let err = rewrite_slots(&data, &[a.duplicate()], &[a]).unwrap_err();
        assert!(
            matches!(err, Error::HeaderMacMismatch | Error::MalformedHeader { .. }),
            "篡改必须被拒绝，实际 {err:?}"
        );
    }

    /// 空槽必须是随机填充，不能是零。
    ///
    /// 不这样会怎样：填零会让「这个文件只配了 1 个密码」从字节上直接可见，
    /// 可否认性当场失效。而这种回归不会让任何功能测试变红。
    #[test]
    fn unused_slots_are_random_not_zero() {
        let a = kek(0x12);
        let data = sample(&[a.duplicate()]);
        let out = rewrite_slots(&data, &[a.duplicate()], &[a]).unwrap();
        let (lo, hi) = slot_area_range();
        let pad = &out.bytes[lo + SLOT_LEN..hi];
        assert_eq!(pad.len(), SLOT_LEN * 7);
        assert!(pad.iter().any(|&b| b != 0), "空槽不得填零");

        // 两次改写的填充应当不同，否则说明填充不是每次新取的随机
        let out2 = rewrite_slots(&data, &[kek(0x12)], &[kek(0x12)]).unwrap();
        assert_ne!(
            &out.bytes[lo + SLOT_LEN..hi],
            &out2.bytes[lo + SLOT_LEN..hi],
            "两次改写的随机填充不应相同"
        );
    }

    /// 改写后的文件长度与 header_len 必须仍然自洽。
    #[test]
    fn header_len_still_consistent() {
        let a = kek(0x34);
        let data = sample(&[a.duplicate()]);
        let out = rewrite_slots(&data, &[a.duplicate()], &[a, kek(0x56)]).unwrap();
        let h = FixedHeader::parse(&out.bytes).unwrap();
        h.validate().unwrap();
        assert!(out.bytes.len() >= h.header_len as usize);
    }
}
