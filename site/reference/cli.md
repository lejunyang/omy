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
| `remote` | 远程位置（WebDAV / Telegram）：注册、浏览、上传下载、缓存管理 |
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
| `remove` | 只保留当前密码，作废该文件上的其它密码<strong>（包括恢复码）</strong> |
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

### device（设备密钥）

用系统生物识别免密解锁（Windows Hello；macOS Touch ID **尚未可用**，见下），不必每次输密码。

```bash
omy key device secret.omy add        # 挂上（需要先输一次密码）
omy key device secret.omy status     # 查看这台机器上有没有挂
omy key device secret.omy remove     # 移除
omy decrypt secret.omy --device      # 用它解锁
```

它就是一个普通的密码槽，区别只在这把钥匙从哪来：密码槽的钥匙由密码算出来，
设备密钥的钥匙是**随机生成**的，交给这台机器的 OS 密钥库保管：

- **Windows**：私钥封进 TPM 2.0，取用时系统强制 Windows Hello 确认。
- **macOS**：密钥放进 Data Protection Keychain，访问控制挂 Touch ID，
  每次读取由系统强制本人在场。

::: warning 它不会让文件更安全，只是更方便
它挡得住硬盘被偷和换机器解密，**挡不住正在这台机器上以你的身份运行的恶意
程序**——它可以在你按下指纹之后的那个窗口里发起解锁。

所以不要因为开了它就把密码设弱。设备密钥省的是打字，不是防护。
:::

::: danger 它随时可能永久失效
换机器、重装系统、清除 TPM、清除 Touch ID、重置生物识别——任一发生，设备密钥
就再也解不开了，而且**无法恢复**。

所以它不是备份手段，务必继续记住密码。
:::

一把设备密钥覆盖整个库（同一个 `vault_salt` 下的所有文件），不同库之间自动
隔离。文件夹也可以：`omy key device <加密后的文件夹> add`。

平台可用性现状（务必先读）：

| 平台 | 当前官方分发下可用吗 |
|---|---|
| Windows | 可用：TPM 2.0 + Windows Hello 已真机验证。 |
| macOS（Apple Silicon） | **暂不可用**。当前官方发布的是**未签名、未公证的裸 tar.gz**，写不了 Data Protection Keychain（系统 `-34018` 直接拒绝）。命令会如实报告不可用，**继续用主密码解锁，不会**退回一个不设防的普通钥匙串条目——这是有意的无降级语义。Touch ID 这条路径的成功/取消/锁定也还**没有**在带 Touch ID 的真机上端到端验证过。 |
| 其它平台 | 如实报不支持，不降级。 |

未来要在 macOS 上打开 Touch ID 设备密钥，需要：

1. omy GUI 以 **Developer ID 签名 + 公证**打包成 `.app` 分发；
2. CLI 要么放进同一个签名 `.app` 的包内，要么由同一 Team、同一
   `keychain-access-groups` 的 wrapper 调用——否则它没有那把钥匙串权限。

在此之前，macOS 用户请继续用主密码。macOS 的**常规功能**（加解密、播放、远程
位置等）已在 Apple Silicon 上真机验证过；只是**Touch ID 免密这一项**尚未可用。

用的是「任意已录入指纹均可」这一档：之后增删指纹不会让已有设备密钥失效。

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

## remote

```
omy remote <COMMAND>
```

远程位置与图形界面**共用同一份配置**（`remote.places`）：命令行加的位置 GUI 认得出，GUI 加的位置命令行解得了密码。位置 id 形如 `p1`、`p2`……下文的 `<位置>` 既可以传 id，也可以传显示名；显示名重复时会报错并要求改用 id。

