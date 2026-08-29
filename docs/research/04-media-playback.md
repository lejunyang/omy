# 04 · 媒体播放架构

> 本文档解决整个项目最核心的技术问题：**加密的视频如何做到任意拖动播放，且不产生临时明文文件。**

## 1. 结论先行

**不需要 DRM 式的"帧加密"，也不需要 MP4 faststart 重整。**

正解是：**分块 AEAD + HTTP Range 自定义协议**。

```
<video src="omy://file/{id}">
        │
        ▼ WebView 发出 Range: bytes=1048576-1310719
┌───────────────────────────────────────┐
│  Tauri 自定义协议 handler (Rust)       │
│    1. 明文偏移 → 块号                  │
│    2. 块号 → 密文偏移（O(1) 公式）      │
│    3. 只解密涉及的 1–2 个块            │
│    4. 返回 206 Partial Content         │
└───────────────────────────────────────┘
        │
        ▼ 播放器以为在读普通文件
   seek 完全原生，全程零落盘
```

---

## 2. 为什么不用帧加密

调研中曾考虑过 ISO Common Encryption（CENC，`cenc`/`cbcs` 方案）——即 DRM 领域的真·帧加密：保留 MP4 容器结构明文，只加密样本数据。

**否决理由**：

| 问题 | 说明 |
|---|---|
| **容器元数据明文泄露** | 时长、分辨率、码率、章节、轨道信息全部可见。与威胁模型 T3（共享设备）冲突 |
| **实现复杂度翻倍** | 需自行解析 MP4 box 树，按 codec 区分加密子样本（H.264 的 NAL 头必须明文） |
| **只覆盖特定容器** | MP4/CMAF 之外的格式无法适用；而我们需要加密**任意文件** |
| **收益为零** | 分块 AEAD 已经能做到 O(1) 随机访问 |

---

## 3. 分块 AEAD + Range 映射

### 3.1 偏移换算（未压缩）

```rust
// 明文偏移 → 密文偏移，O(1)，无需索引
fn ct_offset(header_len: u64, chunk_size: u32, i: u64) -> u64 {
    header_len + i * (chunk_size as u64 + TAG_LEN)
}

fn chunk_index(plain_offset: u64, chunk_size: u32) -> u64 {
    plain_offset / chunk_size as u64
}
```

已验证：公式计算结果与实际字节布局精确一致。

### 3.2 Range 请求处理流程

```
收到 Range: bytes=START-END
  │
  ├─ 1. 解析 Range 头（注意处理 suffix range "bytes=-500"）
  ├─ 2. first_chunk = START / chunk_size
  │     last_chunk  = END / chunk_size
  ├─ 3. for i in first_chunk..=last_chunk:
  │        密文 = pread(ct_offset(i), ct_len(i))
  │        明文 = AEAD_decrypt(payload_key, nonce(i), 密文, aad(i))
  │        取所需切片
  ├─ 4. 返回:
  │        Status: 206 Partial Content
  │        Content-Range: bytes START-END/plaintext_size
  │        Content-Length: END-START+1
  │        Accept-Ranges: bytes
  │        Cache-Control: no-store, no-cache, must-revalidate  ← 关键，见 §9.1
  └─ 5. 首次请求（无 Range）返回 200 + Accept-Ranges: bytes
```

### 3.3 最小解密量

| 请求 | 解密块数（已验证） |
|---|---:|
| 块内任意区间 | 1 |
| 跨一个块边界 | 2 |
| 长度 L | ≤ `ceil(L / chunk_size) + 1` |

**256 KiB 块 + 4K 视频 seek** → 单次解密 256 KiB ≈ 亚毫秒级。

### 3.4 Tauri 实现要点

使用 `register_asynchronous_uri_scheme_protocol`（**异步**版本，避免阻塞主线程）：

```rust
tauri::Builder::default()
    .register_asynchronous_uri_scheme_protocol("omy", move |_ctx, request, responder| {
        // 1. 从 URL 提取 file_id（不透明句柄，不是路径）
        // 2. 查会话表拿到 FEK（JS 永远拿不到）
        // 3. 解析 Range 头
        // 4. 解密对应块
        // 5. responder.respond(response)
    })
```

**为什么用自定义协议而非本地 HTTP server**：无端口冲突、无 CORS 问题、可按窗口作用域隔离、不会被同机其他进程访问。

### 3.5 缓冲与预读

