# FFmpeg 裁剪内置方案

> 对应决策 **D-11**（FFmpeg 追求最大兼容）、**D-23**（分层授权）、
> **D-12**（HEIC 分层策略）。
>
> 状态：**方案已定方向，实施细节待细化**。已确定走自建构建链、出三档 release
> 产物（无 FFmpeg / 内置免版税档 / 全部内置档）。本文档不描述已实现的行为。
>
> 撰写日期：2026-09-12

## 0. 一句话结论

**可行，而且比预想的小得多。** 已在本机完成一次真实裁剪构建（win-x64、静态、
strip）：`ffmpeg.exe` 5.59 MB + `ffprobe.exe` 5.49 MB = **11.08 MB**，
对照完整构建的三个 exe 各约 212 MB。**这 11 MB 已含 H.264/HEVC/AV1/AAC
等全部解码器**，即"全部内置档"就是这个量级，免版税档只会更小。

（文档早期按模块估算的 20–35 MB 偏大约 3 倍，原因见 §1.3 的订正。
你最初说的"5–15 MB 可直接内置"——**结论是成立的**。）

**Windows 不需要装 MSYS2**：已实测用 osdk 的 `conda:` backend 拿到全套工具链
并跑通 configure + make，产物仅依赖系统 DLL，可脱离 osdk 直接运行（§4.2）。

**专利仍是主要约束**：内置解码器会把 H.264/HEVC/AAC 的专利责任从用户转移到
我们身上，而 D-12 策略正是"把专利责任转移给 OS 厂商"。**三档产物方案化解了
这个两难**——默认发免版税档，完整能力由用户显式选择（§4.3）。

**仍待实测**：加上 libwebp 编码器后的体积，以及 linux / macOS 的对应数字。



***

## 1. omy 到底需要哪些组件

> **标题已订正。** 本节原题是"为什么 5–15 MB 不适用"，依据是按模块估算得出的
> 20–35 MB。**实测把这个结论推翻了**：真实构建是 11.08 MB（§1.3），
> 你最初的判断是对的。本节保留下来的价值在于**逐条说明 omy 需要哪些组件**，
> 这是写 configure 配方的依据。

作为对照，`genesys-ffmpeg-minimal` 那个 5–15 MB 的适用范围是

**音频转 MP3**（WAV/FLAC/AIFF/OGG/AAC/M4A → MP3），没有视频解码器、没有

视频滤镜、没有 MP4 muxer。\["[https://github.com/directivegames/genesys-ffmpeg-minimal](https://github.com/directivegames/genesys-ffmpeg-minimal)"]

omy 的需求面比它宽得多——**但仍然落在同一个量级**。下面按代码实测逐条列出。

### 1.1 omy 实际用到的四条路径



| 路径             | 代码位置               | 调用形态                                 |
| -------------- | ------------------ | ------------------------------------ |
| 媒体探测           | `probe.rs:487`     | `ffprobe -show_format -show_streams` |
| 图片缩略图的 WebP 编码 | `thumbnail.rs:269` | `rawvideo → libwebp`                 |
| 视频抽帧缩略图        | `thumbnail.rs:416` | `任意容器 → scale → libwebp`             |
| P2 转封装         | `remux.rs:235/298` | `任意容器 → -c copy → fMP4`              |

**不含任何重编码**：`remux.rs:174` 是 `-c copy`，`tier.rs` 的 P3（需转码）

当前不调用 FFmpeg。D-26 的 "转码后加密" 尚未实现。

### 1.2 各路径的组件需求（实测）

**转封装：视频 / 音频确实不需要解码器。** 用 LGPL 构建跑 omy 的真实参数，

`Stream mapping` 显示：



```
Stream #0:0 -> #0:0 (copy)

Stream #0:1 -> #0:1 (copy)

Stream #0:2 -> #0:2 (subrip (srt) -> mov\_text (native))
```

前两条是纯 copy。**但第三条是真的转码**——`remux.rs:183` 的 `-c:s mov_text`

要求字幕解码器（subrip/ass/webvtt…）+ mov\_text 编码器。加 `-sn` 时这条消失。

**视频抽帧则必须有完整解码器。** `thumbnail.rs:402` 用了 `-vf scale=...`，

要解码真实帧再缩放。用户手上的视频是什么编码就得能解什么 —— 这是体积的主要来源，

也是与那个 "音频专用" 参考项目的根本差异。

### 1.3 体积估算



| 组件                                   | 必要性        | 估算           |
| ------------------------------------ | ---------- | ------------ |
| avformat（\~20 个 demuxer + mp4 muxer） | 必需         | 4–6 MB       |
| avcodec 框架 + parser + bsf            | 必需         | 3–4 MB       |
| h264 解码器                             | 抽帧必需       | 1.5–2 MB     |
| hevc 解码器                             | 抽帧必需       | 2–3 MB       |
| vp8/vp9 解码器                          | 抽帧必需       | 2–3 MB       |
| av1（dav1d）                           | 抽帧必需       | 3–5 MB       |
| 音频解码器（aac/mp3/opus/vorbis/flac/pcm）  | 探测 + 字幕对齐  | 1.5–2 MB     |
| 字幕编解码（subrip/ass/webvtt + mov\_text） | remux 字幕必需 | 0.3 MB       |
| libwebp 编码器                          | 必需         | 0.5 MB       |
| swscale（scale 滤镜）                    | 抽帧必需       | 1–1.5 MB     |
| avfilter 框架（仅 scale/format/null）     | 抽帧必需       | 1–2 MB       |
| avutil                               | 必需         | 1–1.5 MB     |
| **合计（单平台，静态，strip）**                 |            | **21–33 MB** |

