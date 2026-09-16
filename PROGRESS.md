# omy 实现进度

> 本文件是**跨会话的权威状态来源**。每完成一个可验证的阶段就更新，
> 并随代码一起提交，以便任何时候都能接续。
>
> 最后更新：2026-09-16

## 状态速览

| crate | 状态 | 测试 | 说明 |
|---|---|---|---|
| `omy-core` | 🟢 格式核心可用 | 154 项 | 格式读写、密钥、分块、分片、原子写、扫描、容器、BlockSource、媒体 TLV、符号链接 |
| `omy-config` | 🟢 可用 | 13 项 | **CLI 与 GUI 共用**的配置读写；便携优先、写回保留未知键、原子写盘 |
| `omy-remote` | 🟢 WebDAV 可用 | 28 项 | `RemoteStore` 抽象、能力位图、WebDAV 驱动、密文块缓存、`BlockSource` 桥接 |
| `omy-cli` | 🟢 13 个命令可用 | 65 项 + 88 项端到端 + 19 项局域网实测 | 配置层已切到 `omy-config` |
| `omy-media` | 🟢 探测/分级/moov/缩略图/**P2 转封装**可用 | 117 项 + 105 项真实文件验证 | LGPL，FFmpeg 封装 |
| `omy-net` | 🟢 全链路可用 | 151 项 + 55 项端到端 | 编解码、配对、mDNS、Noise IK、零密钥服务端、加密持久化、授权会话、服务端主循环 |
| `omy-gui` | 🟢 桌面端可用，Android 已在模拟器实测 | 162 项 + 43 项端到端 + 23 项设置与远程实测 | Tauri v2；Vue 3 + Vite；**设置页**、**远程位置（WebDAV）**、**自动锁定**已接入 |

`cargo test --workspace` 全绿，`cargo clippy --workspace --all-targets -- -D warnings` 零告警。

### 本轮新增（远程位置与设置，2026-09-16）

设计文档见 [14-remote-locations-cloud.md](docs/research/14-remote-locations-cloud.md)，
界面原型见 [appendix/remote-locations-prototype.html](docs/research/appendix/remote-locations-prototype.html)。

| 能力 | 状态 | 验证方式 |
|---|---|---|
| 共享配置模块（`omy-config`） | ✅ | 13 项单测 + 变异测试（取消未知键保留 → 被抓到）|
| 配置便携优先（exe 旁 `omy-data/`，移动 app data） | ✅ | 端到端实测确认路径，只读位置回退靠真实写入判定 |
| WebDAV 驱动（列目录/Range 读/写/删/改名） | ✅ | 单测；只读配置在**发请求前**拒绝写 |
| 密文块缓存（LRU + 上限，版本键防同名旧文件串台） | ✅ | 含「扫描缓存目录确认无明文」「LRU 不自噬」「同名更新不命中旧缓存」硬断言 |
| 远程位置 GUI（侧栏、添加、浏览、识别 `.omy`） | ✅ | 端到端断言 |
| **远程 `.omy` 预览与点播（真实视频、Range/拖动 seek）** | ✅ | 自建 WebDAV 服务器 + ffmpeg 真视频，移动/桌面两视口点播成功；无 Range 也正确截断 |
| 设置页七分类（PC 弹窗 / 移动列表 + 二级页） | ✅ | 通用/远程位置/安全与密码/加密默认值/播放与预览/设备与共享/关于，逐页断言 |
| 密文缓存二级页（真实用量、上限档、清空、打开目录、退出清空、即时生效） | ✅ | 端到端改值→关闭→后端读回 |
| 自动锁定（闲置、切后台按闲置、播放中豁免、立即锁定、当前会话计数） | ✅ | 端到端：点立即锁定后会话密码数归零、弹窗关闭 |
| 加密默认值驱动加密对话框初始值 | ✅ | 端到端写入非默认值后打开对话框逐控件核对 |
| 关于页（应用/OMYFILE 格式版本、分层许可、配置路径、便携模式） | ✅ | 版本取编译期常量，端到端核对文本 |
| 缩略图开关真正生效、设备与共享页、默认视图持久化、启动位置（上次/主目录） | ✅ | 端到端：切视图后配置落盘；导航后 last_dir 落盘 |
| 只读能力投影（菜单项/操作按能力禁用） | 🟡 部分 | 后端只读写请求已拒绝；只读菜单「请先解密到本地」提示随写链路落地 |

**尚未实现**（见下方「还没做」）：远程位置持久化与 WebDAV 凭据加密（**当前位置仅存内存，
重启丢失，是首期可用性硬伤**）、加密上传/解密到本地/新建文件夹/改名删除等远程**写链路**、
加密文件用外部程序打开后擦除临时明文、删除优先回收站、远程缩略图接线、缓存自定义目录/
头部缓存单独计量、私有云盘驱动（光鸭等）。


### Spike 结论

| Spike | 结论 | 影响 |
|---|---|---|
| S1 自定义协议 206 + seek | ✅ **通过**（16/16） | 不需要本地 HTTP server，省掉端口占用与 CORS 复杂度 |
| S5 WebView 缓存泄露 | ✅ **通过**（有前提） | 必须保留 `Cache-Control: no-store` 等响应头 |
| LAN 协议栈（SPAKE2 + Noise IK） | ✅ **通过**（10/10） | 见下方「局域网 spike 的三个硬结论」 |
| S2/S3/S4/S6/S7/S8 | ⚪ 未开始 | — |

复现：`pwsh -NoProfile -ExecutionPolicy Bypass -File spikes\run-spike.ps1`
（全自动，无需人工点击；需先 `cargo build --release -p omy-spike-webview`）

#### S1 实测数据

- 6 次乱序 seek（85%/15%/60%/5%/95%/35%）全部触发 `seeked`，落点偏差 **0.00s**，耗时 1~129 ms，`readyState` 均为 4
- canvas 在 5/30/50 秒取到 503/538/529 种颜色，三帧特征互不相同 → 排除「seek 报成功但画面卡住」的假通过
- Range 语义：无 Range → 200；`bytes=0-1023` → 206/1024B；`bytes=-512` → 206/512B 且 `Content-Range: bytes 4459850-4460361/4460362`；越界 → 416 + `bytes */total`
- `fromDiskCache` 全程为 0

#### 产品实现必须遵守的约束（由 S1/S5 实测得出）

1. **响应头必须带 `Cache-Control: no-store, no-cache, must-revalidate` 与 `Pragma: no-cache`**。S5 只证明「带上时不落盘」，未证明「不带也不落盘」。
2. **必须回 CORS 头**。页面 origin 是 `http://tauri.localhost`，自定义协议是 `omystream://localhost`，不同源。`<video>` 不检查 CORS 所以能播，但 `fetch` 会被拦、canvas 会被污染——图片预览、文本读取、缩略图生成都依赖这两者。且必须 `Access-Control-Expose-Headers: Content-Range`，否则 JS 读不到它。
3. **`<video>` / `<img>` 要加 `crossorigin="anonymous"`**，否则即使服务端放行，canvas 取帧仍报污染。
4. **必须提前处理 `OPTIONS` 预检并返回 204**，不能落入解密路径——实测预检曾触发整个文件解密。
5. **必须限制单次响应上限**。WebView 的 seek 请求是开放结尾（`bytes=1572864-`），返回到文件末尾等于近全量解密。当前上限 2 MiB；产品应按码率与块大小调整，并考虑对齐块边界避免同块重复解密。无 Range 的 200 响应仍须返回完整内容。
6. **协议处理器必须用异步版**（`register_asynchronous_uri_scheme_protocol`），同步版会阻塞 WebView 线程。


## 已完成

### omy-core 格式核心（提交 `ec5676a`）

| 模块 | 职责 |
|---|---|
| `crypto` | 两级 KDF（Argon2id → HKDF）、AEAD 抽象、密钥析构擦除 |
| `header` | Fixed Header 编解码、O(1) 块偏移、`argon2_params()` |
| `slot` | Key Slot Area，未用槽填随机字节 |
| `tlv` | TLV 编解码、文件名 64 B 桶填充、压缩索引 |
| `payload` | 分块 AEAD、任意区间随机访问 |
| `shard` | 切分/合并/冗余头恢复/缺片空洞分析 |
| `file` | 顶层封装、容器 FOLDER_INDEX |
| `error` | 结构化错误码 + CLI 退出码映射 |

### omy-core 运行时（提交 `27b0331`）

| 模块 | 职责 |
|---|---|
| `fsatomic` | 原子写入、临时明文擦除、`mark_dir_no_index`、清理残留 |
| `session` | KEK 会话缓存、空闲自动上锁、恢复码 |
| `scan` | 目录扫描、多密码匹配、统计 |

### omy-core 容器与数据源（本轮）

| 模块 | 职责 | 测试 |
|---|---|---|
| `container` | 目录容器索引：紧凑二进制编码、路径安全校验、区间自洽校验 | 19 项 |
| `source` | `BlockSource` trait + 内存/本地文件/容器条目视图 | 6 项 |

设计要点：

- **路径用数组而非拼接字符串**，天然规避分隔符差异与路径注入。
- **解析阶段就拒绝**空串 / `.` / `..` / 含分隔符 / 含 NUL / 含冒号 / 超 255 字节的组件，不留给调用方自觉。
- 自定义紧凑二进制而非 JSON：索引可达 10 MB 量级且要在移动端解析。
- `BlockSource` **有意做成同步 trait**（设计文档草案是 `#[async_trait]`）：`payload::read_range` 是纯 CPU 解密逻辑，异步化会污染整条调用链；core 保持 MIT/Apache 轻量、不引入 async 运行时；远程实现在 omy-net 内部自行桥接异步。已在模块 doc 注明。
- `FOLDER_INDEX` TLV 标为 **CRITICAL**：不认识该 TLV 的实现必须拒绝打开，否则会把容器误当普通文件解出一堆拼接字节。

### omy-cli（本轮）

12 个子命令全部可用：

| 命令 | 状态 | 要点 |
|---|---|---|
| `encrypt` | ✅ | 批量复用同一 KEK（Argon2 只跑一次）；`--original trash/delete` 先真解密比对再动原件 |
| `decrypt` | ✅ | 容器展开含二次路径校验 + Windows 非法字符/保留名处理并报告调整项 |
| `info` | ✅ | **slot 占用恒显示"未知"**，JSON 里 `slot_count` 恒为 null |
| `verify` | ✅ | 两阶段：无密码验结构 / 有密码验全量 |
| `list` | ✅ | 目录内 omy 文件清单 |
| `scan` | ✅ | 先探测 `(salt, Argon2 参数)` 组合再派生 KEK 再扫描 |
| `cat` | ✅ | 按块流式，内存恒定；支持 `--range` |
| `shard` | ✅ | `split` / `merge` / `check`，缺片报退出码 6 |
| `bench` | ✅ | 本机实测 KDF 与 AEAD；debug 构建会警告数字无效 |
| `doctor` | ✅ | 环境自检，如实报告未实现能力 |
| `completion` | ✅ | bash / zsh / fish / powershell |
| `key list` | ✅ | — |
| `key add/remove/change` | ⛔ 如实报未实现 | core 缺"保留 FEK、原地重建 slot 区、重算头部 MAC 且不重写载荷"的能力 |
| `encrypt --mode tree` | ✅ | 逐文件加密 + 目录名 base32；`decrypt` 侧对目录自动分流 |

模块设计要点：

- `password.rs`：五种通道 + **在 `main` 里前置拦截 `--password`**（clap 报"未知参数"对用户无帮助，而这是最易误用、后果最重的旁路 L12）。非终端时拒绝交互并指明替代；Unix 下密码文件权限过宽时警告；`first_line` 保留前导空格（密码可以空格开头）。
- `output.rs`：stdout 只放结果数据、stderr 放进度与提示，保证 `omy cat x | mpv -` 与 `omy info --json | jq` 不被污染。`report_error` **遍历整个错误链**取 code 与退出码。
- `i18n.rs`：用 `catalog!` 宏同时生成两语言查表以保证 key 不漏；缺失 key 返回 key 本身而非 panic。
- `config.rs`：`deny_unknown_fields` 让拼错的键报错而非静默忽略；显式 `--config` 读不到必须报错，默认路径不存在则用内置默认。

### omy-net 协议层（本轮）

| 模块 | 职责 | 测试 |
|---|---|---|
| `wire` | 请求/响应的二进制编解码 | 17 项 |
| `pairing` | SPAKE2 配对 + **密钥确认** | 12 项 |
| `discovery` | mDNS 广播与浏览、设备指纹 | 14 项 |
| `channel` | Noise IK 加密信道、分帧 | 12 项 |
| `handshake` | 配对与信道的接线：加密交换静态公钥 | 13 项 |
| `store` | 已配对设备与本机身份的**加密**持久化 | 20 项 |
| `server_loop` | accept 循环、超时、并发限制、访问日志 | 9 项 |
| `serve` | 零密钥服务端 + 授权会话 | 17 项 |
| `error` | 结构化错误码 | 2 项 |

#### 局域网 spike 的三个硬结论

实现前先跑了 `spikes/lan-pairing`（10 项断言），三个结论直接决定了实现：

1. **SPAKE2 在 PIN 不同时不报错**。`finish()` 照常返回 `Ok`，只是双方
   拿到不同的密钥。很容易误以为「PAKE 会自己检测错误密码」从而漏掉
   确认步骤——那样攻击者用任意 PIN 都能"成功"握手，直到通信全是乱码
   才暴露，而连接已经建立、静态公钥已经交换。因此 `pairing` 强制跑一轮
   **密钥确认**，且两侧用不同的 tag 串（否则可以把 A 的确认值反射给 A）。
2. **Noise 单条消息明文上限 65519 字节**（65535 密文上限 − 16 字节 tag）。
   `MAX_FRAME` 取该值，`MAX_READ_LEN` 取 60 KiB 为响应头留余量，
   并有测试锁定「按 MAX_READ_LEN 读满时响应必须能编码」。
3. IK 握手 2 条消息完成，单会话跑 64 轮请求-响应正常，满足连接复用。

#### mDNS 广播里放什么，不放什么

mDNS 是**局域网内明文广播**，同网段任何设备都能收到。TXT 记录只放
三项：协议版本、设备指纹（静态公钥的 SHA-256 前 8 字节）、设备显示名。

**不放**：文件数量、共享路径、用户名、vault 解锁状态、文件名密文——
要么泄露隐私，要么帮攻击者筛选目标。有一条回归测试锁定「TXT 只有 3 个
字段」，日后有人想"顺便广播文件数"时会失败并提醒他这是明文广播。

指纹用哈希而非公钥前缀：直接截断的话，攻击者可以刻意构造前 8 字节
相同的公钥造成碰撞。有测试断言指纹**不等于**公钥前缀。

对方广播的设备名同样要校验控制字符——否则可用于伪造终端输出。

#### 配对与信道的接缝：两个方向必须用不同 nonce

`handshake` 用 SPAKE2 派生的密钥加密交换静态公钥。同一个密钥要加密
两条消息（各方向一条），**绝不能用同一个 nonce**——ChaCha20-Poly1305
在 (key, nonce) 重用下会直接泄露明文异或值并使认证失效。

实现按角色区分 nonce 首字节，并有两条测试固定这个不变量：
方向 nonce 必须不同、跨方向解密必须失败。

公钥加密传输不是因为公钥是秘密（明文发也不影响 Noise 安全性），
而是防止被动观察者记录「这两台设备配过对」，长期积累可画出设备关系图。
#### GUI 前端重构为 Vue 3 + Vite

原来是手写的 `index.html` + `app.js` + `i18n.js` 直接放 `dist/`，
无打包步骤。到 20 KB 单文件时三个问题同时出现：

- 所有渲染都是字符串拼 HTML，每处动态文本都要记得手动 `esc()`
- 搜索框每敲一个字整页重绘会失焦，只能手写「只重绘内容区」的特例，
  再重新绑定其中的事件
- 切语言必须记得末尾补一次全量重绘，漏了就是半个界面还是旧语言

这些都是模板引擎解决过的问题。加打包步骤后 `{{ }}` 天然转义、
diff 保持焦点、响应式自动重渲染，那些特例连同注释一起删掉了。

#### 严格 CSP 决定了构建配置

应用的 CSP 是 `script-src 'self'`，不允许 eval 与内联脚本。所以：

- **必须预编译模板**。Vue 的运行时编译器内部用 `new Function`，
  在这个 CSP 下直接抛错。SFC 由 `@vitejs/plugin-vue` 编译成渲染函数，
  运行时只需 `vue.runtime.*`——这正是加构建步骤换来的东西
- `assetsInlineLimit: 0` 关掉小文件转 data URI
- `modulePreload.polyfill: false`，那是段内联脚本

`base: './'` 也是必需的：i18n 用 `fetch('locales/...')` 是相对路径，
脚本若用绝对 `/app.js`，页面不在根路径时两者会失配。

#### dist 不入库，build.rs 兜底

产物入库会让每次改前端都在 diff 里混进压缩后的 JS。改为不入库，
由 `build.rs` 在产物缺失时自动跑一次前端构建——全新 clone 后
`cargo run -p omy-gui` 直接可用，不需要先记得手动 `pnpm build`。

`cargo:rerun-if-changed` 只盯 `src` / `public` / 三个配置文件，
不盯整个 `frontend/`，否则 node_modules 任何变动都会触发重建。

构建脚本里用了 `panic!`，与 crate 级别的 `clippy::panic = warn` 冲突。
没有改 lint 配置，而是在 build.rs 局部 `#![allow]` 并写明理由：
构建脚本不是运行期代码，cargo 的约定就是构建失败即 panic，
返回 Result 不会让构建停下来，只会静默产出坏掉的二进制。

#### GUI 改为文件管理器式交互：把密码从门槛变成工具

原来的界面一启动就是密码框，解锁后才显示文件。这个模型假设
**用户已经有加密文件了**——但新用户第一次打开应用时，手里
一个 `.omy` 文件都没有，却被要求输入一个不存在的密码。
唯一能做的事被一道没有钥匙的门挡住了。

实测确认了这一点：`启动时没有密码门槛` 现在是一条断言。

新模型把两件事分开：

| | 旧模型 | 新模型 |
|---|---|---|
| 启动后看到 | 密码框 | 文件管理器 |
| 没有密码时 | 什么都做不了 | 浏览、选文件、**加密** |
| 密码的作用 | 进入应用的钥匙 | 让加密文件显示真名 |
| 解锁粒度 | 全局开关 | 每个库（vault）独立 |

`store.js` 里 **删掉了 `unlocked` 全局布尔**。会话里有几个凭据
只影响「哪些加密文件能看见真名」，不影响应用能不能用。这个字段
存在时，界面很难不写成「解锁了显示 A，没解锁显示 B」的二分。

#### 加密完不该再问一次密码

`encrypt_paths` 成功后会调 `adopt_credential`，用**产出文件真实的**
vault_salt 把刚才用的密码装进会话。

不这么做的话：用户选中文件 → 输密码 → 加密成功 → 文件变成
「🔒 需要密码」→ 应用要求他输入三秒前刚打过的那个密码。

实测断言 `加密后密码自动进入会话` 与 `刚加密的文件立刻显示真名`
固定这个行为。

#### 同一目录必须共用一个 vault salt

`existing_vault` 会先扫目录里已有的加密文件，沿用它的 salt 与
KDF 参数；没有才新生成。

违反的后果很隐蔽：同一个密码在同一个目录里派生出两个不同的 KEK，
症状是「密码明明是对的，却只解开了一部分文件」。用户无从判断
是密码错了还是文件坏了。`second_file_reuses_vault_salt` 锁定这一点。

#### 曾经明确报错、现已实现的两个功能

| 功能 | 早期处理 | 现状 |
|---|---|---|
| 移到回收站 | 返回 `trash_not_supported` | 已接 `trash` crate，走各平台原生回收站 |
| 加密整个文件夹 | 返回 `folder_not_supported` | 已实现（容器打包） |

当初宁可报错也不静默降级，是因为「悄悄改成永久删除」等于数据丢失。
这条底线现在由 `trash_is_recoverable_not_permanent_delete` 接着守：
它不只验证原件消失，还要求能在系统回收站里找到同名条目。
把实现换成 `remove_file` 时功能测试照样通过，只有这条会失败——
已用变异测试确认它真的会失败，不是摆设。

#### GUI 接入设备发现、配对与共享

底层能力早就在 `omy-net` 里跑通了，但只有 CLI 接着。这一轮把它接进
界面，过程中撞出三个真实缺陷，都不是靠读代码发现的。

**设备库会话：不要每次操作都问密码**

CLI 每条命令都是独立进程，问一次密码做一件事，所以 `omy share pair`
问两次密码可以接受。GUI 是长驻的，用户会连续操作：看看有谁在线 →
配对 → 改名字 → 再配一台。沿用 CLI 的做法要输七八次同一个密码，
用户很快就会把它设成 `1`。**让安全措施难用，结果是用户绕过它**，
所以解开的 `Store` 留在会话里，密码用 `Zeroizing` 持有。

`lock()` 时**一并关闭设备库**。只锁一半会让锁定成为假象：
静态私钥还在内存里，攻击者可以冒充这台设备。

**Store::duplicate_for_serve：不 derive(Clone) 但要有明确的复制口**

服务端主循环需要独立持有一份身份与设备列表。让 `Store` 随手可 clone
等于邀请调用方到处复制私钥；但让调用方用 `from_parts` 手工拼一个，
又极易漏掉设备列表——症状是「配对明明成功了却连不上」，
日志只说 `Unauthorized`，排查时容易怀疑到握手上去。
`duplicate_for_serve_is_complete` 直接断言副本能做出与原件相同的
授权判断。

#### 三个只有实测才能发现的缺陷

**其一：改写 `APPDATA` 隔离不住测试环境**

端到端测试想用改写 `APPDATA` 的办法隔离，结果测试身份被写进了
开发者的真实配置目录——`dirs::config_dir()` 在 Windows 上走
`SHGetKnownFolderPath` 系统调用，**不看环境变量**。

修法不是给测试打补丁，而是承认这个路径本来就该可控：新增
`OMY_DEVICE_STORE`，同时服务于便携模式（设备库放 U 盘）和
多身份（同机不同身份连不同设备组）。验证脚本现在会主动检查
「用户真实配置目录未被触碰」。

决策逻辑抽成纯函数 `resolve_store_path`，因为 `omy-net` 是
`unsafe_code = "forbid"`，而 `set_var` 在 Rust 2024 里是 unsafe。
**没有为了测试去破坏那条禁令**——依赖进程全局状态的测试在并行
执行下本来也不可靠。

**其二：任何人连一下端口，配对就作废了**

原来的实现里，配对期间只 accept 一次。实测中我用一条 TCP 连接
验证「端口真的在监听」，那条连接被 accept 了，握手失败，整轮配对
就此结束——而屏幕上还显示着配对码，用户在另一台设备上怎么输都没用，
且没有任何提示。端口扫描器同样能触发。

改为单次握手失败只跳过这个连接，继续等下一个。结束条件只有三个：
成功、总时限到、用户取消。这不削弱安全性——PAKE 的抗猜测能力
本来就不依赖「只允许试一次」，总时限仍限制着尝试次数。

**其三：点了取消，却弹出「配对失败」**

后台任务收尾时无条件写入 `Failed`，覆盖了 `cancel()` 刚设的 `Idle`。
用户明明是自己点的取消，却被告知失败了。新增 `DeviceError::Cancelled`
让收尾逻辑能区分这两种结束方式。

顺带修了错误码混淆：握手失败原本报 `store_wrong_password`
（设备库密码错），用户会去改设备库密码，而真正该重试的是配对码。
现在分成 `pair_failed` 与 `connect_failed`——前者改配对码，后者改地址。
`pair_with` 失败时也**同时**返回 `Err`，不能只写进 task：
否则前端 `await` 正常返回，界面会先显示成功，要等下一次轮询才暴露。
#### CLI 只在网络命令内部起 runtime

CLI 整体是同步的。把 `main` 改成 `#[tokio::main]` 会让加解密命令
为一个用不到的运行时付出启动开销，也会把 async 传染到全部子命令。
所以 `share` 各命令在自己内部 `Builder::new_multi_thread()`。

`serve` 的连接事件回调要求 `'static`，而 `ctx.out` 是借用，不能直接
塞进去。用 mpsc 把事件送回命令函数打印：输出仍然走统一的 `Out`
（尊重 `--json` / `--quiet`），也不必给 `Out` 加 `Arc`。

#### connect 用指纹而不是地址定位设备

mDNS 广播的 TXT 记录里**只有指纹，没有完整公钥**。这是有意的：
任何人都能广播任意内容，如果从广播里取公钥，攻击者广播自己的公钥
配上别人的名字就能冒充。

所以 `share connect <指纹>` 拿指纹去**本地设备库**反查公钥，
广播只用来回答"这台设备现在在哪个 IP"。

#### 远端不给明文文件名，这不是缺陷

服务端零密钥，它自己也不知道文件叫什么（决策 D-30）。`Entry` 里
只有加密过的 header。所以 `connect` 列出的是 handle 短标识与大小，
取回后要用 `omy info` / `omy decrypt` 才能看到真名。

顺带解决了路径穿越：落盘文件名由**本地** handle 生成，不含任何
远端字符串。不是"过滤危险字符"，而是根本不用远端给的名字。
#### 主循环第一次面对敌意的并发环境

前面的模块都是"给定输入算出输出"的纯逻辑。主循环要处理的是别人
能主动做什么：

| 问题 | 不处理的后果 |
|---|---|
| 未授权连接 | 任何人都能读共享文件 |
| 握手慢连接 | 连上不发数据，耗尽连接槽 |
| 连接数无上限 | 打满内存与文件描述符 |
| 单连接异常 | 一个畸形客户端让所有人无法访问 |

"连上但永不发送握手消息"是**最廉价的拒绝服务方式**——不需要带宽也
不需要算力。所以握手有独立的短超时（10 秒），已建立的连接才用
60 秒空闲超时。

超出并发上限时**立即断开而不是排队**：排队会让攻击者用连接把正常
用户挤到队尾。

畸形请求回 `BAD_REQUEST` 而不是断开连接——可能只是版本差异，
让对方知道原因比静默断线更有用，有测试确认连接在之后仍可用。

#### 并发缺陷差点混过去，靠一条"同时开两条连接"的测试抓到

第一版在 accept 循环里 `await` 连接结束，服务实际是**串行的**：
一次只服务一个客户端，后面的要排队，一个慢客户端挡住所有人。