- **LRU 块缓存**：缓存最近解密的 N 个块（默认 8 块）。播放器常对同一区域发多次请求
- **顺序预读**：检测到连续 Range 模式时预解密后续 1–2 块
- **内存上限**：`chunk_size × 缓存块数`，移动端需下调（见 §8）
- **锁定时立即清空**：缓存中是明文，必须随会话销毁

---

## 4. MP4 与 moov：为什么不需要 faststart

### 4.1 一个被广泛误解的问题

常见说法是"moov 在文件尾部的 MP4 必须下载完才能播放，需要 faststart 优化"。

**这个说法只在跨公网时成立。** 准确的结论是：

> moov 在尾部 + 服务端支持 Range = **照样能秒起播、能 seek**。播放器只是在开头多做两次 seek，产生 3 个初始 206 请求。

faststart 在 Web 上是刚需，是因为**跨公网的每次 seek 都要付一个 RTT（几十到几百毫秒）**。

而我们是**本地磁盘 + O(1) 随机访问**——多两次 seek 的代价是**微秒级**。

### 4.2 结论

**原文件按字节原样加密，什么都不动。**

- ✅ bit-for-bit 还原天然成立（决策 D-07）
- ✅ 不需要记录"逆变换配方"
- ✅ 不需要解析 MP4 box 结构

### 4.3 更好的优化：moov 缓存进 header

既然我们本来就要在加密时探测媒体信息，不如**把 moov box 的内容缓存进 `TLV_MOOV_CACHE`**（加密存储）。

| 收益 | 说明 |
|---|---|
| **比 faststart 还快** | 起播时 moov 直接从 header 拿，连那两次额外 seek 都省了 |
| **原文件零改动** | 不违反可还原性 |
| **局域网远程播放收益最大** | 省掉真实网络往返 |
| **顺带解决 iOS 兼容性** | WKWebView 对 moov 在尾部的容忍度历史上比桌面差，缓存后绕过 |
| **顺带解决缺片播放** | 每个分片的冗余 header 都带一份 moov（见 [05](05-container-and-sharding.md)） |

实现：加密时用 FFmpeg probe 定位 `moov` box 的偏移和长度，把该区间字节存入 TLV。播放时优先从 TLV 提供。

---

## 5. 三条播放路径

### 5.1 路径定义

| 路径 | 图标 | 适用 | 机制 | seek 延迟 |
|---|:---:|---|---|---|
| **P1 直通** | ⚡ | WebView 原生可解的格式 | 自定义协议 + Range，**零处理** | 毫秒级 |
| **P2 转封装** | 🔄 | 容器不支持但编码支持 | FFmpeg demux → fMP4 → MSE | 数十毫秒 |
| **P3 全解码** | 🐌 | 编码也不支持 | FFmpeg 解码 → 转码 → MSE | 数百毫秒–秒级 |

### 5.2 格式分级表

**WebView 原生支持范围比想象的窄**：

| 格式 | Chromium 系（Win/Android/Linux） | WKWebView（macOS/iOS） | 分级 |
|---|:---:|:---:|:---:|
| **视频编码** ||||
| H.264 | ✅ | ✅ | P1 |
| H.265/HEVC | ⚠️ 看硬件 | ✅ | P1/P2 |
| VP8 / VP9 | ✅ | ⚠️ 部分 | P1/P2 |
| AV1 | ⚠️ 看版本 | ⚠️ 看版本 | P1/P2 |
| XviD / MPEG-4 ASP | ❌ | ❌ | **P3** |
| **音频编码** ||||
| AAC / MP3 | ✅ | ✅ | P1 |
| AC-3 / E-AC-3 | ❌ Chrome 已移除 | ⚠️ 部分 | P2/P3 |
| **DTS / DTS-HD** | ❌ | ❌ | **P3** |
| **TrueHD** | ❌ | ❌ | **P3** |
| FLAC / Opus | ✅ (WebM/Ogg) | ⚠️ | P1/P2 |
| Vorbis | ✅ (WebM/Ogg) | ❌ | P2 |
| **容器** ||||
| MP4 | ✅ | ✅ | P1 |
| WebM | ✅ | ⚠️ | P1/P2 |
| **MKV** | ❌ `<video>` 不支持 | ❌ | **P2** |
| AVI / FLV / TS | ❌ | ❌ | P2/P3 |

### 5.3 ⚠️ MKV 转封装的真实命中率

一个反直觉的关键点：**如果一个片源是 H.264 + AAC，它当初大概率就直接封成 MP4 了。**

用户手上之所以是 MKV，常见原因恰恰是 MP4 装不下的东西：

