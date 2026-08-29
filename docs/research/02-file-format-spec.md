# 02 · 文件格式规范（.cvlt v1.0）

> 本规范完整到足以支持第三方独立实现。所有偏移量、字段长度均已通过[参考实现](reference/)验证，并附有[确定性测试向量](appendix/test-vectors.json)。

## 1. 总体布局

```
┌──────────────────────────────────────────────┐  offset 0
│  Fixed Header            96 bytes            │
├──────────────────────────────────────────────┤  96
│  Key Slot Area           48 × 8 = 384 bytes  │
├──────────────────────────────────────────────┤  480
│  TLV Extension Area      variable            │
├──────────────────────────────────────────────┤  480 + tlv_len
│  Header MAC              32 bytes            │
├──────────────────────────────────────────────┤  header_len
│  Encrypted Payload       variable            │
│    chunk[0]  ciphertext + 16B tag            │
│    chunk[1]  ciphertext + 16B tag            │
│    ...                                       │
└──────────────────────────────────────────────┘
```

**字节序**：所有多字节整数使用**小端序（little-endian）**，唯一例外是 nonce 中的块序号（大端序，见 §5.2）。

**最小文件尺寸**：672 字节（空文件 + 单密码 + 加密文件名）。

---

## 2. Fixed Header（96 字节）

| 偏移 | 长度 | 字段 | 类型 | 说明 |
|-----:|-----:|------|------|------|
| 0 | 8 | `magic` | bytes | `43 56 41 55 4C 54 01 00` = `"CVAULT"` + 0x01 + 0x00 |
| 8 | 2 | `version_major` | u16 | 当前 `1`。**不认识则必须拒绝打开** |
| 10 | 2 | `version_minor` | u16 | 当前 `0`。不认识可安全忽略 |
| 12 | 4 | `header_len` | u32 | 从文件起始到载荷起始的总字节数 |
| 16 | 16 | `file_uuid` | bytes | 随机生成，用于 nonce 派生、AAD 绑定、分片归属 |
| 32 | 16 | `vault_salt` | bytes | Argon2id 盐。**同一 vault 内所有文件相同**（见 §3.3） |
| 48 | 4 | `flags` | u32 | 位标志，见 §2.1 |
| 52 | 1 | `cipher_id` | u8 | 1=XChaCha20-Poly1305, 2=AES-256-GCM |
| 53 | 1 | `kdf_id` | u8 | 1=Argon2id |
| 54 | 1 | `compress_id` | u8 | 0=none, 1=zstd |
| 55 | 1 | `slot_count` | u8 | 固定为 `8` |
| 56 | 4 | `argon2_m_kib` | u32 | 内存成本（KiB） |
| 60 | 4 | `argon2_t` | u32 | 迭代次数 |
| 64 | 4 | `argon2_p` | u32 | 并行度 |
| 68 | 4 | `chunk_size` | u32 | 明文分块大小（字节） |
| 72 | 8 | `plaintext_size` | u64 | 原始明文总长度 |
| 80 | 7 | `base_nonce` | bytes | 随机，与块序号组合成完整 nonce |
| 87 | 1 | `chunk_version` | u8 | 保留给未来的就地编辑功能，当前必须为 `0` |
| 88 | 4 | `tlv_len` | u32 | TLV 区总字节数 |
| 92 | 4 | `reserved` | u32 | 必须为 `0`，读取时忽略 |

> **为什么 KDF 参数必须存在 header 里**：换机器、换版本后仍需能解开文件。参数被 header MAC 覆盖，无法被篡改降级（威胁 T7）。

### 2.1 flags 位定义

| 位 | 名称 | 含义 |
|---|------|------|
| 0 | `FILENAME_ENCRYPTED` | TLV 中存有加密的原文件名 |
| 1 | `EXT_PRESERVED` | 后缀以明文单独存放（`TLV_PLAIN_EXT`） |
| 2 | `COMPRESSED` | 载荷分块经 zstd 压缩，**必须**存在压缩索引表 |
| 3 | `SHARDED` | 本文件是分片集合的一部分 |
| 4 | `HAS_THUMBNAIL` | TLV 中存有加密缩略图 |
| 5 | `CONTAINER` | 容器模式（整个文件夹打包） |
| 6 | `DISGUISED` | 伪装模式，真实 header 不在文件起始处 |
| 7 | `TRANSCODED` | ⚠️ 载荷**非原始字节**（转码过），无法 bit-for-bit 还原 |
| 8 | `META_NORMALIZED` | 时间戳等元数据已被抹平 |
| 9 | `SIZE_PADDED` | 载荷尾部有随机填充以隐藏真实大小 |
| 10–31 | 保留 | 必须为 0 |

