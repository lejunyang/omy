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
//! # 但「保住一个槽」不需要先认出它（[`OtherSlots::Carry`]）
//!
//! 上面那段长期被理解成「改密码必然抹掉不认识的槽」，于是出现了一个实测
//! 确认的真缺陷：文件挂着「主密码 + 恢复码」，`key change` 只提供当前与新
//! 密码，恢复码就被当作不认识的槽填成了随机字节。用户改一次密码即永久失去
//! 恢复码，且只会在真正忘记密码那天才发现。
//!
//! 实际上**不知道某个槽属于谁，照样能把它原样搬过去**：
//!
//! ```text
//! 常规重建：        新密码可开 = true，恢复码可开 = false
//! 原样搬运+重算 MAC：新密码可开 = true，恢复码可开 = true，明文正确
//! ```
//!
//! 成立靠三个性质叠加：slot 密文只依赖 `(KEK, file_uuid, slot_index)`、与
//! 相邻槽无关；本模块保留原 FEK，旧槽里包的还是同一个 FEK；头部 MAC 的密钥
//! 同样由 FEK 派生，搬完重算一次即自洽。
//!
//! **「保住某个槽」与「知道某个槽是什么」是两件事，前者不需要后者。**
//!
//! 所以上面那条约束要精确化：调用方仍须提供「本次操作后应当能打开此文件的
//! 密码」，但**不必**穷举文件上原有的全部密码——不提供的那些会被原样保留
//! （`Carry`）或明确作废（`Discard`），由调用方选择。
//!
//! 代价是保留只能**无差别**：想清掉某个特定密码，本模块做不到，只能整体
//! `Discard` 或换 FEK 重新加密。UI 必须讲清这点，否则用户会以为「改了密码
//! 别人就进不来了」。
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
    /// 原样搬运过来的、来历不明的槽数量。
    ///
    /// 这个数字**不代表「还有几个密码」**：它把真实的其它密码和随机填充
    /// 算在了一起，因为两者在字节层面本就不可区分（可否认性）。
    /// 所以 UI 不能拿它说「保留了 N 个密码」——那是它并不知道的事。
    ///
    /// 它唯一的正当用途是端到端验证：断言「搬运策略下这个数是 7，
    /// 清场策略下是 0」，从而证明两条路径真的走了不同实现。
    pub carried_opaque: usize,
    /// 本次操作是否可能顶掉了原先占着这些下标的槽。
    ///
    /// `keep` 里的 KEK 占据 slot 0..n，搬运只能从 n 开始。所以当 `keep`
    /// 比文件原有的密码数更多时（典型是 `add`），原本在 slot n 上的槽会被
    /// 新密码盖掉——**而我们无法知道那里原来是真密码还是随机填充**。
    ///
    /// 为 true 时调用方应提醒用户：这次新增可能作废了此文件上的某个
    /// 其它密码（如恢复码）。不能断言「一定作废了」，那同样是我们不知道
    /// 的事；只能如实说「可能」。
    ///
    /// 判据是 `keep.len() > 1`：只保留一个密码时（`change` / `remove`）
    /// 不存在增长，slot 0 本来就会被重写。
    pub may_have_evicted: bool,
}