| 典型 MKV 来源 | 内容 | 能否 remux |
|---|---|---|
| 蓝光原盘 remux | DTS-HD / TrueHD 音轨 + PGS 字幕 | ❌ 音轨必须转码 |
| 动画压制 | 多音轨 + ASS 特效字幕 + 章节 | ⚠️ 视频可 copy，字幕丢失 |
| 老资源 | XviD + AC-3 | ❌ 全部需转码 |
| 现代 web-dl | H.264/H.265 + AAC | ✅ 可直接 remux |

**MP4 容器的硬限制**：DTS、DTS-HD、TrueHD、Vorbis 无法封入；PGS/VobSub 图形字幕无法转 `mov_text`；FLAC/Opus 虽然新版规范支持但很多播放器不认。

→ **P2 的实际收益要打折扣，很多 MKV 会落到 P3。**

### 5.4 分级结果写入 header

加密时探测一次，把结果存入 `TLV_MEDIA_META`：

```json
{
  "container": "matroska",
  "video": {"codec": "h264", "width": 1920, "height": 1080,
            "profile": "high", "level": 41},
  "audio": [{"codec": "dts", "channels": 6, "lang": "eng"},
            {"codec": "aac",  "channels": 2, "lang": "jpn"}],
  "subtitles": [{"codec": "ass", "lang": "chs"}],
  "duration_ms": 1423000,
  "playback_tier": {"default": "P3", "reason": "audio codec dts not supported",
                    "alternatives": [
                      {"tier": "P2", "note": "select audio track 1 (aac)"}
                    ]}
}
```

**三个好处**：
1. **列表页秒判**：不用重新解密探测，直接读 header 就知道能不能内嵌播放
2. **UI 提前显示正确按钮**：P3 文件直接标注"建议用外部应用打开"
3. **多轨道可选**：上例中切换到 AAC 音轨即可从 P3 降到 P2

### 5.5 用户可选播放方式（决策 D-25）

UI 提供播放方式选择，不强制自动决策：

```
▶ 播放方式
  ◉ 自动（推荐）              当前将使用：🔄 转封装
  ○ ⚡ 直通                   ⚠️ 此文件可能无法播放
  ○ 🔄 转封装                 需选择兼容音轨
  ○ 🐌 全解码                 兼容性最好，速度最慢
  ○ 📤 用外部应用打开
```

播放器界面上**始终显示当前路径图标**，让用户理解为什么某些文件慢。

---

## 6. MSE 路径（P2/P3）

### 6.1 为什么 P2/P3 不能用 Range

一旦 remux 或转码，播放器看到的字节流**不再是原文件**。它请求的"fMP4 第 5,000,000 字节"与原始 MKV 的字节位置之间没有简单映射，**Range 方案失效**。

### 6.2 MSE 流程

```
播放器 seek 到 t 秒
  → 查 MKV 的 Cues 索引，定位覆盖 t 的 cluster
  → 算出这些 cluster 在原文件的字节区间
  → 用分块 AEAD 解密对应区间（复用同一套块寻址！）
  → FFmpeg demux → mux 成 fMP4 片段
  → SourceBuffer.appendBuffer()
```

**关键**：底层的"解密某个字节区间"逻辑与 P1 完全共用，只是上层多了 demux/mux 一层。

### 6.3 ⚠️ MSE 的平台限制

| 平台 | MSE 支持 |
|---|---|
| Chrome / Edge / Chromium WebView | ✅ 完整 |
| Safari macOS | ✅ Safari 8+ |
| Safari iPadOS | ✅ iPadOS 13+ |
| **Safari iPhone** | ⚠️ 长期只有部分支持，**iOS 17.1（2023-11）才通过 ManagedMediaSource 补齐** |

`ManagedMediaSource` 是 `MediaSource` 的子类，增加了 `startstreaming`/`endstreaming` 事件和 `streaming` 属性。实现时需要特性检测：

```js
const MS = window.ManagedMediaSource || window.MediaSource;
```

⚠️ **MSE 通道下的编码支持与 `<video>` 直连不同**，需要维护第二张兼容性矩阵。已知差异：HEVC 的 `hev1` 在 Safari 可走 MSE 但 Chrome 不行，`hvc1` 两边都不行；WebM/VP8 在 Safari 走 MSE 不支持。

**实现建议**：用 `MediaSource.isTypeSupported()` 运行时探测，而非硬编码平台判断。

### 6.4 安全影响

MSE 路径下解密后的数据**必须**经过 JS `appendBuffer`，进入 JS 堆内存。