所有单连接测试都通过——它们从不同时开两条连接。

改成 spawn 后补了 `serves_multiple_clients_concurrently`，判据是
「两条连接**同时保持打开**时都能正常收发」，而不是「两条连接先后
都成功」——后者串行实现也能满足。

把实现改回串行做变异验证：**只有这条和并发上限那条失败，
其余 7 条全过**。
#### 存储文件里有私钥，所以整个文件必须加密

需要落盘两类东西，泄露后果完全不同：

| 内容 | 泄露后果 |
|---|---|
| 本机静态**私钥** | 攻击者可冒充本设备连接他人 |
| 已配对设备列表 | 暴露"这台机器和哪些设备配过对"的关系图 |

第一条决定了不能明文存。既然项目本身就是加密工具，直接复用
`omy_core` 的文件格式，不另造密钥管理，也让这份文件享受同样的
两级 KDF 与 AEAD 保护。有测试逐字节搜索磁盘内容，确认私钥与
设备名都不出现在文件里。

落盘走 `fsatomic::write_atomic`：写配对表的时刻正好是用户刚配对完的
时刻，此时崩溃若留下半个文件，用户会**静默失去全部已配对设备**。

每次保存换新 salt——这份文件会被反复重写，固定 salt 会让多个版本
共享同一 KEK。

#### 授权判断做成类型，而不是一个容易忘的步骤

`Server::handle` 不知道请求来自谁，它只管把 handle 变成字节。身份
核对是**另一件事**，很容易漏：**握手成功只证明对方持有某个私钥，
不证明那是已配对的设备**。

所以把「核对」做成 `Session::authorize` 构造函数、「服务」做成
`Session::handle` 方法——拿不到 `Session` 就调不到 handle，
"未核对就服务"在类型上无法表达。

`is_authorized` 同时检查「认识」与「未过期」：分两步判断容易漏掉
后者，有专门测试锁定「过期设备必须拒绝」。
#### 零密钥由类型系统保证，不是靠注释

DEC-16 要求「服务端绝不允许持有 KEK/FEK/明文」。写在文档里没用——
半年后有人为了"顺便显示个文件名"就会把 KEK 传进来。

因此服务端读数据的唯一入口是 `CiphertextSource` trait，它只有
`len()` 与 `read_at()` 两个方法，**没有任何途径拿到密钥或明文**。
想让服务端解密，必须先改 trait 定义——那是 code review 里看得见的改动。

#### 路径穿越：让它无法表达，而非拦截

客户端用 16 字节随机 `Handle` 指代文件，与磁盘路径无任何可推导关系。
客户端**根本没有表达路径的手段**，也就谈不上构造 `../`。
这比"收到路径再校验"可靠——后者依赖校验没有疏漏，前者从表达能力上
就排除了整类问题。handle 必须随机而非顺序：顺序会泄露文件数量并可被枚举。
## 验证

### 独立验证通道（提交 `217df52`）

不采信与实现同源的自测，另开四条独立通道：

| 工具 | 手段 | 结果 |
|---|---|---|
| `crash_writer` + `scripts/verify-atomic-write.ps1` | 真实 `process::abort()` 杀进程 | 11/11。崩溃不产生半成品；覆盖写崩溃时原文件完好 |
| `bench_scan` | 生产参数实测 | Argon2id 91.2 ms、HKDF 0.30 µs；300 文件精确解锁 100 个，75 个噪音零误判 |
| `fuzz_parse` | 8 策略 × 20,000 次随机变异 | 零 panic |
| `audit_mac_scope` | 逐字节翻转 | 头部 656 B 全被 MAC 拒绝、载荷 3016 B 全被 AEAD 拒绝，均零漏网 |

### CLI 端到端（本轮，`scripts/verify-cli.ps1`）

用**真实编译出的二进制**走完整流程，79 项断言全通过。单元测试无法验证"命令行参数解析 + 进程退出码 + stdout/stderr 分工"，这些只能靠真实调用二进制。

覆盖：拒绝明文密码参数、加解密逐字节往返、退出码契约（2/3/4/6）、JSON 错误格式、cat 管道与范围读取、相同内容不同密码文件大小一致、list/scan、分片切分/缺片检测/合并哈希一致、目录容器嵌套与空目录还原、doctor/bench/completion、帮助与版本。

**这一轮端到端验证发现了 3 个单元测试没发现的真实缺陷**（见下节）。

### 局域网真实 TCP 验证（16 项，`examples/verify_lan.rs`）

`serve.rs` 的单元测试直接调用 `Server::handle`，走同一进程同一份内存，
验证不到：编码 → TCP → 解码后是否仍正确、分帧是否正确（TCP 是字节流，
不保证一次 read 拿到整条消息）、大文件跨多次 READ 拼接后能否**真正解密还原**。

最后一条最关键：**前面所有测试都只验证"字节搬运正确"，没有一条验证过
"搬过去的字节真的能解密成原文"**——那才是用户实际关心的事。

实测：500 KB 文件经 9 次 READ 拼回，与磁盘逐字节相同；用正确密码打开后
文件名解密为 `big.omy`、内容与原文逐字节相同；错误密码必定失败（反证）。
错误路径三项（未知 handle / 超限 / 越界）均返回预期错误码。
### 局域网端到端验证（39 项，`examples/verify_e2e.rs`）

在 `verify_lan` 的明文 TCP 之上加了完整安全层：PIN 配对 → 交换静态
公钥 → Noise IK 握手 → 加密信道上取密文 → 真正解密还原。

实测链路：生成 6 位 PIN → 双方配对并交换设备名与公钥 → 指纹一致 →
Noise IK 1-RTT 握手 → LIST → 5 次 READ 取回 300688 B → 用正确密码
解出中文文件名 `机密-季度报告-2026.docx.omy` 与逐字节相同的原文。

三条反证：错误密码打不开、未配对设备连不上、用错误公钥握手必定失败。

第 6 组验证持久化的真实价值——**模拟重启**：把身份与已配对设备存盘，
再从磁盘重新加载，确认公钥/私钥/设备名都不变，并用重新加载的身份真的
与原已配对设备建立了一条 Noise 信道、完成了服务端身份核对。

配套四条反证：吊销后立即拒绝、过期授权必须拒绝、静态私钥不明文落盘、
错误密码打不开存储文件。

第 7 组走 `serve()` **真实入口**：前几组都是手写的 accept 循环，
测不到真实入口的超时、并发限制、授权检查是否接对了线。这一组确认
真实端口上报正确、握手到解密还原的完整链路通畅、陌生设备读不到
文件列表，以及访问日志同时记下了已服务与未授权两类连接。

### GUI 前端验证（19 项）

分两层，因为「构建通过」和「界面能用」是两回事。

**产物静态检查**（8 项，`spikes/verify-gui-build.ps1`）：
产物里不能出现 `eval` / `new Function`（CSP 会拦）、不能打进完整版
Vue（含模板编译器）、index.html 不能有内联脚本、locales 要随产物复制。

**运行时实测**（11 项，`spikes/verify-gui-runtime.ps1` +
`probe-gui-vue.mjs`）：启动真实 GUI 进程，通过 CDP 连上 WebView，
确认 Vue 已挂载、文案真的翻译成了中文而不是键名、v-model 双向绑定
生效、控制台无错误、无 CSP 违规。

第二层抓到过第一层抓不到的东西——CSP 拦截和 IPC 不可用只在运行时暴露。

### GUI 文件管理器实测（26 项）

新交互模型的两条主链路都用真实进程 + CDP 验证，不采信「界面渲染
出来了」这种表面证据。

**交互模型**（15 项，`spikes/verify-gui-filemanager.ps1` +
`probe-gui-fm.mjs`）：造一个含普通文件、加密文件、子目录的真实目录，
确认启动无密码门槛、侧栏位置已翻译、能识别加密文件、加密文件默认
锁定。关键反证：**锁定文件不返回真实名与 entry_id**——不是"界面
上不显示"，而是数据根本没出后端。

**加密与解锁**（11 项，`spikes/verify-gui-crypto.ps1` +
`probe-gui-crypto.mjs`）：全程通过真实 Tauri 命令走完
加密 → 自动可见 → 锁定 → 错密码 → 对密码 五个阶段。

三条反证是这组的重点：

| 反证 | 若缺失会漏掉什么 |
|---|---|
| 锁定后加密文件全部回到锁定态 | 锁定只清了界面没清密钥 |
| 错误密码解不开任何文件 | 派生成功就当解锁成功 |
| 普通文件不被误判为加密文件 | magic 判断过于宽松 |

`probe_one` 的两条断言覆盖「双击已解锁文件不该再问密码」这条
交互路径——它在界面上表现为"没有弹窗"，很容易被当成没生效。

### GUI 设备与共享实测（35 项）

`spikes/verify-gui-devices.ps1` + `probe-gui-devices.mjs`。
设备库路径用 `OMY_DEVICE_STORE` 隔离，并在结束时验证
**用户真实配置目录未被触碰**——这条断言本身就来自一次真实事故。

不满足于「命令返回成功」，端口类断言一律**真的建一条 TCP 连接**：

| 断言 | 若只看返回值会漏掉 |
|---|---|
| 配对端口真的在监听 | 后端返回一个假端口号也能"通过" |
| 共享端口真的在监听 | 服务报告启动成功但根本没绑定 |
| 取消后端口已释放 | 配对码作废了但端口还占着 |
| 停止后端口已释放 | 服务停了但 socket 泄漏 |

四条关键反证：

- **陌生连接不会让配对作废**——直接守护上面那个真实缺陷
- **锁定后设备库也关了**，且不再泄露本机身份
- **未打开时不泄露身份**（`device_name` / `fingerprint` 必须为 null）
- **设备名未明文落盘**（直接读文件字节找中文设备名）

还有一条容易被忽略但很关键：**重复打开保持同一身份**。
每次开库都换身份的话，已配对的设备会全部失效，而现象是
「昨天还能连，今天连不上了」。
### CLI 端到端实测（19 项，`spikes/verify-cli-share.ps1`）

把 `omy.exe` 当成两台设备真的跑起来：A 生成配对码、B 用
`--pin-file` 完成配对、A `share serve`、B `share connect --fetch`
取回密文、再用 `omy decrypt` 还原，最后比对 SHA-256。

配套两条反证：未配对设备连接必须失败、吊销后设备库里不再有记录。

这一层抓到了单测抓不到的东西：第一版脚本用 `--kdf` 而正确的参数名
是 `--kdf-profile`，跑一次才发现。也暴露了 `pair` 缺少非交互配对码
通道——自动化和开机自启都需要，于是补了 `--pin-file`。

#### 验证程序自己挂死过一次

第一版没加超时，第 5 步的连接尝试永久阻塞：服务端 `accept` 只调用
一次，此时正在 loop 里处理已有连接，没有空闲 accept 在等新连接——
TCP 连接进入 backlog，客户端发出握手后永远收不到应答。

程序静默挂住，终端无输出，只能靠查进程才发现。这是**测试设计**问题
而非实现缺陷，但暴露了一个真实约束：**产品代码里所有网络等待都必须
有超时**，否则对端不响应就会永久卡住 UI。

现已加三层保障：单个连接尝试 2 秒超时、服务端汇合 5 秒超时、
整个程序 120 秒看门狗（超时打印明确提示并以退出码 2 结束）。
验证程序绝不该让人盯着不动的终端猜是「还在跑」还是「挂了」。

#### 分帧测试必须能真的抓到分帧缺陷

`framing_survives_byte_at_a_time_reads` 用一个每次只返回 1 字节的
流包装器，逼出 `read_exact` 的必要性。

验证过它确实有效：把 `read_exact` 临时改成 `read` 后，**只有这一条
失败，其余 10 条全过**——包括发送 65519 字节大消息的那条，因为
loopback 上一次就读全了。若没有这条测试，分帧缺陷会一直潜伏到
真实网络上才随机暴露。
### 本机实测性能（release，2026-08-29）

| 项 | 数值 |
|---|---|
| Argon2id mobile（32 MiB, t=4） | 46.9 ms |
| Argon2id interactive（64 MiB, t=3） | 75.4 ms |
| HKDF（每文件每 slot） | 0.288 µs |
| 比值 | 约 262,000× |
| ChaCha20-Poly1305 | 加密 1.35 GB/s、解密 1.31 GB/s |
| AES-256-GCM | 加密 1.55 GB/s、解密 1.57 GB/s（本机有 AES-NI） |

⚠️ **debug 构建的数字毫无参考价值**：实测 AEAD 吞吐差约 130 倍（0.01 vs 1.35 GB/s）、HKDF 差约 23 倍。`bench` 命令已在 debug 下主动警告。

### 复现命令

```bash
cargo test --workspace                            # 299 项
cargo clippy --workspace --all-targets -- -D warnings   # 零告警
cargo build -p omy-cli && cargo build --release -p omy-cli
pwsh -File scripts/verify-cli.ps1                 # 79 项端到端
pwsh -File scripts/verify-media-tlv.ps1           # 46 项媒体 TLV 端到端

# GUI 端到端（需 Node 跑 CDP 驱动脚本；会自动启动并关闭 GUI）
pwsh -File spikes/make-gui-vault.ps1              # 造测试库（4 个文件同库 + 1 个异库）
pwsh -File scripts/verify-gui.ps1                 # 43 项 GUI 端到端
pwsh -File spikes/verify-gui-filemanager.ps1      # 15 项 文件管理器交互
pwsh -File spikes/verify-gui-crypto.ps1           # 11 项 加密/解锁链路
pwsh -File spikes/verify-gui-devices.ps1          # 35 项 设备/配对/共享（含隔离验证）
./target/release/omy bench                        # 本机性能
cargo run --release --example bench_scan -- 300
cargo run --release --example fuzz_parse -- 20000
cargo run --release --example audit_mac_scope
cargo build --example crash_writer && pwsh -File scripts/verify-atomic-write.ps1

# omy-media：先生成素材，再跑真实文件验证
pwsh -File spikes/make-media-fixtures.ps1
pwsh -File spikes/make-seek-fixtures.ps1          # P2 需要 60s/60 关键帧素材
cargo run --release --example verify_media -p omy-media    # 63 项
cargo run --release --example verify_remux -p omy-media    # 42 项 P2 转封装

# omy-net：协议栈 spike + 真实 TCP 全链路
cargo run --release -p omy-spike-lan                       # 10 项协议栈 spike
cargo run --release --example verify_lan -p omy-net        # 16 项真实 TCP
cargo run --release --example verify_e2e -p omy-net        # 18 项端到端（配对+Noise）
cargo run --release --example verify_keyless_serve -p omy-core  # 5 项零密钥反证

# P2 的两轮 spike（实现前的可行性验证，产物保留供人工核对）
pwsh -File spikes/spike-remux.ps1
pwsh -File spikes/spike-mkv-seek.ps1
```

> ⚠️ 跑端到端脚本前**必须先重新构建**。两个脚本已内置陈旧产物检测
> （源码比二进制新就拒绝运行），但仍建议养成先 build 的习惯——
> 缺陷 #11 就是拿旧二进制跑出的假失败。

### omy-media 真实文件验证（63 项）

单元测试用的是**手写的 ffprobe JSON 样本**，只能验证"给定这段 JSON 能否正确解析"，
无法验证"真实 ffprobe 的输出是否真的长这样"。两者同源时，对格式的误解会同时存在于
样本和实现里——这正是缺陷 #6 漏过 76 项端到端验证的原因。

因此 `examples/verify_media.rs` 用**真实媒体文件 + 真实 ffprobe** 独立验证，
每个断言的期望值按文档 §5.2 人工判定，而非"实现算出什么就认什么"。

素材（`spikes/make-media-fixtures.ps1` 生成 10 个）刻意覆盖易错组合：

| 素材 | 验证点 |
|---|---|
| `h264_aac.mp4` | P1 基线；且**moov 在 99% 处**，是缺陷 #7/#8 的触发源 |
| `h264_aac.mkv` | 容器不被 `<video>` 支持但可 remux → P2 |
| `vp9_opus.webm` | 真 WebM 可直通 → P1 |
| `mpeg4_mp3.avi` | MPEG-4 ASP 两端都不支持 → P3 |
| `h264_flac.mkv` | FLAC 原生可解但进不了 MP4 |
| `with_cover.mp3` | 带封面 → 必须**不**被判成视频（`attached_pic`） |
| `not_media.txt` | 必须返回可降级的 `NOT_MEDIA` 而非崩溃 |
| `h264_aac_faststart.mp4` | 与尾部 moov 版本对照，锁定重排正确性 |

关键证据：尾部 moov 素材经纯 Rust 重排后抽帧得到 **4404 字节**，
与直接用文件路径抽帧的结果**完全一致**——这是偏移修正正确的最强证据。

### 媒体 TLV 端到端（46 项，`scripts/verify-media-tlv.ps1`）

`examples/verify_media.rs` 是在**库内部**调用 API，验证不到 CLI 参数解析、
TLV 落盘、解密还原这条完整链路。本脚本用**真实编译出的二进制**走
`encrypt → info → decrypt`，覆盖：

| 分组 | 关键断言 |
|---|---|
| MP4 三 TLV | `has_thumbnail` / `has_moov_cache` / `media` 段齐全，判为 P1、时长 8000 ms、h264 320 宽 |
| **可还原性** | 加了媒体 TLV 后解密结果与原文件 **SHA-256 相同** |
| MKV | 判为 P2，且**不应**有 moov 缓存 |
| 纯音频 | 有 meta 无 moov、无视频轨，且**不刷缩略图失败警告** |
| 非媒体 | 无 media 段、无缩略图、**不产生警告噪音** |
| 三个开关 | `--thumbnail none` / `--no-media-meta` / `--no-moov-cache` **互不影响** |
| 取帧写法 | `5` / `5.5` / `00:05` / `00:00:05` 均可用；`abc` 必须**非零退出且不产出文件** |
| 冲突组合 | `--thumbnail none` + `--thumbnail-frame` 必须报错而非静默忽略 |
| **不泄露** | 未解锁时 `media` 为 null、`has_moov_cache` 为 **null 而非 false** |

最后一条是刻意设计的：「不知道」与「没有」是两件事，脚本要能区分。
而 `has_thumbnail` 来自 header flag，本就无需密码即可读。

#### moov 缓存存的是**原始**字节，不是重排后的

这是最容易搞错的一点，`prepare::extract_moov` 的注释里也写明了：

- 缓存 moov 是为了让播放器起播时不必 seek 到文件尾部；
- 但 moov 里的 `stco` 偏移是相对**原始布局**的，播放时要与原始载荷配合才正确；
- 若存了重排后的 moov，偏移就与实际载荷不符，会解析出错位的样本。

验证时用「重排后的 moov 与缓存内容**不同**」作反证锁定这个区别。

#### MediaMeta 为什么不直接序列化 `MediaInfo`

三个理由，都不是风格问题：

1. **体积**：完整探测结果远大于播放决策所需，而它要进每个文件的头部；
2. **稳定性**：`MediaInfo` 随 ffprobe 版本漂移，旧文件的 meta 会解析不了；
3. **隐私**：ffprobe 的 `tags` 里可能含拍摄设备、剪辑软件甚至 GPS 坐标——
   把它原样写进加密文件的元信息区，等于把用户以为已加密的隐私换个地方存。

`MediaMeta` 因此是独立定义的稳定结构，字段全部 `serde(default)` +
`skip_serializing_if`，既兼容旧版也压体积。实测 MKV 素材的 meta 仅 **399 字节**。

### P2 转封装验证（42 项，`examples/verify_remux.rs`）

P2 是「容器不支持但编码支持」的播放路径：只换容器不重编码，
耗时数十毫秒，而全解码转码是数百毫秒到秒级。

实现前先做了两轮 spike，因为这条路有两个命门必须先证伪：

| Spike | 问题 | 实测结论 |
|---|---|---|
| `spike-remux.ps1` | 管道进管道出能否产 fMP4 | ✅ 可以，`-c copy` 无重编码，353K→354K |
| | 输入 seek 在管道下可用吗 | ❌ **失效**，`Seek to desired resync point failed` |
| | init segment 能否单独产出 | ✅ `-frames 0` 产出 1253 字节的 `ftyp`+`moov` |
| `spike-mkv-seek.ps1` | 裸 Cluster 能否 demux | ❌ `Invalid data found`（缺 Tracks） |
| | 头部+中间 Cluster 拼接呢 | ✅ **成功**，754 B 头部 + 第 30 个 Cluster → 可解码 |

第二条决定了架构：**不能靠 FFmpeg 自己 seek**，必须由调用方先算出
目标时间对应的字节区间，只喂那一段。这正好复用与 P1 完全相同的
「解密某个字节区间」逻辑，与文档 §6.2 的设想一致。

验证覆盖（真实 60 秒 MKV，60 个关键帧 / 60 个 Cluster）：

| 分组 | 关键断言 |
|---|---|
| EBML 解析 | 60 个 Cluster、Tracks 识别、时长 60023 ms、头部 754 B |
| **交叉验证** | 正规 EBML 解析与裸字节扫描的 Cluster 偏移**逐个一致** |
| 时间定位 | 30.5s → 第 30 个（**向前取整**，不能往后跳） |
| 裸 Cluster | **必须失败**——这是"头部不可省"的反证 |
| 拼接 remux | 产物含 `moof`、2 条流、时长 2.02s、真解码通过 |
| 多点 seek | 0/10/25/45/59 秒均产出可播放片段（63–133 KB） |
| init segment | 1253 B、含 `ftyp`+`moov`、**不含 `moof`**（符合定义） |
| 轨道映射 | 只映射 `0:v:0` 时产物只有 1 条流 |
| 截断文件 | 解析出 31 个 Cluster（< 60），可用部分仍能播放 |

#### ⚠️ 不要用管道探测去核对 fMP4 的时长

这个坑让我误判了一次。`empty_moov` 让 `moov` 不含总时长，时长分散在
各个 `moof` 里，ffprobe 走管道读不到末尾，只能报**第一个 fragment** 的时长。

实测对照（`spikes/dbg-fmp4-duration.ps1`）：

| 探测方式 | 报告时长 |
|---|---|
| fMP4 + 文件路径（可 seek） | 60.02s ✅ |
| fMP4 + 管道（不可 seek） | **2.04s** ❌ |
| 非分片 MP4 + 管道 | 60.00s ✅ |
| 真解码全片 | 60.02s ✅ |

产物其实完全正确——60 个 `moof`、真解码满 60 秒、尺寸 100.1%。
是断言用错了探测方式。核对完整性应当**数 `moof` 数量**，或用文件路径探测。


### GUI 端到端（43 项，`scripts/verify-gui.ps1`）

GUI 是唯一无法靠 `cargo test` 验证的部分：协议注册、WebView 的 Range 行为、
视频能否真的播放与 seek、锁定后密钥是否真的抹掉——这些都只在**真实 WebView
进程**里才成立。因此用 CDP（Chrome DevTools Protocol）驱动真实 GUI，
全自动无需人工点击。

**这一轮 GUI 验证抓出 4 个缺陷（#13–#16），其中 #13 会让 Windows 上
所有内容加载全部失败，而编译、330 项单测、clippy 全部通过。**

覆盖：

| 阶段 | 关键断言 |
|---|---|
| 启动 | 未解锁时**不渲染主界面**，DOM 里不含任何文件信息 |
| 解锁 | 读取多库参数、密码正确时 100ms 内进入主界面 |
| 列表 | 5 个文件扫到 5 个，4 个解开、1 个保持锁定（异库） |
| **锁定态不泄露** | 锁定文件的原名、磁盘名、大小、缩略图**一律不出现** |
| 协议 | 200 / 206 / 416 / suffix Range / 2 MiB 上限 / `no-store` / MIME / 404 |
| **播放** | 双击加载元数据；5 次乱序 seek 全成功，落点偏差 **0.00s**，耗时 1–3 ms |
| **画面真实性** | canvas 在多个时间点取到帧，最少 195 种颜色，且各帧互不相同 |
| 预览 | 图片尺寸正确；文本命中哨兵字符串；多字节字符正确解码 |
| 多语言 | zh-CN ↔ en 切换后文案确实改变 |
| 锁定 | 回到解锁界面、无卡片残留、协议立即返回 404 |
| 网络观察 | `fromDiskCache` 全程为 0 |

「画面真实性」是刻意加的：只断言 `seeked` 事件会放过「seek 报成功但画面
卡在同一帧」。取帧比对颜色分布才能排除这种假通过。

#### 验证脚本自身也会有断言错误

「控制台无错误」最初把测试**自己触发**的 416（越界 Range 用例）和 404
（未知 id、锁定后访问用例）当成缺陷报出来——那恰恰是断言通过的证据。
已改为只放行这两个特定状态码，其余错误一律照报；整条规则放宽会让真问题溜过去。

## 本轮修复的真实缺陷

端到端验证与 clippy 严格门禁各暴露出必须修的问题：

