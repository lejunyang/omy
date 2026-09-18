//! 设备密钥不能成为唯一凭据。
//!
//! 这是防数据丢失的拦截，必须有测试盯着：设备密钥绑在这台机器的硬件上，
//! 换机器、重装系统、清除 TPM、重置 Hello 之后就永久解不开，而且**无法
//! 恢复**。那时若没有密码或恢复码，数据彻底拿不回来。
//!
//! 而用户很容易走到这一步：按指纹进来、看着槽位清单觉得「密码反正记不住，
//! 删了吧」。删的那一刻什么都正常，代价几个月后换电脑时才显现。

use omy_core::crypto::{Argon2Params, Kek, SecretKey};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt};
use omy_core::keyslot::{SlotPlan, rewrite_slots_managed};
use omy_core::slotdir::{SlotDirectory, SlotEntry, SlotKind};

fn kek(seed: u8) -> Kek {
    Kek::from_key(SecretKey::from_bytes(core::array::from_fn(|i| {
        u8::try_from(i & 0xFF).unwrap_or(0).wrapping_mul(31) ^ seed
    })))
}

/// 造一个「密码 + 设备密钥」的可管理模式文件。
fn file_with_device() -> (Vec<u8>, Kek, Kek, SlotDirectory) {
    let pw = kek(1);
    let dev = kek(2);
    let mut dir = SlotDirectory::new();
    dir.set(0, SlotEntry::of(SlotKind::Vault)).expect("set 0");
    dir.set(1, SlotEntry::of(SlotKind::Device)).expect("set 1");

    let bytes = encrypt(
        b"payload",
        &[pw.duplicate(), dev.duplicate()],
        &[7u8; 16],
        &EncryptOptions {
            argon2: Argon2Params { m_kib: 8, t: 1, p: 1 },
            slot_directory: Some(dir.encode()),
            ..EncryptOptions::default()
        },
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;
    (bytes, pw, dev, dir)
}

fn plans_keeping(indices: &[usize]) -> Vec<SlotPlan> {
    (0..omy_core::header::SLOT_COUNT)
        .map(|i| {
            if indices.contains(&i) {
                SlotPlan::Keep
            } else {
                SlotPlan::Clear
            }
        })
        .collect()
}

#[test]
fn refuses_to_leave_only_a_device_key() {
    // 核心断言：删掉密码、只留设备密钥必须被拒绝。
    //
    // 不拦会怎样：用户按指纹进来删掉密码，一切正常；几个月后换电脑，
    // 设备密钥随旧机器的 TPM 一起消失，数据再也打不开。
    let (bytes, pw, _dev, dir) = file_with_device();

    // 前提自证：现在两把钥匙都能开，否则下面的断言证明不了什么
    assert!(
        omy_core::file::open(&bytes, &[pw.duplicate()]).is_ok(),
        "密码一开始就该能开"
    );

    let mut after = dir;
    after.set(0, SlotEntry::empty()).expect("清掉密码那一项");
    let err = rewrite_slots_managed(&bytes, &[pw], &plans_keeping(&[1]), &after);
    assert!(err.is_err(), "只剩设备密钥必须被拒绝");
}

#[test]
fn allows_device_key_alongside_a_password() {
    // 反证：留着密码时放行。
    //
    // 这条不能省——只有它能说明上一条拦的是「只剩设备密钥」，
    // 而不是把设备密钥整个禁掉了
    let (bytes, pw, dev, dir) = file_with_device();
    let out = rewrite_slots_managed(&bytes, &[pw.duplicate()], &plans_keeping(&[0, 1]), &dir)
        .expect("密码 + 设备密钥应当放行");
    assert!(
        omy_core::file::open(&out.bytes, &[pw]).is_ok(),
        "改写后密码还该能开"
    );
    assert!(
        omy_core::file::open(&out.bytes, &[dev]).is_ok(),
        "改写后设备密钥还该能开"
    );
}

#[test]
fn allows_device_key_alongside_a_recovery_code() {
    // 恢复码也算逃生路径：它不绑机器，换电脑之后照样能用。
    //
    // 所以「设备密钥 + 恢复码」是安全的组合，不该被拦
    let pw = kek(1);
    let dev = kek(2);
    let reco = kek(3);
    let mut dir = SlotDirectory::new();
    dir.set(0, SlotEntry::of(SlotKind::Vault)).expect("0");
    dir.set(1, SlotEntry::of(SlotKind::Device)).expect("1");
    dir.set(2, SlotEntry::of(SlotKind::Recovery)).expect("2");

    let bytes = encrypt(
        b"payload",
        &[pw.duplicate(), dev.duplicate(), reco.duplicate()],
        &[7u8; 16],
        &EncryptOptions {
            argon2: Argon2Params { m_kib: 8, t: 1, p: 1 },
            slot_directory: Some(dir.encode()),
            ..EncryptOptions::default()
        },
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;

    // 删掉密码，留设备密钥 + 恢复码
    let mut after = dir;
    after.set(0, SlotEntry::empty()).expect("清掉密码");
    let out = rewrite_slots_managed(&bytes, &[pw], &plans_keeping(&[1, 2]), &after)
        .expect("设备密钥 + 恢复码应当放行");
    assert!(
        omy_core::file::open(&out.bytes, &[reco]).is_ok(),
        "恢复码还该能开"
    );
    assert!(
        omy_core::file::open(&out.bytes, &[dev]).is_ok(),
        "设备密钥还该能开"
    );
}

#[test]
fn still_refuses_to_leave_nothing() {
    // 原有的「一个都不留」拦截不能因为加了新检查而失效。
    //
    // 两条检查顺序相邻，改动时容易把其中一条的作用范围写错
    let (bytes, pw, _dev, _dir) = file_with_device();
    let all_clear: Vec<SlotPlan> = (0..omy_core::header::SLOT_COUNT)
        .map(|_| SlotPlan::Clear)
        .collect();
    assert!(
        rewrite_slots_managed(&bytes, &[pw], &all_clear, &SlotDirectory::new()).is_err(),
        "一个槽都不留必须仍然被拒"
    );
}

#[test]
fn two_device_keys_are_still_refused() {
    // 两把设备密钥不比一把安全：它们绑的是同一台机器的同一个 TPM。
    //
    // 不这样会怎样：用户在同一台机器上挂两次，以为有了冗余，
    // 而换电脑时两把一起失效
    let pw = kek(1);
    let d1 = kek(2);
    let d2 = kek(3);
    let mut dir = SlotDirectory::new();
    dir.set(0, SlotEntry::of(SlotKind::Vault)).expect("0");
    dir.set(1, SlotEntry::of(SlotKind::Device)).expect("1");
    dir.set(2, SlotEntry::of(SlotKind::Device)).expect("2");

    let bytes = encrypt(
        b"payload",
        &[pw.duplicate(), d1, d2],
        &[7u8; 16],
        &EncryptOptions {
            argon2: Argon2Params { m_kib: 8, t: 1, p: 1 },
            slot_directory: Some(dir.encode()),
            ..EncryptOptions::default()
        },
        &RandomMaterial::generate(),
    )
    .expect("加密")
    .bytes;

    let mut after = dir;
    after.set(0, SlotEntry::empty()).expect("清掉密码");
    assert!(
        rewrite_slots_managed(&bytes, &[pw], &plans_keeping(&[1, 2]), &after).is_err(),
        "只剩两把设备密钥仍必须被拒"
    );
}
#[test]
fn checks_plans_not_just_the_directory() {
    // 存活的第三个变异暴露了测试数据的问题：上面几条里旧目录已经清空了
    // 密码项，所以「按 plans 过滤」和「不过滤」结果相同，变异看不出差别。
    //
    // 这条专门区分两者：目录**仍然记着**密码在 slot 0，但 plans 要清掉它。
    // 只看目录会以为「还有密码」而放行，实际写出去之后只剩设备密钥。
    //
    // 现实里这正是最容易发生的形态——调用方忘了同步更新目录。
    let (bytes, pw, _dev, dir) = file_with_device();

    // 注意：目录原样传（slot 0 还记着 Vault），但 plans 清掉 slot 0
    let err = rewrite_slots_managed(&bytes, &[pw], &plans_keeping(&[1]), &dir);
    assert!(
        err.is_err(),
        "plans 清掉了密码槽，即使目录还记着它也必须拒绝"
    );
}