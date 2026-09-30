---
title: Building from source
---

# Building from source

## Prerequisites

**Rust 1.88 or newer** — `grammers-mtsender`'s DNS dependencies require 1.88,
which is higher than edition 2024's own 1.85 floor.

```bash
rustc --version
```

The CLI needs nothing else. The GUI additionally needs Node.js and a package manager, since the interface is built with Vue 3 and Vite.

## Building the CLI only

```bash
git clone https://github.com/lejunyang/omy.git
cd omy
cargo build --release -p omy-cli
```

The artifact is `target/release/omy` (`omy.exe` on Windows). Note the binary is called `omy`, not `omy-cli`.

## Building the GUI

```bash
cargo build --release -p omy-gui
```

The build script handles the frontend automatically: when the output is missing it installs dependencies and builds, so a fresh clone works with that one command.

::: tip The package manager follows the lockfile
`build.rs` picks based on which lockfile exists: `pnpm-lock.yaml` → pnpm, `package-lock.json` → npm, `yarn.lock` → yarn. This repository ships pnpm. Mixing them produces two competing dependency trees.
:::

The frontend output (`crates/omy-gui/dist/`) is **not committed** — otherwise every frontend change would drag minified JS into the diff. Editing frontend sources and rebuilding picks the changes up (`cargo:rerun-if-changed` watches `src`, `public`, `index.html`, `vite.config.js` and `package.json`).

To work on the frontend directly:

```bash
cd crates/omy-gui/frontend
pnpm install
pnpm build      # outputs to ../dist
```

`pnpm dev` starts a Vite dev server, but a plain browser has no Tauri IPC, so the interface reports an explicit error. Real development still means `cargo run -p omy-gui`.

## System dependencies per platform

### Windows

Verified. Rust and Node suffice — WebView2 ships with the system.

### Linux

::: warning Not yet verified
The dependencies below come from the official Tauri v2 documentation; omy has not been tested on Linux.
:::

Debian / Ubuntu:

```bash
sudo apt update
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file \
  libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

Fedora:

```bash
sudo dnf install webkit2gtk4.1-devel openssl-devel gtk3-devel \
  libappindicator-gtk3-devel librsvg2-devel
sudo dnf group install "c-development"
```

Tauri v2 requires **WebKitGTK 4.1**, so the baseline is Ubuntu 22.04 or Debian 12 and newer.

### macOS

::: warning Not yet verified
:::

Xcode Command Line Tools are needed:

```bash
xcode-select --install
```

Tauri v2 generally needs no extra system packages on macOS.

## Cross-checking with zig

If you only have a Windows machine, [zig](https://ziglang.org/) can act as the C
cross-compiler so you can `cargo check` other platforms locally instead of waiting
for CI. zig bundles libc for each target plus the macOS SDK headers, so a single
executable is enough — no Xcode, no Linux sysroot.

`cc-rs` invokes `CC_<target>` as one executable, so `zig cc` (two words) does not
fit; it needs a wrapper. And **cc-rs and zig disagree on architecture names**: for
Apple targets cc-rs passes `--target=arm64-apple-macosx`, while zig only accepts
`aarch64` and fails with `unknown architecture: 'arm64'`. The wrapper has to strip
the `--target` / `-arch` that cc-rs appends and substitute zig's spelling.

Write the wrapper in Python — **not batch, and not `pwsh -File`**. Both mangle
`-mmacosx-version-min=11.0` into `-mmacosx-version-min=11` plus `.0`, which
surfaces as the baffling `.0: unrecognized file extension`; batch also cannot
filter arguments containing `=` via `echo %~1 | findstr`. Both were hit in
practice.

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

cc-rs only accepts an executable, so wrap that in a `.cmd` (`zigcc.cmd`):

```bat
@echo off
python <path to zigcc.py> %*
```

`zigar.cmd` is the same idea, containing `zig ar %*`. Then:

```bat
set OMY_ZIG_TARGET=aarch64-macos
set CC_aarch64_apple_darwin=<path to zigcc.cmd>
set AR_aarch64_apple_darwin=<path to zigar.cmd>
cargo check --workspace --all-targets --target aarch64-apple-darwin
```

To switch targets, change `OMY_ZIG_TARGET` and the matching `CC_<target>` variable.
Note that zig and Rust spell triples differently: `aarch64-apple-darwin` →
`aarch64-macos`, `x86_64-unknown-linux-gnu` → `x86_64-linux-gnu`.

Measured coverage (zig 0.16.0):

| Target | Result |
| --- | --- |
| `aarch64-apple-darwin` | ✅ the whole workspace (including omy-gui) checks |
| `x86_64-apple-darwin` | ✅ same |
| `x86_64-unknown-linux-gnu` | ⚠️ core / media / net / cli work, **omy-gui does not** |
| `aarch64-linux-android` | ❌ unusable, the NDK is still required |

On Linux, omy-gui stops at `libdbus-sys`: it looks for `dbus-1` through
`pkg-config`, and that is a **Linux system library**, not something a compiler can
provide. zig supplies libc, not third-party `.so` files and headers, so this one
needs CI or a real machine.

Android does not work because zig **does not bundle Bionic headers**: compiling a
file that only does `#include <string.h>` already fails with
`'string.h' file not found` (for both `aarch64-linux-android` and the
API-qualified `aarch64-linux-android.24`). Android therefore still requires the
NDK's clang — see the next section.