> **上表已被实测推翻，偏大约 3 倍。** 2026-09-12 用 osdk 的 conda 工具链在本机
> 完成了一次真实裁剪构建（FFmpeg 7.1.1，win-x64，静态，strip）：
>
> | 产物 | 体积 |
> |---|---|
> | `ffmpeg.exe` | **5.59 MB** |
> | `ffprobe.exe` | **5.49 MB** |
> | **合计** | **11.08 MB** |
>
> 对照本机完整 GPL 构建：三个 exe 各约 212 MB。
>
> 而且**这 11 MB 已经含 H.264 / HEVC / AV1 / VP8 / VP9 / AAC 等全部解码器**，
> 即它对应的是"全部内置档"，不是免版税档。免版税档只会更小。
>
> 估算偏大的原因：按模块分别估体积时，重复计入了各解码器共享的框架代码，
> 而 `--disable-everything` + `--enable-small` 后链接器会把未引用的部分整个丢掉。
>
> 实测配方与验证过程见 §4.2。**仍待实测**：加上 libwebp 编码器后的体积，
> 以及 linux/macOS 三平台的对应数字。

可核对的锚点：完整 LGPL 构建有 **547 个解码器、238 个编码器**，而 omy 需要的

解码器只有约 **13 个**（h264/hevc/vp8/vp9/av1 + aac/mp3/opus/vorbis/flac +

subrip/ass/webvtt），编码器 2 个（libwebp、mov\_text）。已实测确认这 13 个

**全部存在于 LGPL 构建中**，不需要 GPL 组件。

但组件数占比不能线性换算成体积：解码器之间体积差异极大（av1/hevc 是 h264 的

数倍），而 avformat/avutil/swscale 这些框架部分无论裁到多小都得留。所以

上表按模块估算，而非按 13/547 折算。

**降级档位**：若砍掉 AV1 与 HEVC 抽帧（缺时视频无缩略图，静态图不受影响），

可降到 **12–18 MB**；若连视频抽帧一起砍掉（只保留探测 + WebP 编码 + 纯 copy

转封装），可降到 **8–12 MB**—— 但那样 P2 播放的字幕会丢，视频列表没有缩略图。

**乘以平台数**：win-x64 /mac-arm64 /mac-x64 /linux-x64 /linux-arm64，

每个安装包各自带一份。Android 另算（见 §4）。



***

## 2. 专利：比体积更需要先决策的事

10 号文档 §3.3 为 HEIC 确立了 D-12 三层策略，理由写得很清楚：

> 这就是为什么 Firefox、Chromium 等浏览器长期不内置 HEVC 软解，而是调用系统
> 解码器 ——
>
> **把专利责任转移给已获授权的操作系统厂商**
>
> 。

\*\* 内置 H.264/HEVC/AAC 解码器会反转这个决定。\*\* 目前 omy 不分发任何解码器，

用户自己装的 FFmpeg 由用户自己负责；一旦官方安装包里带上解码器，我们就成了

"分发编解码实现" 的一方。

10 号文档 §3.4 已指出风险主要落在**商业分发者**与**应用商店上架者**，并要求

在 README 中声明不提供专利许可。内置后这条声明的分量会明显加重。

具体到各编码（数据来自 10 号文档 §3.1）：



| 编码               | 内置后的风险                           | 备选        |
| ---------------- | -------------------------------- | --------- |
| H.264            | 🟡 MPEG LA 池，基础专利 2027–2030 陆续到期 | 系统解码器     |
| HEVC             | 🔴 多池并存，学术界认为开源无法获得合规许可          | **建议不内置** |
| AAC              | 🟡 Via LA 池                      | 系统解码器     |
| VP8/VP9/AV1/WebP | 🟢 免版税                           | 可安全内置     |

**建议**：裁剪构建只内置免版税编码（VP8/VP9/AV1/Opus/Vorbis/FLAC/WebP），

H.264/HEVC/AAC 走系统解码器或让用户自备完整 FFmpeg。这与 D-12 的分层思路一致，

体积也能压到 **12–18 MB**。

代价要说清楚：H.264 是目前最常见的视频编码，不内置意味着**多数用户的视频仍拿不到**

**缩略图**—— 除非他们自己装 FFmpeg。

> **三档方案如何化解这个两难**（后续决定，见 §4.3）
>
> 分成"免版税档"与"全量档"两份产物之后，这里不必二选一：
> 默认分发免版税档（专利干净），需要完整能力的用户显式选择全量档，
> 专利责任也随之回到作出选择的一方。这与 D-12 的分层思路一致。
>
> 仍需定的是 **H.264 落在哪一档**（见 §7）——它的风险等级（🟡）明显低于
> HEVC（🔴），且 2027–2030 基础专利陆续到期，可能值得单独对待。



***

## 3. 许可证：裁剪后仍是 LGPL，且要保留可替换性

`--disable-gpl` 且不引入 x264/x265 时，FFmpeg 为 LGPL-2.1+。\["[https://ffmpeg.org/legal.html](https://ffmpeg.org/legal.html)"]

已实测确认 BtbN 的 LGPL 构建**自带 libwebp**（`--enable-libwebp` 为有，

`--enable-gpl` 为无），omy 需要的编码能力一个不缺。

但**静态链接 LGPL 库有额外义务**：LGPL-2.1 §6 要求最终用户能够替换该库。

静态链接时通常需提供目标文件或等效手段。10 号文档 §2.3 已为此列了各平台落地方式，

并在 §2.4 把 "LGPL 静态链接的合规问题" 列为 iOS 不带 FFmpeg 的理由之一。

**两个可选姿势**：



* **A. 静态裁剪 + 子进程调用**（推荐）。omy 通过管道调用独立的 `ffmpeg`/`ffprobe`

  可执行文件，不与 libav\* 链接。这属于聚合而非派生，`omy-media` 自身不受 LGPL

  传染；我们分发的是未修改的 FFmpeg 二进制，附 LGPL 全文 + 构建脚本 + 源码地址

  即可。**当前架构就是这样，不需要改代码。**

* **B. 动态链接 libav\***。要改造成 FFI，还要处理 LGPL 的可替换性，收益是省掉

  一次进程启动（几十毫秒）。不值得。

顺带修正：10 号文档 §2 目前写的是 "omy-media **动态链接** FFmpeg"，与实现

