# 09 · CLI 设计

> 对应决策 **D-22**（要 CLI）。
> CLI 与 GUI 共享同一个 `omy-core` crate，**行为完全一致**——CLI 能做的 GUI 都能做，反之亦然（除交互式浏览外）。

## 1. 定位

| CLI 面向 | 典型用途 |
|---|---|
| 脚本化 / 自动化 | 定时备份加密、CI 流水线、批量归档 |
| 服务器环境 | 无 GUI 的 NAS / VPS 上操作 vault |
| 高级用户 | 精确控制参数、管道组合、批处理 |
| 排障 | 检查文件头、验证完整性、诊断损坏 |

**CLI 是一等公民，不是 GUI 的附属品。** 但有两点例外：
- 不提供交互式文件浏览器（那是 GUI 的价值）
- 不提供媒体播放（CLI 用 `omy mount` 或管道输出给外部播放器）

---

## 2. 命令总览

```
omy <命令> [选项]

核心
  encrypt        加密文件或目录
  decrypt        解密文件
  info           查看文件信息（无需密码可看部分）
  verify         验证完整性

浏览
  list           列出目录中的 .omy 文件
  scan           扫描目录，匹配密码并列出可解锁文件
  cat            解密并输出到 stdout（管道友好）

密钥
  key add        添加密码 slot
  key remove     移除密码 slot
  key list       列出 slot 占用情况
  key change     修改密码

分片
  shard split    将已加密文件切分为分片
  shard merge    合并分片

共享
  serve          启动局域网只读共享
  connect        连接远程设备

工具
  bench          KDF 性能基准测试
  doctor         环境自检
  completion     生成 shell 补全脚本
```

---

## 3. 全局选项

```
-v, --verbose          详细输出（可叠加 -vv）
-q, --quiet            仅输出错误
    --json             以 JSON 输出（脚本友好）
    --no-color         禁用彩色
    --config <PATH>    指定配置文件
    --yes              跳过所有确认（危险操作慎用）
-h, --help
-V, --version
```

### 3.1 退出码

| 码 | 含义 |
|---|---|
| 0 | 成功 |
| 1 | 一般错误 |
| 2 | 参数错误 |
| 3 | 密码错误 / 无匹配 slot |
| 4 | 文件损坏或篡改（MAC 校验失败） |
| 5 | 格式版本不支持 |
| 6 | 缺少分片 |
| 7 | 权限不足 |
| 8 | 用户取消 |

脚本可据此区分「密码错」与「文件坏」——这是 GUI 无法提供的精度。

---

## 4. 密码输入（安全关键）

> **对应旁路清单 L12（最高优先级）**：命令行参数对同机其它用户可见（`ps aux`、`/proc/*/cmdline`），且会进入 shell history。

**因此 CLI 不提供 `--password <PASS>` 这样的明文参数。**

支持的输入方式，按推荐度排序：

| 方式 | 参数 | 说明 |
|---|---|---|
| **交互式提示**（默认） | 无 | 从 `/dev/tty` 读取，不回显 |
| **环境变量** | `--password-env OMY_PW` | 传变量**名**而非值；仍需注意 `/proc/*/environ` |
| **文件** | `--password-file <PATH>` | 读取首行；建议 `chmod 600` |
| **stdin** | `--password-stdin` | 管道传入，适合 CI secret |
| **系统钥匙串** | `--keyring <NAME>` | 桌面平台，走 OS 凭据管理器 |

```bash
# 推荐：CI 场景
echo "$SECRET" | omy encrypt --password-stdin file.mp4

# 推荐：本地脚本
omy encrypt --password-file ~/.omy-pw file.mp4

# 明确拒绝：不存在这样的参数
omy encrypt --password "mypass" file.mp4   # ✗ 报错并说明原因
```

若用户尝试 `--password`，CLI 输出明确的错误说明而非静默失败：

```
error: 不支持 --password 参数

  命令行参数对同机其它用户可见（ps aux），且会被记入 shell 历史。

  请改用：
    --password-stdin       从管道读取
    --password-file <路径>  从文件读取
    --password-env <变量名> 从环境变量读取
    （不带参数）             交互式输入
```

