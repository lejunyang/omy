# FFmpeg 裁剪内置方案

> 对应决策 **D-11**（FFmpeg 追求最大兼容）、**D-23**（分层授权）、**D-12**（HEIC 分层策略）。
> 状态：**调研方案，待评审**。本文档不描述已实现的行为。
> 撰写日期：2026-09-12

## 0. 一句话结论

**技术上可行，但"5–15 MB"这个数字对 omy 不成立。**实测与组件分析表明，能覆盖 omy
全部功能的裁剪构建落在 **20–35 MB**（单平台、静态、strip 后）；若接受功能降级，
可压到 **8–12 MB**。

更关键的是：**内置解码器会把 H.264/HEVC/AAC 的专利责任从用户转移到我们身上**，
而这与 10 号文档已确立的 D-12 策略（"把专利责任转移给 OS 厂商"）直接冲突。
这一条比体积更值得先决策。

---

## 1. 为什么"5–15 MB"不适用

那个数字来自 `genesys-ffmpeg-minimal`，它的适用范围是
**音频转 MP3**（WAV/FLAC/AIFF/OGG/AAC/M4A → MP3），没有视频解码器、没有
视频滤镜、没有 MP4 muxer。["https://github.com/directivegames/genesys-ffmpeg-minimal"]

omy 的需求面要宽得多。下面按代码实测逐条列出。

### 1.1 omy 实际用到的四条路径

| 路径 | 代码位置 | 调用形态 |
|---|---|---|
| 媒体探测 | `probe.rs:487` | `ffprobe -show_format -show_streams` |
| 图片缩略图的 WebP 编码 | `thumbnail.rs:269` | `rawvideo → libwebp` |
| 视频抽帧缩略图 | `thumbnail.rs:416` | `任意容器 → scale → libwebp` |
| P2 转封装 | `remux.rs:235/298` | `任意容器 → -c copy → fMP4` |

**不含任何重编码**：`remux.rs:174` 是 `-c copy`，`tier.rs` 的 P3（需转码）
当前不调用 FFmpeg。D-26 的"转码后加密"尚未实现。

### 1.2 各路径的组件需求（实测）

**转封装：视频/音频确实不需要解码器。** 用 LGPL 构建跑 omy 的真实参数，
`Stream mapping` 显示：

```
Stream #0:0 -> #0:0 (copy)
Stream #0:1 -> #0:1 (copy)
Stream #0:2 -> #0:2 (subrip (srt) -> mov_text (native))
```

前两条是纯 copy。**但第三条是真的转码**——`remux.rs:183` 的 `-c:s mov_text`
要求字幕解码器（subrip/ass/webvtt…）+ mov_text 编码器。加 `-sn` 时这条消失。

**视频抽帧则必须有完整解码器。** `thumbnail.rs:402` 用了 `-vf scale=...`，
要解码真实帧再缩放。用户手上的视频是什么编码就得能解什么——这是体积的主要来源，
也是与那个"音频专用"参考项目的根本差异。

### 1.3 体积估算

| 组件 | 必要性 | 估算 |
|---|---|---|
| avformat（~20 个 demuxer + mp4 muxer） | 必需 | 4–6 MB |
| avcodec 框架 + parser + bsf | 必需 | 3–4 MB |
| h264 解码器 | 抽帧必需 | 1.5–2 MB |
| hevc 解码器 | 抽帧必需 | 2–3 MB |
| vp8/vp9 解码器 | 抽帧必需 | 2–3 MB |
| av1（dav1d） | 抽帧必需 | 3–5 MB |
| 音频解码器（aac/mp3/opus/vorbis/flac/pcm） | 探测+字幕对齐 | 1.5–2 MB |
| 字幕编解码（subrip/ass/webvtt + mov_text） | remux 字幕必需 | 0.3 MB |
| libwebp 编码器 | 必需 | 0.5 MB |
| swscale（scale 滤镜） | 抽帧必需 | 1–1.5 MB |
| avfilter 框架（仅 scale/format/null） | 抽帧必需 | 1–2 MB |
| avutil | 必需 | 1–1.5 MB |
| **合计（单平台，静态，strip）** | | **21–33 MB** |

> 这是**估算不是实测**：本机缺 `make` 且 WSL 无 root（sudo 需密码）、
> 无 Docker，无法在本地完成 FFmpeg 构建。第一步行动项就是用 CI 产出真实数字。

