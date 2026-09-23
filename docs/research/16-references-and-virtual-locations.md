# 16 · 引用模型与虚拟远程位置（调研）

> **文档状态**：调研稿（v0.1），**只出结论、不落实现代码**。
> 面向"下一步要不要做、怎么做、工作量多大"的决策，不是实现规格。
>
> **来源核实口径**：凡标【已核实】的，均对照 grammers 0.10 的 TL schema
> （`grammers-tl-types-0.10.0/tl/api.tl`）或 grammers-client 0.10 源码
> （`message/message.rs`、`client/messages.rs`）逐字确认，并注明构造器/方法名；
> 凡标【待定】的，是尚未验证或依赖产品决策的部分，不得当成结论引用。
> **区分"官方协议支持"与"grammers 已暴露"**：协议里有的字段，grammers 未必
> 给了高层 getter；本文对每条都分开说明。

本文分两部分：
- **调研 1**：各类型的唯一 id 与引用能力——消息、媒体的稳定标识，"视频右键→
  定位源消息"、"消息引用→跳转"到底能不能做、靠什么字段做。
- **调研 2**：**虚拟远程位置**——本地配置的、组合/引用其他真实位置内容的位置，
  它的数据模型、如何接入现有 `RemoteStore`/`PlaceStore`、**缓存能否共享**、
  跳转与打开、搜索整理，以及可行性与工作量估。

---

## 调研 1：各类型唯一 id 与引用能力

### 1.1 消息的稳定 id：`message.id`，对话内唯一、跨对话必须带 peer

【已核实】TL 里 `message#7600b9d3 ... id:int ... peer_id:Peer ...`
（`api.tl:123`），`messageService#7a800e0a ... id:int peer_id:Peer ...`
（`api.tl:124`）。grammers-client 暴露 `Message::id() -> i32`
（`message/message.rs:238`）。

关键性质：
- **`message.id` 只在其所属对话内唯一**，是一个 `i32`，不是全局唯一。两个不同
  对话里各有一条 id=6 的消息是常态。
- 所以"稳定引用一条消息"的最小 key 是 **`(对话, 消息id)`** 二元组，缺一不可。
- 频道/超级群与私聊/普通群在这一点上**没有区别**：都靠 `(peer, id)` 定位。区别
  在于 peer 的构造（频道要 `access_hash`），但 id 本身语义一致。

**omy 现状印证**【已核实，`telegram/store.rs:282-286`】：文件条目 id 就是
`tg:<chat>:<message>`，正是这个二元组的字符串化。目录（对话）id 是 `tg:<chat>`
（`store.rs:441`）。这套 id 已经是"对话内消息唯一 + 带对话前缀"的实现，调研 2
的引用 key 可直接复用它，不需要另造。

### 1.2 媒体标识：`document.id` / `photo.id` 长期稳定，但 `file_reference` 会过期

【已核实】
- `document#8fd4c4d8 ... id:long access_hash:long file_reference:bytes ...`
  （`api.tl:540`）
- `photo#fb197a65 ... id:long access_hash:long file_reference:bytes ...`
  （`api.tl:218`）
- 下载定位用的 `inputDocumentFileLocation#bad07584 id:long access_hash:long
  file_reference:bytes thumb_size:string`（`api.tl:66`）、
  `inputPhotoFileLocation#40181ffe ...`（`api.tl:69`）。

三个字段的寿命完全不同，这是本节最要紧的区分：

| 字段 | 类型 | 寿命 | 能否当长期 id |
|---|---|---|---|
| `id` | `long` | 该文件在 Telegram 上的**永久**标识 | 能标识"是不是同一个文件"，但**单靠它下不了载** |
| `access_hash` | `long` | 与账号绑定、较稳定 | 下载需要，但换账号可能变 |
| `file_reference` | `bytes` | **短期**，服务端只保证短时间有效 | **绝不能**当长期 id |

**同一个文件可以在多条消息里转发**：它们的 `document.id` 相同，但各自消息的
`(对话, 消息id)` 不同、拿到的 `file_reference` 也可能不同。所以：
- 判断"两个条目是不是同一个文件" → 比 `document.id`/`photo.id`。
- **实际要下载** → 必须有一份**新鲜的** `file_reference`，它只能通过"重新拉那条
  消息"获得。

**omy 现状印证**【已核实，`store.rs:266-286`、`796-828`、`1401`、`1532`】：注释
明确写了"不用 `file_reference` 当 id：它会过期，编进去会让缓存键随刷新而变"；
etag 也刻意不用 `file_reference`。omy 用 `(对话,消息)` 做稳定 id，下载时**按需
重新拉消息换取新鲜 `file_reference`**（`store.rs:469` 注释"会过期，过期后要重新
list"）。这套"稳定 id + 按需刷新 reference"的两层结构，正是调研 2 的引用模型该
沿用的范式。

