# 11 · 工程实现路线

> 用户明确「你只需要产出设计文档，先不用考虑分期」（D-原始需求）。
> 因此**本文档不排期、不定里程碑**，只给出：模块划分、依赖关系、必须先做的技术验证、风险登记册与测试策略。

## 1. Crate 分层

```
cvault/
├── crates/
│   ├── cvault-core/          MIT OR Apache-2.0 · 零 FFmpeg 依赖
│   │   ├── format/           .cvlt 二进制读写
│   │   ├── crypto/           KDF / AEAD / slot
│   │   ├── container/        目录容器与树形模式
│   │   ├── shard/            分片切分与合并
│   │   └── source/           BlockSource trait（本地 / 远程统一抽象）
│   │
│   ├── cvault-media/         LGPL-2.1+ · FFmpeg 封装
│   │   ├── probe/            格式探测与播放路径决策
│   │   ├── remux/            P2 转封装
│   │   ├── decode/           P3 全解码
│   │   ├── thumbnail/        缩略图与自选帧
│   │   └── transcode/        D-26 转码（可选 feature）
│   │
│   ├── cvault-net/           MIT OR Apache-2.0
│   │   ├── discovery/        mDNS
│   │   ├── pairing/          SPAKE2
│   │   └── transport/        Noise IK
│   │
│   ├── cvault-cli/           GPL-3.0
│   └── cvault-gui/           GPL-3.0 · Tauri v2
│
└── spec/                     CC BY 4.0 · 格式规范 + 测试向量
```

### 1.1 依赖方向（强制单向）

```
gui ──┬──> media ──> core
      ├──> net   ──> core
      └──> core

cli ──┴──> （同上）
```

**禁止反向依赖。** `cvault-core` 不得知道 media / net / gui 的存在。

### 1.2 `BlockSource` 是关键抽象

本地文件与局域网远程文件必须走同一套上层逻辑，否则播放器、缩略图、扫描要各写两遍。

```rust
#[async_trait]
pub trait BlockSource: Send + Sync {
    async fn header(&self) -> Result<&Header>;
    async fn read_chunk(&self, index: u64) -> Result<Bytes>;
    async fn chunk_count(&self) -> Result<u64>;
    fn is_remote(&self) -> bool;
}
```

实现者：`LocalFile` / `ShardedFile` / `RemoteDevice` / `ContainerEntry`。

这个抽象直接决定了 D-04（局域网只传密文）能否干净落地——远程实现返回的就是密文块，解密统一在上层做。

---

## 2. 必须先做的技术验证（Spike）

这些不是"开发任务"，而是**在投入大量实现前必须先确认可行**的技术点。若某项失败，架构需要调整。

| # | 验证内容 | 失败的后果 | 优先级 |
|---|---|---|---|
| **S1** | Tauri v2 自定义协议返回 206 + Content-Range，`<video>` 能任意 seek | **整个播放方案不成立**，需退回本地 HTTP server | 🔴 最高 |
| **S2** | Android `content://` URI + SAF 读取外部文件 | 移动端只能做导入式保险箱 | 🔴 高 |
| **S3** | FFmpeg 交叉编译到 Android（含 16 KB 页对齐） | 移动端无 P2/P3，只剩系统解码器 | 🔴 高 |
| **S4** | Android `ProxyFileDescriptorCallback` 向外部应用提供流式解密 | "用外部应用打开"需落临时明文文件 | 🟡 中 |
| **S5** | WebView 是否缓存自定义协议响应到磁盘（**L1 泄露**） | 解密后的媒体数据落盘，安全目标受损 | 🔴 最高 |
| **S6** | iOS `ManagedMediaSource` / MSE 可用性与编码支持范围 | iOS 播放能力进一步受限 | 🟡 中（D-06 优先级最低） |
| **S7** | mDNS 在各平台的权限与可靠性（尤其 iOS 本地网络权限） | 局域网发现需改为手动输入 IP | 🟡 中 |
| **S8** | 系统 HEIC 解码器在各平台的实际可用性 | HEIC 预览覆盖率低于预期 | 🟢 低 |

> **S1 与 S5 必须最先做。** 它们分别决定"能不能播"和"播了会不会泄露"，是整个媒体方案的地基。

### 2.1 S5 的验证方法（安全关键）

不能只看文档，必须实测：

1. 播放一个大视频，seek 若干次
2. 检查 WebView 缓存目录（各平台路径不同）
3. 用 `grep` 在缓存文件中搜索已知的明文特征字节
4. 若命中 → 必须在响应头强制 `Cache-Control: no-store`，并重新验证

---

## 3. 风险登记册

