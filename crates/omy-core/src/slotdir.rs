//! 槽位目录（`TLV_SLOT_DIRECTORY`，规范 §3.5）。
//!
//! # 它解决什么
//!
//! 可否认模式有一个必然代价：空槽填随机、与真实槽不可区分，于是**连文件
//! 的主人自己也分不清哪个槽是谁的**。后果实测确认过——文件挂着「主密码 +
//! 恢复码」，`key change` 只提供当前与新密码时，恢复码被当作不认识的槽
//! 填成随机字节，用户改一次密码就永久失去了它。
//!
//! 上一轮的缓解是「原样搬运」（[`crate::keyslot::OtherSlots::Carry`]）：
//! 不知道某个槽属于谁，照样能把它原样搬过去。但那只能**无差别**保留，
//! 做不到「精确删除某个协作者而保留恢复码」，`add` 时也仍然会赌一个下标。
//!
//! 想要精确管理就必须放弃一部分可否认性。这个取舍交给用户，在加密时二选一。
//!
//! # 让渡的究竟是什么
//!
//! 比直觉小。目录本身是 ENCRYPTED 的，读它要先有 FEK：
//!
//! | 观察者 | 可否认模式 | 可管理模式 |
//! |---|---|---|
//! | 拿到文件、没有密码 | 槽位一无所知 | 槽位**仍然**一无所知 |
//! | 有一个密码、能打开 | 仍看不出还有几个 | **能看出**共几个、哪些是恢复码 |
//!
//! 即：让渡的只是「对**已经能打开这个文件的人**，无法再隐瞒还有几个密码」。
//!
//! # 为什么定长 24 字节
//!
//! 同一模式内的文件之间长度必须一致，否则「这个文件的槽位目录比那个长」
//! 会泄露密码数量——那正是要藏的东西。所以只存 `label_id`（2 字节），
//! 真正的名字放在应用侧，不进文件。
//!
//! 注意这与「模式标志公开」不冲突：定长是为了**在同一模式内部**防止泄露
//! 密码数量，不是为了让两种模式的 `header_len` 相等——后者已确认是无效
//! 遮掩（`flags` 位本身就是明文可见的）。

use crate::error::{Error, Result};
use crate::header::SLOT_COUNT;
use crate::session::CredentialKind;

/// 单条目录项的字节数：kind(1) + label_id(2)。
pub const ENTRY_LEN: usize = 3;

/// 槽位目录的固定字节数。
pub const DIRECTORY_LEN: usize = SLOT_COUNT * ENTRY_LEN;

/// 一个槽位在目录里记录的类型。
///
/// 编码与规范 §3.5.4 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// 空槽，对应 slot 区的随机填充。
    Empty,
    /// 日常浏览密码。
    Vault,
    /// 设备密钥（生物识别）。
    Device,
    /// 单文件独立密码。
    Portable,
    /// 恢复码。
    Recovery,
    /// 本实现不认识的类型。
    ///
    /// **不得当成空槽**：把未知当空，新版本写的槽会被旧版本覆盖掉。
    /// 保留原始字节，改写时原样写回。
    Unknown(u8),
}

impl SlotKind {
    /// 编码成 1 字节。
    #[must_use]
    pub const fn to_byte(self) -> u8 {
        match self {
            Self::Empty => 0x00,
            Self::Vault => 0x01,
            Self::Device => 0x02,
            Self::Portable => 0x03,
            Self::Recovery => 0x04,
            Self::Unknown(b) => b,
        }
    }

    /// 从 1 字节解码。
    #[must_use]
    pub const fn from_byte(b: u8) -> Self {
        match b {
            0x00 => Self::Empty,
            0x01 => Self::Vault,
            0x02 => Self::Device,
            0x03 => Self::Portable,
            0x04 => Self::Recovery,
            other => Self::Unknown(other),
        }
    }

    /// 这个槽位上有没有挂东西。
    ///
    /// 未知类型算「占用」——它是别的版本写的，不能拿去放新密码。
    #[must_use]
    pub const fn is_occupied(self) -> bool {
        !matches!(self, Self::Empty)
    }

    /// 给人看的类型名。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Vault => "vault",
            Self::Device => "device",
            Self::Portable => "portable",
            Self::Recovery => "recovery",
            Self::Unknown(_) => "unknown",
        }
    }
}

impl From<CredentialKind> for SlotKind {
    fn from(k: CredentialKind) -> Self {
        match k {
            CredentialKind::Vault => Self::Vault,
            CredentialKind::Device => Self::Device,
            CredentialKind::Portable => Self::Portable,
            CredentialKind::Recovery => Self::Recovery,
        }
    }
}

/// 一个槽位的目录项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotEntry {
    /// 槽位类型。
    pub kind: SlotKind,
    /// 应用侧标签表的 id，0 表示无标签。
    ///
    /// 只存 id 不存字符串：名字是变长的，会让目录长度泄露密码数量。
    pub label_id: u16,
}

