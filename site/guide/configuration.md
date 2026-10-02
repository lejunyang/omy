---
title: 配置文件
---

# 配置文件

把常用参数写成默认值，不必每次都在命令行里重复。

## 位置

omy **优先把配置放在程序自己的目录旁**，找不到可写位置时才退回系统目录。

| 情形 | 路径 |
|---|---|
| 程序目录可写（便携） | 可执行文件旁的 `omy-data/config.toml` |
| Windows（回退） | `%APPDATA%\omy\config.toml` |
| Linux（回退） | `~/.config/omy/config.toml` |
| macOS（回退） | `~/Library/Application Support/omy/config.toml` |
| Android / iOS | 应用私有数据目录 |

::: tip 为什么优先放程序目录旁
omy 常被连同加密文件一起放在移动硬盘或 U 盘上。配置散落在系统目录里，换台机器就全没了；放在程序旁边，拷走那一个文件夹就能带走全部设置与缓存。

装在 `C:\Program Files`、`/usr/bin` 这类只读位置时会自动回退到系统目录。判定方式是真的写一个临时文件试试，而不是看权限位——只读挂载和 Windows 的 ACL 都不是单看权限位能判断的，判断错的后果是设置悄悄丢失。
:::

便携根一旦确定，其它本机状态也都落在同一个 `omy-data/` 下，拷走整个文件夹才是完整迁移：

| 状态 | 便携路径 | 系统回退 |
|---|---|---|
| 配置文件 | `omy-data/config.toml` | `%APPDATA%\omy\config.toml` 等 |
| 远程缓存（临时分块 + 完整永久文件） | `omy-data/cache/` | 系统缓存目录下的 `omy/` |
| 日志 | `omy-data/logs/` | 系统数据目录下的 `omy/logs/` |
| 设备库（本机身份与配对记录，内含私钥） | `omy-data/devices.omy` | `%APPDATA%\omy\devices.omy` 等 |

设备库路径另有环境变量 `OMY_DEVICE_STORE` 可显式覆盖（指向一个具体文件），优先级高于便携根——例如把身份指到随身 U 盘做多身份。空白值视为未设置。设了 `OMY_DEVICE_STORE` 时程序按你指的路径走，**不做**下面的自动迁移。

::: tip 升级时老设备库会自动迁一次
便携位置是后来才纳入默认路径的。旧版本的设备身份存在系统配置目录里；升级到便携模式后，仅当**便携 `devices.omy` 还不存在、而老系统 `devices.omy` 存在**时，程序会把老库一次性复制到便携位置（先写临时文件再改名，失败则本次仍用老库、下次重试）。老库**永不删除**，并发启动也绝不会互相覆盖。也就是说配对身份会跟着 `omy-data/` 走，不会因为升级凭空生成一份空白身份。
:::

文件默认**不存在**，此时全部用内置默认值。`omy doctor` 会打印当前实际使用的路径；图形界面里在「设置 › 关于」也能看到。

用 `--config` 可临时指定别的文件：

```bash
omy --config ./ci-config.toml encrypt file.mp4
```

::: tip 显式指定与默认路径的区别
`--config` 指定的文件读不到会**直接报错**；默认路径的文件不存在则静默用默认值。区别是刻意的——你明确指定的文件被静默忽略，会让你以为配置生效了。
:::

## 日志

图形界面会把操作、接口调用和错误写进一份滚动日志，供排查问题用。

| 情形 | 路径 |
|---|---|
| 便携 | 可执行文件旁的 `omy-data/logs/omy.log` |
| 回退 | 系统数据目录下的 `omy/logs/omy.log` |
| Android / iOS | 不写文件日志 |

在「设置 › 关于」里能看到日志目录的完整路径，旁边的按钮可直接在文件管理器里打开它。

几点需要知道：

- 单个日志文件写到 5 MiB 就滚动，最多保留 4 个历史文件（`omy.log.1` 到 `omy.log.4`），更旧的自动删除。磁盘占用因此有上界，约 25 MiB。
- 日志里**不含**密码、验证码、两步验证密码、手机号、登录凭据或解密后的文件内容。接口调用只记方法名、结果和耗时；文件与对话标识记成不可逆的短哈希，用来在日志里对上是不是同一个，但无法还原出原始路径。
- 日志目录不可写时（例如装在只读位置）不写文件，程序照常运行。

## 图形界面共用同一份配置

GUI 的「设置」写的就是这个文件，命令行与图形界面读同一份、用同一套默认值。

两点需要知道：

- **保存时会保留 omy 不认识的键。** 这样在新旧版本之间切换不会互相抹掉对方写入的设置。
- **保存会丢失注释。** 手写配置文件时加的注释，在图形界面里保存一次之后就没有了。
- **GUI 与 CLI 并发写不会互相冲掉。** 位置的增删改（`remote.places`）与 Telegram `api_hash` 的写回都在同一把跨进程配置锁内完成「读最新磁盘 → 改本次那一处 → 原子写回」，而不是拿启动时的快照整体覆盖——否则 CLI 刚加的位置会被常驻 GUI 的旧快照写回时静默删掉。锁是 OS 建议锁（配置文件旁的 `config.toml.lock`，进程崩溃随句柄自动释放，无残留锁文件），拿不到锁 5 秒超时即报错；读路径不加锁，临时文件 + 改名让读者要么看到旧文件要么看到新文件，不会读到半截。

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
# 启动时自动开启局域网共享（仅 GUI）
autostart = false

[ui]
# auto | zh-CN | en
language = "zh-CN"
# auto | dark | light
theme = "dark"
# grid | list
view = "list"
# 启动时打开：last（上次目录）| home（主目录）；仅 GUI
startup = "last"
# 上次浏览目录，startup = "last" 时由应用自动记录，一般不用手写
# last_dir = "C:/Users/me/Vault"
# 列表中是否显示缩略图（缩略图存在文件头里）
thumbnails = true