| # | 缺陷 | 根因 | 修复 |
|---|---|---|---|
| 1 | `scan` 解锁数恒为 0 | 硬编码 `Argon2Params::INTERACTIVE`，但文件可能用任意档位加密，参数不符必然派生出错误 KEK | 改为**成对**取自同一文件头的 `(vault_salt, argon2_params)`；label 附带参数指纹避免同 salt 不同参数时缓存串用 |
| 2 | 缺片退出码退化为 1 | `report_error` 只 `downcast_ref` 顶层，经 `.context()` 包装后取不到 | 改为遍历整个错误链；补测试锁定多层包装场景 |
| 3 | 缺片时 `shard check` 报格式错误而非缺片 | `load_parts` 遇缺失序号立刻 `break`，只读到缺口之前的片，`shard_total` 与片数矛盾 | 改为容忍连续 64 个缺号；`decrypt` 里同一处一并修正 |
| 4 | 盘符校验漏判且逻辑不对 | `c.as_bytes()[1] == b':'` 只看第 2 字节，多字节 UTF-8 下冒号不在下标 1 就漏过 | 改用字符迭代判断；并**拒绝任意位置的冒号**（Windows 上 `a.txt:hidden` 会创建 NTFS 备用数据流） |
| 5 | 超长多字节字符串编码会 panic | `&root[..root_len]` 在 65535 处按字节切，切断多字节字符即 panic（中文每字符 3 字节，必然命中） | 新增 `truncate_utf8` 按字符边界回退；root / 路径组件 / target / xattr key 全部改用；补测试实际触发验证 |
| 6 | `cat --range` 每个区间少一个字节 | `parse_range` 返回 `(start, end)` 却未定义开闭区间，调用处按开区间处理，用户按 HTTP Range 惯例期望闭区间。**76 项端到端验证没抓到**，因为断言写成"`0-100` 输出 100 字节"，恰好用开区间的期望值匹配了开区间的实现——测试与实现同错、互相掩护 | `parse_range` 改为返回 `(offset, length)` 让调用方不可能弄错；同时修正 `-N` 语义（原为"前 N 字节"，与 HTTP `bytes=-N` 的"最后 N 字节"相反）；污染的断言改为 `0-99 → 100 字节`，补单字节/末字节/suffix 三类边界 |
| 7 | 尾部 moov 的 MP4 探测与抽帧全部失败 | 管道输入无法 seek，FFmpeg 读到 mdat 就报 `partial file` + `Cannot determine format after EOF`，stdout 为空。让 FFmpeg 自己 `-movflags frag_keyframe+empty_moov` remux 也只产出 1301 字节空壳（它同样读不到 moov）。而录屏、相机直出、`-c copy` 输出**默认都是尾部 moov** | 新增 `mp4::to_faststart`：在主进程内用**纯 Rust** 把 moov 前移并修正 `stco`/`co64` 偏移，再喂管道。既解决问题又不违反文档 §14「FFmpeg 子进程无文件系统访问」 |
| 8 | 同一个 stco 被登记 16 次，偏移累加 16 遍 | `collect_offset_tables` 递归进容器时**没有收窄搜索上界**，子调用一直扫到 moov 末尾，把容器之外的 stco 又扫一遍。嵌套 trak/mdia/minf/stbl 四层就重复多次。实测首项从应有的 4611 变成 63930，FFmpeg 报 `Invalid NAL unit size (1593407596 > 5369)`。**单元测试没抓到，因为手写样本只有一层嵌套、单条轨道** | 递归时传入 `end` 上界并收窄到当前容器末尾；补三条回归测试（不得重复登记、双轨恰好 2 个表、双轨端到端逐项核对偏移只加一次） |
| 9 | 误把 FFmpeg 的 stderr 当成有效产物 | 诊断脚本用 `> out 2>&1` 把 stdout 与 stderr 混进同一文件，又只检查"长度 > 100"，于是 844 字节的错误文本被判定为成功抽帧，据此得出「尾部 moov 也能抽帧」的错误结论，并按错误结论改了实现 | 校验产物必须看**内容特征**：`image_decodable` 先验 WebP/JPEG/PNG 魔数再真实解码；诊断时 stdout 与 stderr 必须分开重定向 |
| 10 | `cat --range -256` 在真实 CLI 下直接失败 | clap 默认把以 `-` 开头的值当短选项，`--range -256` 报 `unexpected argument '-2' found`。`parse_range` 的单测全部通过——因为它测的是**解析函数**，而 clap 在把参数交给它**之前**就拒绝了。修缺陷 #6 时我加了这条端到端断言却没验证它能通过，等于加了个从未真正跑绿的断言 | 给 `range` 加 `allow_hyphen_values = true`；补 `clap_accepts_suffix_range` 测试直接验证**解析层**（含空格式、等号式，并固定「跟着的选项会被吃成值但 `parse_range` 必报错」这一副作用行为） |
| 11 | 验证脚本用陈旧二进制跑出假失败 | `verify-cli.ps1` 硬编码 `target\debug\omy.exe`，而修复后只重建了 release，脚本拿着 19 分钟前的旧 debug 跑，报出一条已经修好的失败。我据此以为新代码有缺陷，追查两轮才发现是产物陈旧 | 两个验证脚本都改为：取 debug/release 中**较新**者，并在**源码比二进制新**时直接拒绝运行并提示重新构建 |
| 12 | 8 字节 VINT 解析会 panic | EBML 的大小字段要去掉标记位，实现写成 `0xFFu8 >> len`。`len == 8` 时（首字节 `0x01`，标记位占满整字节）触发**移位溢出 panic**——Rust 要求移位量小于位宽。而 8 字节 VINT 在真实 MKV 里很常见，muxer 常用最大宽度占位 | 改用 `checked_shr().unwrap_or(0)` 表达"移满即为 0"；补 `vint_encoding_roundtrip` 与 `unknown_size_does_not_hang` 覆盖 8 字节与未知长度两种边界 |
| 13 | Windows 上所有 `omystream://` 请求全部失败 | WebView2 **不支持自定义 scheme**，Tauri 在 Windows/Android 上把它映射成 `http://<scheme>.localhost/`；macOS/Linux 才用原生形式。前端写死 `omystream://localhost/` 导致 `fetch` 报 `URL scheme not supported`、`<video>` 报静默的 `ERR_UNKNOWN_URL_SCHEME`。**编译、单测、clippy 全部通过**——这类平台差异只有真机跑 GUI 才暴露 | 新增 `stream_base` 命令由后端按编译目标下发前缀（后端本就知道自己编到哪个目标，比前端嗅探 UA 可靠）；CSP 与 CDP 脚本同步改为从应用取前缀而非硬编码 |
| 14 | 目录含多个 vault 时只解开其中一个 | `vault_params_of` 取第一个能解析的文件的 salt 就返回。但一个文件夹里混着多个库是常态（分批加密、从别处拷入）。实测 5 个文件只解开 1 个，而用户密码明明是对的 | 改为收集**全部去重后**的 `(salt, 参数)` 逐个派生；`UnlockResult` 增加 `vaults_unlocked` 让前端能区分「全解开」与「部分解开」。有 N 个库就跑 N 次 Argon2，这是正确性的必需开销 |
| 15 | canvas 取帧报 `SecurityError: canvas has been tainted` | 页面在 `tauri.localhost`、协议在 `omystream.localhost`，两者不同源。服务端虽已发 `Access-Control-Allow-Origin`，但 `<video>`/`<img>` 未声明 `crossorigin` 时浏览器**根本不走 CORS 校验**，直接判为跨源污染。影响的不只是测试——应用内截图、缩略图生成都要读像素 | `<video>`/`<audio>`/`<img>` 全部补 `crossorigin="anonymous"`。S1 spike 的页面本来就有这个属性，实现时漏抄了 |
| 16 | 图片被标成「🐌 需重新编码」 | 播放分级对图片没有意义，但 `scan` 无条件取 `playback_tier`。PNG 走 ffprobe 会被识别成「单帧视频」从而落到 P3，界面上就成了一张 PNG 挂着重编码警告 | 仅当 kind 为 video/audio 时才写入 tier 与 duration |

## 文档纠错（实测推翻原描述）

设计文档是实现前写的，有些描述被实测证伪。已在原文标注修正，此处汇总：

| 文档 | 原描述 | 实测结论 |
|---|---|---|
| `04-media-playback.md` §5.3 | 「FLAC/Opus 虽然新版规范支持但很多播放器不认」 | ❌ **已过时**。FFmpeg 可将 FLAC `-c copy` 直接 remux 进 MP4（tag `fLaC`），音轨完整、可解码出 705678 字节 PCM；MDN 明确 FLAC 容器支持含 MP4，浏览器覆盖 Chrome/Edge/Firefox/Safari。`tier.rs` 的 `MP4_INCOMPATIBLE_AUDIO` 按实际能力编写，**不含** flac |

发现方式：spike 里 FLAC 素材 remux "成功"了，与文档矛盾。没有默认文档
正确，而是追查产物的真实流构成 + 真解码 + 查证 MDN，三者一致才下结论。

## 待办

### core 缺口

- [x] `container` 目录容器索引
- [x] `BlockSource` trait
- [x] `tree` 模式：目录名加密 + base32 编码（文档 05 §3）
- [x] slot 原地增删改：保留 FEK、重建 slot 区、重算头部 MAC，**不重写载荷**（`keyslot`，`key add/remove/change` 已接）
- [x] 密钥轮换：换 FEK 并重新加密载荷（`reencrypt`，`key reencrypt` 已接）
- [x] 元数据**保存**：`pack.rs` 采集 mtime/btime，unix 上另有 mode/uid/gid
- [x] 元数据**还原**与还原报告（D-20 / N3）——`restore` 模块，解包后按平台能力还原并如实报告未能还原的项
- [ ] 伪装模式：`footer` / `host-jpeg` / `host-png`（D-17）——格式层已占号（`flags::DISGUISED`、`TLV 0x000F`），零实现。动手前先看「下一批四项能力的现状核查」里的两处热路径
- [x] 恢复码：SLIP-39 词表 + 26 词编解码 + 可定位校验和（`recovery` 模块，`key recovery` / `key restore` 已接；11 项单测 + 20 项端到端 + 6 项变异）
- [x] **`rewrite_slots` 原样搬运未识别的槽**（`OtherSlots::Carry` / `Discard`；18 项单测 + 15 项端到端 + 5 项变异）
- [x] 槽位可管理模式：`flags` bit 10 + `TLV_SLOT_DIRECTORY`（0x0010），加密时二选一（DEC-17）——`slotdir` 模块 + `rewrite_slots_managed`；CLI 的 `--slot-mode` 与 `key list --password-*`，GUI 的模式选项与槽位清单。8 项单测 + 7 项集成 + 21 项 CLI 端到端 + 11 项 GUI 端到端 + 6 项变异
- [x] 符号链接跨平台还原——容器模式存进索引（`target` 字段）并在解密时按平台能力还原；权限不足归「平台不支持」而非失败，指向容器外的目标一律拒绝。树形模式无处存目标，仍如实报为跳过

### omy-cli

- [x] 12 个子命令
- [x] 密码五通道 + 拒绝 `--password`
- [x] `--json` 输出；`code` 恒为英文常量
- [x] i18n 简中 + 英文
- [x] 配置文件
- [x] `--vault`：加入已有库，复用 salt 与 KDF 参数
- [x] `share serve / discover / pair / connect / devices`（19 项端到端通过）
- [x] 进度条（`indicatif`，仅在 stderr 是终端且非 `--json` / `-q` 时显示）

### omy-media

- [x] `ffprobe`：探测 ffprobe/ffmpeg 可用性、管道运行、超时强杀
- [x] `probe`：容器/流解析，容忍字符串-数字-缺失-`N/A` 四态；排除 `attached_pic`
- [x] `mp4`：顶层 box 解析、moov 定位、**faststart 重排（含 stco/co64 偏移修正）**
- [x] `tier`：P1/P2/P3 播放分级，区分真 WebM 与 MKV
- [x] `thumbnail`：图片走纯 Rust `image`，视频走 FFmpeg 抽帧
- [x] `meta`：`MediaMeta` 独立序列化结构（**不直接序列化 `MediaInfo`**，见下）
- [x] `prepare`：一站式入口，一次探测同时产出三个 TLV 的负载
- [x] 与 core 打通：写入 `TLV_MEDIA_META` / `TLV_MOOV_CACHE` / `TLV_THUMBNAIL`
- [x] `mkv`：EBML 解析、Cluster 索引、时间→字节定位、头部拼接
- [x] `remux`：**P2 转封装**，fMP4 产出、init segment、轨道映射
- [ ] 转码选项（加密时可选转 web 原生格式，D-26）
- [ ] 字幕轨提取（首期只做文本类，ASS/PGS 留接口）

### 后续

- [x] `omy-net` 线路协议编解码（防御性解析，拒绝超长/截断/尾部垃圾）
- [x] `omy-net` SPAKE2 配对 + 密钥确认
- [x] `omy-net` 零密钥服务端（LIST/STAT/READ）
- [x] `omy-net` mDNS 设备发现（广播/浏览/指纹/去重）
- [x] `omy-net` Noise IK 信道（握手、分帧、连接复用）
- [x] `omy-net` 配对与信道接线（加密交换静态公钥）
- [x] `omy-net` 已配对设备的加密持久化（原子写 + 权限收紧）
- [x] `omy-net` 会话有效期与吊销（`Session` 授权入口）
- [x] `omy-net` 服务端主循环（accept + 多连接并发 + 超时 + 访问日志）
- [x] `omy-cli` `share` 命令组（serve / discover / pair / connect / devices）
- [x] GUI 的设备发现与配对界面
- [ ] `omy-net` 真实双机 mDNS 实测（当前只在单机验证协议逻辑）
- [x] `omy-gui`：Tauri v2（解锁/扫描/预览/播放/多语言/锁定）
- [x] `omy-gui`：文件管理器式交互（浏览/选中加密/双击解锁/统一密码）
- [x] `omy-gui`：设备库、局域网发现、配对、共享服务
- [x] `omy-gui`：连接远端设备浏览其共享文件（流式随机读，明文不落盘）
- [x] `omy-gui`：文件夹加密（容器打包，与 CLI 共用 `omy_core::pack`）
- [x] `omy-gui`：浏览加密文件夹（复用主列表渲染，与进普通文件夹一致）
- [x] `omy-gui`：预览容器内的单个文件（`/citem/` 协议，按需只解该文件区间）
- [x] `omy-gui`：还原文件到磁盘（可选当前目录或自选目录，解包逻辑与 CLI 共用 `omy_core::unpack`）
- [x] `omy-gui`：移到回收站（`trash` crate，实测可从系统回收站还原）
- [x] `omy-gui`：加密进度条（Tauri 事件按整百分比节流，前端实时渲染）
- [x] `omy-gui`：响应式 UI（≤768px 走移动端布局：抽屉侧栏 + 2 列网格 + 底部导航）
- [x] Android 打包与实机验证（API 35 x86_64 模拟器上跑通构建 / 安装 / 全盘访问授权 / 撤销后报错，见「Android 现状」）
- [x] `omy-gui`：会话同时持有多个密码（`add_password` 按 KEK 指纹判重；21 项端到端实测）
- [ ] 生物识别：桌面需自研原生插件，Windows 那条凭据隔离缺陷必须先 spike
- [ ] Spike S2/S3/S4/S6/S7/S8（S1、S5 已通过，见上文「Spike 结论」）

## 已定决定

| 项 | 决定 |
|---|---|
| 项目名 / 后缀 | `omy` / `.omy` |
| MAGIC | `"OMYFILE" + 0x01`（已确认不改，固化于代码、5 组向量、格式文档） |
| 许可 | core/net：MIT OR Apache-2.0；media：LGPL-2.1+；cli/gui：GPL-3.0 |
| 首期语言 | 简体中文 + 英文 |
| 字幕 | ASS/SSA/PGS/VobSub 暂不实现，已在文档标注；外挂字幕随文件一起加密并提示 |
| 移动端解码 | 优先系统解码器，FFmpeg 仅 demux/remux |
| 转码 | 加密时可选转为 web 原生格式，强制二次确认（不可逆） |
| `BlockSource` | 同步 trait（偏离文档草案的 async，理由见上） |

## 已知偏差与坑

- `chunk_size` 范围校验**只能**在新建文件时做，不能放解析路径——测试向量 v3/v4 用 4096/2048 字节块，格式上合法。
- Argon2 耗时强依赖设备（本机 release 75.4 ms，文档记 180 ms），**不得**把任何具体耗时当常量写入代码。
- 文档中"扫描需 2.5 小时"对应 `10,000 文件 × 5 密码 × 180 ms`；单密码口径为 30 分钟。两种口径下两级 KDF 都是可行性前提。
- `bench` 的"扫描 N 文件"外推**只含密钥运算**，不含文件 I/O。实测 300 文件约 95 ms 说明 I/O 才是主导项，不要把这个外推当端到端耗时。
- `scan` 目前会遍历目录两遍（一遍探 vault 配置、一遍解锁）。两遍都只读文件头不读载荷，相比一次 Argon2 可忽略；但网络文件系统上目录遍历本身可能很慢，届时应改为一次遍历、边收集边惰性派生。
- `CipherId::ChaCha20Poly1305` 是 **12 字节 nonce 的 RFC 8439 版本，不是 XChaCha20**。`--cipher xchacha20` 当前实际落到前者，`doctor` 会如实报告这一点。
- 测试代码用 `#![cfg_attr(test, allow(...))]` 在 crate 根统一放宽 `unwrap_used` / `indexing_slicing`；**库代码不放宽**——库处理不可信输入，任何 panic 路径都是拒绝服务缺陷。
- `cargo search` 在当前网络下必然超时（镜像的 `source.crates-io.replace-with` 只作用于依赖解析，不影响 registry API）。查版本走 rsproxy sparse 索引 `https://rsproxy.cn/index/<按名长分层路径>`，不影响正常构建。
- PowerShell 内联嵌套引号构造 Rust 代码字符串极易 ParserError。改文件请用 Write/Edit 工具，不要用 shell 拼字符串。

### 校验外部命令产物：只看长度会自证成功

排查尾部 moov 抽帧时，诊断脚本用 `> out 2>&1` 把 stdout 与 stderr 混进同一文件，
再用"长度 > 100"判定成功。结果 844 字节的 FFmpeg **错误文本**被当成 WebP 图片，
据此得出「尾部 moov 也能抽帧」的错误结论，并按这个错误结论改了实现——
绕了两轮才发现前 16 字节是 `[in#0/mov,...`。

由此定下两条硬规矩：

1. **诊断时 stdout 与 stderr 必须分开重定向**，绝不用 `2>&1` 混流；
2. **校验产物必须看内容特征**：验图片先验 WebP(`RIFF....WEBP`)/JPEG(`FFD8FF`)/PNG 魔数，
   再做一次真实解码。退出码为 0 不代表产物有效，非零也不代表没产出。

同理，FFmpeg 的退出码不可单独作为判据——需要 `stdout 非空` + `内容可解码` 双重确认。

### 端到端脚本必须自证测的是最新构建

修好 `--range -256` 后重跑 `verify-cli.ps1`，它仍然报同一条失败。
我先怀疑 clap 配置没生效，又怀疑 PowerShell 的 `>` 重定向破坏了二进制流，
写了两个诊断脚本分别验证——**两个假设都被实测推翻**（三种重定向方式全部正确）。
最后在脚本内插桩打印 `exit=2`，才发现脚本硬编码 `target\debug\omy.exe`，
而我只重建了 release，它拿着 19 分钟前的旧二进制在跑。

教训：验证脚本报失败时，**先确认它测的是不是最新产物**，再去怀疑代码。
两个脚本现在都会取 debug/release 中较新者，并在源码比二进制新时直接拒绝运行。

顺带一提，那两个被推翻的假设也有价值：已确认 PowerShell 的 `>`、`cmd` 的 `>`
和直接读 `StandardOutput.BaseStream` 三种方式对二进制 stdout **都是安全的**，
下次不必再怀疑这一层。

### ffprobe 对 MKV 与 WebM 返回相同的 format_name

两者都是 `matroska,webm`（WebM 是 Matroska 子集、共用 demuxer），
但 `<video>` 支持 WebM、不支持 MKV。**只靠容器名判断必然把所有 MKV 误判为 P1**。
`tier::container_natively_supported()` 改为按内容判断：只有视频全在 vp8/vp9/av1
且音频全在 vorbis/opus 才算真 WebM。已有 `mkv_and_webm_share_format_name_but_differ_in_tier` 锁定。

另：`format_name` 是**逗号分隔多值**（MP4 返回 `mov,mp4,m4a,3gp,3g2,mj2`），不能当单值比较。
数值字段类型也不统一——`duration`/`size`/`bit_rate` 是字符串，`channels`/`width`/`index` 是数字，
还可能是 `"N/A"`，故用自定义 `LooseNumber` 容忍四态。

### `payload::read_range` 的 `fetch_ct` 偏移语义（最易踩）

回调收到的偏移是**相对载荷起点**的，不是文件绝对偏移——`read_range`
内部已经减掉了 `header_len`。数据源若是整个文件，必须自己加回：

```rust
let header_len = u64::from(opened.header.header_len);
read_range(&opened.header, opened.payload_key(), None, off, len,
    |payload_off, l| {
        let abs = payload_off + header_len;   // ← 漏掉这步就出错
        Ok(bytes[abs as usize..(abs + l) as usize].to_vec())
    })
```

漏掉会取到偏移 `header_len` 的字节，报 `ChunkAuthFailed`。**错误信息
指向「数据损坏或被篡改」，极易误判成文件坏了或密码不对**，实际是偏移
用错。实现 spike 时踩过一次，已在 `payload.rs` 补测试固化——其中一项
专门断言错误用法必然触发 `ChunkAuthFailed`。

之所以此前没被发现：往返测试都走 `decrypt_all`，它内部自己算偏移，
完全绕过了 `fetch_ct` 契约。这类「只有外部调用方才会踩」的接口坑，
自测同源的用例覆盖不到，必须有独立调用方（如 spike）才能暴露。

对只存载荷的数据源（分片、远程对象存储）而言，相对偏移正是它需要的
形式，所以这个设计本身是对的，只是文档要写得更醒目。

### 远端预览复用 `read_range`：换的只是取密文的闭包

远端浏览一开始很容易做成「把整个文件取回来再解密」——CLI 的
`share connect --fetch` 就是这么做的。但 GUI 要在应用内预览，
照搬会同时踩两条红线：看一个 1 GB 的视频要先等它下完；
取回的密文落盘、解密后的明文也要落盘，直接违背「明文不落盘」。

实际做法是把 `fetch_ct` 闭包从「从已读入的字节里切一段」换成
「发一条 `Request::Read` 去对方那里取一段」，其余**完全不动**：
Range 解析、`MAX_SPAN` 2 MiB 截断、416、CORS、`no-store` 全部共用。
这不只是省代码——两套实现必然漂移，症状会是「本地能拖进度条，
远端不能」这种极难归因的差异。

能这么做的前提是线路协议本来就带 `offset`/`len`（`wire.rs`），
且 `Response::ListOk` 的 `Entry` 直接携带完整 `header` 字节：
文件名、媒体元信息、缩略图全在头里，所以列目录、解文件名、
出缩略图**一次载荷往返都不需要**。

### 锁定的保证必须落在后端，前端调用不算数

`lock` 原本只清会话与设备库，远端连接是在前端 `doLock()` 里
额外调 `disconnectRemote()` 断的。双机实测直接把这个漏洞打出来了：
锁定后 `remote_status` 仍报 `connected`，协议里照样取得到明文
（`status=500` 而非 `403`，因为会话密钥没了但连接还在）。

根因是把安全保证寄托在「前端记得多调一个函数」上。任何绕过那段
代码的路径——别的页面脚本、CDP、将来新加的快捷键分支——都会留下
一条**活着且已认证**的信道。改为在后端 `lock` 命令里 `disconnect()`，
前端只负责同步界面状态。

### 两个 GUI 实例要各自的 WebView2 用户数据目录

双机验证起两个 `omy-gui.exe`，第二个的 CDP 端口死活连不上，
但进程活着、stderr 里也打印了「CDP 已启用，端口 9352」。

原因是 WebView2 默认共用用户数据目录，第二个实例**复用了第一个的
浏览器进程**，于是它自己的 `--remote-debugging-port` 根本没生效。
设 `WEBVIEW2_USER_DATA_FOLDER` 分开即可，两个端口都正常。

这是测试环境的坑不是产品缺陷，但排查起来很费时间：进程在、日志正常、
端口不通，三个现象凑在一起完全不像同一个原因。

### GUI 漏传 `EncryptOptions.argon2`：静默产出打不开的文件

用户实测报的：桌面上首次加密一个文件，用刚设的密码解不开；
再加密一次同一个文件，第二个却正常。

根因是 `encrypt.rs` 构造 `EncryptOptions` 时走 `..Default::default()`
而没显式传 `argon2`。于是 **KEK 用用户选的档位派生，头部却写着
`Argon2Params::default()`（= INTERACTIVE, m=64MiB/t=3）**。
解密方按头部参数派生，得到一个永远打不开这个文件的 KEK。

为什么第二次就好了：`existing_vault()` 会沿用目录里已有文件的
salt **和参数**，而第一个文件头里记的恰好就是 INTERACTIVE——
于是第二次的派生与头部对上了。这个「第一次坏、第二次好」的现象
极具迷惑性，很容易往「时序问题」「缓存问题」上猜。

三条防线，缺一不可：

1. `argon2: params` 显式传入（改掉根因）；
2. `every_kdf_profile_roundtrips` 跨档位往返测试。**原有的
   `roundtrip_encrypt_then_open` 抓不到它**——那个用例固定用
   `interactive`，恰好与 default 相同。同源参数的用例覆盖不到
   「参数不一致」这类缺陷；
3. 写盘前用 `open_with_password` 自检。这条最关键：它必须走
   「按头部参数重新派生」的完整路径，复用手上的 `keks` 会恰好
   绕过要防的问题。

代价是每次加密多一次 Argon2。对「产出永久无法恢复的文件」这个
后果而言，这点开销完全值得。

CLI 没有这个缺陷——它显式列出了 `EncryptOptions` 的每个字段。
`..Default::default()` 在密码学参数上是危险的省事写法。

### `label` 让同一个密码被算成两条凭据

会话缓存键是 `(vault_salt, kind, label)`，所以同一个密码配不同
label 会被当成两条独立凭据，状态栏显示「2 个密码已解锁」。

用户实际遇到的正是这个：加密时自动装入一条，手动解锁时又装一条。
而 GUI 里根本没有任何界面消费这个名字——让用户填一个看不见、
又会让计数出错的字段没有意义，已从两个对话框移除，后端统一成
`"main"`。

core 层保留 `label` 是对的：将来做「诱饵密码」「多用户」时正要靠它
区分不同凭据。这是 GUI 暴露过多底层概念的问题，不是 core 的问题。

### 未加密文件不能预览，成功提示不会消失

两个用户实测报的界面问题，根子都在「只顾了加密文件」。

**提示条**：`state.notice` 只被赋值、从不清除，于是「已加密 1 个文件」
一直挂在屏幕上等人来点。加了 `setNotice(text, ms)` 让成功提示 4 秒后
自动消失。**错误提示刻意不自动消失**——自动消失的错误等于没报错，
用户很可能正低头看别处，回头只看到操作「好像没反应」。

**未加密文件**：双击只是选中，因为 `omystream://` 只服务加密文件。
直觉方案是开 Tauri 的 `protocol-asset` 让 WebView 直接读磁盘，但那会
把**整个文件系统**暴露给 WebView 里的 JS。对一个加密工具，这个攻击面
不划算。

改成在同一条协议上加 `/plain/<token>` 前缀：

- token 是路径的 BLAKE3 哈希，**只有被浏览过的文件才拿得到**。
  脚本猜不出 token，也就读不到没被列出来的文件。
- token 里不含路径信息，泄露到日志里也不暴露用户的目录结构。
- Range 语义与加密路径**完全一致**，否则会出现「加密的视频能拖，
  没加密的反而不能」这种荒唐事。
