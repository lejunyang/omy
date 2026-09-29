# FFmpeg 裁剪构建

为 omy 构建只含自身所需组件的 FFmpeg。背景、取舍与全部实测数据见
[`docs/research/13-ffmpeg-minimal-build.md`](../../docs/research/13-ffmpeg-minimal-build.md)。

## 产物

单一产物，包含 omy 需要的全部能力。实测体积：

| 平台 | 内容 | 实测体积 |
|---|---|---|
| win-x64 | `ffmpeg.exe` + `ffprobe.exe` | **12.44 MB** |
| linux-x64 | `ffmpeg` + `ffprobe` | **12.53 MB** |

已 strip，静态链接 libwebp 与 zlib：Windows 版除系统 DLL 外无任何外部依赖
（可以只拷这两个 exe）；Linux 版 glibc 等系统库仍动态链接（全静态 glibc 有
NSS 等已知坑），libwebp/zlib 已嵌入，`ldd` 看不到它们。

能力范围：

- 视频抽帧缩略图：H.264 / HEVC / VP8 / VP9 / AV1
- 图片缩略图：JPEG / PNG / WebP / BMP / GIF
- 音频、字幕、`-c copy` 转封装

### 为什么不再分档

曾经出过 `royalty-free`（不含 H.264/HEVC）与 `full` 两份产物，按专利风险分。
现已取消，只发一份全量产物。

原因是免版税档的失败模式太糟：H.264 覆盖了用户手上绝大多数视频，而
**demuxer 与 decoder 是两件事**——免版税档遇到 H.264 视频时仍能读出时长和
分辨率，只是抽不出帧，报 `no decoder found for: h264`。用户看到的是「有的
视频有缩略图、有的没有」，既不像配置问题也不像能力边界，就是功能坏了。

专利立场改为在文档里声明，而不是靠砍能力来表达。

## 用法

### Windows

```powershell
# 1. 确认工具链（会打印下一步要用的 SYSROOT）
pwsh -File scripts\ffmpeg-build\prepare-toolchain.ps1

# 2. 按它打印的命令构建
$env:SYSROOT='...'
bash scripts/ffmpeg-build/build-windows.sh

# 3. 校验产物
bash scripts/ffmpeg-build/verify.sh /c/Users/LJY/AppData/Local/Temp/omy-ffmpeg-build/out
```

Windows 上**不需要安装 MSYS2**，工具链全部由根目录的 `osdk.toml` 提供。

### Linux

```bash
# nasm 由 osdk.toml 提供（系统通常不自带），gcc/make/cmake/pkg-config 用系统的
bash scripts/ffmpeg-build/build-linux.sh

# 校验产物（verify.sh 按文件名自动识别平台，Linux 查 ldd，Windows 查 objdump）
bash scripts/ffmpeg-build/verify.sh /tmp/omy-ffmpeg-build/out
```

产物目录默认是 `$OMY_FF_WORK/out`（`OMY_FF_WORK` 默认
`/tmp/omy-ffmpeg-build`），可用 `OMY_FF_OUT` 指定别处——CI 就是这么把产物
直接放进打包目录的。

## 文件

| 文件 | 作用 |
|---|---|
| `VERSION` | 三个上游库的版本与 SHA256，每两行一组 |
| `configure-flags.sh` | 组件配方（Windows / Linux 共用，由 `FF_TARGET` 区分） |
| `build-windows.sh` | 下载校验、编 zlib 与 libwebp、交叉编 FFmpeg |
| `build-linux.sh` | 下载校验、编 zlib 与 libwebp、原生编 FFmpeg |
| `verify.sh` | 校验许可证边界、外部依赖、逐项复查组件 |
| `prepare-toolchain.ps1` | 检查工具链并算出 `SYSROOT`（仅 Windows） |

## 为什么必须跑 verify.sh

本轮踩到的四个问题（`rawvideo` / `fd` 协议 / `image_png_pipe` / `movtext`）
**全都是 configure 接受了参数但没启用，且不报任何错**：configure 退出 0、
make 退出 0、手工命令行还跑得通，只有 omy 自己的测试才失败。

其中最隐蔽的是 pipe 类 demuxer：`is_still_image` 靠 ffprobe 报的容器名判断，
拿不到容器名时图片走不进图片分支，结果 `thumbnail=None` 且 `warnings=[]`
——没有任何错误信息。

所以「编过了」不能作为接受标准。改动配方后除了 `verify.sh`，还要跑：

```powershell
$env:OMY_FFMPEG='<产物目录>\ffmpeg.exe'
$env:OMY_FFPROBE='<产物目录>\ffprobe.exe'
cargo test -p omy-media
```

`verify.sh` 里那条「必须含 h264/hevc」的检查是分档取消后保留下来的：
方向反转了，作用没变。配方改动让专利解码器意外掉出去时，编得过、跑得动、
体积只小两三兆，只有用户打开 H.264 视频才暴露——必须在构建期就拦住。

## 组件名有两套，别混

configure 的组件名与 CLI 显示名不一定相同，写错时 configure **不报错也不启用**：

| configure | CLI |
|---|---|
| `--enable-demuxer=image_png_pipe` | `png_pipe` |
| `--enable-encoder=movtext` | `mov_text` |

`verify.sh` 查的是 CLI 名，因为那才是 omy 实际传给 ffmpeg 的名字。
