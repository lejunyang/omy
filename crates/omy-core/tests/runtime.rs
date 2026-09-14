//! 原子写入、会话缓存与目录扫描的行为测试。
//!
//! 这些测试关注的是**可观察的外部行为**，而非内部实现：
//! 临时文件是否真的被清理、缓存是否真的避免了重复 KDF、
//! 扫描是否真的只读文件头。

#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
#![allow(clippy::expect_used, clippy::too_many_lines)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use omy_core::crypto::{Argon2Params, Kek, SecretKey};
use omy_core::fsatomic::{
    AtomicWriter, TempPlaintext, cleanup_stale, write_atomic,
};
use omy_core::scan::{ScanOptions, UnlockOutcome, probe_file, scan_dir};
use omy_core::session::{CredentialKind, SessionKeys, kek_from_recovery_words};
use omy_core::{EncryptOptions, RandomMaterial, encrypt};

/// 每个测试用独立的临时目录，避免相互干扰。
fn tmpdir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    // 加入进程 id 与纳秒时间戳，确保并发跑测试时不冲突
    let uniq = format!(
        "omy-test-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    p.push(uniq);
    fs::create_dir_all(&p).expect("create temp dir");
    p
}

/// 用固定弱参数加密一个文件到指定路径，返回明文。
fn make_encrypted(
    dir: &Path,
    name: &str,
    filename: &str,
    plaintext: &[u8],
    passwords: &[&str],
    vault_salt: &[u8; 16],
) -> Vec<u8> {
    let opts = EncryptOptions {
        filename: Some(filename.to_owned()),
        argon2: Argon2Params::TEST_WEAK,
        ..EncryptOptions::default()
    };
    let keks: Vec<Kek> = passwords
        .iter()
        .map(|p| {
            Kek::from_password(p.as_bytes(), vault_salt, Argon2Params::TEST_WEAK)
                .expect("derive kek")
        })
        .collect();
    let f = encrypt(plaintext, &keks, vault_salt, &opts, &RandomMaterial::generate())
        .expect("encrypt");
    write_atomic(&dir.join(name), &f.bytes).expect("write");
    plaintext.to_vec()
}

// ============================================================
// 原子写入
// ============================================================

#[test]
fn atomic_write_produces_exact_content() {
    let d = tmpdir("atomic-exact");
    let target = d.join("out.bin");
    let data = b"hello atomic world";

    write_atomic(&target, data).expect("write_atomic");

    let got = fs::read(&target).expect("read back");
    assert_eq!(got, data, "写入内容必须逐字节一致");
}

#[test]
fn atomic_write_leaves_no_tmp_file() {
    let d = tmpdir("atomic-notmp");
    let target = d.join("out.bin");
    write_atomic(&target, b"x").expect("write");

    let leftovers: Vec<_> = fs::read_dir(&d)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "提交后不应残留 .tmp 文件，实际: {leftovers:?}");
}

#[test]
fn atomic_write_overwrites_existing() {
    let d = tmpdir("atomic-overwrite");
    let target = d.join("out.bin");
    write_atomic(&target, b"first version").expect("first");
    write_atomic(&target, b"second").expect("second");

    let got = fs::read(&target).expect("read");
    assert_eq!(got, b"second", "第二次写入必须完全替换，不能残留旧内容尾部");
}

#[test]
fn aborted_writer_does_not_create_target() {
    let d = tmpdir("atomic-abort");
    let target = d.join("never.bin");

    let mut w = AtomicWriter::create(&target).expect("create");
    w.write_all(b"partial data").expect("write");
    w.abort();

    assert!(!target.exists(), "abort 后目标文件不应存在");
    let tmp = d.join("never.bin.tmp");
    assert!(!tmp.exists(), "abort 后临时文件必须被删除");
}

#[test]
fn dropped_writer_cleans_up_without_commit() {
    let d = tmpdir("atomic-drop");
    let target = d.join("dropped.bin");

    {
        let mut w = AtomicWriter::create(&target).expect("create");
        w.write_all(b"data that never commits").expect("write");
        // 不调用 commit，直接离开作用域
    }

    assert!(!target.exists(), "未提交的写入不应产生目标文件");
    assert!(!d.join("dropped.bin.tmp").exists(), "drop 必须清理临时文件");
}