/// 未被 `keep` 覆盖的那些槽怎么处理。
///
/// # 为什么要做成显式选择，而不是默认某一种
///
/// 两种处理方式的后果**都不可逆**，且方向相反：一个会永久丢失用户可能
/// 依赖的恢复码，另一个会让用户以为「删掉的密码」其实还留着。调用方必须
/// 明确表态，不能靠一个容易漏看的布尔默认值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherSlots {
    /// 原样搬运：把不属于 `keep` 的槽逐字节复制过去。
    ///
    /// 用于 `add` / `change`——用户只想动自己这一个密码，不该殃及
    /// 这个文件上的恢复码或别人的密码。
    ///
    /// # 为什么不需要先认出那些槽
    ///
    /// 每个 slot 的密文由 `(KEK, file_uuid, slot_index)` 唯一确定，
    /// 与相邻槽无关；而本函数保留原 FEK，所以旧槽里包的还是同一个 FEK。
    /// 头部 MAC 的密钥同样由 FEK 派生，搬完重算一次即自洽。
    ///
    /// **「保住某个槽」与「知道某个槽是什么」是两件事，前者不需要后者。**
    ///
    /// 代价是保留只能**无差别**：当初给同事配的临时密码也会一起留下。
    /// 想清掉特定密码，本模式做不到（格式上分不清谁是谁），只能整体
    /// [`OtherSlots::Discard`] 或换 FEK 重新加密。
    ///
    /// # 边界：`keep` 增长时仍会顶掉靠前的槽
    ///
    /// `keep` 占据 slot 0..n，搬运只能从 n 开始。所以 `add` 一个新密码时，
    /// 原本在 slot n 上的槽会被新密码盖掉。实测：slot0=主密码、
    /// slot1=恢复码的文件，`add` 一个新密码后恢复码没了。
    ///
    /// 这个边界**绕不开**：包裹密钥绑定 `slot_index`，新密码必须写在它
    /// 自己的下标上；而我们又分不清哪个下标是空的，没有「找个真空位」
    /// 这种操作可用。调用方应根据 [`RewriteOutcome::may_have_evicted`]
    /// 提醒用户。
    Carry,
    /// 全部丢弃，填随机字节——即「只留下 `keep` 里这些」。
    ///
    /// 用于 `remove`。UI 文案必须点明这会作废**包括恢复码在内**的其它
    /// 全部密码，否则用户以为只是删掉了某个协作者。
    Discard,
}

/// 用一组新的 KEK 重建 slot 区，保留原 FEK 与全部载荷。
///
/// `unlock` 用于解开现有文件（只需其中一个能命中）；`keep` 是改写后应当能打开
/// 该文件的 KEK，按顺序占用 slot 0..n。其余槽按 `others` 处理。
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
pub fn rewrite_slots(
    data: &[u8],
    unlock: &[Kek],
    keep: &[Kek],
    others: OtherSlots,
) -> Result<RewriteOutcome> {
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

    let old_area = data.get(SLOT_AREA_OFFSET..SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN))
        .ok_or(Error::Truncated {
            context: "key slot area",
            need: SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN),
            got: data.len(),
        })?;
    let (slot_area, carried_opaque) = build_area(
        keep,
        opened.fek(),
        &header.file_uuid,
        header.cipher_id,
        old_area,
        others,
    )?;

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
        carried_opaque,
        // keep 占 slot 0..n，搬运从 n 起。n > 1 就意味着 slot 1..n 这几个
        // 下标被新写入覆盖了，而那里原来是什么我们不知道
        may_have_evicted: others == OtherSlots::Carry && keep.len() > 1,
    })
}