impl SlotEntry {
    /// 一个空槽。
    #[must_use]
    pub const fn empty() -> Self {
        Self { kind: SlotKind::Empty, label_id: 0 }
    }

    /// 带类型、无标签的槽位。
    #[must_use]
    pub const fn of(kind: SlotKind) -> Self {
        Self { kind, label_id: 0 }
    }
}

impl Default for SlotEntry {
    fn default() -> Self {
        Self::empty()
    }
}

/// 8 个槽位的目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotDirectory {
    entries: [SlotEntry; SLOT_COUNT],
}

impl Default for SlotDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl SlotDirectory {
    /// 全空的目录。
    #[must_use]
    pub const fn new() -> Self {
        Self { entries: [SlotEntry::empty(); SLOT_COUNT] }
    }

    /// 借出全部条目。
    #[must_use]
    pub const fn entries(&self) -> &[SlotEntry; SLOT_COUNT] {
        &self.entries
    }

    /// 取第 `i` 个槽位。越界返回 `None`。
    #[must_use]
    pub fn get(&self, i: usize) -> Option<SlotEntry> {
        self.entries.get(i).copied()
    }

    /// 设置第 `i` 个槽位。
    ///
    /// # Errors
    ///
    /// 下标越界时返回 [`Error::TooManySlots`]。
    pub fn set(&mut self, i: usize, e: SlotEntry) -> Result<()> {
        let slot = self
            .entries
            .get_mut(i)
            .ok_or(Error::TooManySlots { got: i, max: SLOT_COUNT })?;
        *slot = e;
        Ok(())
    }

    /// 找一个真正空闲的槽位。
    ///
    /// 这正是可管理模式的核心价值：可否认模式下没有这种操作可用，`add`
    /// 只能把新密码写在 `keep.len()` 那个下标上，赌它原来是空的。
    #[must_use]
    pub fn first_free(&self) -> Option<usize> {
        self.entries.iter().position(|e| !e.kind.is_occupied())
    }

    /// 占用了几个槽位。
    #[must_use]
    pub fn used(&self) -> usize {
        self.entries.iter().filter(|e| e.kind.is_occupied()).count()
    }

