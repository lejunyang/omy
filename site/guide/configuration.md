---
title: 配置文件
---

# 配置文件

把常用参数写成默认值，不必每次都在命令行里重复。

## 位置

| 平台 | 路径 |
|---|---|
| Windows | `%APPDATA%\omy\config.toml` |
| Linux | `~/.config/omy/config.toml` |
| macOS | `~/Library/Application Support/omy/config.toml` |

文件默认**不存在**，此时全部用内置默认值。`omy doctor` 会打印当前实际使用的路径。

用 `--config` 可临时指定别的文件：

```bash
omy --config ./ci-config.toml encrypt file.mp4
```

::: tip 显式指定与默认路径的区别
`--config` 指定的文件读不到会**直接报错**；默认路径的文件不存在则静默用默认值。区别是刻意的——你明确指定的文件被静默忽略，会让你以为配置生效了。
:::

## 优先级

命令行参数 > 环境变量 > 配置文件 > 内置默认。

## 完整示例

下面每一项都是真实存在的键，取值也是有效值：

```toml
[defaults]
# mobile | interactive | moderate | sensitive
kdf_profile = "moderate"
# xchacha20 | aes256gcm
cipher = "aes256gcm"
chunk_size = "4M"
# encrypt | keep-ext | plain
name_mode = "keep-ext"
# keep | trash | delete
original_action = "trash"

[compress]
enabled = true
level = 12          # zstd 1–19

[scan]
paths = ["~/Documents", "~/Videos"]
max_depth = 5

[serve]
device_name = "书房台式机"
default_expire = "24h"

[ui]
# auto | zh-CN | en
language = "zh-CN"
```

## 各项默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `defaults.kdf_profile` | `"interactive"` | KDF 档位 |
| `defaults.cipher` | `"xchacha20"` | AEAD 算法 |
| `defaults.chunk_size` | `"256K"` | 分块大小 |
| `defaults.name_mode` | `"encrypt"` | 文件名处理 |
| `defaults.original_action` | `"keep"` | 加密后如何处置原文件 |
| `compress.enabled` | `false` | 是否默认压缩 |
| `compress.level` | `3` | zstd 级别 |
| `scan.paths` | `[]` | `scan` 的默认扫描路径 |
| `scan.max_depth` | `8` | 最大递归深度 |
| `serve.device_name` | 无 | 局域网广播名 |
| `serve.default_expire` | 无 | 默认授权有效期 |
| `ui.language` | `"auto"` | 界面语言，`auto` 跟随系统 locale |

::: warning original_action 改成 trash 或 delete 要想清楚
这两个值会让原文件在加密后从原位置消失。写进配置文件意味着**以后每次加密都这样**。仍然会要求确认（除非加 `--yes`），但配置 + `--yes` 的组合在脚本里很容易变成静默删除。
:::

## 拼错的键会报错

配置解析是严格的，未知字段直接失败：

```toml
[defaults]
kdf_profil = "mobile"    # 少个 e
```

```
错误: 读取配置失败: 解析配置文件 ... 失败
```

这是刻意的——静默忽略拼错的键，会让你以为配置生效了，而实际上一直在用默认值。

## 语言

```toml
[ui]
language = "auto"    # 跟随系统 locale，默认
```

支持 `zh-CN` 与 `en`。`omy doctor` 会显示当前生效的语言以及探测到的系统 locale：

```
· 界面语言    zh-CN（系统 locale: zh-CN）
```

::: tip JSON 输出不受语言影响
`--json` 输出里的 `error.code` 恒为英文常量（如 `WRONG_PASSWORD`），不随界面语言变化。脚本应该匹配它或退出码，不要匹配人类可读的消息文本。
:::
