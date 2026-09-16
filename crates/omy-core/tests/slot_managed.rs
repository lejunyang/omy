//! 槽位可管理模式的端到端测试（规范 §3.5）。
//!
//! 验的是这个模式存在的唯一理由：**精确删掉某个密码而保住恢复码**。
//! 可否认模式做不到这件事——它分不清哪个槽是谁的，只能整体保留或整体
//! 清场。

use omy_core::crypto::{Argon2Params, Kek, SecretKey};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};
use omy_core::keyslot::{SlotPlan, rewrite_slots_managed};
use omy_core::slotdir::{SlotDirectory, SlotEntry, SlotKind};

fn kek(seed: u8) -> Kek {
    Kek::from_key(SecretKey::from_bytes(core::array::from_fn(|i| {
        // 取低 8 位而不是 as u8：语义一样，但不触发截断告警，
        // 也不必为测试单独放宽 lint
        u8::try_from(i & 0xFF).unwrap_or(0).wrapping_mul(31) ^ seed
    })))
}

fn opts_managed(dir: &SlotDirectory) -> EncryptOptions {
    EncryptOptions {
        argon2: Argon2Params { m_kib: 8, t: 1, p: 1 },
        slot_directory: Some(dir.encode()),
        ..EncryptOptions::default()
    }
}