相比 P1（字节从 Rust 直接进媒体管线，不经过 JS），这是攻击面的实质性扩大。在本威胁模型下可接受，但：
- 应在文档和 UI 中标注
- 用完的 `ArrayBuffer` 应主动置空（虽然 JS 无法保证真正清零）

---

## 7. 视频转码选项（新增需求 D-26）

> 用户补充：「加密时也添加一个视频转码选项，用户可选择转为 web 原生支持的格式再加密，避免后续性能问题」

这个需求很有价值——**用一次性的转码成本换取永久的播放性能**。但它与决策 D-07（必须完整还原）存在**直接冲突**，必须让用户明确知情。

### 7.1 三种模式

| 模式 | 存什么 | 可还原 | 播放路径 | 文件大小 |
|---|---|:---:|---|---|
| **① 原样加密**（默认） | 原始字节 | ✅ bit-for-bit | 按格式定级 | = 原文件 |
| **② 转码后加密** | 转码产物 | ❌ **不可还原** | ⚡ 恒为 P1 | 可能变大或变小 |
| **③ 双存储** | 原始字节 + 转码副本 | ✅ | ⚡ P1（播放副本） | ≈ 两份之和 |

### 7.2 UI 必须呈现的警告

选择模式 ② 时，**必须**弹出确认对话框：

```
⚠️  转码后无法还原原始文件

    你选择了「转码后加密」。加密后保存的是转码产物，
    不是你的原始文件。

    • 原始文件的画质、音轨、字幕可能有损失
    • 解密后得到的是转码后的 MP4，不是原始的 MKV
    • 此操作不可逆

    原始文件哈希会被记录，但仅供核对，无法据此还原。

    [ 取消 ]    [ 改用双存储 ]    [ 我了解，继续 ]
```

### 7.3 格式标记

模式 ② 和 ③ 必须：
- 置位 `FLAG_TRANSCODED`（模式 ③ 置位但同时存原始载荷）
- 写入 `TLV_TRANSCODE_INFO`：

```json
{
  "mode": "transcode_only",
  "original": {
    "filename": "movie.mkv",
    "size": 8547231744,
    "blake2b256": "a3f9...",
    "container": "matroska",
    "video": "h264", "audio": "dts"
  },
  "transcoded": {
    "container": "mp4", "video": "h264 (copy)", "audio": "aac 192k",
    "ffmpeg_args": ["-c:v", "copy", "-c:a", "aac", "-b:a", "192k"]
  },
  "transcoded_at": "2026-08-29T10:30:00Z"
}
```

- 列表页和详情页显示 **⚠️ 已转码** 标记

### 7.4 转码预设

| 预设 | 参数 | 适用 | 质量损失 |
|---|---|---|---|
| **仅换容器** | `-c copy` | MKV(H.264+AAC) → MP4 | **无损** |
| **仅转音轨** | `-c:v copy -c:a aac -b:a 192k` | DTS/TrueHD → AAC | 仅音频 |
| **快速兼容** | `-c:v libx264 -preset veryfast -crf 23 -c:a aac` | 任意 → H.264/AAC | 中等 |
| **高质量** | `-c:v libx264 -preset slow -crf 18 -c:a aac -b:a 256k` | 归档 | 轻微 |
| **自定义** | 用户输入 FFmpeg 参数 | 高级 | — |

⚠️ **"仅换容器"预设是无损的**，UI 应特别标注——这种情况下虽然字节变了，但音视频流本身完全一致。

⚠️ **libx264 是 GPL**。若启用视频重编码预设，会把 FFmpeg 构建拉到 GPL 档位。详见 [10-许可证](10-licensing-and-patents.md)。可考虑：重编码预设仅在 GPL 构建中提供，LGPL 构建只提供"仅换容器"和"仅转音轨"（AAC 编码器在 LGPL 范围内）。

### 7.5 转码时机与体验

- 转码是**耗时操作**（大文件可能几十分钟），必须：
  - 后台队列执行，可暂停/取消
  - 显示进度和预估剩余时间
  - 移动端警告耗电和发热
- 转码在**加密之前**进行，中间产物落在受控临时目录，完成后立即清理

---

## 8. 移动端策略（决策 D-24）

> 用户决策：移动端优先调用系统解码器，FFmpeg 只做 demux/remux。

### 8.1 为什么

| 方案 | 性能 | 耗电 | 包体 | 结论 |
|---|---|---|---|---|
| FFmpeg 软解 4K HEVC | 卡顿 | 严重发热 | +20 MB | ❌ |
| **系统硬件解码** | 流畅 | 正常 | 0 | ✅ |

