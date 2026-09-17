---
title: 命令总览
---

# 命令总览

```
omy <命令> [选项]
```

本页内容取自 `omy --help` 的实际输出。任何时候都可以用 `omy <命令> --help` 查看权威版本。

## 全部命令

| 命令 | 作用 |
|---|---|
| `encrypt` | 加密文件或目录 |
| `decrypt` | 解密文件 |
| `info` | 查看文件信息（无需密码即可看部分） |
| `verify` | 验证完整性 |
| `list` | 列出目录中的 `.omy` 文件 |
| `scan` | 扫描目录，匹配密码并列出可解锁文件 |
| `cat` | 解密并输出到标准输出（管道友好） |
| `key` | 密钥 slot 管理 |
| `shard` | 分片切分与合并 |
| `share` | 局域网共享与访问 |
| `bench` | KDF 与加解密性能基准 |
| `doctor` | 环境自检 |
| `completion` | 生成 shell 补全脚本 |

## 全局选项

对所有子命令都有效：

| 选项 | 作用 |
|---|---|
| `-v, --verbose` | 详细输出，可叠加（`-vv`） |
| `-q, --quiet` | 仅输出错误。与 `-v` 冲突 |
| `--json` | 以 JSON 输出，便于脚本消费 |
| `--no-color` | 禁用彩色输出 |
| `--config <PATH>` | 指定配置文件路径 |
| `--yes` | 跳过所有确认（危险操作慎用） |
| `-h, --help` | 帮助 |
| `-V, --version` | 版本 |

## 密码输入选项

`encrypt` / `decrypt` / `verify` / `cat` / `info` / `scan` / `key` 都支持：

| 选项 | 说明 |
|---|---|
| （不带任何参数） | 交互式输入，不回显。默认且最推荐 |
| `--password-stdin` | 从标准输入读 |
| `--password-file <PATH>` | 从文件读首行 |
| `--password-env <VAR>` | 从环境变量读，传变量**名** |

**没有 `--password <明文>`。** 用它会得到说明和退出码 2。`scan` 的 `--password-file` / `--password-env` 可重复以提供多个密码。

## encrypt

```
omy encrypt [OPTIONS] <PATHS>...
```

| 选项 | 说明 |
|---|---|
| `-o, --output <PATH>` | 输出路径（单个输入时有效） |
| `--output-dir <DIR>` | 批量输出目录 |
| `--name-mode <MODE>` | `encrypt` / `keep-ext` / `plain` |
| `--add-password` | 追加密码 slot，可重复，最多 8 个。仅交互式 |
| `--kdf-profile <PROFILE>` | `mobile` / `interactive` / `moderate` / `sensitive` |
| `--vault <PATH>` | 加入已有库，复用其 vault salt 与 KDF 参数 |
| `--cipher <CIPHER>` | `xchacha20` / `aes256gcm` |
| `--chunk-size <SIZE>` | 如 `256K` / `1M` / `4M` |
| `--compress` | 启用 zstd 压缩 |
| `--compress-level <N>` | zstd 级别 1–19 |
| `--mode <MODE>` | `container`（默认）/ `tree` |
| `--slot-mode <MODE>` | 密码管理方式：`deniable`（默认，查不出配了几个密码）/ `managed`（能列出并精确删除，加密后改不了） |
| `--original <ACTION>` | `keep`（默认）/ `trash` / `delete` |
| `--thumbnail <MODE>` | `auto`（默认）/ `none` |
| `--thumbnail-frame <T>` | 视频取帧时间点，如 `00:01:23` / `83` / `83.5` |
| `--no-media-meta` | 不写入媒体元信息 |
| `--no-moov-cache` | 不缓存 MP4 的 moov box |

输出名是在原文件名后**整体追加** `.omy`：`note.txt` → `note.txt.omy`。

## decrypt

```
omy decrypt [OPTIONS] <FILES>...
```

| 选项 | 说明 |
|---|---|
| `-o, --output <PATH>` | 输出路径（单个输入时有效） |
| `--output-dir <DIR>` | 批量输出目录 |
| `--stdout` | 输出到标准输出。与 `-o` / `--output-dir` 冲突 |
| `--verify-only` | 只校验不写出 |
| `--ignore-missing-shards` | 缺片时输出可用部分 |

## info

```
omy info [OPTIONS] <FILE>
```

| 选项 | 说明 |
|---|---|
| `--with-password` | 提供密码以查看加密的元信息（原文件名等） |
| `--extract-thumbnail <PATH>` | 把缩略图导出到指定路径（需密码，格式为 WebP） |

不带密码可见的是公开字段：格式版本、UUID、算法、KDF 参数、分块大小、块数、密文与明文大小。`Slot 占用` 恒为「未知」——设计上不可探测。

## verify

```
omy verify [OPTIONS] <FILES>...
```

逐块验证认证标签并比对内容哈希。

## list

```
omy list [OPTIONS] [DIRS]...
```

| 选项 | 说明 |
|---|---|
| `-r, --recursive` | 递归子目录 |
| `--max-depth <N>` | 最大递归深度 |
| `--any-extension` | 也列出后缀不是 `.omy` 但文件头匹配的文件 |

不需要密码——只列文件，不试解密。默认目录是当前目录。

## scan

```
omy scan [OPTIONS] [DIRS]...
```

| 选项 | 说明 |
|---|---|
| `-r, --recursive` | 递归子目录（**默认开启**） |
| `--no-recursive` | 只看目录本层 |
| `--max-depth <N>` | 最大递归深度 |
| `--show-locked` | 同时列出未匹配的文件 |
| `--any-extension` | 也检查后缀不是 `.omy` 的文件（识别靠文件头 magic，所以改过后缀的也能找到） |
| `--max-files <N>` | 单次扫描的文件数上限 |

