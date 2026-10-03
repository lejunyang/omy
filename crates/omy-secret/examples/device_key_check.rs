//! 真机探针：验证「设备密钥」保管器在当前平台上的真实行为。
//!
//! 走完整的 `Protector` 接口，而不是直接调底层 API。需要真人交互的步骤
//! （Windows Hello 确认 / Touch ID 扫描）会弹系统对话框，所以它不进 CI。
//!
//! 跑法：cargo run -p omy-secret --example device_key_check
//!
//! 在不支持或未配置的机器上，它会如实打印每一条路径返回什么错误，
//! 而不是 panic——这本身就是要验证的行为之一。

#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use omy_secret::{Protector};

fn main() {
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut skip = 0u32;
    let mut check = |name: &str, ok: bool, detail: String| {
        if ok {
            pass += 1;
            println!("  OK  {name}");
        } else {
            fail += 1;
            println!("  FAIL  {name} — {detail}");
        }
    };
    let mut note = |name: &str, detail: String| {
        skip += 1;
        println!("  --  {name}（跳过：{detail}）");
    };

    println!("--- 后端可用性 ---");
    let p = match omy_secret::device_protector("omy-devkey-check") {
        Ok(p) => p,
        Err(e) => {
            println!("  当前环境用不了设备密钥：{e}");
            println!();
            println!("这本身就是一条要验证的失败路径：");
            println!("  Windows：没有 TPM / 没配 Hello；macOS：没有 Touch ID / 未录入 / 未签名。");
            println!("探针到此结束（{skip} 条跳过）。");
            return;
        }
    };
    check("后端可用", true, String::new());
    check(
        "自称需要用户在场",
        p.requires_user_presence(),
        format!("requires_user_presence = {}", p.requires_user_presence()),
    );
    println!("  后端名：{}", p.name());

    let id = "probe-slot";
    let _ = p.delete(id);

    println!();
    println!("--- 没存过时要报 NotFound，而不是返回一把新密钥 ---");
    let missing = p.retrieve(id);
    check(
        "未存过时报 NotFound",
        matches!(missing, Err(omy_secret::Error::NotFound)),
        format!("{missing:?}"),
    );

    println!();
    println!("--- has() 不应弹窗、且没存过时为 false ---");
    check("没存过时 has() = false", !p.has(id), "".into());

    println!();
    println!("--- 存一把密钥 ---");
    let key = omy_secret::random_key();
    match p.store(id, &key) {
        Ok(()) => check("store 成功", true, String::new()),
        Err(omy_secret::Error::NoBackend(d)) => {
            note("store（未签名/未配对等环境限制）", d);
            println!();
            println!("通过 {pass} 项，失败 {fail} 项，跳过 {skip} 项");
            return;
        }
        Err(e) => {
            check("store 成功", false, e.to_string());
            println!();
            println!("通过 {pass} 项，失败 {fail} 项，跳过 {skip} 项");
            std::process::exit(1);
        }
    }
    check("存过之后 has() = true", p.has(id), "".into());

    println!();
    println!("--- 取回来（会弹系统生物识别确认）---");
    match p.retrieve(id) {
        Ok(got) => {
            check("retrieve 成功", true, String::new());
            check(
                "取回的密钥与存进去的一致",
                got.as_slice() == key.as_slice(),
                "字节不一致".into(),
            );
        }
        Err(e) => check("retrieve 成功", false, e.to_string()),
    }

    println!();
    println!("--- 删除后要真的没了，且重复删除不报错 ---");
    check("delete 成功", p.delete(id).is_ok(), "".into());
    check(
        "删除后报 NotFound",
        matches!(p.retrieve(id), Err(omy_secret::Error::NotFound)),
        "".into(),
    );
    check("重复删除不报错", p.delete(id).is_ok(), "".into());

    println!();
    println!("通过 {pass} 项，失败 {fail} 项，跳过 {skip} 项");
    if fail > 0 {
        std::process::exit(1);
    }
}
