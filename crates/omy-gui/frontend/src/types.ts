/**
 * omy-gui 前端的核心数据模型（与后端 Tauri 命令返回的 JSON 对应）。
 *
 * 这里放的是**跨多个组件/状态流转共享**的实体形状；只在单个对话框内用一次的
 * 局部载荷暂不建类型（随该模块重构再补），避免一次性铺一大堆没人引用的接口。
 *
 * 迁移现状：state 是 reactive({...}) 无泛型（字段数百个），暂不强推整体类型；
 * 消息时间线（这次 TS 化的重点）与文件条目是最值得先钉住的两条链。
 */

/** 一条 Telegram 消息行（后端 telegram::store::MessageRow 的投影）。
 *  字段大多可空：纯文本消息没有 file_*；没有缩略图/时长/回复时对应字段为 null。 */
export interface MessageRow {
  /** 对话内单调递增的消息号。 */
  message: number;
  /** 消息文本（可能为空串）。 */
  text: string;
  /** UNIX 秒。 */
  date: number;
  /** 是否是当前账号发出的。 */
  outgoing: boolean;
  /** 关联文件在文件视图里的条目 id（形如 tg:<chat>:<msg>）；纯文本为 null。 */
  file_id: string | null;
  file_name: string | null;
  file_size: number | null;
  /** 内嵌 JPEG 缩略图的字节（后端序列化成数字数组）；无则 null。 */
  thumb: number[] | null;
  /** 视频时长（秒）。 */
  duration: number | null;
  /** 这条消息回复的那条消息号；不是回复则 null。 */
  reply_to: number | null;
  /** 带文件时该文件所在分栏 media/file/link/audio/gif；纯文本为 null。 */
  media_tab: string | null;
}

/** remote_messages_around 的返回：以某条为中心的一段消息（新→旧）+ 双向游标。 */
export interface MessageWindow {
  /** 降序（新→旧）的一段消息。 */
  rows: MessageRow[];
  /** 段内最旧一条的消息号（继续向旧翻的 before 游标）。 */
  oldest: number | null;
  /** 段内最新一条的消息号（继续向新翻的 after 游标）。 */
  newest: number | null;
  /** 目标是否真的在段内（已删/超范围为 false）。 */
  found: boolean;
  /** 旧方向是否取满一页（可能还有更旧）。 */
  has_older: boolean;
  /** 新方向是否取满一页（可能还有更新）。 */
  has_newer: boolean;
}

/** 远程（云盘/Telegram）目录条目。文件与目录、明文与加密、虚拟引用都复用它，
 *  所以除 id/name 外大多数字段在特定形态下才有效（见各处 is_* 判据）。 */
export interface RemoteEntry {
  id: string;
  name: string;
  size?: number;
  plaintext_size?: number | null;
  mtime?: number | null;
  is_dir: boolean;
  /** 加密（.omy）条目。 */
  is_encrypted?: boolean;
  /** 已在本会话解锁（real_name 等才有值）。 */
  unlocked?: boolean;
  /** 解密后的真实文件名（仅 unlocked）。 */
  real_name?: string | null;
  /** 后台识别中（边扫边出的骨架）。 */
  probing?: boolean;
  /** 识别失败（网络），整卡可点重试。 */
  probe_failed?: boolean;
  /** Telegram 对话列表行标记（根目录列的是对话不是文件夹）。 */
  is_conversation?: boolean;
  /** 缩略图拉取令牌。 */
  thumb_token?: string | null;
  /** 虚拟远程里的引用条目（指向真实位置的文件）。 */
  is_ref?: boolean;
  source_state?: string | null;
  source_place?: string | null;
  source_dir?: string | null;
  source_file?: string | null;
  /** 源文件所在 Telegram 分栏（真实条目 media_tab；虚拟引用快照 source_media_tab）。 */
  media_tab?: string | null;
  source_media_tab?: string | null;
  [k: string]: unknown;
}

export interface TelegramForwardTarget {
  dir_id: string;
  title: string;
  kind: 'user' | 'group';
}

