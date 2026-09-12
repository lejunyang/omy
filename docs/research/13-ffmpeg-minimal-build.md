# FFmpeg 裁剪内置方案

> 对应决策 **D-11**（FFmpeg 追求最大兼容）、**D-23**（分层授权）、
> **D-12**（HEIC 分层策略）。
>
> 状态：**方案已定方向，实施细节待细化**。已确定走自建构建链、出三档 release
> 产物（无 FFmpeg / 内置免版税档 / 全部内置档）。本文档不描述已实现的行为。
>
> 撰写日期：2026-09-12

## 0. 一句话结论

**技术上可行，但 "5–15 MB" 这个数字对 omy 不成立。** 实测与组件分析表明，
能覆盖 omy 全部功能的裁剪构建落在 **20–35 MB**（单平台、静态、strip 后）；
只留免版税编码约 **12–18 MB**；若连视频抽帧一起砍掉可压到 **8–12 MB**。

**专利是这里的主要约束**：内置解码器会把 H.264/HEVC/AAC 的专利责任从用户
转移到我们身上，而 10 号文档已确立的 D-12 策略正是"把专利责任转移给
OS 厂商"。**三档产物方案化解了这个两难**——默认发免版税档，完整能力由用户
显式选择（§4.2）。

**构建链方面**：Windows 上 **MSVC 不够**，FFmpeg 官方与生产项目都要 MSYS2；
实践中直接用 MSYS2 的 mingw-w64 gcc 即可，因为我们产出的是独立子进程
可执行文件，不与 Rust 链接（§4.1）。

**唯一未实测的是分组件体积**——本地缺 make、WSL 无 root、无 Docker，
真实数字必须在 CI 上产出。



***

## 1. 为什么 "5–15 MB" 不适用

那个数字来自 `genesys-ffmpeg-minimal`，它的适用范围是

**音频转 MP3**（WAV/FLAC/AIFF/OGG/AAC/M4A → MP3），没有视频解码器、没有

视频滤镜、没有 MP4 muxer。\["[https://github.com/directivegames/genesys-ffmpeg-minimal](https://github.com/directivegames/genesys-ffmpeg-minimal)"]

omy 的需求面要宽得多。下面按代码实测逐条列出。

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

> 这是
>
> **估算不是实测**
>
> ：本机缺 
>
> `make`
>
>  且 WSL 无 root（sudo 需密码）、
> 无 Docker，无法在本地完成 FFmpeg 构建。第一步行动项就是用 CI 产出真实数字。

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

> **三档方案如何化解这个两难**（后续决定，见 §4.2）
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

| 平台 | 构建链 | 谁提供 |
|---|---|---|
| win-x64 | MSYS2 + MINGW64 工具链 + nasm | runner 预装 MSYS2，用 `msys2/setup-msys2@v2` 装包 |
| linux-x64 / arm64 | gcc + make + nasm（x86 才需要） | `apt-get` |
| mac-arm64 | Xcode CLT（clang + make） | runner 自带 |
| mac-x64 | 同上 + nasm，从 arm64 交叉编译 | `brew install nasm` |
| Android | NDK 交叉编译 | **建议不做，见下** |

各平台的具体依赖：

```yaml
# Windows：MSYS2 预装在 runner 上但不在 PATH 里，用官方 action 更省事
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

### 4.2 三档产物怎么组织

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

### 4.3 建议的仓库结构

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

### 4.4 configure 配方草案

下面是**免版税档**（§2 建议的口径）的起点。未经实测，需在 CI 上迭代 ——

`--disable-everything` 之后漏掉任何一个组件都表现为运行期失败而非构建失败。



```
./configure \\

&#x20; \--disable-everything --disable-gpl --disable-nonfree \\

&#x20; \--disable-doc --disable-debug --disable-network \\

&#x20; \--disable-programs --enable-ffmpeg --enable-ffprobe \\

&#x20; \--enable-small \\

&#x20; \\

&#x20; \`# 容器：用户手上可能是任何格式，这部分不能省\` \\

&#x20; \--enable-demuxer=matroska,mov,avi,flv,mpegts,mpegps,asf,ogg,wav,mp3,flac,aac,rawvideo,image2,png\_pipe,mjpeg\_pipe,webp\_pipe \\

&#x20; \--enable-muxer=mp4,webp,rawvideo \\

&#x20; \\

&#x20; \`# 免版税视频解码（H.264/HEVC 见 §2 的专利讨论，此档不含）\` \\

&#x20; \--enable-decoder=vp8,vp9,av1,theora,png,mjpeg,webp,bmp,gif \\

&#x20; \\

&#x20; \`# 音频解码：探测与字幕时间对齐需要\` \\

&#x20; \--enable-decoder=opus,vorbis,flac,mp3,pcm\_s16le,pcm\_s24le \\

&#x20; \\

&#x20; \`# 字幕：remux 的 -c:s mov\_text 需要\` \\

&#x20; \--enable-decoder=subrip,ass,webvtt,text \\

&#x20; \--enable-encoder=mov\_text \\