---

## 5. 核心命令详解

### 5.1 `encrypt`

```
omy encrypt [选项] <路径>...

输出
  -o, --output <PATH>        输出路径（默认同目录同名 .omy）
      --output-dir <DIR>     批量输出目录

文件名（D-01）
      --name-mode <MODE>     encrypt | keep-ext | plain   [默认 encrypt]

密码
      --password-stdin / --password-file / --password-env / --keyring
      --add-password         追加更多 slot（可重复，最多 8 个）

KDF（档位见 03 号文档）
      --kdf-profile <P>      mobile | interactive | moderate | sensitive
                             [默认 interactive]

加密
      --cipher <C>           xchacha20 | aes256gcm   [默认 xchacha20]
      --chunk-size <SIZE>    分块大小，如 256K / 1M / 4M   [默认 256K]

压缩（D-09）
      --compress             启用 zstd
      --compress-level <N>   1–19   [默认 3]

目录（D-05）
      --mode <M>             container | tree   [默认 container]

分片（D-18）
      --shard-size <SIZE>    如 4095M；不指定则不分片
      --shard-redundant-header   每片含冗余头   [默认开]

缩略图（D-10）
      --thumbnail <MODE>     auto | none | <图片路径>   [默认 auto]
      --thumbnail-frame <T>  视频取帧时间点，如 00:01:23

视频转码（D-26）
      --transcode <MODE>     none | web | dual   [默认 none]
      --transcode-preset <P> 转码预设

字幕（D-28）
      --with-subtitles       自动加密同名字幕   [默认开]
      --no-subtitles         不处理字幕

原文件（D-15）
      --original <ACTION>    keep | trash | delete   [默认 keep]
                             trash/delete 需 --yes 或交互确认
                             不指定时取配置 defaults.original_action

伪装（D-17）
      --disguise <MODE>      footer | host-jpeg | host-png
      --disguise-cover <P>   宿主图片路径
```

**块大小与压缩率的实测关系**（2 MB 高重复语料，zstd-3）：

| 块大小 | 压缩后 | 占原始 |
|---|---|---|
| 64 KiB | 133,515 B | 6.52% |
| 256 KiB | 34,004 B | 1.66% |
| 1 MiB | 9,158 B | 0.45% |
| 4 MiB | 5,017 B | 0.24% |

4 MiB 块比 64 KiB 小 **96.2%**。默认 256 KiB 是压缩率、seek 精度、内存占用的折中；归档用途建议 `--chunk-size 4M`。

**批量加密的正确姿势**：CLI 对多文件自动**复用同一次 KDF 派生的 KEK**（Argon2 只跑一次），这是 GUI 与 CLI 共用的核心逻辑。实测 500 文件 × 5 密码扫描仅 171 ms；若每文件独立跑 Argon2 需 450 s（慢 2,627×）。

**示例**：

```bash
# 基本
omy encrypt report.docx

# 归档：大块 + 高压缩 + 双密码
omy encrypt --chunk-size 4M --compress --compress-level 12 \
               --add-password --kdf-profile moderate  archive/

# 分片到 U 盘容量
omy encrypt --shard-size 4095M bigfile.mkv

# 转码为 web 友好格式（会二次确认）
omy encrypt --transcode web movie.mkv
```

转码时的强制确认（对应 D-26 与 D-07 的冲突）：

```
警告：--transcode web 会改变文件内容

  加密后保存的是转码产物，不是原始文件。
  解密后得到 MP4，无法还原为原始的 MKV。

  原始文件 SHA-256 会记录在文件头中，但仅供核对。

  确认继续？[y/N]
```

### 5.2 `decrypt`

```
omy decrypt [选项] <文件.omy>...

  -o, --output <PATH>      输出路径
      --output-dir <DIR>   批量输出目录
      --stdout             输出到标准输出
      --keep-encrypted     保留 .omy   [默认]
      --verify-only        只校验不写出
      --ignore-missing-shards   缺片时输出可用部分（D-18）
      --metadata-report <PATH>  元数据还原报告（D-20 / N3）
```

**元数据还原报告**（对应 N3 决策：不支持的项**明确报告**而非静默丢弃）：

