# 19 · Telegram 对话高级搜索

> 状态：**后端与目标选择已接线**。查询契约与校验/递归求值在共享 crate
> `omy-remote::telegram::advanced`；两个 Tauri 命令已注册。真实账号端到端验证
> 尚未完成（不得把 mock 或单元测试当成真实账号验证）。

## 1. 要解决的问题

现有的搜索只有两档：本地过滤已加载条目、服务端按关键字搜。用户想要的是「这个群里
所有 500 MB 以上、不是 mkv 的视频」这类组合条件，并且**一次对全部命中做处理**
（永久保留、转发、添加到虚拟远程、上传到别的位置）——而命中往往分布在几万条历史里，
不可能先全部加载进界面再勾选。

## 2. 入口与界面

- 入口：Telegram 对话内，搜索框右缘的筛选按钮（`data-tg="adv-search"`）。对话列表和网盘目录
  不显示——条件是按 Telegram 消息定义的。
- 弹窗 `TelegramAdvancedSearchDialog.vue`：上半拼条件，下半即时列出命中，底部是批量动作。
- 移动端是底部弹层，动作按钮两列排布；和桌面共用同一个组件。

### 条件

| 种类 | 字段 | 取反（negate = true）的含义 |
|---|---|---|
| `media` | `types`：photo / video / gif / audio / voice / file / link，任一即命中 | 不属于其中任何一种；**纯文本消息也算命中** |
| `ext` | `exts`：小写、无点的扩展名 | 不是这些扩展名；**没有文件的消息也算命中** |
| `size` | `min` / `max`：字节，闭区间，null 为不限 | 不在区间内；没有文件的消息也算命中 |
| `text` | `text`：正文或媒体说明包含，不区分大小写 | 不包含 |
| `date` | `from` / `to`：Unix 秒，闭区间，null 为不限 | 不在区间内 |
| `group` | `match` + `conditions`：子条件树 | `all` 取反 = 不全满足，`any` 取反 = 都不满足 |

条件是一棵树：查询根是 `{ match, conditions }`，`conditions` 里既可以是叶子条件，也可以是
`group`。根不取反（要取反就套一层子组）；界面最多嵌套两层子组（`MAX_GROUP_DEPTH`）。
空的子组前端直接报错，不发请求：空的 all 恒真、空的 any 恒假，哪种都和用户以为的不同。

日期由界面按**本机时区**换成所选日当天 00:00:00 与 23:59:59（取次日零点 − 1，夏令时那天
不是 86400 秒），后端拿到的已是秒数，不再做时区换算。

组合方式 `match`：`all`（全部满足）或 `any`（任一满足）。没有文件的消息对 ext / size
条件的取值是「不满足」，取反后为「满足」——这条要在后端实现与测试里钉住，界面文案
「不包含 / 不是 / 不在范围内」就是按这个语义写的。

前端在 `tg-advanced-search.ts` 里校验：任何一行不完整（没选媒体、扩展名为空、大小两端都空、
min > max、文字为空、日期两端都空或颠倒）、有空的子组，或一个条件都没有，就**不发请求**。后端仍应自行校验，不能依赖前端。

## 3. 命令契约

类型定义在 `crates/omy-gui/frontend/src/types.ts`（`TgAdvancedQuery`、`TgSearchCondition`、
`TgSearchHit`、`TgSearchPage`、`TgSelection`、`TgBatchAction`），调用在 `api.ts`。

### `telegram_advanced_search`

```
req: { place_id, dir, query: TgAdvancedQuery, cursor: string | null, limit: number }
→   { hits: TgSearchHit[], cursor: string | null, scanned: number }
```

- 按时间**从新到旧**扫描；`cursor` 不透明，由后端决定内容（例如 offset_id）；返回 null
  表示已扫到对话开头。
- `scanned` 是累计扫描的消息数，界面显示为进度。一页可能 0 条命中但 cursor 非空（这一段
  全被过滤掉），前端会继续请求，不能把它当成结束。
- 能下推的尽量下推：只有一个正向 `media` 条件时用 `messages.search` 的对应 filter；
  正向 `text` 条件可以作为服务端 q。根为 `all` 时，根上的正向 `date` 条件可以下推为
  `messages.search` 的 `min_date` / `max_date`，并且扫到早于 `from` 就可以提前结束。
  取反条件、子组内的条件、ext、size 只能拉回后在本机按整棵树求值。
- **服务端搜索的隐私语义沿用现有约定**：正向文字条件发给 Telegram，弹窗里已有提示
  （`adv_search.server_hint`）。

### `telegram_advanced_batch`

```
req: { place_id, dir, query, selection: TgSelection, action: TgBatchAction, target?: TgBatchTarget }
→   { task_id }
```

- `selection = { mode: 'ids', ids }`：只处理这些条目。
- `selection = { mode: 'all', except }`：处理**全部命中**，后端按同一个 `query` 从头遍历整个对话，
  跳过 `except`。这是「界面只加载了一部分也能全选」的关键：前端不需要、也不应该把全部 id
  传过来。
