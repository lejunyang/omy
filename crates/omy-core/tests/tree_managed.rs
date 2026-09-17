//! 树形目录的槽位可管理模式。
//!
//! 与单文件的分工：`slot_managed.rs` 验的是文件头里的槽位目录，
//! 这个验的是边车（`.omy-keys`）里的。两者格式不同——边车的包裹密钥
//! 与槽位下标无关，而文件的绑定 `slot_index`。

use omy_core::crypto::{Argon2Params, CipherId, Kek};
use omy_core::slotdir::{SlotEntry, SlotKind};
use omy_core::file::EncryptOptions;
use omy_core::tree::encrypt_tree;

const SALT: [u8; 16] = [7u8; 16];

fn kek(pw: &str) -> Kek {
    Kek::from_password(pw.as_bytes(), &SALT, Argon2Params::TEST_WEAK).expect("派生 KEK")
}

fn tmp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("omy-tree-managed-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).expect("建临时目录");
    p
}

/// 造一棵两层的小树。
fn make_src(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("子目录")).expect("建子目录");
    std::fs::write(root.join("甲.txt"), b"alpha").expect("写甲");
    std::fs::write(root.join("子目录").join("乙.txt"), b"beta").expect("写乙");
}

fn managed_dir(kinds: &[SlotKind]) -> Vec<u8> {
    let mut d = omy_core::slotdir::SlotDirectory::new();
    for (i, k) in kinds.iter().enumerate() {
        d.set(i, SlotEntry::of(*k)).expect("set");
    }
    d.encode()
}

fn opts(slot_dir: Option<Vec<u8>>) -> EncryptOptions {
    EncryptOptions {
        argon2: Argon2Params::TEST_WEAK,
        slot_directory: slot_dir,
        ..EncryptOptions::default()
    }
}