### 8.2 各平台

| 平台 | 解码器 | 调用方式 |
|---|---|---|
| Android | MediaCodec | 通过 WebView 原生播放（P1），或自写插件 |
| iOS | VideoToolbox | 同上 |

### 8.3 移动端的路径可用性

| 路径 | Android | iOS |
|---|:---:|:---:|
| P1 直通 | ✅ | ✅ |
| P2 转封装（FFmpeg demux + MSE） | ✅ | ⚠️ 需 iOS 17.1+ |
| P3 全解码 | ❌ **不提供** | ❌ **不提供** |

P3 在移动端直接引导"用外部应用打开"或"在桌面端转码后再同步"。

### 8.4 ⚠️ 移动端 FFmpeg 的工程风险

**FFmpegKit（移动端最主流的 FFmpeg 封装）已于 2025 年 4 月退役，二进制包被删除**，导致大量依赖项目崩溃。

现状：
- 官方仓库指向社区续作 `FFmpegKitNext`
- 有社区维护 fork（如 `ffmpegkit-maintained`），但**明确只覆盖 Android，不含 iOS/macOS**
- 新增合规要求：**Google Play 自 2025 年 11 月起强制要求支持 16 KB 内存页**，老构建无法过审

**结论**：移动端 FFmpeg 需要**自行维护交叉编译链**（Android NDK / iOS xcframework），这是**持续性工程负担**，不是一次性成本。

这也进一步支持"移动端只用 FFmpeg 做 demux/remux"的决策——精简构建可以大幅减小维护面。

### 8.5 移动端资源限制

| 参数 | 桌面默认 | 移动端默认 |
|---|---|---|
| chunk_size 上限 | 16 MiB | **1 MiB** |
| LRU 块缓存 | 8 块 | **3 块** |
| Argon2 档位 | interactive | **mobile** |
| 并发预读 | 2 块 | 1 块 |

---

## 9. 旁路泄露：WebView 缓存（重要）

### 9.1 问题

**自定义协议返回的响应，WebView 的 HTTP 缓存层可能落盘持久化。**

这意味着：你辛苦避免了临时明文文件，结果 WebView 在自己的 cache 目录里存了一份解密后的视频片段。

### 9.2 对策

**必须**在每个自定义协议响应中设置：

```
Cache-Control: no-store, no-cache, must-revalidate, max-age=0
Pragma: no-cache
Expires: 0
```

并且：
- 应用退出时主动清理 WebView 缓存目录
- 锁定时清理
- 启动时清理上次残留

各平台缓存目录：

| 平台 | 路径 |
|---|---|
| Windows (WebView2) | `%LOCALAPPDATA%\{app}\EBWebView\Default\Cache` |
| macOS (WKWebView) | `~/Library/Caches/{bundle-id}` + `WKWebsiteDataStore.removeData` |
| Linux (WebKitGTK) | `~/.cache/{app}` |
| Android | `WebView.clearCache(true)` + `WebStorage.deleteAllData()` |
| iOS | `WKWebsiteDataStore.default().removeData` |

完整旁路清单见 [07-平台适配与旁路防护](07-platform-and-sidechannels.md)。

---

## 10. 图片预览

### 10.1 支持范围（决策 D-14）

| 格式 | 支持 | 实现 |
|---|:---:|---|
| JPEG / PNG / WebP | ✅ | 自定义协议直出，浏览器原生解码 |
| **GIF / APNG** | ✅ | 同上（动图原生支持） |
| **HEIC / HEIF** | ✅ | **分层策略**，见 §10.2 |
| **SVG** | ✅ | **强制 `<img>` 渲染**，见 §10.3 |
| BMP / ICO | ✅ | 原生 |
| TIFF | ⚠️ | Safari 原生支持，Chromium 需转码 |
| RAW (CR2/NEF/ARW) | ❌ | 用户明确不需要 |

### 10.2 HEIC 分层策略（决策 D-12）

| 层级 | 做法 | 覆盖 |
|---|---|---|
| **① 优先：系统解码器** | macOS/iOS → VideoToolbox / ImageIO；Windows → Media Foundation / WIC；Android → MediaCodec | 覆盖绝大多数场景（HEIC 主要来自 iPhone，Apple 设备原生支持） |
| **② 兜底：libheif 软解** | 编译为**可选 feature，默认关闭** | Linux 桌面、Windows 未装 HEVC 扩展的用户 |
| **③ 发布策略** | 官方预编译包**不含**软解；提供源码和构建脚本让用户自行开启 | 类似 Chromium 的思路 |