- 立即返回，建一个全局任务挂进传输管理（与「全部缓存」同一套父子任务、暂停/取消/重试）；
  前端收到成功后关弹窗、打开传输管理。
- 批量动作使用的是**上一次搜索**的 query，不是弹窗里之后改过但没重搜的表单。

## 4. 批量动作

| action | 行为 | 目标 |
|---|---|---|
| `pin` | 每个命中文件转为永久保留（完整原文件） | 无 |
| `forward` | 原生 `forwardMessages`；按 100 条一批，保持原时间顺序 | 转发目标对话 |
| `add_virtual` | 在虚拟远程里为每个命中文件建引用 | 虚拟位置 + 文件夹 |
| `upload_to` | 复制到另一个远程位置 | 远程位置 + 目录 |

**已接线**：`forward` / `add_virtual` / `upload_to` 先复用现有目标选择框；
确认目标后才发请求。`target` 是按 `kind` 区分的联合类型：

- `forward`：`{ kind: 'forward', dir }`（当前账号的目标对话）；
- `add_virtual`：`{ kind: 'virtual', place_id, folder }`；
- `upload_to`：`{ kind: 'remote', place_id, dir, mode: 'memory' | 'permanent_cache' }`；
- `pin`：不带目标。

目标类型不匹配、虚拟位置锁定、目标目录只读时拒绝。上传不覆盖已有同名对象。
文件动作跳过无可下载文件的命中；转发允许纯文本。先扫描发现任务，再顺序执行子任务；
转发收齐消息号后全局按旧→新排序，每批最多 100 条。任务请求仅驻内存，重启不恢复。
扫描失败重试从上一成功页续跑；子任务失败只重跑失败项，成功件不重放。
转发断网或返回数不足时结果不确定，不给盲目重试入口，应先检查目标对话。

受保护内容（noforwards）的对话里，转发按钮禁用；后端仍须在发请求前拒绝。

## 5. 选择与计数

- 选择状态 = `{ all, ids }`：`all=false` 时 ids 是勾选的；`all=true` 时 ids 是取消勾选的。
- 全选且尚未扫完时，已选数量只是下界，界面显示「已选全部匹配项（目前 N+ 项）」，避免用户
  以为只会处理已加载的那些。
- 全选但一条都没扫到、扫描仍在进行时，允许执行（后端会继续遍历）；扫完且确实没有命中时不允许。

## 6. 命令行

高级查询已抽到共享 crate；CLI 提供 `omy remote telegram advanced-search <位置>
--dir tg:<chat> --query-file <JSON路径> --limit 100`。JSON 使用同一 `{ match, conditions }`
条件树，可以表达分组与取反，不再另写一套 CLI 过滤器。

批量全选动作仍只提供 GUI：它依赖驻留的父子任务与目标选择、暂停/继续/重试管理。
CLI 的已有 `forward` / `copy` / `cache pin` / 虚拟引用命令继续可用，覆盖表明确分开记录。

## 7. 测试

- 纯逻辑：`tg-advanced-search.test.ts`（扩展名规整、大小换算、日期换算、子组嵌套与空组、校验、选择计数）；
  `calendar.test.ts`（月末夹取、闰年、周起始、当天首末秒）。
- 编排：`TelegramAdvancedSearchDialog.test.ts`（分页 cursor、全选 + except、动作用上次搜索的查询、
  受保护对话禁用转发、子组取反、点连接词切换且/或）。
- 控件：`AppSelect.test.ts`、`AppDatePicker.test.ts`（键盘、Esc 不外泄、min/max、滚动收起）。
- 浏览器预览：`dev-mock.ts` 用 230 条假历史按条件筛选分页返回。
- 只有正向媒体/文字可下推时才用 `messages.search`；仅日期或纯本地条件仍用
  `getHistory`，避免 `filterEmpty` 在部分超级群漏历史。日期在已有搜索路径中向外扩一秒下推，
  再由本地闭区间求值，超过 Telegram i32 日期范围时不截断。
- 每页至多 300 条实际返回消息，命中上限 1–100；空页游标非空时继续。游标绑定对话与查询指纹。

### 本次验证

- 自动化：Rust 工作区测试、Clippy `-D warnings`、前端 234 项测试、类型检查、i18n 键检查与中英文站点构建。
- 真实界面/账号：未验证。本机桌面停在 Windows 登录画面，人工接管被跳过；未执行转发或上传测试，
  未对用户真实对话产生测试写入。测试覆盖不替代真实账号端到端验证。

- 后端单测已覆盖递归组与叶子取反、日期/大小闭区间、无文件消息、空页游标与累计进度、
  全选 except、跨对话选择拒绝、230 条跨批转发排序、暂停/继续/取消、扫描期父任务状态、
  虚拟引用重试幂等性。
- 仍需真实账号上的端到端验证，以及故障注入下的扫描失败续跑/动作重试验证；未声称已实测。