---

## 3. Key Slot Area（384 字节）

### 3.1 结构

固定 **8 个 slot**，每个 **48 字节**：

```
slot[i] = AEAD_encrypt(
    key   = HKDF-SHA256(KEK_i, salt=file_uuid, info="cvault/v1/slot" || u16le(i)),
    nonce = 0x000000000000000000000000,      // 12 字节全零
    pt    = FEK,                              // 32 字节
    aad   = none
)                                             // = 32B 密文 + 16B tag = 48B
```

**零 nonce 是安全的**：每个 slot 的包裹密钥都由 `(KEK, file_uuid, slot_index)` 唯一确定，同一密钥永不重复使用。

### 3.2 可否认性：固定槽数 + 随机填充

**未使用的 slot 必须填充密码学安全随机字节**，不得填零、不得留空。

这样从外部看：
- 无法判断文件有几个真实密码
- 无法判断是否存在"另一个密码集"
- 一个密码的文件和三个密码的文件**字节长度完全相同**

**验证结果**（见[验证报告](appendix/verification-report.md) §3）：

| 检验项 | 结果 |
|---|---|
| 1 密码 vs 3 密码文件大小 | 772 B vs 772 B ✅ 完全相同 |
| slot 区香农熵 | 7.491 bits/byte，落在纯随机基线 [7.332, 7.508] 内 ✅ |
| 真实 slot 段 vs 随机填充段熵差 | 0.506，未超随机对照组 p95 = 0.597 ✅ |

> **注意统计方法**：384 字节样本对 256 种取值严重欠采样，两段纯随机数据之间的熵差中位数就有约 0.46。因此判据必须是"与同长度随机对照组比较"，而非某个凭直觉设定的绝对阈值。

### 3.3 vault_salt 的设计权衡

`vault_salt` 存在**每个文件的 header 里**，且同一 vault 内所有文件**共用同一个值**。

**为什么不每文件独立 salt**：那样每个文件都要跑一次 Argon2id（~180ms），扫描 10000 个文件需要约 2.5 小时。

**为什么不把 salt 只存在应用配置里**：重装系统 / 换设备后无法恢复，数据永久丢失。

**共用 salt 是否破坏可否认性**：不会。因为同一 vault 下的**多个密码共用同一 salt**：

```
Argon2id(密码甲, vault_salt) → KEK甲 → 打开文件集 A（真实）
Argon2id(密码乙, vault_salt) → KEK乙 → 打开文件集 B（诱饵）
```

外部观察者只能看到"这些文件属于同一个 vault"，**无法区分哪些文件属于哪个密码**。已验证：同密码加密的不同文件，其 slot 区内容完全不同（因为 `file_uuid` 不同）。

### 3.4 slot 类型

slot 本身不存类型标识（那会泄露信息）。类型信息存放于 TLV 区或应用侧的 vault 配置：

| 类型 | KEK 来源 | 扫描时是否尝试 | 用途 |
|---|---|---|---|
| **vault slot** | `Argon2id(密码, vault_salt)`（会话内缓存） | ✅ 尝试（微秒级） | 日常使用 |
| **device slot** | OS 密钥库解包出的设备密钥 | ✅ 尝试 | 生物识别 |
| **portable slot** | `Argon2id(密码, 本文件独立 salt)` | ❌ 跳过 | 单独分享出去的文件 |
| **recovery slot** | 恢复码直接作为 KEK（熵足够，无需 KDF） | ❌ 跳过 | 灾难恢复 |

portable slot 的独立 salt 存放于 `TLV_PORTABLE_SLOT`，仅在用户**手动打开单个文件**时才尝试（可接受几百毫秒延迟）。

详见 [03-密钥体系](03-key-management.md)。

---

## 4. TLV Extension Area

### 4.1 编码

```
┌────────┬────────┬────────┬─────────────┐
│ type   │ flags  │ length │   value     │
│ u16le  │ u16le  │ u32le  │  length B   │
└────────┴────────┴────────┴─────────────┘
```

