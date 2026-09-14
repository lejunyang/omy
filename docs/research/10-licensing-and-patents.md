# 10 · 许可证与专利

> 对应决策 **D-08**（开源）、**D-23**（分层授权，GUI 接受 GPL-3.0）、**D-12**（HEIC 分层策略）。
>
> ⚠️ **本文档是技术分析，不是法律意见。** 正式发布前应咨询执业律师，特别是涉及商业分发或跨法域的场景。

## 1. 用户提出的核心疑问

> 「需要支持 HEIC/HEIF，专利风险是啥？开源软件不能用吗」

这个问题背后是一个非常普遍的误解，值得先澄清：

**版权许可 ≠ 专利许可。这是两套完全独立的法律体系。**

| | 版权（Copyright） | 专利（Patent） |
|---|---|---|
| 保护对象 | 具体的**代码写法** | 抽象的**技术方法** |
| 开源许可证解决的 | ✅ 是这个 | ❌ 通常不是 |
| 「我能拿到代码吗」 | 是版权问题 | — |
| 「我能运行这段逻辑吗」 | — | 是专利问题 |

所以「libx265 是 GPL 开源的」和「我实现 HEVC 解码需不需要付专利费」是**两个不相干的问题**。前者说的是你可以自由获得、修改、分发那段源代码；后者说的是 HEVC 这套编解码**方法**本身被数万项专利覆盖，无论你用谁的实现、是否开源，在专利有效的法域内制造或销售产品都可能需要许可。

GPL v3 第 11 条甚至专门处理这个：贡献者授予你其**自身持有**的专利许可，但这不能覆盖第三方专利。所以一个 GPL 项目完全可能"版权上自由，专利上有风险"。

---

## 2. 分层许可证结构（D-23）

用户选择 FFmpeg 追求最大兼容性（D-11），这直接决定了 GUI 的许可证。但**不应该让整个项目被拖进 GPL**——核心格式库的价值在于能被任何人集成。

```
┌─────────────────────────────────────────────────────┐
│  omy-gui  (Tauri 应用)              GPL-3.0      │
│  ├── 依赖 omy-media                              │
│  └── 依赖 omy-core                               │
├─────────────────────────────────────────────────────┤
│  omy-cli                            GPL-3.0      │
│  （因为也链接 omy-media）                         │
├─────────────────────────────────────────────────────┤
│  omy-media  (FFmpeg 封装)           LGPL-2.1+    │
│  └── 子进程调用 ffmpeg/ffprobe (LGPL 构建)           │
├─────────────────────────────────────────────────────┤
│  omy-core   (格式 + 加密)      MIT OR Apache-2.0 │
│  └── 零 FFmpeg 依赖，纯 Rust                         │
├─────────────────────────────────────────────────────┤
│  omy-format-spec  (格式规范文档)      CC BY 4.0  │
└─────────────────────────────────────────────────────┘
```

### 2.1 为什么这样分

| 层 | 许可证 | 理由 |
|---|---|---|
| `omy-core` | MIT OR Apache-2.0 | 双许可是 Rust 生态惯例。任何人（含闭源商业软件）都能集成 `.omy` 格式支持。**这是格式能被广泛采用的前提** |
| `omy-media` | LGPL-2.1+ | 与 FFmpeg 一致。子进程调用独立可执行文件，用户可直接替换那两个 exe |
| `omy-cli` / `omy-gui` | GPL-3.0 | 最终产物包含 FFmpeg 且形成整体作品；GPL-3.0 有明确的专利条款与反 Tivoization 条款 |
| 格式规范 | CC BY 4.0 | 文档不是代码；鼓励第三方独立实现 |

**关键设计约束**：`omy-core` 必须**零 FFmpeg 依赖**。这不只是许可证考虑——它同时意味着：
- 第三方可以只用格式库做加解密，不背 GPL 包袱
- 核心加密逻辑的攻击面不包含 FFmpeg（见 07 号文档 §4）
- 移动端可以只编 core，不带 FFmpeg（iOS 尤其重要）

CI 中应有一条硬性检查：`omy-core` 的依赖树里出现任何 FFmpeg 相关 crate 即构建失败。

### 2.2 FFmpeg 许可的关键事实

FFmpeg 默认是 **LGPL v2.1+**；只有显式传 `--enable-gpl` 才会引入 GPL 组件（x264、x265、libpostproc 等），整体变为 **GPL v2+**。["https://www.ffmpeg.org/legal.html","https://ffmpeg.org/doxygen/trunk/md_LICENSE.html"]