### 1.3 "视频右键 → 定位源消息"：可做，且 omy 已具备全部材料

诉求：在媒体分栏里对一个视频右键 →"跳到它在对话里的原消息"。

【已核实】做这件事需要的两样东西 omy 都已经有：
1. **从媒体条目拿到 `(对话, 消息id)`**：条目 id 就是 `tg:<chat>:<message>`
   （`store.rs:282`），`strip_prefix("tg:")` 再切一次即可得到二元组
   （`store.rs:298`）。
2. **跳回消息视图并定位那条消息**：消息视图本身按 `getHistory` 增量分页（见 15
   号文档 §5.2.4），定位到某条消息可用 `offset_id` 把该消息放到首屏。

【已核实】grammers 侧还提供了直接按 id 取消息的入口
`Client::get_messages_by_id`（`client/messages.rs:1111`），可用于"跳过去时先确
保那条消息在手"。

【待定】纯 UI 工作量：媒体分栏 → 消息视图的视图切换 + 高亮目标消息的滚动定位，
需要一次交互设计（消息视图当前是顺序流，"跳到第 N 条并高亮"要加锚点滚动）。属
调研 2 的"跳转与打开"，此处只确认**协议与 id 层面无障碍**。

### 1.4 "消息引用 → 跳转"：回复可跳，转发来源可展示

诉求：一条消息若是"回复某条"或"转发自某处"，点引用能跳到原消息。

#### 回复（reply）——【已核实，可跳】

- TL：`message ... reply_to:flags.3?MessageReplyHeader`（`api.tl:123`），
  `messageReplyHeader#1b97dd66 ... reply_to_msg_id:flags.4?int
  reply_to_peer_id:flags.0?Peer ...`（`api.tl:1417`）。
- **grammers 已暴露**：
  - `Message::reply_to_message_id() -> Option<i32>`（`message/message.rs:586`）
    ——被回复消息的 id。
  - `Message::reply_header() -> Option<MessageReplyHeader>`
    （`message/message.rs:327`）——完整头，含 `reply_to_peer_id`（跨对话回复时
    的原对话）。
  - `Message::get_reply() -> Option<Message>`（`message/message.rs:599`，
    转发到 `Client::get_reply_to_message`，`client/messages.rs:966`）——直接把
    被回复的那条消息取回来。
- **结论**：回复跳转能做。同对话回复只需 `reply_to_msg_id`；**跨对话/话题回复**
  要一起看 `reply_to_peer_id`（`api.tl:1417`，flags.0），否则会在当前对话里找一
  个不属于它的 id。

#### 转发来源（forward）——【已核实，可展示；能否"跳"取决于来源是否可达】

- TL：`message ... fwd_from:flags.2?MessageFwdHeader`（`api.tl:123`），
  `messageFwdHeader#4e4df4bb ... from_id:flags.0?Peer from_name:flags.5?string
  channel_post:flags.2?int saved_from_peer:flags.4?Peer
  saved_from_msg_id:flags.4?int ...`（`api.tl:825`）。
- **grammers 已暴露**：`Message::forward_header() -> Option<MessageFwdHeader>`
  （`message/message.rs:308`）。
- **结论**：
  - **展示**"转发自谁"总能做（`from_id`/`from_name`）。
  - **跳到源**只在源可达时可行：`saved_from_peer` + `saved_from_msg_id`
    （`api.tl:825`，flags.4）给了原始位置，但**源可能是当前账号看不到的频道、
    或已删除**——此时只能展示、不能跳。`from_name` 分支（flags.5）是"发信人隐藏
    了账号只留名字"，天然无法跳。
  - 界面必须区分"可跳"与"仅展示"，不能给一个点了就报错的跳转。

---

## 调研 2：虚拟远程位置

### 2.0 它是什么

用户设想的"虚拟远程位置"：一个**本地配置的位置**，本身不持有任何字节，而是
**引用**散落在各个真实位置（某个 Telegram 对话、某个 WebDAV 目录）里的条目，
把它们聚合成一个用户自定义的视图——可打标签、加备注，方便整理和搜索，点一下能
跳回真实位置，或直接播放（若缓存共享则秒开）。

类比：它对远程位置，约等于"播放列表/收藏夹"对音乐库——**引用而非副本**。

### 2.1 数据模型：存引用，不存副本

**核心 key 用调研 1 的结论**：一条引用最小需要
**`(真实位置 id, 对话/目录 id, 条目 id)`** 三元组。对 Telegram 就是
`(place_id, tg:<chat>, tg:<chat>:<message>)`；对 WebDAV 就是
`(place_id, 目录路径, 文件路径)`。