/** 消息窗口化列表里拍平后的一行：日期组标题或一条消息。 */
export type MessageListItem =
  | { type: 'group'; key: string; label: string }
  | { type: 'msg'; key: string; m: MessageRow };

/** 第三方密码管理器连接状态；不包含任何凭据内容。 */
export interface PasswordManagerStatus {
  provider: 'keepassxc' | 'android_credential_manager';
  installed: boolean;
  running: boolean;
  database_open: boolean;
  associated: boolean;
  database_hash: string | null;
  version: string | null;
  detail: string | null;
}

/** 可以安全进入 WebView 的凭据摘要；真正的 password 只留在 Rust 后端。 */
export interface PasswordManagerCredential {
  id: string;
  name: string;
  login: string;
  group: string;
}

/* ---------------- Telegram 对话高级搜索 ----------------
 * 前端界面先行、后端待实现：下面是 telegram_advanced_search / telegram_advanced_batch
 * 两个命令的载荷契约，设计说明见 docs/research/19-telegram-advanced-search.md。
 * 改这里的字段，那份文档与 dev-mock.ts 里的假实现要一起改。 */

/** 可以按「是否包含」过滤的消息媒体种类。与 Telegram 服务端的消息过滤器一一对应，
 *  后端据此尽量下推到 messages.search 的 filter，而不是全部拉回本地再筛。 */
export type TgMediaKind = 'photo' | 'video' | 'gif' | 'audio' | 'voice' | 'file' | 'link';

/** 单个搜索条件。每种都带 negate：true 表示「不满足这一条」才命中。 */
export type TgSearchCondition =
  /** 消息带的媒体属于其中任一种（negate：不属于其中任何一种，纯文本消息也算命中）。 */
  | { kind: 'media'; types: TgMediaKind[]; negate: boolean }
  /** 文件扩展名（小写、不带点）属于其中任一个；没有文件的消息不命中（negate 时命中）。 */
  | { kind: 'ext'; exts: string[]; negate: boolean }
  /** 文件字节数落在 [min, max] 闭区间，任一端为 null 表示不限；没有文件的消息不命中。 */
  | { kind: 'size'; min: number | null; max: number | null; negate: boolean }
  /** 消息正文或媒体说明包含这段文字（不区分大小写）。 */
  | { kind: 'text'; text: string; negate: boolean };

/** 一次高级搜索的查询：条件按 match 组合，all = 全部满足，any = 任一满足。 */
export interface TgAdvancedQuery {
  match: 'all' | 'any';
  conditions: TgSearchCondition[];
}

/** 一条命中结果。id 与文件视图的条目 id 同构（tg:<chat>:<msg>），批量动作与
 *  「定位源消息」都靠它对回原条目。 */
export interface TgSearchHit {
  id: string;
  message: number;
  date: number;
  text: string;
  media: TgMediaKind | null;
  file_name: string | null;
  file_size: number | null;
  /** 缩略图令牌，拼 /pthumb/<token>；没有缩略图为 null。 */
  thumb_token: string | null;
}

/** telegram_advanced_search 的一页结果。后端按时间从新到旧扫描历史，
 *  每页返回这一段里命中的条目；cursor 为 null 表示已扫到对话开头。 */
export interface TgSearchPage {
  hits: TgSearchHit[];
  cursor: string | null;
  /** 截至这一页累计扫描过的消息条数，用来给用户显示进度。 */
  scanned: number;
}

/** 批量动作的选择范围。
 *  - ids：只对明确勾选的这些条目执行；
 *  - all：对**全部命中**执行，包括还没扫描到的——后端按同一个 query 从头遍历，
 *    跳过 except 里被用户取消勾选的条目。界面上只加载了一部分时也能全选，靠的就是它。 */
export type TgSelection =
  | { mode: 'ids'; ids: string[] }
  | { mode: 'all'; except: string[] };

/** 批量动作种类：永久保留、原生转发、添加到虚拟远程、上传到另一个远程位置。 */
export type TgBatchAction = 'pin' | 'forward' | 'add_virtual' | 'upload_to';
