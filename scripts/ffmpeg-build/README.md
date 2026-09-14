# FFmpeg 裁剪构建

为 omy 构建只含自身所需组件的 FFmpeg。背景、取舍与全部实测数据见
[`docs/research/13-ffmpeg-minimal-build.md`](../../docs/research/13-ffmpeg-minimal-build.md)。

## 两个档位

| 档位 | 视频解码器 | 实测体积（win-x64） |
|---|---|---|
| `royalty-free` | VP8 / VP9 / AV1 | **9.87 MB** |
| `full` | 再加 H.264 / HEVC | **12.44 MB** |

体积均为 `ffmpeg.exe` + `ffprobe.exe` 之和，已 strip，静态链接 libwebp 与
zlib，除系统 DLL 外无任何外部依赖（可以只拷这两个 exe）。

两档的图片能力完全相同（JPEG / PNG / WebP / BMP / GIF 缩略图），音频、字幕、
remux 也相同。差异只在视频：

```
             探测时长/分辨率    生成缩略图
royalty-free      都可以        VP8/VP9/AV1 可以，H.264/HEVC 不行
full              都可以        全都可以
```

注意「探测成功但缩略图失败」这个组合——demuxer 与 decoder 是两件事。
免版税档遇到 H.264 视频时仍能读出时长和分辨率，只是抽不出帧，报
`no decoder found for: h264`。**这一点必须在 `omy doctor` 里说清**，
否则用户会以为功能坏了。

## 用法

```powershell
# 1. 确认工具链（会打印下一步要用的 SYSROOT）
pwsh -File scripts\ffmpeg-build\prepare-toolchain.ps1

# 2. 按它打印的命令构建，两个档位各跑一次
$env:SYSROOT='...'; $env:OMY_FF_PROFILE='royalty-free'
bash scripts/ffmpeg-build/build-windows.sh

# 3. 校验产物
bash scripts/ffmpeg-build/verify.sh /c/Users/LJY/AppData/Local/Temp/omy-ffmpeg-build/out/royalty-free
```

Windows 上**不需要安装 MSYS2**，工具链全部由根目录的 `osdk.toml` 提供。

## 文件

| 文件 | 作用 |
|---|---|
| `VERSION` | 三个上游库的版本与 SHA256，每两行一组 |
| `configure-flags.sh` | 组件配方，两档共用，由 `$OMY_FF_PROFILE` 切换差异 |
| `build-windows.sh` | 下载校验、编 zlib 与 libwebp、编 FFmpeg |
| `verify.sh` | 校验许可证边界、外部依赖、逐项复查组件 |
| `prepare-toolchain.ps1` | 检查工具链并算出 `SYSROOT` |

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

`verify.sh` 里那条「免版税档不得包含 h264/hevc」的检查同样重要：
一旦误带进专利编解码器，整个分档的意义就没了，而这从体积和外观都看不出来。

## 组件名有两套，别混

configure 的组件名与 CLI 显示名不一定相同，写错时 configure **不报错也不启用**：

| configure | CLI |
|---|---|
| `--enable-demuxer=image_png_pipe` | `png_pipe` |
| `--enable-encoder=movtext` | `mov_text` |

`verify.sh` 查的是 CLI 名，因为那才是 omy 实际传给 ffmpeg 的名字。