【已核实，源自调研 1】引用里**不存 `file_reference`**：它会过期。打开时按
现有路径"重新拉消息/PROPFIND 换新鲜 reference"（omy 已有此逻辑，
`store.rs:469/1532`）。引用里可以存 `document.id`/`photo.id`（长期稳定）用于
"这份引用指的还是不是同一个文件"的校验。

一条引用建议存：
- 三元组 key（定位与去重）；
- 冗余一份**显示快照**（名字、大小、类型、缩略图 token）——让虚拟位置在源不可
  达时仍能列出条目（灰显），这与 15 号文档 §5.2.5 "列表快照落盘"是同一手法；
- 用户附加的**标签 / 备注**（挂在引用上，属于虚拟位置的本地数据）。

【待定】引用失效处理：源消息被删、文件被移走时，引用要能标"失效"而不是静默消
失——需要一次"解析引用"的健康检查设计（可懒执行：打开时才发现失效）。

### 2.2 接入现有 `RemoteStore` / `PlaceStore`

【已核实】现有抽象（`store.rs:65` `trait RemoteStore`）的形状很适配：
`list(dir_id) -> Vec<Entry>`（`store.rs:111`）、
`read_range(id, offset, len)`（`store.rs:122`）、`Entry` 结构（`store.rs:19`，
含 id/name/is_dir/size/thumb 等）。

两种接入路线：

**路线 A：虚拟位置作为一个特殊 `RemoteStore` 实现（推荐）**
- 它的 `list()` 不发网络，而是从本地引用表组装 `Entry` 列表（用 2.1 的显示快照
  填充 name/size/thumb）。
- 它的 `read_range()` **委托给引用指向的真实位置的 store** ——即"虚拟位置的读
  = 找到该引用的真实位置，转调它的 read_range"。
- 好处：对上层（命令层、UI、缓存）几乎透明，虚拟位置和真实位置走同一套浏览/
  播放/解密链路。
- 【待定】难点：`read_range` 委托需要虚拟 store 能拿到真实位置的 store 实例——
  要一个"按 place_id 取已连接 store"的注册表（`PlaceRegistry` 已管理位置，
  但需确认它能返回可调用的 store 句柄，见 2.3 缓存共享同一前提）。

**路线 B：纯 GUI 层聚合，不进 `RemoteStore`**
- 虚拟位置只是前端一个"引用列表视图"，点条目时前端直接用真实位置的现有命令
  打开。
- 好处：后端零改动。
- 坏处：虚拟位置无法复用"位置"这套框架（侧栏、能力矩阵、缓存角标都要在前端
  另做一遍），与"它是一个位置"的产品心智不符。

倾向路线 A：它让虚拟位置真正成为一等的"位置"。

### 2.3 缓存共享（关键）——现有 key 结构**能**共享，但有一个硬前提

这是决定"虚拟位置打开已缓存内容是否秒开"的关键，必须讲准。

【已核实】缓存块的 key 是 **`digest_hex(place ‖ \0 ‖ id ‖ \0 ‖ block)`**
（`cache.rs:785-796` `block_path`），其中：
- `place` 是**位置 id 字符串**（如 `"p3"`、`"tg"`、`"nas"`）；
- `id` 是**带版本后缀的条目键**（形如 `<条目id>\u{1}<版本哈希>`，
  见 `cache.rs` 各测试用例 `"/a\u{1}v1"`，版本来自 `cache_version`）;
- `pin`（永久层）也用同一 `(place,id,block)`（`cache.rs:268` `pinned_path_of`）。

**推论（这是本调研最重要的一条结论）**：
- 缓存命中**只认 `(place, id, block)` 三者全等**。
- 所以虚拟位置要命中真实位置**已经缓存好的块**，它在 `read_range`/预热时**必须
  用真实位置的 `place` 字符串和真实位置的版本化 `id`**，而**不是**虚拟位置自己
  的 place_id。
- 这正好与 2.2 路线 A"read_range 委托给真实位置的 store"**天然一致**：只要委托
  过去、由真实 store 用它自己的 `(place,id)` 去 `BlockCache`，缓存就自动共享，
  **不需要改缓存层**。
- 反过来的**反面教训**：如果偷懒让虚拟位置用自己的 place_id（如 `"virt1"`）去
  读，key 里 `place` 不同 → **哈希不同 → 必然缓存未命中**，会把同一个文件在
  磁盘上缓存两份。这与 15 号文档里"列表快照落盘前必须剥 thumb_token""dir id
  用真实哈希"是同一类坑：**缓存 key 的每一个组成部分都必须来自真实来源**。

【待定】pin（永久保留）语义：虚拟位置里对一条引用点"永久保留"，应等价于对真实
位置那份 pin（同一 `(place,id)`），而不是在虚拟位置名下再 pin 一份。实现上就是
委托，但产品上要想清"在收藏夹里 unpin 会不会影响真实位置那份"——它们是同一份，
答案是"会"，UI 要说清。