    /// 列出某个类型占用的全部下标。
    #[must_use]
    pub fn indices_of(&self, kind: SlotKind) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == kind)
            .map(|(i, _)| i)
            .collect()
    }

    /// 编码成固定 24 字节。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DIRECTORY_LEN);
        for e in &self.entries {
            out.push(e.kind.to_byte());
            out.extend_from_slice(&e.label_id.to_le_bytes());
        }
        debug_assert_eq!(out.len(), DIRECTORY_LEN, "槽位目录必须恰好 {DIRECTORY_LEN} 字节");
        out
    }

    /// 从 24 字节解码。
    ///
    /// # Errors
    ///
    /// 长度不是 [`DIRECTORY_LEN`] 时返回 [`Error::MalformedTlv`]。长度不对
    /// 就拒绝而不是尽力解析：目录与 slot 区必须严格对应，一个解析出 5 项
    /// 的「目录」比没有目录更危险——用户会拿它去删协作者。
    pub fn decode(raw: &[u8]) -> Result<Self> {
        if raw.len() != DIRECTORY_LEN {
            return Err(Error::MalformedTlv {
                tlv_type: crate::tlv::types::SLOT_DIRECTORY,
                reason: "slot directory must be exactly 24 bytes",
            });
        }
        let mut dir = Self::new();
        for i in 0..SLOT_COUNT {
            let at = i.saturating_mul(ENTRY_LEN);
            let kind = SlotKind::from_byte(*raw.get(at).unwrap_or(&0));
            let lo = *raw.get(at.saturating_add(1)).unwrap_or(&0);
            let hi = *raw.get(at.saturating_add(2)).unwrap_or(&0);
            // 用 get_mut 而不是索引：omy-core 禁止可能 panic 的切片索引。
            // i 恒在 0..SLOT_COUNT 内，所以这个 if 永远成立，但让编译器
            // 看得见这件事比让人来保证更可靠
            if let Some(slot) = dir.entries.get_mut(i) {
                *slot = SlotEntry { kind, label_id: u16::from_le_bytes([lo, hi]) };
            }
        }
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_every_slot() {
        let mut dir = SlotDirectory::new();
        dir.set(0, SlotEntry { kind: SlotKind::Vault, label_id: 7 }).unwrap();
        dir.set(3, SlotEntry { kind: SlotKind::Recovery, label_id: 0 }).unwrap();
        dir.set(7, SlotEntry { kind: SlotKind::Device, label_id: 65535 }).unwrap();

        let raw = dir.encode();
        assert_eq!(raw.len(), DIRECTORY_LEN);
        let back = SlotDirectory::decode(&raw).unwrap();
        // 不这样会怎样：label_id 用错字节序或写串位置，界面上会显示别人的
        // 标签名——而目录本身「看起来是对的」，只有对照才发现
        assert_eq!(back, dir);
    }

    #[test]
    fn length_is_constant_regardless_of_how_many_slots_are_used() {
        // 不这样会怎样：长度随密码数量变化，就能从 header_len 数出这个
        // 文件配了几个密码——那正是这个模式要藏住的东西
        for n in 0..=SLOT_COUNT {
            let mut dir = SlotDirectory::new();
            for i in 0..n {
                dir.set(i, SlotEntry::of(SlotKind::Vault)).unwrap();
            }
            assert_eq!(dir.encode().len(), DIRECTORY_LEN, "{n} 个槽位时长度变了");
        }
    }

    #[test]
    fn unknown_kind_is_preserved_and_counts_as_occupied() {
        // 不这样会怎样：把未知类型当空槽，新版本写的槽会被旧版本当空闲
        // 拿去放新密码，那个槽就此丢失——而且是静默的
        let mut raw = SlotDirectory::new().encode();
        raw[0] = 0x7F; // 一个本实现不认识的类型
        let dir = SlotDirectory::decode(&raw).unwrap();
        assert_eq!(dir.get(0).unwrap().kind, SlotKind::Unknown(0x7F));
        assert!(dir.get(0).unwrap().kind.is_occupied(), "未知类型必须算占用");
        assert_eq!(dir.first_free(), Some(1), "不该把未知槽当成空闲");
        // 原样写回，不被规整成别的值
        assert_eq!(dir.encode()[0], 0x7F, "未知类型的字节被改写了");
    }

    #[test]
    fn first_free_finds_a_real_hole_not_just_the_tail() {
        // 可管理模式的核心价值：add 不再赌下标。
        //
        // 不这样会怎样：若 first_free 只会返回末尾，中间被 remove 腾出来的
        // 槽位永远用不上，8 槽很快就「满」了
        let mut dir = SlotDirectory::new();
        for i in 0..SLOT_COUNT {
            dir.set(i, SlotEntry::of(SlotKind::Vault)).unwrap();
        }
        assert_eq!(dir.first_free(), None, "全满时必须返回 None");

        dir.set(2, SlotEntry::empty()).unwrap();
        assert_eq!(dir.first_free(), Some(2), "中间的空洞要能被找到");
    }

    #[test]
    fn rejects_wrong_length() {
        // 不这样会怎样：一个解析出 5 项的「目录」比没有目录更危险——
        // 用户会拿它去删协作者，而它与 slot 区根本对不上
        assert!(SlotDirectory::decode(&[0u8; DIRECTORY_LEN - 1]).is_err());
        assert!(SlotDirectory::decode(&[0u8; DIRECTORY_LEN + 1]).is_err());
        assert!(SlotDirectory::decode(&[]).is_err());
    }

    #[test]
    fn credential_kind_maps_onto_slot_kind() {
        // 两处枚举必须对得上。不这样会怎样：会话里是恢复码、目录里记成
        // 日常密码，用户按目录删「日常密码」时把恢复码删了
        assert_eq!(SlotKind::from(CredentialKind::Vault), SlotKind::Vault);
        assert_eq!(SlotKind::from(CredentialKind::Device), SlotKind::Device);
        assert_eq!(SlotKind::from(CredentialKind::Portable), SlotKind::Portable);
        assert_eq!(SlotKind::from(CredentialKind::Recovery), SlotKind::Recovery);
    }

    #[test]
    fn byte_encoding_matches_the_spec() {
        // 编码是格式的一部分，写错了就是另一种格式。规范 §3.5.4 的表
        assert_eq!(SlotKind::Empty.to_byte(), 0x00);
        assert_eq!(SlotKind::Vault.to_byte(), 0x01);
        assert_eq!(SlotKind::Device.to_byte(), 0x02);
        assert_eq!(SlotKind::Portable.to_byte(), 0x03);
        assert_eq!(SlotKind::Recovery.to_byte(), 0x04);
    }

    #[test]
    fn indices_and_used_count_agree() {
        let mut dir = SlotDirectory::new();
        dir.set(0, SlotEntry::of(SlotKind::Vault)).unwrap();
        dir.set(1, SlotEntry::of(SlotKind::Recovery)).unwrap();
        dir.set(5, SlotEntry::of(SlotKind::Vault)).unwrap();

        assert_eq!(dir.used(), 3);
        assert_eq!(dir.indices_of(SlotKind::Vault), vec![0, 5]);
        assert_eq!(dir.indices_of(SlotKind::Recovery), vec![1]);
        assert!(dir.indices_of(SlotKind::Device).is_empty());
    }
}
