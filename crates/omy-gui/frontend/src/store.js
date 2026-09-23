/** 应用状态。
 *
 * # 交互模型：文件管理器优先，密码按需
 *
 * 早先的模型是「先解锁才能进门」：启动就是一个密码框，输对了才看得到
 * 任何东西。这假设了用户已经是老用户——可新用户第一次打开时还没有
 * 任何加密文件，他要做的第一件事恰恰是**挑几个文件来加密**。
 *
 * 现在的模型：
 *
 * - 启动直接进文件管理器，能浏览任何目录，不需要密码
 * - 加密文件在列表里标出来，双击才问密码
 * - 顶栏有个统一密码框，输了就对当前目录批量试解锁
 *
 * 所以这里**没有** `unlocked` 这个全局开关。会话里有几个凭据是
 * `credentials`，那只影响「哪些加密文件能看见真名」，不影响能不能
 * 用这个应用。
 *
 * # 锁定态仍然是数据
 *
 * 文档 §10 那条结论没变：每个加密文件自己的 `unlocked` 决定它怎么显示，
 * 不能用「统一重设样式」实现。锁定时后端清空已识别列表，
 * 前端拿到的条目就都是锁定态。
 */

import { reactive, computed, watch } from 'vue';
import * as api from './api.js';
import * as i18n from './i18n.js';