- 无 Range 的请求也要截断到 2 MiB。WebView 的第一个请求通常不带
  Range，老实返回整个文件的话，双击一个 4 GB 的视频会把 4 GB
  读进内存。

预览组件三种来源（本地加密 / 远端加密 / 本地明文）共用一个，
不是为了省代码，是为了让它们的行为**不可能**产生差异。

「用系统程序打开」没有引入 `tauri-plugin-opener`：那个插件会把
「打开任意路径」暴露给前端 JS。自己用系统命令实现，只在 Rust 侧执行，
且只接受登记过的 token。

读未加密文件不产生任何**新的**明文——它本来就以明文躺在磁盘上。
这与文档 §3 的 L2（临时解密文件）是两回事，那条针对的是把加密内容
解出来写到磁盘。

**顺带删掉的「密码名称」**：会话缓存键是 `(salt, kind, label)`，同一个
密码配不同 label 会被算成两条凭据，状态栏显示「2 个密码已解锁」。
而 GUI 里根本没有界面消费这个名字。core 层保留不变——将来做诱饵密码、
多用户时正要靠它。

**一条关于测试的教训**：第一版探针里有个 `zipUi.overlay || true` 的
恒真断言，跑出来是 PASS 但什么都没验。断言必须能失败才有意义。
另外有两项失败其实是探针自己的状态污染（前一步解锁成功后会话里
已有正确密码，再输错密码仍能解开），差点被当成真缺陷——诊断脚本
出问题时，先查脚本再改产品。

### 文件夹加密：从「如实报不支持」到真正可用

CLI 早就能加密目录，GUI 一直返回 `folder_not_supported`。补齐时的
第一个决定是**不复制那份遍历逻辑**。

复制的代价不是多写几行，而是两份实现迟早分歧——哪天给 CLI 修了
符号链接处理，GUI 那份还在按老样子打包，同一个目录在两个入口会得到
不同的容器。所以提取到 `omy_core::pack`，CLI 改成调它。

提取时顺手修了原实现的两个问题：

- **递归深度无上限**。`walkdir` 默认一路走到底，深层嵌套会爆栈，
  而爆栈不可恢复。新实现用显式栈 + 64 层上限。
- **顺序不稳定**。`read_dir` 的顺序由文件系统决定，同一目录两次打包
  会得到不同的载荷布局。索引里记的是每个文件在载荷中的**区间**，
  布局变了就没法做「同样输入产出同样容器」的校验。现在先排序再写。

另外没有引入 walkdir 依赖：core 的依赖目前只有密码学与格式相关的
几个 crate，保持这份克制让它能被任意项目复用，审计面也更小。

#### 一个会毁数据的隐患

`run_encrypt` 算输出目录时，对文件夹返回的是**文件夹自身**。也就是说
加密 `D:\photos` 会把 `photos.omy` 写进 `D:\photos\` —— 产物落在正被
打包的目录里。轻则下次加密把上次的产物也打包进去，重则边写边读同一棵
目录树。改成一律取父目录，并加了专门的测试盯住它。

#### 容器在界面上必须是「文件夹」

容器的载荷是多个文件拼接。如果 GUI 不认识它，双击会走进单文件预览，
把整包当成一个文件——用户看到一堆首尾相接的字节。

关键决定是**在扫描时就判定**，而不是等 `enrich_file`：那个只在预览时
才调用，而「这是不是个文件夹」在列表刚出来时就要知道。容器标志本来
就在文件头的 flags 里，扫描已经解析过头部，读它零额外开销。

容器内容**复用主列表**渲染（面包屑 + 网格/列表 + EntryCard），进一个
加密文件夹和进一个普通文件夹在体验上没有差别。

早先的实现是一个独立只读面板，当时的理由是：主列表的每个操作（选中、
加密、系统打开、在文件管理器中显示）都以磁盘路径为前提，而容器里的
条目只是载荷里的一段区间，硬塞会让一半按钮点了没反应。这个顾虑是真的，
但结论选错了——正确的做法不是另做一套界面，而是让那些依赖路径的能力
在容器里**自然不出现**：

- `encryptable` 在容器内恒为空数组，所以「加密」按钮不会出现，而不是
  出现了点下去报错；
- 容器内的条目不去查 `state.known`（那张表按磁盘路径索引，容器内的
  相对路径可能与某个磁盘路径撞上，硬查会把别的文件的缩略图贴过来）；
- 应用内看不了的格式落到预览层的兜底提示，但**不给**「用其他应用
  打开」——那要求磁盘上有个真实文件，等于要先把明文解出来落盘。

换来的好处是「同一逻辑只有一处实现」：排序、图标、搜索、状态栏、预览
行为全都只有一份，不可能出现「容器里的视频不能拖进度条、容器外可以」
这类分歧。代价是 `state.container` 成了一个「虚拟位置」，面包屑的段
需要带 `kind`（`dir` / `container` / `inner`）来分派跳转——容器内路径
和相对磁盘路径长得一样，只靠字符串猜会跳错。

#### 预览容器里的单个文件

容器的载荷就是各文件首尾相接后按块加密，索引里记着每个文件的明文区间
`(offset, len)`。而 `read_range` 本来就支持从任意明文偏移读任意长度，
只会解开覆盖该区间的那几个块。所以「预览容器内的一个文件」的实现核心
只是**一层偏移**：请求该文件的第 N 字节 = 读载荷的第 `offset + N` 字节。
**不需要解密整个文件夹**，也不需要往磁盘上解出临时文件。

协议层新增 `/citem/<token>`，Range 语义与 `/file/` 完全照搬（2 MiB 上限
截断、416 + `bytes */{total}`、无 Range 也截断并如实标 206、全套 CORS 与
`no-store`）。有两处容易写错：

- `total` 必须是**该文件自己**的长度，不是容器的 `plaintext_size`。用错
  会让播放器以为文件有整个容器那么长，拖到后面读出别人的字节。
- `fetch_ct` 闭包收到的偏移仍是相对载荷起点的，取密文时要 `+ header_len`。
  这是老坑（漏加的表现是 chunk 0 认证失败 → `MEDIA_ERR_SRC_NOT_SUPPORTED`）。

给前端的是 token 而不是偏移长度，沿用 `plain.rs` 的模型：否则等于把
「读这个容器任意位置」的能力交给 WebView 里的任何脚本，绕过索引这层
约束。登记表里不存路径也不存密钥；token **不构成**授权，每次请求都拿
`entry_id` 回查会话的解锁状态，`lock()` 同时清空登记表。

四种预览来源（本机加密 / 远端加密 / 磁盘明文 / 容器内文件）共用同一个
`PreviewOverlay`，差别只有 URL 前缀。

#### 锁定后容器视图要自己收掉

视图里列的是解密出来的文件名，锁定的语义就是「这些都不该再看得见」。
`lock()` 确实清了 `state.container`，但那是一处容易在重构中被漏掉的
赋值。store 里另有一个 `watch(state.credentials)` 归零即清，让这条保证
不依赖某一行代码没被删掉——和「断开远端连接的保证放在后端」是同一个
道理。原先这道保证长在 ContainerPanel 自己身上，面板拆掉时它是跟着
搬过来的，不是丢掉的。

后端仍是最终判据：`lock()` 清空文件表后，`list_container` 会直接报
`file_not_found`，`/citem/` 也拿不到数据，实测已确认。

#### 调试记录：三次失败都是探针自己的问题

GUI 实测第一轮 19/26，面板死活不出现。三轮诊断：

1. 第一次诊断直接调 `encrypt_paths` 而非走界面，凭据没进前端状态，
   `refreshKnown` 提前返回——**脚本的路径差异**，不是产品缺陷。
2. 面包屑选择器写的是 `.crumbs button`，真实 class 是 `.crumbseg`。
3. 最关键的一次：原文件夹保留着（`original=keep`），列表里同时存在
   📁my-folder（原目录）和 🔒 加密产物。探针按名字匹配，命中的一直是
   **原目录**，双击它只是进目录，什么都测不到。

每一轮都先怀疑脚本、打印真实字段值，而不是改产品去迁就测试。真正的
产品缺陷只有一个（前端 `refreshKnown` 回填时漏了 `is_container` 字段），
它同样是「同一逻辑两处实现」的代价——后端 `annotate_unlocked` 抄了，
前端那份漏了，已在代码注释里标明新增字段时两边都得改。

#### 调试记录（复用主列表这一轮）：又是探针的选择器

改造后重跑实测，43 项里 3 项失败，全部出在探针：

1. **子串陷阱，和上一轮同一个坑换了个位置**。探针按文字找容器那一段
   面包屑（`textContent.includes('my-folder')`），但测试目录本身叫
   `omy-folder-test`，**包含** `my-folder`。于是命中的是磁盘那一段，
   一点就退出了容器，后面两项跟着连环失败。改成按 `.crumbseg.box`
   定位——容器段有专属类名，不必靠文字猜。
2. **`.overlay` 不只是预览层**。加密/解锁对话框和设备面板也用
   `.overlay dlg-overlay`，所以「锁定后没有残留预览层」被一个无关
   对话框判成失败。收紧为 `.overlay:not(.dlg-overlay)`。
3. 顺带把「关闭预览」从「页面上最后一个 `.iconbtn`」改成预览层自己的
   `.overlay-bar .iconbtn`：前者会随页面上出现别的按钮而点错。

教训与上一轮一致：**探针要按结构定位，不要按文字定位**。名字会互相
包含，类名不会。修完 43/43 全绿，产品代码一行没改——再次说明先怀疑
脚本是对的。

#### 实测结果（文件夹加密 + 容器内预览，43 项）

`spikes/verify-folder-encrypt.ps1` + `probe-folder-encrypt.mjs`，全程走
真实 DOM（点侧栏、点选、点工具栏、真实对话框填密码、双击产物与容器内
条目），43/43 通过。关键几项：

- 容器内容渲染在**主列表**里，旧的独立面板已不存在（带反证）；
- 面包屑为 `C: › Users › … › omy-folder-test › 📦 my-folder › inner`，
  容器与容器内层级都能点；
- 容器内 `hello.txt` 预览出的正文是 `hello from folder`——**内容正确
  才说明偏移、密钥、Range 全对**，只验「有响应」是不够的；
- 容器内 `pic.png` 能被浏览器真正解码出 4×4，且 URL 走 `/citem/`；
- 子目录里的 `deep.txt` 同样预览正确（证明偏移不是只对第一层碰巧算对）；
- 容器内文件**不**提供「用其他应用打开」（反证）；
- 在容器根按 ↑ 退出容器回到磁盘目录，状态栏徽标同时消失；
- 锁定后列表、徽标、预览层全部消失，且后端 `list_container` 直接报
  `file_not_found`——**前端不显示不算数，后端拒绝才算**。

单测侧新增 `container_items_decrypt_to_their_own_bytes`：造真加密容器，
三个文件用不同字节填充、长度互不相同且非块整数倍，逐个比对**完整
内容**并额外验一次从中间读。这一项针对本功能最危险的失效模式——偏移
算错不会报错，只会安静地返回相邻文件的字节。omy-gui 单测 109 项通过，
`clippy -D warnings` 零告警，前端构建通过，i18n 键中英一致。

## 回收站与加解密进度（本轮）

### 进度回调放在 omy-core 的哪一层

`payload.rs` 的分块循环是**唯一**知道「第几块 / 共几块」的地方，进度只能
从这里报。新增 `ProgressFn<'a> = &'a mut dyn FnMut(u64, u64)` 与四个
`*_with_progress` 变体，旧 API 全部转调新 API 并传 `None`，所有既有调用方
零改动。

三个刻意的选择：

- **用回调而不是 channel**：不把「CLI 画进度条还是 GUI 发事件」这个决定
  固化进 core，也不引入异步运行时。core 依旧只依赖密码学与格式相关的包，
  `indicatif` / `trash` 都在 core 之外。
- **报明文字节，不报密文字节**：压缩会让密文进度与明文脱节，高压缩比时
  按密文报会让进度条走得莫名其妙。
- **回调同步调用**，实现必须廉价，不得做 IO 或加锁。

### CLI 进度条：什么时候**不**显示比显示更重要

`omy-cli/src/progress.rs`。四个关闭条件缺一不可：`--json`（机器消费，
任何装饰都是噪声）、`-q`、stderr 不是终端（进度条靠 `\r` 原地刷新，重定向
到文件会变成几千行垃圾）、文件小于 8 MiB（几十毫秒就结束，一闪而过比
不显示更烦人）。

开关条件收在 `Out::wants_progress()` 里，而不是让各调用点自己拼
`!quiet && !json`——各自拼迟早漏一个，后果是 `omy info --json | jq` 被
污染。进度一律走 stderr，与既有的 stdout/stderr 分工一致。

实测（`_ttyprobe.py`，用 pty 给子进程真终端）：96 MiB 文件下进度条正常
刷新，含百分比、速率与 ETA；`--json` 模式 stdout 是干净可解析的 JSON；
加解密往返 SHA256 一致，证明进度回调没有改坏产物。

### GUI 进度：卡了整整一轮的 ACL

后端在分块回调里 `emit` 事件，**按整百分比节流**——256 KiB 分块下加密
1 GB 会发四千多次事件，不节流 IPC 反而成为瓶颈。前端在**发起加密之前**
就订阅（小文件可能在 await 返回前就发完事件，晚一步订阅一个都收不到），
`finally` 里取消订阅（否则加密 N 次后同一事件被处理 N 遍）。

第一次实测：加密成功，但进度条**一帧都没出现**。分层诊断（A 原始事件 /
B store 状态 / C DOM）一下就定位了：

```
A层 原始事件监听: "listen-failed:Command plugin:event|listen not allowed by ACL"
C层 DOM 采样帧数: 310 其中含 .prog 的: 0
```

`crates/omy-gui/capabilities/` **根本不存在**，生成的 `capabilities.json`
是空对象 `{}`，Tauri v2 默认拒绝一切插件命令——包括 `listen`。补上
`capabilities/default.json` 声明 `core:event:default` 后即通。

这个坑的迷惑性在于：前端 `listen()` 抛错被我 `catch` 成「订阅失败只是
没有进度条，不该让加密失败」，于是**加密照常成功、进度条静默消失**，
从表面完全看不出是权限问题。只看 DOM 的话，「后端没发」「前端没收」
「渲染没写对」三种断法长得一模一样——分层诊断是值得的。

### 回收站：后端早就支持，用户却选不到

`handle_original` 的 `trash` 分支换成 `trash::delete`。真正的问题在实测
时才暴露：**加密对话框只有「保留」和「直接删除」两个选项，压根没有
回收站**。后端分支写好了，用户永远走不到——单测直接调 `handle_original`，
完全绕过「对话框选项 → 请求字段 → 后端分支」这条链，所以一直是绿的。
这正是 AGENTS.md 要求「端到端验证必须走真实界面」的原因。

补上 `value="trash"` 的单选项后 7/7 通过，并用 Shell API 查系统回收站，
确认原件确实躺在里面、可还原。

守护这条底线的是 `trash_is_recoverable_not_permanent_delete`：它不只验
原件消失（永久删除也满足，而那是数据丢失），还要求回收站里能找到同名
条目。已做**变异测试**——把实现换成 `remove_file`，该测试如期失败：

```
回收站里应能找到 omy-recover-probe-36620.txt，否则说明是永久删除而非可还原
```

顺带修了同一个函数里的既存缺陷：`delete` 分支用 `remove_file`，它对目录
一律失败。而 GUI 支持文件夹加密，「加密文件夹后删除原件」此前是静默
失败的——用户以为删了，其实没删。现在按类型分流到 `remove_dir_all`。

### CLI 的原文件处理：三个参数并成一个（已修）

改造前有三处不一致：命令行是 `--keep-original` / `--delete-original` 两个
布尔开关（其中 `--keep-original` 从未被读过，纯装饰）；配置项
`defaults.original_action` 定义了 `keep|trash|delete` 却**从没被读过**，
写了不生效；而 `trash` 在 CLI 侧压根没实现。

现在统一为 `--original <keep|trash|delete>`，不传则回落配置，与
`--name-mode` 的处理完全同构。取值与 GUI 的 `EncryptRequest::original`
一一对应，`OriginalAction` 的文档注释里标了「新增分支时两边都要改」。

两点没有妥协：

- **先验证再动原件**这条顺序保留，且回收站也一样先验——不能指望用户
  自己去回收站里捞。
- 确认提示按处置方式分开：回收站说「可从回收站还原」，永久删除说
  「不可撤销」。用同一句话，要么让用户高估回收站的风险，要么把删除
  说成可还原，后者更危险。

顺带修了与 GUI 同款的目录缺陷：`remove_file` 对目录一律失败，而
container 模式加密的正是目录，不分流会让「加密文件夹后删原件」静默失效。

### 环境坑：安全软件会删掉测试二进制

调试期间 `cargo test -p omy-core --lib` 持续报：

```
could not execute process ... (never executed)
拒绝访问。 (os error 5)
```

看起来像文件占用或权限问题，实际都不是。实测确认：exe 确实被链接出来
（2 MB），随后被安全软件删除；同一份字节改名成 `.dat` 就不会被删，但
一执行仍然 `WinError 5`；而同一时刻 omy-core 的**集成**测试（另一个
二进制）一切正常。

触发源是测试里 `[0x5A; 32]`、`vec![0xC7; 336]` 这类「同一字节重复几百次」
的字面量，它们在 .rodata 里留下极长的同值串，命中了启发式规则。改用
算式生成填充后即恢复。已写进 AGENTS.md。


## 响应式 UI 与 Android（本轮）

### 响应式：一套组件，两套外壳

`≤768px` 走移动端布局，否则桌面。断点同时写在 CSS 与 JS 两处
（`styles/app.css` 的 `@media` 和 `src/viewport.js` 的 `MOBILE_MAX`），
**改一处必须改两处**——两边不一致会产生最难查的状态：界面已经是移动
版，但打开手势还是桌面版。

移动端与桌面**共用同一个 `MainScreen`**，只换外壳（侧栏变抽屉、顶栏
精简、底部加导航）。没有复制出一个 `MobileScreen`：文件列表、搜索、
进度条、统计的逻辑完全一样，分成两份迟早出现「桌面修了的 bug 手机
上还在」。

### 只写 CSS 是不够的：触屏没有双击

这是本轮最关键的一点。原本打开条目靠 `@dblclick`，而触屏上根本没有
双击这个手势——移动 WebView 要么不触发，要么把它当成双击缩放吃掉。
只做 CSS 适配的结果是**界面看着完全正常，但点什么都打不开**，截图和
CSS 审查都发现不了。

所以「当前是不是移动端」必须是 JS 能读到的响应式状态（`viewport.js`
监听 `matchMedia`，转屏也要跟着变），组件据此换绑事件：

| | 桌面 | 移动 |
|---|---|---|
| 打开 | 双击 | 单击 |
| 多选 | Ctrl/Shift + 单击 | 长按 500ms |
| 已有选中项时单击 | 切换选中 | 加选（不打开） |

长按判定带 10px 移动容差，否则滑动列表会频繁误触发选择；长按后要
吞掉随之而来的 `click`，不然选中完立刻又打开了。

这条断言做过**变异测试**：把移动端单击改回「只选中」，
`verify-responsive.ps1` 第 4 项如期 FAIL，跑完自动还原并重新构建。
证明它真能抓到「移动端点不开」这个回归。

### 真机才会暴露、桌面测不出的两点

- **安全区**：Android 手势导航条会盖住底部导航栏。靠
  `viewport-fit=cover` + `env(safe-area-inset-bottom)` 把内容顶上去。
  桌面浏览器没有安全区，这个缺陷在开发机上永远看不到。
- **长按与系统手势冲突**：长按默认会触发文字选择和放大镜，把多选手感
  毁掉。卡片、列表行、导航项上禁用 `-webkit-touch-callout` 与
  `user-select`，并去掉点击蓝色高亮。

### Android 现状：已在模拟器上跑通

代码侧修掉的**真实缺陷**（不修的话真机必炸）：

- `plain.rs` 的 `launch` / `reveal` 用 `cfg(all(unix, not(macos)))` 分支
  调 `xdg-open`。**Android 也满足这个 cfg**，但 Android 上没有
  `xdg-open`，会 spawn 一个不存在的命令，只得到含糊的 `open_failed`。
  已单独分出 Android 分支，明确返回 Unsupported。
- `trash` 5.2 的 cfg 明确排除了 android，在 Android 上这个 crate 不提供
  任何函数，无条件依赖会**在链接期才报错**。已改为
  `[target.'cfg(not(target_os = "android"))'.dependencies]`，调用点分两支。
  Android 分支返回 `trash_not_supported`，**绝不降级为永久删除**——
  用户选回收站就是想要能后悔。
- scoped storage 下无权限时 `read_dir` **不报错，而是返回过滤后的残缺
  列表**。原先只在 `Err(PermissionDenied)` 时报 `storage_permission_lost`，
  那条分支永远走不到，表现是「侧栏点进去看到个空目录」，像是目录真的空。
  已改为进入沙箱外目录前先查授权状态。

工具链已由 `osdk.toml` 固定（JDK 21 / NDK 29 / build-tools 37 /
platform-tools 37 / emulator 37）。JDK 上限是 **21**，两条独立约束叠加：
Gradle 8.14 最高支持 JDK 24，而 Kotlin 1.9.25 的 JVM target 上限是 21
（更高会报 `JVM target N is not supported`）。Kotlin 与 AGP 版本是 Tauri
生成模板写死的，未改动。

已实测通过（API 35 x86_64 模拟器）：构建 → 安装 → 授权全盘访问 → 侧栏
列出整机 13 项系统目录 → 撤销权限后如实报 `storage_permission_lost`。

顺带确认了一件对 `fsatomic` 很关键的事：**FUSE 上原子写的三个前提全部
成立**——`open(dir)`、`fsync(dirfd)`、同目录 `rename` 覆盖都可用，回读
正确，`ftruncate` 也支持。所以 `fsatomic` 不需要 Android 分支。

#### 仍有摩擦的地方

- **Windows 上打包最后一步需要创建符号链接的权限。** 非管理员且未开
  开发者模式时 `cargo tauri android build` 会在 symlink 步骤失败（Rust
  编译本身已经成功）。开开发者模式，或用 `secpol.msc` 只把「创建符号
  链接」权限给自己的账号即可。
- **`ANDROID_AVD_HOME` 不在 osdk 注入的变量里。** osdk 把 AVD 建到
  `<osdk-data>\data\avd\`，而 emulator 只搜 `$ANDROID_AVD_HOME`、
  `$ANDROID_SDK_HOME\avd`、`$HOME\.android\avd`，于是
  `osdk android avd list` 列得出来、`emulator -avd` 却报
  `Unknown AVD name`。手动设这个变量可绕过。
- **不要给命令加 `-NoProfile`。** osdk 的环境注入在 profile 里，带上它
  `JAVA_HOME` 会退回全局默认（26），然后在 `:buildSrc` 配置期崩掉，
  报错只有一行 `> 26.0.2.1`，指不到真正原因。

#### debug APK 体积（541 MB）不是缺陷

实测拆解 x86_64 debug APK：`lib/x86_64/libomy_gui_lib.so` 占 267.04 MB
（98.6%），其中 `.debug*` 段合计 239.30 MB（`.debug_info` 97 MB、
`.debug_loc` 56 MB、`.debug_str` 51 MB），真正的 `.text` 只有 11.11 MB。
实际 strip 一遍：**267.04 MB → 20.25 MB**。

workspace 的 `[profile.release]` 本来就配了 `strip = true` / `lto =
"thin"` / `codegen-units = 1` / `panic = "abort"`，所以 release APK 约
20 MB 出头。

两个顺带发现：`.so` 在 APK 内**完全未压缩**（AGP 4.0+ 的默认行为，为了
让系统直接 mmap；Play 分发会重新压缩，只影响侧载体积）；`assets/` 是
0 项，不是漏打——Tauri 2 把 `frontendDist` 编进了 Rust 二进制。


## 两个「面向用户的输出」缺陷（本轮）

Android 与局域网双机验证在**当时**都在等环境（Android 已于 2026-09-06
在模拟器上跑通，见「Android 现状」；双机 mDNS 仍未做），所以这轮转向
已有代码里能就地验证的东西。查 `not_implemented|未实现|TODO` 时发现两处
真实缺陷，都属于「产品能跑，但告诉用户的信息是错的」——这类问题不会让
测试变红，只会让用户做出错误判断。

### 1. `doctor` 报假消息

`doctor.rs` 开头写着「只报告**实测**结果，不猜测」，文件末尾却硬编码
了三条警告，其中两条已经过期：

| doctor 原本说 | 实际情况 |
|---|---|
| 媒体预览：未实现，FFmpeg 封装尚未接入 | omy-media 早已可用（探测/分级/moov/缩略图/P2 转封装） |
| 局域网共享：未实现，mDNS 与 Noise 尚未接入 | `share serve/discover/pair/connect/devices` 全部可用 |
| XChaCha20-Poly1305 未实现 | 真的，core 的 `CipherId` 只有 ChaCha20Poly1305 与 Aes256Gcm |

`doctor` 的唯一价值就是回答「这台机器上什么能用」。报假消息比没有
这个命令更糟：用户会放着可用的功能不用，或者跑去排查一个不存在的
问题。

讽刺的是 `omy_media::ffprobe` 早就提供了 `has_ffprobe()` /
`has_ffmpeg()` / `version()`，函数注释明写「供 `doctor` 这类命令如实
报告环境能力」——能力一直在，doctor 没用它。

现在改为真探测，并且**分开报缺哪个**：图片缩略图走纯 Rust 不需要
FFmpeg，只缺 ffmpeg 时探测仍可用但视频抽帧不可用，二者影响范围
不同，合并成一句「FFmpeg 不可用」会误导。第三条保留但降为 note：
它是格式层面的已知偏差，不是本机环境问题，用 ⚠ 会让用户以为自己
机器缺东西。

验证用 `spikes\verify-doctor.ps1`：**走 PATH 独立查一遍** ffprobe /
ffmpeg 是否存在，再与 doctor 的结论比对——用产品自己的输出验证
产品等于没验证。

### 2. `--json` 输出不是合法 JSON

`Out::result(human, value)` 在 JSON 模式下打印 `value`。有 **6 处**
在逐行循环里调它并传 `json!(null)`，只想打印人类可读的那一行，结果
JSON 模式下每行吐一个裸 `null`，最后才是真正的对象。

实测：`--json doctor` 吐 **11 个** `null`，`--json list` 吐 1 个，整体
`ConvertFrom-Json` 直接失败。`--json` 存在的唯一意义是给脚本消费，
输出不可解析等于这个开关是坏的——而且坏得隐蔽：人类模式完全正常，
只有写脚本的人会撞上。

根因修在 `Out` 上，新增 `line()`：只在人类模式打印，JSON 模式静默。
没有去改 `result()` 的语义——「JSON 模式打印 value」这条约定是对的，
错的是调用方拿它当纯文本输出用。

一个旁证说明这个坑早就被踩过：`scan.rs` 外面手写了
`if !ctx.out.is_json()` 来绕开它，但只是就地打补丁，没解决根因，于是
另外 5 处继续犯。改用 `line()` 后这层手写判断也去掉了，条件收在
一个地方。

`spikes\verify-json-contract.ps1`（16 项）同时守两个方向：JSON 模式
必须是单个可解析文档且无裸 `null`；**人类模式该打印的仍要打印**——
修法是「JSON 模式静默」，方向搞反就会变成人类模式也不打印，那等于
把功能删了。造了真实的 `.omy` 让逐行循环真的执行，否则测不到这个坑。


## 密钥 slot 管理：add / remove / change（本轮）

`key add/remove/change` 之前是 `bail!("尚未实现")`。现在实现了，核心
是**原地改写**：只重建 384 字节 slot 区、重算 32 字节头部 MAC，
载荷密文与 `file_uuid` 逐字节不变。

实测（`spikes\verify-key-slots.ps1`，35 项）：692 字节的文件改密码前后
大小一致，`header_len=656` 之后的载荷字节完全相同，slot 区确实变了。
这条断言的作用是防止有人图省事改成「解密后重新加密」——那样会换掉
`file_uuid` 与全部密文，对外部观察者是一个全新文件，增量备份要重传
整份、去重失效，大文件还是数小时的重写。

### 接口为什么要求传「全部保留的密码」而不是「要加的那一个」

这是格式的直接后果，不是实现偷懒。每个 slot 的包裹密钥由
`(KEK, file_uuid, slot_index)` 唯一确定，而未使用的 slot 填的是随机
字节、与真实 slot 在字节层面不可区分（可否认性，决策 D-02）。所以
**无法探测哪个槽是空的**——`add` 不能「找个空位填进去」，猜错就会
静默覆盖掉另一个密码，用户要到下次用那个密码时才发现，那时已经无法
恢复。

于是这三个命令的真实语义都是「重新声明这个文件的密码集合」：

| 命令 | 保留 | 作废 |
|---|---|---|
| `add` | 解锁用的密码 + 新密码 | — |
| `change` | 只有新密码 | 解锁用的那个 |
| `remove` | 只有解锁用的密码 | 该文件上其它所有密码 |

`remove` 这层语义必须在提示里讲清楚：它不是「删掉某一个密码」，而是
「只留下我现在用的这个」。所以除 `add` 外都要确认，且警告文案点明
「其它密码都会失效」。

### 三个不能省的检查

- **改写前完整走一遍 `open`，MAC 必须验过**。跳过的话，若头部已被
  篡改，我们会给篡改后的字段算一个新的有效 MAC，等于替攻击者洗白。
  实测确认篡改 `plaintext_size` 后被拒。
- **`keep` 为空直接报错**。留下 0 个 slot 的文件永远打不开，等同销毁
  数据，不该能通过一次「移除密码」意外达成。
- **写回前先自证新文件能用每个保留密码打开**，逐个验而不是只验第一
  个。`add` 最容易犯的错是新密码能开、原密码被挤掉，只验一个正好
  漏掉。验证失败就放弃写回，原文件不动——顺序反了的话，用户会同时
  失去旧文件和访问权。

`change` 这里差点写出真 bug：一开始想用 `keep[0]` 当解锁密钥，但
`change` 的 `keep` 里只有新密码，拿它去解原文件必然失败。解锁与保留
是两个不同的集合，必须分开传。

### 变异测试

两条最容易被无声破坏的断言做了变异验证：把空槽改成填零 →「空槽随机」
如期失败；把实现改成只覆盖前 n 槽 →「被移除的密码打不开」如期失败。
后者正是 `remove` 变成假功能的典型写法。

变异测试本身踩了个坑：还原源码时用了保留时间戳的复制，源码 mtime
因此倒退，cargo 认为二进制比源码新直接复用，跑的还是带缺陷的二进制。
表现是「明明还原了测试却仍然失败」，而源码里搜不到任何变异痕迹。
已记入 AGENTS.md。

### 一个把脚本 bug 显示成产品缺陷的例子

容器那一段最初断言 `restored\a.txt`，FAIL。实际容器会把原文件夹名
作为一层子目录还原，正确路径是 `restored\folder\a.txt`——产品是对的。
先怀疑脚本、打印真实路径，几分钟就定位了。

局限（文档 03 §5 已写明，UI 也要照此表述）：移除 slot 只影响这一份
文件。攻击者若留有旧副本，仍能用旧密码打开那个副本——真正的密钥轮换
必须重新加密载荷，属于另一个操作。

## 密码管理的 GUI（本轮）

core 与 CLI 能改密码之后，界面上还没有入口，用户得去命令行。现在补上
`manage_key` 命令 + 一个对话框，工具栏在选中单个已解锁的加密文件时
出现 🔑。

三个操作放在同一个对话框里用单选切换。它们在格式层面是同一件事
（改写 slot 区），分成三个入口会让人以为代价不同——尤其会让 `remove`
看起来像个轻量操作。

不做批量：各文件的 vault salt 可能不同，而且「有几个密码」查不出来，
一半成功时用户手里是一堆状态不明的文件。

### 界面必须主动说的两件事

- **「这个文件配了几个密码」查不出来**，所以对话框里常驻一句说明。
  用户第一反应就是去界面上找那个数字，找不到会以为是 bug。查不出来
  是可否认性的直接后果，不是没做。
- **`remove` 只影响这一份文件**。旧副本仍能用被移除的密码打开。这条
  最容易被理解成「密码已经彻底作废」，所以用醒目的警示块单独讲，
  确认按钮也变红。

`remove` 时不渲染新密码框；切换操作时清空新密码输入框——不清的话来回
切一圈会留下用户已经不记得的旧输入，然后带着它提交。

### 顺手修掉一个每天都会撞上的会话缺陷

验证 `change` 时卡住了：界面报改密码成功，用新密码解锁也说成功，但
文件仍显示「🔒 需要密码」。

根因在 `session.rs`：缓存键是 `(vault_salt, kind, label)`，**不含
密码**。`unlock_password` 原来在键命中时直接 `return Ok(())`，可键
命中只说明这个**名字**用过，不说明密码相同——同一 label 下的第二个
密码于是从未被派生，会话里留着的还是上一个 KEK，而返回值是 `Ok`，
调用方以为解锁成功了。

GUI 的 label 恒为 `"main"`（有意如此，否则同一个密码会被算成两条凭据、
状态栏数字出错），所以这是常态路径而非边角：

- 先打错一次密码、再输对的 → 第二次被忽略，界面一直提示打不开；
- 改完密码后自动装入新密码 → 装不进去，文件显示成锁定。

改成每次都重新派生并覆盖，旧 KEK 被挤出时随 `ZeroizeOnDrop` 清零。
扫描性能不受影响：慢速 KDF 每个密码只跑一次靠的是调用方在**文件循环
外**解锁，每个文件复用 KEK 靠的是 `scannable_for` 从缓存读，都不依赖
这个早退。

没有改成「把密码指纹并入缓存键」：那需要在 Argon2 之前先算一个**快速**
密码哈希并驻留内存，能读进程内存的攻击者就从「逐个猜 Argon2（每次几百
毫秒）」变成可离线秒破，而密码常被复用到别处。为省一次 KDF 削弱密码
强度不值得。

原来那条 `repeated_unlock_runs_kdf_only_once` 断言 `kdf_runs==1`，锁死
的正是这个有害的实现细节。测试锁实现细节的代价在这里很具体：它不但没
抓到缺陷，还让缺陷看起来是「设计如此」。改为只断言「不会多出凭据条目」
这个真正的外部行为，另补一条回归测试，并用变异测试确认注入早退后它
确实会失败。

### 端到端验证（`spikes\verify-keymgmt.ps1`，36 项）

core 的单测直接调 `rewrite_slots`，CLI 的 35 项走命令行，两者都绕过了
「按钮出现条件 → 对话框字段 → invoke 参数名 → 后端分支」这条链。字段
名写错它们照样全绿，而界面上点「应用」什么都不会发生。

跑完再用 CLI 独立解一次密文复核。用产品自己的界面验证产品等于没验证，
这一步换一条完全独立的路径确认最终密码状态与界面结论一致。

两个探针 bug 都伪装成了产品缺陷：

- 加密文件的行文本是占位名「🔒 加密文件」，**不含磁盘文件名**，按
  `secret` 定位必然找不到——看起来像「列表里没有这个文件」。按结构
  定位才对。
- 解锁必须**走界面填密码**，不能直接 invoke `unlock_directory`：后者
  只派生密钥，列表里的 `unlocked` 要靠 `scan_directory` 的结果回填
  （`store.refreshKnown` / 后端 `annotate_unlocked`）。跳过这步，所有
  定位都会落到锁定行上。

自证「测的是最新构建」时把**前端产物**也算进来：只改 `.vue` 不重新
`npm run build`，跑的还是旧界面，而这类问题从截图上完全看不出来。

移动端把对话框宽度放开到 100%。440px 固定宽在 390px 屏上会把「应用」
按钮推到屏幕外——界面看着正常却根本点不到，所以探针里用
`elementFromPoint` 断言按钮真的可达，而不只是看宽度。

## 真正的密钥轮换：重新加密（本轮）

上一轮结束时留的缺口：对话框只用文字说了「`remove` 不等于密钥轮换」，
却没给出那个操作。`remove` 的警示文案当时就写着「要让旧密码彻底失效，
必须重新加密」——一句用户照做不了的建议。现在补上 core 的
`reencrypt`、CLI 的 `key reencrypt`、GUI 的第四个选项。

分工（文档 03 §4.3 的成本表）：`add` / `change` / `remove` 只重写 384
字节 slot 区，毫秒级，载荷密文一字节不变；轮换换掉 FEK、`file_uuid`、
`base_nonce`，把载荷整个重新加密，耗时与文件大小成正比。所以它只能是
用户明确选择的另一个操作，不能做成密码管理的默认行为。

**它仍然挡不住已经流出去的副本**：那是一份独立的密文，本操作对它无能
为力。UI 的说法必须是「这份文件从此与旧密码无关」，不能是「彻底作废
旧密码」——后者会给人已经收回泄露的错觉。

### 一条看起来很严格、其实测不到东西的断言

这是本轮最值得记的一件事。轮换的核心性质是「换掉 FEK」，我给它写的
测试断言了三件事：`file_uuid` 变了、`base_nonce` 变了、载荷密文全变了。
读起来相当严密。

变异测试注入「复用旧 FEK」（即把轮换退化成 keyslot 改写）之后，**这条
测试照样通过**。原因很清楚：那三样东西全部由调用方传入的随机材料决定。
`rnd` 是新生成的，所以 `file_uuid` 必然变、`base_nonce` 必然变，而载荷
密钥是 `HKDF(FEK, file_uuid)`，`uuid` 一变密文自然全变——三条断言同时
满足，而 FEK 根本没换。

而复用旧 FEK 是真漏洞，不是理论问题。用一个独立小程序实测确认了攻击
可行：攻击者拿旧副本 + 旧密码解出 `FEK_old`，新文件头里的 `file_uuid`
是明文，`HKDF(FEK_old, uuid_new)` 就是新文件的载荷密钥——他不需要新密码
就能解开轮换后的文件，轮换等于没做。

改法是把攻击本身写成断言，而不是去比 FEK 的字节：比字节只能证明「变
了」，写成攻击才能证明「旧的用不了」。另外三个变异（丢缩略图、丢容器
索引、Argon2 参数走 default）都被原有断言抓到了，唯独最关键的这条是
装饰品。**断言的严格程度不能靠读起来的样子判断。**

### naive 的「解密再加密」会静默丢元数据

`EncryptOptions` 只覆盖 15 个 TLV 类型里的 7 个。直接解密再加密，其余
元数据一声不响就没了，而症状离原因很远：丢容器索引会让加密文件夹解成
一堆拼接字节且不报错；丢 Argon2 参数会让头部记录的参数与派生 KEK 用的
参数不一致，文件当场打不开（`encrypt.rs` 里记着同一个坑）。

所以 `rebuild_options` 逐项搬运。核对了生产代码实际只写 8 种 TLV，逐项
覆盖即可。容器索引取**解密后的原始字节**而不是 `parse` → `encode` 往返：
任何编码差异都会变成「轮换后容器打不开」，且只在带容器的文件上出现。

另外两个顺序约束：`decrypt_all` 必须在换 FEK **之前**（原载荷若已损坏，
重新加密只会把损坏内容用新密钥固定下来，旧密文被覆盖后再也无从追查）；
改写前要完整走 `file::open` 验头部 MAC（跳过等于把篡改后的字段连同新
MAC 一起签进去，替攻击者背书）。

### 新密码是可选的

轮换的核心是换文件密钥，改不改密码是另一件事。强制要求新密码会让「只
想让旧副本的密码对这份文件失效、密码不变」这个正当需求无法表达。也因此
不拦「新旧相同」——那对轮换是合法用法；但两次输入不一致仍要拦，那是打错。

CLI 也不主动追问新密码，只在显式给了 `--new-password-*` 时才读。交互式
追问会让用户以为轮换必须改密码。

### 进度条不是可选项

轮换是唯一耗时与文件大小成正比的密码操作，而警示里写着「期间请不要关闭
程序」。没有进度条的话，用户面对一个静止界面又被告知不能关，无从判断是
在跑还是已经卡死。

复用 `encrypt://progress` 与现成的渲染，不新开事件——两份进度渲染迟早
出现「加密的能动、轮换的不动」。core 给的是两个独立回调（读一遍、写
一遍），映射成 `index` / `total_files`，**不合成一条总进度**：读和写速度
不同，合成会让进度条在中点莫名减速，用户以为卡住了。