退出码：用法错误 2；加密 Telegram 位置密码错 **3**（`TG_PLACE_WRONG_PASSWORD`）；其余远程失败统一 1（用法错与「跑起来了但失败」可据此区分）。建连阶段的失败在 `--json` 的 `error.code` 里给结构化 `TG_*` 码，见[退出码 › 远程与虚拟](./exit-codes#远程与虚拟命令的退出码)。

**显式 `--config <PATH>` 后，Telegram 建连用的应用身份（api_id / api_hash）与代理也都从这同一个文件解析**，绝不退回默认配置——否则位置清单来自 `--config` 文件、登录态却拿默认配置里的 api_id 去连，服务端会因 session 信封绑定的是另一个 api_id 而拒绝。

### 位置管理

| 子命令 | 作用 |
|---|---|
| `list` | 列出已注册位置（id、读写、类型、名称；不显示密码） |
| `show <位置>` | 显示一个位置的连接信息（不含密码） |
| `add-webdav <名字> --url <URL>` | 添加一个 WebDAV 位置 |
| `remove <位置>` | 移除位置（只删本地注册，**不删云端文件**，有确认） |
| `rename <位置> <新名字>` | 只改本地显示名，不碰服务端 |

`add-webdav` 选项：

| 选项 | 说明 |
|---|---|
| `--url <URL>` | **必填**，WebDAV 根地址，如 `https://dav.example.com/dav` |
| `--user <USER>` | 用户名 |
| `--vendor <NAME>` | `generic`（默认）或 `nextcloud` |
| `--read-only` | 只允许读取（默认可写） |
| `--anonymous` | 匿名访问：不存用户名/密码，与任何 `--password-*` 互斥 |
| `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>` | 密码来源，三选一 |

除加 `--anonymous` 外**密码必填**（交互隐藏输入 / stdin / 文件 / 环境变量），不再接受「留空当匿名」。添加前会先构造客户端并列一次根目录探活：URL 非法或连不上时**不会保存位置**。和其它命令一样，**没有 `--password <明文>`**。

### 浏览与文件操作

| 子命令 | 作用 |
|---|---|
| `ls <位置> [路径]` | 列出远程目录内容，路径默认根目录 |
| `upload <位置> <本地路径> <远程目录>` | 上传本地文件；本地路径是**目录时递归上传整棵树**（见下） |
| `download <位置> <远程文件> <本地路径>` | 分块（1 MiB）下载**密文**到本地，流式写盘（不解密） |
| `copy <源位置>:<文件> <目标位置>:<路径>` | 在两个位置之间原样复制远程文件（密文字节流） |
| `mkdir <位置> <远程目录>` | 建远程目录，缺失的中间层级一并创建 |
| `delete <位置> <远程路径>` | 删除远程文件或目录（目录递归删除，不可恢复；默认确认，`--force` 跳过） |
| `move <位置> <源路径> <目标路径>` | 同目录内改名/移动；跨目录暂不支持，会明确报错 |
| `decrypt <位置> <远程.omy> <本地目录>` | 远程单个 `.omy` 流式边下边解到本地目录 |

这些写操作都有 `--force`：**默认拒绝覆盖同名目标**，确认要覆盖才加它——`upload` 覆盖远程同名文件、`download` / `decrypt` 覆盖本地已存在文件、`copy` 覆盖目标远程文件。`delete` 的 `--force` 是跳过「永久删除」确认。

**`ls` / `upload` / `download` / `mkdir` / `delete` / `move` / `cache pin` / `cache unpin` 都带同一组 `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>`。** 它是**位置 session 密码**，与 `.omy` 文件密码是两码事，且**只在「per-place 加密的 Telegram 位置、本机机器密钥又自动解不开」时才真的需要**：WebDAV 位置永远不需要（它的密码由本机凭据库信封解开）；未加密的 Telegram 位置不需要；已加密但本机机器密钥能自动开的位置也不需要。判定为不需要时**既不提示、也不消费 stdin**，对未加密位置误喂 `--password-stdin` 不会吃掉后面真正要用 stdin 的输入。需要而又没给通道、且不在 TTY 上时，报 `TG_PLACE_PASSWORD_REQUIRED`（退出 1）。

几点真实行为：

- **`upload` 传文件或整个目录**：本地路径是文件就传该文件；是目录就递归上传整棵树，在远程 `远程目录` 下建一个同名镜像目录。目录上传**不是事务**——一个文件失败不会回滚已传好的兄弟文件，命令逐项记录 `files_ok` / `files_failed`，有任一失败就以非零退出，并明确提示「部分成功」。
- **提交是原子临时改名**：目标支持改名/删除时，先写随机临时名再 MOVE 成最终名，失败可精确清理残留；不支持改名的位置直写最终名，失败尽力清理，清不掉的残留风险由错误信息带出。`upload` / `copy` / `decrypt` 都走这同一份共享提交逻辑。
- `copy` 复制的是**原始字节（密文）**，两端不需要共享同一个**文件**密码；传输是**有界流式**的（64 KiB 管道背压分块，内存里同时只有一两块），大文件不会把整份读进内存。但两端各自可能是一个加密 Telegram 位置，所以密码通道按端拆开：源端用 `--source-password-stdin` / `--source-password-file` / `--source-password-env`，目标端用 `--dest-password-*`；每端都**只在那一端是加密 Telegram 位置、本机自动解不开时才需要**。**双 stdin 互斥**：一条 stdin 只能读一次，当两端都真的需要密码、且你两端都选了 `--source-password-stdin` / `--dest-password-stdin` 时会直接报错，把其中一端改用 `--*-file` / `--*-env` 即可（只有一端需要时，另一端那个多余的 stdin 标志根本不会被读，无害）。
- `move` 目前只做**同目录改名**（和 `remote rename <位置> <新名字>` 区分：后者改的是本地显示名）。跨目录移动受限于 store 抽象，发请求前即报错。
- `decrypt` 只解**单个 `.omy` 文件**：远程目录、加密文件夹（容器）都明确拒绝。它只读文件头再按块边下边解，不先把整份密文落盘；文件名取自文件头内部并做路径清洗，本地输出目录不存在会创建。这里有**两种密码，别混**：`--password-stdin` / `--password-file` / `--password-env` 是 **`.omy` 文件本身的密码**；而 `--place-password-stdin` / `--place-password-file` / `--place-password-env` 是**解锁那个加密 Telegram 位置 session 用的位置密码**。位置未加密时后者不需要。
- 远程路径里的 `..` 段会在发请求前被拒，避免越级写到意料之外的目录；相对路径会自动补成以 `/` 开头的绝对路径。
- 写目标是只读位置时，发请求前即拒绝。

### 远程缓存

| 子命令 | 作用 |
|---|---|
| `cache status` | 显示临时/永久缓存占用与永久保留清单 |
| `cache clear` | 清空临时缓存块（永久保留的文件不受影响） |
| `cache pin <位置> <远程文件>` | 把完整远端原文件保存到永久目录（离线可用，可直接复制/查看） |
| `cache unpin <位置> <远程文件>` | 删除该完整永久副本；已有临时分块仍由 LRU 管理 |

缓存目录与上限与 GUI 同口径（`remote.cache_dir` / `remote.cache_limit`）。临时层仍是内部哈希分块；永久层按稳定远程身份存完整原文件：Telegram 使用 `userId/chatId/messageId/服务端文件名`，WebDAV 使用规范化 URL + username 隔离源、远端路径哈希隔离条目。复制或合并整个 `pinned` 目录后，重新添加同一远程即可复用；普通未加密远端文件在该目录中保持原格式和内容可见。

### Telegram

```
omy remote telegram <COMMAND>
```

#### 登录与会话

| 子命令 | 作用 |
|---|---|
| `login` | 扫码登录一个 Telegram 账号，并保存为一个位置 |
| `phone` | 手机号 + 验证码 + 两步密码登录 |
| `logout <位置>` | 移除位置并**销毁**本机登录态（之后要用须重新登录） |
| `detach <位置>` | 从列表移除位置，但**保留**本机登录态（之后可重新加回而不必重登） |
| `tdata probe` | 探测本机常见的 tdata 位置（只读目录） |
| `tdata check <目录>` | 检查一个目录是否像 tdata（只看结构，不解密） |
| `tdata import <目录>` | 从桌面端 tdata 导入登录态：先问服务端认不认，成功才收编 |
| `status [位置]` | 查看 Telegram 位置的本地会话状态（**不联网**） |

`login` 选项：

| 选项 | 说明 |
|---|---|
| `--name <名字>` | 位置名，默认取服务端昵称 |
| `--proxy <URL>` | 代理地址，如 `socks5://127.0.0.1:7897`；默认自动探测系统代理 |
| `--password-stdin` / `--password-file <PATH>` / `--password-env <VAR>` | 两步验证密码来源，三选一 |

`phone` 在 `login` 之上多了 `--phone <号码>`（国际格式，如 `+861…`；交互下可省略）与验证码通道 `--code-env` / `--code-file` / `--code-stdin`。`tdata import` 多了 `--name`、`--proxy`，以及 tdata 本地密码 `--passcode-stdin` / `--passcode-file` / `--passcode-env`。

**所有建连入口共用同一份连接上下文**：扫码 `login`、`phone`、`tdata import`、之后的重连与打开已有 session，用的 api_id / api_hash 与代理都从同一个配置解析出来（`--proxy <URL>` 只是那一次登录的一次性覆盖）。原因是已落盘 session 把建连时的 api_id 记在信封里，加载时会校验——「打开/重连」用的 api_id 必须与「当初登录」同源，否则换了配置就撞 `SessionMismatch`（旧登录态无法沿用，需重登一次）。

**tdata 是这条同源规则的唯一边界**：桌面端 `tdata` 里的 auth key 是在 Telegram Desktop 自己的 api_id（内置 **2040**）下协商出来的，所以 `tdata import` 会**无视你配置里的自定义应用身份、强制用内置 2040** 去建连——配别的 id 会以难解释的方式失败。**代理仍与其它入口同源**，这条边界只钉死 api_id。

二维码打印到 **stderr**：在真终端里用 Unicode 半角块渲染；管道 / 重定向（非 TTY）时回落到纯 ASCII，并**始终同时打印一行可复制的 `tg://login?token=...` 链接**，手机上可在别处打开。二维码过期会自动刷新。两步验证密码绝不出现在 argv；非交互（管道）且密码错了只试一次就退出，不会对着管道反复等。

::: warning 成功的 `--json` 结果不含登录票据
那行 `tg://login?token=...` 是短时可扫码票据，**只打到 stderr，不进 `--json` 的 stdout**——否则 `... --json | tee …` 这类脚本会把它原样写进日志，等于持久化一个别人扫一下就能登录的链接。成功 JSON 只含 `id` / `name` / `duplicate` / `user_id` / `session_saved`。
:::

登录成功后按账号 `user_id` 去重：同一个账号重复登录会收编到已有位置而不是新建。位置一旦保存，上面的 `ls` / `upload` / `download` / `copy` / `cache` 对 Telegram 位置同样可用（每个对话是一个目录）。

#### 代理与连通性

| 子命令 | 作用 |
|---|---|
| `proxy-status` | 查看 Telegram 全局代理当前生效情况 |
| `proxy-set --url <URL>` | 手动设全局代理（`http://`、`host:port` 会自动归一为 SOCKS5） |
| `proxy-set --system` | 改为跟随系统代理（与 `--url` 互斥） |
| `proxy-reset` | 恢复为跟随系统代理 |
| `check` | 登录前连通性自检：按配置的代理连 Telegram 主 DC，只测 TCP；**不通时退出码为 1** |

这是一个**全局**策略，所有账号与登录 / 浏览 / 转发共用，和 GUI「设置 › Telegram 代理」同一份。

#### 应用身份（api_id）

| 子命令 | 作用 |
|---|---|
| `app-id-status` | 查看当前应用身份（内置或自定义 api_id） |
| `app-id-set --api-id <ID> --hash-*` | 保存自定义 api_id / api_hash；`api_hash` 经 env/file/stdin 安全通道输入，**封成加密信封**，不进 argv |
| `app-id-reset` | 恢复使用内置应用身份 |

自定义 `api_hash` 由系统凭据库的主密钥封成信封落盘；本机没有可用凭据库时**拒绝写明文**，让你先用内置身份。旧版本写成明文 `api_hash` 的配置会在下次解析时，于配置跨进程锁内自动重封成信封完成迁移（无凭据库则保留明文不动，不丢身份）。

`api_hash` 是凭据，**任何对外输出都不泄露它**：它从不进 argv，`app-id-status` / `remote show` 及其 `--json` 只报公开的 api_id 与「内置 / 自定义」，`SessionMismatch` 等错误也只带两个 api_id 数字、不带 hash。连接上下文这个结构甚至不派生 `Debug`，手写调试输出只报公开的 id 与代理有无。

#### 位置独立加密（离线）

这组命令只变换落盘的登录态信封，**不联网**：

| 子命令 | 作用 |
|---|---|
| `encrypt <位置>` | 用现场密码独立加密该位置登录态（KDF 默认 `moderate`，可选 `--kdf`） |
| `unlock <位置>` | 用现场密码验证能否解开（一次性进程，验证完即结束，不持久） |
| `lock <位置>` | 确认位置已加密并处于锁定态（CLI 无长期会话，不改盘） |
| `decrypt <位置>` | 取消位置加密，恢复为机器密钥保护 |

位置密码一律走 `--password-stdin` / `--password-file` / `--password-env`。CLI 没有 GUI 那样的长期会话：`unlock` 只判断「密码对不对」，进程退出后内存即清空，下次访问仍需通过文件命令的 `--password-*` 现派密钥；`lock` 只是如实确认它是加密位置，未加密的位置会报 `[tg_not_encrypted]` 而不是假装已锁。错误是带方括号前缀的稳定串（`[tg_unlock_wrong]` 密码错 / `[tg_no_protector]` 无凭据库拒绝落明文 / `[tg_no_session]` 还没登录态），退出码都是 1——脚本应匹配前缀，不要匹配退出码。

注意区分两类 Telegram 错误标记：这组**离线**命令（以及 `targets` / `forward` / `search` / `group`）的错误是消息里带的 `[tg_*]` 方括号串、退出码恒 1；而**建连**阶段（`ls` / `upload` / `download` / `copy` / `decrypt` 等去连一个加密 Telegram 位置时）的失败走结构化 `TG_*` 码，进 `--json` 的 `error.code`，且密码错是退出 3。两者不要混为一谈。

#### 联机操作（需真实连 Telegram）

| 子命令 | 作用 |
|---|---|
| `targets <位置>` | 列出可接收转发的会话（转发目标选择） |
| `forward <位置> --target tg:CHAT --entry tg:CHAT:MSG…` | 把同一源对话里的一条或多条消息原生转发到目标会话 |
| `search <位置> <QUERY>` | 服务端搜索消息内容（`--dir tg:CHAT` 限定对话，缺省全局；`--limit N` 默认 50） |
| `group <位置> <TITLE>` | 创建一个只含自己的私密超级群（转发归档目标） |

这些都要真连 Telegram；加密位置需经 `--password-*` 现场解锁。`forward` 的源条目可重复给 `--entry tg:<对话>:<消息>`，或用 `--entries-json <文件>` 读一个 id 数组（可并用合并）；**所有条目必须来自同一源对话**，否则报 `[tg_cross_chat]`。

::: warning 对着真号完成登录 / 转发 / 搜索这段网络尚未无头端到端实测
`phone` / `tdata import` / `targets` / `forward` / `search` / `group` 的参数接线、失败报错都已完成，离线部分（信封加密、代理配置、连通性自检、虚拟收藏）有单测与对着本地 dav-server 的 WebDAV 端到端兜底；但真正连 Telegram 数据中心、用真人账号完成扫码 / 收验证码 / 转发 / 搜索这一段，无头环境代替不了人操作，**没有当成已验证**。详见[远程位置](../guide/remote-locations)末尾的覆盖表。
:::

### 虚拟远程位置（收藏夹）

```
omy remote virtual <COMMAND>
```

虚拟位置不连任何服务器，只在本地存「对真实远程文件的引用」（收藏夹）。本树**全部命令都不发网络请求**，与 GUI 共用同一份模型与落盘。

| 子命令 | 作用 |
|---|---|
| `place create <名字>` / `list` / `show <位置>` / `rename <位置> <名>` / `remove <位置>` | 虚拟位置管理（`remove` 删位置及其收藏，不碰真实远程文件） |
| `folder add <位置> <名字>` / `rename <位置> <文件夹id> <名>` / `remove <位置> <文件夹id>` | 虚拟文件夹（`remove` 连同整棵子树引用）；用 `--root` 指根目录或 `--folder <id>` 指父文件夹 |
| `ref add <位置>` | 收藏一个真实远程文件：`--source-place <p…>` / `--dir-id` / `--file-id` / `--name`（`--folder` 指目标文件夹） |
| `ref remove <位置> <引用>` / `move <位置> <引用>` / `copy <源位置> --ref-id <引用> --to-place <位置>` | 整理引用（`remove` 不碰真实文件；`move` 同位置内、`copy` 可跨位置并生成新引用 id） |
| `ref list <位置>` | 递归列出位置 / 文件夹下全部引用 |
| `ref browse <位置>` | 单层浏览：直属子文件夹与引用 |
| `encrypt <位置>` / `unlock <位置>` / `lock <位置>` / `decrypt <位置>` | 独立密码加密 / 校验密码 / 确认锁定 / 取消加密 |

位置 id 形如 `v1`、`v2`。**一次性进程语义**：CLI 每条命令都全新载入配置与收藏，加密位置在新进程里默认锁定。因此读 / 改命令都接受同一组 `--password-*`；没密码就报 `VIRTUAL_LOCKED`。

- `unlock` 只**验证密码对不对**：成功不持久、不落盘，进程退出后下次访问仍需密码。
- `lock` 是如实确认：一次性 CLI 进程间加密位置本就锁定，它不改盘；未加密位置报 `VIRTUAL_NOT_ENCRYPTED`，而不是对任何位置都硬报「已锁定」。
- `encrypt` 用独立密码加密整份收藏树；`decrypt` 解开后写回明文收藏。
- 退出码：成功 0、用法错 2、**虚拟密码错 3**（`VIRTUAL_WRONG_PASSWORD`）、取消确认 8、其余虚拟失败 1。机器可读错误码在 `--json` 的 `error.code`（`VIRTUAL_*` 前缀），脚本应匹配它。
- 引用记的是源位置的**稳定标识**（Telegram 取 `user_id`、WebDAV 取 url+账号），源位置被移除后再加回来，引用会重新认上；任何操作都不碰真实远程文件。

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