条目连续排列，总长度由 `tlv_len` 界定。

### 4.2 TLV flags

| 位 | 名称 | 语义 |
|---|------|------|
| 0 | `CRITICAL` | **不认识此类型时必须拒绝打开文件** |
| 1 | `ENCRYPTED` | value 是密文，需用 FEK 派生密钥解密 |
| 2–15 | 保留 | 必须为 0 |

> **critical 位是格式可扩展性的关键**。类似 PNG chunk 的大小写位机制：未来加入"必须理解才能正确解读文件"的字段时（如新的载荷编码方式），置 critical；加入纯附加信息时（如新的元数据），不置 critical，老版本可安全忽略。
>
> 已验证：未知的非 critical TLV（type=0xF000）能被正确忽略并保留原值。

### 4.3 加密 TLV 的密钥派生

```
key   = HKDF-SHA256(FEK, salt="", info="cvault/v1/tlv" || u16le(tlv_type))
nonce = 0x000000000000000000000000
value = AEAD_encrypt(key, nonce, plaintext, aad=none)
```

每种 TLV 类型使用独立派生密钥，零 nonce 安全（密钥唯一）。

### 4.4 已分配类型

| Type | 名称 | flags | 内容 |
|-----:|------|-------|------|
| 0x0001 | `FILENAME` | CRITICAL+ENC | `u16le 真实长度` + UTF-8 文件名 + 零填充至 64B 桶 |
| 0x0002 | `PLAIN_EXT` | — | 明文后缀（不含点）。仅当 `EXT_PRESERVED` 时存在 |
| 0x0003 | `THUMBNAIL` | ENC | WebP/JPEG 缩略图，建议 ≤ 32 KB |
| 0x0004 | `MEDIA_META` | ENC | JSON：宽高、时长、编码、码率、播放路径分级 |
| 0x0005 | `COMPRESSION_INDEX` | CRITICAL+ENC | 压缩块索引表，见 §6.2 |
| 0x0006 | `MOOV_CACHE` | ENC | MP4 的 moov box 副本，加速起播（见 [04](04-media-playback.md)） |
| 0x0007 | `SHARD_INFO` | CRITICAL | 分片总数、每片大小 |
| 0x0008 | `ORIGINAL_META` | ENC | JSON：mtime/ctime/权限位/xattr（见 [05](05-container-and-sharding.md)） |
| 0x0009 | `CONTENT_HASH` | ENC | 原始明文的 BLAKE2b-256，解密后自检 |
| 0x000A | `ZSTD_DICT` | ENC | zstd 训练字典（可选高级功能） |
| 0x000B | `PORTABLE_SLOT` | — | portable slot 的独立 Argon2 salt + 参数 |
| 0x000C | `SUBTITLES` | ENC | 内嵌/外挂字幕，见 [04](04-media-playback.md) |
| 0x000D | `TRANSCODE_INFO` | ENC | 转码参数与原始文件哈希（`TRANSCODED` 时必须存在） |
| 0x000E | `FOLDER_INDEX` | CRITICAL+ENC | 容器模式的目录索引，见 [05](05-container-and-sharding.md) |
| 0x000F | `DISGUISE_INFO` | — | 伪装模式的真实 header 偏移 |
| 0x0010–0x7FFF | 保留 | | 供本规范未来扩展 |
| 0x8000–0xFFFF | 私有 | | 第三方实现自用，官方实现必须忽略 |

### 4.5 文件名长度 padding

文件名 padding 到 **64 字节的整数倍**，隐藏真实长度：

```
need    = 2 + len(name_utf8)          // 含长度前缀
bucket  = ceil(need / 64) * 64
padded  = u16le(真实长度) || name_utf8 || 0x00 × (bucket - need)
```

⚠️ **实现陷阱**：桶大小必须基于 `2 + 名字长度` 计算，**不能**只基于名字长度。若写成 `(len // 64 + 1) * 64`，当 `len = 63` 时会算出负填充，`padded` 实际长度变成 65 字节、越出桶边界，该长度的文件名反而**暴露了自己的长度特征**。参考实现在交叉验证中命中过这个 bug。

已验证：长度 0–199 及 250/300 的文件名共落入 5 个桶，**同一桶内 `header_len` 完全唯一**：

