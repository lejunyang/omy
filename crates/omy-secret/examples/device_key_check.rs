//! 验证 `HelloProtector`：走完整的 `Protector` 接口，而不是直接调 CNG。
//!
//! 这一步必须单独做。前面的 spike 证明了 CNG 调用本身可行，但那不等于
//! 包了一层之后还对——store/retrieve/delete 的配合、密文落盘路径、
//! NotFound 的判定，每一处都可能出错而 spike 完全看不到。
//!
//! 需要真人交互：创建密钥时弹一次 Hello，每次 retrieve 各弹一次。
//! 所以它不能进 CI，只能手动跑。
//!
//! 跑法：cargo run -p omy-secret --example device_key_check

#![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]

use omy_secret::{HelloProtector, Protector};

fn main() {
    let mut pass = 0u32;
    let mut fail = 0u32;
    let mut check = |name: &str, ok: bool, detail: String| {
        if ok {
            pass += 1;
            println!("  OK  {name}");
        } else {
            fail += 1;
            println!("  FAIL  {name} — {detail}");
        }
    };

    println!("--- 后端可用性 ---");
    let p = match HelloProtector::new("omy-devkey-check") {
        Ok(p) => p,
        Err(e) => {
            println!("  FAIL  创建 HelloProtector — {e}");
            println!("\n这台机器可能没有 TPM，或 CNG 打不开 Platform Crypto Provider。");
            std::process::exit(1);
        }
    };
    check("后端可用", true, String::new());
    check(
        "自称需要用户在场",
        p.requires_user_presence(),
        format!("requires_user_presence = {}", p.requires_user_presence()),
    );

    let id = "probe-slot";
    // 先清一次，避免上次跑残留影响判断
    let _ = p.delete(id);

    println!();
    println!("--- 没存过时要报 NotFound，而不是返回一把新密钥 ---");
    // 这条最要紧：返回新密钥会让上层拿错误的密钥去解旧数据，
    // 得到「数据损坏」这种完全指错方向的错误
    let missing = p.retrieve(id);
    check(
        "未存过时报 NotFound",
        matches!(missing, Err(omy_secret::Error::NotFound)),
        format!("{missing:?}"),
    );

    println!();
    println!("--- 存一把密钥（会弹 Hello：创建期确认）---");
    let key = omy_secret::random_key();
    match p.store(id, &key) {
        Ok(()) => check("store 成功", true, String::new()),
        Err(e) => {
            check("store 成功", false, e.to_string());
            std::process::exit(1);
        }
    }

    println!();
    println!("--- 取回来（会弹 Hello：每次解封都要确认）---");
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
    println!("--- 换一个 id 不该取到同一把 ---");
    // 不这样会怎样：所有槽位共用一把密钥，删掉一个等于删掉全部
    let other = p.retrieve("another-slot");
    check(
        "别的 id 报 NotFound",
        matches!(other, Err(omy_secret::Error::NotFound)),
        format!("{other:?}"),
    );

    println!();
    println!("--- 删除后要真的没了 ---");
    match p.delete(id) {
        Ok(()) => check("delete 成功", true, String::new()),
        Err(e) => check("delete 成功", false, e.to_string()),
    }
    let after = p.retrieve(id);
    check(
        "删除后报 NotFound",
        matches!(after, Err(omy_secret::Error::NotFound)),
        format!("{after:?}"),
    );
    // 幂等：再删一次不该报错
    check("重复删除不报错", p.delete(id).is_ok(), "第二次 delete 失败".into());

    println!();
    println!("通过 {pass} 项，失败 {fail} 项");
    if fail > 0 {
        std::process::exit(1);
    }
}
