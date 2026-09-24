//! Tauri 构建脚本：确保前端产物存在，再生成 context、注入 capability 与图标。
//!
//! # 为什么这里允许 panic
//!
//! crate 级别禁止 `panic!`（见 Cargo.toml 的 lints），因为运行期崩溃会
//! 让用户丢失正在处理的数据。但构建脚本不是运行期代码：cargo 约定
//! 构建失败就是 panic，这是**唯一**能让 `cargo build` 停下来并显示原因的
//! 方式。返回 `Result` 不会让构建失败，只会静默产出一个坏掉的二进制。
//!
//! 所以这里局部豁免，并保证每处 panic 的信息都足够让人知道下一步做什么。
//!
//! # 为什么要在这里管前端
//!
//! `dist/` 是 Vite 的构建产物，不入库（否则每次改前端都会在 diff 里
//! 混进一堆压缩后的 JS）。但 `tauri_build::build()` 要求产物已经存在，
//! 而且 `generate_context!` 会把它们编进二进制。
//!
//! 所以这里做一次检查：产物缺失时自动跑一次前端构建。
//! 效果是 clone 之后直接 `cargo run -p omy-gui` 就能用，
//! 不需要先记得手动 `pnpm build`。
//!
//! # 什么时候会重跑
//!
//! `cargo:rerun-if-changed` 盯着前端源码目录。改了 `.vue` / `.js` / `.css`
//! 之后再 `cargo build`，前端会跟着重新构建。
//! 只改 Rust 时不会触发，省掉几百毫秒。

#![allow(clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let frontend = crate_dir.join("frontend");
    let dist = crate_dir.join("dist");

    // 前端源码变化时重跑。列出具体子路径而不是整个 frontend/，
    // 否则 node_modules 的任何变动都会触发重建
    for sub in [
        "src",
        "public",
        "index.html",
        "vite.config.ts",
        "vitest.config.ts",
        "tsconfig.json",
        "package.json",
    ] {
        println!("cargo:rerun-if-changed={}", frontend.join(sub).display());
    }

    if needs_build(&dist) {
        build_frontend(&frontend, &dist);
    }

    emit_build_stamp();

    tauri_build::build();
}

/// 把「构建时间 + git 短 hash」作为编译期环境变量注入，供设置「关于」页显示。
///
/// # 为什么要有它
///
/// 用户报问题时，我们和他都需要一个能自证「他开的是哪一版」的东西——否则
/// 「改了没生效」永远分不清是没生效还是在跑旧构建（这个坑真发生过）。
/// 界面显示的 hash / 时间与 `git log` 一对就知道。
fn emit_build_stamp() {
    // 构建时间：UTC，秒级 UNIX 时间戳转成可读串。不引 chrono，简单用系统时间。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=OMY_BUILD_UNIX={now}");

    // git 短 hash。拿不到（不是 git 仓库、没装 git）就给 "unknown"，不让构建失败。
    let hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| String::from("unknown"));
    println!("cargo:rustc-env=OMY_BUILD_GIT={hash}");
    // 源码变了要重算 hash：盯 .git/HEAD
    println!("cargo:rerun-if-changed=../../.git/HEAD");
}

/// 产物是否需要重建。
fn needs_build(dist: &Path) -> bool {
    // 只看关键文件是否齐全。更精细的增量判断交给 Vite 自己——
    // 它比我们清楚哪些模块变了
    !dist.join("index.html").exists() || !dist.join("app.js").exists()
}

/// 调用包管理器构建前端。
fn build_frontend(frontend: &Path, dist: &Path) {
    if !frontend.join("package.json").exists() {
        panic!(
            "前端源码缺失：{} 下没有 package.json。\n\
             GUI 的界面由 frontend/ 构建而来，这个目录不能删。",
            frontend.display()
        );
    }

    // 依赖没装时先装。CI 与新机器上都会走到这条路径
    if !frontend.join("node_modules").exists() {
        println!("cargo:warning=正在安装前端依赖（首次构建）…");
        // 用 install 而不是 ci/--frozen-lockfile：三个包管理器的
        // 严格模式开关各不相同，而这里只需要「把依赖装上」
        run(frontend, &["install"], "安装依赖");
    }

    println!("cargo:warning=正在构建前端…");
    run(frontend, &["run", "build"], "构建前端");

    if needs_build(dist) {
        panic!(
            "前端构建跑完了，但 {} 下仍然没有产物。\n\
             检查 vite.config.js 里的 build.outDir 是否指向这个目录。",
            dist.display()
        );
    }
}

/// 执行包管理器命令。
///
/// Windows 上 `pnpm` 是 `pnpm.cmd`，必须经由 cmd 调用，
/// 直接 `Command::new("pnpm")` 会报「找不到程序」。
fn run(dir: &Path, args: &[&str], what: &str) {
    let pm = pick_pm(dir);

    let status = if cfg!(windows) {
        let joined = format!("{} {}", pm, args.join(" "));
        Command::new("cmd").arg("/C").arg(&joined).current_dir(dir).status()
    } else {
        Command::new(pm).args(args).current_dir(dir).status()
    };

    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!(
            "{what}失败（退出码 {:?}）。\n\
             可以进 {} 手动跑 `{pm} run build` 看详细报错。",
            s.code(),
            dir.display()
        ),
        Err(e) => panic!(
            "{what}失败：无法执行 {pm}（{e}）。\n\
             GUI 的界面需要 Node.js 工具链构建。\n\
             装好 Node 20+ 与 pnpm 之后重试，或进 {} 手动构建。",
            dir.display()
        ),
    }
}

/// 选包管理器。
///
/// 优先跟随仓库里已有的 lockfile：混用包管理器会产生两份互相打架的
/// 依赖树。都没有时用 pnpm（本项目的 lockfile 就是它生成的）。
fn pick_pm(dir: &Path) -> &'static str {
    if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("package-lock.json").exists() {
        "npm"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else {
        "pnpm"
    }
}