export const state = reactive({
  /** 当前目录路径。空串表示还在「起点」页。 */
  cwd: '',
  /** 当前目录的条目（目录 + 文件）。 */
  entries: [],
  /** 侧栏的起点：常用目录与磁盘根。 */
  places: [],
  /** 已注册的远程位置，每项含 `caps` 能力位图。 */
  remotePlaces: [],
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
  /** 还有没有更多文件可加载。 */
  hasMoreFiles: false,
  /** 正在加载更多文件。 */
  loadingMoreFiles: false,
  /** 后端一页多少条。用来判断「取满了就可能还有」。 */
  pageSize: 100,
  /** 还有没有更早的消息可加载。取回的条数少于一页就说明到头了。 */
  hasMoreMessages: false,
  /** 正在加载更早。 */
  loadingMore: false,
  /** 消息视图正在加载。 */
  loadingMessages: false,
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
let noticeTimer = 0;
export function setNotice(text, ms = 4000) {
  state.notice = text;
  if (noticeTimer) clearTimeout(noticeTimer);
  noticeTimer = 0;
  if (!text) return;
  noticeTimer = setTimeout(() => {
    state.notice = '';
    noticeTimer = 0;
  }, ms);
}

/** 立刻清掉提示（用户点 ✕ 时调用）。 */
export function clearNotice() {
  if (noticeTimer) clearTimeout(noticeTimer);
  noticeTimer = 0;
  state.notice = '';
}

/** 拼出远端文件的内容 URL。 */
export function remoteFileUrl(id) {
  return `${state.streamBase}/rfile/${encodeURIComponent(id)}`;
}

/** 拼出远端文件的缩略图 URL。 */
export function remoteThumbUrl(id) {
  return `${state.streamBase}/rthumb/${encodeURIComponent(id)}`;
}

/** 拼出远程位置（WebDAV 等）已打开文件的内容 URL。
 *
 * token 由 remotePlaceOpen 颁发，对应后端一个带密文块缓存的来源；
 * 多次 Range 请求复用它，seek 才不会重复下载。
 */
export function placeFileUrl(token) {
  return `${state.streamBase}/pfile/${encodeURIComponent(token)}`;
}

/** 拼出远程位置文件的缩略图 URL。 */
export function placeThumbUrl(token) {
  return `${state.streamBase}/pthumb/${encodeURIComponent(token)}`;
}

/** 连接一台设备并进入远端视图。 */
export async function connectRemote(fingerprint, addr) {
  state.busy = true;
  state.busyKey = 'busy.connecting';
  state.error = '';
  try {
    state.peer = await api.remoteConnect(fingerprint, addr || null);
    state.remoteEntries = await api.remoteList();
    state.remoteMode = true;
    state.selected = [];
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.connect_failed'));
    state.peer = null;
    state.remoteMode = false;
    return false;
  } finally {
    state.busy = false;
  }
}

/** 断开远端，回到本机视图。 */
export async function disconnectRemote() {
  await api.remoteDisconnect().catch(() => {});
  state.peer = null;
  state.remoteEntries = [];
  state.remoteMode = false;
}

/** 重新拉远端列表。 */
export async function reloadRemote() {
  if (!state.remoteMode) return;
  state.busy = true;
  state.busyKey = 'busy.loading';
  try {
    state.remoteEntries = await api.remoteList();
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.busy = false;
  }
}

/** 在远端视图里试一个密码。
 *
 * 远端没有「目录」可探测 vault，所以不能走 `unlockDirectory`。
 * 这里直接用远端文件头里的 salt 派生——头部在列表里已经有了。
 */
export async function tryUnlockRemote(password) {
  if (!state.remoteMode) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const vaults = await api.remoteVaults();
    if (!vaults.length) {
      state.error = i18n.te('no_vault_found');
      return false;
    }
    const r = await api.unlock('main', password, vaults);
    state.credentials = r.credentials;
    state.remoteEntries = await api.remoteRelock();
    const opened = state.remoteEntries.filter((f) => f.unlocked).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      state.error = i18n.te('wrong_password');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 刷新设备相关的概览状态（侧栏角标与共享指示）。
 *
 * 只取计数不取明细：侧栏不需要设备名，而少取一层就少一处泄露面。
 */
export async function refreshDeviceOverview() {
  try {
    const st = await api.deviceStatus();
    state.pairedCount = st.opened ? st.paired_count : 0;
    const sh = await api.shareStatus();
    state.shareRunning = sh.running;
  } catch {
    // 设备库不可用不该影响文件浏览
    state.pairedCount = 0;
    state.shareRunning = false;
  }
}

/** 拼出某个文件的内容 URL。 */
export function fileUrl(id) {
  return `${state.streamBase}/file/${encodeURIComponent(id)}`;
}

/** 拼出未加密文件的内容 URL。
 *
 * 走的是同一条 `omystream://` 协议，只是换个前缀。为什么不直接用
 * 文件路径：那需要开启 Tauri 的 asset 协议，等于把整个文件系统
 * 暴露给 WebView 里的 JS。
 */
export function plainUrl(token) {
  return `${state.streamBase}/plain/${encodeURIComponent(token)}`;
}

/** 拼出容器内单个文件的内容 URL。
 *
 * 同样只换前缀。token 由后端 `list_container` 登记后下发，前端**拿不到**
 * 「按偏移读容器任意位置」的能力——那个约束在后端。
 */
export function containerItemUrl(token) {
  return `${state.streamBase}/citem/${encodeURIComponent(token)}`;
}

/** 用系统默认程序打开。 */
export async function openWithSystem(entry) {
  if (!entry?.token) return false;
  try {
    await api.openExternal(entry.token);
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('open_failed'));
    return false;
  }
}

/** 在系统文件管理器里定位。 */
export async function revealEntry(entry) {
  if (!entry?.token) return false;
  try {
    await api.revealInFolder(entry.token);
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('open_failed'));
    return false;
  }
}

/** 拼出某个文件的缩略图 URL。 */
export function thumbUrl(id) {
  return `${state.streamBase}/thumb/${encodeURIComponent(id)}`;
}

/** 过滤并排序后的条目。
 *
 * 排序在后端已按自然序做过（目录在前）。这里只做搜索过滤，
 * 不重排——否则会和后端的顺序打架。
 */
export const visibleEntries = computed(() => {
  // 服务端搜索模式：列的是候选集，而不是当前目录。
  //
  // 仍然要过一遍本地精筛——这就是原型里的「两段式搜索」：服务端按文字缩小
  // 范围（快、省流量），本地再按**解出来的真实文件名**匹配。服务端搜的是
  // 消息文字，omy 加密文件的真实文件名它永远没有，所以少了本地这一段，
  // 搜索对加密文件就完全失效。
  const source = state.searchMode === 'server' && state.remotePlace
    ? state.searchResults
    : state.container
      ? containerEntries.value
      : state.entries;
  const q = state.query.trim().toLowerCase();
  if (!q) return source;
  return source.filter((e) => {
    // 锁定的加密文件与加密目录都没有可搜的名字，搜索时直接排除：
    // 用磁盘名去匹配等于拿 base32 密文当明文搜，还会泄露信息
    if ((e.is_encrypted || e.is_encrypted_dir) && !e.unlocked) return false;
    const name = e.real_name || e.name;
    return name.toLowerCase().includes(q);
  });
});

/** 当前视图里的条目总数（状态栏用）。
 *
 * 不能直接用 `state.entries.length`：在容器里那是**外层磁盘目录**的
 * 数量，与眼前列的东西无关。
 */
export const currentCount = computed(() =>
  state.container ? containerEntries.value.length : state.entries.length,
);

/** 当前目录里加密内容的数量。容器内恒为 0：里面的东西已经解出来了。
 *
 * 加密**目录**也要算：一个只含树形加密目录的文件夹否则会显示
 * 「0 个加密文件」，而用户眼前明明有一个带锁的加密文件夹。
 */
export const encryptedCount = computed(() =>
  state.container
    ? 0
    : state.entries.filter((e) => e.is_encrypted || e.is_encrypted_dir).length,
);

/** 其中还锁着的数量。加密目录同样计入，理由见 `encryptedCount`。 */
export const lockedCount = computed(() =>
  state.container
    ? 0
    : state.entries.filter(
        (e) => (e.is_encrypted || e.is_encrypted_dir) && !e.unlocked,
      ).length,
);

/** 选中项里可加密的（排除已经是加密文件的）。
 *
 * 容器内恒为空：那些条目没有磁盘路径，加密命令收的是路径。
 * 在这里返回空数组，「加密」按钮自然就不出现——比让按钮出现
 * 然后点了报错要好。
 */
export const encryptable = computed(() =>
  state.container
    ? []
    : state.selected
        .map((p) => state.entries.find((e) => e.path === p))
        .filter((e) => e && !e.is_encrypted),
);

/** 选中项里可以还原到磁盘的（已加密且已解锁的）。
 *
 * 与 `encryptable` 互补：一个挑还没加密的，一个挑已经加密的。
 *
 * 为什么要求 `unlocked`：还原需要密钥，而密钥来自会话。锁着的文件
 * 即使选中了也解不开，让按钮出现然后点了报 `locked` 不如不出现。
 *
 * 容器内恒为空：里面的条目没有独立的文件 id，还原命令收的是 id。
 * 真要取出容器里的单个文件，是另一件事（预览面板的「导出」），
 * 不该混进这个批量入口。
 */
export const restorable = computed(() =>
  state.container
    ? []
    : state.selected
        .map((p) => state.entries.find((e) => e.path === p))
        .filter((e) => e && e.is_encrypted && e.unlocked),
);

/** 选中项里可以管理密码的那**一个**。
 *
 * 只在恰好选中一个已解锁的加密文件时返回它，否则返回 null。
 *
 * # 为什么不支持批量
 *
 * 每个文件的 vault salt 可能不同，同一个密码在不同文件上派生出的 KEK
 * 就不同；而「这个文件配了几个密码」查不出来，批量操作时无法逐个确认
 * 结果。真要批量改，一半成功一半失败时用户手里就是一堆状态不明的文件。
 *
 * # 为什么要求已解锁
 *
 * 改密码必须先能打开这个文件（要取出 FEK 重新包裹）。锁着的文件先走
 * 正常的解锁流程，解开后这个入口自然出现——而不是在密码管理对话框里
 * 再套一层解锁。
 *
 * 容器内的条目恒为空：它们没有独立的磁盘文件，密码在外层容器上。
 *
 * # 树形加密的目录也算
 *
 * 加密目录的 `is_encrypted` 恒为 false（那个字段的语义是「内容是密文」，
 * 目录的内容是子项），要看 `is_encrypted_dir`。只判断前者的话，选中一个
 * 加密目录时 🔑 按钮压根不出现，用户得不到任何提示。
 */
export const keyManageable = computed(() => {
  if (state.container) return null;
  if (state.selected.length !== 1) return null;
  const e = state.entries.find((x) => x.path === state.selected[0]);
  if (!e || !e.unlocked) return null;
  return e.is_encrypted || e.is_encrypted_dir ? e : null;
});

/** 当前可管理密码的目标是不是树形加密的目录。
 *
 * 决定对话框只给 change：一棵树同时只能有一个密码（目录名由第一个 KEK
 * 派生），add 出来的第二个密码能开文件却解不开目录名，解密会报「文件
 * 损坏」。
 */
export const keyTargetIsTree = computed(() => !!keyManageable.value?.is_encrypted_dir);

/** 能不能给它生成恢复码：已解锁的单个加密文件或加密文件夹。
 *
 * 目录曾经被排除，理由是「树里每个文件各自挂槽，共用一份还是各一份
 * 未定」。现在定了：整棵树共用一份，由 rekey_tree 统一改写。
 * 「整棵树共用一份还是各一份」还没定，后端会如实拒绝。既然做不到，
 * 菜单项就该是灰的——让用户点进去再看到报错，不如一开始就说清楚。
 */
export const recoveryGeneratable = computed(() => {
  const e = keyManageable.value;
  return e;
});

/** 能不能对它使用恢复码：单个加密文件或加密文件夹，锁着或开着都行。
 *
 * 这里刻意**不要求** unlocked，而 keyManageable 要求。原因是这两个
 * 功能面对的处境正好相反：管理密码的前提是你还记得密码，而用恢复码
 * 的前提是你已经忘了。若照抄 keyManageable 的判据，菜单项只在文件
 * 已解锁时才亮——那时用户根本不需要它，真正需要的时候反而是灰的。
 */
export const recoveryUsable = computed(() => {
  if (state.container) return null;
  if (state.selected.length !== 1) return null;
  const e = state.entries.find((x) => x.path === state.selected[0]);
  if (!e || (!e.is_encrypted && !e.is_encrypted_dir)) return null;
  return e;
});

/* ---------------- 右键菜单 ---------------- */

/** 右键菜单的状态。`entry` 为 null 表示菜单没开。 */
export const ctxMenu = reactive({ entry: null, x: 0, y: 0 });

export function openContextMenu({ entry, x, y }) {
  ctxMenu.entry = entry;
  ctxMenu.x = x;
  ctxMenu.y = y;
}

export function closeContextMenu() {
  ctxMenu.entry = null;
}

/** 当前右键菜单该显示哪些项。
 *
 * 用 disabled 而不是直接隐藏：菜单项的位置固定，用户才能形成肌肉记忆。
 * 每次右键都换一套项目、位置浮动，会让人反复找「删除在哪一行」。
 * 不可用的原因写进 hint，鼠标悬停能看到——否则用户只知道点不动，
 * 不知道为什么。
 *
 * 容器内的条目全部不可操作：它们没有独立的磁盘文件，删一个「条目」
 * 得重写整个容器，这不是右键菜单该做的事。
 */
export const ctxItems = computed(() => {
  const e = ctxMenu.entry;
  if (!e) return [];
  const t = i18n.t;
  const inContainer = !!state.container;
  const many = state.selected.length > 1;

  // 容器内只留一项说明，不给任何会失败的操作
  if (inContainer) {
    return [
      {
        key: 'noop-container',
        icon: 'ℹ️',
        label: t('ctx.in_container'),
        disabled: true,
        hint: t('ctx.in_container_hint'),
      },
    ];
  }

  const items = [];

  // 打开：目录进去，文件预览
  items.push({
    key: 'open',
    icon: e.is_dir ? '📂' : '👁️',
    label: e.is_dir ? t('ctx.open_folder') : t('ctx.preview'),
    disabled: many,
    hint: many ? t('ctx.single_only') : '',
  });

  items.push({ key: 'sep' });

  // 加密：非加密项才行。加密目录本身已经是密文，再套一层没有意义
  const canEncrypt = encryptable.value.length > 0;
  items.push({
    key: 'encrypt',
    icon: '🔒',
    label: t('file.encrypt'),
    disabled: !canEncrypt,
    hint: canEncrypt ? '' : t('ctx.already_encrypted'),
  });

  // 还原：已解锁的加密文件
  const canRestore = restorable.value.length > 0;
  items.push({
    key: 'restore',
    icon: '📤',
    label: t('file.restore'),
    disabled: !canRestore,
    hint: canRestore ? '' : t('ctx.restore_needs_unlocked'),
  });

  // 密码管理：必须是**已解锁的单个**加密文件
  const canKey = !!keyManageable.value;
  items.push({
    key: 'manage-key',
    icon: '🔑',
    label: t('keymgmt.title'),
    disabled: !canKey,
    hint: canKey ? '' : t('ctx.key_needs_unlocked_file'),
  });

  // 恢复码：生成要已解锁（得先证明你现在能打开），使用则不要求——
  // 会用到它正是因为密码已经忘了
  const canGenReco = !!recoveryGeneratable.value;
  items.push({
    key: 'recovery-generate',
    icon: '🔐',
    label: t('recovery.menu_generate'),
    disabled: !canGenReco,
    hint: canGenReco ? '' : t('ctx.recovery_needs_unlocked_file'),
  });

  const canUseReco = !!recoveryUsable.value;
  items.push({
    key: 'recovery-restore',
    icon: '🔓',
    label: t('recovery.menu_restore'),
    disabled: !canUseReco,
    hint: canUseReco ? '' : t('ctx.recovery_needs_file'),
  });

  items.push({ key: 'sep' });

  // 重命名：密文目录不行——名字本身就是密文，改掉就再也解不开了
  const isEncDir = !!e.is_encrypted_dir;
  items.push({
    key: 'rename',
    icon: '✏️',
    label: t('ctx.rename'),
    disabled: many || isEncDir,
    hint: isEncDir
      ? t('ctx.rename_encrypted_dir')
      : many
        ? t('ctx.single_only')
        : '',
  });

  items.push({
    key: 'newfolder',
    icon: '📁',
    label: t('ctx.new_folder'),
  });

  items.push({ key: 'sep' });

  items.push({ key: 'trash', icon: '🗑️', label: t('ctx.trash') });
  items.push({ key: 'delete', icon: '⛔', label: t('ctx.delete'), danger: true });

  items.push({ key: 'sep' });

  // 「在文件管理器中显示」需要后端给的 token，锁定的加密文件没有
  items.push({
    key: 'reveal',
    icon: '📍',
    label: t('ctx.reveal'),
    disabled: many,
    hint: many ? t('ctx.single_only') : '',
  });

  return items;
});

/* ---------------- 常规文件操作 ---------------- */

/** 删除选中项。`toTrash` 为 false 是永久删除。
 *
 * 作用于**整个选中集**而不只是右键点的那一项：右键前已经先选中了，
 * 用户看到几个高亮就期望删掉几个。
 */
export async function doDelete(toTrash) {
  const paths = state.selected.slice();
  if (!paths.length) return null;
  state.busy = true;
  try {
    const r = await api.deletePaths(paths, toTrash);
    state.selected = [];
    await reload();
    if (r.failed.length) {
      // 部分失败要说清哪些没删掉。只报「删除失败」的话，用户不知道
      // 20 个里有 19 个已经删了
      state.error = i18n.t('ctx.delete_partial', {
        ok: String(r.deleted.length),
        bad: String(r.failed.length),
      });
    } else {
      setNotice(
        i18n.t(toTrash ? 'ctx.trashed_n' : 'ctx.deleted_n', {
          n: String(r.deleted.length),
        }),
      );
    }
    return r;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return null;
  } finally {
    state.busy = false;
  }
}

/** 重命名一个条目。 */
export async function doRename(path, name) {
  state.busy = true;
  try {
    await api.renamePath(path, name);
    state.selected = [];
    await reload();
    setNotice(i18n.t('ctx.renamed'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 在当前目录下新建文件夹。 */
export async function doCreateFolder(name) {
  if (!state.cwd) return false;
  state.busy = true;
  try {
    await api.createFolder(state.cwd, name);
    await reload();
    setNotice(i18n.t('ctx.folder_created'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/* ---------------- 容器（加密文件夹）浏览 ---------------- */

/** 进入一个加密文件夹，像打开普通文件夹那样。
 *
 * 拿到的是**扁平**的全路径列表，之后由 `visibleEntries` 按当前层级过滤。
 * 一次取全而不是每层问一次：索引本来就是整份解出来的，分层请求只会
 * 让每次进目录都重解一遍容器头部。
 */
export async function enterContainer(entry) {
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.error = '';
  try {
    const items = await api.listContainer(entry.entry_id);
    state.container = {
      entryId: entry.entry_id,
      name: entry.real_name || entry.name,
      items,
      cwd: '',
    };
    state.selected = [];
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('container_failed'));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 进入一个**远程**目录容器。
 *
 * 与本地 `enterContainer` 同构，区别只在拿条目的方式：本地读磁盘，
 * 这里要先 `remotePlaceOpen` 拿句柄（容器明文不在磁盘上）。
 *
 * 复用同一个 `state.container`，于是面包屑、返回、列表渲染全都不用分叉——
 * 另起一套的话「本地修了远程还是老样子」是迟早的事。
 */
export async function enterRemoteContainer(f) {
  if (!state.remotePlace) return false;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  try {
    const opened = await api.remotePlaceOpen(state.remotePlace, f.id, f.size || 0);
    if (!opened || !opened.token) {
      state.placeError = i18n.te('container_failed');
      return false;
    }
    const items = await api.remoteListContainer(opened.token);
    state.container = {
      entryId: f.id,
      name: f.real_name || f.name,
      items,
      cwd: '',
      // 记下来源：容器内条目的预览要走 /pcitem/ 而不是 /citem/，
      // 少了这一位前端会拼出本地那条 URL，表现是点开里面的文件一片空白
      remote: true,
      fileToken: opened.token,
    };
    state.selected = [];
    return true;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.te('container_failed'));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 在容器内进入一个子目录。 */
export function enterContainerDir(path) {
  if (!state.container) return;
  state.container.cwd = path;
  state.selected = [];
}

/** 离开容器，回到它所在的磁盘目录。 */
export function leaveContainer() {
  state.container = null;
  state.selected = [];
}

/** 把容器内条目转成主列表认识的形状。
 *
 * # 为什么要适配而不是让 EntryCard 认识第二种结构
 *
 * 卡片和列表行只需要「名字、大小、是不是目录、图标线索」这几件事。
 * 与其在模板里到处加 `v-if="isContainerItem"`，不如在这里补齐字段——
 * 那种分支写法正是「改了一个分支忘了另一个」的温床。
 *
 * `path` 用容器内的相对路径：主列表用它做 `:key` 和选中标识，只要在
 * 当前视图里唯一就够，不需要是磁盘路径。
 */
function adaptContainerItem(it) {
  return {
    path: it.path,
    name: it.name,
    is_dir: it.is_dir,
    size: it.size,
    // 容器内的条目已经在解密后的视野里了，不是「一个加密文件」——
    // 标成 is_encrypted 会让卡片显示锁图案并要求再输一次密码
    is_encrypted: false,
    unlocked: true,
    real_name: null,
    entry_id: null,
    ext: it.name.includes('.') ? it.name.split('.').pop().toLowerCase() : null,
    // token 走 /citem/ 而不是 /plain/，两者不能混：前者要密钥，
    // 后者是磁盘明文。`in_container` 就是给预览层区分用的
    token: it.token,
    preview: it.kind,
    mime: it.mime,
    is_container: false,
    in_container: true,
  };
}

/** 容器内当前层级的直接子项。
 *
 * 容器索引是扁平的全路径列表，这里按 `cwd` 过滤出直接子项——
 * 否则进根目录会把所有层级的文件一股脑铺平列出来。
 */
const containerEntries = computed(() => {
  const c = state.container;
  if (!c) return [];
  const prefix = c.cwd ? `${c.cwd}/` : '';
  const out = [];
  for (const it of c.items) {
    if (!it.path.startsWith(prefix)) continue;
    const rest = it.path.slice(prefix.length);
    if (!rest || rest.includes('/')) continue; // 只要直接子项
    out.push(adaptContainerItem(it));
  }
  // 目录在前、各自自然序——与后端 `list_dir` 对磁盘目录的约定一致。
  // 两处都要排是因为容器索引的顺序是打包时的字节序，不是显示序
  return out.sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true });
  });
});

/* ---------------- 导航 ---------------- */

/** 打开一个目录。 */
/** 切换网格/列表视图，并把选择持久化为「默认视图」。
 *
 * 界面上的切换若只改内存，重启就回到网格——设置页那个「默认视图」选项
 * 也就形同虚设。这里在切换后读改写一次配置；保存失败不阻断本次切换。
 */
export async function setView(v) {
  if (v !== 'grid' && v !== 'list') return;
  state.view = v;
  try {
    const c = await api.configGet();
    if (c.ui) {
      c.ui.view = v;
      await api.configSet(c);
    }
  } catch {
    // 偏好存不下时本次切换仍生效，只是不跨重启
  }
}

/** 记住最后浏览的本地目录，去抖落盘。
 *
 * 连续进入多级目录（双击进入、上级、面包屑）会在短时间内触发多次 navigate，
 * 每次都读改写配置既浪费也可能乱序，去抖后只记最后停留的目录。
 */
let lastDirTimer = null;
function schedulePersistLastDir(dir) {
  if (lastDirTimer) clearTimeout(lastDirTimer);
  lastDirTimer = setTimeout(() => {
    lastDirTimer = null;
    api.configGet().then((c) => {
      if (c.ui && c.ui.last_dir !== dir) {
        c.ui.last_dir = dir;
        return api.configSet(c);
      }
      return undefined;
    }).catch(() => { /* 记不住不影响本次浏览 */ });
  }, 800);
}

/** 启动时按「启动时打开」配置恢复起始目录。
 *
 * last：回到上次最后浏览的目录；home：进主目录；其余（询问/指定）不导航。
 * 目录已失效或未授权时 navigate 自身会给出可关闭的错误条，不阻断启动。
 */
export async function restoreStartupDir() {
  try {
    const c = await api.configGet();
    const mode = c.ui?.startup || 'last';
    if (mode === 'last' && c.ui?.last_dir) {
      await navigate(c.ui.last_dir);
    } else if (mode === 'home') {
      const home = state.places.find((p) => p && p.name === 'home');
      if (home && home.path) await navigate(home.path);
    }
  } catch {
    // 配置读不出就停在空白起始页，不影响启动
  }
}

export async function navigate(dir) {
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.error = '';
  try {
    state.entries = await api.browseDirectory(dir);
    state.cwd = dir;
    state.selected = [];
    // 进磁盘目录就意味着不在容器里了。不清的话面包屑会同时显示
    // 磁盘路径和容器层级，点哪个都对不上
    state.container = null;
    await refreshKnown();
    schedulePersistLastDir(dir);
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.busy = false;
  }
}

/** 回到上一级。
 *
 * 在容器里时先在容器内部往上走，走到容器根再退出容器——
 * 与普通文件夹的层级感受一致。
 */
export async function goUp() {
  if (state.container) {
    const cwd = state.container.cwd;
    if (!cwd) {
      leaveContainer();
      return;
    }
    const i = cwd.lastIndexOf('/');
    enterContainerDir(i < 0 ? '' : cwd.slice(0, i));
    return;
  }
  if (!state.cwd) return;
  const parent = await api.parentOf(state.cwd).catch(() => null);
  if (parent) await navigate(parent);
}

/** 刷新当前位置。
 *
 * 在容器里就重新取一次容器索引（同时刷新 token），而不是退回磁盘目录。
 * 刷新不该改变用户所在的位置。
 */
export async function reload() {
  if (state.container) {
    const { entryId, name, cwd } = state.container;
    const items = await api.listContainer(entryId).catch(() => null);
    if (items) {
      state.container = { entryId, name, items, cwd };
    } else {
      // 取不到通常意味着已经锁定或文件不在了，退回磁盘目录比
      // 停在一个再也刷不出内容的位置要好
      leaveContainer();
      if (state.cwd) await navigate(state.cwd);
    }
    return;
  }
  if (state.cwd) await navigate(state.cwd);
}

/** 载入侧栏起点。 */
export async function loadPlaces() {
  state.places = await api.listPlaces().catch(() => []);
}

/** 查询存储访问权限状态。
 *
 * 查询失败时保持原值不动：把它当成「没权限」会让桌面端也弹出授权提示，
 * 而那里根本没有可申请的东西。
 */
export async function loadStorageAccess() {
  const s = await api.storageAccess().catch(() => null);
  if (s) state.storage = s;
}

/** 申请存储访问权限，成功后重新载入侧栏。
 *
 * 安卓上这会跳到系统设置页，用户回来后 Promise 才 resolve。
 * 必须重新 loadPlaces：拿到权限后可列的位置完全不同，不刷新的话
 * 侧栏还是那两个沙箱目录，用户以为授权没生效。
 */
export async function grantStorageAccess() {
  const s = await api.requestStorageAccess().catch((e) => {
    state.error = i18n.te(e?.code, e?.message);
    return null;
  });
  if (!s) return false;
  state.storage = s;
  if (s.granted) await loadPlaces();
  return s.granted;
}

/** 面包屑的各段。
 *
 * 段的形状是 `{ name, path, kind }`：`kind` 为 `dir` 表示磁盘目录，
 * `container` 表示容器本身，`inner` 表示容器内的子目录。点击时按 kind
 * 分派——三者的「跳转」是三件不同的事，靠路径字符串猜会出错
 * （容器内路径和相对磁盘路径长得一样）。
 *
 * 在容器里时，磁盘路径那几段仍然保留在前面：用户需要知道这个加密
 * 文件夹是从哪儿打开的，而且点它能回去。
 */
export const crumbs = computed(() => {
  const out = [];
  if (state.cwd) {
    // Windows 用反斜杠，Unix 用正斜杠。分割后重新拼接成可点击的路径
    const sep = state.cwd.includes('\\') ? '\\' : '/';
    const parts = state.cwd.split(/[\\/]/).filter(Boolean);
    let acc = '';
    for (const [i, p] of parts.entries()) {
      if (i === 0) {
        // Windows 的 "C:" 要补上分隔符才是合法路径；
        // Unix 的第一段前面要加根斜杠
        acc = sep === '\\' ? `${p}${sep}` : `${sep}${p}`;
      } else {
        acc = `${acc}${acc.endsWith(sep) ? '' : sep}${p}`;
      }
      out.push({ name: p, path: acc, kind: 'dir' });
    }
  }
  const c = state.container;
  if (c) {
    out.push({ name: c.name, path: '', kind: 'container' });
    if (c.cwd) {
      const parts = c.cwd.split('/').filter(Boolean);
      for (const [i, p] of parts.entries()) {
        out.push({
          name: p,
          path: parts.slice(0, i + 1).join('/'),
          kind: 'inner',
        });
      }
    }
  }
  return out;
});

/** 点击面包屑的某一段。 */
export async function gotoCrumb(c) {
  if (c.kind === 'dir') {
    await navigate(c.path);
    return;
  }
  if (c.kind === 'container') {
    enterContainerDir('');
    return;
  }
  enterContainerDir(c.path);
}

/* ---------------- 选择 ---------------- */

/** 切换单项选中。 */
export function toggleSelect(path, additive) {
  if (!additive) {
    state.selected = state.selected.includes(path) && state.selected.length === 1 ? [] : [path];
    return;
  }
  const i = state.selected.indexOf(path);
  if (i >= 0) state.selected.splice(i, 1);
  else state.selected.push(path);
}

/** 全选当前可见项。 */
export function selectAll() {
  state.selected = visibleEntries.value.map((e) => e.path);
}

/** 清空选择。 */
export function clearSelection() {
  state.selected = [];
}

/* ---------------- 加密与解锁 ---------------- */

/** 加密选中的路径。 */
export async function encryptSelected(opts) {
  const paths = encryptable.value.map((e) => e.path);
  if (!paths.length) {
    state.error = i18n.te('empty_selection');
    return null;
  }

  state.busy = true;
  state.busyKey = 'busy.encrypting';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅要在发起加密**之前**建立：小文件可能在 await 返回前就发完事件，
  // 晚一步订阅就会一个都收不到。
  let unlisten = null;
  try {
    unlisten = await api.onEncryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    // 订阅失败只是没有进度条，不该让加密本身失败
    unlisten = null;
  }

  try {
    const summary = await api.encryptPaths({ paths, ...opts });
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await reload();

    if (summary.failed.length) {
      // 部分失败要说清楚哪些失败了，不能只报个数字。
      // 用户需要知道是哪个文件出了问题才能处理
      const first = summary.failed[0];
      state.error = i18n.t('notice.encrypt_partial', {
        ok: summary.items.length,
        failed: summary.failed.length,
        reason: i18n.te(first[1], first[1]),
      });
    } else {
      setNotice(i18n.tn('notice.encrypted', summary.items.length));
    }
    return summary;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.encrypt_failed'));
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    // 必须取消订阅：每次加密都新建一个监听器，不取消的话
    // 加密 N 次之后同一个事件会被处理 N 遍
    if (unlisten) unlisten();
  }
}

/** 把选中的加密文件还原到磁盘。
 *
 * `opts` 形如 `{ target_dir, overwrite }`，来自还原对话框。
 *
 * 成功后**不** reload：还原不改变加密文件本身，列表内容没有变化。
 * 白刷一次列表会让大目录闪一下，看起来像出了什么事。
 */
export async function restoreSelected(opts) {
  // 字段是 entry_id 不是 id：DirEntry 里的 id 专指「已登记的 FileEntry
  // 的 id」，与磁盘条目本身区分开
  const ids = restorable.value.map((e) => e.entry_id).filter(Boolean);
  if (!ids.length) {
    state.error = i18n.te('empty_selection');
    return null;
  }

  state.busy = true;
  state.busyKey = 'busy.restoring';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅必须在发起之前建立，理由同加密：小文件可能在 await 返回前
  // 就把事件发完了，晚一步订阅一个都收不到
  let unlisten = null;
  try {
    unlisten = await api.onDecryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    unlisten = null;
  }

  try {
    const summary = await api.decryptPaths({ ids, ...opts });

    if (summary.failed.length) {
      // 部分失败要说清是哪个、为什么。只报数字的话用户无从下手，
      // 尤其 target_exists 是他自己能解决的（勾覆盖或换目录）
      const first = summary.failed[0];
      state.error = i18n.t('notice.restore_partial', {
        ok: summary.items.length,
        failed: summary.failed.length,
        reason: i18n.te(first[1], first[1]),
      });
    } else {
      setNotice(restoreNotice(summary.items));
    }
    return summary;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.restore_failed'));
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    if (unlisten) unlisten();
  }
}

/** 还原成功后的提示文案。
 *
 * 单独提出来是因为它要合并三种信息：还原了什么、放在哪、元数据有没有
 * 缺项。挤在一行里会很长，所以元数据只在**确实有缺项**时才提。
 *
 * 元数据那句不能省：解出来的文件时间戳不对是用户会注意到的事，
 * 事先说明「哪些项没还原、为什么」比让他事后怀疑文件坏了要好。
 */
function restoreNotice(items) {
  const where = items[0]?.output || '';
  const base = i18n.tn('notice.restored', items.length, { path: where });

  // 汇总所有条目的不支持项。同一种原因在多个文件上出现算一次——
  // 用户要知道的是「哪类元数据没还原」，不是每个文件重复一遍
  const kinds = new Set();
  for (const it of items) {
    for (const [code] of it.metadata?.unsupported || []) kinds.add(code);
  }
  if (!kinds.size) return base;

  const names = [...kinds].map((c) => i18n.t(`restore.meta_${c}`)).join('、');
  return `${base}（${i18n.t('restore.meta_partial', { items: names })}）`;
}

/** 给一个已加密文件增删改密码，或重新加密。
 *
 * add / change / remove 只改文件头，再大的文件也是毫秒级，所以不订阅
 * 进度——显示一个瞬间闪过的进度条只会让人以为出了什么事。
 *
 * reencrypt 相反：它要把载荷读一遍再写一遍，耗时与文件大小成正比，而
 * 对话框里还写着「期间请不要关闭程序」。不给进度条的话，用户面对一个
 * 静止的界面又被告知不能关，无从判断是在跑还是已经卡死。
 *
 * 成功后必须 reload：改完密码，会话里装的凭据变了，列表里这个文件
 * 是解锁还是锁定要重新算。不刷新的话界面还显示旧状态，用户点开会
 * 发现和刚才的操作对不上。
 */
/** 放弃重试并擦掉留在内存里的密码。对话框关闭时调用。 */
export function clearRetry() {
  state.retry = null;
  state.errorDetails = [];
}

/**
 * 只重试上次失败的那些文件。
 *
 * 与重跑整个操作的区别：重跑会拿旧密码去开已经改成新密码的文件，产生一堆
 * 假失败，用户分不清哪些是真问题；轮换时还要把已处理的文件再读写一遍。
 */
export async function retryKeyFiles() {
  const ctx = state.retry;
  if (!ctx || ctx.paths.length === 0) return null;

  state.busy = true;
  state.busyKey = ctx.rotate ? 'busy.reencrypting' : 'busy.deriving';
  state.error = '';
  state.notice = '';
  state.progress = null;

  let unlisten = null;
  if (ctx.rotate) {
    try {
      unlisten = await api.onEncryptProgress((p) => {
        state.progress = p;
      });
    } catch {
      unlisten = null;
    }
  }

  try {
    state.errorDetails = [];
    const r = await api.retryKeyFiles(ctx);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    // 全都补上了，重试上下文就该丢掉——留着会让"重试"按钮继续显示，
    // 用户再点一次会拿旧密码去开已经改好的文件，得到 wrong_password
    state.retry = null;
    await reload();
    setNotice(i18n.t('keymgmt.done_retry', { n: r.files_changed }));
    return r;
  } catch (e) {
    const p = e && typeof e === 'object' ? e.params : null;
    const files = p && Array.isArray(p.files) ? p.files : [];
    const paths = p && Array.isArray(p.paths) ? p.paths : [];
    if (files.length > 0) await reload();
    state.error = i18n.te(api.errCode(e));
    state.errorDetails = files;
    // 仍有失败就缩小清单：只留这次还没成的，别让用户重复处理已经好了的
    if (paths.length > 0) state.retry = { ...ctx, paths };
    return null;
  } finally {
    if (unlisten) unlisten();
    state.busy = false;
    state.busyKey = '';
    state.progress = null;
  }
}

/** 把后端错误翻成给人看的话。
 *
 * 单独抽出来是因为恢复码有一条特殊的错误：bad_recovery_code 带着
 * core 给的定位信息（「第 7 个词『acadmic』不在词表中，是不是
 * 『academic』？」）。i18n.te 只按码取文案、不做插值，直接用会让
 * 界面上出现一个原样的「{{detail}}」，而这恰恰是最该说清的一条。
 */
function recoveryError(e) {
  const code = api.errCode(e);
  const params = e && typeof e === 'object' ? e.params : null;
  const msg = i18n.te(code);
  const detail = params && typeof params.detail === 'string' ? params.detail : '';
  return detail ? msg.replace('{{detail}}', detail) : msg;
}

/** 生成恢复码。
 *
 * 成功时**不** reload：文件内容没变，只是多了一个槽位，列表上看不出
 * 任何差别。刷一次只会让界面闪一下，还会清掉用户的选中项。
 *
 * 也不在这里发提示条——词还要留在对话框里给用户抄，弹一句「已生成」
 * 会让人以为事情已经办完。提示留到关闭对话框时再发。
 */
export async function generateRecovery(req) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.generateRecovery(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    return r;
  } catch (e) {
    state.error = recoveryError(e);
    return null;
  } finally {
    state.busy = false;
  }
}

/** 用恢复码重设密码。
 *
 * 这个要 reload：新密码装进了会话，原本锁着的文件可能因此显形。
 */
export async function restoreWithRecovery(req) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.restoreWithRecovery(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await reload();
    return r;
  } catch (e) {
    state.error = recoveryError(e);
    return null;
  } finally {
    state.busy = false;
  }
}

export async function manageKey(req) {
  const rewrites = req.action === 'reencrypt';
  state.busy = true;
  // 阶段不同，说法也要不同：轮换时说"正在派生密钥"，用户会以为卡在
  // 一个本该几百毫秒的步骤上
  state.busyKey = rewrites ? 'busy.reencrypting' : 'busy.deriving';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅要在发起之前建立：小文件可能在 await 返回前就发完事件，
  // 晚一步订阅就会一个都收不到
  let unlisten = null;
  if (rewrites) {
    try {
      unlisten = await api.onEncryptProgress((p) => {
        state.progress = p;
      });
    } catch {
      // 订阅失败只是没有进度条，不该让操作本身失败
      unlisten = null;
    }
  }

  try {
    // 上一次的失败清单必须清掉，否则这次成功了，界面上还挂着上次的红字
    state.errorDetails = [];
    const r = await api.manageKey(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    // 树形改密码后目录名会变（它由密码派生）。正在这棵树里面浏览时，
    // 当前目录本身也失效了，要在刷新前换过去，否则会去列一个不存在的路径
    const moved = r.is_tree && r.new_path && r.new_path !== req.path;
    if (moved && state.cwd === req.path) state.cwd = r.new_path;
    await reload();
    // 选中项必须在 reload **之后**设置：reload -> navigate 里有
    // `state.selected = []`，放在前面会被清掉。表现是改完密码一项都没选中，
    // 而列表里的名字又变了，用户会以为操作对象弄错了
    if (moved) state.selected = [r.new_path];
    setNotice(
      r.is_tree
        ? i18n.t('keymgmt.done_tree', { n: r.files_changed })
        : i18n.t(`keymgmt.done_${r.action}`),
    );
    return r;
  } catch (e) {
    // 部分失败要说清是哪些文件。i18n.te() 只按码取文案、不做插值，
    // 后端带过来的清单会被丢掉——只剩一句「部分文件改写失败」，用户既
    // 不知道该处理什么，也无法判断损失多大
    const p = e && typeof e === 'object' ? e.params : null;
    const files = p && Array.isArray(p.files) ? p.files : [];
    const paths = p && Array.isArray(p.paths) ? p.paths : [];
    // 攒好重试要用的东西。密码用 req.current 而不是 req.next：失败的文件
    // 没被改写，还是旧密码
    state.retry =
      paths.length > 0
        ? { paths, current: req.current, next: req.next || '', rotate: rewrites, root: state.cwd }
        : null;
    // 刷新必须在设置错误**之前**：reload -> navigate 里有
    // `state.error = ''`，反过来会把刚设好的错误和清单一起冲掉，界面上
    // 对话框开着却什么都不显示，看起来像「什么都没发生」。
    // 与「选中项被 reload 清掉」是同一类顺序缺陷
    if (files.length > 0) await reload();
    state.error = i18n.te(api.errCode(e));
    state.errorDetails = files;
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    // 必须取消订阅：每次操作都新建一个监听器，不取消的话
    // 操作 N 次之后同一个事件会被处理 N 遍
    if (unlisten) unlisten();
  }
}

/** 用一个密码试解锁当前目录。 */
/**
 * 用设备密钥解锁当前目录。
 *
 * 与 tryUnlock 并列而不是替代：会话里的凭据是累加的，按指纹不会挤掉
 * 之前手动输入的密码。
 */
export async function tryDeviceUnlock() {
  if (!state.cwd) return false;
  state.busy = true;
  // 文案是「正在等待确认」而不是「正在派生」：这一步在等用户按指纹，
  // 说「派生中」会让人以为该干等
  state.busyKey = 'busy.waiting_hello';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.deviceKeyUnlock(state.cwd);
    state.credentials = r.credentials;
    await reload();
    // 与 tryUnlock 同一个判据：加密目录的 is_encrypted 故意是 false
    // （那个字段指「内容是密文」，而目录只有名字是密文），只按它过滤
    // 会把「只含树形加密目录」的文件夹误判成解锁失败
    const opened = state.entries.filter(
      (e) => (e.is_encrypted || e.is_encrypted_dir) && e.unlocked,
    ).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      // 设备密钥能解封说明硬件那边没问题，打不开文件就是另一回事了：
      // 这把钥匙不属于这个位置的文件
      state.error = i18n.te('device_key_not_enrolled');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(e);
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 在**远程位置**里试密码。
 *
 * 与局域网那条 `unlockRemote` 同构：远端没有「目录」可探测 vault，
 * 所以先从当前远程目录的文件头里收集 vault 参数，再派生。
 *
 * 成功后要重新识别当前目录——解锁改变的是**识别结果**（真实文件名、
 * 缩略图、可否播放），不重新识别的话界面上一切照旧，
 * 用户会以为密码没生效。
 */
async function tryUnlockRemotePlace(password) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const vaults = await api.remotePlaceVaults(state.remotePlace, state.remoteDir || '');
    if (!vaults.length) {
      // 这个目录里没有加密文件，谈不上解锁。与「密码不对」分开说：
      // 前者是「这儿没东西要解」，后者是「你输错了」
      state.error = i18n.te('no_vault_found');
      return false;
    }
    const r = await api.unlock('main', password, vaults);
    state.credentials = r.credentials;
    await reloadRemoteDir();
    const opened = state.remoteItems.filter((f) => f.is_encrypted && f.unlocked).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      // 派生几乎总会「成功」，真正的判据是有没有东西被解开
      state.error = i18n.te('wrong_password');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

export async function tryUnlock(password) {
  // 在远程位置里走远程那条：它没有本地目录，unlockDirectory 用不了。
  //
  // 不分流会怎样：下面那句 `if (!state.cwd)` 直接 return false，
  // 于是用户在远程位置里输密码**什么都不会发生，连错误都没有**，
  // 看到的是「密码输了没反应，文件一直锁着」。
  if (state.remotePlace) return tryUnlockRemotePlace(password);
  if (!state.cwd) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.unlockDirectory(state.cwd, password);
    state.credentials = r.credentials;
    await reload();
    // 判据要同时算上加密文件与加密目录。
    //
    // 加密目录的 is_encrypted 故意是 false——那个字段的含义是「内容是
    // 密文」，而目录没有内容，只有名字是密文。所以只按 is_encrypted 过滤
    // 会漏掉它们：一个只含树形加密目录的文件夹，解锁成功后 opened 仍是 0，
    // 被判成「密码不对」，用户输了正确密码却看到红字报错，而目录名其实
    // 已经解出来了。
    const opened = state.entries.filter(
      (e) => (e.is_encrypted || e.is_encrypted_dir) && e.unlocked,
    ).length;
    if (opened > 0) {
      // 重复输入同一个密码要与「真的加了一个」区分开。
      //
      // 不区分会怎样：用户忘了这个密码已经装过，再输一次看到「解锁了 N 个
      // 文件」，但状态栏的密码数纹丝不动——看起来像计数坏了。如实说
      // 「已经在用了」才对得上他看到的现象
      if (r.added === false) {
        setNotice(i18n.t('unlock.already_loaded'));
      } else {
        setNotice(i18n.tn('notice.unlocked', opened));
      }
    } else {
      // 派生成功但一个都没解开 = 密码不对。
      // 这个区分很重要：KEK 派生几乎总是"成功"的，
      // 真正的判据是有没有东西被解开
      state.error = i18n.te('wrong_password');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 刷新已识别加密文件的索引。 */
async function refreshKnown() {
  if (state.credentials === 0) {
    state.known = {};
    return;
  }
  try {
    // scan_directory 会把结果存进后端状态，预览时按 id 取
    const files = await api.scanDirectory(state.cwd, false);
    const map = {};
    for (const f of files) map[f.path] = f;
    state.known = map;

    // 把解锁信息补回列表，让预览能找到对应条目。
    //
    // 后端 `annotate_unlocked` 做的是同一件事。两处都要改是重复，
    // 但这条路径（扫描后回填）与那条（列目录时标注）触发时机不同，
    // 目前无法合并——**新增字段时两边都得加**，漏了就会出现
    // 「后端算对了、界面上却没生效」。
    for (const e of state.entries) {
      const f = map[e.path];
      if (f && f.unlocked) {
        e.unlocked = true;
        e.real_name = f.name;
        e.entry_id = f.id;
        e.is_container = f.is_container;
      }
    }
  } catch {
    // 扫描失败不影响浏览——列表还在，只是加密文件显示成锁定
  }
}

/** 锁定：抹掉所有凭据。
 *
 * 后端的 `lock` 会**同时关闭设备库**，所以这里也要把设备概览清掉——
 * 否则侧栏还挂着「已配对 3 台」的角标，而设备库其实已经锁上了。
 */
export async function lock() {
  await api.lock();
  state.credentials = 0;
  state.known = {};
  state.pairedCount = 0;
  // 容器视图里列的是解密出来的文件名，锁定后不该继续可见。
  // 下面还有一道 watch 兜着，两道都要：这一行是主路径，
  // watch 保证「即使某条路径忘了清，界面也不会留着内容」
  state.container = null;
  // 后端的 lock 已经断开了远端连接，这里同步界面状态。
  // 顺序不能反：先清状态再调后端的话，中间那一刻界面显示的是
  // 「已断开」而连接其实还在
  state.peer = null;
  state.remoteEntries = [];
  state.remoteMode = false;
  // 重新载入让加密文件回到锁定显示。
  // 不能只改本地字段——那样万一漏改一处就是信息泄露
  await reload();
  await refreshDeviceOverview();
}

/** 切换语言。 */
export async function switchLanguage() {
  const next = i18n.nextLang();
  await i18n.load(next);
  await api.setLanguage(next);
  localStorage.setItem('omy.lang', next);
}

/** 切到指定语言。设置页用这个，顶栏的循环切换用 switchLanguage。
 *
 * `auto` 表示跟随系统：此处解析成具体语言再加载，因为 i18n.load 要一个
 * 确定的语言包。配置文件里仍然存 'auto'——存解析结果的话，用户换了系统
 * 语言之后应用不会跟着变，而他明明选的是「跟随系统」。
 */
export async function applyLanguage(pref) {
  const target = pref === 'auto' ? detectSystemLang() : pref;
  if (!i18n.LANGS.includes(target)) return;
  await i18n.load(target);
  await api.setLanguage(target);
  localStorage.setItem('omy.lang', pref);
}

/** 从浏览器环境猜系统语言，猜不出时回落到英文。
 *
 * 与后端 `state::detect_language` 是同一套意图——**改一处要想到另一处**。
 * 两边不一致的表现是「启动瞬间是一种语言，加载完配置后跳成另一种」。
 */
function detectSystemLang() {
  const raw = (navigator.language || 'en').toLowerCase();
  return raw.startsWith('zh') ? 'zh-CN' : 'en';
}

/* ---------------- 远程位置 ---------------- */

/** 本地位置的全能力。
 *
 * 这几份常量的字段必须与后端 `omy_remote::Capabilities` 逐一对上（含
 * `search`）：少写一个字段，读到的就是 `undefined`，而 `!caps.write` 为真会让
 * 只读位置反而全部可写。后端 `caps.rs` 的 `field_names_are_stable` 就是为拦这
 * 件事存在的——**后端加字段时这里三处都要跟着加**。
 */
const LOCAL_CAPS = {
  read: true,
  write: true,
  delete: true,
  rename: true,
  create_dir: true,
  random_write: true,
  range_read: true,
  // 本地搜索是对已列出条目的前端过滤，没有「下推到服务端」这回事
  search: false,
};

/** 状态不明时用的保守能力：能读，什么都不能写。 */
const SAFE_READONLY_CAPS = {
  read: true,
  write: false,
  delete: false,
  rename: false,
  create_dir: false,
  random_write: false,
  range_read: true,
  search: false,
};

/** 当前所在位置 + 目录的**有效**能力。界面一律读这一个。
 *
 * 不在远程位置时返回本地全能力——这样上层判断可以统一写 `caps.write`，
 * 不必到处分叉「是不是远程」。
 *
 * 在远程位置但还没查到目录能力时返回**保守只读**，而不是回落到位置级上界：
 * 回落等于把「最多能做什么」当成「现在能做什么」用，那正是这层收窄要消除的
 * 缺陷——用户会在一个只读目录里看到亮着的删除按钮。少几个按钮只是等一下就
 * 补上，点了报错却已经发出去了。
 */
export const currentCaps = computed(() => {
  if (!state.remotePlace) return LOCAL_CAPS;
  return state.remoteDirCaps || SAFE_READONLY_CAPS;
});

/** 切到消息视图并加载。
 *
 * 只在对话内可用：位置列表那一层没有「消息」这个概念。
 */
/** 消息视图缓存：`位置\x00目录` -> 消息行数组。
 *  二次进入某对话的消息栏时先出缓存、再后台刷新，别每次全量重拉。 */
const remoteMsgCache = new Map();

/** 加载消息栏首屏（带缓存优先）。
 *
 * 先出缓存（若有），再拉服务端刷新。缓存/刷新都写回 remoteMsgCache。
 * 与文件栏的 reloadRemoteDir 同一种「缓存先出 + 后台刷新」节奏。 */
export async function loadMessagesFirstPage() {
  if (!state.remotePlace || !state.remoteDir) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  const key = remoteDirKey(place, dir);
  state.placeError = '';
  // 缓存优先：有上次的就先摆出来，避免空屏
  const cached = remoteMsgCache.get(key);
  if (cached && cached.length) {
    state.remoteMessages = cached;
    state.loadingMessages = false;
  } else {
    state.remoteMessages = [];
    state.loadingMessages = true;
  }
  try {
    const rows = await api.remoteMessages(place, dir);
    // 回写前校验没切走对话/栏
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    if (state.remoteTab !== 'messages') return;
    state.remoteMessages = rows;
    remoteMsgCache.set(key, rows);
    // 取满一页就假定还有更早的。少于一页说明到头了——不靠「下一次返回空」
    state.hasMoreMessages = rows.length >= 25;
  } catch (e) {
    // 有缓存就保留，别让一次网络抖动把已看到的消息抹掉
    if (!cached || !cached.length) state.remoteMessages = [];
    state.placeError = i18n.te(api.errCode(e), i18n.te('remote_failed'));
  } finally {
    state.loadingMessages = false;
  }
}

/** 兼容保留：旧调用点（若有）切文件/消息视图。现在消息是第六个分栏，
 *  统一走 setRemoteTab；这里仅转发，避免遗漏的调用点报错。 */
export async function setRemoteViewMode(mode) {
  if (mode === 'messages') {
    await setRemoteTab('messages');
  } else {
    state.remoteViewMode = 'files';
    state.placeError = '';
    if (state.remoteTab === 'messages') await setRemoteTab('media');
  }
}

/** 切换对话内的分栏：媒体/文件/链接/音频/GIF/消息。
 *
 * 消息栏走 getHistory 时间线（loadMessagesFirstPage）；其余五栏走服务端
 * 类型 filter（reloadRemoteDir）。两条加载路径不同，切栏时正确分发。 */
export async function setRemoteTab(tab) {
  if (state.remoteTab === tab) return;
  state.remoteTab = tab;
  api.uiLog('switch-tab', tab);
  if (tab === 'messages') {
    // remoteViewMode 仍用 'messages' 驱动模板渲染消息时间线
    state.remoteViewMode = 'messages';
    await loadMessagesFirstPage();
  } else {
    state.remoteViewMode = 'files';
    await reloadRemoteDir();
  }
}

/** 对话内文件视图的媒体分栏定义。key 与后端 MediaTab::from_key 对应。 */
export const MEDIA_TABS = [
  { key: 'media', i18n: 'rplace.tab_media' },
  { key: 'file', i18n: 'rplace.tab_file' },
  { key: 'link', i18n: 'rplace.tab_link' },
  { key: 'audio', i18n: 'rplace.tab_audio' },
  { key: 'gif', i18n: 'rplace.tab_gif' },
  // 消息作为第六个分栏（末位）：原来是「文件/消息」两 tab 那层单独切换，
  // 合并进这一排后交互更一致。消息栏走 getHistory（时间线），其余五栏走
  // 服务端类型 filter。
  { key: 'messages', i18n: 'msgs.view_messages' },
];

/** 分栏 tab 该不该显示：Telegram 对话内、文件视图下才出现。 */
export const showMediaTabs = computed(
  () => !!state.remotePlace && !!state.remoteDir
    && (state.remotePlaces.find((p) => p.id === state.remotePlace)?.kind === 'telegram'),
);

/** 消息视图里能不能用。
 *
 * 只有 Telegram 有「消息」这个概念，而且要已经进到某个对话里。
 * WebDAV 上不显示这个切换——不是置灰，是整个不出现（与搜索那个分段控件同理）。
 */
export const canShowMessages = computed(
  () => !!state.remotePlace && !!state.remoteDir
    && (state.remotePlaces.find((p) => p.id === state.remotePlace)?.kind === 'telegram'),
);

/** 远程视图里该显示哪些条目。
 *
 * 远程网格以前直接渲染 `state.remoteItems`，于是顶栏那个搜索框**对远程位置
 * 完全无效**——输入什么都不会过滤。本地视图有 `visibleEntries` 做过滤，
 * 远程没有对应物，这个缺口从界面上看不出来（搜索框照常能输入）。
 *
 * 两种语义都落在这里：
 * - 本地过滤：在已加载的这一屏里按**解出来的真实文件名**匹配，搜索词不出本机；
 * - 服务端搜索：列服务端给的候选集，再用同一套本地规则精筛一遍。
 */
export const remoteVisible = computed(() => {
  const server = state.searchMode === 'server';
  const source = server ? state.searchResults : state.remoteItems;
  const q = state.query.trim().toLowerCase();
  if (!q) return source;
  return source.filter((e) => {
    // 锁定的加密文件没有可搜的名字：用磁盘上的密文名去匹配等于拿乱码当明文搜，
    // 既搜不到也会泄露信息。解锁对应密码后它会自动出现在结果里
    if (e.is_encrypted && !e.unlocked) return false;
    const name = e.real_name || e.name;
    return name.toLowerCase().includes(q);
  });
});

/** 被本地精筛挡掉的加密文件数量。
 *
 * 界面要如实说明「还有 N 个加密文件因为没解锁而没参与搜索」——不说的话
 * 用户会以为那些文件不存在，而其实只是差一个密码。
 */
export const searchLockedOut = computed(() => {
  const q = state.query.trim();
  if (!q) return 0;
  const source = state.searchMode === 'server' ? state.searchResults : state.remoteItems;
  return source.filter((e) => e.is_encrypted && !e.unlocked).length;
});

/** 当前位置支不支持服务端搜索。
 *
 * 分段控件据此决定**出不出现**（而不是置不置灰）：本地目录和网盘根本没有
 * 「搜索整个对话」这个概念，摆一个灰按钮只会让人以为是暂时不可用。
 */
export const canServerSearch = computed(
  () => !!state.remotePlace && !!currentCaps.value.search,
);

/** 发起一次服务端搜索。
 *
 * 只有用户显式切到「搜索整个对话」时才会走到这里——这是把搜索词发出去的
 * 唯一路径，保持它唯一才好确认「发出去」和「告诉用户发出去了」是同步的。
 */
/** 搜索请求的序号。
 *
 * 用于丢弃**已经作废的那一次**的结果。实测过一条竞态：切到「搜索整个
 * 对话」会自动发起一次搜索，用户在请求飞行途中清空了输入框，清空逻辑
 * 已经把提示条撤掉，可请求随后回来又把搜索词写了回去——警告条于是挂着
 * 一个用户已经删掉的词。
 *
 * 与 `refreshRemoteDirCaps` 回写前校验位置与目录是同一种做法。 */
let searchSeq = 0;

export async function runServerSearch() {
  const q = state.query.trim();
  searchSeq += 1;
  const seq = searchSeq;
  if (!state.remotePlace || !q) {
    state.searchResults = [];
    state.searchedQuery = '';
    return;
  }
  state.searching = true;
  state.placeError = '';
  try {
    const rows = await api.remoteSearch(state.remotePlace, state.remoteDir, q);
    // 结果回来时若已经不是最新那一次，整份丢弃：用户可能已经清空输入框
    // 或又搜了别的词，写回去就是把界面拉回一个他已经离开的状态
    if (seq !== searchSeq) return;
    state.searchResults = rows;
    // 记下真正搜出去的那个词，而不是读 state.query——用户可能在请求飞行途中
    // 又改了输入框，那样提示条会显示一个其实没发出去的词
    state.searchedQuery = q;
  } catch (e) {
    // 作废的请求失败了不该清当前状态，更不该弹一条属于旧请求的错误
    if (seq !== searchSeq) return;
    state.searchResults = [];
    state.searchedQuery = '';
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  } finally {
    if (seq === searchSeq) state.searching = false;
  }
}

/** 输入框清空时，把「已经搜出去的那个词」一并作废。
 *
 * 不这样会怎样：实测过——输入 omy、服务端搜一次、再清空输入框，
 * 那条「搜索词「omy」已发送到 Telegram 服务器」的警告仍挂着。
 * 这条提示的作用是告诉用户**哪个词离开了本机**，停在一个他已经删掉的
 * 词上，会让人以为清空动作又发了一次请求。隐私提示给错状态比不给更糟。
 *
 * 不在渲染条件里加 `state.query`：那样刚敲第一个字符警告就没了，
 * 而那一刻上一次搜索的结果还在屏幕上摆着。 */
export function onSearchQueryCleared(ev) {
  // 读事件里的 DOM 值，不读 state.query：同一个元素上 v-model 与 @input
  // 谁先跑不由我们决定，读响应式变量可能拿到上一轮的旧值。实测正是如此
  // ——直接清空不生效，先敲一个字符再清空却生效，差别只在这个时序上。
  // ev.target.value 在 input 触发时一定已是新值。
  const v = ev?.target?.value ?? state.query;
  if (String(v).trim()) return;
  // 让飞行中的那次搜索作废，否则它回来会把刚撤掉的提示条又挂回去
  searchSeq += 1;
  state.searchResults = [];
  state.searchedQuery = '';
}

/** 切换搜索模式。 */
export async function setSearchMode(mode) {
  state.searchMode = mode;
  if (mode === 'server') {
    // 只有已经有搜索词才真去服务端搜。空词时若无条件 runServerSearch，
    // 服务端返回空集，而 server 模式下列表 source 用的是 searchResults，
    // 于是「切一下模式」就把整屏内容清空了——用户切模式并不等于想搜空。
    // 空词时只切模式标记、保留当前列表，等用户真输入词再搜。
    if (state.query.trim()) {
      await runServerSearch();
    }
  } else {
    // 切回本地时清掉服务端结果：留着会让用户以为本地过滤也能搜到那些
    state.searchResults = [];
    state.searchedQuery = '';
  }
}

/** 刷新远程位置列表。 */
export async function reloadRemotePlaces() {
  state.remotePlaces = await api.remotePlaceList().catch(() => []);
}

/** 进入一个远程位置的根目录。 */
export async function openRemotePlace(id) {
  // 上报"切到某个远程位置/账号"（记 redact 后的短标识，不记位置名）
  api.uiLog('open-place', id ? `#${id}` : '');
  state.remotePlace = id;
  state.remoteDir = '';
  // 切账号先清掉上一个账号的列表：不清的话，切到一个**从没加载过**的账号时
  // 会先显示上一个账号的内容 + 顶部小刷新图标，而不是「占满区域的大加载圈」。
  // reloadRemoteDir 随后若命中缓存会立刻把内容摆回来，所以有缓存的账号仍是
  // 「先出缓存」，不受影响——三态就此区分开。
  state.remoteItems = [];
  await reloadRemoteDir();
}

/** 离开远程位置，回到本地浏览。 */
export function leaveRemotePlace() {
  // 暂存区跟着清：它只服务「当前这一屏」，留着既占内存，
  // 也可能在下次进同一目录时贴上已经过时的识别结果
  remoteEntryBuffer.clear();
  state.remotePlace = '';
  state.remoteDir = '';
  state.remoteItems = [];
  state.hasMoreFiles = false;
  state.remoteRetrying = [];
  // 能力必须跟着清：留着会让下次进另一个位置时先按上一个位置的能力渲染一帧
  state.remoteDirCaps = null;
  state.remoteProtected = false;
  // 搜索态也要清：回到本地后还留着「服务端搜索」模式的话，界面会显示一个
  // 本地位置根本不支持的模式，而那批结果也已经不属于眼前这个位置了
  state.searchMode = 'local';
  state.searchResults = [];
  state.searchedQuery = '';
  // 消息视图也要复位：留着会让下次进另一个对话时先显示上一个对话的消息
  state.remoteViewMode = 'files';
  state.remoteMessages = [];
  state.remoteDirName = '';
}

/** 列出当前远程目录。
 *
 * 失败时清空列表并报错，而不是留着上一个目录的内容——那会让用户
 * 以为自己进到了一个内容相同的目录。
 */
export async function reloadRemoteDir() {
  if (!state.remotePlace) return;
  // 捕获进入时的位置/目录：读磁盘 meta 是异步的，回来前用户可能切走，
  // 回写前要校验还在同一屏（与 loadMoreFiles 的飞行校验同理）。
  const placeAtStart = state.remotePlace;
  const dirAtStart = state.remoteDir;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  state.remoteRetrying = [];
  // 先作废上一个目录的能力再去查：留着旧值会让新目录短暂显示错误的能力，
  // 而用户完全可能在那一瞬间点下去
  state.remoteDirCaps = null;
  state.remoteProtected = false;
  // 换目录就清掉上一个对话的消息，否则切到消息视图会先闪一屏别的对话的内容
  state.remoteMessages = [];
  state.remoteViewMode = 'files';
  refreshRemoteDirCaps(state.remotePlace, state.remoteDir);
  // 本次重载的暂存区从空开始：留着上一次的会把已被服务端改动过的
  // 旧识别结果贴到新骨架上（比如文件被替换后 id 相同但内容已变）
  remoteEntryBuffer.delete(remoteDirKey(state.remotePlace, state.remoteDir));

  // 这个目录之前进过，就**先把上次的结果摆出来**再去刷新。
  //
  // 实测后端一次 browse 稳定约 0.4 秒（连接是复用的，那 0.4 秒是真的
  // 在拉消息）。不先摆出来的话，每次进对话都是 0.4 秒空白 + 骨架重画
  // ——用户说的「每次进来都需要重新加载」就是这个。
  //
  // 摆出来之后仍然会刷新（Telegram 里随时可能有新文件），只是用户不必
  // 盯着空屏等。
  // 是否在 Telegram 对话内：对话内按媒体分栏取（remote_browse_tab），
  // 根目录（列对话）与网盘走原来的 remote_browse。用 kind + remoteDir 判，
  // 不硬编码 id 形状。
  const inDialog = !!state.remoteDir && isTelegramDialog();
  const tab = inDialog ? state.remoteTab : '';
  // 缓存键带上分栏：同一对话的媒体栏和文件栏是两批内容，共用一个键会串。
  const ckey = remoteDirKey(state.remotePlace, state.remoteDir, tab);
  let cachedRows = remoteDirCache.get(ckey);
  if (cachedRows && cachedRows.length) {
    state.remoteItems = cachedRows;
    void refreshCacheStats(state.remotePlace, cachedRows);
  } else {
    // 内存未命中（多半是重启后首次）：尝试读磁盘 meta 缓存，先把上次这一屏
    // 摆出来，避免空屏等网络。磁盘里存的行不带 thumb_token（见落盘处的说明），
    // 缩略图/头像由后台事件重新补上。
    const diskRows = await loadDirSnapshotFromDisk(ckey);
    if (diskRows && diskRows.length
        && state.remotePlace === placeAtStart && state.remoteDir === dirAtStart) {
      cachedRows = diskRows;
      remoteDirCache.set(ckey, diskRows);
      state.remoteItems = diskRows;
      void refreshCacheStats(state.remotePlace, diskRows);
    }
  }

  try {
    const rows = inDialog
      // 首屏 limit 按视口估：填满可见网格 + 一屏缓冲，别拍一个固定 N
      ? await api.remoteBrowseTab(
          state.remotePlace, state.remoteDir, tab, 0, viewportFillCount(),
        )
      : await api.remoteBrowse(state.remotePlace, state.remoteDir);
    // 从缓存里把**已升级的清晰缩略图 token**带到刷新回来的新行上。
    // 新行带的是 stripped 占位 token（后端每次 browse 都先给占位、清晰图走
    // 后台事件）。若直接用新行覆盖，二次进对话就会先退回糊占位、再等后台
    // 重新拉一遍清晰图——用户报的「切出切入清晰图退回占位」。缓存里那份 token
    // 是上次后台升级后写透进来的（见 onRemoteEntry），沿用它即可直接出清晰图。
    if (cachedRows && cachedRows.length) {
      const prev = new Map(cachedRows.map((x) => [x.id, x.thumb_token]));
      for (const r of rows) {
        const pt = prev.get(r.id);
        // 缓存里那份是「上次已知的最新 token」——媒体栏首屏给的是 stripped 占位、
        // 后台升级事件再把它换成清晰图并写透进缓存，所以缓存里多半已是清晰图。
        // 新行带的又是 stripped 占位，二者都非空。**优先沿用缓存的**，避免退回
        // 糊占位；本次刷新的后台升级仍会照常跑一遍、确认或再更新它。
        if (pt) r.thumb_token = pt;
      }
    }
    state.remoteItems = rows;
    remoteDirCache.set(ckey, rows);
    // 写透磁盘 meta（重启后可先出）：去掉 thumb_token——它是会话内 PlaceThumbs
    // 的句柄，重启后失效，存了会让重启后短暂显示裂图。缩略图/头像自有各自的
    // 磁盘缓存(rthumbs)与后台回填，列表 meta 只负责「名字/大小/结构」这一层。
    void saveDirSnapshotToDisk(ckey, rows);
    void refreshCacheStats(state.remotePlace, rows);
    // 取满一页就假定还有更多。只对「对话内层」成立：根目录列的是对话
    // （不分页），WebDAV 的 PROPFIND 也是一次列全。对话内按本次请求的
    // limit 判（不是固定 pageSize），因为分栏首屏 limit 是按视口算的。
    state.hasMoreFiles = inDialog
      ? rows.length >= viewportFillCount()
      : (!!state.remoteDir && rows.length >= state.pageSize);
    // 骨架刚落地，把 await 期间早到的识别结果补贴上去。
    // 少了这一句，识别快于 browse 返回时整屏都会卡在「识别中」
    applyBufferedRemoteEntries();
  } catch (e) {
    // 有缓存就保留：刷新失败不该把用户已经看到的内容抹掉。
    // 清空的话一次网络抖动就让整屏变空，而那些文件其实都还在
    if (!cachedRows || !cachedRows.length) state.remoteItems = [];
    state.hasMoreFiles = false;
    // 用云盘视图自己的错误位，不弹底部全局条——那是给本地操作留的
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 加载更早的消息。
 *
 * 用当前最老那条的消息号作 offset。Telegram 的消息号在对话内单调递增，
 * 所以「更早」就是号更小的那些。
 *
 * # 两件必须做对的事
 *
 * 1. **全部追加，不能只追加带文件的。** §7.8 要求消息视图包含纯文本
 *    消息，界面上那行「N 条消息，其中 M 条带文件」正是用来暴露判据有没有
 *    退化成文件视图的；只追加带文件的话翻几页后两个数字就相等了。
 * 2. **按消息号去重。** offset_id 理论上是严格早于，但网络重试或消息被删
 *    都可能让边界错位。重复一条在界面上表现为同一个文件出现两次，
 *    用户会以为自己传了两遍。
 */

/** 批量拉这批条目的缓存状态，填进 `state.remoteCacheStat`。
 *
 * 用批量命令：逐个查的话一屏 N 个文件就 N 次 IPC 往返，而这些查询全在
 * 本地（读缓存索引、不碰网络），往返本身反而成了主要成本。 */
export async function refreshCacheStats(place, items) {
  const files = (items || []).filter((x) => !x.is_dir);
  if (!place || !files.length) return;
  try {
    const stats = await api.remoteCacheFileStats(
      files.map((f) => ({
        // size 必须兜底：缺了它后端 build_remote_source 可能拿不到正确的
        // 分块信息，返回一个 pinned=false 的空 stat（单文件路径就因为
        // 有 `|| 0` 而没踩到）
        place_id: place, path: f.id, size: f.size || 0, name: f.name,
      })),
    );
    if (!Array.isArray(stats)) return;
    // 写进**已有的** remoteCacheStat，不另起一份。
    //
    // 它原本只在右键菜单里被单个填充，而 pin / unpin / 移除缓存三处都
    // 已经就地更新它。另存一份的代价实测过：pin 之后菜单知道状态变了、
    // 卡片标识不知道，要退出再进才刷新——因为两者看的不是同一份数据。
    const next = { ...state.remoteCacheStat };
    files.forEach((f, i) => {
      if (stats[i]) next[remoteFileCacheKey(place, f.id)] = stats[i];
    });
    state.remoteCacheStat = next;
  } catch {
    // 查不到就不画标识。这是附加信息，失败不该影响浏览本身
  }
}

/** 取后端的分页大小。启动时问一次就够——它是编译期常量。 */
export async function loadPageSize() {
  try {
    const n = await api.remotePageSize();
    if (Number.isFinite(n) && n > 0) state.pageSize = n;
  } catch {
    // 取不到就用默认值；判据只是「取满一页就可能还有」，
    // 差一点不会导致漏文件，最多多问一次
  }
}

/** 加载更多文件。
 *
 * 与消息视图那条同源：用当前最后一条的消息号作 offset。
 *
 * # 为什么必须有
 *
 * 固定拉一页的话，活跃群里**第 N+1 个文件之后永远看不到，而且界面上
 * 没有任何迹象**——用户会以为那些文件不在这个群里。
 */
export async function loadMoreFiles() {
  if (state.loadingMoreFiles || !state.hasMoreFiles) return;
  if (!state.remotePlace || !state.remoteDir) return;
  const last = state.remoteItems.at(-1);
  if (!last) return;
  // Telegram 条目 id 形如 tg:<对话>:<消息号>，取最后一段。
  // 这里能按 ':' 切是因为**这条路径只对 Telegram 开放**（后端对其它
  // provider 直接回 remote_no_messages），不是在猜通用 id 的形状
  const n = Number.parseInt(String(last.id).split(':').pop(), 10);
  if (!Number.isFinite(n)) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  const inDialog = isTelegramDialog();
  const tab = inDialog ? state.remoteTab : '';
  const lim = viewportFillCount();
  state.loadingMoreFiles = true;
  try {
    // 对话内按当前分栏续拉（同一个类型 filter），否则走无过滤的 more
    const rows = inDialog
      ? await api.remoteBrowseTab(place, dir, tab, n, lim)
      : await api.remoteBrowseMore(place, dir, n);
    // 回写前校验还在同一个目录**且没换栏**：用户可能在请求飞行途中切走
    // 或切了栏，晚到的这批不能贴到别的栏上
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    if (inDialog && state.remoteTab !== tab) return;
    const seen = new Set(state.remoteItems.map((x) => x.id));
    const fresh = rows.filter((x) => !seen.has(x.id));
    state.remoteItems = [...state.remoteItems, ...fresh];
    // 回写内容缓存（带 tab 的键），下次进来先摆这批
    remoteDirCache.set(remoteDirKey(place, dir, tab), state.remoteItems);
    const need = inDialog ? lim : state.pageSize;
    state.hasMoreFiles = rows.length >= need && fresh.length > 0;
    void refreshCacheStats(place, fresh);
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.loadingMoreFiles = false;
  }
}

export async function loadMoreMessages() {
  if (state.loadingMore || !state.hasMoreMessages) return;
  if (!state.remotePlace || !state.remoteDir) return;
  const oldest = state.remoteMessages.at(-1);
  if (!oldest) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  state.loadingMore = true;
  try {
    const rows = await api.remoteMessages(place, dir, oldest.message);
    // 回写前校验还在同一个对话：用户可能在请求飞行途中切走了
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    const seen = new Set(state.remoteMessages.map((m) => m.message));
    const fresh = rows.filter((m) => !seen.has(m.message));
    state.remoteMessages = [...state.remoteMessages, ...fresh];
    state.hasMoreMessages = rows.length >= 25 && fresh.length > 0;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.loadingMore = false;
  }
}

/** 查「受保护内容」标记，查到后写回 `state.remoteProtected`。
 *
 * 与 `refreshRemoteDirCaps` 一样不 await、一样在回写前校验位置与目录
 * 仍是当前这一个——用户可能在请求飞行途中已经切走，那时把旧对话的结果
 * 写上去，界面显示的就是上一个对话的状态。
 *
 * 取不到时按「没开保护」处理：这只是一行提示，不该因为它报错或挡住目录。
 */
function refreshProtected(placeId, dir) {
  if (!placeId || !dir) {
    state.remoteProtected = false;
    return;
  }
  api
    .remoteDirProtected(placeId, dir)
    .then((v) => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteProtected = !!v;
    })
    .catch(() => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteProtected = false;
    });
}

/** 查当前远程目录的有效能力，查到后写回 `state.remoteDirCaps`。
 *
 * 不 await：能力查询不该拖慢列目录（对某些 provider 它是一次网络请求）。
 * 列表先出来，按钮随后补上——反过来会让整屏卡在一个只影响菜单的请求上。
 *
 * 回写前校验位置与目录仍是当前这一个：切目录后晚到的结果必须丢弃，
 * 否则会把上一个目录的能力贴到新目录上。
 */
function refreshRemoteDirCaps(placeId, dir) {
  // 「受保护内容」跟着对话走，与能力同一时机刷新。不在这里刷的话，
  // 从一个受保护的群退回对话列表时那行告知会留着，变成「在没开保护
  // 的地方显示着上一个对话的告知」
  refreshProtected(placeId, dir);
  api
    .remoteEffectiveCaps(placeId, dir)
    .then((caps) => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteDirCaps = caps;
    })
    .catch(() => {
      // 查不到就保持 null，`currentCaps` 会按保守只读处理。
      // 不在这里回落到位置级上界——那等于把上界当实际能力用
    });
}

/** 就地替换一个远程条目（单条目探测与边扫边出共用）。
 *  只更新当前列表里已存在的 id：切目录后晚到的结果找不到骨架，直接丢弃，
 *  绝不 push 进新目录，避免上一屏的条目串到下一屏。 */
export function patchRemoteItem(entry) {
  const idx = state.remoteItems.findIndex((x) => x.id === entry.id);
  if (idx >= 0) state.remoteItems[idx] = entry;
  state.remoteRetrying = state.remoteRetrying.filter((id) => id !== entry.id);
}

/** 单独重试一个「未能读取（网络）」条目，不重载整个目录。
 *  命令本身失败（如位置已失效）时恢复可再点的失败态，不吞错也不卡死。 */
export async function retryRemoteEntry(f) {
  if (!state.remotePlace || state.remoteRetrying.includes(f.id)) return;
  state.remoteRetrying = [...state.remoteRetrying, f.id];
  try {
    // 把已知的名字一并传过去：后端从 id 猜名字的那条回落路径写死了
    // WebDAV 的路径形状，对 Telegram 的 id 会猜成整个 id
    const entry = await api.remoteProbeEntry(
      state.remotePlace, f.id, f.size || 0, f.name);
    patchRemoteItem(entry);
  } catch (_) {
    state.remoteRetrying = state.remoteRetrying.filter((id) => id !== f.id);
  }
}

/** 识别结果暂存区：键是「位置 + 目录」，值是 id 到已识别条目的映射。
 *
 *  存在的理由是一个实测到的竞态，不是防御性编程：后台识别任务在
 *  `remote_browse` **内部**被 spawn，识别结果走事件、骨架走返回值，
 *  两条路径互相独立。头部改走密文块缓存之后，单文件识别只要 2~5ms，
 *  于是事件会**早于** `remote_browse` 返回就到达（实测 browse 在 399ms
 *  返回，6 条事件落在 398~399ms）。那一刻 `state.remoteItems` 还是上一屏，
 *  findIndex 找不到 id，事件被丢掉；紧接着骨架整份覆盖上来，
 *  界面就永远停在「识别中」。
 *
 *  注意这是缓存优化把一个潜伏的竞态变成了必现故障——在那之前识别要 1.5 秒，
 *  稳稳晚于 browse 返回，所以从没暴露过。 */
const remoteEntryBuffer = new Map();

/** 暂存区的键。用 \u0000 分隔是因为它不可能出现在位置 id 或对话 id 里；
 *  用 ':' 会和 Telegram 的 `tg:<对话>:<消息>` 撞上。 */
/** 把一屏列表快照存进磁盘 meta 缓存（kind='list'）。
 *  去掉 thumb_token（会话句柄，重启失效）；只留名字/大小/结构等可明文项。
 *  真实文件名(.omy 解密名)不在 browse 返回里、不会进这里，隐私安全。 */
async function saveDirSnapshotToDisk(key, rows) {
  try {
    const slim = rows.map((r) => {
      const { thumb_token, ...rest } = r;
      return rest;
    });
    const json = JSON.stringify(slim);
    const bytes = new TextEncoder().encode(json);
    await api.remoteMetaPut('list', key, bytes);
  } catch {
    // 缓存是附加项，写不进只降级为「重启后走网络」，不影响浏览
  }
}

/** 从磁盘 meta 缓存读回一屏列表快照；未命中/损坏返回 null。 */
async function loadDirSnapshotFromDisk(key) {
  try {
    const bytes = await api.remoteMetaGet('list', key);
    if (!bytes || !bytes.length) return null;
    const json = new TextDecoder().decode(new Uint8Array(bytes));
    const rows = JSON.parse(json);
    return Array.isArray(rows) ? rows : null;
  } catch {
    return null;
  }
}

function remoteDirKey(placeId, dir, tab) {
  // tab 只在**内容缓存**里带上（同一对话媒体栏/文件栏是两批内容）。
  // 识别事件的暂存区仍用不带 tab 的 2 参数键：识别结果按文件 id 贴，
  // 与它属于哪一栏无关，两者共用同一个函数但传参不同。
  return tab
    ? `${placeId}\u0000${dir}\u0000${tab}`
    : `${placeId}\u0000${dir}`;
}

/** 当前是否在某个 Telegram 对话内（而非根对话列表 / 网盘）。
 *  分栏只在这种情况下有意义。 */
function isTelegramDialog() {
  if (!state.remoteDir) return false;
  const p = state.remotePlaces.find((x) => x.id === state.remotePlace);
  return p?.kind === 'telegram';
}

/** 首屏该拉多少条：填满可见网格 + 一屏缓冲。
 *
 * 官方那样「填满视口就够、别一次拉几百条」。按卡片估行列数：网格卡片约
 * 132px 宽、156px 高（见 PlaceBrowser 的 .grid 覆盖）。估不出来（无 DOM、
 * 尺寸为 0）时给一个稳妥的默认，宁可略多一点也不要首屏留空。 */
function viewportFillCount() {
  const FALLBACK = 30;
  try {
    const grid = document.querySelector('.pb .grid, .pbroot .grid');
    const host = grid?.parentElement || document.querySelector('.pb, .pbroot');
    if (!host) return FALLBACK;
    const w = host.clientWidth || 0;
    const h = host.clientHeight || 0;
    if (w < 40 || h < 40) return FALLBACK;
    const cols = Math.max(1, Math.floor(w / 132));
    const rows = Math.max(1, Math.ceil(h / 156));
    // 可见格数 + 一屏缓冲，夹在 [15, 100]：太小翻页太频繁、太大又违背
    // 「填满即可」的初衷，后端也封顶 100
    const n = cols * rows * 2;
    return Math.min(100, Math.max(15, n));
  } catch {
    return FALLBACK;
  }
}

/** 已加载过的目录列表：`位置\x01目录` -> 条目数组。
 *
 * # 为什么要缓存
 *
 * 实测后端 remote_browse 连续五次是 6332 / 408 / 408 / 428 / 439 ms
 * ——**连接是复用的**（第一次含握手），但没有任何列表缓存，每次进对话
 * 都真的去 RPC 一趟。界面上表现为每次都有约 0.4 秒空白 + 骨架重画，
 * 也就是用户说的「每次进来都需要重新加载」。
 *
 * 缓存之后再进立刻见内容，同时后台静默刷新、有变化才替换。
 *
 * **刻意不设过期时间**：静默刷新总会发生，加个 TTL 只会让「多久算新鲜」
 * 变成一个要调的参数，而调错的表现是用户看到过期列表。
 */
const remoteDirCache = new Map();

// ⚠️ 这个声明必须留在 reloadRemoteDir **之前**。
//
// 它原来写在文件后半，而 reloadRemoteDir 在前面就读它——`const` 不像
// `function` 会提升，模块顶层的 const 在求值到那一行之前处于时间死区，
// 提前访问直接抛 ReferenceError。
//
// 而那个错误**不会显示出来**：reloadRemoteDir 是 async，抛出变成
// rejected promise 被静默吞掉，表现只是「缓存好像没生效」，甚至因为
// 走了退化路径而比不缓存更慢（实测二次进入 1.33s，比首次 0.47s 还慢）。

/** 把暂存区里属于当前这一屏的识别结果贴到列表上。
 *
 *  `reloadRemoteDir` 写完骨架后立刻调用，专治「事件比骨架先到」。 */
function applyBufferedRemoteEntries() {
  const buf = remoteEntryBuffer.get(remoteDirKey(state.remotePlace, state.remoteDir));
  if (!buf || buf.size === 0) return;
  for (let i = 0; i < state.remoteItems.length; i += 1) {
    const hit = buf.get(state.remoteItems[i].id);
    if (hit) state.remoteItems[i] = hit;
  }
}

/**
 * 全局只注册一次「边扫边出」监听：remote_browse 返回骨架后，后台每识别完
 * 一个文件推一条 remote-entry，这里只在事件仍属于当前位置+当前目录时，
 * 就地替换同 id 骨架；切走目录后晚到的事件直接丢弃，绝不串屏。
 */
let remoteListenerStarted = false;
export async function ensureRemoteListeners() {
  if (remoteListenerStarted) return;
  remoteListenerStarted = true;
  await api.onRemoteEntry((p) => {
    if (!p || !p.entry) return;
    if (p.place_id !== state.remotePlace || p.dir !== state.remoteDir) return;
    // 先存再贴：骨架可能还没到（见 remoteEntryBuffer 的说明）。
    // 只为当前位置+目录暂存，切走后晚到的事件连存都不存，
    // 否则反复进出目录会让这张表一直涨。
    const key = remoteDirKey(p.place_id, p.dir);
    let buf = remoteEntryBuffer.get(key);
    if (!buf) {
      buf = new Map();
      remoteEntryBuffer.set(key, buf);
    }
    buf.set(p.entry.id, p.entry);
    const idx = state.remoteItems.findIndex((x) => x.id === p.entry.id);
    if (idx >= 0) state.remoteItems[idx] = p.entry;
    // **写透到内容缓存**：识别结果与清晰缩略图 token 都通过这个事件回来。
    // 只更新 state.remoteItems 而不更新缓存的话，二次进对话摆出的是缓存里
    // 那批**带 stripped 占位 token 的旧行**，于是清晰图退回糊占位、又得重新
    // 后台拉一遍——这正是用户报的「切出切入清晰缩略图退回占位」。
    // 缓存键要带当前分栏（媒体栏和文件栏是两批）。
    const tab = isTelegramDialog() ? state.remoteTab : '';
    const ck = remoteDirKey(p.place_id, p.dir, tab);
    const cachedArr = remoteDirCache.get(ck);
    if (cachedArr) {
      const ci = cachedArr.findIndex((x) => x.id === p.entry.id);
      if (ci >= 0) cachedArr[ci] = p.entry;
    }
  });
}

/** 把一个已解锁的远程 `.omy` 流式解密到用户选择的本地目录。
 *  只读位置也保留（这是只读位置的主要用途）。复用全局解密进度条；
 *  默认不覆盖，目标已存在时后端返回 target_exists。 */
export async function decryptRemoteToLocal(f) {
  if (!state.remotePlace || !f || f.is_dir || !f.is_encrypted || !f.unlocked) return null;
  let dest;
  try {
    dest = await api.pickFolder(i18n.t('rplace.decrypt_pick_title'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.decrypt_failed'));
    return null;
  }
  if (!dest) return null; // 用户在目录选择器里取消

  state.busy = true;
  state.busyKey = 'busy.remote_decrypt';
  state.error = '';
  state.notice = '';
  state.progress = null;
  let unlisten = null;
  try {
    unlisten = await api.onDecryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    unlisten = null;
  }
  try {
    const r = await api.remoteDecryptToLocal(state.remotePlace, f.id, f.size || 0, dest);
    setNotice(i18n.t('rplace.decrypted_local', { name: r.name, dir: dest }));
    return r;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.decrypt_failed'));
    return null;
  } finally {
    state.busy = false;
    state.busyKey = '';
    state.progress = null;
    if (unlisten) unlisten();
  }
}

/** 远程单文件缓存状态在 state.remoteCacheStat 里的键。 */
function remoteFileCacheKey(placeId, path) {
  // 控制字符分隔，正常路径里不会出现，避免 place/path 粘连
  return `${placeId}\u{1}${path}`;
}

/**
 * 打开条目菜单时按需查询该文件的密文块缓存覆盖情况（不阻塞菜单弹出）。
 * 列目录不逐文件查：一部几 GB 的电影有数千块，逐文件 stat 会拖慢整屏扫描。
 */
export async function requestRemoteFileCache(f) {
  // 普通文件也要查：远程位置里它们是主体内容，同样走块缓存、
  // 同样值得永久保留（离线看视频）。原先这里排除非加密文件，
  // 于是普通文件的右键菜单里永远没有「转为永久」，而后端明明支持。
  if (!state.remotePlace || !f || f.is_dir) return;
  try {
    const stat = await api.remoteCacheFileStat(state.remotePlace, f.id, f.size || 0);
    const key = remoteFileCacheKey(state.remotePlace, f.id);
    state.remoteCacheStat = { ...state.remoteCacheStat, [key]: stat };
  } catch {
    // 查不到（非加密 / 头部读取失败）就当无缓存：菜单本就可以没有这一项，不打扰用户
  }
}

/** 读取已查询过的单文件缓存状态（供菜单响应式判断是否显示「从缓存中移除」）。 */
export function remoteFileCache(f) {
  if (!f || !state.remotePlace) return null;
  return state.remoteCacheStat[remoteFileCacheKey(state.remotePlace, f.id)] || null;
}

/** 选本地文件上传到当前远程目录。
 *
 * 上传用的是**磁盘上的文件名**（后端保证）。用户开了文件名加密时磁盘上
 * 就是那串密文名——用解密后的真名上传等于把加密掉的文件名主动交给服务端。
 *
 * 部分成功是常态（网络抖动、个别文件超限），所以逐个报而不是一句
 * 「失败了」：不区分的话用户不知道哪些传上去了，再传一次就产生重复文件。
 */
export async function uploadToRemote() {
  if (!state.remotePlace || !state.remoteDir) return false;
  let paths;
  try {
    paths = await api.pickFiles(i18n.t('rplace.upload_pick_title'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.upload_failed'));
    return false;
  }
  if (!paths || !paths.length) return false; // 用户取消

  state.busy = true;
  state.busyKey = 'busy.uploading';
  state.error = '';
  state.notice = '';
  try {
    const res = await api.remoteUpload(state.remotePlace, state.remoteDir, paths);
    const ok = res.filter((r) => r.ok).length;
    const bad = res.filter((r) => !r.ok);
    // 传完必须重新列目录，否则用户看不到刚传上去的东西、以为失败了
    await reloadRemoteDir();
    if (bad.length && ok) {
      // 部分成功要说清哪些没成——只说「部分失败」的话，
      // 用户只能整批重传，于是产生重复文件
      state.error = i18n.t('rplace.upload_partial', {
        ok,
        names: bad.map((b) => b.name).join('、'),
      });
    } else if (bad.length) {
      state.error = i18n.t('rplace.upload_failed');
    } else {
      setNotice(i18n.tn('rplace.upload_done', ok));
    }
    return ok > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.upload_failed'));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 把一个远程文件转为**永久缓存**。
 *
 * 先把整个文件预热到本地再标记。不预热的话永久层里只有碰巧缓存过的
 * 那几块，而用户以为整个文件都留下了——直到离线打开失败才发现。
 * 那是承诺与事实不符，比不提供这个功能更糟。
 *
 * 预热要下整个文件，所以走 busy 态而不是静默进行。
 */
export async function pinRemoteFile(f) {
  if (!state.remotePlace || !f || f.is_dir) return false;
  state.busy = true;
  state.busyKey = 'busy.pinning';
  state.error = '';
  try {
    // 带上列表里显示的那个名字。不传的话后端只能从 id 猜，而 Telegram
    // 的 id 是 tg:<对话>:<消息号>，猜出来是个数字——传输列表里就会
    // 出现一行「6」，用户不知道那是哪个文件（实测过）
    // 注意：remoteCachePin 返回的是**字节数**（u64），不是 FileCacheStat。
    // 早先把它当 stat 存进 remoteCacheStat，于是 st.pinned 是 undefined，
    // 卡片标识判成 false（菜单却"对"，因为菜单打开时另查了一次真 stat
    // 覆盖掉那个数字——同一份数据两条路径写成不同形状）。
    await api.remoteCachePin(
      state.remotePlace,
      f.id,
      f.size || 0,
      f.real_name || f.name || null,
    );
    // 重新查一次真 stat 填进去。用 requestRemoteFileCache（单文件、
    // 与右键菜单同一条路径），不用批量版：批量版在单文件场景实测不生效
    // （最可能是 f 缺 size 兜底），而单数路径一直可靠
    await requestRemoteFileCache(f);
    setNotice(i18n.t('rplace.pinned'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.pin_failed'));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 取消永久缓存。
 *
 * 内容会搬回临时层，也就是**重新计入上限、重新参与淘汰**，可能很快被清掉。
 * 那正是「取消永久」该有的语义；只改标记的话空间不会真的还回来。
 */
export async function unpinRemoteFile(f) {
  if (!state.remotePlace || !f || f.is_dir) return false;
  try {
    // remoteCacheUnpin 返回字节数、不是 stat。用单数路径重查真 stat
    await api.remoteCacheUnpin(state.remotePlace, f.id, f.size || 0);
    await requestRemoteFileCache(f);
    setNotice(i18n.t('rplace.unpinned'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.unpin_failed'));
    return false;
  }
}

/** 删除单个远程文件的本地密文块。只读位置也允许：只清本机缓存，绝不写云端。 */
export async function removeRemoteFileCache(f) {
  // 同 requestRemoteFileCache：普通文件也有缓存块要清
  if (!state.remotePlace || !f || f.is_dir) return false;
  try {
    const r = await api.remoteCacheRemoveFile(state.remotePlace, f.id, f.size || 0);
    const key = remoteFileCacheKey(state.remotePlace, f.id);
    const prev = state.remoteCacheStat[key];
    const zero = {
      cached_blocks: 0,
      total_blocks: prev ? prev.total_blocks : 0,
      cached_bytes: 0,
      fully_cached: false,
      // 「从缓存中移除」只清临时层，不动永久标记，所以这一位要沿用原值
      // 而不是一律置 false——置 false 会让一个仍然永久保留的文件在界面上
      // 显示成普通缓存，用户以为自己的永久标记丢了
      pinned: prev ? prev.pinned === true : false,
    };
    state.remoteCacheStat = { ...state.remoteCacheStat, [key]: zero };
    setNotice(i18n.t('rplace.removed_cache', { size: i18n.formatSize(r.freed_bytes || 0) }));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.remove_cache_failed'));
    return false;
  }
}

/** 当前位置：侧栏高亮的**唯一**真相。
 *
 * 返回 `{ kind: 'local'|'remote'|'transfers', key }`。
 *
 * # 为什么要这个派生值
 *
 * 原来侧栏两处各自判断——本地看 `state.cwd === p.path`、远程看
 * `state.remotePlace === p.id`。而 `state.cwd` 进远程后**不会清空**
 * （也不该清空：退出远程要回到原来那个目录），于是两个条件同时成立，
 * 侧栏同时高亮本地「文档」和远程「Lol」。
 *
 * 不能靠「进远程就清 cwd」来解决：那会让退出远程后落到一个空目录，
 * 把导航弄坏。所以改成从「当前在哪个视图」+「那个视图里选了什么」
 * 算出唯一位置，两边高亮都从它派生。
 *
 * 这样「不会同时亮两个」是**结构上**的保证，而不是靠两处条件恰好互斥
 * ——后者在下一次有人新增视图时会再次失效。
 */
export const activeLocation = computed(() => {
  if (state.transfersOpen) return { kind: 'transfers', key: '' };
  if (state.placeBrowserOpen) {
    return { kind: 'remote', key: state.remotePlace || '' };
  }
  return { kind: 'local', key: state.cwd || '' };
});

/** 打开云盘浏览器（停在位置列表）。 */
export async function openPlaceBrowser() {
  state.transfersOpen = false;
  state.placeBrowserOpen = true;
  await reloadRemotePlaces();
}/** 打开云盘浏览器并直接进入某个已保存位置（桌面侧栏入口）。
 *  必须同时置 placeBrowserOpen，否则只加载了目录数据、视图却还停在本地，
 *  表现为点侧栏云盘项「没反应」。 */
export async function openPlaceBrowserAt(id) {
  state.transfersOpen = false;
  state.placeBrowserOpen = true;
  await openRemotePlace(id);
}

/** 关闭云盘浏览器，回到本地文件，并退出当前位置。 */
/** 打开传输管理页。
 *
 * 它是顶层入口而不是某个位置的子页：任务本身跨对话、跨位置，
 * 挂在某个位置下面的话，用户切走就找不到了。 */
export function openTransfers() {
  state.transfersOpen = true;
  state.placeBrowserOpen = false;
}

/** 关闭传输管理页，回到文件浏览。 */
export function closeTransfers() {
  state.transfersOpen = false;
}

export function closePlaceBrowser() {
  state.placeBrowserOpen = false;
  leaveRemotePlace();
}

/** 退出所有整屏覆盖的视图，回到本地文件浏览。
 *
 * # 为什么要收口在一处
 *
 * 这些视图都是**整屏覆盖**的。少关一个，本地导航就会「已经发生了、
 * 却被盖住」——用户看到的是点了没反应。
 *
 * 原来调用方只关云盘视图（那时只有它），后来加了传输管理页却没跟着改，
 * 于是从传输页点本地位置界面不动。与其在每个调用方列举要关哪些，
 * 不如收在这里：**以后新增整屏视图只改这一处**。
 */
export function leaveOverlays() {
  state.transfersOpen = false;
  closePlaceBrowser();
}

/** 移除一个远程位置；若正在浏览它，先退回到位置列表。 */
export async function removeRemotePlace(id) {
  await api.remotePlaceRemove(id);
  await afterPlaceGone(id);
}

/** 位置消失后的统一收尾：正在浏览它就先退出，然后刷新列表。
 *
 * 三个入口（通用移除、Telegram 摘除、删除账号）共用这一份。分别写的话，
 * 少写一次 leaveRemotePlace 的那个入口会把用户留在一个已经不存在的位置里，
 * 而那时任何操作都只会报「位置不存在」。 */
async function afterPlaceGone(id) {
  if (state.remotePlace === id) leaveRemotePlace();
  await reloadRemotePlaces();
}

/** Telegram：只把位置从列表摘掉，保留本机登录态（之后可直接加回来）。 */
export async function detachTelegramPlace(id) {
  await api.telegramPlaceDetach(id);
  await afterPlaceGone(id);
  setNotice(i18n.t('rplace.detached'));
}

/** Telegram：删除账号，连本机登录态一起清掉。**不可撤销。** */
export async function deleteTelegramAccount(id) {
  await api.telegramPlaceDeleteAccount(id);
  await afterPlaceGone(id);
  setNotice(i18n.t('rplace.account_deleted'));
}

/** Telegram：改这个位置在本机的显示名。
 *
 * 空名字直接不改：留一个没有名字的位置，侧栏上就只剩一个云图标，
 * 多账号时完全分不清谁是谁。 */
export async function renameTelegramPlace(id, name) {
  const trimmed = (name || '').trim();
  if (!trimmed) return;
  await api.telegramPlaceRename(id, trimmed);
  await reloadRemotePlaces();
  setNotice(i18n.t('rplace.renamed'));
}

/** Telegram：显式加密这个位置——用当前已解锁的 omy 密码保护它的登录态。
 *
 * 失败（如还没解锁任何 omy 库）时把错误码翻成可读文案提示，不静默吞掉。 */
export async function encryptTelegramPlace(id) {
  try {
    const changed = await api.telegramPlaceEncrypt(id);
    setNotice(i18n.t(changed ? 'rplace.encrypted' : 'rplace.already_encrypted'));
    return true;
  } catch (e) {
    setNotice(i18n.te(api.errCode(e), 'errors.tg_encrypt_failed'));
    return false;
  }
}

/** Telegram：取消加密这个位置，转回默认保护。需当前有能打开它的密码。 */
export async function decryptTelegramPlace(id) {
  try {
    const changed = await api.telegramPlaceDecrypt(id);
    setNotice(i18n.t(changed ? 'rplace.decrypted' : 'rplace.already_plain'));
    return true;
  } catch (e) {
    setNotice(i18n.te(api.errCode(e), 'errors.tg_decrypt_failed'));
    return false;
  }
}

/** 进入远程子目录。 */
export async function enterRemoteDir(id, name) {
  state.remoteDir = id;
  // 换对话/回根都回到默认媒体栏：留着上个对话选的栏，会让用户以为
  // 新对话「只有链接」之类（其实是停在链接栏且新对话没链接）
  state.remoteTab = 'media';
  // 记下显示名。拿不到就留空，由面包屑那边决定怎么兜底——
  // 不要在这里回落到 id，那样面包屑就无从区分「有名字」和「没名字」了
  state.remoteDirName = name || '';
  // 上报用户动作（不带对话名/id 明文，只记"进目录"还是"回根"）
  api.uiLog(id ? 'enter-dir' : 'enter-root');
  await reloadRemoteDir();
}

/** 远程位置里返回上一级。
 *
 * 已在根时离开该位置回到本地，而不是什么都不做——否则用户会觉得
 * 返回键卡住了。
 */
export async function remoteGoUp() {
  const cur = state.remoteDir.replace(/\/+$/, '');
  if (!cur) {
    leaveRemotePlace();
    return;
  }
  // 上一级怎么算，取决于 provider 的 id 形状——不能一律按 '/' 切。
  //
  // 这是本文件里第四处「写死 WebDAV 路径形状」的地方（前三处：kindLabel
  // 硬写 WebDAV、remote_probe_entry 按 '/' 猜名字、crumbs 按 '/' 切段）。
  // Telegram 的 dir 是 `tg:<对话>`，里面没有斜杠，lastIndexOf 返回 -1，
  // 于是直接置空回到对话列表——**结果碰巧是对的**（对话只有一层），
  // 但那靠的是「没有斜杠」这个副作用，不是「这个 provider 有几层」。
  //
  // 而且它绕过了 enterRemoteDir，于是不会清 remoteDirName /
  // remoteViewMode / remoteMessages：从对话返回后面包屑还挂着上一个
  // 对话的名字。所以这里统一走 enterRemoteDir，让复位只有一处。
  const i = cur.lastIndexOf('/');
  const parent = i > 0 ? cur.slice(0, i) : '';
  // 回到根（对话列表）时名字也要空，否则面包屑会留着上一层的名字
  await enterRemoteDir(parent, '');
}

/** 添加远程位置后刷新列表并进入它。 */
export async function afterPlaceAdded(id) {
  await reloadRemotePlaces();
  await openRemotePlace(id);
}

// 凭据归零就退出容器视图，不依赖某处记得清状态。
//
// 容器视图里列的是**解密出来的文件名**——锁定的语义是「这些都不该再
// 看得见」。`lock()` 确实清了，但那是一处容易在重构中被漏掉的赋值；
// 这里再守一道，让「锁定即不可见」不依赖某一行代码没被删掉。
// 这与原先 ContainerPanel 自己监听凭据数是同一个道理，面板没了，
// 这道保证要跟着搬过来而不是丢掉。
watch(
  () => state.credentials,
  (n) => {
    if (n === 0) state.container = null;
  },
);

/** 补齐某个已解锁文件的媒体元信息。 */
export async function enrich(entryId) {
  try {
    return await api.enrichFile(entryId);
  } catch {
    return null;
  }
}