/// 构建 slot 区：前 n 个槽包裹同一个 FEK，其余按 `others` 处理。
///
/// 返回 `(slot 区字节, 原样搬运的槽数)`。
///
/// 与 [`crate::slot::build_slot_area`] 的区别：那个用于**新建**文件，没有
/// 「旧槽」可搬，所以只有填随机一种行为。逻辑重叠的部分（前 n 槽的包裹方式、
/// 空槽必须随机）两处都要改。
fn build_area(
    keks: &[Kek],
    fek: &Fek,
    file_uuid: &[u8; 16],
    cipher: CipherId,
    old_area: &[u8],
    others: OtherSlots,
) -> Result<(Vec<u8>, usize)> {
    let mut w = Writer::with_capacity(SLOT_AREA_LEN);
    for (i, kek) in keks.iter().enumerate() {
        let idx = u16::try_from(i)
            .map_err(|_| Error::TooManySlots { got: keks.len(), max: SLOT_COUNT })?;
        let wrap_key = kek.derive_slot_key(file_uuid, idx);
        let wrapped = cipher.encrypt(&wrap_key, &ZERO_NONCE, fek.as_key().as_bytes(), &[])?;
        w.bytes(&wrapped);
    }

    let mut carried = 0usize;
    match others {
        // 原样搬运：逐槽从旧区复制。
        //
        // **下标必须对齐**：slot 的包裹密钥含 slot_index，搬到别的下标上
        // 就再也解不开了。所以这里按 i 对 i 复制，绝不能「压缩到前面」——
        // 那会让被搬运的密码静默失效，而文件看起来完全正常。
        //
        // # 一个实测暴露的边界：keep 增长时会踩掉靠前的旧槽
        //
        // keep 里的 KEK 占据 slot 0..n，搬运只能从 n 开始。所以当 keep 比
        // 上次多一个时，原本在 slot n 上的那个「不认识的槽」会被新密码盖掉。
        //
        // 实测：文件是 slot0=主密码、slot1=恢复码，再 add 一个新密码
        // （keep=[主, 新]）→ 新密码写进 slot1，恢复码没了。
        //
        // 这不是可以靠「换个下标写」绕开的：包裹密钥绑定 slot_index，
        // 新密码必须写在它自己那个下标上；而我们又分不清 slot1 上原来是
        // 真密码还是随机填充（可否认性），没有「找个真空位」这种操作。
        //
        // 所以这里如实统计**实际保住了几个**，由调用方决定要不要提醒用户。
        // 掩盖它才是错的：用户以为 add 是纯增量，而它可能悄悄顶掉一个槽。
        OtherSlots::Carry => {
            for i in keks.len()..SLOT_COUNT {
                let start = i.saturating_mul(SLOT_LEN);
                let end = start.saturating_add(SLOT_LEN);
                match old_area.get(start..end) {
                    Some(slot) => {
                        w.bytes(slot);
                        carried = carried.saturating_add(1);
                    }
                    // 旧区短于预期（理论上不可能，调用方已按 SLOT_AREA_LEN
                    // 取过切片）。退回随机而不是报错：宁可多丢一个来历不明
                    // 的槽，也不要让整个改密码操作失败
                    None => {
                        w.random(SLOT_LEN);
                    }
                }
            }
        }
        // 空槽必须是随机而非零：填零会让「用了几个密码」直接可见
        OtherSlots::Discard => {
            let used = keks.len().saturating_mul(SLOT_LEN);
            w.random(SLOT_AREA_LEN.saturating_sub(used));
        }
    }

    let out = w.into_vec();
    debug_assert_eq!(out.len(), SLOT_AREA_LEN, "slot 区必须恰好 384 字节");
    Ok((out, carried))
}

/// 可管理模式下，一个槽位要怎么处理。
///
/// 不 derive Clone：`Kek` 刻意没有 Clone（密钥不该被随手复制），调用方
/// 按引用传这个切片即可。
#[derive(Debug)]
pub enum SlotPlan {
    /// 保留这个槽位的原有字节。
    ///
    /// 原样搬运，不需要知道它属于谁——「保住某个槽」与「知道某个槽是
    /// 什么」是两件事，前者不需要后者。
    Keep,
    /// 把这个槽位换成给定的 KEK。
    Write(Kek),
    /// 清空这个槽位（填随机，目录里应相应记成 Empty）。
    Clear,
}