列出的是**解密后的真实文件名**。

## cat

```
omy cat [OPTIONS] <FILE>
```

| 选项 | 说明 |
|---|---|
| `--range <RANGE>` | 只输出指定范围，格式 `起始-结束`，端点含两端 |

支持 HTTP Range 风格的后缀形式：`--range -256` 取最后 256 字节。

## key

```
omy key <COMMAND> <FILE>
```

| 子命令 | 作用 |
|---|---|
| `add` | 添加密码 slot（需已知一个现有密码） |
| `remove` | 只保留当前密码，作废该文件上的其它密码**（包括恢复码）** |
| `list` | 显示 slot 占用情况 |
| `change` | 修改密码（旧密码作废，其它槽位原样保留） |
| `reencrypt` | 重新加密：换掉文件密钥并重写载荷（可同时改密码） |
| `recovery` | 生成恢复码并挂到文件上 |
| `restore` | 用恢复码打开文件并设置新密码 |

`add` 与 `change` 用 `--new-password-file` / `--new-password-env` 提供新密码。

可否认模式（默认）下 `key list` 不需要密码：槽位占用本就不可探测，给密码也没有额外信息。

可管理模式的文件则可以给密码，那时它会列出每个槽位的类型：

```bash
omy key list file.omy --password-env PW
omy key list 加密后的文件夹 --password-env PW   # 文件夹同样可以
```

| 选项 | 说明 |
|---|---|
| `--password-env <VAR>` | 从环境变量读密码 |
| `--password-file <PATH>` | 从文件读密码 |
| `--password-stdin` | 从标准输入读密码 |

需要密码是设计使然——槽位目录是加密的，读它要先能打开这个文件。

`encrypt` 的 `--slot-mode <deniable|managed>` 决定用哪种模式，默认 `deniable`。
模式写在文件头里，加密后改不了。

### change 与 remove 的关键区别

`change` / `add` 会**原样保留**文件上的其它槽位，所以设过恢复码之后照常改
密码即可。只有 `remove` 是清场，它会作废包括恢复码在内的其它全部密码。

`add` 会占用一个新槽位，而实现无法探测哪个槽位是空的（这正是可否认性所
要求的），所以它可能盖掉原本挂在那个下标上的密码——命令会如实警告，
但无法告诉你那里原来是否真有密码。

### recovery / restore

```bash
omy key recovery secret.omy              # 打印 26 个词
omy key recovery secret.omy --out code.txt   # 写入文件而不是打印到终端
omy key restore secret.omy --code-file code.txt
```

`--out` 写出的是明文，请立即转移到安全的地方。恢复码**不会**出现在
`--json` 输出里——那种输出常被重定向进文件或管道进日志。

`restore` 会作废这个文件上**其它所有日常密码**，只保留新设的那个和
恢复码本身。这是有意为之：用到恢复码通常意味着旧密码忘了或可能已经
泄露，此时留着它们没有意义。恢复码则一律保住——你刚经历过一次「忘了
密码」，这时抽掉唯一的兜底是最坏的时机，同一份恢复码之后还能再用。

可管理模式下还会额外告诉你作废了几个。

详见[恢复码](../guide/passwords.md#恢复码：忘记密码时的唯一退路)。

## shard

```
omy shard <COMMAND>
```

| 子命令 | 作用 |
|---|---|
| `split` | 将已加密文件切分为分片 |
| `merge` | 合并分片 |
| `check` | 检查分片完整性与缺口 |

`split` 的选项：

| 选项 | 说明 |
|---|---|
| `--size <SIZE>` | **必填**，每片大小，如 `4095M` |
| `--output-dir <DIR>` | 输出目录，默认与源文件同目录 |
| `--redundant-header` | 每片含冗余头（**默认开启**） |
| `--no-redundant-header` | 不写冗余头 |
| `--remove-source` | 切分后删除原文件 |

## share

```
omy share <COMMAND>
```

| 子命令 | 作用 |
|---|---|
| `serve <DIR>` | 共享一个目录，等待已配对设备连接 |
| `discover` | 搜索局域网内正在共享的设备 |
| `pair [ADDR]` | 与另一台设备配对（双方都要执行） |
| `connect <FINGERPRINT>` | 连接已配对设备，列出或取回文件 |
| `devices list/revoke/purge` | 管理已配对设备 |

这些子命令的 `--password-*` 指的是**设备库密码**，与文件密码无关。配对码用单独的 `--pin-file`。

`serve` 常用选项：`--name`、`--port`（0 = 系统分配）、`--local-only`、`--no-advertise`、`--max-connections`、`--store`。

`connect` 常用选项：`--addr`（跳过 mDNS）、`--fetch <DIR>`（取回密文，不解密）。

`pair` 常用选项：`--listen`、`--port`、`--expires-in <DAYS>`（0 = 永久）、`--pin-file`。

## bench

```
omy bench [OPTIONS]
```

| 选项 | 说明 |
|---|---|
| `--profile <PROFILE>` | 只测指定档位，可重复；默认测全部 |
| `--quick` | 跳过高内存档位（`moderate` / `sensitive`） |
| `--throughput-size <SIZE>` | 吞吐测试数据量，默认 `64M` |

## doctor

```
omy doctor [--probe-kdf]
```

报告平台、AES 硬件加速、CPU 并行度、配置文件位置、界面语言、各 KDF 档位实测耗时、FFmpeg 可用性、局域网共享可用性，以及已知的实现层面限制。

装完先跑一次，比看文档更可靠。

## completion

```
omy completion <SHELL>
```

支持 `bash` / `elvish` / `fish` / `powershell` / `zsh`。

```bash
omy completion bash > /etc/bash_completion.d/omy
omy completion powershell | Out-String | Invoke-Expression
```