专利风险详细分析见 [10-许可证与专利合规](10-licensing-and-patents.md)。

### 10.3 SVG 安全处理（决策 D-13）

⚠️ **SVG 可以内嵌 `<script>`。** 在 WebView 里直接 inline 渲染一个来路不明的 SVG = 任意 JS 执行 = 完全攻陷。

**强制措施**：

1. **只用 `<img src="omy://...">` 渲染**，绝不 inline 到 DOM
   - `<img>` 加载的 SVG 处于"安全静态模式"，脚本不执行、外部资源不加载
2. **绝不使用** `innerHTML` / `dangerouslySetInnerHTML` / `<object>` / `<embed>` / `<iframe>` 加载 SVG
3. **CSP 兜底**：
   ```
   default-src 'none';
   img-src omy: data:;
   media-src omy:;
   script-src 'self';
   object-src 'none';
   ```
4. Rust 侧返回 SVG 时设置 `Content-Type: image/svg+xml` 并附 `Content-Security-Policy: sandbox`

### 10.4 缩略图（决策 D-10）

**存储**：加密后存入 `TLV_THUMBNAIL`，建议 ≤ 32 KB（WebP 或 JPEG）。

**收益**：500 张加密图片的网格视图**只需读 header**（几 KB/文件），秒开。已验证：含缩略图的 header 为 8,771 B，而整个文件 58,787 B——列表页读取量减少 85%。

**生成方式（用户可选）**：

| 方式 | 说明 |
|---|---|
| 自动生成 | 图片用 `image` crate 缩放；视频取第 N 秒的帧 |
| **视频自选帧** | 见下 |
| 自定义图片 | 用户指定任意图片作为封面 |
| **不存** | 更少元数据泄露，列表页显示统一占位图 |

**视频自选帧的巧妙实现**——**不需要引入额外依赖**：

```
用户在应用内播放视频（走 P1/P2/P3 任一路径）
  → 暂停到想要的那一帧
  → 点击"设为封面"
  → 前端 canvas.drawImage(videoEl) + canvas.toBlob()
  → 交给 Rust 加密写入 TLV
```

完全复用已有的播放链路，零额外依赖。

---

## 11. 字幕

### 11.1 首期范围（决策 D-27）

| 类型 | 首期 | 方案 | 复杂度 |
|---|:---:|---|---|
| **SRT / WebVTT** | ✅ | FFmpeg 提取 → 转 WebVTT → `<track>` | 🟢 |
| **外挂 .srt/.vtt** | ✅ | 自动关联，一并加密 | 🟢 |
| **ASS / SSA** | ❌ **暂不实现** | 需 JASSUB（libass WASM）渲染到 canvas 叠加层 | 🟡 |
| **PGS / VobSub** | ❌ **暂不实现** | 需 FFmpeg 解码成位图 + canvas 贴图 | 🔴 |

### 11.2 未实现部分的架构预留

虽然首期不做，但**必须留好接口**，否则后期改动成本高：

1. **`TLV_SUBTITLES` 结构已定义**，支持任意字幕类型：
```json
{
  "tracks": [
    {"id": 0, "codec": "subrip", "lang": "chs", "title": "简体中文",
     "source": "embedded", "data_ref": "tlv_offset:0"},
    {"id": 1, "codec": "ass", "lang": "jpn", "title": "日本語",
     "source": "external", "original_filename": "movie.jpn.ass",
     "supported": false, "reason": "ASS rendering not implemented"}
  ],
  "fonts": [{"name": "SourceHanSans", "data_ref": "tlv_offset:12345"}]
}
```

2. **渲染层抽象**：
```rust
trait SubtitleRenderer {
    fn supports(codec: &str) -> bool;
    fn render(&self, track: &SubtitleTrack, time_ms: u64) -> RenderOutput;
}
// 首期只实现 WebVttRenderer
// 未来：AssRenderer (JASSUB), BitmapRenderer (PGS/VobSub)
```

3. **UI 降级**：不支持的字幕轨在选单中显示为灰色，标注"暂不支持此字幕格式"，**而不是隐藏**——让用户知道文件里有这条轨道。

### 11.3 ASS 未来实现的注意点（留档）

- **JASSUB** 是 libass 的 WASM 移植，ASS 渲染的事实标准
- **许可证**：libass 是 ISC，但依赖 fontconfig（MIT）/ freetype（FTL 或 GPL 双授权），需纳入许可证清单
- **字体问题**：ASS 会指定字体，本机没有时需 fallback。MKV 内嵌的字体附件（`AttachedFile`）要提取出来喂给渲染器 —— 这些字体也要一起加密存入 `TLV_SUBTITLES.fonts`
- **性能**：复杂特效字幕（卡拉OK、逐字动画）在移动端可能掉帧

