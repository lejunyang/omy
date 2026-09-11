---
title: 从源码构建
---

# 从源码构建

## 前置条件

**Rust 1.85 或更高**——项目用 edition 2024，这是最低要求。

```bash
rustc --version
```

命令行工具只需要 Rust。图形界面还需要 Node.js 与一个包管理器（界面是 Vue 3 + Vite 构建的）。

## 只构建命令行工具

```bash
git clone https://github.com/lejunyang/omy.git
cd omy
cargo build --release -p omy-cli
```

产物在 `target/release/omy`（Windows 上 `omy.exe`）。注意二进制名是 `omy`，不是 `omy-cli`。

## 构建图形界面

```bash
cargo build --release -p omy-gui
```

构建脚本会自动处理前端：产物缺失时装依赖并跑构建，所以全新 clone 之后直接执行上面这条就行。

::: tip 包管理器由 lockfile 决定
`build.rs` 按仓库里已有的 lockfile 选择：`pnpm-lock.yaml` → pnpm，`package-lock.json` → npm，`yarn.lock` → yarn。当前仓库里是 pnpm。混用会产生两份互相打架的依赖树。
:::

前端产物（`crates/omy-gui/dist/`）**不入库**——否则每次改前端都会在 diff 里混进压缩后的 JS。改了前端源码后重新 `cargo build` 会自动重建（`cargo:rerun-if-changed` 盯着 `src`、`public`、`index.html`、`vite.config.js`、`package.json`）。

需要单独操作前端时：

```bash
cd crates/omy-gui/frontend
pnpm install
pnpm build      # 输出到 ../dist
```

`pnpm dev` 可以起 Vite dev server，但浏览器里没有 Tauri IPC，界面会明确报错。真正开发仍应 `cargo run -p omy-gui`。

## 各平台系统依赖

### Windows

已验证。装了 Rust 与 Node 就够——WebView2 是系统自带的。

### Linux

::: warning 尚未验证
下列依赖来自 Tauri v2 官方文档，omy 在 Linux 上还没有实测过。
:::

Debian / Ubuntu：

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Fedora：

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel gtk3-devel \
  libappindicator-gtk3-devel librsvg2-devel
sudo dnf group install "c-development"
```

注意 Tauri v2 要求 **WebKitGTK 4.1**，所以基线系统需要 Ubuntu 22.04 或 Debian 12 及以上。

### macOS

::: warning 尚未验证
:::

需要 Xcode Command Line Tools：

```bash
xcode-select --install
```

Tauri v2 在 macOS 上通常不需要额外的系统包。

## Android

已验证。前置条件较多，仓库里有体检脚本：

```bash
pwsh -NoProfile -File spikes\check-android-env.ps1
```

它会逐项报告缺什么以及怎么补：

| 需要 | 怎么装 |
|---|---|
| JDK 17+ | 装 Android Studio 会带上 |
| Android SDK（`ANDROID_HOME` 指向它） | SDK Manager 里勾 Platform 34 + Build-Tools + Platform-Tools |
| Android NDK（`NDK_HOME` 指向它） | SDK Tools → NDK (Side by side) |
| NDK 里的 clang | 随 NDK 提供 |
| Rust 目标 | `rustup target add aarch64-linux-android armv7-linux-androideabi` |
| tauri-cli | `cargo install tauri-cli --version "^2"` |

::: tip NDK 的 clang 最容易被漏掉
`zstd-sys` 是 C 代码，交叉编译必须有 NDK 的 clang。缺它的话连 `omy-core` 都编不过，报错看起来与 Tauri 无关。
:::

::: warning 报「找不到 aarch64-linux-android-clang」时
`cc-rs` 找的是**不带 API 级别**的 `aarch64-linux-android-clang`，而 NDK 从 r19 起只提供带级别的 wrapper（`aarch64-linux-android24-clang` 之类）。如果 PATH 上没有别的 `clang` 可退回，交叉编译就会失败。

显式指定编译器即可（24 对应本项目的 `minSdk`）：

```bash
BIN="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"   # Windows 换成 windows-x86_64
export CC_aarch64_linux_android="$BIN/aarch64-linux-android24-clang"
export CC_armv7_linux_androideabi="$BIN/armv7a-linux-androideabi24-clang"
```

注意 armv7 的 wrapper 前缀是 `armv7a-`（多一个 a），与 Rust 的目标名 `armv7-linux-androideabi` 不一致，写错了仍然报「找不到工具」。
:::

齐了之后：

```bash
cd crates/omy-gui
cargo tauri android init
cargo tauri android dev      # 连真机或模拟器
cargo tauri android build    # 出 APK
```

## 跑测试

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

两者都必须通过。clippy 在 omy-gui 上禁用 `unwrap` / `expect` / `panic` / 切片索引——加密与文件操作代码里 panic 会变成数据丢失或拒绝服务。

改了前端文案还要跑：

```bash
python spikes\check-i18n-keys.py
```

它检查 `.vue` 里用到的 i18n 键是否都在文案文件里，并确认中英键一致。缺键的症状是界面上直接显示 `remote.readonly` 这种原始键名——构建不报错，测试也不失败，只有肉眼能发现。

## 开发期的便携 FFmpeg

```bash
pwsh -NoProfile -File scripts\fetch-ffmpeg.ps1
```

下载便携版到 `tools/`（该目录不入库）。`omy-media` 会在候选目录里找到它，不必装到系统。

## 文档站

```bash
cd site
bun install
bun run dev        # 本地预览
bun run build      # 构建到 site/dist
```