#[test]
fn streaming_write_matches_single_write() {
    let d = tmpdir("atomic-stream");
    let a = d.join("stream.bin");
    let b = d.join("single.bin");

    let mut w = AtomicWriter::create(&a).expect("create");
    w.write_all(b"chunk-0|").expect("w0");
    w.write_all(b"chunk-1|").expect("w1");
    w.write_all(b"chunk-2").expect("w2");
    assert_eq!(w.written(), 23, "written() 应累计所有写入字节");
    w.commit().expect("commit");

    write_atomic(&b, b"chunk-0|chunk-1|chunk-2").expect("single");

    assert_eq!(
        fs::read(&a).expect("read a"),
        fs::read(&b).expect("read b"),
        "分次写入与一次写入结果必须相同"
    );
}

#[test]
fn atomic_write_creates_missing_parent_dirs() {
    let d = tmpdir("atomic-mkdir");
    let target = d.join("a").join("b").join("c.bin");
    write_atomic(&target, b"nested").expect("should create parents");
    assert_eq!(fs::read(&target).expect("read"), b"nested");
}

#[test]
fn cleanup_stale_removes_tmp_and_progress() {
    let d = tmpdir("cleanup");
    fs::write(d.join("a.omy.tmp"), b"junk").expect("w1");
    fs::write(d.join("b.omy.progress"), b"junk").expect("w2");
    fs::write(d.join("keep.omy"), b"real").expect("w3");

    let n = cleanup_stale(&d).expect("cleanup");
    assert_eq!(n, 2, "应清理 2 个残留文件");
    assert!(d.join("keep.omy").exists(), "正常文件不能被误删");
}

// ============================================================
// 临时明文文件
// ============================================================

#[test]
fn temp_plaintext_removed_on_drop() {
    let d = tmpdir("tempplain");
    let path;
    {
        let t = TempPlaintext::create(&d, "preview.txt", b"secret content").expect("create");
        path = t.path().to_path_buf();
        assert!(path.exists(), "创建后文件应存在");
        assert_eq!(fs::read(&path).expect("read"), b"secret content");
    }
    assert!(!path.exists(), "drop 后临时明文必须被删除");
}

#[test]
fn temp_plaintext_preserves_extension() {
    let d = tmpdir("tempext");
    let t = TempPlaintext::create(&d, "video.mp4", b"data").expect("create");
    assert_eq!(
        t.path().extension().and_then(|e| e.to_str()),
        Some("mp4"),
        "必须保留扩展名，否则外部应用无法正确关联打开方式"
    );
}

#[test]
fn temp_plaintext_dir_gets_no_index_marker() {
    let d = tmpdir("tempmark");
    let _t = TempPlaintext::create(&d, "x.txt", b"data").expect("create");

    // 各平台的标记文件名不同，只要求至少有一个存在
    let markers = [".metadata_never_index", "desktop.ini", ".nomedia"];
    let found = markers.iter().any(|m| d.join(m).exists());
    assert!(found, "临时明文目录必须带不索引标记（旁路 L3/L5）");
}

// ============================================================
// 会话缓存：这组测试直接验证性能主张
// ============================================================

#[test]
fn repeated_unlock_keeps_one_entry() {
    let mut s = SessionKeys::new();
    let salt = [0x11u8; 16];

    for _ in 0..10 {
        s.unlock_password("main", &salt, "hunter2", Argon2Params::TEST_WEAK).expect("unlock");
    }

    // 只断言外部可观察的行为：凭据不会重复堆积。
    //
    // 这里刻意**不**断言 kdf_runs==1。早先的版本那样断言，等于把
    // 「命中缓存就跳过 KDF」这个实现细节锁死，而正是那个跳过让
    // 「同一 label 换一个密码」被静默忽略（见下一条测试）。
    // 扫描性能靠的是调用方在文件循环外解锁一次，不靠这个早退。
    assert_eq!(s.len(), 1, "同一 (salt, label) 只应有一个缓存条目，否则状态栏的凭据计数会虚高");
}

/// 用派生出的 slot key 做 KEK 指纹。
///
/// `Kek` 的 `Debug` 是脱敏的，两个不同的 KEK 打印出来一模一样——直接比
/// Debug 字符串会让下面这条测试永远通过（我第一版探针就踩了这个）。
fn kek_fingerprint(k: &Kek) -> [u8; 32] {
    *k.derive_slot_key(&[0u8; 16], 0).as_bytes()
}