```
$ omy decrypt --metadata-report report.txt photos.omy

已还原 1,247 个文件。

元数据还原情况：
  ✓ 修改时间       1247/1247
  ✓ 创建时间       1247/1247
  ⚠ 权限位          1247/1247（Windows 上仅映射只读标志）
  ✗ 扩展属性        0/89     （目标文件系统不支持 xattr）
  ✗ macOS 资源分支  0/12     （当前平台非 macOS）

详细清单见 report.txt
```

### 5.3 `info`

```
omy info [选项] <文件>

      --no-password        仅显示无需密码的信息   [默认]
      --json
```

不提供密码时可见的信息（这些本就是明文，见 02 号文档）：

```
$ omy info movie.omy

文件           movie.omy
格式           OMYFILE v1.0
UUID           7f3a2b91-...
文件头长度     8,771 字节（含缩略图）
加密算法       XChaCha20-Poly1305
KDF            Argon2id  m=64MiB t=3 p=1  (interactive)
分块大小       256 KiB
块数量         3,412
压缩           zstd level 3
密文大小       894,238,720 字节
Slot 占用      未知（设计上不可探测，见 03 号文档）
分片           3 片（当前目录找到 3/3）
缩略图         有，320×180 JPEG（加密）
文件名         已加密

提示：提供密码可查看原文件名、原始大小、媒体信息。
```

> **注意**：`Slot 占用` 恒显示"未知"。slot 区始终填满 8 个（真实 + 随机填充），实测 1 密码与 3 密码的文件均为 772 B 且字节分布无法区分（slot 区熵 7.491 bits/byte，落在纯随机基线区间 [7.332, 7.508] 内）。这是可否认性（D-02）的基础，CLI 不能提供任何泄露 slot 数量的输出。

### 5.4 `scan`

对应 D-19（扫描缓存放内存）。

```
omy scan [选项] <目录>...

  -r, --recursive
      --max-depth <N>
      --password-file <P>    可重复，多密码
      --show-locked          同时列出未匹配的文件
      --json
```

```
$ omy scan -r ~/Documents --password-file pw1 --password-file pw2

扫描 3,891 个文件，找到 1,247 个 omy 文件…

密码 1 匹配 823 个：
  季度报告.docx              156 KB
  产品演示.mp4               847 MB   [视频 12:34]
  ...

密码 2 匹配 424 个：
  ...

未匹配 0 个
用时 1.4 s
```

> 扫描结果**只在内存中**，进程退出即消失，不落盘任何索引数据库。这避免了"加密了文件但索引泄露文件名"的问题。

### 5.5 `cat`

管道友好，是 CLI 相对 GUI 的独特价值：

```bash
# 直接播放，不产生临时明文文件
omy cat movie.omy --password-file pw | mpv -

# 检查加密的日志
omy cat app.log.omy --password-stdin | grep ERROR

# 校验原始文件哈希
omy cat data.omy | sha256sum
```

`cat` 采用流式解密，内存占用恒定（约 2 个块大小），不受文件大小影响。

### 5.6 `key`

```
omy key add <文件>       添加密码 slot（需已知一个现有密码）
omy key remove <文件>    移除 slot
omy key list <文件>      仅显示 "8 个 slot（内容不可探测）"
omy key change <文件>    修改密码
```

`key remove` 的必要警告：

```
警告：移除 slot 不影响已存在的副本

  此操作只修改当前文件。若该文件曾被复制、备份或同步到
  其它位置，那些副本仍可用被移除的密码打开。

  确认继续？[y/N]
```

### 5.7 `serve` / `connect`

对应 D-04（只传密文）、D-21（只读）。

```
omy serve [选项]
      --path <DIR>          共享目录（可重复）
      --name <NAME>         设备名
      --expire <DURATION>   有效期，如 24h / 7d / session
      --pairing-code        显示配对码
      --revoke <DEVICE>     吊销设备
      --list-devices
      --access-log <PATH>

omy connect <设备>
      --list                列出局域网设备
      --password-file <P>
```

`serve` 只读、只传密文——远端设备拿到的是密文，解密在本地完成。服务端不持有明文，也不接受写入。