（纯子进程，`Cargo.lock` 里没有任何 ffmpeg 绑定）不符。该文档记录的是当初的打算，

按 AGENTS.md「以实测为准」应予订正。



***

## 4. 构建与分发

### 4.1 平台矩阵与构建链

**先回答"MSVC 够不够"：不够。** FFmpeg 官方文档写得很明确——用 MSVC 构建
**仍然需要 MSYS2 和 NASM**，因为 `configure` 是 shell 脚本、构建系统是 GNU
make，MSVC 只是被当作编译器调用（`./configure --toolchain=msvc`），
还得先从 VS 命令提示符里跑 `msys2_shell.cmd -use-full-path`。["https://ffmpeg.org/platform.html"]

而且**实践中一般不这么做**。参考生产级的
[serversideup/ffmpeg-lgpl-builds](https://github.com/serversideup/ffmpeg-lgpl-builds)：
它的产物三元组叫 `x86_64-pc-windows-msvc`，但实际是**用 MSYS2 里的
mingw-w64 gcc 构建**的——README 明说"msvc 只是消费方的命名约定，产出的 PE
可执行文件与调用方工具链无关"。

这对 omy 尤其成立：我们要的是**独立的 `ffmpeg.exe` / `ffprobe.exe` 子进程**，
不与 Rust 代码链接，所以根本不存在 CRT 兼容问题，没有任何理由折腾 MSVC 路线。

**但 Windows 上也不是非装 MSYS2 不可。** 已实测通过 osdk 的 `conda:` backend
拿到全套工具链，`configure` 正常走完（退出码 0，`License: LGPL version 2.1 or
later`），全程没有安装 MSYS2。详见 §4.2。

| 平台 | 构建链 | 谁提供 |
|---|---|---|
| win-x64 | mingw-w64 gcc + make + nasm + coreutils | **osdk `conda:`（已实测）**，或 `msys2/setup-msys2@v2` |
| linux-x64 / arm64 | gcc + make + nasm（x86 才需要） | `apt-get` |
| mac-arm64 | Xcode CLT（clang + make） | runner 自带 |
| mac-x64 | 同上 + nasm，从 arm64 交叉编译 | `brew install nasm` |
| Android | NDK 交叉编译 | **建议不做，见下** |

各平台的具体依赖：

```yaml
# Windows 方案 A：osdk（本地开发推荐，见 §4.2 的实测结论）
# 方案 B：MSYS2，runner 预装但不在 PATH 里
- uses: msys2/setup-msys2@v2
  with:
    msystem: MINGW64
    update: true
    install: >-
      base-devel mingw-w64-x86_64-toolchain
      mingw-w64-x86_64-pkgconf mingw-w64-x86_64-nasm
      git diffutils tar
# 之后所有构建步骤加 shell: msys2 {0}

# Linux
- run: apt-get install -y --no-install-recommends build-essential nasm pkg-config git

# macOS：Xcode CLT 自带 clang 与 make；官方文档指出 x86 需要 nasm 来编汇编优化
- run: brew list nasm >/dev/null 2>&1 || brew install nasm
```

**关于 macOS 的两个坑：**

1. **GitHub 免费 runner 现在只有 Apple Silicon**（`macos-latest`/`macos-14`/
   `macos-15` 均为 arm64），Intel 版要用 `macos-15-intel` 这类单独标签。["https://docs.github.com/en/actions/reference/runners/github-hosted-runners"]
   要出 x86_64 产物，要么用 Intel runner，要么在 arm64 上交叉编译
   （参考项目走的是后者，`--enable-cross-compile --arch=x86_64`）。
   两个架构也可以用 `lipo` 合成 universal binary，省一份分发。
2. **签名与公证**。参考项目**没有做**签名——它面向服务端 Docker 场景。
   但 omy 是分发给终端用户的桌面应用，未签名的可执行文件在 macOS 上会被
   Gatekeeper 拦下。内置的 `ffmpeg`/`ffprobe` 需要跟 omy 主程序一起
   codesign + 公证（hardened runtime），这是内置方案在 macOS 上的额外成本，
   按需下载方案则会把这个问题转嫁给用户（他们得自己解除隔离属性）。

**Android 仍建议不带**：10 号文档 §4.3 已记录 Google Play 自 2025-11 起强制
16 KB 内存页对齐，社区 fork `ffmpegkit-maintained` 需自行验证；FFmpegKit 已于
2025 年 4 月退役。与现状及 D-24 保持一致。**iOS 维持不带**（D-24 已定）。

### 4.2 Windows 不装 MSYS2 也能构建（已实测跑通）

本项目已经在用 osdk 管理工具链（见根目录 `osdk.toml`）。它的 `conda:` backend
能提供 FFmpeg 构建所需的全部东西，**不需要安装 MSYS2**。

2026-09-12 在本机完整验证过一遍：configure 退出码 0，make 退出码 0，
产出的 `ffmpeg.exe` / `ffprobe.exe` 能直接运行，并跑通 omy 的三条真实路径。

**所需的包**（写进 `osdk.toml` 后进入该目录即自动激活）：

```toml
[tools]
"conda:gcc_win-64" = "16.2.0"       # mingw-w64 交叉编译器 + binutils
"conda:nasm" = "2.16.3"             # x86 SIMD 汇编
"conda:cmake" = "4.4.3"             # 编 libwebp / zlib 用
"conda:ninja" = "1.13.2"
# POSIX 环境：只 pin m2-base 一个包，其余用 with 装进同一 prefix
"conda:m2-base" = { version = "2022.6.1", with = "m2-make,m2-diffutils,m2-pkg-config" }
```

**为什么 m2 系列必须用 `with` 而不是每个包各 pin 一行**（这是最关键的一条，
起初按单包写，卡了很久）：

conda 的每个 tool 各占一个独立 prefix，而每个 `m2-*` 包都自带一份
`msys-2.0.dll`。Windows 按**路径**而非内容实例化 DLL，所以 8 个单包在同一
进程树里就是 8 套互不相识的 msys 运行时——每个 `sh.exe` 按自己 DLL 的位置推导
POSIX 根，于是 `m2-bash` 的 sh 眼中 `/usr/bin` 只有它自己的文件，configure 里
`tr` / `head` / `awk` 全部 `command not found`；同时 6 个包都提供 `bash`，
shim 因归属不唯一而**拒绝路由** `bash`。

`with` 让主包与附加包一次联合求解、装进同一个 prefix。实测 msys DLL 从 8 份
收敛到 1 份，`bash` 歧义随之消失，30 个命令全部可裸调。
`m2-base` 是元包，自带 sh/bash/coreutils/grep/sed/gawk/findutils，但**不含
make 与 pkg-config**，两者必须由 `with` 补上。

元包默认不生成任何 shim（否则 msys 版 `ls`/`test`/`sort` 会盖住 Windows 同名
命令），要用 per-tool `expose` 把需要的命令取回来。**不要改用全局
`shims.include`**：那是全体工具的白名单，一旦设置，未列出的 cargo/rustc/java
会全部失去 shim。

**必须踩对的坑**（都卡过，且报错信息具有误导性）：

1. **CRT 路径差一层 `usr`**。conda 的 mingw 编译器按 sysroot 布局打包，
   `crt2.o` 在 `<sysroot>/usr/lib`，而 gcc 默认搜 `<sysroot>/lib`。
   不处理就会得到 `cannot find crt2.o`，看起来像"包没装全"，其实文件就在那里。
   **`--sysroot=` 不管用**（实测仍失败），要用 `-B<sysroot>/usr/lib`。

2. **FFmpeg 的 configure 不读环境变量 `CC`**。只 `export CC=...` 的话它照样去
   调裸 `gcc`，报 `gcc: command not found`。必须用 `--cc=` 显式传，
   同时给 `--target-os=mingw32 --arch=x86_64`，否则还会有
   `Unknown C compiler` 导致选不出正确的 CFLAGS。

3. **binutils 只有带前缀的名字**。`gcc_win-64` 发布的是
   `x86_64-w64-mingw32-nm` 等，没有裸名 `nm`。configure 会调裸 `nm`，
   缺了**只是静默降级**不报错，所以要用 `--nm=` 等显式指定。

4. **构建脚本内不要调 `osdk`**。嵌套的 osdk 会尝试交互提示并卡在等 stdin 上
   （实测挂了 18 分钟只烧掉 4.8 CPU 秒，看起来像死锁）。路径应由外层算好传入。

5. **msys 的 `tar` 不能吃 Windows 路径**。`tar -xzf C:\...` 会把 `C:` 当成远程
   主机，报 `Cannot connect to C: resolve failed`。要用 `/c/...` 形式。

**三个外部库都得自己编，不能用 conda 的现成包**：

| 库 | 为什么不能直接用 conda 包 | 自编产物 |
|---|---|---|
| libwebp | win-64 包只给 MSVC 的 `.lib`，虽然 mingw 能链接，但产物会依赖 `libwebp.dll`，而该 DLL 又依赖 `VCRUNTIME140.dll` —— 等于要随产物分发 MSVC 运行时 | `libwebp.a` 1.02 MB |
| zlib | 同上；且 FFmpeg 的 PNG 解码器依赖它 | `libz.a` 130 KB |

zlib 的 cmake 产出名为 `libzlibstatic.a`，而 FFmpeg 按 `-lz` 查找，
需要复制一份为 `libz.a`。

实测可用的调用方式（配好 `osdk.toml` 后不再需要 `osdk exec` 包装）：

```bash
SYSROOT=$(osdk -q where conda:gcc_win-64)/Library/x86_64-w64-mingw32/sysroot
export PKG_CONFIG_PATH="$WEBP/lib/pkgconfig:$ZLIB/share/pkgconfig"

"${SRC}/configure" \
    --cc=x86_64-w64-mingw32-gcc \
    --nm=x86_64-w64-mingw32-nm \
    --ar=x86_64-w64-mingw32-ar \
    --ranlib=x86_64-w64-mingw32-ranlib \
    --strip=x86_64-w64-mingw32-strip \
    --windres=x86_64-w64-mingw32-windres \
    --target-os=mingw32 --arch=x86_64 \
    --extra-cflags="-B${SYSROOT}/usr/lib -I${SYSROOT}/usr/include -I${WEBP}/include -I${ZLIB}/include -O2" \
    --extra-ldflags="-B${SYSROOT}/usr/lib -L${WEBP}/lib -L${ZLIB}/lib -static" \
    --pkg-config-flags=--static \
    --disable-everything --disable-doc --disable-shared --enable-static \
    --enable-small --disable-network --disable-autodetect \
    --enable-libwebp --enable-zlib \
    ...（组件清单见 §4.5）
```

**产物验证结果**（这些是接受标准，不只是"编过了"）：

| 检查项 | 结果 |
|---|---|
| `ffmpeg.exe` / `ffprobe.exe` 体积 | 6.25 MB / 6.15 MB，合计 **12.40 MB** |
| 依赖的 DLL | 只有 `KERNEL32` / `SHELL32` / `bcrypt` / `api-ms-win-crt-*`，**无 mingw / libwebp / MSVC 运行时** |
| `License:` | `LGPL version 2.1 or later` |
| **`cargo test -p omy-media`** | **123 项全过**（用 `OMY_FFMPEG` 指向裁剪版） |
| `cargo test --workspace` | 全绿 |

最后两行是真正的接受标准：前几轮"手工验证四条路径都通过"之后，
真实测试仍抓出三个缺失组件（见下），所以**必须跑项目自己的测试**。

依赖 DLL 那一条也关键：mingw 构建有时会拖上 `libgcc_s_seh-1.dll` 之类的
运行时 DLL，那样就不能只拷两个 exe 了。实测确认**没有**，UCRT 是系统自带的。

#### 4.2.1 三个只有真实测试才抓得到的缺失组件

手工用文件跑 ffmpeg 命令全部成功，但 omy 的测试仍失败。逐个查出来的原因，
都是"configure 接受了参数但没启用，且不报错"：

| 缺的组件 | 症状 | 为什么手工验证发现不了 |
|---|---|---|
| `rawvideo` demuxer | `Unknown input format: 'rawvideo'` | 手工测试用的是视频文件输入，而 omy 的图片路径是把 RGB 裸数据喂进管道 |
| `fd` protocol | `Protocol not found`，提示 `Did you mean file:fd:?` | FFmpeg 7.x 把 `-i -` 解析成 `fd:` 协议，**光有 `pipe` 不够**；用文件输出时不经过这条路径 |
| `image_png_pipe` 等 demuxer | 管道探测 PNG 报 `Invalid data found`；对**文件**探测却正常返回 `image2` | omy 只走管道探测。而且真实名字带 `image_` 前缀（`image_png_pipe`），写成 `png_pipe` configure 不报错也不启用 |

最后一条的后果最隐蔽：`is_still_image` 靠容器名判断（要求 `image2` 或以
`_pipe` 结尾），管道探测失败 → 拿不到容器名 → 图片走不进图片分支 →
`thumbnail=None` 且 `warnings=[]`，**没有任何错误信息**。

另外 zlib 起初被我判断为"omy 不需要"，理由是缩略图走 rawvideo 不经过 PNG
解码器。这个判断是错的：ffprobe 读 PNG 的宽高**需要** PNG 解码器，
缺了会返回 `width=0 height=0`，omy 因此判定不出静态图片。

**这对方案意味着什么**：Windows 本地开发不再需要 MSYS2，`osdk.toml` 一写、
进目录即用，新同学不必手工配环境。CI 上两条路都可行——继续用
`msys2/setup-msys2@v2`，或装 osdk 复用同一份 `osdk.toml`
（后者的好处是本地与 CI 严格同构，版本由 `osdk.lock` 锁定）。

### 4.3 三档产物怎么组织

目标是三种 release 产物：**无 FFmpeg / 内置免版税档 / 全部内置档**。

关键决定：**三档共用同一套 FFmpeg 构建产物，只是打包时放不放、放哪一份。**
不要为三档各写一套构建脚本——那样"改了 A 档忘了改 B 档"是迟早的事。

```
构建矩阵（在 omy-ffmpeg-minimal 仓库里）
  profile ∈ { royalty-free, full }     # 两种 configure 配方
  target  ∈ { win-x64, linux-x64, linux-arm64, mac-arm64, mac-x64 }
  → 10 份产物，每份含 ffmpeg + ffprobe + COPYING.LGPLv2.1 + SOURCE.txt

omy 主仓打包时
  omy-<ver>-<target>.zip              # 无 FFmpeg，靠 candidate_dirs 探测
  omy-<ver>-<target>-lite.zip         # 放 royalty-free 那份
  omy-<ver>-<target>-full.zip         # 放 full 那份
```

**`omy doctor` 必须能区分这三种情况**，否则用户报错时根本说不清他装的是哪档。
建议输出里明确写出 FFmpeg 的来源（内置 / 用户自备）与档位，以及**缺哪些能力**
——比如 lite 档要直说"H.264 视频无缩略图，因为不含该解码器"，而不是让用户
以为功能坏了。

参考项目的做法值得抄：**构建后校验产物**，确认没有启用禁用的 flag
（`--enable-gpl` / `--enable-nonfree` / `--enable-version3`），并检查动态链接
只指向系统库。这条对 lite 档尤其重要——免版税档一旦不小心带进 H.264 解码器，
整个专利论证就失效了，而这种错误肉眼看不出来。

### 4.4 建议的仓库结构

参照 `genesys-ffmpeg-minimal` 与 `ffmpeg-lgpl-builds` 的共同做法，
**放独立仓库**而非塞进 omy 主仓：

```
omy-ffmpeg-minimal/
  VERSION                       # 第 1 行 ffmpeg 版本，第 2 行配方修订号
  configure-royalty-free.sh     # 免版税档配方
  configure-full.sh             # 全量档配方
  scripts/build-{windows,linux,macos}.sh
  scripts/verify.sh             # 校验 flag 与链接，见上
  scripts/smoke-test.sh         # 跑 omy 的四条真实路径
  .github/workflows/build.yml
```

主仓通过 tag + SHA256 固定版本。理由：构建慢、平台相关，不该拖累主仓 CI；
而且 FFmpeg 版本的升级节奏与 omy 自身完全不同。

**源码获取要锁 SHA256**。参考项目的做法是在每个平台的构建脚本里都写一份
上游 tarball 的 SHA256（防御性冗余），并在 release notes 里记录产物的 SHA256。



参照 `genesys-ffmpeg-minimal` 的做法，**放独立仓库**而非塞进 omy 主仓：



```
omy-ffmpeg-minimal/

&#x20; configure-flags.sh      # 共享的 ./configure 配方

&#x20; VERSION                 # 第 1 行 ffmpeg 版本，第 2 行配方修订号

&#x20; scripts/build-\*.sh      # 各平台构建

&#x20; scripts/smoke-test.sh   # 冒烟测试

&#x20; .github/workflows/      # CI + Release
```

主仓通过 tag + SHA256 固定版本。理由与参考项目相同：构建慢、平台相关，

不该拖累主仓 CI。

### 4.5 configure 配方（已实测，omy 全部测试通过）

下面这份是本机真实跑通的配方，不是草案。它让 `cargo test -p omy-media`
的 123 项全部通过。

```bash
"${SRC}/configure" \
  --cc=x86_64-w64-mingw32-gcc \
  --nm=x86_64-w64-mingw32-nm \
  --ar=x86_64-w64-mingw32-ar \
  --ranlib=x86_64-w64-mingw32-ranlib \
  --strip=x86_64-w64-mingw32-strip \
  --windres=x86_64-w64-mingw32-windres \
  --target-os=mingw32 --arch=x86_64 \
  --extra-cflags="-B${SYSROOT}/usr/lib -I${SYSROOT}/usr/include -I${WEBP}/include -I${ZLIB}/include -O2" \
  --extra-ldflags="-B${SYSROOT}/usr/lib -L${WEBP}/lib -L${ZLIB}/lib -static" \
  --pkg-config-flags=--static \
  \
  --disable-everything --disable-doc --disable-shared --enable-static \
  --enable-small --disable-network --disable-autodetect \
  \
  `# 外部库：libwebp 编缩略图，zlib 供 PNG 解码器` \
  --enable-libwebp --enable-zlib \
  \
  `# 解码器。rawvideo 是图片缩略图路径的输入格式，漏掉会 Unknown input format` \
  --enable-decoder=h264,hevc,vp8,vp9,av1,mjpeg,png,webp,rawvideo \
  --enable-decoder=aac,mp3,opus,vorbis,flac,pcm_s16le \
  --enable-decoder=subrip,ass,webvtt,mov_text \
  \
  `# 编码器。remux 的字幕不是纯 copy，mov_text 必需` \
  --enable-encoder=libwebp,mov_text \
  \
  `# demuxer。pipe 类的真实名字带 image_ 前缀，写成 png_pipe 不报错也不生效` \
  --enable-demuxer=mov,matroska,avi,mpegts,flv,image2,image2pipe \
  --enable-demuxer=image_png_pipe,image_jpeg_pipe,image_webp_pipe \
  --enable-demuxer=image_bmp_pipe,image_gif_pipe \
  --enable-demuxer=mjpeg,webp,wav,mp3,flac,ogg,aac,srt,ass,webvtt,rawvideo \
  \
  --enable-muxer=mov,mp4,matroska,webp,image2,rawvideo \
  \
  `# parser/bsf：不加会出现"能 demux 但 copy 出来播不了"` \
  --enable-parser=h264,hevc,vp8,vp9,av1,aac,mpegaudio,flac,opus,vorbis,png,webp \
  --enable-bsf=extract_extradata,h264_mp4toannexb,hevc_mp4toannexb \
  \
  `# 协议：FFmpeg 7.x 把 -i - 解析成 fd:，光有 pipe 不够` \
  --enable-protocol=file,pipe,fd \
  \
  --enable-filter=scale,format,null,anull,copy,thumbnail,select,fps,transpose,crop \
  --enable-swscale --enable-avfilter
```

几个容易踩的点，前四条都是实测被抓出来的（详见 §4.2.1）：

* `fd`**&#32;协议不能漏**。FFmpeg 7.x 把 `-i -` 解析成 `fd:` 协议，只开 `pipe`
  会报 `Protocol not found`。omy 全程走管道，这条是硬要求。

* **pipe 类 demuxer 的真实名字带&#32;**`image_`**&#32;前缀**。写成 `png_pipe`
  configure 既不报错也不启用，结果 ffprobe 无法从管道识别 PNG。
  而 `prepare.rs` 的静态图判据依赖容器名（`image2` 或以 `_pipe` 结尾），
  拿不到容器名时图片会被误判、`thumbnail=None` 且 `warnings=[]`——**无任何报错**。

* `rawvideo`**&#32;要同时开 decoder 和 demuxer**。图片缩略图是把 RGB 裸数据
  喂进管道，缺了报 `Unknown input format: 'rawvideo'`。

* `zlib`**&#32;是必需的，不是可选**。虽然缩略图走 rawvideo 不经过 PNG 解码器，
  但 ffprobe 读 PNG 宽高需要它，缺了返回 `width=0 height=0`。

* **parser 与 bsf 必须显式开**。只开 demuxer/muxer 时，`-c copy` 会因为
  拿不到 extradata 而产出"能生成但播不了"的 MP4——这类问题不报错，
  只有真正播放才发现。`remux.rs` 的 `looks_like_mp4` 检查也拦不住。

* `--disable-network` 对 omy 安全：全程走管道，不需要网络协议，
  关掉能省体积也能减小攻击面。

* `--enable-small` 以速度换体积，对 omy 这种非实时场景合适。


### 4.6 为什么不用 ffmpeg-next（社区 Rust 绑定）

**结论：不适用，而且它解决不了任何一个我们关心的问题。**

`ffmpeg-next` 是 **绑定，不是 FFmpeg 本身**。实测 `ffmpeg-sys-next` 9.0.0 的
`.crate` 包只有 **27,547 字节（0.03 MB）**，里面是 `build.rs`（68 KB）加两个
头文件，**不含任何 FFmpeg 代码**。所以"它有多大"这个问题的答案是：
crate 本身几乎没有体积，真正的体积仍来自你得自己提供的 FFmpeg。

它有两种工作模式，都不省事：

1. **默认：链接系统已装的 FFmpeg。** 需要 `pkg-config` + `clang`（bindgen 要
   libclang）+ `libav*-dev` 开发头文件；Windows 上还要装 LLVM、设 `LIBCLANG_PATH`、
   用 vcpkg 或下载 `full_build-shared` 并设 `FFMPEG_DIR`。["https://github.com/zmwangx/rust-ffmpeg/wiki/Notes-on-building"]
   —— 这比现在"用户装个 ffmpeg 就行"的要求高得多。

2. **`build` 特性：自己编译 FFmpeg。** 读 `build.rs` 确认，它做的是
   `git clone --depth=1 -b release/<ver> https://github.com/FFmpeg/FFmpeg`
   然后跑 `configure` + `make`（Windows 上还要找 `sh.exe`，找不到直接报
   "Failed to find 'sh.exe', which is required for building FFmpeg"）。
   **也就是说它并没有绕开构建链，只是把构建塞进了 cargo build。**

对 omy 来说还有两个硬伤：

- **`build.rs:543` 写死了 `--disable-programs`**，即不产出 `ffmpeg`/`ffprobe`
  可执行文件。而 omy 全部四条路径都是子进程调用这两个程序。用它就必须把
  `omy-media` 整个改写成 FFI。
- **它的 feature 体系无法表达我们要的裁剪**。`build.rs` 里只有
  `--enable-decoder=*_mediacodec`（Android 硬解）这类零星开关，**没有
  `--disable-everything`**，也没有逐个 demuxer/parser/bsf 的开关。想裁到
  12–18 MB 还是得自己写 configure 配方——那就回到 §4.5 了。

此外改成 FFI 会**丢掉进程隔离**。`security.md` 明确要求 FFmpeg 处理不可信输入时
只在独立子进程里跑、不接触密钥；链接进主进程后，一个解码器漏洞就直接落在
持有密钥的地址空间里。这个代价远大于省下的几十毫秒进程启动。

> 顺带：`ffmpeg-next` 自身是 WTFPL，但它链接的 FFmpeg 仍是 LGPL/GPL，
> 许可证义务不会因为换了绑定而消失。

### 4.7 按需下载：不是只能下 100 多 MB

先纠正一个印象：**官方预编译包大，是因为它把所有东西都塞进去了，不是因为
FFmpeg 本身必须这么大。** 实测 BtbN win64-lgpl：

| 形态 | 压缩包 | 解压后 | omy 实际需要 |
|---|---|---|---|
| win64-lgpl（static） | 164.1 MB | bin 385.6 MB | ffmpeg+ffprobe **254.8 MB** |
| win64-lgpl-shared | 73.4 MB | bin 166.4 MB | 两个 exe + 7 个 DLL **148.6 MB** |

static 反而更大，因为每个 exe 都把全部库链进去了一份（三个 exe 各 127 MB）。
另外 `ffplay.exe`（17.9 MB，shared 版）和 `doc/`（11.2 MB）对 omy 完全无用。

**但 macOS 那边给出了完全不同的数字**：evermeet.cx 的 macOS 静态构建，
`ffmpeg` 9.0.1 只有 **25.0 MB**、`ffprobe` **24.9 MB**。同样是"完整"构建，
比 BtbN 小 6 倍——差别在于启用了多少外部库。这说明 **100+ MB 不是下限，
而是某一家的打包口径。**

所以按需下载有三档可选：

1. **下官方完整包**（73–164 MB）：省事，但用户要等很久。
2. **下 evermeet 口径的精简完整包**（约 25 MB/平台）：需要我们自己为
   win/linux 产出对应口径。
3. **下我们自己的裁剪包**（12–18 MB）：与内置用同一套产物，只是改成按需拉取。

**2 和 3 都要求我们自建构建链**——这与"内置"的前置条件完全相同。
换句话说：**只要决定了要裁剪，内置与按需下载的成本差异很小**，区别只在
安装包里放不放、以及要不要写下载器（校验 SHA256、断点续传、镜像回退）。

### 4.8 下载源

| 平台 | 源 | 体积 | 备注 |
|---|---|---|---|
| win64 / winarm64 | [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds) | 164.1 / 113.2 MB | GitHub Release，有 `latest` 与 `n9.0` 固定 tag |
| linux64 / linuxarm64 | 同上 | 131.9 / 112.1 MB | `.tar.xz` |
| macOS x86_64 | [evermeet.cx](https://evermeet.cx/ffmpeg/) | 25.0 MB | 有 JSON API 可查版本与校验和 |
| macOS arm64 | [osxexperts.net](https://www.osxexperts.net) | — | 已确认可达（HTTP 200），体积待核 |

**BtbN 没有 macOS 构建**，这是最大的缺口——要么走 evermeet + osxexperts 两个
不同的源（两家的构建口径、签名方式、更新节奏都不一样），要么自己构建。
考虑到 macOS 还有 Gatekeeper 公证问题，自建反而更可控。

三个源都是社区维护、非 FFmpeg 官方。omy 若依赖它们做运行时下载，需要：
锁定版本与 SHA256（不能跟 `latest` 漂）、准备镜像或回退源、以及处理源
消失的情况。这些维护成本本身就是"不如自建"的论据之一。

### 4.9 内置与按需下载可以并存

裁剪到 12–18 MB 后可以直接内置。但仍建议**保留按需下载作为补充**：
用户想要 H.264/HEVC 抽帧时，引导其下载完整 LGPL 构建到
`<应用目录>/ffmpeg/bin/`。

**这条路径零代码改动**——已实测验证：把 FFmpeg 放进该目录后，隐藏系统 ffmpeg，
`omy doctor` 仍准确报告"ffprobe 与 ffmpeg 均可用"。`ffprobe.rs:91` 的
`candidate_dirs()` 本来就包含这个位置。

这样内置的那份只需覆盖免版税编码（体积小、专利干净），而想要完整能力的用户
自行下载——专利责任也随之回到用户侧，与 D-12 的分层思路一致。



***

## 5. 一个必须先修的缺陷（已实测，本轮已修）

调研中发现 `thumbnail.rs` 用了 `-quality`，**在较新的 FFmpeg 上该参数被静默
忽略**：

```
BtbN master N-126497   -quality 0/10/40/75/90/100  ->  全是 5,300 字节
BtbN master N-126497   -q:v 10 -> 23,798     -q:v 90 -> 64,822
gyan 9.0.1             -quality 40 -> 38,032   -quality 90 -> 64,822
```

两个构建的 `-h encoder=libwebp` **都声明支持** `-quality`，所以查帮助发现不了。

后果：质量参数失效 -> 缩略图固定按默认质量编码 -> `SIZE_LIMIT` 的"超限就降
质量重编"逻辑整个失灵，且不报错。仅 `lower_quality_yields_smaller_output`
一条测试能发现。

**已改为 `-q:v`**（图片与视频两条路径各一处）。在自建的裁剪版上复测确认分档
生效：`-q:v 10` -> 21,392 字节，`-q:v 90` -> 61,724 字节。

修它的时机很关键：一旦把某个版本的 FFmpeg 固定进安装包，这类版本差异就从
"用户环境问题"变成"我们发出去的缺陷"。


## 6. 建议的推进顺序

已定方向：**自建构建链，出三档 release 产物**（无 FFmpeg / 内置免版税档 /
全部内置档）。三档并存也顺带化解了 §2 的专利两难——免版税档默认分发、
全量档由用户显式选择。

1. **修&#x20;**`-quality`**&#x20;→&#x20;**`-q:v`（§5）。独立小改动，与本方案解耦，
   但必须在固定 FFmpeg 版本进安装包之前完成。

2. **建&#x20;**`omy-ffmpeg-minimal`**&#x20;仓库，先只做 win-x64 打通全流程**：
   两档 configure 配方 + 构建 + 校验 + 冒烟测试。本地无法构建
   （缺 make、WSL 无 root、无 Docker），这一步必须在 CI 上做。
   拿到真实体积后回头修订 §1.3 的估算——**如果免版税档超出预期，
   现在就该重新权衡，而不是等五个平台都铺完**。

3. **冒烟测试覆盖 omy 的四条真实路径**（§1.1），而不只是 `ffmpeg -version`。
   尤其要覆盖 §4.5 提到的 parser/bsf 缺失场景：那类缺陷不报错，
   只表现为"产物生成了但播不了"。

4. **铺开其余平台**：linux-x64 → mac-arm64 → linux-arm64 → mac-x64。
   macOS 放后面是因为还要解决签名与公证（§4.1）。

5. **主仓接入**：打包脚本产出三档 + `omy doctor` 明确报告档位与缺失能力。

6. 同步订正 10 号文档的 "动态链接" 表述，并更新 `site/` 中英两份的 FFmpeg 说明。

***

## 7. 仍待决策

1. **免版税档到底放哪些解码器**。VP8/VP9/AV1/Opus/Vorbis/FLAC/WebP 是安全的；
   争议在 **H.264**——它覆盖面最广（不含则多数视频没缩略图），
   但基础专利要到 2027–2030 才陆续到期（10 号文档 §3.1）。
   三档方案下有两种取法：H.264 归入全量档（更保守），或放进免版税档
   并在 README 声明（更实用）。**这条建议在拿到真实体积后再定**——
   如果两档体积差不大，分档的意义就不大。

2. **macOS 签名**。内置 FFmpeg 意味着 omy 的签名流程要覆盖这两个可执行文件，
   需要 Apple 开发者账号。若暂时没有，macOS 可能只能先出"无 FFmpeg 版"。

3. **是否同时保留按需下载**（§4.9）。它零代码改动，可以作为 lite 档用户
   升级到完整能力的路径，但要维护下载器与源的可用性。



***

## 附：本文档的证据来源

**实测得出**（本机）：

* **用 osdk conda 工具链完成真实裁剪构建**（FFmpeg 7.1.1，win-x64，
  gcc 16.2.0）：configure 与 make 均退出 0，产物 11.08 MB，
  只依赖系统 DLL，三条真实路径（探测 / 抽帧缩放 / `-c copy`）全部通过

* conda mingw 编译器的 CRT 在 `<sysroot>/usr/lib` 而 gcc 默认搜
  `<sysroot>/lib`，须用 `-B` 指定；`--sysroot=` 无效

* FFmpeg configure 不读环境变量 `CC`，必须 `--cc=` 显式传



* LGPL-shared 构建各 DLL 体积与最小可用集合（逐个重命名排除，七个全必需，148.55 MB）

* LGPL 构建自带 libwebp、无 `--enable-gpl`

* remux 的 `Stream mapping`：视频 / 音频 copy、字幕 subrip→mov\_text 真转码

* `-quality` 在 BtbN master 上失效、`-q:v` 有效

* 放入 `<应用目录>/ffmpeg/bin/` 后 `omy doctor` 能找到

* 完整 LGPL 构建有 547 个解码器 / 238 个编码器；omy 需要的 13 个解码器全都在

* `ffmpeg-sys-next` 9.0.0 的 `.crate` 仅 27,547 字节，不含 FFmpeg 代码；
  其 `build.rs` 的 `build` 特性是 `git clone` FFmpeg 再跑 configure+make，
  且第 543 行写死 `--disable-programs`（不产出 ffmpeg/ffprobe 可执行文件）

* BtbN win64-lgpl static：压缩包 164.1 MB，解压 bin 385.6 MB，
  三个 exe 各约 127 MB；shared 版两个 exe + 7 个 DLL 合计 148.6 MB

* evermeet.cx 的 macOS 构建：ffmpeg 9.0.1 为 25.0 MB、ffprobe 24.9 MB
  （同为"完整"构建却比 BtbN 小 6 倍，说明 100+ MB 不是下限）

* BtbN 的 release 资产里**没有 macOS**（仅 win64/winarm64/linux64/linuxarm64）

**引自官方文档 / 生产项目**：

* FFmpeg 官方：用 MSVC 构建仍需 MSYS2 + NASM，且要从 VS 命令提示符里跑
  `msys2_shell.cmd -use-full-path`；macOS on x86 需要 nasm
  （[platform.html](https://ffmpeg.org/platform.html)）

* `serversideup/ffmpeg-lgpl-builds`：产物三元组虽叫 `x86_64-pc-windows-msvc`，
  实际用 MSYS2 MINGW64 的 mingw-w64 gcc 构建；其 CI 的 `pacman` 包清单
  可直接参考；它做构建后 flag 校验与链接检查，但**不做签名**

* GitHub 免费 macOS runner 现为 Apple Silicon（`macos-latest`/`14`/`15` 均 arm64），
  Intel 需 `macos-15-intel` 这类标签
  （[GitHub Docs](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)）

* GitHub Windows runner 预装 MSYS2 于 `C:\msys64` 但不在 PATH 中

**估算未实测**：§1.3 分模块的逐项体积。但**合计值已被实测取代**
（11.08 MB，见 §1.3 的订正框与 §4.2）。仍未实测的是加上 libwebp 后的体积，
以及 linux / macOS 三平台的数字。

**引自既有文档**：专利矩阵与 D-12 策略（`10-licensing-and-patents.md`）、

Android 16 KB 对齐与 FFmpegKit 退役（同上 §4.3、§2.4）。