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

/** 消息窗口化列表里拍平后的一行：日期组标题或一条消息。 */
export type MessageListItem =
  | { type: 'group'; key: string; label: string }
  | { type: 'msg'; key: string; m: MessageRow };
