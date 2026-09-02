# omy 实现进度

> 本文件是**跨会话的权威状态来源**。每完成一个可验证的阶段就更新，
> 并随代码一起提交，以便任何时候都能接续。
>
> 最后更新：2026-08-30

## 状态速览

| crate | 状态 | 测试 | 说明 |
|---|---|---|---|
| `omy-core` | 🟢 格式核心可用 | 142 项 | 格式读写、密钥、分块、分片、原子写、扫描、容器、BlockSource、媒体 TLV |
| `omy-cli` | 🟢 13 个命令可用 | 59 项 + 88 项端到端 + 19 项局域网实测 | 新增 `share` 命令组（serve / discover / pair / connect / devices）|
| `omy-media` | 🟢 探测/分级/moov/缩略图/**P2 转封装**可用 | 111 项 + 105 项真实文件验证 | LGPL，FFmpeg 封装 |
| `omy-net` | 🟢 全链路可用 | 114 项 + 55 项端到端 | 编解码、配对、mDNS、Noise IK、零密钥服务端、加密持久化、授权会话、服务端主循环 |
| `omy-gui` | 🟢 桌面端可用，移动端 UI 就绪 | 111 项 + 43 项端到端 + 25 项响应式实测 | Tauri v2；Vue 3 + Vite；**文件管理器式交互**，无密码也能进门；**设备发现/配对/共享已接入** |

合计 **529 项自动化测试 + 88 项 CLI 端到端断言 + 43 项 GUI 端到端断言 + 46 项媒体 TLV 端到端断言 + 63 项 omy-media 真实文件断言 + 42 项 P2 转封装断言 + 16 项 Spike 断言 + 74 项局域网 Spike/验证断言 + 26 项 GUI 文件管理器实测断言 + 35 项 GUI 设备与共享实测断言 + 33 项双机远端浏览实测断言 + 21 项 KDF 档位实测断言 + 26 项预览与提示实测断言 + 28 项文件夹加密实测断言**，`cargo clippy --workspace --all-targets -- -D warnings` 零告警。

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
| `encrypt --mode tree` | ⛔ 如实报未实现 | 需目录名加密与 base32 编码（文档 05 §3） |

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
- [ ] `tree` 模式：目录名加密 + base32 编码（文档 05 §3）
- [ ] slot 原地增删改：保留 FEK、重建 slot 区、重算头部 MAC，**不重写载荷**（`key add/remove/change` 依赖此能力）
- [ ] 元数据保存与还原报告的完整实现（D-20 / N3）
- [ ] 伪装模式：`footer` / `host-jpeg` / `host-png`（D-17）
- [ ] 符号链接跨平台还原（当前如实报告为跳过）

### omy-cli

- [x] 12 个子命令
- [x] 密码五通道 + 拒绝 `--password`
- [x] `--json` 输出；`code` 恒为英文常量
- [x] i18n 简中 + 英文
- [x] 配置文件
- [x] `--vault`：加入已有库，复用 salt 与 KDF 参数
- [ ] `serve` / `connect`（依赖 omy-net）
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
- [x] `omy-gui`：移到回收站（`trash` crate，实测可从系统回收站还原）
- [x] `omy-gui`：加密进度条（Tauri 事件按整百分比节流，前端实时渲染）
- [x] `omy-gui`：响应式 UI（≤768px 走移动端布局：抽屉侧栏 + 2 列网格 + 底部导航）
- [ ] Android 打包（前端已就绪，卡在本机缺 JDK / SDK / NDK，见「Android 现状」）
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

### Android 现状：代码侧已处理，卡在工具链

已经修掉的**真实缺陷**（不修的话真机必炸）：

- `plain.rs` 的 `launch` / `reveal` 用 `cfg(all(unix, not(macos)))` 分支
  调 `xdg-open`。**Android 也满足这个 cfg**，但 Android 上没有
  `xdg-open`，会 spawn 一个不存在的命令，只得到含糊的 `open_failed`。
  已单独分出 Android 分支，明确返回 Unsupported。
- `trash` 5.2 的 cfg 明确排除了 android，在 Android 上这个 crate 不提供
  任何函数，无条件依赖会**在链接期才报错**。已改为
  `[target.'cfg(not(target_os = "android"))'.dependencies]`，调用点分两支。
  Android 分支返回 `trash_not_supported`，**绝不降级为永久删除**——
  用户选回收站就是想要能后悔。

本机工具链缺口（跑 `pwsh -NoProfile -File spikes\check-android-env.ps1`
可复查）：

| 项 | 状态 |
|---|---|
| Rust 目标 aarch64 / armv7-linux-android | ✅ 已装 |
| tauri-cli 2.11.4 | ✅ 已装 |
| JDK 17+ | ❌ 缺 |
| Android SDK | ❌ 缺 |
| Android NDK | ❌ 缺 |
| NDK 里的 clang | ❌ 缺 |

最后一项容易被忽略：`omy-core` 依赖 `zstd-sys`，那是 C 代码，交叉编译
必须有 NDK 的 clang。已实测确认——`cargo check -p omy-core --target
aarch64-linux-android` 报
`error occurred in cc-rs: failed to find tool "clang.exe"`，**连 core 都
编不过，与 Tauri 无关**。

剩下四项都是 Android Studio / NDK，GB 级下载且要接受许可协议，属于
环境决策，没有代劳。补齐后：

```powershell
pwsh -NoProfile -File spikes\check-android-env.ps1   # 确认 7/7
cd crates\omy-gui
cargo tauri android init
cargo tauri android build
```


## 两个「面向用户的输出」缺陷（本轮）

Android 与局域网双机验证都在等环境，所以这轮转向已有代码里能就地
验证的东西。查 `not_implemented|未实现|TODO` 时发现两处真实缺陷，
都属于「产品能跑，但告诉用户的信息是错的」——这类问题不会让测试
变红，只会让用户做出错误判断。

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