### 端到端验证（`spikes\verify-reencrypt-gui.ps1`，40 项）

与 `verify-keymgmt.ps1` 分开：那个验「改密码不动载荷」，这个验「轮换
必须换载荷」，断言方向正好相反，混在一个脚本里容易写串。CLI 侧另有
`verify-reencrypt.ps1` 23 项。

素材用 24MB 随机字节。小文件会在一次回调里跑完，进度条一闪而过、轮询
不到——那不是产品缺陷，是素材选得不对。

三个探针 bug 又都伪装成了产品缺陷：

- **移动端选中要长按**。移动端 click 的语义是「打开」，进入选择模式的
  唯一手势是长按 500ms。发 click 的结果是把文件打开了、选中数恒为 0、
  🔑 按钮不出现，报 `no-button`——看着像「移动端没有密码管理入口」，
  而入口一直在（桌面端已验证过）。AGENTS.md 里那条「涉及打开、多选、
  拖拽的交互要读 `isMobile` 分别处理」，对探针同样成立。
- **对话框叠层**。上一节故意留着对话框验「出错不关闭」，下一节直接解锁
  就叠出第二层遮罩；下层按钮的几何算得出来，但点击被上层遮罩接住，报
  `covered-by:overlay`——看着像布局把按钮压到遮罩下面。同样的隐患也存在
  于 `probe-keymgmt`：它以前能过是巧合，按钮恰好落在遮罩的可点区域内，
  这次对话框多了一个选项变高，巧合就没了。**靠布局尺寸恰好通过的断言
  迟早会翻**，所以两个探针都加了「只有一层遮罩」的显式自证。
- **断言查了渲染文案而不是数据**。原来靠 `.prog-sub` 的文本含「2」判断
  阶段数。这既依赖语言，又会被文案层面的缺陷带偏——下面那个占位符 bug
  正好让它失败，报的是文案问题却挂在「阶段数」这条上，把排查方向指错。
  改为读 `.prog` 上的 `data-stages`。

### 顺带修掉的 i18n 占位符缺陷

`i18n.js` 的 `interpolate` 只替换 `{{name}}`，但 `busy.file_of` 写的是
「第 `{i}` / `{n}` 个」、`remote.connected_to` 写的是 `{name}`。用户看到
的是原样的字面量而不是数字。

两处平时都看不到，所以一直没发现：`file_of` 只在批量加密**多个**文件时
出现，`connected_to` 要连上另一台设备才出现。而 `check-i18n-keys.py` 只
比对键名是否齐全，管不到占位符写法——**两边文案都「齐全」，检查通过，
缺陷就一直留着**。

给检查器补了两条：占位符必须是双花括号、中英键集合必须完全一致（后者
以前靠人工比对）；并改成检查失败时非零退出（原来只打印不退出，串在
脚本里不会中断）。两条新检查都做了变异测试确认能抓到对应缺陷——一条
永远通过的检查比没有检查更糟，它给人已经防住了的错觉。

### 还没做

轮换的内存占用是整份明文（`decrypt_all` 一次性返回 `Vec<u8>`），大文件
会吃掉与文件等大的内存；也没有取消入口。
## 元数据还原：存了三个月却从没读出来（本轮）

`pack.rs` 的 `meta_from_fs` 一直在采集 mtime/btime，unix 上还有
mode/uid/gid，容器格式也老老实实编码了这些字段。但**解包路径里没有任何
一处读它们**——实测确认解出来的文件修改时间全是「现在」。

这在备份场景下是实质的数据丢失，而且不可逆：用户按「加密归档 → 删原
目录」走完流程，时间信息就再也拿不回来了。而它一直躲过了所有检查，
因为每一层单独看都是对的：采集有测试、格式有测试、内容还原有 43 项
端到端验证——**没有人测「内容之外的东西」**。

发现它的过程也值得记：待办清单上那条写的是「元数据保存与还原报告的
完整实现」，合成一句让人以为整件事都没做。拆成两条（保存已做 / 还原
没做）之后，缺口才清楚。清单本身就是判断依据，它含糊或过期的代价是
具体的——本轮一开始还差点按「slot 增删改还没做」去重复实现一遍已经
提交过的东西。

### 报告必须把「平台不支持」和「这次失败了」分开

两者对用户的含义完全不同：

- **不支持**是平台的既有事实，换台 Linux 解开就能拿到，所以文档要求配
  一句「完整元数据仍保存在加密文件中」。把它报成错误会让人以为文件坏了。
- **失败**是这次操作出了问题（文件被占用、权限不足），同一台机器重试
  可能就好。把它报成「平台限制」会让人放弃重试，白丢元数据。

`uid/gid` 有意不还原：跨机器的数字 uid 指向完全不同的用户，还原它要么
无效要么危险（把文件判给本机另一个账号），且需要 root。但**「有意不做」
和「忘了做」在代码里长得一样**，区别只在有没有告诉用户——所以仍然报告，
并为此单独写了一条测试锁住。

Windows 上不把 `mode` 近似成只读属性。看着贴心，实际是用语义不同的东西
冒充还原成功。

### 一段基于错误理由的复杂度，写完又删了

起初按「设置子项的 mtime 会更新父目录」把目录拆出来按深度倒序处理，
还写了两条测试守着这个顺序。变异测试发现改成正序后测试照样通过——于是
要么断言无效，要么这条理由本身就是错的。

没有推理，写了个小程序实测四种操作：

| 操作 | 父目录 mtime 是否改变 |
|---|---|
| 设置子目录的 mtime | 否 |
| 设置子文件的 mtime | 否 |
| 在目录里**新建**文件 | 是 |
| 在目录里**新建**子目录 | 是 |

与注释相反：只有目录项的增删会动父目录 mtime（这也是 POSIX 的定义）。
所以排序纯属多余，真正的约束在**调用时机**——必须等全部条目落盘之后
再设时间。

删掉排序而不是补一条断言：留着一段理由已被证伪的代码比没有它更糟，
下一个人会以为这里有个已知陷阱而绕着改。相应地把两条测内部顺序的测试
换成一条测真实约束的，外加一条**对照测试**证明「顺序颠倒会怎样」——
没有它，那句文档就只是一句声明，将来有人为了「边解边设省一次遍历」把
调用挪进落盘循环，测试不会有任何反应。

9 个变异全部被预期的那条断言抓到。

### 自己写出来的「只报标题不报内容」

人眼看 CLI 输出时发现默认只有：

```
警告: 部分元数据未能还原（当前平台限制）：
  完整元数据仍保存在加密文件中，在其他平台解开可还原更多项
```

冒号承诺要列举，然后直接跳到结语，具体哪一项一个字都没说。原因是列举用了
`out.detail()`，那是 `-v` 才显示的级别——用户想知道的唯一信息要重跑一次
才拿到，而这段文案的整个目的就是回答那个问题。

**这个缺陷不会让任何断言变红**：磁盘上的时间戳全对、JSON 字段全对，缺的
只是默认输出里的几行字。所以补了一条断言把它锁住：既然 unsupported 里有
btime，默认输出里就必须出现「创建时间」。

### GUI 侧没有「还原元数据」可做（**已被下一轮推翻**）

查之前先实测：GUI 产品代码里只有两处写盘（`encrypt.rs` 写密文、
`keymgmt.rs` 改头部），预览走 `citem` 协议按需解密、不落盘——「明文不
落盘」是既定原则。所以元数据还原只发生在 CLI 解密时。

> 这个结论在当时是准确的，但它描述的是**当时的实现**，不是设计约束。
> 下一轮补上「还原文件到磁盘」之后，GUI 就有了自己的还原路径。留着这段
> 是因为它记录了一次有用的判断方式（先实测写盘点再下结论），但**不要**
> 据此认为 GUI 不该有解密路径——见文末「还原文件到磁盘」。

GUI 侧真正该验的是**采集**那一半：它加密出来的容器如果没带元数据，
CLI 那 27 项验证再全也救不回来，因为值根本没存进去。理论上两边共用
`pack_folder` 一定有，但 `encrypt.rs` 里那段注释就是反例——同一条共用
路径上漏传 `argon2`，文件当场打不开。所以走真实界面加密，再交给 CLI
解开核对磁盘上的时间戳（`verify-metadata-gui.ps1`，探针 10 项 + 驱动 9 项）。

### 四个探针 bug，又都伪装成产品缺陷

1. **CDP 端口的环境变量名凭印象写错**（`OMY_DEVTOOLS_PORT`，实际是
   `OMY_GUI_CDP_PORT`）。GUI 正常启动但不开调试端口，只报「CDP 未就绪」，
   看着像 GUI 起不来。
2. **行定位的类名也是凭印象写的**（`.name/.fname/.lname`，实际网格是
   `.cname`、列表是 `.nm`）。于是全部退回整行文本比较，而整行含图标与
   类型后缀（`📁xxx文件夹`），精确比较必然失败——报「找不到测试目录」，
   可失败信息里其实已经把那一行打印出来了。**「按结构定位」的前提是结构
   得先查准**，凭印象写类名和按文字定位一样不可靠。改为优先读 `aria-label`。
3. **`browse_directory` 返回 `Vec<DirEntry>`（数组本身）**，探针按
   `.entries` 取拿到 undefined，把成功的加密报成失败。而前面 8 项全通过，
   两个结论矛盾时先怀疑脚本。
4. **断言产物必然叫 `folder.omy`**。实测是 `a4332ff02cbd0378.omy`——GUI
   默认开启文件名加密，落盘名是随机的。CLI 产出 `folder.omy` 是因为按
   `-o` 显式指定了路径，**两个入口默认行为不同，拿一个推断另一个就会
   出错**。改为按扩展名找；断言反而更严，名字无关紧要，内容对不对才是要点。

另外让解密根目录自证而不是硬编码 `folder`：容器根名丢了的话，硬编码会让
后面每条断言都报「条目未还原」，指向元数据还原，而真正的问题在容器索引。


## 还原文件到磁盘：GUI 一直缺的那半边（本轮）

在此之前 GUI 只能加密和预览，**没有任何把文件取回来的入口**——用户要
拿回自己的文件只能去装 CLI。加密工具缺解密入口，这个缺口比听起来严重：
它意味着「加密」这个操作在 GUI 里是单向的。

### 先划清和「明文不落盘」的边界

动手前必须回答一个问题：这是不是违背既定原则？F-06 写着「无临时明文
文件」，PROGRESS 上一节还刚记过「GUI 没有明文落盘的路径」。

结论是不冲突，但理由要写下来，否则下一个人会以为这里破了规矩：

| | 预览（F-06 管的） | 还原（本轮做的） |
|---|---|---|
| 谁决定要落盘 | 实现为了方便 | **用户明确要求** |
| 用户知道文件在哪吗 | 不知道 | 自己选的路径 |
| 什么时候消失 | 不确定 | 由用户管理 |

区别不在「有没有明文落盘」，而在**是不是用户的意图**。F-02 本身就要求
「解密后 bit-for-bit 一致」——这条需求没有解密路径根本无法验收。

顺带明确不做的：**解密后自动打开**。那是 F-19（外部应用打开）的事，
走受控临时文件 + 用完即删，与「按用户指定位置持久保存」是两套语义。
混在一个命令里会让「这个文件会不会被自动删掉」变得无法回答。

### 先把解包逻辑提到 core，再动 GUI

解包此前只在 CLI 里。GUI 要用，照抄一份的代价不是多写几行——那段代码
里有两类安全判断：

- **路径逃逸防护**（`..`、盘符）。复制之后再修一个逃逸漏洞，另一个入口
  仍然可被利用，而且没人会想起它还在。
- **文件名兼容处理**（Windows 非法字符与保留名）。两份实现给出不同的
  替换结果，同一个容器在 CLI 和 GUI 解出来会得到不同的文件名。

所以先做 `omy_core::unpack`（9 项单测），CLI 改为调它并删掉自己那份，
`sanitize_filename` / `safe_join` 的单测跟着实现一起搬走——留在原处会
变成「测一个自己不再拥有的行为」。

元数据还原的调用点也一并收进 `unpack` 内部。它有个顺序约束：必须等
**全部条目落盘之后**再调（新建目录项会更新父目录 mtime）。放进函数
末尾，这个约束就由结构保证；留给调用方则是每个入口都要记得，忘了的
后果是「文件时间对了、目录时间还是现在」，只在非空目录上出现。

### 写了一段基于错误前提的代码，核实后删掉