/// 造一个「主密码 + 协作者 + 恢复码」的可管理模式文件。
fn three_key_file() -> (Vec<u8>, Kek, Kek, Kek, SlotDirectory) {
    let owner = kek(1);
    let mate = kek(2);
    let reco = kek(3);

    let mut dir = SlotDirectory::new();
    dir.set(0, SlotEntry::of(SlotKind::Vault)).expect("set 0");
    dir.set(1, SlotEntry::of(SlotKind::Vault)).expect("set 1");
    dir.set(2, SlotEntry::of(SlotKind::Recovery)).expect("set 2");

    let plain: Vec<u8> = (0..5000u32).map(|i| u8::try_from(i % 251).unwrap_or(0)).collect();
    let bytes = encrypt(
        &plain,
        &[owner.duplicate(), mate.duplicate(), reco.duplicate()],
        &[7u8; 16],
        &opts_managed(&dir),
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;

    (bytes, owner, mate, reco, dir)
}

#[test]
fn managed_mode_removes_one_password_and_keeps_the_recovery_code() {
    // 这是整个模式存在的理由。可否认模式下做不到：想清掉协作者只能
    // 整体 remove，恢复码跟着一起没。
    let (bytes, owner, mate, reco, dir) = three_key_file();

    // 前提自证：三把钥匙一开始都能开。否则下面的断言证明不了什么
    for (name, k) in [("主密码", &owner), ("协作者", &mate), ("恢复码", &reco)] {
        assert!(
            omy_core::file::open(&bytes, &[k.duplicate()]).is_ok(),
            "{name}一开始就该能打开"
        );
    }

    // 只删 slot 1 的协作者，其余原样
    let mut after = dir;
    after.set(1, SlotEntry::empty()).expect("清空 slot 1");
    let plans = vec![
        SlotPlan::Keep,  // 0 主密码
        SlotPlan::Clear, // 1 协作者 ← 精确删除
        SlotPlan::Keep,  // 2 恢复码
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
    ];
    let out = rewrite_slots_managed(&bytes, &[owner.duplicate()], &plans, &after).expect("改写");

    assert!(
        omy_core::file::open(&out.bytes, &[owner.duplicate()]).is_ok(),
        "主密码不该受影响"
    );
    assert!(
        omy_core::file::open(&out.bytes, &[reco.duplicate()]).is_ok(),
        "恢复码必须保住——这正是可管理模式的全部意义"
    );
    assert!(
        omy_core::file::open(&out.bytes, &[mate.duplicate()]).is_err(),
        "被删的协作者必须再也打不开"
    );

    // 目录也要跟着更新，否则下次读目录会以为 slot 1 还有人
    let opened = omy_core::file::open(&out.bytes, &[owner]).expect("打开");
    let got = opened.slot_directory().expect("读目录");
    assert_eq!(got.get(1).expect("slot 1").kind, SlotKind::Empty);
    assert_eq!(got.get(2).expect("slot 2").kind, SlotKind::Recovery);
    assert_eq!(got.used(), 2, "应当只剩主密码与恢复码");
}

#[test]
fn managed_mode_reuses_the_hole_left_by_a_removal() {
    // 可管理模式下 add 不必再赌下标：first_free 能找到被删除腾出的空洞。
    //
    // 不这样会怎样：中间的空洞永远用不上，8 个槽位很快就「满」了，
    // 而用户明明只配了两三个密码。
    let (bytes, owner, mate, reco, dir) = three_key_file();

    let mut after = dir;
    after.set(1, SlotEntry::empty()).expect("清空");
    let plans = vec![
        SlotPlan::Keep,
        SlotPlan::Clear,
        SlotPlan::Keep,
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
        SlotPlan::Clear,
    ];
    let removed =
        rewrite_slots_managed(&bytes, &[owner.duplicate()], &plans, &after).expect("删除");

    // 现在往空洞里放一个新密码
    let opened = omy_core::file::open(&removed.bytes, &[owner.duplicate()]).expect("打开");
    let mut dir2 = opened.slot_directory().expect("读目录");
    let free = dir2.first_free().expect("应当有空洞");
    assert_eq!(free, 1, "空洞应当是被删掉的那个下标，而不是末尾");

    let newcomer = kek(4);
    dir2.set(free, SlotEntry::of(SlotKind::Vault)).expect("记入目录");
    let mut plans2 = Vec::new();
    for i in 0..8 {
        plans2.push(if i == free { SlotPlan::Write(newcomer.duplicate()) } else { SlotPlan::Keep });
    }
    let added =
        rewrite_slots_managed(&removed.bytes, &[owner.duplicate()], &plans2, &dir2).expect("添加");

    assert!(
        omy_core::file::open(&added.bytes, &[newcomer]).is_ok(),
        "新密码应当能打开"
    );
    assert!(
        omy_core::file::open(&added.bytes, &[reco]).is_ok(),
        "恢复码不该被 add 顶掉——这正是可否认模式绕不开的那个边界"
    );
    assert!(
        omy_core::file::open(&added.bytes, &[mate]).is_err(),
        "已删除的协作者不该复活"
    );
    assert!(!added.may_have_evicted, "可管理模式不该报告「可能顶掉」");
}

#[test]
fn rewrite_does_not_change_file_length_or_payload() {
    // 改槽位只该动 slot 区、槽位目录密文与头部 MAC。载荷一字节都不能变，
    // 否则增量备份要重传整份文件。
    let (bytes, owner, _mate, _reco, dir) = three_key_file();

    let mut after = dir;
    after.set(1, SlotEntry::empty()).expect("清空");
    let mut plans = vec![SlotPlan::Keep, SlotPlan::Clear];
    plans.resize_with(8, || SlotPlan::Clear);
    let out = rewrite_slots_managed(&bytes, &[owner.duplicate()], &plans, &after).expect("改写");

    assert_eq!(out.bytes.len(), bytes.len(), "文件长度变了");

    // 载荷按 header_len 定位，不硬编偏移
    let h = omy_core::header::FixedHeader::parse(&out.bytes).expect("解析头部");
    let start = h.header_len as usize;
    assert_eq!(
        out.bytes.get(start..),
        bytes.get(start..),
        "载荷被改写了——说明退化成了重新加密"
    );
}

#[test]
fn refuses_to_leave_the_file_unopenable() {
    // 全部 Clear 会写出一个谁也打不开的文件。必须在写盘前拦住：
    // 写完才发现打不开，用户同时失去了文件和访问权。
    let (bytes, owner, _m, _r, _d) = three_key_file();
    let plans: Vec<SlotPlan> = (0..8).map(|_| SlotPlan::Clear).collect();
    assert!(
        rewrite_slots_managed(&bytes, &[owner], &plans, &SlotDirectory::new()).is_err(),
        "一个槽都不留必须报错"
    );
}

#[test]
fn deniable_file_is_rejected_by_the_managed_path() {
    // 可否认模式的文件没有槽位目录可替换。走错路径要明确报错，
    // 而不是写出一个 flag 与 TLV 不一致的文件——那种损坏不会立刻显现。
    let owner = kek(1);
    let plain = b"hello".to_vec();
    let bytes = encrypt(
        &plain,
        &[owner.duplicate()],
        &[7u8; 16],
        &EncryptOptions { argon2: Argon2Params { m_kib: 8, t: 1, p: 1 }, ..Default::default() },
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;

    let mut plans = vec![SlotPlan::Keep];
    plans.resize_with(8, || SlotPlan::Clear);
    assert!(
        rewrite_slots_managed(&bytes, &[owner], &plans, &SlotDirectory::new()).is_err(),
        "对可否认模式的文件应当拒绝"
    );
}

#[test]
fn managed_flag_and_directory_survive_encryption_roundtrip() {
    // 最基础的一条：写进去的目录要能原样读出来，flag 也要置上。
    let (bytes, owner, _m, _r, dir) = three_key_file();
    let opened = omy_core::file::open(&bytes, &[owner]).expect("打开");

    assert!(opened.is_slot_managed(), "SLOT_DIRECTORY flag 没置上");
    assert_eq!(opened.slot_directory().expect("读目录"), dir, "目录内容对不上");
}

#[test]
fn deniable_file_reports_no_directory() {
    // 反证：可否认模式下读目录必须报错，而不是返回一个空目录。
    //
    // 不这样会怎样：返回空目录会让 UI 显示「这个文件一个密码都没有」，
    // 而它明明能打开——用户会以为文件坏了。
    let owner = kek(1);
    let bytes = encrypt(
        b"x",
        &[owner.duplicate()],
        &[7u8; 16],
        &EncryptOptions { argon2: Argon2Params { m_kib: 8, t: 1, p: 1 }, ..Default::default() },
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;

    let opened = omy_core::file::open(&bytes, &[owner]).expect("打开");
    assert!(!opened.is_slot_managed());
    assert!(opened.slot_directory().is_err(), "可否认模式不该给出目录");
}