**本项目只需解码 / demux / remux，不需要 x264/x265 编码器。**

因此：

```bash
# 本项目的 FFmpeg 构建配置（LGPL）。
# 这里只列与许可证相关的几项，完整配方见
# scripts/ffmpeg-build/configure-flags.sh（那份是实测跑通的）
./configure \
  --disable-gpl \          # 明确不启用 GPL 组件
  --disable-nonfree \
  --disable-version3 \
  --enable-static \        # 出独立可执行文件，靠子进程隔离而非动态链接
  --disable-shared \
  --disable-doc
```

> **静态链接与 LGPL 可替换性。** 静态链接 libav\* 会触发 LGPL-2.1 §6 的
> 「用户须能替换该库」义务，但这里静态链接的产物是**独立的 `ffmpeg.exe` /
> `ffprobe.exe`**，omy 通过管道调用它们、不与 libav\* 链接。用户把这两个
> 文件换成自己的构建即可，义务通过「可替换的可执行文件 + 随附 LGPL 全文 +
> 构建脚本 + 源码地址与 SHA256」来履行（见 `SOURCE.txt`）。
> 详见 13 号文档 §3。

> **例外：D-26 转码功能。** 用户新增的"转码后加密"需要**编码器**。若使用 x264/x265，必须 `--enable-gpl`，FFmpeg 变 GPL v2+。
> **处理方式**：转码功能编译为**独立的可选组件**，用户可选择安装 GPL 版 FFmpeg 后启用。官方预编译包默认使用 LGPL 版 + 系统硬件编码器（VideoToolbox / NVENC / QSV / MediaCodec），不内置 x264/x265。

### 2.3 LGPL 可替换性的合规要点

LGPL 要求最终用户能够**替换**该库。本项目走的是「独立可执行文件 + 子进程
调用」，所以要替换的是那两个 exe，而不是一组 DLL。各平台的落地：

| 平台 | 做法 | 难点 |
|---|---|---|
| Windows | 内置的 `ffmpeg.exe` / `ffprobe.exe` 放在 `<应用目录>\ffmpeg\`，直接覆盖即可；也可用 `OMY_FFMPEG` / `OMY_FFPROBE` 指向别处 | 无 |
| macOS | 同理放进 `.app/Contents/`，用户可替换 | 代码签名 —— 替换后签名失效，需说明如何处理 |
| Linux | 优先用系统 FFmpeg；随包附带的同样可替换 | 无 |
| Android | 当前不带 FFmpeg（见 §4.3） | — |
| **iOS** | ⚠️ **经典难题** | App Store 禁止动态加载非系统库，且用户无法替换 |

> 目前只有 Windows 产物内置 FFmpeg，其余平台仍靠用户自备；构建链只在
> Windows 上实测过。见 13 号文档 §4.1。

**iOS 的处理**（结合 D-06：iOS 优先级最低，降级为导入式保险箱）：

iOS 版**不包含 FFmpeg**。只用 AVFoundation 系统解码器 + `omy-core`。
这样 iOS 版可以是 **MIT/Apache-2.0**，同时规避了：
- LGPL 静态链接的合规问题
- FFmpegKit 已于 2025 年 4 月退役、二进制被删的维护风险
- 移动端 FFmpeg 交叉编译链的持续维护负担

对用户的影响：iOS 上只能播放 AVFoundation 支持的格式（H.264/HEVC in MP4/MOV）。这与 D-06 的定位一致，需在 iOS 版说明中明确告知。

---

## 3. 编解码专利

### 3.1 现状概览

| 编解码 | 专利状态 | 本项目风险 |
|---|---|---|
| **H.264/AVC** | MPEG LA 池，多数基础专利 2027–2030 陆续到期 | 🟡 中 |
| **HEVC/H.265** | ⚠️ **多池并存**，最复杂 | 🔴 高 |
| **VP8/VP9** | Google 免版税授权 | 🟢 低 |
| **AV1** | AOMedia 免版税专利池；但 Access Advance 的 VDP 池声称覆盖 AV1 | 🟡 中 |
| **AAC** | Via LA 池 | 🟡 中 |
| **MP3** | ✅ 已全部到期（2017） | 🟢 无 |
| **HEIC/HEIF 容器** | 容器本身 MPEG-4 系；**内部图像用 HEVC 编码** | 🔴 随 HEVC |
| **AVIF** | 基于 AV1 | 🟡 低 |
| **JPEG/PNG/WebP/GIF** | 已到期或免版税 | 🟢 无 |

### 3.2 HEVC 为什么特别麻烦

HEVC 没有单一的"付一次就完事"的池：

- **Access Advance**（原 HEVC Advance）：管理超过 23,000 项专利，声称覆盖 HEVC 必要专利的多数["https://hevcadvance.com","https://accessadvance.com/hevc-advance-patent-pool-general-pool-terms/"]
- **Via LA**（MPEG LA 与 Velos 合并后）：另一个池
- **未加入任何池的独立持有者**：如 Nokia、部分高校

即便向所有池付费，也无法保证覆盖全部。这与 H.264 的单池格局有本质区别。

Access Advance 的许可初始期至 2025-12-31，之后自动续为不可终止的 5 年期；且已宣布自 2026-01-01 起 HEVC 费率将向 VVC 定价结构对齐。["https://accessadvance.com/hevc-advance-patent-pool-general-pool-terms/","https://accessadvance.com/wp-content/uploads/2021/06/HEVC-Advance-Program-Overview-July-2025.pdf"]

**对开源软件的特殊困境**：现行 (F)RAND 条款以"每台设备"或"每个产品单位"计费，而开源软件的分发模式（任意人可下载、可再分发、无法计数）与之根本不兼容。已有学术研究得出"在现行 (F)RAND 条款下无法获得允许在开源软件中实现 HEVC 的许可"的结论。

这就是为什么 Firefox、Chromium 等浏览器长期不内置 HEVC 软解，而是调用系统解码器——**把专利责任转移给已获授权的操作系统厂商**。

### 3.3 本项目的 HEIC 策略（D-12）

采用与主流浏览器相同的思路，分三层：

```
第 1 层：系统解码器（默认，官方包唯一路径）
  ├── macOS/iOS  → ImageIO / AVFoundation
  ├── Windows    → Windows Imaging Component
  │                （需用户自行安装 "HEIF 图像扩展"）
  ├── Android    → MediaCodec (API 28+)
  └── Linux      → 通常不可用 → 降级
        专利责任：由 OS 厂商承担（用户已通过购买设备/系统获得授权）