我原本给单文件路径写了还原 mtime 的代码。核实时发现两个假定的 API
（`Opened::mtime_ns`、`restore::restore_one`）都不存在，更根本的是：
**单文件加密从来没保存过时间戳**。

格式里确实预留了 `ORIGINAL_META`（TLV 0x0008，注释写着「原始文件元数据
JSON」），但全仓库 grep 只有两处命中——常量定义和 `is_known` 白名单，
**没有任何写入或读取**。这是个从未启用的坑位。

所以那段代码取不到值，写多少都是摆设。改为如实报 `not_recorded`：

- 假装支持：用户解开单文件，时间是「现在」，界面什么也不说。他会以为
  是 bug，或者以为自己记错了。
- 如实报告：明说「单个文件未保存修改时间（加密时未保存）」。用户知道
  这不是故障，**而且顺带知道了一个替代方案**——加密整个文件夹走容器
  路径，时间是保住的。

### 目标位置：两个显式选项，而不是一个输入框

「当前目录」是绝大多数情况下想要的（在哪看到就解在哪）。但默认值不能是
「上次选的目录」——写盘有副作用，不该有隐藏状态；同一个按钮在不同时刻
把文件写到不同地方，是最难排查的那类问题。

所以两个选项都摆出来，并把**最终路径直接显示在对话框里**，用户点确定
之前就能看到文件会落在哪。选了「自选目录」却还没挑路径时禁止提交——
否则会静默落到源目录，而用户以为自己已经指定了别的地方。

其余几个默认值按「错了也不至于毁数据」定：`overwrite` 默认 false
（覆盖不可逆），目标目录在解密**之前**校验（否则白算一遍 Argon2 才报错）。
部分失败时不关对话框：`target_exists` 是用户自己能解决的，关掉的话他得
从选文件重新开始。

### 守卫漏掉的那条边：exe 比 dist 新

端到端验证跑出「按钮点了但对话框不出现」，前 10 项全部通过。我先去翻了
`App.vue`（那里确实另有一个真问题，见下），修完重跑**结果一模一样**，
才想到查时间戳——exe 是 14:45 构建的，dist 是 14:51。

前端的依赖链有两条边：

```
frontend/src --(npm run build)--> dist --(cargo build)--> exe
```

`tauri.conf.json` 里 `frontendDist: "dist"`，前端在**编译期被内嵌进
exe**。而我的守卫只查了「dist 比 src 新」和「exe 比 `*.rs` 新」，
**漏了 exe 比 dist 新**。于是跑起来的 exe 内嵌着旧界面。

这个漏洞的现象极具误导性：按钮在、可点、点了不报错，就是没反应。截图、
CSS 审查、查 DOM 全都发现不了——因为界面本身是「正确的旧版本」。

**和本轮修的元数据缺陷是同一类问题**：每个检查单独看都对，漏的是它们
之间那条边。补上之后守卫会明确报出两个时间戳，一眼能看出该重建什么。

### 一个「检查通过但什么都没保证」的自证

`App.vue` 的真问题是：接线脚本在中途失败并返回（它的语义是「全部成功
才写盘」），于是 script 段的 import、ref、handler、store 导入**一处都
没写进去**，而随后另一个脚本只补了模板那两处。模板引用了四个不存在的
标识符。

而我的自证检查是 `'restorable' in content` 这类**子串存在**——那些词在
模板里已经出现了，所以检查全绿。它证明的是「这个词出现在文件某处」，
不是「这个定义存在」。

改为按定义特征校验（`const showRestore = ref(false);`、
`import RestoreDialog from ...`），并逐项打印。**断言必须针对它声称在
验证的那个东西**，否则通过与否和事实无关。

### 两个探针坑

1. **光调 `unlock_directory` 不够**。它返回
   `{credentials:1, vaults_unlocked:1}`（确实成功），但紧接着
   `browse_directory` 报 `unlocked:false`。查 `browse.rs:204`
   `annotate_unlocked` 才明白：它是拿 `state.files()`（已扫描登记表）
   按路径匹配的，**解锁不会往那张表里加东西**。真实 UI 走
   `refreshKnown` → `scan_directory` 才登记。探针跳过了这一步，
   `entry_id` 恒为 null，前端 `restorable` 为空，按钮永不出现——
   后面 12 项全是这一条的连带反应。又是「层与层之间」。
2. **解锁后界面显示的是原名，不是磁盘名**。按 `payload.omy` 找不到行，
   得用 `real_name`（`payload`）。首轮失败信息里打印出的候选是
   「🔒 加密文件」——那是未解锁时的占位文案，其实已经指明了真正的问题
   在第 1 条。

### 验证

`verify-restore-gui.ps1` + `probe-restore-gui.mjs`：探针 20 项 + 驱动
13 项。走真实界面把 CLI 加密的目录树解出来，核对内容、空目录和四个不同
年份（2017/2018/2019/2020）的时间戳。

两条容易漏的断言：

- **原目录在加密后立即删除**。留着的话「还原出来的」和「原来就有的」
  分不清，一个什么都不做的实现也能让断言通过。
- **未选中时按钮不该出现**（反证）。否则一个永远显示的按钮点下去只会
  报 `empty_selection`。

素材用 CLI 加密而不是 GUI：本轮验的是还原，参照物要确定。GUI 加密那半边
另有 `verify-metadata-gui.ps1` 覆盖。

## 树形模式（逐个加密，模式 B）

文档 05 §3 的模式 B：保持目录结构逐文件加密，目录名也加密。与已有的容器
模式（模式 A）并存，用户加密文件夹时**必须明确选择**其中一个。

### 两种模式的取舍

| | 容器（模式 A） | 树形（模式 B） |
|---|---|---|
| 产物 | 一个 `.omy` 文件 | 一棵目录树 |
| 藏住什么 | 整棵树，连"有几个文件"都看不出 | 只藏文件名与内容 |
| 泄露什么 | 仅总体积 | 文件数、每个文件大小、树深度 |
| 改一个文件 | 重写整包 | 只重写那一个 |
| 云盘同步 | 每次全量重传 | 增量 |
| 单个文件分享 | 做不到 | 可以 |
| 损坏影响 | 头部坏了全丢 | 只影响一个文件 |

泄露的那部分是威胁模型 **N6（完全隐藏元数据）声明的非目标**，不是缺陷。
但必须在用户勾选那一刻就告知——加密完再说，原目录可能已经删了。

文案在 core 的 `TreeReport::leak_notice()` 里只有一份，CLI 与 GUI 都复用它。
措辞刻意具体：「泄露元数据」用户无法据此判断风险，「别人能数出你有多少
文件」才能。

### 目录名为什么必须 base32

文件名有 TLV 可以藏，**目录名没有**——它就是磁盘上的真实路径组件，只能把
密文编码进名字本身。

不能用 base64，两条都是硬伤：含 `/`（路径分隔符），且**大小写敏感**——
Windows 与 APFS 默认大小写不敏感，两个只差大小写的密文目录名会在磁盘上
撞车。而同一明文每次 nonce 不同、密文不同，只在偶然差一个字母大小写时才
炸，测试里几乎不可能复现。代价是膨胀 1.6×（base64 是 1.33×）。

长度限制会真撞上：密文是 `nonce(12)||ct||tag(16)` 再乘 1.6×，原始名只剩
约 130 字节，中文按 UTF-8 三字节算也就 40 多个汉字。超长走截断 +
8 字符哈希后缀，完整密文写进目录**内部**的 `.omy-name`（放目录内而非父
目录：整体移动时它跟着走）。

哈希后缀**必须算密文而不是明文**：算明文的话，同名的两个长目录会得到相同
后缀，观察者不解密就知道这两个目录同名——而藏住目录名正是这个模式的全部
意义。这条是变异测试第一轮唯一存活的缺陷。

### 一个固有约束

目录名密钥是 vault 级的（`INFO_DIRNAME`），所以**目录结构的可见性绑定整个
vault**：能解开 vault 的任一密码都能看到完整目录树，无法按 slot 区分权限。
这是模式 B 的固有限制，不说清楚会变成安全事故。

### 遍历必须与容器模式共用

`pack::walk_dir` 从 `pack_folder` 里抽出来给两个模式共用。必须共用的判定：
稳定排序、`symlink_metadata` 而非 `metadata`、跳过非普通文件、
`MAX_DEPTH=64`。各写一份的话，哪天给容器模式修了"跳过 socket"，树形模式
还按老样子，同一目录两种模式会得到不同的内容集合。

符号链接是个例外，但例外的方式恰好维持了这条约定：`walk_dir` 不自己决定
跳过，而是把链接目标交给调用方（`WalkItem::link_target`），因为两种模式
的存储能力不同——容器模式有索引可以记目标，树形模式每个条目是独立 `.omy`
文件、没地方记。判定规则（「这是不是链接」「目标是什么」）仍然只有一处，
只把处置权交出去。

重构顺带补了两个**原有测试的盲区**：

- `ordering_is_stable` 抓不到"漏排序"——它只比较两次打包结果，而两次都拿到
  同一个文件系统顺序，把 sort 整行删掉照样通过。它证明的是确定性，不是
  "按字节序排"。NTFS 实测返回 `a.txt, Z.txt`，而字节序应为 `Z.txt, a.txt`
  （`Z`=0x5A < `a`=0x61）。
- 符号链接测试是 `#[cfg(unix)]`，于是"改用 metadata 会跟随链接"在 Windows
  开发机上**压根没有覆盖**。创建符号链接需特权，但**目录联接（junction）
  不需要**，`mklink /J` 就能造，Rust 报成 symlink。

### GUI 侧

模式选择只在选中文件夹时出现，默认 container。默认值不做成"记住上次选择"：
两种模式泄露量不同，而"上次选了什么"是用户看不见的状态。

修了一个读代码时发现的缺陷：`adopt_credential` 从产物读 vault 参数来实现
"加密完立即可见"，但树形产物是**目录**，读它的前 256 字节只会失败，函数
静默返回——用户刚加密完的树立刻显示成锁定，然后被要求输入他三秒前刚打过的
密码。编译器和现有测试都发现不了，因为表现是"功能少了一点"而不是报错。

### 验证

`verify-tree-mode.ps1`（31 项，CLI 加密 + CLI 解开）与
`verify-tree-gui.ps1`（14+13 项，**GUI 加密 + CLI 解开**）。后者的参照物
刻意用 CLI：两头都用 GUI 的话，一个"两边都错得一样"的缺陷会通过。两个
脚本合起来才证明两个入口产出的格式真的互通。

脚本里第 3 节是**反证**：断言不解密也能数出文件数与目录层数。若哪天加了
填充或诱饵让它不成立，应同步改文档与 UI 文案，而不是删掉断言。

两个脚本首次都是全绿，所以各注入了变异自证断言有效（磁盘名改用原文件名
应报红、去掉事先的泄露告知应报红），确认能抓到后再还原。

## 浏览树形加密的目录

树形模式做完后，GUI 浏览侧完全不认识它的产物：用户看到一串 base32 目录名，
进去是一堆随机十六进制的 `.omy`，名字解不开也打不开——这个模式在界面里
基本没法用。这一节把它补齐：**UI 上能看出是加密目录，但访问方式与普通
文件夹完全一样**。

### 为什么加密目录仍然是 `is_dir=true`

只额外加一个 `is_encrypted_dir`，不新造条目类型。这样 `App.vue` 里
`if (entry.is_dir) navigate()` 那一支自动覆盖它，**双击进入不需要改一行
代码**。反过来若把它当成特殊条目，就得为它写第二套打开逻辑，而用户要的
正是「跟正常文件夹一样访问」。

`is_encrypted` 对它保持 `false`：那个字段的含义是「内容是密文」，而目录
没有内容、只有名字是密文。混用会让前端分不清要不要解密才能预览。

### 判定与解名分开

`looks_encrypted` 只看后缀与字符集、不试解密——列目录时每个目录都要过
一次，解密的开销不该出现在这条路径上。解名放在排序**之后**：排序按磁盘名
做（那是稳定的），若按解出的名字排，锁定与解锁两种状态下顺序会不一样，
列表会在输入密码的瞬间跳动。

解名静默失败、保留磁盘名。没密码时本该看不懂，这是正常状态不是错误；而
一个解不开的名字不应该让整个目录列不出来。

### 修掉的两个缺陷（同一个过时假设）

都源于「加密的东西 = `is_encrypted`」：

- **`vault_params_of` 只看当前层的文件**（`if !p.is_file() { continue }`），
  而树形产物是目录、文件都在里面。表现是站在密文树的**父目录**输密码，报
  「这个目录里没有加密文件」——可用户眼前明明有一个带锁的加密文件夹。而
  这恰好是最常见的位置：刚加密完，站在原地看产物。
- **`tryUnlock` 的成功判据只数加密文件**，于是只含树形目录的文件夹解锁后
  `opened=0`，被判成密码不对——用户输了正确密码却看到红字报错，而目录名
  其实已经解出来了。状态栏两个计数与搜索过滤有同样的漏判，一并修：不一起
  改会出现最难查的状态，解锁生效了、名字也解出来了，但状态栏说「0 个加密
  文件」。

### 断言必须查用户看到的那一层

`verify-tree-browse.ps1`（18+4 项）。变异测试 4 个缺陷第一轮第 4 个存活，
暴露的是断言的真漏洞：原来只查 `credential_count`（后端会话状态），而
`tryUnlock` 的判据只影响**界面反馈**。把判据改回漏掉加密目录后，密钥照样
进会话、名字照样解出来，只是界面同时弹红色「密码不正确」，探针全绿。补了
两条界面层断言后 4/4 全被抓到。

**这条值得记住：后端状态正确 ≠ 用户看到的是对的。**

### 探针自己的两个坑

- 把「点到了提交按钮」当成「解锁成功」。按钮被点到不等于生效，实测中
  `credential_count` 为 0 而那条断言依然 PASS，让一个真缺陷看起来只是
  「后面几条莫名失败」。
- 填值与点击写在同一个 `evaluate` 里。Vue 的 DOM 更新在微任务里批处理，
  `:disabled` 不会在同一同步块内刷新，click 被吃掉——表现是「密码填了但
  什么都没发生」，极像产品缺陷。**必须拆成两次调用**，中间留出刷新时间。

### 还没做

就地编辑：新增、覆盖、删除密文树里的单个文件。格式层面早就支持（每个文件
独立可解、独立可替换），缺的是界面入口。注意这与「rsync 式增量同步」不是
一回事——后者要求外部留一份明文副本，那等于让加密失去意义，且磁盘名是随机
uuid、头部没记 mtime，根本无法与源文件配对比对。

## 右键菜单与常规文件操作

在此之前应用里连删一个文件都做不到——加密、还原、改密码都有入口，但复制、
删除、重命名这些文件管理器的基本操作一个都没有。

这一节的起点是一次纠偏：我原本把「在密文树里增删改单个文件」当成树形模式
的专属缺口，用户指出那本来就是常规文件操作。确实如此——删掉一个 `.omy`
就是删一个文件，新增就是把加密输出写到那个目录。它们不需要任何树形专属
代码，需要的是通用入口。

### 对加密内容不做特殊照顾

删一个 `.omy` 与删一张 jpg 走同一条路径。密文树下之所以能增删改，正是因为
这里不区分：每个密文文件都是磁盘上独立的一个文件，删它不需要密码，也不用
通知任何索引。

唯一的例外是**重命名密文目录**。目录名本身就是密文，改掉它等于把那段密文
扔了——里面的文件全在，但名字永远解不开，没有任何办法恢复。所以拦在操作
之前，而不是让用户事后才发现。

### 校验是拒绝，不是净化

`fileops::is_valid_filename` 与 `unpack::sanitize_filename` 目标不同：后者
处理来自密文内部的不可信输入，必须容错地清洗；前者是用户输入，非法就该明确
报错让他改。**不拦 `../` 的话，重命名框就是一个任意路径写入**——变异测试里
把这段删掉后，后端真的在工作目录的上一级建出了「逃逸」。

一并拒掉几个 Windows 上会被系统静默处理的名字（以点或空格结尾、`CON` 等
设备名）。不拦的表现是「改名成功了，但名字不是我输的那个」。

### 交互上的两条防误删

- **右键先选中该项**，已在多选里的不动。在未选中的条目上右键、操作却作用于
  之前选中的东西，是最容易造成误删的交互。
- **不可用的项禁用而非隐藏**，原因写进 title。位置固定用户才能形成肌肉记忆；
  每次右键换一套项目会让人反复找「删除在哪一行」。

菜单定位要夹进视口，越界时翻到触发点的另一侧而不是简单减去溢出量——后者会
让菜单盖住用户刚点的那个条目，而靠右下角右键时被裁掉的往往正是最后一项。

### 触屏：长按不能只做加选

移动端没有右键，长按原本只做加选，意味着**手机上完全没有办法删除或重命名
文件**。现在长按先选中再弹同一套菜单。

### 变异测试暴露的三条断言漏洞

第一轮 4 个变异存活 3 个，问题全在断言自己：

1. **只查前端不算验证**。「提交按钮禁用」是前端自己算的，把后端校验整段
   删掉，前端照样拦得住、探针全绿。而后端才是真正的防线：任何不走对话框的
   路径都绕过前端。补了一节直接 `invoke` 命令的断言，并配反证（合法名字
   必须被接受），否则「什么都拒」也能让前面几条通过。
2. 「加密目录不能重命名」同样只查了菜单项禁用。
3. **「删除后有提示」被一条过期提示满足**。提示条 4 秒后才消失，而探针从
   重命名走到删除只用了约 2.8 秒——屏幕上还挂着上一步的「已重命名」。断言
   只问「有没有提示」就通过了。改成核对提示**内容**。

### 探针与变异脚本自己的坑

- **报「CDP 未就绪」而端口明明是通的**：我按 `try {` 做锚点插代码，命中了
  重试循环里那个 `try`，导航代码被塞进 fetch 之前，外层 `catch {}` 把
  ReferenceError 静默吞掉。**到处都有的片段不能当锚点。**
- **`subprocess` 用 `shell=True` 在 Windows 上走的是 cmd.exe**，不认
  PowerShell 的 `&`，验证脚本压根没跑——于是「输出里没有 FAIL」被判成
  「缺陷存活」，4 个变异全显示存活，看着像断言集体失效。
- **判断行尾必须读字节**。文本模式下 Python 的 universal newlines 会把
  `\r\n` 翻成 `\n`，于是 CRLF 文件被误判成 LF、锚点 0 命中。
- **工作目录之外的残留要清**。路径校验失效时建出的「逃逸」在上一级目录，
  `finally` 只清工作目录，残渣会让此后每次运行的反证断言都失败，且失败
  信息指向产品。

### 还没做

复制、剪切、粘贴、移动；多选批量重命名。删除目前固定走系统回收站或永久
删除两项，没有「删除到指定位置」之类。
## 给树形加密的目录换密码

单个 `.omy` 文件的密码增删改早就有了，但选中一个树形加密的**目录**时，
🔑 按钮压根不出现，右键菜单里的「密码管理」也是禁用态。用户得不到任何提示，
也无从知道这件事到底能不能做——静默不可用比明确报错糟得多。

CLI 那边同样：对着一棵树跑 `omy key change`，`fs::read` 会返回一个含糊的
IO 错误（Windows 上是「拒绝访问」），看不出这是「目录要用别的方式」还是
「文件坏了」。

### 只重建 slot 区，不重新加密

复用 `keyslot::rewrite_slots`：它只重建 384 字节的 slot 区与 32 字节头部
MAC，载荷密文与 `file_uuid` 逐字节不变。用「解密后重新加密」代替是错的——
会换掉 `file_uuid` 与 `base_nonce`，让增量备份重传整份文件，大文件中途失败
还可能同时失去新旧两份。

整棵树共用一个 `vault_salt`（实测确认：3 个密文文件的 salt 与 Argon2 参数
完全相同，而 `file_uuid` 各不相同），所以 **Argon2 只派生一次**。否则 1000
个文件就是 1000 次 KDF，要跑好几分钟。

### 目录名必须一起改——这是最容易漏掉的一半

我最初的实现只改文件。写完之后有一条测试报 `ContentHashMismatch`，我一度
以为是测试写错了。补上**基线对照**（加密后、还没换密码就先测一遍）才定位
清楚：那一次确实是我传错了 cipher 参数（硬写 `Aes256Gcm`，而
`EncryptOptions::default()` 是 `ChaCha20Poly1305`，目录名用错算法解密自然
认证失败）。

但修掉参数之后，真正的缺陷露了出来，而且比预想的严重：

```
改密码后  新密码 → 打得开文件，解不开目录名（进不去）
          旧密码 → 解得开目录名，打不开文件（进去了打不开）
```

**两个密码都无法完整访问这棵树。** 目录名由
`DirnameKey::derive(keks[0], vault_salt)` 加密，KEK 一换密钥就变了。这比
「不能改密码」糟得多，所以两者必须在同一个操作里完成。

改名自底向上做：先递归处理完子目录，回来再改它自己的名字。反过来的话，
重命名之后手里的路径就失效了。根目录名最后改，新路径通过
`RekeyReport::root` 返回——前端必须用这个值，否则刷新后指向一个不存在的
目录，表现为「改完密码文件夹不见了」。

密钥没变时完全不碰目录名。`add` 保留原密码作第一个 KEK 时正是这种情形，
无谓改名会让调用方以为整棵结构变了，增量备份还要重传全部目录项。判据是
拿固定 nonce 加密同一个探针名字比对密文——AEAD 在密钥、nonce、明文都相同
时输出确定。

### 一棵树只能有一个密码

`add` 是实测发现不能支持的：给树加第二个密码后，新密码能打开每一个文件，
却解不开目录名，`decrypt` 报 `content hash mismatch`——**用户会以为文件
损坏了**。根因就是上面那条，目录名只认第一个 KEK。

与其给出一个半残的密码，不如明确拒绝，并指向 `key change`。`remove` 同理。
`reencrypt` 也拒绝：要重写整棵树的载荷，中途失败会留下一半新一半旧，单文件
上这个风险是有界的，整棵树上不是。

### 部分失败如实报告，不回滚

三条路都想过。回滚要留 N 份原始字节（几十 GB 的树做不到），而且回滚本身
也会失败；假装成功最糟——用户以为全换了，等哪天用新密码打不开另一半时早就
忘了旧密码。

所以如实报告，并且退出码不为 0。这么做的前提是**旧密码不会失效**：用户
手上一定有能打开剩余文件的密码，把新旧都提供给命令就能补齐。

### 验证与变异测试

CLI 30 项、GUI 31 项，变异 4/4 全抓到（不改目录名、入口退回只认
`is_encrypted`、后端不拒绝 add、不更新选中项）。

端到端验证抓到两个只查代码发现不了的缺陷：

- **i18n 新键全部无效**。语言包是嵌套结构（`keymgmt` 是个对象），我按扁平
  的 `"keymgmt.tree_warning"` 写，运行期取不到，界面直接显示键名本身。
  `check-i18n-keys.py` 没抓到是因为它只比中英两边键集合是否一致——两边我
  加的是同样的错键，于是显示「全部齐了」。
- **改完密码选中项丢失**。我把 `state.selected` 设在 `reload()` 之前，而
  `reload -> navigate` 里有 `state.selected = []`，赋值被冲掉。

探针本身踩的坑（都写进了文件头）：不能 `import('/src/store.js')`（release
构建里 `/src/` 不存在，dev 下也是另一个模块实例）；「🔑」也出现在凭据指示器
「🔑 未输入密码」上，按文字找会点开解锁对话框；解锁后列表显示的是解密后的
原名，按密文名同样找不到；判断改名要比对后端返回的真实路径，看界面显示名
会得到一个恒真的假通过。

### 还没做

GUI 上没有给树提供 `reencrypt`（轮换 FEK）。整棵树重写载荷的耗时与中断
风险都比单文件高一个数量级，需要先定清进度上报与中断恢复的语义。

部分失败目前只报「有文件没改成」，没有在界面上列出是哪些。
## 轮换整棵树的文件密钥，与部分失败的交代

上一节把密文树的改密码补齐了，留下两个缺口：GUI 上没有轮换（换掉文件密钥
并重写载荷），以及部分失败只报一句「有文件没改成」、不说是哪些。

### 为什么改密码不够

改密码只重建每个文件 384 字节的 slot 区，**FEK 没有变**。这意味着：攻击者
手里若有旧文件的副本，仍能用旧密码打开那个副本。只有换掉 FEK 并重写载荷，
才能让旧密码与这份数据彻底无关。

已经流出去的副本仍是独立的密文，本操作对它无能为力——它永远可以被当时的
密码打开。这一点必须写进警示，否则用户会以为轮换等于「把发出去的东西收
回来」。

### 之前拒绝它的理由站不住

CLI 里原本明确拒绝对目录 reencrypt，理由是「中途失败会留下一半新一半旧」。
重新查过之后这个顾虑不成立：每个文件各自 `write_atomic`，任一时刻每个文件
要么是完整的旧密文、要么是完整的新密文，不存在写坏一半的文件。

若同时换了密码，部分失败留下的是「一半新密码、一半旧密码」——如实报告是
哪些文件，两个密码都提供就能重跑补齐。而轮换**可以不换密码**（「让旧副本
作废、密码不变」是它的正当用法），那时连这个状态都不会出现。

### 进度：文件内的字节也要报

只报「第 i / n 个文件」是不够的。轮换一个 4 GB 的视频要几十秒，进度条会
长时间停在同一格，用户无从判断是在跑还是卡死了。所以回调带上文件内的
字节进度，读一半写一半合成一条 0→100 —— 分成两段各自 0→100 的话，用户
看到的是「跑到 100% 又归零重来」。

密钥没变时完全不碰目录名，`change` 与 `reencrypt` 共用这条判断：目录名
密钥由密码派生，密码没变就没有改名的必要，无谓改名会让备份工具把整棵树
当成全新文件、全量重传。

### 实测抓到的三个真缺陷

**1. `--new-password-file` 被静默忽略。** `modify_tree` 用
`needs_new_password()` 决定要不要读新密码，而它对 `reencrypt` 是 false
（轮换不强制换密码）。于是 `key reencrypt <目录> --new-password-file X`
里的 X 被完全忽略，命令报成功、退出码 0，密码其实一点没变——用户要等到
下次用新密码打不开时才发现。单文件路径本来是对的（多一条「显式给了来源
就读」），写目录分支时漏抄了那一支。现在抽成一个共用判定，不会再分叉。