#[test]
fn relocking_same_label_with_new_password_replaces_kek() {
    // 不这样会怎样：`unlock_password` 曾在 (salt, kind, label) 命中时
    // 直接 `return Ok(())`。缓存键里没有密码，所以「命中」只说明这个
    // 名字用过、不说明密码相同——第二个密码于是从未被派生，会话里留着
    // 的还是第一个 KEK，而返回值是 Ok，调用方以为解锁成功了。
    //
    // GUI 的 label 恒为 "main"，这个 bug 每天都会撞上：先打错一次密码
    // 再输对的，界面会一直说打不开；改完密码后自动装入新密码也装不进去。
    let salt = [0x77u8; 16];
    let p = Argon2Params::TEST_WEAK;

    let mut s = SessionKeys::new();
    s.unlock_password("main", &salt, "wrong-password", p).expect("first");
    let first = kek_fingerprint(&s.scannable_for(&salt).first().expect("cred").kek);

    s.unlock_password("main", &salt, "correct-password", p).expect("second");
    let second = kek_fingerprint(&s.scannable_for(&salt).first().expect("cred").kek);

    assert_eq!(s.len(), 1, "覆盖而不是新增：同名凭据只该有一条");
    assert_ne!(first, second, "换密码后会话里必须是新 KEK，否则第二次解锁被静默忽略");

    // 还要是**正确**的那个 KEK，不能只是「变了」
    let mut fresh = SessionKeys::new();
    fresh.unlock_password("main", &salt, "correct-password", p).expect("fresh");
    let expected = kek_fingerprint(&fresh.scannable_for(&salt).first().expect("cred").kek);
    assert_eq!(second, expected, "覆盖后的 KEK 必须等于用新密码独立派生的结果");
}

#[test]
fn different_labels_are_separate_entries() {
    let mut s = SessionKeys::new();
    let salt = [0x22u8; 16];
    s.unlock_password("real", &salt, "pw-a", Argon2Params::TEST_WEAK).expect("a");
    s.unlock_password("decoy", &salt, "pw-b", Argon2Params::TEST_WEAK).expect("b");

    assert_eq!(s.len(), 2);
    // 两个不同 label 各派生一次：这里的 kdf_runs 反映的是「不同凭据不共享
    // 缓存」，与「同 label 是否跳过 KDF」无关，所以仍然成立
    assert_eq!(s.kdf_runs(), 2);
}

#[test]
fn same_password_different_salt_yields_different_kek() {
    // 这是缓存键必须含 vault_salt 的原因：
    // 若只用密码做键，切换 vault 后会复用错误的 KEK，
    // 表现为「密码明明对却打不开」，极难排查。
    let salt_a = [0xAAu8; 16];
    let salt_b = [0xBBu8; 16];

    let kek_a = Kek::from_password(b"same-password", &salt_a, Argon2Params::TEST_WEAK).expect("a");
    let kek_b = Kek::from_password(b"same-password", &salt_b, Argon2Params::TEST_WEAK).expect("b");

    assert_ne!(
        kek_a.as_key().as_bytes(),
        kek_b.as_key().as_bytes(),
        "相同密码 + 不同 salt 必须得到不同 KEK"
    );

    let mut s = SessionKeys::new();
    s.unlock_password("main", &salt_a, "same-password", Argon2Params::TEST_WEAK).expect("a");
    s.unlock_password("main", &salt_b, "same-password", Argon2Params::TEST_WEAK).expect("b");
    assert_eq!(s.len(), 2, "不同 vault_salt 必须是不同的缓存条目");
    assert_eq!(s.kdf_runs(), 2, "不同 salt 不能复用缓存");
}

#[test]
fn lock_clears_all_credentials() {
    let mut s = SessionKeys::new();
    let salt = [0x33u8; 16];
    s.unlock_password("a", &salt, "pw1", Argon2Params::TEST_WEAK).expect("a");
    s.unlock_password("b", &salt, "pw2", Argon2Params::TEST_WEAK).expect("b");
    assert_eq!(s.len(), 2);

    s.lock();

    assert!(s.is_empty(), "锁定后必须清空所有凭据");
    assert!(s.scannable_for(&salt).is_empty(), "锁定后不应有可用于扫描的凭据");
}

#[test]
fn forget_removes_single_credential() {
    let mut s = SessionKeys::new();
    let salt = [0x44u8; 16];
    s.unlock_password("keep", &salt, "pw1", Argon2Params::TEST_WEAK).expect("a");
    s.unlock_password("drop", &salt, "pw2", Argon2Params::TEST_WEAK).expect("b");

    assert!(s.forget("drop", CredentialKind::Vault, &salt));
    assert_eq!(s.len(), 1);
    assert!(!s.forget("drop", CredentialKind::Vault, &salt), "重复移除应返回 false");
}