| ID | 风险 | 影响 | 应对 |
|---|---|---|---|
| R1 | Tauri 自定义协议 Range 支持不完整 | 🔴 播放方案失效 | S1 先验；备选：本地 HTTP server（有端口/CORS 代价） |
| R2 | WebView 缓存解密数据落盘 | 🔴 安全目标受损 | S5 先验；`no-store` + 定期清理 |
| R3 | 移动端 FFmpeg 维护负担 | 🟡 长期成本 | FFmpegKit 已退役 → 自维护交叉编译链；优先系统解码器（D-24） |
| R4 | iOS 限制过多导致体验割裂 | 🟡 | 已接受（D-06 降级为导入式保险箱） |
| R5 | HEVC 专利主张 | 🟡 法律 | 分层策略（D-12）+ PATENTS.md 声明 |
| R6 | 大文件加密中断导致损坏 | 🟡 数据 | 原子写入（临时文件 + rename）+ 冗余 header |
| R7 | 用户遗忘密码导致永久丢失 | 🔴 数据 | 恢复码机制 + UI 强提示；**设计上无后门** |
| R8 | 分片丢失 | 🟡 数据 | 冗余 header（D-18）+ 缺片降级播放 |
| R9 | 转码不可逆但用户误操作 | 🔴 数据 | 强制二次确认（见 08 §3.3、09 §5.1）+ 默认 `none` |
| R10 | 局域网共享被未授权设备访问 | 🟡 安全 | SPAKE2 配对 + Noise IK + 有效期 + 吊销 |
| R11 | 格式演进破坏兼容 | 🟡 | major/minor 版本策略已验证（测试 §9）；预留 TLV 扩展空间 |
| R12 | 依赖引入 copyleft 污染 core | 🟡 法律 | `cargo-deny` CI 白名单强制 |

---

## 4. 测试策略

### 4.1 已完成的验证（参考实现）

本设计并非纸上谈兵——已用 Python 参考实现验证了格式规范的可实现性与自洽性：

| 套件 | 项数 | 结果 |
|---|---|---|
| `reference/test_cvlt.py` | 68 | ✅ 全部通过 |
| `verify_spec.py`（文档 ↔ 实现交叉验证） | 98 | ✅ 全部通过 |

详见 [appendix/verification-report.md](appendix/verification-report.md)。

### 4.2 Rust 实现必须继承的测试

| 层级 | 内容 |
|---|---|
| **单元测试** | 每个 TLV 类型的编解码往返；padding 桶边界（尤其 `len=63/64/65`）；nonce 构造 |
| **测试向量** | 5 组向量必须能被 Rust 实现解密，且结果与 `test-vectors.json` 记录**逐字节一致** |
| **属性测试** | `proptest`：任意字节序列加密后解密 == 原文（bit-for-bit，D-07） |
| **模糊测试** | `cargo-fuzz` 喂畸形 header —— 必须**永不 panic**，只返回错误 |
| **并发测试** | 多线程同时读同一文件的不同块 |
| **跨实现验证** | Rust 加密 → Python 参考实现解密，以及反向 |
| **CLI ↔ GUI** | 二者产物互相可读（见 09 §10） |

### 4.3 必须覆盖的边界用例

来自参考实现中**实际发现过 bug** 的位置：

| 用例 | 为什么重要 |
|---|---|
| 文件名长度 = 62 / 63 / 64 / 65 字节 | 参考实现在此处出现过负填充 bug（见 12 号文档 DEC-11） |
| 空文件（0 字节） | 最小文件 672 B；块数为 0 |
| 单字节文件 | 单块且为 final chunk |
| 恰好等于块大小的文件 | 边界：是 1 块还是 2 块 |
| 8 个 slot 全占满 | slot 区无填充空间 |
| 分片恰好在块边界 / 块中间切开 | 合并逻辑 |
| 缺第一片（含主 header） | 需靠冗余 header 恢复 |
| TLV 长度声明超出实际数据 | 畸形输入，必须拒绝而非崩溃 |

### 4.4 性能回归基线

以下为参考实现实测值，Rust 实现应显著优于（同机对比）：

| 指标 | 参考实现（Python） |
|---|---|
| Argon2id (m=64MiB, t=3, p=1) | 180 ms |
| HKDF 单次 | 13.2 µs |
| 比值 | ~13,700× |
| 扫描 500 文件 × 5 密码 | 170 ms（500/500 命中） |
| 推算 10,000 文件 | ~3.4 s |
| 对照：每文件独立 Argon2 | 394 s（慢 2,324×） |

> 两级 KDF 不是优化，是**可行性前提**。没有它，扫描功能根本无法使用。

---

## 5. CI 强制门禁

| 检查 | 失败即阻断 |
|---|---|
| `cargo-deny` 许可证白名单（core 禁 copyleft） | ✅ |
| `cvault-core` 依赖树不含 FFmpeg | ✅ |
| 5 组测试向量逐字节比对 | ✅ |
| `cargo-fuzz` 冒烟（畸形 header 不 panic） | ✅ |
| `cargo audit` 无已知漏洞 | ✅ |
| 跨实现验证（Rust ↔ Python） | ✅ |
| 可复现构建校验 | ⚠️ 警告 |

---

## 6. 发布前检查清单

- [ ] 5 组测试向量在全部目标平台通过
- [ ] S1 / S5 两项安全关键 spike 已实测确认
- [ ] `THIRD-PARTY-NOTICES.md` 由 `cargo-about` 自动生成且最新
- [ ] `PATENTS.md` 已包含专利声明
- [ ] BIS / NSA 出口管制通知已发送（见 10 号文档 §4.1）
- [ ] FFmpeg 构建确认为 LGPL 配置（无 `--enable-gpl`）
- [ ] 简中 / 英文文案完整，无硬编码字符串
- [ ] 危险操作（删原文件 / 转码 / 移除 slot）的二次确认已实装
- [ ] 恢复码流程端到端可用
- [ ] 锁定态不泄露文件名 / 缩略图 / 元信息（见 08 §10）