**2. 样本文件不检查头部，错误随枚举顺序变化。** 同一棵树、同一条命令，
两次运行给出不同的错误：一次正确地点名坏文件，一次报「文件已损坏」像是
整棵树都毁了。根因是 `find_any_file` 返回目录枚举顺序里第一个 `.omy`，
不检查它是不是真的密文。改成两级：有能解析的就用；全都解析不过才退回第
一个，让调用方如实报「已损坏」；压根没有 `.omy` 才报「不是密文树」。
**「坏了」和「不是」指向完全不同的排查方向**，不该压成同一个结果。

**3. `reload()` 把刚设好的错误冲掉了。** 为了让用户看到改名后的新位置，
我在 catch 里加了刷新，而 `reload -> navigate` 里有 `state.error = ''`，
错误和失败清单一起被清空——对话框开着却什么都不显示，看起来像「什么都
没发生」。这与上一节「选中项被 reload 清掉」是同一类顺序缺陷：**任何写
state 的动作都必须排在 reload 之后**（这次是刷新要排在设错误之前，同一
件事的两种说法）。

### 失败清单

后端把清单放进错误参数，界面在对话框里逐条列出，限高滚动——一棵大树可能
有上百个文件失败，全列出来会把对话框撑到屏幕外，连关闭按钮都够不着。

挂在对话框上而不是底部提示条：提示条 4 秒后自动消失，用户还没读完就没了。
`i18n.te()` 只按码取文案、完全不做插值，所以清单必须单独传，塞进 params
会被静默丢掉。

只给文件名不给全路径：路径里含密文目录名，又长又无意义。

### 变异测试顺带体检了探针

4 个变异全部被抓到（rotate 被忽略、后端不拒绝 add、失败清单缺失、前端不给
轮换选项），但第一轮跑下来「失败清单缺失」是**存活**的——因为压根没有断言
碰过清单。补断言时才发现要测这条路径必须真的制造出部分失败：往树里塞一个
损坏的 `.omy`，它通不过 open，其余文件正常。

补完之后 M3 被抓到了，同时暴露出探针自己的一个缺陷：
`JSON.stringify(undefined)` 返回的是 `undefined` 而不是字符串，对它
`.slice()` 会抛异常。而这恰好发生在**断言失败**的路径上——探针在本该报告
失败的地方崩掉，后面的断言一条都不跑。正常全绿时完全看不出来，只有变异
测试会暴露。

探针新踩的两个坑：选中列表行必须派发冒泡的 `MouseEvent`，`.click()` 选不
中（界面看着正常却什么都没选上）；纯后端 invoke 改完目录名后界面列表是
过期的，那一行的 path 指向已不存在的目录，点它选不中、工具条也就不渲染。

### 还没做

部分失败时界面列出了文件，但没有提供「只重试失败的那些」的入口——目前要
重跑整个操作（已改好的会因为旧密码打不开而报错跳过，所以是安全的，只是
多等一遍）。
## 重试：只补失败的那些文件

上一节留了个缺口：部分失败时界面列出了是哪些文件没改成，但用户拿它没办法，
只能重跑整个操作。

### 为什么不能靠「重跑一遍」

重跑会拿旧密码去开已经改成新密码的文件，报一堆假失败——用户分不清哪些是
真问题。轮换时还要把已处理过的文件再读写一遍，几十 GB 的树要多等一倍。

所以 core 里新增一个只处理给定清单的入口，不遍历、不碰目录名（改名在第一次
就完成了，再改一次等于把整棵树的名字又换一遍）。

### 先撞上一个真缺陷：报告里的路径全是失效的

探针实测发现，部分失败时报告里每一条路径的 `exists()` 都是 **false**。
原因是失败在改名**之前**记下，之后父目录被逐层改名，那些路径全部指向不
存在的位置。

后果具体而恶劣：CLI 把一串不存在的路径打印给用户，让人拿去排查一个根本
找不到的文件；界面也没法按它重试，每个目标都是「文件不存在」。

修法是改名成功后立刻重写已记录的失败路径前缀。改名自底向上做，所以每次
rename 之后修一遍，前缀总是当前那一层。

### 密码要留在内存里

重试必须用**原来的**密码——失败的文件没被改写，还是旧密码。但用户刚输过
新密码，让他自己回想很容易填错，于是把上次那次操作的密码留在内存里，点
重试不用再输。代价是密码多待一会儿，所以对话框一关就清。

仍有失败时不关对话框，并把清单缩小到还没成的那些：关掉那份清单就没了，
用户只能重新走一遍整个操作才知道还剩什么。

### CLI 的建议也改了

原来提示「把新旧两个密码都提供给本命令」重跑整棵树。失败的都是独立的
`.omy` 文件，对报出来的路径直接用单文件命令改一次就补齐了，实测确认补齐后
整棵树能正常解密。路径修好之后可以直接复制粘贴。

### 又一个顺序缺陷

重试失败时只同步了失败清单、忘了错误文案，于是对话框里显示的还是**上一次**
的错误，而清单已经换成这次的——界面上出现「有错误、但没有失败文件」这种
自相矛盾的状态，用户完全判断不出这次重试到底怎么了。

这是本项目第三次踩同一类坑（前两次是选中项被 reload 清掉、错误被 reload
清掉）。**凡是「一次操作要更新多个状态」的地方，都得检查是不是全都更新了、
顺序对不对。**

### 变异测试：四个全部假「存活」

第一轮四个变异全部「存活」，看着像断言完全无效。实际是**残留的 omy-gui
进程占着调试端口**——每次验证都连到那个旧进程，跑的始终是变异前的二进制。

这个假象极难识破：编译确实重做了，脚本确实跑完了，断言也确实执行了，只是
探针连错了目标。唯一的线索是去查端口占用。变异脚本现在每次构建前后都强制
清进程。

清掉之后又有两个「存活」，核查后确认还是**变异写错了**，不是断言不足：一处
删的是重试自身的错误分支，而断言依赖的是首次失败那处；另一处用旧密码去改
已经改好的文件，必然失败且错误被丢弃，等于空操作。空操作当然测不出东西。

最终 4/4 全部抓到。

### 探针这次学到的

制造部分失败改用「把树里真实文件设为只读」，不再塞自造的密文文件。原因是
**每次独立加密都会生成新的 vault_salt**，自造的文件不属于这棵树，重试必然
报密码错误——那是假数据造成的，却极像产品缺陷。只读文件 open 成功、写回
失败，正是真实场景（被别的程序占用、权限不对）走的那条路径，而且解除只读
后重试必然能成功。

密码链要按操作顺序推演清楚。原来第 9 段用后端造完部分失败就直接让界面提交，
但那时树里混着两个密码（卡住的那个文件还是旧密码），界面抽到的样本可能正是
它，于是整个操作先撞密码错误，根本走不到部分失败。

加密产物的文件名由 uuid 生成，不是「原名 + .omy」，要从返回值里取真实路径。
猜名字的表现是「文件没生成」，其实生成得好好的，只是不在猜的位置。

## 符号链接还原（2026-09-06）

容器格式本来就有 `EntryKind::Symlink` 和 `target` 字段，`add_symlink` 也
写好了，但存取两端都跳过——设计好的能力一直没接上，用户的链接在加密后
无声消失。这轮把它接通。

### 判定规则仍然只有一处

`walk_dir` 被容器模式与树形模式共用，而两者对链接的存储能力不同：容器
模式有索引可以记目标，树形模式每个条目是独立 `.omy` 文件、没地方记。

做法是 `walk_dir` 不再自己决定跳过，改为把目标放进 `WalkItem::link_target`
交给调用方。判定（「这是不是链接」「目标是什么」）还是只有一处，只把处置权
交出去——容器模式存进索引，树形模式仍如实报成跳过。

### 最容易写错的一处：让链接落到读文件的分支

`std::fs::read` 会**跟随**链接，把目标的内容当成链接自身的内容存进去。
解出来内容看着完全正常，只是类型从链接变成了普通文件，不比对类型根本
发现不了。两种模式都在读文件之前显式判断，测试用载荷字节数锁住——被
跟随的话内容会出现两次。

### 还原分三类，而不是笼统报失败

沿用 `restore.rs` 既有的 unsupported / failures 分界：

| 情况 | 归类 | 理由 |
|---|---|---|
| 建成功 | `links` 计数 | — |
| 权限不足 | `unsupported` | 环境的既有事实，换机器能还原 |
| 其他错误 | `failures` | 这次的问题，重试可能就好 |

把权限不足报成 failure 会让人反复重试一件不可能成功的事。实测本机（未开
开发者模式、非管理员）建任何链接都返回 `ERROR_PRIVILEGE_NOT_HELD`（1314），
而**它并不映射成 `ErrorKind::PermissionDenied`**——只看 `ErrorKind` 会漏判，
所以 `is_privilege_error` 直接查 `raw_os_error`。

### 链接必须在所有文件目录落盘之后才创建

Windows 分 `symlink_file` 与 `symlink_dir` 两个调用，判断依据是**目标当前
是不是目录**。而容器里的链接完全可能指向索引里排在它后面的条目——放在同一个
循环里处理，创建时目标还没落盘，`is_dir()` 对不存在的路径返回 false，于是
指向目录的链接被建成文件型：能建成功但不可用，比直接失败更难查。

所以链接单独走最后一遍。已实测确认建链接时目标确实已经在磁盘上。

### 顺带堵上一个逃逸口

`safe_join` 只管住了链接**自身**落在哪，完全没看它**指向**哪。恶意容器可以
放 `link -> C:\Windows` 或 `link -> ../../../etc/passwd`，用户在文件管理器里
点进去就在容器外面操作了，而他以为还在解出来的目录里。

现在目标解析后不在容器内就拒绝并报告。**不改写成安全路径**：改写会得到一个
指错地方却看着正常的链接，比不建更难发现。

判断用纯路径推演而不是 `canonicalize`，两个原因：后者对目标不存在的悬空
链接直接报错，会把所有悬空链接误判成逃逸；而且它会跟随路径上已存在的链接，
留下「检查时」与「使用时」看到不同结果的空子。绝对路径一律拒绝——即便字面
上指回容器内，它也写死了本机位置，换机器解开必然指向不存在的地方。

### 开了开发者模式之后补的验证，抓到一个真缺陷

一开始「成功创建链接」那条分支在本机执行不到（未开开发者模式，所有调用都在
权限判断那里就返回了）。开发者模式打开后补跑，非管理员也能建链接，于是抓到
一个不跑就发现不了的缺陷。

#### 目标带子目录的链接在 Windows 上建出来就是坏的

容器里的路径一律用 `/` 分隔，而 **Windows 不接受符号链接目标里的正斜杠**。
形态很坏：链接照样创建成功，`is_symlink` 为 true、`read_link` 读得回来、
`links` 计数照报成功，只有跟随时才报 `winerror 123`（`InvalidFilename`）。
磁盘上于是留下一个看着存在、点开就报错的坏链接。

受影响的是任何目标带子目录的链接（`../lib/libfoo.so` 这类很常见），尤其是
在 Unix 上打包、拿到 Windows 解开的容器。创建前把 `/` 换成 `\` 即可。

#### 类型选择是必要的，而原先的判据抓不到它

变异测试把 `symlink_dir` / `symlink_file` 对调后：链接仍能创建，但
`metadata` 跟随时 `dlink` 报 permission denied、`flink` 报 not a directory，
读文件直接「拒绝访问」——正是那种「能创建但不可用」的产物。所以那段
`is_dir()` 判断不是多余的保险。

但同一个变异下，「透过 `dlink` 读 `sub/inside.txt`」**照样成功**：Windows
沿链接**路径**穿越时并不校验这个类型标记。而原先的测试恰好拿它做判据，等于
没测。判断链接是否健康只能对链接自身取 `metadata`。

顺带把 `link_to_dir_inside_container_gets_right_type` 从 `cfg(unix)` 放开到
两个平台——它测的本来就是 Windows 才有的类型选择，之前恰好不在 Windows 上跑。
建不出链接的环境自动跳过后半段。

#### 已实测通过的完整链路

真目录树（含指向文件、指向目录、多层目标、绝对目标四种链接）→ 容器 → 解开：

| 链接 | 结果 |
|---|---|
| `l_file -> plain.txt` | 文件型正确，读到 5B |
| `l_dir -> sub` | **目录型正确** |
| `l_deep -> sub/deep.txt` | 存成 `sub\deep.txt`，跟随成功 |
| `l_abs -> C:\...\plain.txt` | 如实拒绝（`symlink_escapes_root`） |

载荷是 `"plaindeep"`——只有两个真文件的内容，确认链接没被跟随。

另外确认了两件事：Windows **原样保存相对目标**，只对绝对目标加 `\\?\`
前缀，所以相对链接能原样往返；悬空链接在有特权时也能正常创建，不需要为它
推迟或跳过。

### 一个还没定的取舍

**指向容器外的绝对路径链接目前一律不还原。** 这在安全上是对的（否则解开
一个来路不明的容器，里面的链接可以指到 `C:\Windows`），但用户目录里确实
可能有自己建的绝对路径链接，那些现在会被报成 `symlink_escapes_root` 而丢掉
——只是有报告，不是静默丢弃。

要不要给「加密自己的目录」这种场景留一个开关，还没定。

## 下一批四项能力的现状核查（伪装 / 多密码 / 恢复码 / 生物识别）

动手前先把这四项在现有代码里的真实状态查了一遍。结论是**它们不是四个
平行的新功能**，成熟度差了整整三档，而且有一项的地基正卡着另外两项。

| 能力 | 格式层 | core | CLI | GUI | 真实缺口 |
|---|---|---|---|---|---|
| 多密码 | ✅ 8 槽 | ✅ 完整 | ✅ 可用 | ✅ **已完成（本轮）** | — |
| 恢复码 | ✅ 复用 slot | 🟡 一个孤立函数 | ⛔ | ⛔ | 词表 / 校验和 / 入口全缺 |
| 伪装 | 🟡 只占了号 | ⛔ | ⛔ | ⛔ | 整条链路，且要动两处热路径 |
| 生物识别 | ✅ 复用 slot | 🟡 只有枚举 | ⛔ | ⛔ | 桌面要自研原生插件 |

### 多密码：core 早就做完了，卡在 GUI 会话层

> **本轮已解决**，实现细节见下文「多密码：一个会话同时装多个密码」。
> 以下保留当时的诊断，因为它解释了为什么不能简单地把 label 放开。

`slot::build_slot_area` 接受最多 8 个 KEK，`keyslot::rewrite_slots` 的原地
增删改也在（只重建 384 字节 slot 区 + 32 字节 MAC，载荷逐字节不变），
CLI 的 `encrypt --add-password` 与 `key add/change/remove` 都能用。

真正缺的是**会话能同时持有多个密码**。`SessionKeys` 的缓存键是
`(vault_salt, kind, label)`，而 GUI 四处调用点全把 label 写死成 `"main"`
（`commands::unlock`、`unlock_directory`、`encrypt`、`keymgmt` ×3）——
第二个密码会直接覆盖第一个。

这个写死不是疏忽，是上一轮为修「同一个密码被算成两条凭据」而**刻意**做的
（见上文「`label` 让同一个密码被算成两条凭据」）。所以多密码不能简单地把
label 放开回去，否则那个缺陷会原样回来：必须换一种既能区分不同密码、
又不会把同一个密码算两次的判据。

注意 `commands::unlock` 里那套 `main#0` / `main#1` **不是多密码**，它解决
的是「一个目录里混了多个 vault（多个 salt）」，同一个密码在每个 salt 下
各派生一次。

### 恢复码：只有一个孤立函数

`session::kek_from_recovery_words` 存在，但它只做了「词序列归一化后
SHA-256」，函数自己的文档就写着「真正的 BIP-39 校验和验证由上层实现」。
没有词表、没有校验和、没有生成函数、没有任何 CLI/GUI 入口，也没有把它
写进 slot 的路径。

几条已经定下、实现时不要再推翻的：

- **不需要 Argon2**。256 bit 随机熵，慢速 KDF 只是白费时间。
- **校验和是必需的**，不是锦上添花：没有它，「手抄错一个词」和「这不是
  本库的恢复码」表现完全一样，而这两种处境的处置方式相反。
- `CredentialKind::Recovery` 的 `scannable()` 是 false（已实现），所以输完
  恢复码**列表仍然显示锁定**，只有手动打开单个文件才会尝试。这点必须在
  UI 说明，否则看起来像恢复码没生效。
- 恢复码是整个 vault 的强度下限，它是一张明文纸条；它与可否认性冲突。
  两句都要进 UI（文档 03 §8.3）。

### 伪装：core 待办里仅存的那个 `[ ]`，且有两处会咬人

格式层留好了坑位（`flags::DISGUISED` 是 bit 6、`TLV DISGUISE_INFO`
= 0x000F，`is_known` 已认），零实现。功能本身不难，难的是两处既有假设：

1. **`scan::probe_file` 的快速排除路径**是「前 8 字节不是 magic 就
   `return Ok(None)`」。伪装文件开头是 `FFD8FF`，**扫描根本扫不到它**。
   补尾部探测会给每个非 omy 文件多一次 seek+read，一万个文件的目录代价
   不可忽略——应只对已知宿主扩展名做。
2. **`source` / `keyslot` 把 `header_len` 与 `SLOT_AREA_OFFSET` 当文件
   绝对偏移用**，伪装必须引入 base_offset。漏改的表现是
   `ChunkAuthFailed`，而这个错误的文案指向「数据损坏或被篡改」——上文
   「`payload::read_range` 的 `fetch_ct` 偏移语义」已经记过一次同类事故，
   它会被误判成文件坏了或密码不对。

强度边界照抄文档 03 §6.4：只挡随手点开的人，`binwalk` / `file` /
任何十六进制编辑器一眼看穿，不是隐藏卷。

### 生物识别：摊子最大，且 Windows 有个必须处理的安全边界

先纠正那个误解：**生物识别不产生密钥**，指纹是门禁不是钥匙。

- Tauri 官方 `tauri-plugin-biometric` **只支持 Android/iOS**，
  Windows/macOS/Linux 明确列为不支持，桌面必须自研。
- Windows Hello 的 `KeyCredentialManager` 在 windows-rs 里有绑定，但它
  **只能签名、没有解封装 API**，只能走 sign-then-derive（固定 challenge
  签名 → 哈希成密钥）。
- **最关键的一条**：微软官方答复确认，非打包桌面应用的 KeyCredential
  **只按用户隔离，不按应用隔离**——同一用户下任意进程 `OpenAsync("同名")`
  都能拿到同一凭据、签出同一结果（提问者实测三个不同发布者的应用互相
  可见）。所以**绝不能把签名直接当 KEK**，必须与本机存储的一份随机熵
  混合，否则用户按一次 Hello，同机任何程序都能解开整个库。
- Android Keystore 的 `setUserAuthenticationRequired(true)` 密钥在改锁屏
  密码、增删指纹时不可逆失效，这是设计不是缺陷 → 所以它是便利性，
  **不是恢复手段**，必须永远保留密码 slot 作后备。

### 推进顺序：多密码 → 恢复码 → 伪装 → 生物识别

理由是硬的，不是偏好：**会话只认单 label 这一条是后三项共同的地基**。
恢复码和生物识别做出来的效果都是「往会话里塞第二种凭据」，会话层不支持
多凭据时，它俩即使实现了也看不出效果，还会撞上「装进去把主密码挤掉了」
这种极难排查的表现。

伪装与密钥体系正交，可以并行，但它要动 `scan` 与 `source` 两条热路径，
应单独一个提交序列。生物识别放最后：自研原生插件 + 三套平台语义 +
上面那条凭据隔离缺陷，是四项里唯一需要先做 spike 的。

## 多密码：一个会话同时装多个密码（本轮，已完成）

上面那条「会话只认单 label」已经解决。core 的 8 槽、slot 增删改本来就
完整，本轮补的是会话层。

### 判据换成 KEK 指纹，而不是把 label 放开

不能简单地把写死的 `"main"` 放开回去。那个写死是上一轮为修「同一个密码
被算成两条凭据」刻意加的，放开就等于把旧缺陷请回来。

两个缺陷其实是**同一个根因的两面：拿名字当身份**。名字在两个方向上都
不成立——同一个密码可以被起两个名字（计数虚高），不同密码也可以重名
（后者静默覆盖前者）。

改用 `Kek::fingerprint()`：HKDF，salt 取 `vault_salt`，info 用独立的
`omy/v1/kek-fingerprint`。它是密码在本库下的稳定标识，两个方向都对。
`add_password` 按它判重，label 降级为纯显示名，重名时自动加 `#2` 后缀
让开而不是覆盖。

**指纹只许留在内存。** 落盘等于给攻击者一个可离线比对的口令摘要：他能
据此判断两个库是不是同一个密码、能拿字典逐个比对候选，而 Argon2 那几
百毫秒的代价在这里完全不起作用（KEK 已是派生结果，指纹只是一次 HKDF）。

同理，判重**必须**在派生之后做。想省掉那次 Argon2 就得在派生前算一个
快速的密码哈希并驻留内存，那正好把上面这条漏洞制造出来。有一条测试
断言 `kdf_runs == 2`，锁住这个不能做的优化。

### 顺带修掉三处同源隐患

`encrypt` 与 `keymgmt` 原先也是替换语义。它们的后果比「多密码用不了」
更糟：一个库里十个文件都用密码 A，用户改其中**一个**文件的密码，会话里
的 A 被顶掉，另外九个当场全部锁上——用户只想改一个文件，却发现整个目录
都看不见了。

`browse::annotate_tree_names` 原先取 `keks.first()` 就收工。单密码时看不
出问题，装多个之后它错得很难查：`all_for` 底层是 `HashMap`，**返回顺序
每次运行都可能不同**——同一个目录，这次进去目录名解开了，下次又变回一串
base32，用户会以为是随机的界面 bug。改成每个 KEK 各派生一个目录名密钥
逐个试。

### 变异测试：两条第一版写成了空操作

core 侧 6 个变异最终全部被抓到，但第一轮有两条「存活」，查下来是**变异
本身等于空操作**，不是断言不足：

- 把指纹的 info 换成 `INFO_SLOT`——slot 密钥用 `file_uuid` 作 HKDF salt，
  指纹用 `vault_salt`，本来就撞不上。真正的碰撞面是 **dirname 密钥**，
  它同样用 `vault_salt`，只有 info 串不同。
- 去掉 `add_password` 里的 `k.vault_salt == vault_salt` 检查——指纹自己
  已经绑定 vault，去掉它也不会误判。

判断一个变异有没有意义，要问「它真的改变了程序的可观察行为吗」。

第三条是真的断言缺口：「跨库指纹不同」那条断言用的是两个**不同 salt 派生
出的 KEK**，KEK 本身就不同，哪怕 `fingerprint` 内部把 salt 写死成常量也
照样通过。补了一条拿**同一个 KEK** 问两个 salt 的断言才锁住。

### 端到端验证（`spikes\verify-multipw.ps1`，21 项）

core 的 6 项集成测试直接调 `add_password`，绕过了「对话框 → invoke →
`commands::unlock` → scan → 列表 → 渲染」整条链。变异测试确认：把 GUI
侧改回 `unlock_password`，探针如期变红（core 测试则仍全绿）。

两个要点，不这么做测试就是假的：

1. **素材必须造在同一个 vault 里**（用 `--vault` 复用 salt）。否则测的是
   早就支持的「多 vault」，必然假通过。
2. **判据是界面上具体哪两个文件名同时可见**，不是数量。退回替换语义时
   数量仍然是 1，只数个数会漏掉「顶掉一个又解开一个」。
   另外反向确认 `pw-alpha` 打不开 `b.omy`，排除两个文件其实挂了同一批
   密码这种让整个测试失去意义的情形。

### 探针自己的坑：invoke('lock') 不等于点锁定按钮

第一版最后一项报「锁定后仍看得见文件名」，看起来像信息泄露这种严重缺陷。
实际是探针直接 `invoke('lock')`，而后端的 `lock` 只抹密钥，界面那份列表
是 `store.lock()` 里的 `reload()` 重新拉的。绕过产品代码测出来的「缺陷」
不是缺陷。改成点界面上真正的锁定按钮后通过。

这已经是本项目第 N 次「探针 bug 伪装成产品缺陷」，规律很稳定：**凡是断言
报出「安全相关的严重问题」，先确认探针走的是不是用户真实走的那条路径。**

### 界面上必须说出来的一件事

后端是累加语义，但用户没理由知道——大多数应用的密码框都是「重来一次」。
所以会话里已有密码时，解锁框换一句提示明说「再输一个不会顶掉现有的」，
否则想同时看两批文件的人根本不会想到可以再输一次。

重复输入同一个密码时提示「这个密码已经在用了」，而不是「解锁了 N 个
文件」——后者配上纹丝不动的凭据计数，看起来像计数坏了。
`UnlockResult.added` 就是为这个区分加的。


## 槽位模式：一个实测缺陷引出的格式扩展（本轮，仅文档）

做恢复码前先问了一句「8 个槽能不能识别哪个是恢复码」，查下去发现这不是
一个设计偏好问题，而是一个**会静默毁数据的真缺陷**。

### 实测：改密码会抹掉恢复码

```
给文件挂「主密码 + 恢复码」，两个都验证可用
  ↓ omy key change（只提供当前密码与新密码）
生效密码数 1
  ↓
新主密码：可打开        恢复码：打不开
```

用户改一次密码就永久失去恢复码，而他只会在真正忘密码那天才发现。
CLI 确实有一句「其它密码将失效」的确认，但那太笼统——刚设完恢复码的
用户不会把这句话和「我的恢复码」联系起来。

### 关键发现：保住一个槽，不需要先认出它

一度以为只能二选一：要么放弃可否认性去标记槽位，要么接受恢复码会被抹。
写了个探针才发现第三条路——**原样搬运**：

```
常规 rewrite_slots：      新密码可开 = true，恢复码可开 = false
把 slot 1 原样搬回+重算 MAC：新密码可开 = true，恢复码可开 = true
                            旧主密码可开 = false，明文正确
```

成立靠三个性质叠加：slot 密文只依赖 `(KEK, file_uuid, slot_index)`、
与邻居无关；`rewrite_slots` 保留原 FEK；头部 MAC 密钥由 FEK 派生，
所以搬完重算一次即自洽。

这是格式设计里一个之前没被利用的性质。**「保住某个槽」与「知道某个槽
是什么」是两件事。**

### 被用户推翻的一个论证

设计新模式时我写了「两种模式的 `header_len` 必须对齐，否则长度差异会
泄露用了哪种模式」。用户直接反问：**flags 不也是一眼看出区别么？**