| bucket | header_len | 覆盖的文件名字节长度 |
|---:|---:|---|
| 64 | 656 | 0 – 62 |
| 128 | 720 | 63 – 126 |
| 192 | 784 | 127 – 190 |
| 256 | 848 | 191 – 254 |
| 320 | 912 | 255 – 318 |

---

## 5. 载荷：分块 AEAD

### 5.1 分块

明文按 `chunk_size` 切分，每块**独立**加密：

```
n_chunks = max(1, ceil(plaintext_size / chunk_size))
```

空文件也有 1 个块（零长度明文 + 16 字节 tag）。

### 5.2 nonce 构造

```
nonce(i) = base_nonce(7B) || u32be(i) || final_flag(1B)
                                          0x01 = 最后一块
                                          0x00 = 其他
```

共 12 字节。

- **`u32be(i)` 是全文档唯一使用大端序的地方**，遵循 STREAM 构造惯例
- `final_flag` 用于**防截断攻击**：删掉尾部块后，新的"最后一块"其 flag 不匹配，解密失败
- 块序号上限 2³² → 配合 64 KiB 最小块大小，单文件上限 256 TiB

> **XChaCha20-Poly1305 的 24 字节 nonce**：生产实现使用 XChaCha20 时，前 12 字节为随机值，后 12 字节按上式构造。参考实现使用标准 ChaCha20-Poly1305（12B nonce）以便用通用库验证。

### 5.3 AAD 绑定

```
aad(i) = file_uuid(16B) || u32be(i)
```

作用：
- 绑定文件身份 → **防跨文件块移植**
- 绑定块序号 → **防块重排**

已验证：把文件 A 的块 0 移植到文件 B（即使同一 vault、同一密码），解密必定失败。

### 5.4 载荷密钥

```
payload_key = HKDF-SHA256(FEK, salt=file_uuid, info="cvault/v1/payload")
```

与 slot 包裹密钥、header MAC 密钥、TLV 密钥完全域分隔。

### 5.5 O(1) 随机访问（未压缩）

这是**视频任意 seek 的数学基础**：

```
chunk_index  = plain_offset / chunk_size
ct_offset(i) = header_len + i × (chunk_size + 16)
ct_len(i)    = min(chunk_size, plaintext_size - i × chunk_size) + 16
```

无需任何索引表。已验证：`chunk_ct_range(5)` 返回值与公式计算结果精确相等。

**读取任意 Range 的最小解密量**：

| 请求 | 解密块数 |
|---|---|
| 块内任意区间 | 1 |
| 跨一个边界 | 2 |
| 长度 L 的区间 | `ceil(L / chunk_size) + 1` 上界 |

---

## 6. 压缩

### 6.1 压缩改变了什么

压缩**必须在加密之前**（密文不可压缩）。但每块独立压缩后长度可变，§5.5 的线性公式**失效**，必须依赖索引表。

| | 未压缩 | 压缩 |
|---|---|---|
| 偏移计算 | O(1) 公式 | 查索引表 |
| 索引表 | 不需要 | **必需**（CRITICAL TLV） |
| seek 成本 | 常数 | 常数（表在内存） |

### 6.2 压缩索引表格式

`TLV_COMPRESSION_INDEX` 的明文内容：

```
u32le  entry_count
repeat entry_count times:
    u64le  ct_offset      // 相对载荷起始的密文偏移
    u32le  ct_len         // 该块密文长度（含 tag）
    u32le  plain_len      // 该块解压后的明文长度
```

每条 16 字节。1 GiB 文件 / 1 MiB 块 → 1024 条 → 16 KB 索引，可完全载入内存。

### 6.3 块大小对压缩率的影响（实测）

用户提出的"加大块能否挽回压缩率"——**实测确认可以，且效果显著**。

测试语料：2 MB 高重复度数据，zstd level 3

| chunk_size | 输出大小 | 压缩比 |
|---|---:|---:|
| 64 KiB | 133,515 B | 6.52% |
| 256 KiB | 34,004 B | 1.66% |
| 1 MiB | 9,158 B | 0.45% |
| **4 MiB** | **5,017 B** | **0.24%** |

4 MiB 块比 64 KiB 块**小 96.2%**。

### 6.4 推荐默认值