### 5.8 `bench` / `doctor`

```bash
$ omy bench
Argon2id 基准（本机）：
  mobile      (32MiB, t=4)     124 ms
  interactive (64MiB, t=3)     180 ms   ← 默认
  moderate    (256MiB, t=4)    891 ms
  sensitive   (1GiB, t=4)     3,847 ms

HKDF（每文件）                 13.2 µs
比值                          13,700×

加解密吞吐：
  XChaCha20-Poly1305    1.8 GB/s
  AES-256-GCM (AES-NI)  3.2 GB/s
```

```bash
$ omy doctor
✓ CPU AES-NI 支持
✓ 可用内存 15.4 GB（sensitive 档位需 1 GB）
✓ FFmpeg 6.1.1（LGPL 构建）
⚠ HEIC 解码：系统解码器不可用，libheif 未编译进本构建
  → HEIC 文件可加密，但无法生成缩略图或预览
✓ 钥匙串可用（Secret Service）
✓ mDNS 可用
```

---

## 6. JSON 输出

所有命令支持 `--json`，便于脚本消费：

```bash
$ omy info --json movie.omy
{
  "format": "OMYFILE",
  "version": "1.0",
  "uuid": "7f3a2b91-...",
  "header_len": 8771,
  "cipher": "xchacha20-poly1305",
  "kdf": { "algorithm": "argon2id", "m_kib": 65536, "t": 3, "p": 1,
           "profile": "interactive" },
  "chunk_size": 262144,
  "chunk_count": 3412,
  "compression": { "algorithm": "zstd", "level": 3 },
  "ciphertext_size": 894238720,
  "shards": { "total": 3, "found": 3 },
  "has_thumbnail": true,
  "filename_encrypted": true
}
```

错误也以 JSON 输出，便于判断：

```json
{ "error": { "code": "WRONG_PASSWORD", "exit_code": 3,
             "message": "无匹配的密码 slot" } }
```

---

## 7. 配置文件

`~/.config/omy/config.toml`（遵循 XDG）：

```toml
[defaults]
kdf_profile = "interactive"
cipher = "xchacha20"
chunk_size = "256K"
name_mode = "encrypt"
original_action = "keep"

[compress]
enabled = false
level = 3

[scan]
paths = ["~/Documents", "~/Videos"]
max_depth = 8

[serve]
device_name = "书房台式机"
default_expire = "24h"

[ui]
language = "auto"    # auto | zh-CN | en
```

优先级：命令行参数 > 环境变量 > 配置文件 > 内置默认。

---

## 8. 本地化（D-29）

CLI 与 GUI 共享同一套文案资源。语言检测：

```
OMY_LANG 环境变量
  → 配置文件 [ui].language
  → 系统 locale（LC_ALL / LC_MESSAGES / LANG）
  → 英语兜底
```

**但 `--json` 输出的 `code` 字段永远是英文常量**（如 `WRONG_PASSWORD`），只有 `message` 会翻译。脚本应当匹配 `code` 而非 `message`——这一点在 `--help` 中明确说明。

---

## 9. Shell 补全

```bash
omy completion bash > /etc/bash_completion.d/omy
omy completion zsh  > ~/.zsh/completions/_omy
omy completion fish > ~/.config/fish/completions/omy.fish
omy completion powershell | Out-String | Invoke-Expression
```

补全应覆盖子命令、选项、枚举值（如 `--kdf-profile` 的四个档位），以及 `.omy` 文件路径。

---

## 10. 与 GUI 的一致性保证

| 保证 | 实现方式 |
|---|---|
| 相同的格式行为 | 共用 `omy-core`，CLI 与 GUI 都是薄封装 |
| 相同的默认值 | 默认值定义在 core 中，两端不各自硬编码 |
| 相同的警告 | 危险操作的警告文案来自同一套 i18n 资源 |
| 互操作 | CLI 加密的文件 GUI 能打开，反之亦然；测试向量双向验证 |

**测试要求**：CI 中必须包含 CLI ↔ GUI 交叉验证——CLI 加密的文件用 core API 解密，以及反向操作，覆盖全部 5 组测试向量。