这一问戳破了自相矛盾——既然靠一个明文可见的 flag 位标记模式，再对齐
长度就只是给一件已经公开的事做遮掩，纯属无效功。

改正后的判据是**区分产品事实与用户秘密**：

| 公开，不打算藏 | 必须藏住 |
|---|---|
| 这是 omy 文件（magic，威胁模型已接受） | 配了几个密码 |
| 用了哪种槽位模式 | 有没有恢复码 |
| Argon2 参数、密文长度 | 哪些文件属于同一个密码 |

于是方案变成：**flag 如实公开，槽位目录加密**。定长仍然要做，但理由
换了——不是为了藏模式，而是防止同模式内部「这个文件的目录比那个长」
泄露密码数量。

顺带修正了一处我之前说错的：可管理模式的代价不是「拿到文件的人能看出
你配了恢复码」，而是「**能打开这个文件的人**才看得出」。目录是 ENCRYPTED
的，得先有 FEK。这个差别让可管理模式比原先描述的实用得多。

### 为什么用 flag 位而不是抬 major 版本

用户最初的提法是「新增一个格式，文件头用另外的版本区分」。方向对，
但 `version_major` 是更贵的做法：

| | 抬 major 版本 | 新 flag 位 |
|---|---|---|
| 旧实现读到 | **直接拒绝打开** | 正常解密（CRITICAL TLV 才拒绝）|
| 5 组测试向量 | 全部重做 | 不受影响 |
| 防降级 | 靠版本号 | 靠头部 MAC，同样可靠 |

`flags` 被头部 MAC 覆盖（MAC 覆盖 `fixed||slot||tlv`），攻击者改不了
标记位，所以防降级效果与版本号相同。

### 本轮只落文档

格式规范 §3.5（含五个子节）、TLV 0x0010 登记、flags bit 10、
DEC-17、03 号文档 §4.4。代码未动，待办已登记两项。

## 止血：改密码不再抹掉恢复码（本轮）

DEC-17 登记的两项里先做第一项——它不依赖新格式，能立刻止住数据损失，
而且是恢复码能成立的前提。

### 做法：策略做成枚举，不是布尔

`rewrite_slots` 新增 `OtherSlots` 参数：`Carry`（原样搬运）与 `Discard`
（填随机清场）。做成显式枚举而不是 `bool` 或默认值，因为两种处理的后果
**都不可逆且方向相反**：一个会永久丢掉用户依赖的恢复码，另一个会让用户
以为「删掉的密码」其实还留着。调用方必须表态。

`add` / `change` 用 `Carry`，只有 `remove` 是 `Discard`。判据写成
`Op::other_slots()` 方法而不是在调用点现写——调用点有单文件与树形两处，
分开写迟早出现「单文件保住了、目录里没保住」这种不一致。

### 端到端抓到一个我没想到的边界

写验证脚本时按直觉断言「add 之后恢复码仍可开」，实测失败。
**先查探针再改产品**，dump 了 slot 区字节才看清真相：

```
只有主密码：     slot0=主  slot1..7=随机
add 恢复码后：   slot0=主  slot1=恢复
再 add 新密码：  slot0=主  slot1=新     ← 恢复码被顶掉了
```

不是策略传错。`keep` 占 slot 0..n，搬运只能从 n 开始，所以 keep 增长时
新密码必然写到原本属于别人的下标上。这个边界**绕不开**：包裹密钥绑定
`slot_index`，新密码必须写在自己的下标；而实现又分不清哪个下标是空的
（正是可否认性要求的）。

于是增加 `RewriteOutcome::may_have_evicted`，由 UI 如实提醒。措辞只能说
「可能」——那个下标上原来是真密码还是随机填充，我们确实不知道。

这也让可管理模式多了一条价值：有了槽位目录就能找到真正空闲的槽，
`add` 不必再赌。

### 文案跟着改

`change` 不再说「其它密码会失效」——那是改用搬运之前的行为，现在它不
殃及恢复码，继续这么写会让用户不敢改密码。改成「其它密码不受影响，
想清掉请用 key remove」。

`remove` 反过来要更狠：原来说「其它密码都会失效」太抽象，用户设恢复码时
想的是「灾难兜底」，不会把它归进「其它密码」。改成点名
「**包括你可能设过的恢复码**」。

### 验证

| 层 | 项数 | 关键断言 |
|---|---|---|
| core 单测 | 18 | 搬运后明文正确；下标对齐；两种策略产出可观察差异 |
| 端到端（CLI）| 15 | 用 decrypt 独立复核，不看命令自述 |
| 变异测试 | 5 | 全部被抓到 |

变异覆盖四种「看似合理的错误实现」：搬运退化成填随机、搬运下标错位、
change 误用清场、remove 误用搬运、不报告可能顶掉的槽。

`carry_keeps_slot_indices_aligned` 这条专门测下标错位——那种缺陷最难查：
文件长度对、MAC 对、新密码能开，只有被搬错位的密码静默失效。

### 探针写法上又确认一次的规律

变异脚本第一版把 cargo 的收尾行 `error: test failed` 误判成「编译失败」，
于是「断言抓到」被报成了「变异没测到东西」。**判据顺序必须是先看
`test result: FAILED`，再看 `error[E0xxx]`** —— 这两个结论方向相反，
一个说明断言有效，一个说明变异白写了。

## 恢复码（本轮）

DEC-17 里登记的第二项。地基（槽位搬运）上一轮已经打好，所以这一轮能
直接做到「改密码不会弄丢恢复码」。

### 词表：自带 SLIP-39，不引 crate

词表是公有领域的固定常量，为它引一个 bip39 crate 不划算——那些 crate
都带着 HD 钱包派生（`coins-bip39` 还会拖进 `bip32`），我们一点用不上，
还给 `cargo-deny` 白名单添维护量。

选 SLIP-39 而非 BIP-39 的理由是**手抄**这个真实场景：SLIP-39 全部 4–8
字母且**前 4 个字母全局唯一**，用户抄前 4 个字母就够。BIP-39 的词有长
有短、前缀也不唯一。

这些性质**没有只信规范描述**，`spikes/gen-wordlist.py` 在生成时实测五项：
恰好 1024 词（10 bit/词）、前 4 字母唯一、长度 4–8、全小写 ASCII、
已排序可二分。任何一条不成立，编解码的假设就要跟着改。

### 校验和要能定位到词

26 词 × 10 bit = 260 = 256 熵 + 4 校验。

校验和的职责是**区分「抄错了」和「拿错了」**——这两种处境的处置方式
相反：前者该回去核对纸条，后者该承认拿错了东西。没有校验和时两者表现
完全一样（都是解不开）。

进一步，26 个词里错一个，让用户从头核对 26 遍是很糟的体验。所以解析
失败分三种，各自指向不同的下一步：

| 失败 | 提示 |
|---|---|
| 词数不对 | 「应有 26 个词，实际数到 25 个」 |
| 某词不在表内 | 「第 7 个词「acadmic」不在词表中，是不是「academic」？」 |
| 整体校验不过 | 「每个词都认识但组合不对，常见原因是两个词写反了顺序」 |

第二种能精确建议，正是靠「前 4 字母唯一」这条性质。

**4 bit 校验够不够**：它有约 1/16 漏网率，但漏网时派生出的 KEK 也解不开
任何文件，用户照样得到「打不开」——只是错过了「第几个词可能有问题」
这条更有用的提示。加宽要占熵或加词，而 26 词已是抄写负担的上限。

### 替换掉旧的半成品

`session::kek_from_recovery_words` 把词拼起来做一次 SHA-256，**丢掉了熵
的结构**——它接受任意词、任意个数，因而无从校验。标了 `#[deprecated]`
并在文档里写清为什么不能用。

### --json 不输出恢复码

`--json` 的输出常被重定向进文件或管道进日志，把万能钥匙写进那里等于
白设恢复码。需要程序化保存的走 `--out`，那至少是用户显式指定的路径，
且命令会警告「该文件是明文，请立即转移」。

### 验证

| 层 | 项数 | 关键断言 |
|---|---|---|
| core 单测 | 11 | 往返保熵、抄写噪声容忍、词序错被校验和抓到、域分隔 |
| 端到端（CLI）| 20 | 全部用 decrypt 独立复核，不看命令自述 |
| 变异测试 | 6 | 全部被抓到 |

端到端第 7 项专门验证「改密码后恢复码仍有效」——那是上一轮槽位搬运的
直接收益，若 `change` 仍填随机，恢复码在这里就没了。

### 变异测试暴露的两个探针缺陷

**一、归因张冠李戴。** 第一版用 `FAIL\s+(.+)` 提取失败项，结果把 cargo
的 `test result: FAILED. 3 passed...` 也匹配进来。表现是「restore 抽掉
恢复码」被报成某个单测失败，而那个单测其实根本没失败。归因不准会让
照着报告查问题的人走错方向。改成锚定 Check 的固定格式
（`^\s+FAIL\s\s(.+)`）。同一个 bug 也在上一轮的 `mutate-slot-carry.ps1`
里，一并修了。

**二、用例之间有状态污染。** 「同一份恢复码可再次使用」原本接着上一步
的文件跑，于是测的其实是上一步留下的结果。改用独立 fixture 后，
变异才被抓到。

### 一个写错了的变异

「restore 抽掉恢复码」第一版写成「keep 里去掉 reco_kek」，实测**等于
空操作**：恢复码在 slot 1，keep 只剩一个时 Carry 正好把它从旧槽原样
带回，行为完全没变。只改成 Discard 同样是空操作——keep 里还有恢复码。
必须两处一起改才真的抽掉。

又一次印证：变异存活先问「它真的改变了程序的可观察行为吗」。

### 还没做

- ~~GUI 侧的恢复码界面~~（本轮已完成，见下）
- 树形加密目录的恢复码：每个文件各自挂槽，需要先定下「整棵树共用一份
  还是各一份」，命令目前如实拒绝

## 恢复码的 GUI 界面（本轮）

详见 `docs/recovery-code-ui.md`（含界面截图）。

### 为什么独立成一个对话框

没有并进「密码管理」。那个对话框回答的是「我怎么让某个密码不再有效」，
四个操作共享同一套输入。恢复码是另外两个心智：**生成**根本没有「新
密码」这个概念，**使用**根本没有「当前密码」。硬塞进去会让它变成一堆
互斥分支，而用户来的时候本来就清楚自己要哪一个。

### 两个菜单项的可用条件相反

「生成恢复码」要求文件**已解锁**（先证明你现在能打开它），「用恢复码
打开」**不要求**——会用到它正是因为密码已经忘了。

照抄 `keyManageable`（要求 unlocked）会让第二项只在文件已解锁时才亮，
而那时用户根本不需要它，真正需要时反而是灰的。

### 词为什么能回传给前端

CLI 的 `--json` 刻意不输出恢复码，因为那种输出常被重定向进文件或管道
进日志。GUI 走的是 Tauri IPC：只进 WebView 内存、当场显示，不落盘、
不进日志。前端只显示不存储。

### 几个刻意的界面选择

- **没有「复制」按钮**：有的话多数人会粘进笔记应用——那是同步到云端、
  进搜索索引、留在剪贴板历史里的一份明文万能钥匙。
- **等宽字体**：比例字体下 `rn`/`m`、`l`/`I` 在小字号里难分辨，而用户
  是照着屏幕往纸上抄。
- **grid 而非 flex 排版**：flex 会让末行 2 个词平分整行，列对不齐，
  用户会以为自己漏抄了。
- **必须勾选「我已抄下」才能关闭**：后端不保存明文，关掉就真没了。

### 验证

| 层 | 项数 | 说明 |
|---|---|---|
| 端到端（走真实界面）| 28 | 真右键、真点菜单、真填词 |
| CLI 独立复核 | 2 | 新密码能开且明文一致；旧密码已失效 |
| 变异测试 | 6 | 全部被抓到 |

### 变异测试抓出一个真实的断言缺口

前 26 项写完时，「generate 抹掉当前密码」这个变异**存活**了——它真的把
用户的日常密码弄丢了（让 generate 变成「把密码换成恢复码」），却全绿。

原因是探针的走向：生成之后一路用恢复码重设密码，全程不再碰原密码；
最后 CLI 复核的又是「新密码能开、旧密码已失效」——而旧密码本来就该
失效。整条链里没有一处会问「原密码在生成之后是否还活着」。

而那恰恰是这个功能最该保住的：**用户只是加一条退路，不该丢掉日常入口。**
补上「锁定后用原密码重新解锁」这条断言后，变异立刻被抓到。

同一轮里另有一个变异是**空操作**（只把当前密码从 `keep` 去掉，
`OtherSlots::Carry` 会把它从原槽位原样搬回来）。两者表现一样、处理方式
相反，所以每次都要先问「它真的改变了可观察行为吗」。

### 踩到的两个坑

**前端是内嵌进二进制的。** `tauri.conf.json` 的 `frontendDist` 指向
`dist`，release 构建会把前端打进 exe。改了 `.vue` 只跑 `bun run build`
不够，必须再 `cargo build`。表现极具迷惑性：dist 确实是新的、构建确实
成功了，但 GUI 显示的仍是旧界面，**所有断言一起失败**，看上去像功能
完全没做。线索是 9 张截图字节数一模一样。验证脚本现在会拿 exe 同时跟
`.rs` 和 `dist` 比。

**Vue 的 `:data-key` 不会渲染成 DOM 属性**（被归一化成保留的 `key`
用于列表 diff）。探针要按结构定位菜单项（不能按文字——「生成恢复码」
和「用恢复码打开」都含「恢复码」），改用 `data-mi` 才正常。

### 还没做

- ~~树形加密目录~~（本轮已完成，见下）
- 批量补挂：目前一次只能给一个文件生成

## 树形目录支持多密码与恢复码（本轮）

详见格式规范 §8.5 与 DEC-18。

### 根因不是恢复码，是目录名

树形一直只能有一个密码。写探针实测后看清：**槽位那层早就是多钥匙的，
卡的一直是目录名**。

| | 第一把钥匙 | 第二把 / 恢复码 |
|---|---|---|
| 打开单个密文文件 | 能 | 能 |
| 解开整棵树 | 能 | `content hash mismatch` |

目录名密钥由 `keks[0]` 派生，所以只有第一把认得。多密码和恢复码撞的是
同一堵墙，是一个问题不是两个。

### 改成两层结构

```text
目录名密文 = AEAD(DK, 目录名)        DK 随机，整棵树共用
.omy-keys  = 每把 KEK 各包一份 DK    固定 8 槽、408 字节
```

包裹**必须**放边车文件，这不是取舍是唯一可行解：

| 钥匙数 | 塞进目录名 | 放边车 |
|---|---|---|
| 1 | 196 | 100 |
| 2 | 292（超 255） | 100 |
| 4 | 484（超） | 100 |
| 8 | 868（超） | 100 |

两把钥匙就破了 255 字节上限；8 把时留给明文目录名的额度是 **0 字节**。
放边车则与钥匙数量完全无关。

### 顺带解决了扩展性

槽位区与钥匙**种类**无关——生物密码、显式槽位以后都只是「又一把 KEK」，
不占额外格式空间，不用再动这里。

### 连带发现一个会造成安全事故的缺陷

写探针验 `remove` 时实测到：它承诺「只保留当前密码」，而 `rekey_tree`
内部对文件槽位和边车都写死了 `Carry`，**第二个密码照样能解开整棵树**。
用户以为撤销了访问权，实际没有。

修法是给 `rekey_tree` 加 `OtherSlots` 参数，让两层跟随同一策略。
单文件那边早有这个参数，树形一直没跟上——`Op::other_slots()` 这个
方法当初就是为「两处保持一致」抽出来的，现在终于两处都用上了。

### 意外的简化

DK 随机且与密码无关，所以**换密码不再需要给目录改名**。删掉了
`rename_dir` / `find_renamed` / `retarget` / `same_dirname_key` 四个
函数，以及「改完名回填失败路径」那段容易漏的逻辑。用户也不会再遇到
「改完密码文件夹不见了」。

`rekey_tree_with_progress` 的参数到 8 个时 clippy 报了 too many
arguments，没有放宽 lint 而是打包成 `RekeyParams`——调用点写成一串
`&salt, cipher, false, Carry, None` 确实读不出谁是谁，而把 rotate 和
others 写反不会编译报错，只会静默改变语义。

### 新增的泄露面（已写进格式规范）

每个密文目录恒定带一份边车，所以**边车个数等于目录个数**，又多了一个
「这是 omy 加密目录」的指纹。它不泄露钥匙数量（长度固定）。

冗余是刻意的：只在根目录放一份更省，但那样任何一个子目录被单独拷出来
就再也解不开名字，而单独发一个子目录给别人是正常用法。

### 验证

| 层 | 项数 | 结果 |
|---|---|---|
| 边车单测 | 7 | 全过 |
| 全工作区 | 775 | 全过，clippy 零告警 |
| 端到端（CLI 真实调用）| 13 | 全过 |

端到端覆盖：两个密码都能解开整棵树、树形恢复码全流程、换密码后路径
不变、每个目录都有边车、删掉边车会明确报错。

### 测试断言的两类修正

六项失败必须分清，混为一谈会把真回归当「断言过期」放过去：

- **断言过期（4 项）**：换密码不再改名，「根目录名必须变」这类断言的
  前提不成立了。换成等价强度的新判据（「路径不变且新密码能解开整棵树」
  其实更接近用户关心的事实）。
- **真回归（2 项）**：测试辅助函数把边车当成用户文件，导致「外界能数出
  几个文件」从 3 变成 7。改的是辅助函数不是断言。

另外 `rekey_tree` 遇到错密码时行为变了：以前逐个文件试、报 N 条失败，
现在开头读边车就失败、整体返回 Err。这是更好的行为——用户拿到一句明确的
「打不开这棵树」，而不是一串分不清「密码不对」还是「文件坏了」的失败。
不变的是那条核心断言：磁盘上一字节都不能动。

### 已补完

- GUI 侧树形端到端 13 项 + CLI 独立复核 2 项，截图在
  `docs/screenshots/tree-multikey/`
- 变异测试 7 项全部被抓到

## 树形改造的收尾（本轮）

### 变异测试暴露了两处断言缺口

7 个变异最终全部被抓到，但过程本身比结果有价值：

**一、树形 remove 的语义没有任何自动化测试守着。** 那个缺陷当初是靠
临时探针偶然发现的，而探针已删——也就是说改造完成时，一个安全事故级
的行为处于无人看守状态。补的单测先自证第二个密码确实能用，再断言
remove 之后它在文件层和目录名层都失效；只查一层不够，缺陷正是
「文件层清了、目录名层没清」这种半残状态。

**二、边车层的丢弃分支是死代码。** 「只让边车层回到 Carry」这个变异
存活过。判断它是空操作是对的——文件槽位仍 Discard，被移除的密码连
文件都打不开，端到端根本观测不到目录名层。但那恰恰说明那半边没人
守着：今天有文件槽位挡着所以没后果，两层一旦不同步它就是真实的泄露。
补的单测直接对着 `rebuild_sidecar_carrying` 测，不经过文件层。

变异脚本先跑 core 单测再跑端到端：有几个变异（空槽填零、salt 绑定、
remove 语义）是单测的职责，端到端观测不到——只跑端到端会把它们误报成
「断言不足」，而断言就在那里、只是没被执行。

### GUI 端到端抓到一个真回归

`browse.rs` 的目录名解密还在用旧的单钥匙路径
（`DirnameKey::derive(kek)`）。改造时只改了 core 与 CLI，漏了这一处。

后果：**GUI 里加密文件夹永远解不开**，而错误提示说「密码不正确」——
CLI 明明能开。这类提示会把排查方向带偏到密码上。

定位过程值得记：先用 CLI 对同一棵树 decrypt + scan，两者都成功，
于是确定 core 没问题、问题在 GUI 层；再逐环验证 `vault_params_of`
那条链（`looks_encrypted` → `find_any_file` → `peek_header`），
全部正常；最后才锁定到 `annotate_tree_names` 里的密钥派生。

**教训：改格式时要全局搜一遍调用点。** 修完顺手搜了
`DirnameKey::derive|decrypt_dirname\(|encrypt_dirname\(`，确认只剩
`dirname.rs` 里旧 API 自己的测试。

顺带解决了那一处的旧隐患：以前逐个 KEK 派生再逐个试，而 `all_for`
底层是 HashMap、返回顺序每次运行都可能不同，表现为「同一个目录这次
名字解开了下次变回 base32」。现在任一把钥匙都能从边车解出同一个 DK。

### 前端判据也漏了解禁

`recoveryGeneratable` / `recoveryUsable` 里还排除着 `is_encrypted_dir`，
后端能用而菜单项永远灰着。两条菜单提示文案也还写着「加密目录暂不
支持」——这类过时文案比没有更糟，用户会照着一个不存在的限制做决定。

### 验证

| 层 | 项数 | 结果 |
|---|---|---|
| 全工作区 | 777 | 全过，clippy 零告警 |
| CLI 端到端 | 13 | 全过 |
| GUI 端到端 | 13 | 全过 |
| GUI 后的 CLI 独立复核 | 2 | 全过 |
| 变异测试 | 7 | 全部被抓到 |

GUI 端到端覆盖：解锁后能看到文件夹真名、密码管理对四个操作都放行、
边车告知真的显示出来、树形恢复码全流程、换密码后文件夹名不变。
界面说成功不算数，脚本最后用 CLI 复核整棵树（含嵌套目录）真的能用
新密码解开且明文一致，旧密码真的已失效。

## 2026-09-17　槽位可管理模式（DEC-17）

上一轮解禁了树形的多密码，但有一条边界始终绕不开：**无法只删掉某一个
密码**。用户想踢掉一个协作者，只能「只保留当前密码」，恢复码跟着一起没。
根因是可否认性——空槽填的是随机字节，与真实槽不可区分，所以实现自己也
不知道哪个下标是谁。

这一轮给格式加了第二种模式，让用户在加密时自己选。

### 为什么是加密时二选一，而不是「让它更聪明一点」

试过的两条路都不成立：

- **固定槽位用途**（比如 slot 7 恒为恢复码）：换不到可识别性。空槽仍要
  填随机，所以「slot 7 有没有东西」照样看不出来，只是把不确定性挪了个
  地方。
- **应用配置里记一笔**「本库启用了恢复码」：跨设备就失效了，而且与
  DEC-14（不落盘索引）直接冲突。

真正的取舍是：**可否认性与可管理性无法同时拿满**。所以不再找折中方案，
改成让用户在加密时明确选一边，并把代价讲清楚。

### 泄露边界：产品事实 vs 用户秘密

一开始我认为两种模式在字节层面必须不可区分，否则「选了可管理」这件事
本身就是信息。用户一句反问推翻了这个论证：`flags` 本来就是明文，
`header_len` 能一眼看出的东西 flags 同样能。

于是判据换成了「产品事实 vs 用户秘密」：

- **模式标志**是产品事实——omy 支持两种模式是公开的，如实公开它
  不泄露任何属于这个用户的东西。
- **目录内容**（有几个密码、哪个是恢复码）是用户秘密，所以放在
  ENCRYPTED 的 TLV 里。

这么一算，真实代价比直觉小得多：目录是加密的，让渡的只是「对**已经能
打开这个文件的人**，无法再隐瞒还有几个密码」。对打不开的人，两种模式
保护的东西一模一样。

### 格式

`flags` bit 10 `SLOT_DIRECTORY` + `TLV 0x0010`（CRITICAL + ENCRYPTED）。
目录固定 24 字节 = 8 槽 × 3 字节，每项 `kind(1B) || label_id(2B le)`。

三个决定各有理由：

- **定长**。变长会让目录长度泄露密码数量——那正是这个模式想保住的最后
  一点东西。定长还带来一个意外好处，见下面「就地替换」。
- **只存 label_id 不存名字字符串**。同上，变长名字会泄露。名字放应用侧。
- **用 flag 而非抬 `version_major`**。flags 被头部 MAC 覆盖，防降级效果
  相同；抬主版本会让既有实现直接拒绝打开，且 5 组测试向量全要重做。

未知 kind 读取时视为**未知类型**而非空槽。把未知当空，新版本写的槽会被
旧版本覆盖掉。

### 就地替换，不重建 TLV 区

`rewrite_slots` 里有一条 `debug_assert_eq!(bytes.len(), data.len())`——
改写槽位不得改变文件长度。所以新增 `replace_tlv_value` 就地换掉 value
密文段，而不是重建整个 TLV 区。槽位目录定长、同一 AEAD 下密文长度也
固定，所以这条路是安全的。新旧长度不等时直接报错而非尽力而为：长度一变
`header_len` / `tlv_len` 就都对不上，改它们等于重写整个文件。

### 轮换必须把目录带过去

编译器帮忙抓到的：`reencrypt.rs` 的 `rebuild_options` 缺了 `slot_directory`
字段。漏掉的话轮换会让文件从可管理模式**静默退回**可否认模式——用户收不到
任何提示，只会在下次想精确删协作者时发现能力没了。

### 恢复码要如实记进目录

端到端第一轮就抓到：恢复码挂上了，但目录里那个槽还写着「空」。
`key recovery` 当时还走的是老路径。这种不一致很危险——用户看着目录以为
slot 2 空着，拿它去放新密码，恢复码就没了。

### 验证

| 层 | 项数 | 结果 |
|---|---|---|
| 全工作区 | 592 | 全过，clippy 零告警 |
| slotdir 单测 | 8 | 全过 |
| core 集成 | 7 | 全过 |
| CLI 端到端 | 21 | 全过 |
| GUI 端到端 | 11 | 全过 |
| 变异测试 | 6 | 全部被抓到 |

核心承诺已被证实：精确删掉协作者、恢复码保住、主密码不受影响；删除留下
的空洞能被 `add` 复用（可否认模式下这一步会顶掉恢复码）；文件长度与载荷
一字节未变。

### 两个教训

**变异存活先问「它真的改变了行为吗」。** 有一个变异（去掉
`rewrite_slots_managed` 开头的 flag 检查）存活了，但它是空操作——去掉之后
`replace_tlv_value` 仍会因为找不到 TLV 而报错，可观察行为一点没变。换成
两个真能改变行为的变异后 6/6 全灭。

**PowerShell 的 `-match` 不区分大小写。** 变异脚本里用 `\bFAILED\b` 判断
测试失败，结果命中了每一行都有的 `0 failed`，基线永远判为失败，而输出
看起来全是 ok。改用 `-cmatch` 才对。