/// 按槽位目录精确改写 slot 区（可管理模式，规范 §3.5）。
///
/// # 与 [`rewrite_slots`] 的区别
///
/// 那个是可否认模式的路径：`keep` 里的钥匙依次占 slot 0..n，其余槽位
/// 只能**无差别**地整体搬运或整体丢弃。它做不到「删掉协作者 B 而保住
/// 恢复码」，因为它根本分不清哪个槽是谁的。
///
/// 这里要求调用方给出**每个下标**的处置方案，所以能精确管理。前提是
/// 文件处于可管理模式、有一份槽位目录告诉调用方谁在哪个下标上。
///
/// # 为什么下标不能重排
///
/// 包裹密钥含 `slot_index`，一个槽搬到别的下标上就再也解不开了。所以
/// `plans[i]` 严格对应 slot i，`Keep` 是原地保留而不是「挪到前面去」。
/// 这也意味着删除之后会留下空洞——那不是缺陷，`SlotDirectory::first_free`
/// 正是为了把空洞重新用起来，这也是可管理模式下 `add` 不必再赌下标的原因。
///
/// # Errors
///
/// - `plans` 长度不是 [`SLOT_COUNT`]
/// - 改写后一个槽位都不剩（会留下永远打不开的文件）
/// - `unlock` 打不开这个文件，或头部 MAC 校验失败
/// - 文件不是可管理模式（没有槽位目录可替换）
pub fn rewrite_slots_managed(
    data: &[u8],
    unlock: &[Kek],
    plans: &[SlotPlan],
    new_directory: &crate::slotdir::SlotDirectory,
) -> Result<RewriteOutcome> {
    if plans.len() != SLOT_COUNT {
        return Err(Error::TooManySlots { got: plans.len(), max: SLOT_COUNT });
    }
    // 一个都不留等于销毁数据。写出去之后才发现打不开，用户同时失去了
    // 文件和访问权，所以在这里就拦住
    if !plans.iter().any(|p| matches!(p, SlotPlan::Keep | SlotPlan::Write(_))) {
        return Err(Error::MalformedHeader {
            reason: "refusing to leave a file with zero key slots; it could never be opened again",
        });
    }

    // 完整走一遍 open：它会验证头部 MAC。绝不能跳过——若头部已被篡改，
    // 我们会把篡改后的字段连同新 MAC 一起签进去，等于替攻击者背书
    let opened = crate::file::open(data, unlock)?;
    let header = FixedHeader::parse(data)?;
    if !header.has_flag(crate::header::flags::SLOT_DIRECTORY) {
        return Err(Error::MissingTlv { tlv_type: crate::tlv::types::SLOT_DIRECTORY });
    }

    let old_area = data
        .get(SLOT_AREA_OFFSET..SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN))
        .ok_or(Error::Truncated {
            context: "key slot area",
            need: SLOT_AREA_OFFSET.saturating_add(SLOT_AREA_LEN),
            got: data.len(),
        })?;

    let mut w = Writer::with_capacity(SLOT_AREA_LEN);
    let mut carried = 0usize;
    let mut used = 0usize;
    for (i, plan) in plans.iter().enumerate() {
        match plan {
            SlotPlan::Keep => {
                let start = i.saturating_mul(SLOT_LEN);
                let end = start.saturating_add(SLOT_LEN);
                match old_area.get(start..end) {
                    Some(slot) => {
                        w.bytes(slot);
                        carried = carried.saturating_add(1);
                        used = used.saturating_add(1);
                    }
                    // 旧区短于预期（理论上不可能）。退回随机而不是报错，
                    // 理由同可否认路径：宁可多丢一个槽也不要整个操作失败
                    None => {
                        w.random(SLOT_LEN);
                    }
                }
            }
            SlotPlan::Write(kek) => {
                let idx = u16::try_from(i)
                    .map_err(|_| Error::TooManySlots { got: i, max: SLOT_COUNT })?;
                let wrap_key = kek.derive_slot_key(&header.file_uuid, idx);
                let wrapped = header.cipher_id.encrypt(
                    &wrap_key,
                    &ZERO_NONCE,
                    opened.fek().as_key().as_bytes(),
                    &[],
                )?;
                w.bytes(&wrapped);
                used = used.saturating_add(1);
            }
            // 空槽必须是随机而非零。这一条在可管理模式下同样成立：目录是
            // 加密的，但 slot 区不是——填零照样能被数出用了几个槽
            SlotPlan::Clear => {
                w.random(SLOT_LEN);
            }
        }
    }
    let slot_area = w.into_vec();
    debug_assert_eq!(slot_area.len(), SLOT_AREA_LEN, "slot 区必须恰好 384 字节");

    // TLV 区：换掉槽位目录那一条的密文，其余字节原样。
    //
    // 直接替换密文段而不是重建整个 TLV 区：后者会重排条目顺序、改变
    // 长度，而本操作的可验证性质是「文件长度不变」。槽位目录定长 24
    // 字节，同一 AEAD 下密文长度也固定，所以就地替换是安全的
    let tlv_end = TLV_AREA_OFFSET.saturating_add(header.tlv_len as usize);
    let tlv_blob = data.get(TLV_AREA_OFFSET..tlv_end).ok_or(Error::Truncated {
        context: "tlv area",
        need: tlv_end,
        got: data.len(),
    })?;
    let new_value = crate::tlv::encrypt_entry(
        crate::tlv::types::SLOT_DIRECTORY,
        crate::tlv::tlv_flags::CRITICAL,
        &new_directory.encode(),
        opened.fek(),
        header.cipher_id,
    )?
    .value;
    let new_tlv = replace_tlv_value(tlv_blob, crate::tlv::types::SLOT_DIRECTORY, &new_value)?;

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
    covered.bytes(fixed).bytes(&slot_area).bytes(&new_tlv);
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
        slot_used: used,
        carried_opaque: carried,
        // 可管理模式不会「可能顶掉」：调用方明确指定了每个下标怎么处理。
        // 这是这个模式最实在的收益——add 不再需要赌
        may_have_evicted: false,
    })
}