#[test]
fn tree_managed_mode_lists_slot_kinds() {
    // 最基础的一条：加密时选可管理模式，之后能列出每个槽是什么。
    //
    // 不这样会怎样：树形一直只能显示「查不出来」，而同一个界面上
    // 单文件能列出清单——同一个功能在两种对象上表现不一致。
    let work = tmp("list");
    let src = work.join("机密项目");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let mate = kek("mate");
    let dir = managed_dir(&[SlotKind::Vault, SlotKind::Vault]);
    encrypt_tree(
        &src,
        &out,
        &[owner.duplicate(), mate.duplicate()],
        &SALT,
        &opts(Some(dir)), None)
    .expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("应当有一个密文根目录");

    let info = omy_core::tree::tree_slots(
        &enc_root,
        &[owner.duplicate()],
        &SALT,
        CipherId::ChaCha20Poly1305,
    )
    .expect("读槽位");

    assert!(info.managed, "应当是可管理模式");
    let d = info.directory.expect("应当有目录");
    assert_eq!(d.used(), 2, "两把钥匙");
    assert_eq!(info.current, 0, "owner 在 slot 0");

    // 换一把钥匙读，current 要跟着变——精确删除依赖这个下标，
    // 它错了就会删到别人头上
    let info2 = omy_core::tree::tree_slots(&enc_root, &[mate], &SALT, CipherId::ChaCha20Poly1305)
        .expect("读槽位");
    assert_eq!(info2.current, 1, "mate 在 slot 1");

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn tree_deniable_mode_reports_no_directory() {
    // 反证：可否认模式必须如实说「没有目录」，而不是返回空目录。
    let work = tmp("deniable");
    let src = work.join("普通");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    encrypt_tree(&src, &out, &[owner.duplicate()], &SALT, &opts(None), None).expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    let info = omy_core::tree::tree_slots(&enc_root, &[owner], &SALT, CipherId::ChaCha20Poly1305)
        .expect("读槽位");
    assert!(!info.managed);
    assert!(info.directory.is_none(), "可否认模式不该给出目录");

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn tree_precise_removal_keeps_the_others() {
    // 树形可管理模式存在的理由：精确踢掉一个协作者而保住恢复码。
    let work = tmp("remove");
    let src = work.join("项目");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let mate = kek("mate");
    let reco = kek("reco");
    let dir = managed_dir(&[SlotKind::Vault, SlotKind::Vault, SlotKind::Recovery]);
    encrypt_tree(
        &src,
        &out,
        &[owner.duplicate(), mate.duplicate(), reco.duplicate()],
        &SALT,
        &opts(Some(dir)), None)
    .expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    // 前提自证：三把钥匙一开始都能解开这棵树
    for (name, k) in [("owner", &owner), ("mate", &mate), ("reco", &reco)] {
        assert!(
            omy_core::tree::tree_slots(&enc_root, &[k.duplicate()], &SALT, CipherId::ChaCha20Poly1305)
                .is_ok(),
            "{name} 一开始就该能解开"
        );
    }

    let changed = omy_core::tree::tree_remove_slot(
        &enc_root,
        &[owner.duplicate()],
        1,
        &SALT,
        CipherId::ChaCha20Poly1305,
    )
    .expect("删除 slot 1");
    assert!(changed >= 2, "根目录与子目录的边车都要改，实际只改了 {changed} 份");

    assert!(
        omy_core::tree::tree_slots(&enc_root, &[owner.duplicate()], &SALT, CipherId::ChaCha20Poly1305)
            .is_ok(),
        "主密码不该受影响"
    );
    assert!(
        omy_core::tree::tree_slots(&enc_root, &[reco], &SALT, CipherId::ChaCha20Poly1305).is_ok(),
        "恢复码必须保住——这正是可管理模式的全部意义"
    );
    assert!(
        omy_core::tree::tree_slots(&enc_root, &[mate.duplicate()], &SALT, CipherId::ChaCha20Poly1305)
            .is_err(),
        "被踢掉的协作者必须再也解不开"
    );

    // 目录要同步更新，否则下次读会以为 slot 1 还有人
    let after =
        omy_core::tree::tree_slots(&enc_root, &[owner], &SALT, CipherId::ChaCha20Poly1305)
            .expect("读槽位");
    let d = after.directory.expect("目录");
    assert_eq!(d.get(1).expect("slot 1").kind, SlotKind::Empty);
    assert_eq!(d.get(2).expect("slot 2").kind, SlotKind::Recovery);
    assert_eq!(d.used(), 2);

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn removal_reaches_every_subdirectory() {
    // 漏改任何一个子目录，那个子目录被单独拷走时被删的钥匙仍能解开
    // 它的名字——「删掉了」就只在根目录成立。
    let work = tmp("deep");
    let src = work.join("深");
    std::fs::create_dir_all(src.join("一").join("二")).expect("建深层目录");
    std::fs::write(src.join("一").join("二").join("丙.txt"), b"gamma").expect("写丙");
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let mate = kek("mate");
    let dir = managed_dir(&[SlotKind::Vault, SlotKind::Vault]);
    encrypt_tree(
        &src,
        &out,
        &[owner.duplicate(), mate.duplicate()],
        &SALT,
        &opts(Some(dir)), None)
    .expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    omy_core::tree::tree_remove_slot(
        &enc_root,
        &[owner.duplicate()],
        1,
        &SALT,
        CipherId::ChaCha20Poly1305,
    )
    .expect("删除");

    // 逐个子目录核对：每一份边车都不该再被 mate 解开
    let mut stack = vec![enc_root];
    let mut checked = 0usize;
    while let Some(cur) = stack.pop() {
        if cur.join(".omy-keys").exists() {
            checked += 1;
            assert!(
                omy_core::tree::tree_slots(&cur, &[mate.duplicate()], &SALT, CipherId::ChaCha20Poly1305)
                    .is_err(),
                "{} 的边车还能被已删除的钥匙解开",
                cur.display()
            );
        }
        let Ok(rd) = std::fs::read_dir(&cur) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            }
        }
    }
    assert!(checked >= 3, "深层树至少该有 3 份边车，实际检查了 {checked} 份");

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn refuses_to_remove_the_key_in_use() {
    // 删掉自己正在用的那把，之后就没法继续操作了。必须拦住。
    let work = tmp("self");
    let src = work.join("自");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let mate = kek("mate");
    let dir = managed_dir(&[SlotKind::Vault, SlotKind::Vault]);
    encrypt_tree(
        &src,
        &out,
        &[owner.duplicate(), mate],
        &SALT,
        &opts(Some(dir)), None)
    .expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    assert!(
        omy_core::tree::tree_remove_slot(&enc_root, &[owner], 0, &SALT, CipherId::ChaCha20Poly1305)
            .is_err(),
        "删掉当前这把钥匙必须被拒绝"
    );

    let _ = std::fs::remove_dir_all(&work);
}

#[test]
fn deniable_tree_rejects_precise_removal() {
    // 可否认模式下分不清哪个槽是谁，精确删除无从谈起。要明确报错，
    // 而不是删错一个人。
    let work = tmp("den-rm");
    let src = work.join("普通");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let mate = kek("mate");
    encrypt_tree(&src, &out, &[owner.duplicate(), mate], &SALT, &opts(None), None).expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    assert!(
        omy_core::tree::tree_remove_slot(&enc_root, &[owner], 1, &SALT, CipherId::ChaCha20Poly1305)
            .is_err(),
        "可否认模式应当拒绝精确删除"
    );

    let _ = std::fs::remove_dir_all(&work);
}
#[test]
fn rekey_keeps_the_tree_manageable() {
    // 换密码不该让整棵树静默退回可否认模式。
    //
    // 不这样会怎样：用户只是改了个密码，下次想精确删协作者时发现
    // 「能看清有几把钥匙」这个能力没了，而当初加密时他是特意选的。
    // 单文件的 reencrypt 犯过同样的错。
    //
    // 这条断言原先只在 CLI 脚本里有，core 层没覆盖——变异测试把这个
    // 缺口抓了出来。
    let work = tmp("rekey");
    let src = work.join("换密码");
    make_src(&src);
    let out = work.join("out");
    std::fs::create_dir_all(&out).expect("建输出目录");

    let owner = kek("owner");
    let dir = managed_dir(&[SlotKind::Vault]);
    encrypt_tree(&src, &out, &[owner.duplicate()], &SALT, &opts(Some(dir)), None).expect("加密");

    let enc_root = std::fs::read_dir(&out)
        .expect("读输出")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .expect("密文根目录");

    // 前提自证：换之前确实是可管理模式
    let before =
        omy_core::tree::tree_slots(&enc_root, &[owner.duplicate()], &SALT, CipherId::ChaCha20Poly1305)
            .expect("读槽位");
    assert!(before.managed, "加密时就该是可管理模式");

    let fresh = kek("fresh");
    omy_core::tree::rekey_tree(
        &enc_root,
        &[owner],
        &[fresh.duplicate()],
        &SALT,
        CipherId::ChaCha20Poly1305,
        omy_core::keyslot::OtherSlots::Carry,
    )
    .expect("换密码");

    let after = omy_core::tree::tree_slots(&enc_root, &[fresh], &SALT, CipherId::ChaCha20Poly1305)
        .expect("换完之后应当还能读槽位");
    assert!(after.managed, "换密码后退回可否认模式了");
    assert_eq!(
        after.directory.expect("目录").used(),
        1,
        "换完只剩新密码这一把"
    );

    let _ = std::fs::remove_dir_all(&work);
}