第 2 层：libheif 软解（可选 feature，默认关闭）
  cargo build --features heif-software
        专利责任：由自行编译者承担
        官方预编译包不包含此 feature

第 3 层：降级
  无解码器时 → HEIC 文件仍可正常加密/解密/传输
              但不生成缩略图、不能预览
              UI 明确提示原因和解决办法
```

**关键点：加密功能永远不受影响。** omy 的核心是加密，不是图像解码。任何格式都能被加密和还原（D-07 bit-for-bit），只是能否**预览**取决于解码器可用性。这个边界必须在 UI 上说清楚，避免用户误以为"不支持 HEIC"。

`omy doctor` 会检测并报告（见 09 号文档 §5.8）：

```
⚠ HEIC 解码：系统解码器不可用，libheif 未编译进本构建
  → HEIC 文件可加密，但无法生成缩略图或预览
  → Windows 用户可在 Microsoft Store 安装「HEIF 图像扩展」
```

### 3.4 地域性

专利是**属地权利**。Access Advance 的规则中，若制造国和销售国均无相关专利覆盖，则无需付费。

对个人自用的开源软件用户，实际风险极低（专利权人几乎不起诉个人）。风险主要落在：
- 商业分发者（把本项目打包进商业产品）
- 大规模分发的应用商店上架者

**因此项目应当在 README 与发布说明中明确**：本项目不提供任何专利许可；商业使用者需自行评估所在法域的专利义务。

---

## 4. 加密算法的出口管制

### 4.1 美国 EAR

开源加密软件适用 **License Exception TSU**（15 CFR 740.13(e)）：
- 公开可得的源代码，出口前**通知** BIS 和 NSA（发邮件告知 URL 即可）
- 不需要事前审批
- 通知后即可自由分发

**行动项**：首次公开发布前，向 `crypt@bis.doc.gov` 和 `enc@nsa.gov` 发送包含仓库 URL 的通知邮件。

### 4.2 其它法域

| 地区 | 情况 |
|---|---|
| 欧盟 | 公开可得的加密软件一般豁免（Dual-Use Regulation Annex I Note 3） |
| 中国 | 商用密码管理条例；使用国际通用算法的开源软件一般不需专门许可 |
| 俄罗斯 / 部分中东国家 | 有加密软件使用限制，用户自行负责 |

在 README 中放一段简短说明即可，不必过度设计。

### 4.3 应用商店

| 商店 | 要求 |
|---|---|
| Apple App Store | 需申报使用加密；开源/标准算法可勾选豁免；可能需要提供 CCATS 或自分类报告 |
| Google Play | 需在数据安全表单声明；2025-11 起强制 16 KB 内存页对齐 |
| Microsoft Store | 相对宽松 |

> **注意**：Google Play 的 16 KB 页对齐要求是移动端 FFmpeg 的又一个维护负担（社区 fork `ffmpegkit-maintained` 只覆盖 Android 且需自行验证对齐）。这强化了"iOS 不带 FFmpeg、Android 优先系统解码器"的决策（D-24）。

---

## 5. 依赖许可证清单

`omy-core`（必须全部为宽松许可，否则破坏 MIT/Apache 承诺）：

| Crate | 用途 | 许可证 |
|---|---|---|
| `chacha20poly1305` | XChaCha20-Poly1305 | Apache-2.0 OR MIT |
| `aes-gcm` | AES-256-GCM | Apache-2.0 OR MIT |
| `argon2` | KDF | Apache-2.0 OR MIT |
| `hkdf` / `sha2` | 密钥派生 | Apache-2.0 OR MIT |
| `zstd` | 压缩 | BSD-3-Clause / GPL-2.0 双许可（用 BSD） |
| `zeroize` | 内存清理 | Apache-2.0 OR MIT |
| `uuid` | 文件 UUID | Apache-2.0 OR MIT |
| `snow` | Noise 协议 | Apache-2.0 |

⚠️ **`zstd` 需要注意**：Zstandard 是 BSD-3-Clause 与 GPLv2 双许可，选 BSD 分支即可。但要确认所用 Rust binding 正确传递了这一点。

`omy-gui` 额外依赖：

| 依赖 | 许可证 | 备注 |
|---|---|---|
| Tauri v2 | Apache-2.0 OR MIT | |
| FFmpeg (LGPL 构建) | LGPL-2.1+ | 子进程调用，不与 libav* 链接 |
| 前端框架 | MIT 系 | |

**CI 强制检查**：用 `cargo-deny` 配置许可证白名单，在 `omy-core` 上禁止任何 copyleft 依赖：

```toml
# deny.toml
[licenses]
allow = ["MIT", "Apache-2.0", "BSD-3-Clause", "ISC", "Unicode-DFS-2016"]
# omy-core 中出现 GPL/LGPL/AGPL 即失败
```

---

## 6. 需要随发行版附带的文件

```
LICENSE                    # 顶层：GPL-3.0（GUI/CLI）
LICENSE-MIT                # omy-core
LICENSE-APACHE             # omy-core
THIRD-PARTY-NOTICES.md     # 全部依赖的许可证与版权声明
FFMPEG-LICENSE.txt         # FFmpeg 的 LGPL 全文
PATENTS.md                 # 专利声明（见下）
```

`THIRD-PARTY-NOTICES.md` 应由 `cargo-about` 自动生成并纳入 CI，避免手工维护漏项。

**`PATENTS.md` 建议内容要点**：
- 本项目不提供任何专利许可
- 列出可能涉及专利的编解码（HEVC/AAC/AV1 等）
- 说明官方构建默认走系统解码器，不内置受专利覆盖的软件解码器
- 提示商业使用者自行评估

---

## 7. 小结：回答用户最初的问题

> 「专利风险是啥？开源软件不能用吗」

1. **开源软件当然能用 HEIC** —— 版权上没有任何障碍，libheif 是 LGPL，随便用。
2. **但开源不等于免专利** —— HEVC 的编解码方法被数万项专利覆盖，与代码是否开源无关。
3. **实际风险取决于你是谁** —— 个人自用几乎无风险；商业分发者才是专利权人的目标。
4. **本项目的应对** —— 官方构建只调用系统解码器（专利责任在 OS 厂商），软解作为可选 feature 默认关闭。这既让绝大多数用户能正常预览 HEIC，又不让项目自身承担分发责任。
5. **加密功能永不受限** —— 无论能否解码，HEIC 文件都能被完整加密和 bit-for-bit 还原。
