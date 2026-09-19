//! 依赖树里**不能**出现 C 工具链。
//!
//! # 为什么值得为这件事写一个测试
//!
//! 「没有 C 依赖」是选 grammers 而不是 TDLib 的核心理由，也是 Android 交叉编译
//! 能跑通的前提。但它是一条**极易被无声破坏**的性质：
//!
//! - 某个依赖的**默认 feature** 里带了 sqlite / openssl，升个小版本就进来了；
//! - 新增依赖时没人会想到去看它的 build-dependencies；
//! - 而破坏之后，**在这台装了 MSVC 的开发机上一切照常编译**，只有别人的机器
//!   或 Android 交叉编译才会失败——那时离引入已经过了很久，很难归因。
//!
//! 已经踩过一次：`grammers-session` 的默认 feature `sqlite-storage` 会拉进
//! `libsql`，而它的 build 依赖是 `bindgen` + `cmake`。加上
//! `default-features = false` 才消掉。这个测试就是为了让下一次同样的事立刻
//! 失败，而不是等到发版。
//!
//! # 为什么用 cargo tree 而不是读 Cargo.lock
//!
//! 需要的恰恰是 cargo 的**解析结果**（feature 合并之后的真实依赖集合），
//! 而不是声明。读 lock 文件会把「声明了但因 feature 未启用而不参与编译」的
//! 条目也算进来，结论会偏严；而 `cargo tree` 给的是这次实际会编译的东西。

use std::process::Command;

/// 这些 crate 一旦出现，就意味着构建需要 C/C++ 工具链。
///
/// `cc` 与 `cmake` **不在**这个名单里：`reqwest` 走的 rustls 经
/// `aws-lc-sys` 本来就带 `cmake`，那是加 Telegram **之前**就存在的状况
/// （`omy-gui` 的依赖树里同样有），不属于本次要守的范围。把它们列进来会让
/// 这个测试从第一天起就是红的，然后被人加 `#[ignore]` —— 那比没有测试更糟。
const FORBIDDEN: &[&str] = &[
    // TDLib 被否决的直接原因
    "libsql",
    "libsql-ffi",
    "libsql-sys",
    "rusqlite",
    "libsqlite3-sys",
    // 交叉编译 Android 的老大难
    "openssl",
    "openssl-sys",
    "native-tls",
    // 需要 libclang，CI 上装它本身就是一件事
    "bindgen",
];

/// omy-remote 的依赖树里不能出现需要 C 工具链的 crate。
///
/// 不这样会怎样：某次升级或新依赖的默认 feature 把 sqlite / openssl 带进来，
/// 而这台开发机装着 MSVC，编译照样通过——直到别人 clone 下来构建失败，或者
/// Android 交叉编译挂掉。届时距离引入已经很久，很难查到是哪一次改动。
#[test]
fn no_c_toolchain_in_dependency_tree() {
    let out = Command::new(env!("CARGO"))
        .args(["tree", "-p", "omy-remote", "--edges", "all", "--prefix", "none"])
        .output()
        .expect("cargo tree 应当可执行");

    assert!(
        out.status.success(),
        "cargo tree 失败: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8_lossy(&out.stdout);

    // 逐行取出 crate 名再精确比较，而不是对整棵树做子串匹配。
    // 不这样会怎样：`openssl` 会命中 `openssl-probe`（纯 Rust，无害），
    // 把一个正常依赖报成违规——假警报会让人给测试加 ignore。
    let mut found: Vec<&str> = Vec::new();
    for line in tree.lines() {
        let name = line.split_whitespace().next().unwrap_or("");
        if FORBIDDEN.contains(&name) {
            found.push(name);
        }
    }

    assert!(
        found.is_empty(),
        "依赖树里出现了需要 C 工具链的 crate: {found:?}\n\
         这会破坏 Android 交叉编译，也让「不需要 C 工具链」的选型理由失效。\n\
         通常的原因是某个依赖的默认 feature——先试 default-features = false。"
    );

    // 顺带自证这个测试**真的读到了**依赖树，而不是在空字符串上做了几次比较。
    // 不这样会怎样：cargo tree 输出格式变了或参数失效时，上面的循环会一条都
    // 匹配不到，测试照样绿——一个永远通过的测试比没有测试更危险。
    assert!(
        tree.lines().any(|l| l.split_whitespace().next() == Some("grammers-client")),
        "依赖树里应当能看到 grammers-client，否则说明这个测试没真正读到树"
    );
}
