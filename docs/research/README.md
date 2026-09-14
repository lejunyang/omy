# omy 设计文档

跨平台加密文件管理应用的完整技术设计。Rust + Tauri v2，覆盖桌面（Windows / macOS / Linux）与移动端（Android / iOS）。

> **文档状态**：01–12 为设计定稿（v1.0）。
> 13 是**实现期新增的待评审方案**，不属于 v1.0 定稿范围。
> **最后更新**：2026-09-12

---

## 这是什么

一个把任意文件加密成自定义 `.omy` 格式的应用，核心差异点在于**加密后依然能直接用**：

- 视频加密后可在应用内**任意拖动播放**，无需先解密落盘
- 图片/音频/文本同样支持内嵌预览
- 文件列表直接显示**解密后的原文件名和缩略图**
- 自动扫描授权目录，密码匹配的文件自动"显形"
- 局域网内其他设备可浏览、播放（**只传密文**，密钥不出本机）

---

## 文档索引

| # | 文档 | 内容 | 主要读者 |
|---|---|---|---|
| 01 | [需求与威胁模型](01-requirements-and-threat-model.md) | 需求清单、威胁模型、**明确的非目标** | 所有人 |
| 02 | [文件格式规范](02-file-format-spec.md) | `.omy` 二进制格式、TLV、分片格式、测试向量 | 实现者、第三方兼容实现 |
| 03 | [密钥体系](03-key-management.md) | 两级 KDF、key slot、生物识别、恢复方案、可否认性 | 实现者、安全审计 |
| 04 | [媒体播放架构](04-media-playback.md) | 分块 AEAD、Range 映射、三条播放路径、字幕、转码 | 实现者 |
| 05 | [文件夹与分片](05-container-and-sharding.md) | 容器模式 / 树形模式、元数据保留、分片合并 | 实现者 |
| 06 | [局域网共享](06-lan-sharing.md) | 设备发现、配对、密文传输、权限与吊销 | 实现者 |
| 07 | [平台适配与旁路防护](07-platform-and-sidechannels.md) | 五平台能力矩阵、**旁路泄露清单**、沙箱隔离 | 实现者、安全审计 |
| 08 | [UI/UX 设计](08-ui-ux-design.md) | 界面结构、交互流程、视觉规范、多语言 | 设计、前端 |
| 09 | [CLI 设计](09-cli-design.md) | 命令行接口完整规格 | 实现者、高级用户 |
| 10 | [许可证与专利合规](10-licensing-and-patents.md) | GPL/LGPL 分层、HEVC 专利、出口管制 | 所有人、法务 |
| 11 | [工程实现路线](11-engineering-roadmap.md) | crate 分层、技术验证清单、风险登记册 | 实现者、项目管理 |
| 12 | [设计决策记录](12-decision-log.md) | 关键决策及其理由，含**被否决的方案** | 所有人 |
| 13 | [FFmpeg 裁剪内置方案](13-ffmpeg-minimal-build.md) | 裁剪构建的体积实测与估算、专利取舍、configure 配方 | 实现者、法务 |

**附录**
- [appendix/ui-prototype.html](appendix/ui-prototype.html) — **可交互界面原型**（浏览器直接打开，可切换深/浅主题、网格/列表、锁定/解锁态）
- [appendix/test-vectors.json](appendix/test-vectors.json) — 5 组确定性测试向量
- [appendix/verification-report.md](appendix/verification-report.md) — 验证报告（格式 68 + 交叉验证 98 + UI 原型 23 项断言）
- [reference/](reference/) — Python 参考实现（可运行，用于验证格式设计）

---

## 快速理解：三个核心设计

### 1. 两级 KDF —— 让"扫描上万个文件"成为可能

```
密码 ──Argon2id（~180ms，每密码每会话仅一次）──▶ 库主密钥 KEK
                                                     │
文件 header 的 file_uuid ────────────────────────────┤
                                                     ▼
                                      HKDF（~13µs）──▶ slot 包裹密钥
                                                     │
                                                     ▼
                                              解包得到 FEK
```

