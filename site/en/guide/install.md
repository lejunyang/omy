---
title: Installation
---

# Installation

omy has two front ends: the `omy` command line tool and the `omy-gui` graphical interface. Installing just one is fine.

::: info Tested platforms
The steps below have been tested on **Windows** and **Android**. **Linux and macOS are not yet verified** — the code paths exist but have never been exercised on real machines, so trust the actual error output over this page there.
:::

## Option 1: download a prebuilt artifact

Each release attaches artifacts for every platform to [GitHub Releases](https://github.com/lejunyang/omy/releases) (a build failure on any platform aborts the release, so either every file below is present or there is no release at all):

| Platform | Artifact | Notes |
|---|---|---|
| Windows | `omy-<version>-x86_64-pc-windows-msvc.zip` | Contains `omy.exe`, `omy-gui.exe`, and a bundled FFmpeg; unzip and run |
| Linux | `omy-<version>-x86_64-unknown-linux-gnu.tar.gz` | Contains `omy` and `omy-gui`; no bundled FFmpeg |
| macOS (Apple Silicon) | `omy-<version>-aarch64-apple-darwin.tar.gz` | Contains `omy` and `omy-gui`; no bundled FFmpeg |
| macOS (Intel) | `omy-<version>-x86_64-apple-darwin.tar.gz` | Same as above |
| Android | `*.apk` (one per ABI) | GUI; **unsigned**, requires allowing unknown sources |

The Linux and macOS artifacts are compiled and packaged in CI but have not been exercised on real machines; Windows and Android have been tested.

Put the `omy` executable (`omy.exe` on Windows) anywhere on your `PATH`, then check it:

```bash
omy --version
omy doctor
```

`omy doctor` reports what this machine can actually do — whether AES hardware acceleration exists, whether FFmpeg was found, and the measured cost of each KDF profile. Run it once after installing; it is more reliable than any documentation.

## Option 2: install with cargo

::: warning Not published to crates.io yet
As of now `omy-cli` and the other packages are **not on crates.io** (the registry returns 404). The command below only works once the first version is published. Use option 1 or 3 for the time being.
:::

After publication:

```bash
cargo install omy-cli
```

The installed binary is named `omy`, not `omy-cli`.

## Option 3: build from source

Requires Rust 1.85 or newer, since the project uses edition 2024.

```bash
git clone https://github.com/lejunyang/omy.git
cd omy

# CLI only
cargo build --release -p omy-cli
# Artifact at target/release/omy (omy.exe on Windows)
```

The GUI additionally needs Node.js and a package manager, because the interface is built with Vue 3 and Vite:

```bash
cargo build --release -p omy-gui
```

The build script installs frontend dependencies and builds them when the output is missing, so a fresh clone works with just that command.

::: tip Which package manager the frontend uses
`build.rs` follows whichever lockfile is present: `pnpm-lock.yaml` means pnpm, `package-lock.json` means npm, `yarn.lock` means yarn. This repository ships a pnpm lockfile. Mixing them produces two dependency trees fighting each other.
:::

For per-platform system dependencies and Android packaging, see [Building from source](/en/guide/building).

## FFmpeg

**The Windows build ships with FFmpeg** — unzip and it works, no separate install. It lives in the `ffmpeg\` directory inside the archive and is our own trimmed build (12.44 MB, with H.264 / HEVC / VP8 / VP9 / AV1 decoders), depending on nothing beyond the system DLLs.

Other platforms currently ship **without** FFmpeg and need their own install — the build chain has only been verified on Windows, and an unverified bundled binary is worse than none.

Without FFmpeg, encryption, decryption, sharding and sharing all work normally; only these features are unavailable:

- Video thumbnails (image thumbnails need no FFmpeg — they go through the pure-Rust image crate)
- Media metadata probing and playback tiering
- P2 remux playback (rewrapping containers such as MKV into fMP4 for the player)

### Using your own FFmpeg

Bundled does not mean locked in. omy looks for `ffprobe` and `ffmpeg` in this order:

1. The `OMY_FFPROBE` / `OMY_FFMPEG` environment variables (pointing at the executable itself, not a directory)
2. A set of candidate directories, including the bundled `ffmpeg\` and `ffmpeg\bin`
3. `PATH`

The environment variables come first, so pointing them at your own build is enough; overwriting the files in `ffmpeg\` works too.

::: warning A wrong environment variable does not silently fall back
If `OMY_FFMPEG` points at a path that does not exist, omy will not quietly ignore it and search `PATH` instead — that would let you believe your setting took effect.
:::

Confirm with `omy doctor`, which prints the version it actually found.

::: tip On codec patents
The bundled FFmpeg includes H.264 / HEVC decoders, which are covered by patents. omy distributes this build under LGPL-2.1+, but grants **no codec patent licence**. Personal use is generally unaffected; evaluate for yourself before redistributing commercially or publishing to an app store.
:::

## Android

The Android build is the graphical interface. Install the APK and it works; encryption, decryption and browsing match the desktop.

Note that Android has no FFmpeg, so video thumbnails and remux playback are unavailable, and there is no trash either — the `trash` crate provides no implementation on that platform, so the "move to trash" option does not appear.

## Next

- [Getting started](/en/guide/getting-started)
- [Configuration file](/en/guide/configuration)