[security]
# 闲置多少秒后自动锁定；0 表示从不
auto_lock_secs = 600
# 切到后台（移动切应用 / 桌面最小化）时立即锁定；
# false（默认）时切后台只等于「开始闲置」，仍按 auto_lock_secs 计时，播放中豁免
lock_on_background = false
# 用外部程序打开加密文件后，关闭时立即清理临时明文（能力待接入 GUI）
wipe_temp_plaintext = true

[remote]
# 临时分块缓存上限（字节）；0 表示不限制
cache_limit = 2147483648
# 缓存目录；留空用默认（桌面为可执行文件旁 omy-data/cache/remote，移动为 app data）
# cache_dir = ""
# 退出应用时清空缓存
clear_cache_on_exit = false
# 仅在 Wi-Fi 下缓存（移动端，暂未生效）
cache_wifi_only = false
# 远程扫描并发请求数，1–32；这是风控边界不是性能调参，调高易被限流
scan_concurrency = 8
# 远程扫描是否只看 .omy 扩展名；false 可识别伪装文件但每个文件多一次请求
scan_omy_only = true
```

::: tip 哪些键 CLI 也用、哪些只在图形界面生效
`remote.places`、`remote.cache_dir`、`remote.cache_limit` 现在由 GUI 与 `omy remote` 命令**共用**：命令行加的位置、缓存目录与上限设置两边一致。
`security.*`（自动锁定等）以及 `remote.clear_cache_on_exit` / `scan_concurrency` / `scan_omy_only` 仍只在图形界面生效。两边读同一份文件，CLI 用不到的键会被正常保留，不会当成未知键警告。
:::

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
| `serve.autostart` | `false` | 是否开机自动开启局域网共享（仅 GUI）|
| `serve.default_expire` | 无 | 默认授权有效期 |
| `ui.language` | `"auto"` | 界面语言，`auto` 跟随系统 locale |
| `ui.theme` | `"auto"` | 界面主题，`auto` 跟随系统（仅 GUI）|
| `ui.view` | `"grid"` | 默认视图，`grid` 或 `list`（仅 GUI）|
| `ui.startup` | `"last"` | 启动时打开，`last`（上次目录）或 `home`（主目录）（仅 GUI）|
| `ui.last_dir` | 无 | 上次浏览目录，应用自动记录（仅 GUI）|
| `ui.thumbnails` | `true` | 列表中是否显示缩略图（仅 GUI）|
| `security.auto_lock_secs` | `0` | 闲置多少秒后锁定，`0` 为从不（仅 GUI）|
| `security.lock_on_background` | `false` | 切后台时**立即**锁定；关闭时切后台只按闲置计时、播放中豁免（仅 GUI）|
| `security.wipe_temp_plaintext` | `true` | 外部程序关闭后清理临时明文（GUI 能力待接入）|
| `password_managers.keepassxc.proxy_path` | 无 | KeePassXC proxy 的自定义路径；留空时自动探测。桌面 GUI 可在“设置 → 安全与密码 → KeePassXC 便携版”修改 |
| `password_managers.keepassxc.associations` | `[]` | 已关联数据库的公开 hash 与名称；授权 key 单独存入系统凭据库 |
| `remote.cache_limit` | `2147483648` | 临时分块缓存上限，2 GiB；`0` 不限制（GUI 与 CLI `remote cache` 共用）|
| `remote.cache_dir` | 无 | 自定义缓存目录，留空用默认便携路径（GUI 与 CLI `remote cache` 共用）|
| `remote.clear_cache_on_exit` | `false` | 退出应用时清空临时分块缓存（仅 GUI；永久文件不受影响）|
| `remote.cache_wifi_only` | `false` | 仅 Wi-Fi 下缓存（移动端，暂未生效）|
| `remote.scan_concurrency` | `8` | 远程扫描并发请求数，1–32（仅 GUI）|
| `remote.places` | `[]` | 已保存的远程位置，GUI 与 CLI `remote` 命令共同维护；密码在其中以加密信封形式保存 |
| `remote.telegram_api_id` | 无 | 自定义 Telegram api_id（整数）；留空用内置身份（GUI 与 CLI `telegram app-id-*` 共用） |
| `remote.telegram_api_hash` | 无 | 自定义 api_hash，以系统凭据库封成的加密信封保存；旧版本写成的明文会在跨进程锁内自动重封，无凭据库时拒绝写明文 |
| `remote.scan_omy_only` | `true` | 远程扫描是否只看 `.omy`（仅 GUI）|

::: warning original_action 改成 trash 或 delete 要想清楚
这两个值会让原文件在加密后从原位置消失。写进配置文件意味着**以后每次加密都这样**。仍然会要求确认（除非加 `--yes`），但配置 + `--yes` 的组合在脚本里很容易变成静默删除。
:::

## 拼错的键会被警告并忽略

```toml
[defaults]
kdf_profil = "mobile"    # 少个 e
```

```
警告: 配置中有无法识别的项 `defaults.kdf_profil`，已忽略
```

命令照常执行，那一项用默认值。**必须看到这条警告**——静默忽略会让你以为配置生效了，而实际上一直在用默认值。

::: tip 为什么不直接报错
早期版本遇到未知键会直接拒绝整个文件。但图形界面也写这份配置，一旦新版本加了键，旧版本就完全读不了，你在两个版本之间切换一次设置就全失效了。现在改为「警告并忽略」，既能发现拼写错误，又不会让跨版本读写互相破坏。
:::

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