可核对的锚点：完整 LGPL 构建有 **547 个解码器、238 个编码器**，而 omy 需要的
解码器只有约 **13 个**（h264/hevc/vp8/vp9/av1 + aac/mp3/opus/vorbis/flac +
subrip/ass/webvtt），编码器 2 个（libwebp、mov_text）。已实测确认这 13 个
**全部存在于 LGPL 构建中**，不需要 GPL 组件。

但组件数占比不能线性换算成体积：解码器之间体积差异极大（av1/hevc 是 h264 的
数倍），而 avformat/avutil/swscale 这些框架部分无论裁到多小都得留。所以
上表按模块估算，而非按 13/547 折算。

**降级档位**：若砍掉 AV1 与 HEVC 抽帧（缺时视频无缩略图，静态图不受影响），
可降到 **12–18 MB**；若连视频抽帧一起砍掉（只保留探测 + WebP 编码 + 纯 copy
转封装），可降到 **8–12 MB**——但那样 P2 播放的字幕会丢，视频列表没有缩略图。

**乘以平台数**：win-x64 / mac-arm64 / mac-x64 / linux-x64 / linux-arm64，
每个安装包各自带一份。Android 另算（见 §4）。

---

## 2. 专利：比体积更需要先决策的事

10 号文档 §3.3 为 HEIC 确立了 D-12 三层策略，理由写得很清楚：

> 这就是为什么 Firefox、Chromium 等浏览器长期不内置 HEVC 软解，而是调用系统
> 解码器——**把专利责任转移给已获授权的操作系统厂商**。

**内置 H.264/HEVC/AAC 解码器会反转这个决定。**目前 omy 不分发任何解码器，
用户自己装的 FFmpeg 由用户自己负责；一旦官方安装包里带上解码器，我们就成了
"分发编解码实现"的一方。

10 号文档 §3.4 已指出风险主要落在**商业分发者**与**应用商店上架者**，并要求
在 README 中声明不提供专利许可。内置后这条声明的分量会明显加重。

具体到各编码（数据来自 10 号文档 §3.1）：

| 编码 | 内置后的风险 | 备选 |
|---|---|---|
| H.264 | 🟡 MPEG LA 池，基础专利 2027–2030 陆续到期 | 系统解码器 |
| HEVC | 🔴 多池并存，学术界认为开源无法获得合规许可 | **建议不内置** |
| AAC | 🟡 Via LA 池 | 系统解码器 |
| VP8/VP9/AV1/WebP | 🟢 免版税 | 可安全内置 |

**建议**：裁剪构建只内置免版税编码（VP8/VP9/AV1/Opus/Vorbis/FLAC/WebP），
H.264/HEVC/AAC 走系统解码器或让用户自备完整 FFmpeg。这与 D-12 的分层思路一致，
体积也能压到 **12–18 MB**。

代价要说清楚：H.264 是目前最常见的视频编码，不内置意味着**多数用户的视频仍拿不到
缩略图**——除非他们自己装 FFmpeg。这个取舍需要你拍板。

---

## 3. 许可证：裁剪后仍是 LGPL，且要保留可替换性

`--disable-gpl` 且不引入 x264/x265 时，FFmpeg 为 LGPL-2.1+。["https://ffmpeg.org/legal.html"]
已实测确认 BtbN 的 LGPL 构建**自带 libwebp**（`--enable-libwebp` 为有，
`--enable-gpl` 为无），omy 需要的编码能力一个不缺。

但**静态链接 LGPL 库有额外义务**：LGPL-2.1 §6 要求最终用户能够替换该库。
静态链接时通常需提供目标文件或等效手段。10 号文档 §2.3 已为此列了各平台落地方式，
并在 §2.4 把"LGPL 静态链接的合规问题"列为 iOS 不带 FFmpeg 的理由之一。

**两个可选姿势**：

- **A. 静态裁剪 + 子进程调用**（推荐）。omy 通过管道调用独立的 `ffmpeg`/`ffprobe`
  可执行文件，不与 libav* 链接。这属于聚合而非派生，`omy-media` 自身不受 LGPL
  传染；我们分发的是未修改的 FFmpeg 二进制，附 LGPL 全文 + 构建脚本 + 源码地址
  即可。**当前架构就是这样，不需要改代码。**