实测（本机基准，见 [验证报告](appendix/verification-report.md)）：

| 方案 | 500 文件 × 5 密码 | 推算 10000 文件 |
|---|---|---|
| **两级 KDF（本设计）** | **171 ms** | **~3.4 s** |
| 每文件独立 Argon2 | 450 s | ~2.5 小时 |

且 slot 区经统计检验与纯随机数据不可区分 → **同时获得可否认性**。

### 2. 分块 AEAD —— 视频任意拖动的基础

明文切成固定块，每块独立 AEAD 加密，nonce 绑定块序号：

```
块号     = 明文偏移 / chunk_size
密文偏移 = header_len + 块号 × (chunk_size + 16)
```

**O(1) 随机访问**，无需索引。播放器 seek 时只解密涉及的 1~2 个块。

不需要 DRM 式帧加密，也不需要 faststart 重整——原文件字节零改动，bit-for-bit 可还原。

### 3. 三条播放路径 —— 兼容性与性能的平衡

| 路径 | 适用 | 机制 | 速度 |
|---|---|---|---|
| ⚡ **P1 直通** | H.264/AAC MP4、WebM、JPEG… | 自定义协议 + Range | 毫秒级 seek |
| 🔄 **P2 转封装** | MKV(H.264/HEVC+AAC) | FFmpeg demux → fMP4 → MSE | 数十毫秒 |
| 🐌 **P3 转码** | DTS/TrueHD/XviD | FFmpeg 全解码 | 数百毫秒~秒 |

加密时探测一次并**把结果写进 header**，列表页无需解密即可判断。UI 上显示对应图标，用户明确知道为什么某些文件慢。

---

## 关键取舍（务必阅读）

| 取舍 | 说明 | 详见 |
|---|---|---|
| **魔数可识别** | 文件头有明显 magic，任何人都能看出"这是加密文件"。这是设计**目标**（威胁模型不要求隐蔽），不是缺陷 | [01](01-requirements-and-threat-model.md) |
| **伪装模式只挡随手点开** | `binwalk` 一眼看穿，不是隐藏卷 | [03](03-key-management.md) |
| **转码 ≠ 可还原** | 启用转码选项后存的不是原字节，**无法 bit-for-bit 还原**。三种模式让用户明确选择 | [04](04-media-playback.md) |
| **FFmpeg 是最大攻击面** | 为播放便利引入了 CVE 高发的 C 代码库，必须子进程 + 沙箱隔离 | [07](07-platform-and-sidechannels.md) |
| **iOS 无法全盘扫描** | 系统限制，只能扫沙盒 + 用户授权书签目录 | [07](07-platform-and-sidechannels.md) |
| **SSD 无法安全擦除** | 不提供虚假的"安全删除"按钮，只做诚实告知 | [07](07-platform-and-sidechannels.md) |
| **GUI 必须 GPL-3.0** | 因为链接 FFmpeg。核心库保持 MIT/Apache 双授权 | [10](10-licensing-and-patents.md) |

---

## 许可证结构

```
omy-core / omy-format / omy-cli    MIT OR Apache-2.0    零 FFmpeg 依赖
omy-media（FFmpeg 封装）                  LGPL-2.1（子进程调用）
omy GUI                              GPL-3.0
```

第三方可以只取 `omy-format` 实现自己的兼容工具，不受 GPL 约束。详见 [10-许可证与专利合规](10-licensing-and-patents.md)。

---

## 参考实现

`reference/` 下的 Python 实现**不是玩具**——它完整实现了格式规范，并通过 68 项断言验证了往返一致性、随机访问、篡改检测、可否认性、分片重组、KDF 性能等全部关键设计。

```bash
cd reference
pip install cryptography argon2-cffi zstandard
python3 test_omy.py      # 运行 68 项自测
python3 gen_vectors.py    # 重新生成测试向量
```

Rust 生产实现必须能通过同一组测试向量。