【待定】前提确认：路线 A 的委托要求"能按 place_id 拿到一个可 `read_range` 的真
实 store 句柄"。`PlaceRegistry` 管理 `SavedPlace` 元数据，但**是否持有/能重建
已连接的 store 实例**需要在实现前核实（Telegram store 的连接是有状态的，见 15
号文档连接复用一节）。若不能直接拿到，虚拟位置的 read 就要走"按 place_id 现连
一次"的路径，性能上仍可接受（连接复用实测 ~2ms）。

### 2.4 跳转与打开

- **跳转到真实位置**：复用调研 1.3 的 `(对话,消息)` 定位。虚拟位置的一条引用带
  着三元组，"在真实位置中打开"= 切到该 place → 该对话 → 定位该消息。
- **直接播放**：复用现有播放链路（`RemoteSource` 块对齐 + 密文缓存，见 15 号
  文档）。若 2.3 的缓存共享成立，且真实位置已缓存过 → **秒开**（读盘，不发网络）。
- 【待定】两种入口的 UI：右键菜单"在原位置打开" vs 双击"就地播放"，需交互设计。

### 2.5 搜索与整理

- 虚拟位置的条目带**本地标签 / 备注 + 显示快照里的名字**，可做**纯本地搜索**
  （不发网络），这是它相对真实位置的核心增值：真实位置搜索受服务端能力限制
  （Telegram 的 `messages.search` 按类型/关键词，见 15 号文档六分栏），而虚拟
  位置可按用户自己打的标签、备注、甚至**解密后的真名**（若该文件是 .omy 且已解
  锁过、真名进过本地索引）来找。
- 【待定】"按真名搜"要复用 omy 的识别结果缓存（15 号文档 §5.2.5 ④文件头），且
  要遵守"解密真名不落明文"的既有红线——搜索索引若含真名，需与该红线一致处理
  （可能只在会话内存里建索引，不落盘）。

### 2.6 可行性与工作量估

| 部分 | 复用什么 | 新建什么 | 估量 |
|---|---|---|---|
| 数据模型 | 调研 1 的三元组 id；§5.2.5 快照落盘手法 | 引用表结构 + 标签/备注 + 持久化 | 中 |
| 接入 store | `RemoteStore` trait、现有浏览/播放/解密链路 | 一个委托型 `VirtualStore` + place_id→store 句柄获取 | 中 |
| 缓存共享 | **`BlockCache` 完全不用改**（key 已够用） | 仅需保证委托时用真实 (place,id) | 低（前提：委托做对） |
| 跳转/打开 | 调研 1.3 定位、现有播放链路 | 媒体→源消息的滚动定位 + 两种入口 UI | 中 |
| 搜索 | §5.2.5 识别结果 | 本地标签/备注/真名索引（真名不落盘） | 中 |
| 引用健康检查 | 现有"过期重拉"路径 | 失效标记 + 懒解析 | 低-中 |

**总体**：无需改动缓存层、无需改协议层，主要是"一个委托型虚拟 store + 引用表 +
若干 UI"。风险点集中在两处，都在上文标了【待定】：
1. **place_id → 可用 store 句柄**的获取路径（决定委托能不能干净实现）；
2. **缓存 key 必须全用真实来源**（决定缓存是否真共享，做错就是隐性双份缓存）。

两者都不是协议障碍，是接线正确性问题——而"接线正确性"恰是本项目反复出问题的
地方（camelCase 参数、猜 id 形状、缓存 key 用错来源），所以实现时要把
"虚拟位置的读，其缓存 key 等于真实位置的缓存 key"写成一条断言测试来锁死。

---

## 附：本文引用来源一览（便于复核）

- TL schema：`grammers-tl-types-0.10.0/tl/api.tl`
  - `message#7600b9d3`（L123）、`messageService#7a800e0a`（L124）
  - `document#8fd4c4d8`（L540）、`photo#fb197a65`（L218）
  - `inputDocumentFileLocation#bad07584`（L66）、`inputPhotoFileLocation#40181ffe`（L69）
  - `messageReplyHeader#1b97dd66`（L1417）、`messageFwdHeader#4e4df4bb`（L825）
- grammers-client 0.10：
  - `Message::id/reply_to_message_id/reply_header/forward_header/get_reply`
    （`message/message.rs:238/586/327/308/599`）
  - `Client::get_reply_to_message/get_messages_by_id`
    （`client/messages.rs:966/1111`）
- omy 现状：
  - id 形态 `tg:<chat>:<message>`、"不用 file_reference 当 id"
    （`telegram/store.rs:266-298/796-828/1532`）
  - 缓存 key `digest_hex(place‖id‖block)`（`cache.rs:785`）、pin 同 key
    （`cache.rs:268`）
  - `RemoteStore` 契约（`store.rs:19/65/111/122`）