&#x20; \\

&#x20; \`# 缩略图\` \\

&#x20; \--enable-libwebp --enable-encoder=libwebp \\

&#x20; \--enable-filter=scale,format,null,anull \\

&#x20; \--enable-protocol=pipe,file \\

&#x20; \\

&#x20; \`# parser/bsf：不加会出现"能 demux 但 copy 出来播不了"\` \\

&#x20; \--enable-parser=h264,hevc,vp8,vp9,av1,aac,opus,flac,mpegaudio \\

&#x20; \--enable-bsf=h264\_mp4toannexb,hevc\_mp4toannexb,aac\_adtstoasc,extract\_extradata
```

几个容易踩的点：



* `--disable-network`**&#x20;要慎用**：omy 全程走管道，不需要网络协议，关掉能省体积

  也能减小攻击面。但要确认 `pipe:` 协议不受影响（配方里已显式 enable）。

* **parser 与 bsf 必须显式开**。只开 demuxer/muxer 时，`-c copy` 会因为

  拿不到 extradata 而产出 "能生成但播不了" 的 MP4—— 这类问题不报错，

  只有真正播放才发现。`remux.rs:254` 的 `looks_like_mp4` 检查也拦不住。

* `image2`**&#x20;与&#x20;**`*_pipe`**&#x20;别漏**：`prepare.rs` 的静态图判据依赖 ffprobe 报出的

  `png_pipe`/`mjpeg_pipe`/`image2` 容器名（见 `prepare.rs:405` 的测试）。

  漏掉会让静态图片被误判成视频送去抽帧。

* `--enable-small` 以速度换体积，对 omy 这种非实时场景合适。

### 4.5 为什么不用 ffmpeg-next（社区 Rust 绑定）

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
  12–18 MB 还是得自己写 configure 配方——那就回到 §4.4 了。

此外改成 FFI 会**丢掉进程隔离**。`security.md` 明确要求 FFmpeg 处理不可信输入时
只在独立子进程里跑、不接触密钥；链接进主进程后，一个解码器漏洞就直接落在
持有密钥的地址空间里。这个代价远大于省下的几十毫秒进程启动。

> 顺带：`ffmpeg-next` 自身是 WTFPL，但它链接的 FFmpeg 仍是 LGPL/GPL，
> 许可证义务不会因为换了绑定而消失。

### 4.6 按需下载：不是只能下 100 多 MB

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

### 4.7 下载源

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

### 4.8 内置与按需下载可以并存

裁剪到 12–18 MB 后可以直接内置。但仍建议**保留按需下载作为补充**：
用户想要 H.264/HEVC 抽帧时，引导其下载完整 LGPL 构建到
`<应用目录>/ffmpeg/bin/`。

**这条路径零代码改动**——已实测验证：把 FFmpeg 放进该目录后，隐藏系统 ffmpeg，
`omy doctor` 仍准确报告"ffprobe 与 ffmpeg 均可用"。`ffprobe.rs:91` 的
`candidate_dirs()` 本来就包含这个位置。

这样内置的那份只需覆盖免版税编码（体积小、专利干净），而想要完整能力的用户
自行下载——专利责任也随之回到用户侧，与 D-12 的分层思路一致。



***

## 5. 一个必须先修的缺陷（已实测，未修）

调研中发现 `thumbnail.rs` 的 263 与 410 行用了 `-quality`，

**在较新的 FFmpeg 上该参数被静默忽略**：



```
BtbN master N-126497   -quality 0/10/40/75/90/100  →  全是 5,300 字节

BtbN master N-126497   -q:v 10 → 23,798     -q:v 90 → 64,822

gyan 9.0.1             -quality 40 → 38,032   -quality 90 → 64,822
```

两个构建的 `-h encoder=libwebp` **都声明支持&#x20;**`-quality`，所以查帮助发现不了。

后果：质量参数失效 → 缩略图固定按默认质量编码 → `SIZE_LIMIT` 的 " 超限就降质量

重编 " 逻辑整个失灵，且不报错。仅 `lower_quality_yields_smaller_output` 一条测试

能发现。

**这条必须在内置之前修**（改用 `-q:v`）：一旦把某个版本的 FFmpeg 固定进安装包，

这类版本差异就从 "用户环境问题" 变成 "我们发出去的缺陷"。



***

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
   尤其要覆盖 §4.4 提到的 parser/bsf 缺失场景：那类缺陷不报错，
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

3. **是否同时保留按需下载**（§4.8）。它零代码改动，可以作为 lite 档用户
   升级到完整能力的路径，但要维护下载器与源的可用性。



***

## 附：本文档的证据来源

**实测得出**（本机）：



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

**估算未实测**：§1.3 的分组件体积。本地无法构建 FFmpeg
（缺 `make`；WSL 的 sudo 需密码装不了工具链；无 Docker/Podman）。

**引自既有文档**：专利矩阵与 D-12 策略（`10-licensing-and-patents.md`）、

Android 16 KB 对齐与 FFmpegKit 退役（同上 §4.3、§2.4）。