- **B. 动态链接 libav\***。要改造成 FFI，还要处理 LGPL 的可替换性，收益是省掉
  一次进程启动（几十毫秒）。不值得。

顺带修正：10 号文档 §2 目前写的是"omy-media **动态链接** FFmpeg"，与实现
（纯子进程，`Cargo.lock` 里没有任何 ffmpeg 绑定）不符。该文档记录的是当初的打算，
按 AGENTS.md「以实测为准」应予订正。

---

## 4. 构建与分发

### 4.1 平台矩阵

| 平台 | 工具链 | 备注 |
|---|---|---|
| win-x64 | MSYS2 MinGW64 | 参考项目的主路径 |
| linux-x64 / arm64 | gcc + 静态 glibc 或 musl | |
| mac-arm64 / x64 | clang + osxcross 或原生 runner | 需两份 |
| Android | NDK 交叉编译 | 见下 |

**Android 尤其麻烦**，10 号文档 §4.3 已经记过：Google Play 自 2025-11 起强制
16 KB 内存页对齐，而社区 fork `ffmpegkit-maintained` 需自行验证对齐。
FFmpegKit 已于 2025 年 4 月退役。**建议 Android 继续不带 FFmpeg**，
与现状及 D-24 保持一致。

**iOS 维持不带**（D-24 已定）。

### 4.2 建议的仓库结构

参照 `genesys-ffmpeg-minimal` 的做法，**放独立仓库**而非塞进 omy 主仓：

```
omy-ffmpeg-minimal/
  configure-flags.sh      # 共享的 ./configure 配方
  VERSION                 # 第 1 行 ffmpeg 版本，第 2 行配方修订号
  scripts/build-*.sh      # 各平台构建
  scripts/smoke-test.sh   # 冒烟测试
  .github/workflows/      # CI + Release
```

主仓通过 tag + SHA256 固定版本。理由与参考项目相同：构建慢、平台相关，
不该拖累主仓 CI。

### 4.3 configure 配方草案

下面是**免版税档**（§2 建议的口径）的起点。未经实测，需在 CI 上迭代——
`--disable-everything` 之后漏掉任何一个组件都表现为运行期失败而非构建失败。