| 内容类型 | chunk_size | 压缩 | 理由 |
|---|---|---|---|
| 视频 / 音频 / 已压缩图片 | 256 KiB – 1 MiB | **关** | 本身已压缩，zstd 收益 <2%；小块 seek 更细腻 |
| 文本 / 文档 / 代码 | 1 – 4 MiB | 开（level 3–9） | 大块基本追平全文件压缩率 |
| 大型归档 | 4 – 16 MiB | 开（level 9–19） | 极致压缩率 |

**可配置范围**：64 KiB – 16 MiB。

**UI 必须呈现的三维权衡**：
```
压缩率  ←→  seek 粒度  ←→  内存峰值
 大块优         小块优        小块优
```

⚠️ **移动端必须限制上限**：4 MiB 块 × 并发预读 3 块 = 12 MB 瞬时占用，低端机需下调。建议移动端默认上限 1 MiB。

### 6.5 zstd 训练字典（可选高级功能）

对大量小块结构化数据，训练字典可提升压缩率 30%–50%。字典加密后存入 `TLV_ZSTD_DICT`。

⚠️ **副作用**：zstd 官方 issue 报告小块 + 共享字典场景下**解压速度反而下降**。因此本功能**默认关闭**，仅作为高级选项提供。

---

## 7. Header MAC

```
mac_key = HKDF-SHA256(FEK, salt=file_uuid, info="cvault/v1/header-mac")
mac     = HMAC-SHA256(mac_key, fixed_header || slot_area || tlv_blob)
```

覆盖 **header_len - 32** 字节，即除 MAC 自身外的全部头部内容。

**防护的攻击**（均已验证）：

| 攻击 | 检测 |
|---|---|
| 篡改 Argon2 参数（m=64MiB → 8KiB 降级） | ✅ |
| 篡改 flags（如关闭 COMPRESSED 导致误读） | ✅ |
| 篡改 chunk_size / plaintext_size | ✅ |
| 增删/修改 TLV 条目 | ✅ |
| 替换 slot 区 | ✅ |

**验证时机**：解包出 FEK 后**立即**验证 MAC，验证失败必须终止，不得继续读取载荷。

---

## 8. 分片格式

### 8.1 分片头（56 字节）

| 偏移 | 长度 | 字段 | 说明 |
|-----:|-----:|------|------|
| 0 | 8 | `magic` | `"CVSHARD"` + 0x01 |
| 8 | 16 | `file_uuid` | 与主文件一致，用于归属判定 |
| 24 | 4 | `shard_index` | 从 0 开始 |
| 28 | 4 | `shard_total` | 总片数 |
| 32 | 8 | `data_offset` | 本片数据在原密文中的偏移 |
| 40 | 8 | `data_len` | 本片数据长度 |
| 48 | 4 | `crc32` | 本片数据的 CRC-32（IEEE） |
| 52 | 4 | `redundant_header_len` | 冗余主 header 长度，0 表示无 |

之后是可选的冗余主 header 副本，再之后是数据。

### 8.2 冗余 header

`shard_index > 0` 的分片可携带完整主 header 副本。第 0 片本身就含主 header，无需冗余。

**收益**：任意一片都能独立识别文件身份、读出加密文件名和缩略图。即使第 0 片丢失，UI 仍能正确显示这个文件的信息。

**代价**：每片增加 `header_len` 字节（典型 656 B – 20 KB，取决于是否含缩略图）。

已验证：第 1/2/3 片均可通过 `recover_header_from_shard()` 独立恢复出有效 header。

### 8.3 合并规则

1. 校验所有片的 `magic`
2. 校验所有片的 `file_uuid` 一致（否则报"分片属于不同文件"）
3. 逐片校验 CRC-32
4. 按 `shard_index` 排序
5. 检查 `0..shard_total` 无缺失
6. 拼接数据段

已验证：乱序输入可正确重组；缺片报告具体缺失编号；单字节损坏被 CRC 检出。

### 8.4 缺片时的降级播放

用户明确要求"缺片时视频最好能单独播放"，作为可选设置。

**可行性**：因为分块 AEAD 的每块独立，缺片只影响该片覆盖的块。实现方式：

- 把缺失区间标记为"空洞"
- Range 请求落在空洞内 → 返回 `416 Range Not Satisfiable`
- 播放器会跳过无法读取的区间
- UI 明确提示"⚠️ 缺少第 N 片，部分内容不可播放"

