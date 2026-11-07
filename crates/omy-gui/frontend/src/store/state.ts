/** 全局响应式状态与底部提示。其余 store 模块都只依赖它，它不依赖任何一个——
 * 保证 barrel 先求值它，后面模块顶层的 watch 立即读 state 时已经就绪。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import { reactive } from 'vue';
import { isAndroid } from '../mobile-platform';
import type { SortKey } from './local';

/** 本平台有没有系统回收站。与后端 `move_to_trash` 的 cfg 对应：Android 没有。 */
export const trashSupported = !isAndroid;

export const state = reactive({
  /** 当前目录路径。空串表示还在「起点」页。 */
  cwd: '',
  /** 当前目录的条目（目录 + 文件）。 */
  entries: [],
  /** 侧栏的起点：常用目录与磁盘根。 */
  places: [],
  /** 已注册的远程位置，每项含 `caps` 能力位图。 */
  remotePlaces: [],
  /** 虚拟远程位置（本地收藏夹式），每项 { id, name }。侧栏「远程」区列在真实位置之后。 */
  virtualPlaces: [],
  /** 「添加到虚拟远程」树形选择对话框的待添加条目（真实位置里的文件）。
   *  { placeId, dirId, items:[{fileId,name,size}] }；null=对话框关闭。 */
  addToVirtual: null,
  /** “上传至”目标选择对话框。source 为 local 或 remote，确认后进入统一传输页。 */
  uploadTo: null,
  /** Telegram 原生转发对话框上下文：同账号、同源对话的一组消息号。 */
  telegramForward: null,
  /** 消息时间线的多选消息号；切位置、目录或分栏必须清空。 */
  remoteMessageSelected: [],
  /** 当前所在的远程位置 id；为空表示在本地。 */
  remotePlace: '',
  /** 远程位置里的当前目录。 */
  remoteDir: '',
  /** 远程目录的条目，已附带识别结果。 */
  remoteItems: [],
  /**
   * 当前远程**目录**的有效能力；`null` 表示还没查到（或查失败）。
   *
   * 与 `remotePlaces[].caps` 分开存是刻意的：那个是位置级**上界**，这个是
   * 「在这个目录里实际能做什么」。同一个位置里有的目录可写、有的只读
   * （「对话即目录」的位置就是这样），只读上界会在只读目录里点亮必然失败的
   * 删除与上传。
   *
   * 切目录时必须先置回 `null` 再去查：留着上一个目录的值会让新目录短暂显示
   * 错误的能力——而用户完全可能在那一瞬间点下去。
   */
  remoteDirCaps: null,
  /** Telegram 群组“全部缓存”的筛选弹窗上下文。 */
  telegramCacheAll: null,
  /** 传输任务快照（下载 / 上传 / 永久保留三类汇总）。 */
  transfers: [],
  /** 传输管理页是否打开。它是顶层页，与文件浏览、云盘并列。 */
  transfersOpen: false,
  /** 当前对话是否开了「受保护内容」。只用于显示一行低权重告知。 */
  remoteProtected: false,
  /** 正在单独重试探测的远程条目 id 集合（「未能读取」点击重试中转 ⏳）。 */
  remoteRetrying: [],
  /**
   * 单文件密文块缓存覆盖情况，键为 `${placeId}\u{1}${path}`，值为后端
   * FileCacheStat（cached_blocks/total_blocks/cached_bytes/fully_cached）。
   * 只在打开条目菜单时按需查询——列目录逐文件 stat 数千块会拖慢扫描，
   * 故不做成常驻卡片角标。
   */
  remoteCacheStat: {},
  /** 云盘（远程位置）浏览器是否打开。与局域网对端的 remoteMode 平行。 */
  placeBrowserOpen: false,
  /** 远程目录列表/打开过程中的局部错误（区别于全局 error，不弹底部条）。 */
  placeError: '',
  /**
   * 存储访问权限状态：`{ granted, mode }`。
   *
   * 初值刻意设成 `granted: true`：绝大多数平台（桌面）确实如此，
   * 而首次查询是异步的。设成 false 的话每次启动都会先闪一下授权提示，
   * 然后在查询返回后消失。
   *
   * `mode` 为 `not-applicable` 时前端完全不显示授权相关的界面。
   */
  storage: { granted: true, mode: 'not-applicable' },
  /** 已识别的加密文件，按路径索引，用于预览。 */
  known: {},
  /** 选中的路径集合。 */
  selected: [],
  /** 视图模式。 */
  view: 'grid',
  /** 本地列表的排序：字段与方向。按观看者记在 localStorage（见 loadSort）。 */
  sortKey: 'name' as SortKey,
  sortDir: 'asc' as 'asc' | 'desc',
  /** 是否在列表中显示缩略图；由设置页 ui.thumbnails 驱动，App 启动/保存设置时同步。 */
  showThumbnails: true,
  /** 搜索词。 */
  query: '',
  /** 搜索模式：'local' 本地过滤（默认）/ 'server' 服务端搜索。
   *
   * **默认必须是 local**：服务端搜索会把搜索词发到 Telegram 的服务器上，
   * 用户搜「离婚协议」这个词就出去了。所以它是一个要显式切换、切换后能看到
   * 提示的模式，而不是悄悄发生的默认行为。
   */
  searchMode: 'local',
  /** 对话内的渲染方式：'files' 文件网格（默认）/ 'messages' 消息时间线。
   *
   * 默认 files：omy 是文件管理器，消息视图是辅助找文件的手段而不是主线
   * （文档 §1.3）。
   */
  remoteViewMode: 'files',
  /** 对话内文件视图的**媒体分栏**：media/file/link/audio/gif。
   *
   * 官方客户端进对话看的是分类型的媒体页，而不是「所有文件」一锅端。
   * 默认 media：图片视频是对话里最常翻的东西。只在 Telegram 对话内的
   * 文件视图下有意义（消息视图、根目录、网盘都不用它）。 */
  remoteTab: 'media',
  /** 当前所在目录的**显示名**。
   *
   * 面包屑不能直接拿 remoteDir 当名字：那是 provider 的 id。WebDAV 的 id
   * 恰好是人可读的路径，Telegram 的是 `tg:<对话>`——直接显示会让用户看到
   * 一串数字，完全不知道自己在哪个对话里。
   *
   * 进目录时由调用方把列表里那个名字传进来（它本来就有）。
   */
  remoteDirName: '',
  /** 消息视图的数据。与 remoteItems 分开：一个是「这个对话里有哪些文件」，
   *  一个是「这个对话里发生过什么」，来源和生命周期都不同。 */
  remoteMessages: [],
  /** 引用跳转 / 定位源消息命中的那条消息号；高亮它、滚到它。null 表示无高亮。 */
  highlightMsg: null,
  /** 「在文件中显示 / 定位到源文件」命中的文件条目 id；网格/列表高亮它、滚到它。 */
  highlightFile: null,
  /** 远程浏览（含虚拟位置）的多选集合：条目 id 列表。空数组表示没在多选。
   *  桌面 Ctrl/⌘+点、移动端长按进入；仅当前目录/分栏有效，切目录/分栏/位置即清空。 */
  remoteSelected: [],
  /** 正在为哪个**虚拟位置**弹加密对话框。{id,name} 或 null。 */
  vEncryptFor: null,
  vEncryptBusy: false,
  /** 正在为哪个锁定的虚拟位置弹解锁框。{id,name} 或 null。 */
  vUnlockFor: null,
  vUnlockBusy: false,
  vUnlockError: '',
  /** 虚拟位置条目的剪贴板：{ mode:'cut'|'copy', placeId, items:[RemoteEntry] }。
   *  只在虚拟位置之间粘贴——真实远程不接受引用条目。 */
  virtualClipboard: null,
  /** 正在为哪个 Telegram 位置弹加密对话框。{id,name} 或 null。 */
  tgEncryptFor: null,
  /** 加密对话框是否正在提交（跑 Argon2 派生 + 写盘时置忙）。 */
  tgEncryptBusy: false,
  /** 正在为哪个锁定的加密 Telegram 位置弹解锁对话框。{id,name} 或 null。 */
  tgUnlockFor: null,
  /** 解锁对话框是否正在提交（跑 Argon2 派生时置忙）。 */
  tgUnlockBusy: false,
  /** 解锁上一次输错的提示文案（非空即在密码框下红字）。 */
  tgUnlockError: '',
  /** 还有没有更多文件可加载。 */
  hasMoreFiles: false,
  /** 正在加载更多文件。 */
  loadingMoreFiles: false,
  /** 后端一页多少条。用来判断「取满了就可能还有」。 */
  pageSize: 100,
  /** 还有没有更早的消息可加载（向下/向旧）。取回的条数少于一页就说明到头了。 */
  hasMoreMessages: false,
  /** 还有没有更新的消息可加载（向上/向新）。引用跳到历史中间时为 true。 */
  hasNewerMessages: false,
  /** 正在加载更早。 */
  loadingMore: false,
  /** 正在加载更新（列表顶部续翻）。 */
  loadingNewer: false,
  /** 消息视图正在加载（仅首次进栏/从文件栏切来等整屏加载时为 true；
   *  引用跳转复用已在屏的时间线时**不**置它，避免整个内容区闪成加载态）。 */
  loadingMessages: false,
  /** 引用跳转正在为哪条消息取上下文；非 null 时只在那个引用按钮上转小圈，
   *  不动整个内容区。null 表示当前没有内联定位在飞。 */
  locatingMsg: null,
  /** 服务端搜索返回的候选集。与 remoteItems 分开存。
   *
   * 不复用 remoteItems：那是「当前目录里有什么」，而搜索结果可能来自别的
   * 对话。混在一起的话退出搜索时无从恢复原来那一屏。
   */
  searchResults: [],
  /** 这批服务端结果是用哪个词搜出来的，用于提示条如实显示。 */
  searchedQuery: '',
  /** 服务端搜索进行中。 */
  searching: false,
  /** 会话里的凭据数量。0 表示没有任何密码，但**不影响浏览**。 */
  credentials: 0,
  /** 正在忙（扫描 / 派生 / 加密）。 */
  busy: false,
  /** 忙碌提示文案的键。 */
  busyKey: '',
  /** 加密进度。null 表示当前没有在加密。
   *
   * 单独一个对象而不是摊平成几个字段：它整体有效或整体无效，
   * 摊开后容易出现「换了文件但百分比还是上一个的」这种半旧状态。
   */
  progress: null,
  /** 错误文案。 */
  error: '',
  /** 错误的逐条明细（例如部分失败时是哪些文件）。
   *
   * 必须在这里声明：Vue 的响应式只跟踪初始化时就存在的属性，事后
   * `state.x = []` 赋上去的字段不会触发重渲染——界面永远是空的，而
   * console 里看 state 又确实有值，极难排查。
   */
  errorDetails: [],
  /**
   * 重试所需的上下文：上次失败的完整路径，以及那次用的密码。
   *
   * 留着密码是为了让用户不用再输一遍——重试要用**原来的**密码（失败的文件
   * 没被改写），而用户刚输过新密码，让他自己回想很容易填错。代价是密码
   * 多在内存里待一会儿，所以对话框一关就清掉
   */
  retry: null,
  /** 提示文案（成功类）。 */
  notice: '',
  /** 协议前缀，由后端按平台下发。 */
  streamBase: 'omystream://localhost',
  /** 已配对设备数，侧栏角标用。 */
  pairedCount: 0,
  /** 是否正在共享，侧栏据此显示指示灯。 */
  shareRunning: false,

  /** 正在浏览的加密文件夹（容器），null 表示在看磁盘目录。
   *
   * `{ entryId, name, items, cwd }`：`items` 是后端 `list_container`
   * 返回的**扁平**全路径列表，`cwd` 是容器内的当前子目录（相对容器根，
   * 空串为根）。
   *
   * # 为什么放进主状态而不是一个弹窗组件的局部 ref
   *
   * 「进入一个加密文件夹」在用户眼里就是进入一个文件夹，不该换一套
   * 界面。把它做成一个**位置**，主界面的面包屑、网格/列表、双击预览
   * 就能原样复用——否则容器视图会长成第二套文件列表，两边的排序、
   * 图标、预览行为迟早分歧。
   */
  container: null,

  /* ---- 远端浏览 ---- */

  /** 已连接的对端信息。null 表示没连。 */
  peer: null,
  /** 远端文件列表。 */
  remoteEntries: [],
  /** 是否正在看远端（而不是本机文件）。
   *
   * 这是**视图切换**不是权限开关：断开时回到本机目录，
   * 本机的浏览能力任何时候都不受影响。
   */
  remoteMode: false,
});

/** 设置一条成功提示，若干秒后自动消失。
 *
 * 只有**成功**类提示自动消失。错误必须留在屏幕上等用户主动关掉——
 * 自动消失的错误等于没报错：用户很可能正低头看别处，回头只看到
 * 操作「好像没反应」。
 *
 * 重复调用会取消上一个计时器，否则连续两次操作时，第一次的计时器
 * 会把第二条提示提前撤掉。
 */
let noticeTimer: number = 0;
export function setNotice(text, ms = 4000) {
  state.notice = text;
  if (noticeTimer) window.clearTimeout(noticeTimer);
  noticeTimer = 0;
  if (!text) return;
  noticeTimer = window.setTimeout(() => {
    state.notice = '';
    noticeTimer = 0;
  }, ms);
}

/** 立刻清掉提示（用户点 ✕ 时调用）。 */
export function clearNotice() {
  if (noticeTimer) window.clearTimeout(noticeTimer);
  noticeTimer = 0;
  state.notice = '';
}