```sh
./configure \
  --disable-everything --disable-gpl --disable-nonfree \
  --disable-doc --disable-debug --disable-network \
  --disable-programs --enable-ffmpeg --enable-ffprobe \
  --enable-small \
  \
  `# 容器：用户手上可能是任何格式，这部分不能省` \
  --enable-demuxer=matroska,mov,avi,flv,mpegts,mpegps,asf,ogg,wav,mp3,flac,aac,rawvideo,image2,png_pipe,mjpeg_pipe,webp_pipe \
  --enable-muxer=mp4,webp,rawvideo \
  \
  `# 免版税视频解码（H.264/HEVC 见 §2 的专利讨论，此档不含）` \
  --enable-decoder=vp8,vp9,av1,theora,png,mjpeg,webp,bmp,gif \
  \
  `# 音频解码：探测与字幕时间对齐需要` \
  --enable-decoder=opus,vorbis,flac,mp3,pcm_s16le,pcm_s24le \
  \
  `# 字幕：remux 的 -c:s mov_text 需要` \
  --enable-decoder=subrip,ass,webvtt,text \
  --enable-encoder=mov_text \
  \
  `# 缩略图` \
  --enable-libwebp --enable-encoder=libwebp \
  --enable-filter=scale,format,null,anull \
  --enable-protocol=pipe,file \
  \
  `# parser/bsf：不加会出现"能 demux 但 copy 出来播不了"` \
  --enable-parser=h264,hevc,vp8,vp9,av1,aac,opus,flac,mpegaudio \
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,extract_extradata
```

几个容易踩的点：

- **`--disable-network` 要慎用**：omy 全程走管道，不需要网络协议，关掉能省体积
  也能减小攻击面。但要确认 `pipe:` 协议不受影响（配方里已显式 enable）。
- **parser 与 bsf 必须显式开**。只开 demuxer/muxer 时，`-c copy` 会因为
  拿不到 extradata 而产出"能生成但播不了"的 MP4——这类问题不报错，
  只有真正播放才发现。`remux.rs:254` 的 `looks_like_mp4` 检查也拦不住。
- **`image2` 与 `*_pipe` 别漏**：`prepare.rs` 的静态图判据依赖 ffprobe 报出的
  `png_pipe`/`mjpeg_pipe`/`image2` 容器名（见 `prepare.rs:405` 的测试）。
  漏掉会让静态图片被误判成视频送去抽帧。
- **`--enable-small`** 以速度换体积，对 omy 这种非实时场景合适。

### 4.3 内置 vs 按需下载

裁剪到 12–18 MB 后确实可以直接内置。但仍建议**保留按需下载作为补充**：
用户想要 H.264/HEVC 抽帧时，引导其下载完整 LGPL 构建到
`<应用目录>/ffmpeg/bin/`。

**这条路径零代码改动**——已实测验证：把 FFmpeg 放进该目录后，隐藏系统 ffmpeg，
`omy doctor` 仍准确报告"ffprobe 与 ffmpeg 均可用"。`ffprobe.rs:91` 的
`candidate_dirs()` 本来就包含这个位置。

---

## 5. 一个必须先修的缺陷（已实测，未修）

调研中发现 `thumbnail.rs` 的 263 与 410 行用了 `-quality`，
**在较新的 FFmpeg 上该参数被静默忽略**：

```
BtbN master N-126497   -quality 0/10/40/75/90/100  →  全是 5,300 字节
BtbN master N-126497   -q:v 10 → 23,798     -q:v 90 → 64,822
gyan 9.0.1             -quality 40 → 38,032   -quality 90 → 64,822
```

两个构建的 `-h encoder=libwebp` **都声明支持 `-quality`**，所以查帮助发现不了。

后果：质量参数失效 → 缩略图固定按默认质量编码 → `SIZE_LIMIT` 的"超限就降质量
重编"逻辑整个失灵，且不报错。仅 `lower_quality_yields_smaller_output` 一条测试
能发现。

**这条必须在内置之前修**（改用 `-q:v`）：一旦把某个版本的 FFmpeg 固定进安装包，
这类版本差异就从"用户环境问题"变成"我们发出去的缺陷"。

---

## 6. 建议的推进顺序

1. **先决策专利口径**（§2）——内不内置 H.264/HEVC/AAC。这决定了后面所有数字。
2. **修 `-quality` → `-q:v`**（§5）。独立小改动，与本方案解耦。
3. **建 `omy-ffmpeg-minimal` 仓库，用 CI 产出真实体积**。本地无法构建
   （缺 make、无 root、无 Docker），这一步必须在 CI 上做。拿到真实数字后
   再回头修订 §1.3 的估算。
4. **冒烟测试覆盖 omy 的四条真实路径**，而不只是 `ffmpeg -version`。
5. 主仓接入：打包脚本 + `omy doctor` 区分"内置"与"用户自备"。
6. 同步订正 10 号文档的"动态链接"表述，并更新 `site/` 中英两份的 FFmpeg 说明。

---

## 7. 待你决策的三个问题

1. **专利**：内置 H.264/HEVC/AAC 解码器吗？不内置则多数视频没有缩略图；
   内置则与 D-12 已确立的"责任转移给 OS 厂商"策略冲突。
2. **体积上限**：能接受多大？20–35 MB（全功能）/ 12–18 MB（免版税编码）/
   8–12 MB（无视频抽帧）。
3. **平台范围**：先只做 win-x64 验证可行性，还是一次铺开五个平台？

---

## 附：本文档的证据来源

**实测得出**（本机）：
- LGPL-shared 构建各 DLL 体积与最小可用集合（逐个重命名排除，七个全必需，148.55 MB）
- LGPL 构建自带 libwebp、无 `--enable-gpl`
- remux 的 `Stream mapping`：视频/音频 copy、字幕 subrip→mov_text 真转码
- `-quality` 在 BtbN master 上失效、`-q:v` 有效
- 放入 `<应用目录>/ffmpeg/bin/` 后 `omy doctor` 能找到

**估算未实测**：§1.3 的分组件体积。本地无法构建 FFmpeg。

**引自既有文档**：专利矩阵与 D-12 策略（`10-licensing-and-patents.md`）、
Android 16 KB 对齐与 FFmpegKit 退役（同上 §4.3、§2.4）。