⚠️ **限制**：若缺失的片恰好包含 MP4 的 `moov` box，整个文件无法播放。这正是 `TLV_MOOV_CACHE` 的另一个价值——moov 存在 header 里，每片冗余 header 都带一份。

---

## 9. 完整读取流程

```
 1. 读取文件前 96 字节
 2. 校验 magic；version_major > 1 → 拒绝
 3. 解析 fixed header，得到 header_len
 4. 读取完整 header（header_len 字节）
 5. 对每个候选 KEK × 每个 slot：
       wk = HKDF(KEK, file_uuid, "cvault/v1/slot" || i)
       尝试 AEAD 解包 slot[i]
       成功 → 得到 FEK，跳出
    全部失败 → 该文件不属于当前解锁的任何密码
 6. 验证 header MAC —— 失败必须终止
 7. 解密需要的 TLV：
       - CRITICAL 且无法识别的类型 → 拒绝打开
       - CRITICAL 且无法解密 → 报错
       - 非 CRITICAL 解密失败 → 跳过
 8. 若 COMPRESSED：加载压缩索引表
 9. 按需解密载荷块：
       - 未压缩：offset → chunk_index → O(1) 定位
       - 压缩：查索引表定位
10. （可选）全量读取后校验 CONTENT_HASH
```

---

## 10. 原子写入

加密写入必须保证崩溃/断电不产生"看似正常实则损坏"的文件：

```
1. 写入 <target>.cvlt.tmp
2. fsync(文件)
3. fsync(父目录)          ← 容易遗漏，但在部分文件系统上必需
4. rename(tmp → target)   ← 同文件系统内原子
5. fsync(父目录)
```

**分片写入**：全部分片写完后再统一 rename，避免出现"部分分片已就位"的中间态。

**移动端中断恢复**：iOS/Android 可能在后台挂起进程。方案：
- 采用可续传的分段提交，记录已完成的块数到 `.cvlt.progress` 文件
- 下次启动检测到未完成任务 → 提示"继续 / 放弃"
- 放弃时清理 `.tmp` 和 `.progress`

---

## 11. 可还原性保证

用户明确要求：**加密文件必须能 bit-for-bit 完整还原**（决策 D-07）。

### 11.1 默认路径的保证

默认情况下，载荷就是原始文件的**逐字节**内容，不做任何变换：

- ❌ 不做 MP4 faststart 重整（也不需要，见 [04](04-media-playback.md)）
- ❌ 不做容器重写
- ❌ 不做元数据剥离
- ✅ `TLV_CONTENT_HASH` 存原始明文的 BLAKE2b-256，解密后可自检

已验证：0 字节到 3 MB 共 7 种尺寸（含块边界 ±1）全部 bit-for-bit 一致。

### 11.2 唯一的例外：转码模式

启用视频转码选项（决策 D-26）后，**载荷不再是原文件**：

- 必须置位 `FLAG_TRANSCODED`
- 必须写入 `TLV_TRANSCODE_INFO`（含原文件哈希、转码参数）
- UI 必须**显著警告**：此文件无法还原为原始文件

三种模式的完整说明见 [04-媒体播放架构 §7](04-media-playback.md)。

---

## 12. 测试向量

[appendix/test-vectors.json](appendix/test-vectors.json) 提供 5 组确定性向量：

| 向量 | 覆盖 |
|---|---|
| `v1-minimal-single-password` | 最小文件、完整密钥派生链、全文件 hex |
| `v2-multi-password-deniable` | 3 密码、slot 区 hex、错误密码拒绝 |
| `v3-multichunk-random-access` | 多块、5 组 Range 测试、偏移公式验证 |
| `v4-compressed-with-index` | zstd 压缩、完整索引表内容 |
| `v5-sharding` | 分片头逐字段、冗余头、合并验证 |

每组包含中间派生值（KEK、FEK、payload_key、nonce、AAD），便于实现者逐步比对定位问题。

⚠️ 测试向量使用 Argon2 弱参数（m=64 KiB, t=1, p=1）以便快速复现，**生产实现禁用**。

### 12.1 Rust 实现的验收标准

Rust 实现必须能：
1. 读取全部 5 组向量的 `full_file_hex` 并正确解密
2. 用相同输入 + 相同随机源产出**逐字节相同**的输出
3. 通过参考实现的 68 项断言的等价 Rust 版本