::: tip This is only a check, not real-device verification
A cross `cargo check` catches compile-time problems only — an API that does not
exist on macOS, for instance. Runtime behaviour (permissions, the recycle bin,
path case sensitivity) can still only be verified on a real machine.
:::

## Android

Verified. There are several prerequisites, and the repository ships a checker:

```bash
pwsh -NoProfile -File spikes\check-android-env.ps1
```

It reports each missing item and how to obtain it:

| Requirement | How to get it |
|---|---|
| JDK 17+ | Comes with Android Studio |
| Android SDK (`ANDROID_HOME` points at it) | SDK Manager: Platform 34 + Build-Tools + Platform-Tools |
| Android NDK (`NDK_HOME` points at it) | SDK Tools → NDK (Side by side) |
| clang inside the NDK | Ships with the NDK |
| Rust targets | `rustup target add aarch64-linux-android armv7-linux-androideabi` |
| tauri-cli | `cargo install tauri-cli --version "^2"` |

::: tip The NDK's clang is the easiest thing to miss
`zstd-sys` is C code, so cross-compiling requires clang from the NDK. Without it even `omy-core` fails to build, with an error that looks unrelated to Tauri.
:::

::: warning If you see "failed to find tool aarch64-linux-android-clang"
`cc-rs` looks for `aarch64-linux-android-clang` **without an API level**, but since r19 the NDK only ships the versioned wrappers (`aarch64-linux-android24-clang` and friends). If no other `clang` is on `PATH` to fall back to, cross-compilation fails.

Point it at the real compiler (24 matches this project's `minSdk`):

```bash
BIN="$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"   # use windows-x86_64 on Windows
export CC_aarch64_linux_android="$BIN/aarch64-linux-android24-clang"
export CC_armv7_linux_androideabi="$BIN/armv7a-linux-androideabi24-clang"
```

Note the armv7 wrapper is prefixed `armv7a-` (with an extra `a`), which does not match Rust's target name `armv7-linux-androideabi`. Getting it wrong still reports "failed to find tool".
:::

Once ready:

```bash
cd crates/omy-gui
cargo tauri android init
cargo tauri android dev      # attach a device or emulator
cargo tauri android build    # produce an APK
```

## Running the tests

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Both must pass. clippy forbids `unwrap` / `expect` / `panic` / slice indexing in omy-gui — in cryptography and file handling code a panic becomes data loss or denial of service.

After changing frontend strings, also run:

```bash
python spikes\check-i18n-keys.py
```

It verifies that every i18n key used in `.vue` files exists in the string files and that the Chinese and English key sets match. A missing key shows up as a raw key name like `remote.readonly` in the interface — the build does not fail and tests do not catch it; only eyes do.

## A portable FFmpeg for development

```bash
pwsh -NoProfile -File scripts\fetch-ffmpeg.ps1
```

Downloads a portable build into `tools/` (not committed). `omy-media` finds it among its candidate directories, so nothing needs installing system-wide.

## Building the bundled FFmpeg

The FFmpeg shipped in the Windows release is built by us, not downloaded. The recipe and scripts live in `scripts/ffmpeg-build/`. Day-to-day development never needs this — only changes to codec coverage do.

```powershell
# 1. Check the toolchain; it prints the SYSROOT for the next step
pwsh -File scripts\ffmpeg-build\prepare-toolchain.ps1

# 2. Build (about 12.44 MB, with H.264 / HEVC / VP8 / VP9 / AV1)
$env:SYSROOT='...'
bash scripts/ffmpeg-build/build-windows.sh

# 3. Verify the output
bash scripts/ffmpeg-build/verify.sh /tmp/omy-ffmpeg-build/out
```

**MSYS2 is not needed on Windows**: both the mingw cross compiler and the POSIX build environment come from the `osdk.toml` at the repository root.

::: warning verify.sh is not optional
Run it whenever the recipe changes. All four problems hit so far (`rawvideo`, the `fd` protocol, `image_png_pipe`, `movtext`) were cases where configure accepted the argument but did not enable the component, **and reported nothing**: configure exited 0, make exited 0, hand-written command lines still worked, and only omy's own tests failed.

So "it compiled" is not an acceptance criterion. After `verify.sh`, run the project's tests against the build as well:

```powershell
$env:OMY_FFMPEG='<output dir>\ffmpeg.exe'
$env:OMY_FFPROBE='<output dir>\ffprobe.exe'
cargo test -p omy-media
```
:::

## The documentation site

```bash
cd site
bun install
bun run dev        # local preview
bun run build      # build into site/dist
```
