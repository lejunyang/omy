---
title: 安装
---

# 安装

omy 有两个入口：命令行工具 `omy`，以及图形界面 `omy-gui`。两者可以只装一个。

::: info 平台验证范围
下面的步骤在 **Windows** 与 **Android** 上实测过。**Linux 与 macOS 尚未验证**——代码路径已经实现，但没有在真机上跑过，遇到问题请以实际报错为准。
:::

## 方式一：下载预编译产物

每次发布会在 [GitHub Releases](https://github.com/lejunyang/omy/releases) 附上构建产物：

| 平台 | 产物 | 说明 |
|---|---|---|
| Windows | `omy-x86_64-pc-windows-msvc.zip` | 内含 `omy.exe` 命令行工具 |
| Android | `omy-gui-<版本>.apk` | 图形界面，需允许安装未知来源应用 |
| Linux / macOS | 同名压缩包 | 由未验证的流水线任务产出，可能缺失 |

下载后把 `omy` 可执行文件放到 `PATH` 里的任一目录即可。验证一下：

```bash
omy --version
omy doctor
```

`omy doctor` 会报告当前环境的实际能力——是否有 AES 硬件加速、FFmpeg 能否找到、各 KDF 档位实测耗时。装完先跑一次，比看文档更可靠。

## 方式二：用 cargo 安装

::: warning 尚未发布到 crates.io
截至目前 `omy-cli` 等包**还没有发布到 crates.io**（查询返回 404）。下面的命令要等首个版本发布之后才可用。现在请用方式一或方式三。
:::

发布之后：

```bash
cargo install omy-cli
```

装出来的可执行文件名是 `omy`（不是 `omy-cli`）。

## 方式三：从源码构建

需要 Rust 1.85 或更高——项目用的是 edition 2024。

```bash
git clone https://github.com/lejunyang/omy.git
cd omy

# 只要命令行工具
cargo build --release -p omy-cli
# 产物在 target/release/omy（Windows 上是 omy.exe）
```

图形界面还需要 Node.js 与一个包管理器，因为界面是 Vue 3 + Vite 构建的：

```bash
cargo build --release -p omy-gui
```

构建脚本会在前端产物缺失时自动装依赖并构建，所以全新 clone 之后直接跑上面这条命令就行。

::: tip 前端用哪个包管理器
`build.rs` 按仓库里已有的 lockfile 决定：有 `pnpm-lock.yaml` 就用 pnpm，有 `package-lock.json` 用 npm，有 `yarn.lock` 用 yarn。当前仓库里是 pnpm 的 lockfile。混用会产生两份互相打架的依赖树。
:::

更完整的构建说明（各平台系统依赖、Android 打包）见[从源码构建](/guide/building)。

## 可选：FFmpeg

FFmpeg **不是必需的**。缺了它，加解密、分片、共享全都正常工作，只是这些功能不可用：

- 视频缩略图（图片缩略图不需要 FFmpeg，走的是纯 Rust 的 image crate）
- 媒体元信息探测与播放路径分级
- P2 转封装播放（把 MKV 之类的容器转成 fMP4 再喂给播放器）

omy 按这个顺序找 `ffprobe` 与 `ffmpeg`：

1. 环境变量 `OMY_FFPROBE` / `OMY_FFMPEG`（指向可执行文件本身，不是目录）
2. 若干候选目录
3. `PATH`

::: warning 环境变量指错了不会静默回退
如果 `OMY_FFMPEG` 指向的路径不存在，omy 不会假装没看见然后去找 `PATH`——那会让你以为自己的设置生效了。
:::

装好之后用 `omy doctor` 确认，它会打印实际找到的版本。

## Android

Android 版是图形界面。安装 APK 之后可直接使用，加解密与浏览功能与桌面一致。

注意 Android 上没有 FFmpeg，所以视频缩略图与转封装播放不可用；另外回收站也不可用——`trash` crate 在 Android 上不提供任何实现，所以那里的「移到回收站」选项不会出现。

## 下一步

- [快速上手](/guide/getting-started)：走完第一次加密与解密
- [配置文件](/guide/configuration)：改掉每次都要敲的默认参数