#[test]
fn portable_and_recovery_excluded_from_scanning() {
    let mut s = SessionKeys::new();
    let salt = [0x55u8; 16];

    s.unlock_password("vault-pw", &salt, "pw", Argon2Params::TEST_WEAK).expect("vault");
    s.insert_kek(
        "device",
        CredentialKind::Device,
        &salt,
        Kek::from_key(SecretKey::from_bytes([1u8; 32])),
    );
    s.insert_kek(
        "portable",
        CredentialKind::Portable,
        &salt,
        Kek::from_key(SecretKey::from_bytes([2u8; 32])),
    );
    s.insert_kek(
        "recovery",
        CredentialKind::Recovery,
        &salt,
        Kek::from_key(SecretKey::from_bytes([3u8; 32])),
    );

    let scannable = s.scannable_for(&salt);
    assert_eq!(scannable.len(), 2, "只有 vault 与 device 参与扫描");
    assert!(scannable.iter().all(|c| c.kind.scannable()));

    let all = s.all_for(&salt);
    assert_eq!(all.len(), 4, "手动打开单个文件时可用全部 4 种凭据");
}

#[test]
fn auto_lock_triggers_after_idle_timeout() {
    let mut s = SessionKeys::new().with_idle_timeout(Duration::from_millis(50));
    let salt = [0x66u8; 16];
    s.unlock_password("main", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    assert!(!s.should_auto_lock(), "刚解锁不应立即锁定");
    std::thread::sleep(Duration::from_millis(80));

    assert!(s.should_auto_lock(), "超过空闲阈值应触发锁定");
    assert!(s.auto_lock_if_idle(), "auto_lock_if_idle 应返回已锁定");
    assert!(s.is_empty());
}

#[test]
fn touch_postpones_auto_lock() {
    let mut s = SessionKeys::new().with_idle_timeout(Duration::from_millis(100));
    let salt = [0x77u8; 16];
    s.unlock_password("main", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    for _ in 0..3 {
        std::thread::sleep(Duration::from_millis(40));
        s.touch();
    }
    assert!(!s.should_auto_lock(), "持续活动不应触发空闲锁定");
    assert_eq!(s.len(), 1);
}

#[test]
fn empty_session_never_auto_locks() {
    let s = SessionKeys::new().with_idle_timeout(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(10));
    assert!(!s.should_auto_lock(), "空会话没有东西可锁，不应报告需要锁定");
}

#[test]
fn recovery_words_are_normalized() {
    let a = kek_from_recovery_words(&["Abandon", "ABILITY", " able "]).expect("a");
    let b = kek_from_recovery_words(&["abandon", "ability", "able"]).expect("b");
    assert_eq!(
        a.as_key().as_bytes(),
        b.as_key().as_bytes(),
        "大小写与空白差异必须被归一化——用户手抄恢复码时难免不一致"
    );

    let c = kek_from_recovery_words(&["abandon", "ability", "absent"]).expect("c");
    assert_ne!(a.as_key().as_bytes(), c.as_key().as_bytes(), "不同词序列必须得到不同 KEK");
}

// ============================================================
// 扫描
// ============================================================

#[test]
fn scan_finds_and_unlocks_matching_files() {
    let d = tmpdir("scan-basic");
    let salt = [0x7Au8; 16];

    make_encrypted(&d, "a.omy", "report.pdf", b"content-a", &["pw-main"], &salt);
    make_encrypted(&d, "b.omy", "photo.jpg", b"content-b", &["pw-main"], &salt);
    // 属于另一个密码，当前会话打不开
    make_encrypted(&d, "c.omy", "other.txt", b"content-c", &["pw-other"], &salt);
    // 非 omy 文件，应被静默跳过
    fs::write(d.join("readme.txt"), b"just a normal text file, long enough to be examined")
        .expect("write plain");

    let mut s = SessionKeys::new();
    s.unlock_password("main", &salt, "pw-main", Argon2Params::TEST_WEAK).expect("unlock");

    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");

    assert_eq!(r.stats.omy_found, 3, "应识别出 3 个 omy 文件");
    assert_eq!(r.stats.unlocked, 2, "只有 2 个属于当前密码");
    assert_eq!(r.unlocked().len(), 2);

    let names: Vec<String> = r
        .unlocked()
        .iter()
        .filter_map(|h| match &h.unlock {
            UnlockOutcome::Unlocked { filename, .. } => filename.clone(),
            UnlockOutcome::Locked => None,
        })
        .collect();
    assert!(names.contains(&"report.pdf".to_owned()), "应还原原始文件名，实际: {names:?}");
    assert!(names.contains(&"photo.jpg".to_owned()), "应还原原始文件名，实际: {names:?}");
}

#[test]
fn locked_files_are_reported_not_errors() {
    // 打不开 ≠ 出错。文件可能只是属于另一个密码集，
    // UI 必须能区分这两种情况。
    let d = tmpdir("scan-locked");
    let salt = [0x7Bu8; 16];
    make_encrypted(&d, "x.omy", "secret.txt", b"data", &["not-my-password"], &salt);

    let mut s = SessionKeys::new();
    s.unlock_password("mine", &salt, "my-password", Argon2Params::TEST_WEAK).expect("unlock");

    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
    assert_eq!(r.stats.omy_found, 1);
    assert_eq!(r.stats.unlocked, 0);
    assert_eq!(r.stats.malformed, 0, "打不开不算格式损坏");
    assert!(matches!(r.hits[0].unlock, UnlockOutcome::Locked));
}

#[test]
fn scan_with_no_credentials_still_identifies_files() {
    // 未解锁任何密码时，扫描仍应能列出「这里有 N 个加密文件」，
    // 这是引导用户输入密码的前提。
    let d = tmpdir("scan-nocred");
    let salt = [0x7Cu8; 16];
    make_encrypted(&d, "x.omy", "a.txt", b"data", &["pw"], &salt);

    let s = SessionKeys::new();
    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
    assert_eq!(r.stats.omy_found, 1, "无凭据也应识别出 omy 文件");
    assert_eq!(r.stats.unlocked, 0);
}

#[test]
fn multiple_passwords_see_different_file_sets() {
    // 可否认性的实际效果：不同密码看到不同文件集
    let d = tmpdir("scan-multiset");
    let salt = [0x7Du8; 16];

    make_encrypted(&d, "real1.omy", "real1.txt", b"real", &["true-pw"], &salt);
    make_encrypted(&d, "real2.omy", "real2.txt", b"real", &["true-pw"], &salt);
    make_encrypted(&d, "decoy.omy", "decoy.txt", b"decoy", &["decoy-pw"], &salt);

    let mut s1 = SessionKeys::new();
    s1.unlock_password("t", &salt, "true-pw", Argon2Params::TEST_WEAK).expect("u1");
    let r1 = scan_dir(&d, &s1, &ScanOptions::default()).expect("s1");

    let mut s2 = SessionKeys::new();
    s2.unlock_password("d", &salt, "decoy-pw", Argon2Params::TEST_WEAK).expect("u2");
    let r2 = scan_dir(&d, &s2, &ScanOptions::default()).expect("s2");

    assert_eq!(r1.stats.unlocked, 2, "真实密码应看到 2 个文件");
    assert_eq!(r2.stats.unlocked, 1, "诱饵密码应看到 1 个文件");
    assert_eq!(r1.stats.omy_found, r2.stats.omy_found, "两个密码看到的文件总数必须相同");
}

#[test]
fn file_with_multiple_slots_opens_with_any_password() {
    let d = tmpdir("scan-multislot");
    let salt = [0x7Eu8; 16];
    make_encrypted(&d, "shared.omy", "shared.txt", b"team data", &["alpha", "beta"], &salt);

    for pw in ["alpha", "beta"] {
        let mut s = SessionKeys::new();
        s.unlock_password("x", &salt, pw, Argon2Params::TEST_WEAK).expect("unlock");
        let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
        assert_eq!(r.stats.unlocked, 1, "密码 {pw} 应能打开该文件");
    }
}

// ============================================================
// 一个会话同时装多个密码（add_password）
// ============================================================

/// 装入两个不同的密码后，两个文件集必须**同时**可见。
///
/// 不这样会怎样：这正是 `unlock_password` 做不到的事——它按
/// (salt, kind, label) 覆盖，GUI 又把 label 写死成 "main"，于是第二个
/// 密码会把第一个挤掉。表现是「输了第二个密码，第一批文件反而锁上了」，
/// 而两次解锁都返回成功。
#[test]
fn two_passwords_coexist_and_unlock_their_own_files() {
    let d = tmpdir("session-add-two");
    let salt = [0x81u8; 16];
    make_encrypted(&d, "real.omy", "real.txt", b"real", &["true-pw"], &salt);
    make_encrypted(&d, "decoy.omy", "decoy.txt", b"decoy", &["decoy-pw"], &salt);

    let mut s = SessionKeys::new();
    assert!(s.add_password("a", &salt, "true-pw", Argon2Params::TEST_WEAK).expect("a"));
    // 先确认只装一个时确实只解开一个，否则下面的 2 证明不了是第二个密码起了作用
    let mid = scan_dir(&d, &s, &ScanOptions::default()).expect("mid");
    assert_eq!(mid.stats.unlocked, 1, "只装一个密码时应只解开一个");

    assert!(s.add_password("b", &salt, "decoy-pw", Argon2Params::TEST_WEAK).expect("b"));

    assert_eq!(s.len(), 2, "两个不同密码必须并存");
    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
    assert_eq!(r.stats.unlocked, 2, "两个密码应同时生效，各解开自己的文件");
}

/// 同一个密码装两次只算一条，且不论用什么名字。
///
/// 不这样会怎样：状态栏会显示「2 个密码已解锁」而实际只有一个。上一轮
/// 就是这个缺陷（加密时自动装一条、手动解锁又装一条），当时的对策是把
/// label 写死，结果换来了上面那条测试描述的覆盖缺陷。所以这里**故意用
/// 两个不同的名字**——只有按指纹判断才能同时满足两条。
#[test]
fn same_password_added_twice_counts_once_even_with_different_labels() {
    let salt = [0x82u8; 16];
    let mut s = SessionKeys::new();

    assert!(s.add_password("加密时装入", &salt, "pw", Argon2Params::TEST_WEAK).expect("1st"));
    assert!(
        !s.add_password("手动解锁", &salt, "pw", Argon2Params::TEST_WEAK).expect("2nd"),
        "同一个密码再装一次必须返回 false（不是新凭据）"
    );

    assert_eq!(s.len(), 1, "同一个密码不论起几个名字都只该有一条凭据");
    // KDF 仍然跑了两次：判重需要先有 KEK。这条锁住「不要为了省 KDF 去缓存
    // 快速口令摘要」这个危险的优化
    assert_eq!(s.kdf_runs(), 2, "判重必须在派生之后做，不能靠密码的快速哈希");
}

/// 两个**不同**的密码用同一个名字，必须各算一条。
///
/// 不这样会怎样：按 label 做键时后者直接覆盖前者，用户以为两个密码都装上了，
/// 实际第一个已经没了——而界面上没有任何迹象。GUI 恒用 "main" 时这是必然。
#[test]
fn different_passwords_with_same_label_both_survive() {
    let d = tmpdir("session-same-label");
    let salt = [0x83u8; 16];
    make_encrypted(&d, "one.omy", "one.txt", b"1", &["pw-one"], &salt);
    make_encrypted(&d, "two.omy", "two.txt", b"2", &["pw-two"], &salt);

    let mut s = SessionKeys::new();
    s.add_password("main", &salt, "pw-one", Argon2Params::TEST_WEAK).expect("1");
    s.add_password("main", &salt, "pw-two", Argon2Params::TEST_WEAK).expect("2");

    assert_eq!(s.len(), 2, "重名的两个不同密码必须都留下");
    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
    assert_eq!(r.stats.unlocked, 2, "重名不得导致任何一个密码失效");
}

/// 指纹必须绑定 vault：同一密码在不同 salt 下不算重复。
///
/// 不这样会怎样：把两个库的同名密码判成「已经装过了」，第二个库一个文件
/// 也解不开，而表现是「密码明明对却打不开」——与缓存键必须含 vault_salt
/// 是同一类缺陷。
#[test]
fn same_password_in_different_vaults_is_not_a_duplicate() {
    let salt_a = [0x84u8; 16];
    let salt_b = [0x85u8; 16];
    let mut s = SessionKeys::new();

    assert!(s.add_password("main", &salt_a, "pw", Argon2Params::TEST_WEAK).expect("a"));
    assert!(
        s.add_password("main", &salt_b, "pw", Argon2Params::TEST_WEAK).expect("b"),
        "不同 vault 下的同一密码是两条独立凭据"
    );
    assert_eq!(s.len(), 2);
}

/// 指纹本身：同密码同 vault 恒等，换密码或换 vault 就变。
///
/// 不这样会怎样：`add_password` 的全部去重逻辑都建立在这上面。若指纹
/// 实现退化成常量（例如误用了固定 info 却忘了混入 KEK），所有密码都会
/// 被判成重复，第二个密码永远装不进去——而上面那些测试里，
/// `different_passwords_with_same_label_both_survive` 会失败并指向这里。
#[test]
fn kek_fingerprint_is_stable_and_discriminating() {
    let salt_a = [0x86u8; 16];
    let salt_b = [0x87u8; 16];
    let p = Argon2Params::TEST_WEAK;

    let k1 = Kek::from_password(b"pw", &salt_a, p).expect("k1");
    let k2 = Kek::from_password(b"pw", &salt_a, p).expect("k2");
    let other = Kek::from_password(b"other", &salt_a, p).expect("other");
    let cross = Kek::from_password(b"pw", &salt_b, p).expect("cross");

    assert_eq!(k1.fingerprint(&salt_a), k2.fingerprint(&salt_a), "同密码同库必须同指纹");
    assert_ne!(k1.fingerprint(&salt_a), other.fingerprint(&salt_a), "不同密码必须不同指纹");
    assert_ne!(k1.fingerprint(&salt_a), cross.fingerprint(&salt_b), "跨库必须不同指纹");

    // 上面那条「跨库」其实**证明不了 salt 参数起了作用**：k1 与 cross 是用
    // 不同 vault_salt 跑 Argon2 得到的，KEK 本身就不同，哪怕 fingerprint
    // 内部把 salt 写死成常量，两个指纹也照样不同。变异测试抓到过这一点
    // （把 HKDF salt 换成固定串，这条断言全绿）。
    //
    // 要真正锁住「指纹绑定 vault」，必须拿**同一个 KEK** 去问两个不同的
    // vault_salt。不这样会怎样：指纹退化成只认 KEK，而 add_password 的
    // 去重又只在同 salt 内比对，两者叠加后跨库判重的正确性就没有任何
    // 测试覆盖，日后有人简化掉那个 salt 参数不会有红灯
    assert_ne!(
        k1.fingerprint(&salt_a),
        k1.fingerprint(&salt_b),
        "同一个 KEK 在不同 vault_salt 下必须给出不同指纹（salt 参数必须真的参与派生）"
    );

    // 指纹不得等于任何真实子密钥：它会在内存里被传递比较，落到与解密
    // 密钥同一个域里就等于多了一条泄露路径
    assert_ne!(
        k1.fingerprint(&salt_a),
        *k1.derive_slot_key(&[0u8; 16], 0).as_bytes(),
        "指纹与 slot 密钥必须落在不同的 HKDF 域"
    );
    assert_ne!(
        k1.fingerprint(&salt_a),
        *k1.derive_dirname_key(&salt_a, b"omy/v1/dirname").as_bytes(),
        "指纹与目录名密钥必须落在不同的 HKDF 域"
    );
}

/// 锁定后必须连指纹记录一起清空，不能残留「这个密码装过」的痕迹。
#[test]
fn lock_clears_added_passwords_too() {
    let salt = [0x88u8; 16];
    let mut s = SessionKeys::new();
    s.add_password("a", &salt, "pw1", Argon2Params::TEST_WEAK).expect("a");
    s.add_password("b", &salt, "pw2", Argon2Params::TEST_WEAK).expect("b");
    assert_eq!(s.len(), 2);

    s.lock();
    assert!(s.is_empty());

    // 锁定后重新装入同一个密码必须被当成**新**凭据。不这样会怎样：若指纹
    // 残留在别处，重新解锁会返回 false 且不装入任何东西，用户输了正确密码
    // 却什么都打不开
    assert!(
        s.add_password("a", &salt, "pw1", Argon2Params::TEST_WEAK).expect("re-add"),
        "锁定后重新装入必须算新凭据"
    );
    assert_eq!(s.len(), 1);
}

#[test]
fn scan_recurses_into_subdirectories() {
    let d = tmpdir("scan-recurse");
    let salt = [0x7Fu8; 16];
    let sub = d.join("level1").join("level2");
    fs::create_dir_all(&sub).expect("mkdir");

    make_encrypted(&d, "top.omy", "top.txt", b"t", &["pw"], &salt);
    make_encrypted(&sub, "deep.omy", "deep.txt", b"d", &["pw"], &salt);

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("recursive");
    assert_eq!(r.stats.unlocked, 2, "递归扫描应找到子目录中的文件");

    let flat = ScanOptions { recursive: false, ..ScanOptions::default() };
    let r2 = scan_dir(&d, &s, &flat).expect("flat");
    assert_eq!(r2.stats.unlocked, 1, "非递归扫描只应找到顶层文件");
}

#[test]
fn max_depth_limits_recursion() {
    let d = tmpdir("scan-depth");
    let salt = [0x80u8; 16];
    let deep = d.join("a").join("b").join("c");
    fs::create_dir_all(&deep).expect("mkdir");
    make_encrypted(&deep, "deep.omy", "x.txt", b"d", &["pw"], &salt);

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let shallow = ScanOptions { max_depth: 1, ..ScanOptions::default() };
    let r = scan_dir(&d, &s, &shallow).expect("scan");
    assert_eq!(r.stats.omy_found, 0, "深度限制应阻止进入过深的目录");
}

#[test]
fn extension_filter_skips_non_omy_names() {
    let d = tmpdir("scan-extfilter");
    let salt = [0x81u8; 16];
    // 模拟伪装文件：内容是 omy，但扩展名是 .jpg
    make_encrypted(&d, "disguised.jpg", "hidden.txt", b"secret", &["pw"], &salt);
    make_encrypted(&d, "normal.omy", "plain.txt", b"data", &["pw"], &salt);

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let all = scan_dir(&d, &s, &ScanOptions::default()).expect("all");
    assert_eq!(all.stats.omy_found, 2, "默认不按扩展名过滤，应找到伪装文件");

    let filtered = scan_dir(&d, &s, &ScanOptions::omy_only()).expect("filtered");
    assert_eq!(filtered.stats.omy_found, 1, "omy_only 会漏掉伪装文件——这是该选项的已知代价");
}

#[test]
fn probe_returns_none_for_non_omy_file() {
    let d = tmpdir("probe-none");
    let p = d.join("random.bin");
    // 写足够长的随机内容，确保不是「因为太短而跳过」
    let junk = vec![0x5Au8; 2048];
    fs::write(&p, &junk).expect("write");

    let s = SessionKeys::new();
    let r = probe_file(&p, &s).expect("probe");
    assert!(r.is_none(), "非 omy 文件应返回 None 而非错误");
}

#[test]
fn scan_tolerates_corrupted_file_and_continues() {
    let d = tmpdir("scan-corrupt");
    let salt = [0x82u8; 16];
    make_encrypted(&d, "good.omy", "good.txt", b"fine", &["pw"], &salt);

    // 制造一个 magic 正确但头部损坏的文件
    let mut bad = omy_core::MAGIC_FILE.to_vec();
    bad.extend_from_slice(&[0xFFu8; 1024]);
    fs::write(d.join("bad.omy"), &bad).expect("write bad");

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan should not abort");
    assert_eq!(r.stats.unlocked, 1, "损坏文件不应中断整次扫描");
    assert_eq!(r.stats.malformed, 1, "损坏文件应被统计");
}

#[test]
fn scan_reports_plaintext_size_without_reading_payload() {
    // 这个测试验证扫描的核心性质：只读文件头。
    // 用一个远大于 PEEK_SIZE 的文件，若实现读了整个载荷，
    // 也能得到正确结果——所以这里换个角度验证：
    // 头部里声明的 plaintext_size 必须与实际明文长度相符。
    let d = tmpdir("scan-size");
    let salt = [0x83u8; 16];
    let big = vec![0xABu8; 300_000];
    make_encrypted(&d, "big.omy", "big.bin", &big, &["pw"], &salt);

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let r = scan_dir(&d, &s, &ScanOptions::default()).expect("scan");
    let hit = &r.hits[0];
    match &hit.unlock {
        UnlockOutcome::Unlocked { plaintext_size, .. } => {
            assert_eq!(*plaintext_size, 300_000, "应从头部得到正确的明文大小");
        }
        UnlockOutcome::Locked => panic!("应该能解锁"),
    }
    assert!(hit.file_size > 300_000, "密文文件应大于明文");
}

#[test]
fn max_files_caps_examination() {
    let d = tmpdir("scan-maxfiles");
    let salt = [0x84u8; 16];
    for i in 0..5 {
        make_encrypted(&d, &format!("f{i}.omy"), "x.txt", b"data", &["pw"], &salt);
    }

    let mut s = SessionKeys::new();
    s.unlock_password("x", &salt, "pw", Argon2Params::TEST_WEAK).expect("unlock");

    let capped = ScanOptions { max_files: 2, ..ScanOptions::default() };
    let r = scan_dir(&d, &s, &capped).expect("scan");
    assert!(r.stats.files_examined <= 2, "不应超过 max_files 上限");
}

#[test]
fn scan_nonexistent_dir_returns_error() {
    let s = SessionKeys::new();
    let r = scan_dir(Path::new("E:/definitely/does/not/exist/omy-test"), &s, &ScanOptions::default());
    assert!(r.is_err(), "不存在的目录应返回错误");
}
