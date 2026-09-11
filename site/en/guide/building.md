---
title: Building from source
---

# Building from source

## Prerequisites

**Rust 1.85 or newer** — the project uses edition 2024, which sets that floor.

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

## The documentation site

```bash
cd site
bun install
bun run dev        # local preview
bun run build      # build into site/dist
```