### 11.4 外挂字幕加密（决策 D-28）

**规则**：加密视频时，自动检测同目录下的同名字幕文件并**一并加密**。

```
movie.mkv
movie.srt          ← 自动检测
movie.chs.ass      ← 自动检测
movie.eng.srt      ← 自动检测
```

**为什么必须一起加密**：`.ass` 明文放在旁边等于**泄露剧情和文件身份**——即使视频加密了，字幕文件名和内容就把一切说清楚了。

**UI 提示（必须）**：

```
🔍 检测到 3 个关联字幕文件

   ☑ movie.srt          (简体中文, 45 KB)
   ☑ movie.chs.ass      (简体中文特效, 128 KB)
   ☑ movie.eng.srt      (English, 41 KB)

   ℹ️ 字幕将一并加密并关联到视频。
      若不加密，字幕文件的文件名和内容会泄露视频信息。

   [ 全不选 ]                        [ 确定 ]
```

原字幕文件的处理（删除/保留）遵循与主文件相同的设置（决策 D-15）。

---

## 12. 音频与文本

### 12.1 音频

| 格式 | 路径 |
|---|---|
| MP3 / AAC / M4A / WAV / FLAC(部分) / Opus | ⚡ P1 |
| APE / DSD / WavPack | 🐌 P3 |

音频的 Range 播放与视频完全相同，且因为文件小、码率低，seek 更快。

**波形图**：可选在 `TLV_MEDIA_META` 中存储预计算的波形峰值数组（几 KB），播放器显示波形而无需完整解码。

### 12.2 文本

| 项 | 处理 |
|---|---|
| **编码识别** | UTF-8 / UTF-16 / GBK / Big5 / Shift-JIS 自动检测（`chardetng` crate），用户可手动切换 |
| **大文件** | 虚拟滚动 + 按需解密块（复用 Range 逻辑）。10 MB 日志文件不应一次性载入 |
| **语法高亮** | 可选，按扩展名判断（`syntect` 或前端 Prism） |
| **搜索** | 在已解密的可见块内搜索；全文搜索需逐块解密（给进度条） |
| **编辑** | ❌ 首期只读（决策 D-16） |

### 12.3 PDF（决策 D-14）

**不内嵌预览**，走"用外部应用打开"通道。

理由：pdf.js 体积大（~1 MB+），而 PDF 阅读器是每个系统的标配。

---

## 13. 外部应用打开

### 13.1 各平台能力

| 平台 | 能否不落盘 | 方案 |
|---|:---:|---|
| **Android** | ✅ **可以** | 自建 ContentProvider 暴露 `content://` URI + `StorageManager.openProxyFileDescriptor`（API 26+，**支持随机读**） |
| **iOS** | ❌ | Share Sheet 会复制副本到 tmp；除非做 File Provider Extension（重投入） |
| **Windows** | ❌ | 落 tmp，或 WinFsp / Cloud Files API 虚拟盘 |
| **macOS** | ❌ | 落 tmp，或 FSKit（macOS 15+）/ File Provider Extension |
| **Linux** | ❌ | 落 tmp，或 FUSE |

### 13.2 受控临时文件（桌面端）

```
1. 解密到受控临时目录（优先内存盘）
2. 设置最严格的文件权限（0600 / Windows ACL 仅当前用户）
3. 记录到"活跃临时文件表"
4. 启动外部应用
5. 清理触发条件（任一满足）：
   - 外部应用关闭（监听文件句柄）
   - 闲置超时（默认 5 分钟）
   - 应用退出
   - 会话锁定
   - 下次启动时检测残留
```

### 13.3 临时目录选择（按优先级）

| 优先级 | 位置 | 条件 |
|---|---|---|
| 1 | **内存盘**（Linux `/dev/shm`、macOS RAM disk、Windows 内存映射） | 文件 < 可用内存的 1/4 |
| 2 | 用户配置的加密卷内 | 用户已设置 |
| 3 | 系统临时目录 | 兜底 |

### 13.4 ⚠️ SSD 无法安全擦除（威胁模型 N8）

**必须诚实告知，不提供虚假的"安全删除"按钮。**

> SSD 的 wear leveling 和 TRIM 机制使得覆写擦除在物理层面不可靠——
> 你以为覆盖的是原位置，实际上控制器把新数据写到了别的闪存块。
>
> 可靠的做法是：让临时目录本身位于加密卷，或使用内存盘。