/// 就地替换某个 TLV 条目的 value，其余字节原样保留。
///
/// 要求新旧 value 等长——调用方负责保证（槽位目录定长）。不等长时报错
/// 而不是尽力而为：长度一变，`header_len` 与 `tlv_len` 就都对不上了，
/// 而那两个字段在固定头里，改它们等于重写整个文件。
fn replace_tlv_value(blob: &[u8], want_type: u16, new_value: &[u8]) -> Result<Vec<u8>> {
    let mut out = blob.to_vec();
    let mut at = 0usize;
    while at.saturating_add(crate::tlv::TLV_HEADER_LEN) <= blob.len() {
        let t = u16::from_le_bytes([
            *blob.get(at).unwrap_or(&0),
            *blob.get(at.saturating_add(1)).unwrap_or(&0),
        ]);
        let len = u32::from_le_bytes([
            *blob.get(at.saturating_add(4)).unwrap_or(&0),
            *blob.get(at.saturating_add(5)).unwrap_or(&0),
            *blob.get(at.saturating_add(6)).unwrap_or(&0),
            *blob.get(at.saturating_add(7)).unwrap_or(&0),
        ]) as usize;
        let vstart = at.saturating_add(crate::tlv::TLV_HEADER_LEN);
        let vend = vstart.saturating_add(len);
        if vend > blob.len() {
            break;
        }
        if t == want_type {
            if len != new_value.len() {
                return Err(Error::MalformedTlv {
                    tlv_type: want_type,
                    reason: "slot directory length changed; header_len would no longer match",
                });
            }
            out.get_mut(vstart..vend)
                .ok_or(Error::MalformedTlv {
                    tlv_type: want_type,
                    reason: "tlv value out of range",
                })?
                .copy_from_slice(new_value);
            return Ok(out);
        }
        at = vend;
    }
    Err(Error::MissingTlv { tlv_type: want_type })
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

        let out = rewrite_slots(&data, &[a.duplicate()], &[a.duplicate(), b.duplicate()], OtherSlots::Carry).unwrap();

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

        let out =
            rewrite_slots(&data, &[a.duplicate()], &[a.duplicate()], OtherSlots::Discard).unwrap();

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
        let out = rewrite_slots(&data, &[a.duplicate()], &[b], OtherSlots::Carry).unwrap();

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

        let out = rewrite_slots(&data, &[a], &[b.duplicate()], OtherSlots::Carry).unwrap();
        let opened = crate::file::open(&out.bytes, &[b]).unwrap();
        assert_eq!(opened.decrypt_all(&out.bytes).unwrap(), plain);
    }

    /// 空集合必须被拒绝：留下 0 个 slot 等于销毁文件。
    #[test]
    fn empty_keep_is_rejected() {
        let a = kek(0x99);
        let data = sample(&[a.duplicate()]);
        assert!(
            rewrite_slots(&data, &[a], &[], OtherSlots::Carry).is_err(),
            "keep 为空必须报错"
        );
    }

    /// 超过 8 个必须被拒绝，而不是静默丢掉多余的。
    #[test]
    fn too_many_slots_is_rejected() {
        let a = kek(0xAA);
        let data = sample(&[a.duplicate()]);
        let many: Vec<Kek> = (0..9).map(|i| kek(0xB0 + i)).collect();
        let err = rewrite_slots(&data, &[a], &many, OtherSlots::Carry).unwrap_err();
        assert!(matches!(err, Error::TooManySlots { got: 9, max: 8 }), "实际 {err:?}");
    }

    /// 拿不到能解开的密码时必须失败，不能产出一个新 slot 区。
    #[test]
    fn wrong_unlock_password_fails() {
        let a = kek(0xCC);
        let wrong = kek(0xDD);
        let data = sample(&[a]);
        let err =
            rewrite_slots(&data, &[wrong.duplicate()], &[wrong], OtherSlots::Carry).unwrap_err();
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
        let err =
            rewrite_slots(&data, &[a.duplicate()], &[a], OtherSlots::Carry).unwrap_err();
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
        let out =
            rewrite_slots(&data, &[a.duplicate()], &[a], OtherSlots::Discard).unwrap();
        let (lo, hi) = slot_area_range();
        let pad = &out.bytes[lo + SLOT_LEN..hi];
        assert_eq!(pad.len(), SLOT_LEN * 7);
        assert!(pad.iter().any(|&b| b != 0), "空槽不得填零");

        // 两次改写的填充应当不同，否则说明填充不是每次新取的随机
        let out2 =
            rewrite_slots(&data, &[kek(0x12)], &[kek(0x12)], OtherSlots::Discard).unwrap();
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
        let out =
            rewrite_slots(&data, &[a.duplicate()], &[a, kek(0x56)], OtherSlots::Carry).unwrap();
        let h = FixedHeader::parse(&out.bytes).unwrap();
        h.validate().unwrap();
        assert!(out.bytes.len() >= h.header_len as usize);
    }

    // ========================================================
    // OtherSlots::Carry —— 改密码不再抹掉认不出来的槽
    // ========================================================

    /// 改密码后，那个没参与重建的槽（如恢复码）必须仍然能打开文件。
    ///
    /// 不这样会怎样：这正是实测到的真缺陷——文件挂着「主密码 + 恢复码」，
    /// `key change` 只提供当前与新密码，恢复码被当作不认识的槽填成随机，
    /// 用户改一次密码就永久失去它，且只在真忘密码那天才发现。
    #[test]
    fn carry_preserves_unknown_slot_across_password_change() {
        let main = kek(0x41);
        let reco = kek(0x42); // 模拟恢复码：占 slot 1，改密码时不提供它
        let fresh = kek(0x43);
        let data = sample(&[main.duplicate(), reco.duplicate()]);
        // 基线：不先确认两个都能开，后面的断言证明不了任何东西
        assert!(crate::file::open(&data, &[reco.duplicate()]).is_ok(), "基线：恢复码应可用");

        let out =
            rewrite_slots(&data, &[main.duplicate()], &[fresh.duplicate()], OtherSlots::Carry)
                .unwrap();

        assert!(crate::file::open(&out.bytes, &[fresh]).is_ok(), "新密码必须可用");
        assert!(
            crate::file::open(&out.bytes, &[reco]).is_ok(),
            "没参与重建的槽必须被原样保住——这是本次改动的全部意义"
        );
        assert!(
            crate::file::open(&out.bytes, &[main]).is_err(),
            "旧主密码必须失效，否则 change 是假的"
        );
    }

    /// 被搬运的槽解出来的明文必须正确，不能只是「能打开」。
    ///
    /// 不这样会怎样：搬字节最怕「open 通过但解出垃圾」。若搬运时下标错位，
    /// AEAD 可能碰巧不报错（概率极低但不为零），而内容已经不对了。
    #[test]
    fn carried_slot_decrypts_correct_plaintext() {
        let main = kek(0x44);
        let reco = kek(0x45);
        let fresh = kek(0x46);
        let plain: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let data = sample(&[main.duplicate(), reco.duplicate()]);

        let out = rewrite_slots(&data, &[main], &[fresh], OtherSlots::Carry).unwrap();
        let opened = crate::file::open(&out.bytes, &[reco]).unwrap();
        assert_eq!(opened.decrypt_all(&out.bytes).unwrap(), plain, "搬运后明文必须原样");
    }

    /// 搬运必须**下标对齐**，不能把后面的槽压缩到前面。
    ///
    /// 不这样会怎样：slot 的包裹密钥含 slot_index，搬到别的下标上就再也
    /// 解不开了。而文件看起来完全正常——长度对、MAC 对、新密码能开，
    /// 只有那个被搬错位的密码静默失效。这是最难查的一类缺陷。
    #[test]
    fn carry_keeps_slot_indices_aligned() {
        let main = kek(0x47);
        let far = kek(0x48);
        let fresh = kek(0x49);
        // 让 far 落在 slot 2（中间隔一个空槽），这样「压缩到前面」的错误
        // 实现会把它挪到 slot 1，从而解不开
        let filler = kek(0x4A);
        let data = sample(&[main.duplicate(), filler, far.duplicate()]);
        assert!(crate::file::open(&data, &[far.duplicate()]).is_ok(), "基线");

        let out = rewrite_slots(&data, &[main], &[fresh], OtherSlots::Carry).unwrap();

        assert!(
            crate::file::open(&out.bytes, &[far]).is_ok(),
            "slot 2 的槽必须仍在 slot 2 上，压缩到前面会让它永远解不开"
        );

        // 直接比字节：搬运意味着那几个槽逐字节不变
        let (lo, _) = slot_area_range();
        let s2 = lo + SLOT_LEN * 2;
        assert_eq!(
            &data[s2..s2 + SLOT_LEN],
            &out.bytes[s2..s2 + SLOT_LEN],
            "slot 2 必须逐字节原样"
        );
    }

    /// `Discard` 必须真的清场——这是 remove 赖以成立的性质。
    ///
    /// 不这样会怎样：两种策略若行为相同，`remove` 就成了假操作，用户以为
    /// 作废了别人的密码，实际对方照样能打开。
    #[test]
    fn discard_really_wipes_other_slots() {
        let main = kek(0x4B);
        let other = kek(0x4C);
        let data = sample(&[main.duplicate(), other.duplicate()]);

        let out =
            rewrite_slots(&data, &[main.duplicate()], &[main], OtherSlots::Discard).unwrap();

        let err = crate::file::open(&out.bytes, &[other]).unwrap_err();
        assert!(matches!(err, Error::NoMatchingSlot), "Discard 必须让其它密码失效，实际 {err:?}");
    }

    /// 两种策略必须产出不同的 slot 区，且 `carried_opaque` 如实反映。
    ///
    /// 不这样会怎样：若调用方传了策略却没生效（例如实现里写死一种），
    /// 上面两条测试各自都能过，只有并排比较才能发现它们其实走了同一条路。
    #[test]
    fn carry_and_discard_differ_observably() {
        let a = kek(0x4D);
        let b = kek(0x4E);
        let data = sample(&[a.duplicate(), b]);

        let carried =
            rewrite_slots(&data, &[a.duplicate()], &[a.duplicate()], OtherSlots::Carry).unwrap();
        let wiped =
            rewrite_slots(&data, &[a.duplicate()], &[a], OtherSlots::Discard).unwrap();

        assert_eq!(carried.carried_opaque, 7, "保留了 8-1=7 个来历不明的槽");
        assert_eq!(wiped.carried_opaque, 0, "清场不搬运任何槽");

        let (lo, hi) = slot_area_range();
        assert_ne!(
            &carried.bytes[lo..hi],
            &wiped.bytes[lo..hi],
            "两种策略必须产出不同的 slot 区"
        );
        // 搬运版的第 2..8 槽应与原文件逐字节相同
        assert_eq!(
            &data[lo + SLOT_LEN..hi],
            &carried.bytes[lo + SLOT_LEN..hi],
            "Carry 下未涉及的槽必须逐字节不变"
        );
    }

    /// `Carry` 下文件长度与可否认性仍然成立。
    ///
    /// 不这样会怎样：搬运若多写或少写字节，slot 区就不再是 384，
    /// 而 header_len 是明文可见的——长度一变，「用了几个密码」就泄露了。
    #[test]
    fn carry_preserves_length_and_slot_area_size() {
        let a = kek(0x4F);
        let data = sample(&[a.duplicate()]);
        let out = rewrite_slots(&data, &[a.duplicate()], &[a], OtherSlots::Carry).unwrap();

        assert_eq!(out.bytes.len(), data.len(), "文件长度不得改变");
        let (lo, hi) = slot_area_range();
        assert_eq!(hi - lo, SLOT_LEN * 8, "slot 区必须恒为 8 槽");
        let h = FixedHeader::parse(&out.bytes).unwrap();
        h.validate().unwrap();
    }

    /// `keep` 增长时会顶掉靠前的槽，这个边界必须被如实报告。
    ///
    /// 这是端到端验证抓出来的：文件是 slot0=主密码、slot1=恢复码，
    /// 再 `add` 一个新密码（keep=[主, 新]）→ 新密码写进 slot1，恢复码没了。
    ///
    /// 不这样会怎样：`add` 看起来是纯增量操作，用户完全想不到它会顶掉
    /// 一个自己看不见的槽。若不报告，这就是又一个「静默毁掉恢复码」——
    /// 与本次改动要修的缺陷同类，只是触发路径换成了 add。
    ///
    /// 绕不开的原因见 [`OtherSlots::Carry`] 的文档：包裹密钥绑定下标，
    /// 而我们分不清哪个下标是空的。
    #[test]
    fn carry_reports_possible_eviction_when_keep_grows() {
        let main = kek(0x51);
        let reco = kek(0x52); // 占 slot 1
        let extra = kek(0x53);
        let data = sample(&[main.duplicate(), reco.duplicate()]);

        // add：keep 有两个，会占 slot 0/1，slot1 上的恢复码被顶掉
        let grown = rewrite_slots(
            &data,
            &[main.duplicate()],
            &[main.duplicate(), extra],
            OtherSlots::Carry,
        )
        .unwrap();
        assert!(grown.may_have_evicted, "keep 增长必须如实报告可能顶掉了槽");
        assert!(
            crate::file::open(&grown.bytes, &[reco.duplicate()]).is_err(),
            "确认这个边界真实存在——若哪天能保住了，本断言会提醒来更新文档"
        );

        // change：keep 只有一个，不存在增长，slot 0 本来就会被重写
        let same = rewrite_slots(&data, &[main.duplicate()], &[kek(0x54)], OtherSlots::Carry)
            .unwrap();
        assert!(!same.may_have_evicted, "只保留一个密码时不该报告顶掉");
        assert!(
            crate::file::open(&same.bytes, &[reco]).is_ok(),
            "change 必须保住 slot 1 上的恢复码"
        );
    }

    /// `Discard` 永远不报告顶掉——它本来就是清场，语义上没有「意外」。
    #[test]
    fn discard_never_reports_eviction() {
        let a = kek(0x55);
        let b = kek(0x56);
        let data = sample(&[a.duplicate(), b.duplicate()]);
        let out =
            rewrite_slots(&data, &[a.duplicate()], &[a, b], OtherSlots::Discard).unwrap();
        assert!(
            !out.may_have_evicted,
            "Discard 的语义就是作废其它全部，不存在「意外顶掉」这回事"
        );
    }
}
