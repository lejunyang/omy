---
title: 从源码构建
---

# 从源码构建

## 前置条件

**Rust 1.88 或更高**——`grammers-mtsender` 的 DNS 依赖要求 1.88，
高于 edition 2024 自身的 1.85 下限。

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

已验证（Apple Silicon）。需要 Xcode Command Line Tools：

```bash
xcode-select --install
```

Tauri v2 在 macOS 上通常不需要额外的系统包。

## 用 zig 做交叉检查

只有 Windows 机器时，可以用 [zig](https://ziglang.org/) 当 C 交叉编译器，在本地
`cargo check` 别的平台，不必等 CI。zig 自带各目标的 libc 与 macOS SDK 头文件，
装一个可执行文件即可，不需要 Xcode 或 Linux sysroot。

`cc-rs` 会把 `CC_<target>` 当成单个可执行文件调用，塞不进 `zig cc` 两个词，
所以要包一层脚本。而且 **cc-rs 传的架构名和 zig 认的不一样**——针对 Apple 目标
它传 `--target=arm64-apple-macosx`，zig 只认 `aarch64`，会报
`unknown architecture: 'arm64'`。包装脚本要把 cc-rs 追加的 `--target` / `-arch`
滤掉，换成 zig 的写法。

包装脚本用 Python，**不要用 batch 或 `pwsh -File`**：cc-rs 传的
`-mmacosx-version-min=11.0` 经过这两者会被拆成 `-mmacosx-version-min=11` 和
`.0` 两个参数，报错是莫名其妙的 `.0: unrecognized file extension`；batch 里
`echo %~1 | findstr` 也过滤不掉带 `=` 的参数。这两条都实测踩过。

```python
# zigcc.py
import os, subprocess, sys

ZIG_TARGET = os.environ.get("OMY_ZIG_TARGET", "aarch64-macos")
args, skip = [], False
for a in sys.argv[1:]:
    if skip:
        skip = False
        continue
    if a.startswith(("--target=", "-target=")):
        continue
    if a in ("-target", "--target", "-arch"):
        skip = True
        continue
    args.append(a)
sys.exit(subprocess.run(["zig", "cc", "-target", ZIG_TARGET] + args,
                        shell=(os.name == "nt")).returncode)
```

cc-rs 只认可执行文件，所以再包一个 `.cmd`（`zigcc.cmd`）：

```bat
@echo off
python <zigcc.py 的路径> %*
```

`zigar.cmd` 同理，内容是 `zig ar %*`。然后：

```bat
set OMY_ZIG_TARGET=aarch64-macos
set CC_aarch64_apple_darwin=<zigcc.cmd 的路径>
set AR_aarch64_apple_darwin=<zigar.cmd 的路径>
cargo check --workspace --all-targets --target aarch64-apple-darwin
```

换目标时改 `OMY_ZIG_TARGET` 与对应的 `CC_<target>` 变量名即可。注意 zig 与 Rust
的三元组写法不同：`aarch64-apple-darwin` → `aarch64-macos`，
`x86_64-unknown-linux-gnu` → `x86_64-linux-gnu`。

实测可用范围（zig 0.16.0）：

| 目标 | 结果 |
| --- | --- |
| `aarch64-apple-darwin` | ✅ 整个 workspace（含 omy-gui）都能 check |
| `x86_64-apple-darwin` | ✅ 同上 |
| `x86_64-unknown-linux-gnu` | ⚠️ core / media / net / cli 可以，**omy-gui 不行** |
| `aarch64-linux-android` | ❌ 用不了，仍需 NDK |

Linux 上 omy-gui 卡在 `libdbus-sys`：它用 `pkg-config` 找 `dbus-1`，而这是
**Linux 系统库**，不是编译器能提供的东西。zig 给的是 libc，给不了第三方 `.so`
和头文件，所以这条得靠 CI 或真机。

Android 用不了的原因是 zig **不捆绑 Bionic 头文件**：编一个只
`#include <string.h>` 的文件就会报 `'string.h' file not found`（`aarch64-linux-android`
和带 API 级别的 `aarch64-linux-android.24` 都一样）。所以 Android 仍必须用
NDK 的 clang，见下一节。

::: tip 这只是 check，不是真机验证
交叉 `cargo check` 只能抓编译期问题——比如某个 API 在 macOS 上不存在。运行期
行为（权限、回收站、路径大小写）仍然只有真机能验证。
:::

## Android

已验证。前置条件较多，仓库里有体检脚本：

```bash
pwsh -NoProfile -File spikes\check-android-env.ps1
```

它会逐项报告缺什么以及怎么补：

| 需要 | 怎么装 |
|---|---|
| JDK 21 | 装 Android Studio 会带上，版本与 `osdk.toml` 一致 |
| Android SDK（`ANDROID_HOME` 指向它） | SDK Manager 里勾 Platform 36 + Build-Tools 37.0.0 + Platform-Tools |
| Android NDK 29.0.14206865（`NDK_HOME` 指向它） | SDK Tools → NDK (Side by side) |
| NDK 里的 clang | 随 NDK 提供 |
| Rust 目标 | `aarch64-linux-android`、`armv7-linux-androideabi`、`i686-linux-android`、`x86_64-linux-android` |
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
osdk run android-release-build  # 构建前端并生成四 ABI release APK

# 需要交互调试时仍可直接使用 Tauri：
cd crates/omy-gui
cargo tauri android init
cargo tauri android dev      # 连真机或模拟器
```

`android-release-build` 会补齐四个 Rust Android target，安装锁定的前端依赖并
构建 `frontendDist`，再安装 Tauri CLI、执行 APK 构建。这样不会在 Tauri 检查
静态资源时才发现前端尚未生成。

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

## 构建内置的 FFmpeg

Windows release 产物里带的那份 FFmpeg 是自己编的，不是下载的。配方与脚本在 `scripts/ffmpeg-build/`，日常开发用不到——只有改编解码能力时才需要重编。

```powershell
# 1. 确认工具链，它会打印下一步要用的 SYSROOT
pwsh -File scripts\ffmpeg-build\prepare-toolchain.ps1

# 2. 构建（约 12.44 MB，含 H.264 / HEVC / VP8 / VP9 / AV1）
$env:SYSROOT='...'
bash scripts/ffmpeg-build/build-windows.sh

# 3. 校验产物
bash scripts/ffmpeg-build/verify.sh /tmp/omy-ffmpeg-build/out
```

Windows 上**不需要装 MSYS2**，mingw 交叉编译器与 POSIX 构建环境都由根目录的 `osdk.toml` 提供。

### macOS

需要 Xcode Command Line Tools 与 `brew install pkg-config nasm cmake`。两架构共用同一 SDK，交叉编只靠 `-arch`：

```bash
# 本机架构（Apple Silicon 上即 arm64）
bash scripts/ffmpeg-build/build-macos.sh

# Intel：在 arm64 机器上交叉编（产物经 Rosetta 运行）
ARCH=x86_64 bash scripts/ffmpeg-build/build-macos.sh

# 校验产物（macOS 查 otool -L，并打印 lipo 架构）
bash scripts/ffmpeg-build/verify.sh /tmp/omy-ffmpeg-build/out
```

改完配方后同样要跑 `cargo test -p omy-media`，下面的警告对所有平台都成立。

::: warning verify.sh 不是可选步骤
改过配方之后一定要跑。曾经踩到的四个问题（`rawvideo`、`fd` 协议、`image_png_pipe`、`movtext`）全都是 configure 接受了参数但没启用，**且不报任何错**：configure 退出 0、make 退出 0、手工命令行还跑得通，只有 omy 自己的测试才失败。

所以「编过了」不能作为接受标准，跑完 `verify.sh` 还要再跑一次项目测试：

```powershell
$env:OMY_FFMPEG='<产物目录>\ffmpeg.exe'
$env:OMY_FFPROBE='<产物目录>\ffprobe.exe'
cargo test -p omy-media
```
:::

## 文档站

```bash
cd site
bun install
bun run dev        # 本地预览
bun run build      # 构建到 site/dist
```