UI 文案：
```
ℹ️ 临时文件已删除

   注意：在 SSD 上，已删除的文件可能仍有残留数据无法彻底清除。
   如需更高保障，请在设置中启用「仅使用内存盘」。
```

### 13.5 只读语义（决策 D-16 + N8）

外部应用打开的临时文件是**只读**的：
- 若外部应用修改了它，**修改将被丢弃**
- UI 在打开前提示："以只读方式打开，外部修改不会保存回加密文件"

### 13.6 Android 不落盘方案

```kotlin
// ContentProvider 暴露 content:// URI
override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor? {
    val storageManager = context.getSystemService(StorageManager::class.java)
    return storageManager.openProxyFileDescriptor(
        ParcelFileDescriptor.MODE_READ_ONLY,
        object : ProxyFileDescriptorCallback() {
            override fun onGetSize(): Long = plaintextSize
            override fun onRead(offset: Long, size: Int, data: ByteArray): Int {
                // 调用 Rust 侧解密对应区间 —— 复用同一套块寻址
                return nativeReadRange(fileId, offset, size, data)
            }
            override fun onRelease() { /* 清理 */ }
        },
        handler
    )
}
```

**这是唯一能做到"外部应用随机读 + 全程不落盘"的平台方案。**

---

## 14. FFmpeg 安全隔离（关键）

### 14.1 风险

**FFmpeg 是 CVE 高发区**（大量解析不可信输入的 C 代码）。我们的应用会用它处理**用户的任意文件**——一个恶意构造的 MKV 可能导致 RCE。

> 讽刺的是：为了播放便利引入 FFmpeg，反而在一个加密应用里开了最大的攻击面。

### 14.2 强制隔离措施

| 措施 | 说明 |
|---|---|
| **独立子进程** | FFmpeg 绝不在主进程内运行。崩溃不影响主进程和密钥 |
| **子进程不持有密钥** | 主进程解密后，通过管道把**明文字节**喂给子进程 |
| **降权运行** | 丢弃不必要的权限 |
| **沙箱** | Linux: seccomp-bpf + namespaces；macOS: `sandbox_init`；Windows: AppContainer / Job Object |
| **资源限制** | CPU 时间、内存、文件描述符上限；超时强杀 |
| **无网络** | 沙箱内禁止网络访问（防 SSRF 和数据外传） |
| **无文件系统访问** | 只通过 stdin/stdout 管道通信 |
| **版本跟进机制** | 建立 CVE 订阅和更新流程 |

### 14.3 数据流

```
┌────────────────── 主进程（持有密钥）───────────────────┐
│  密文文件 → 分块 AEAD 解密 → 明文字节                   │
└───────────────────────┬───────────────────────────────┘
                        │ pipe (stdin)
┌───────────────────────▼───────────────────────────────┐
│  FFmpeg 子进程（沙箱 + 降权 + 无密钥 + 无网络）          │
│    demux / remux / decode                              │
└───────────────────────┬───────────────────────────────┘
                        │ pipe (stdout)
┌───────────────────────▼───────────────────────────────┐
│  主进程 → fMP4 片段 → WebView (MSE)                    │
└────────────────────────────────────────────────────────┘
```

---

## 15. 关键技术验证（必须最先做）

⚠️ **以下 spike 是整个方案的技术命门，必须在正式开发前验证。**

| # | 验证项 | 失败后果 | 备选方案 |
|---|---|---|---|
| **S1** | **iOS WKWebView 中自定义协议 + `<video>` Range seek** | 🔴 P1 路径在 iOS 上不可用 | 退回本地 HTTP server（localhost） |
| **S2** | **Android WebView 各版本的 Range 行为一致性** | 🟡 部分机型播放异常 | 同上 |
| S3 | ManagedMediaSource 在 iOS 17.1+ 的实际可用性 | 🟡 P2/P3 在 iPhone 不可用 | 移动端只支持 P1 |
| S4 | 自建 FFmpeg 交叉编译链（Android NDK + iOS xcframework） | 🟡 移动端无 P2 | 移动端只支持 P1 |
| S5 | Android ProxyFileDescriptor 的随机读性能 | 🟢 外部打开体验降级 | 落临时文件 |
| S6 | WebView 缓存是否真的被 `no-store` 阻止（各平台实测） | 🔴 明文泄露到磁盘 | 手动清理 + 加密临时目录 |

**S1 和 S6 必须在第一周完成。**
