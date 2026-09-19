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
  // 在容器里就列容器的内容。同一个 computed 供主界面使用，
  // 这样网格、列表、搜索、状态栏全都不需要知道自己在哪种位置
  const source = state.container ? containerEntries.value : state.entries;
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

export async function tryUnlock(password) {
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

/** 当前远程位置的能力位图。
 *
 * 不在远程位置时返回本地的全能力——这样上层判断可以统一写
 * `caps.write`，不必到处分叉「是不是远程」。
 */
export const currentCaps = computed(() => {
  if (!state.remotePlace) {
    return {
      read: true,
      write: true,
      delete: true,
      rename: true,
      create_dir: true,
      random_write: true,
      range_read: true,
    };
  }
  const p = state.remotePlaces.find((x) => x.id === state.remotePlace);
  // 找不到时按只读处理：宁可少几个按钮，也不要对一个状态不明的位置
  // 发起写操作
  return (
    p?.caps || {
      read: true,
      write: false,
      delete: false,
      rename: false,
      create_dir: false,
      random_write: false,
      range_read: true,
    }
  );
});

/** 刷新远程位置列表。 */
export async function reloadRemotePlaces() {
  state.remotePlaces = await api.remotePlaceList().catch(() => []);
}

/** 进入一个远程位置的根目录。 */
export async function openRemotePlace(id) {
  state.remotePlace = id;
  state.remoteDir = '';
  await reloadRemoteDir();
}

/** 离开远程位置，回到本地浏览。 */
export function leaveRemotePlace() {
  state.remotePlace = '';
  state.remoteDir = '';
  state.remoteItems = [];
  state.remoteRetrying = [];
}

/** 列出当前远程目录。
 *
 * 失败时清空列表并报错，而不是留着上一个目录的内容——那会让用户
 * 以为自己进到了一个内容相同的目录。
 */
export async function reloadRemoteDir() {
  if (!state.remotePlace) return;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  state.remoteRetrying = [];
  try {
    state.remoteItems = await api.remoteBrowse(state.remotePlace, state.remoteDir);
  } catch (e) {
    state.remoteItems = [];
    // 用云盘视图自己的错误位，不弹底部全局条——那是给本地操作留的
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
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
    const entry = await api.remoteProbeEntry(state.remotePlace, f.id, f.size || 0);
    patchRemoteItem(entry);
  } catch (_) {
    state.remoteRetrying = state.remoteRetrying.filter((id) => id !== f.id);
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
    const idx = state.remoteItems.findIndex((x) => x.id === p.entry.id);
    if (idx >= 0) state.remoteItems[idx] = p.entry;
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
  if (!state.remotePlace || !f || f.is_dir || !f.is_encrypted) return;
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

/** 删除单个远程文件的本地密文块。只读位置也允许：只清本机缓存，绝不写云端。 */
export async function removeRemoteFileCache(f) {
  if (!state.remotePlace || !f || f.is_dir || !f.is_encrypted) return false;
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

/** 打开云盘浏览器（停在位置列表）。 */
export async function openPlaceBrowser() {
  state.placeBrowserOpen = true;
  await reloadRemotePlaces();
}/** 打开云盘浏览器并直接进入某个已保存位置（桌面侧栏入口）。
 *  必须同时置 placeBrowserOpen，否则只加载了目录数据、视图却还停在本地，
 *  表现为点侧栏云盘项「没反应」。 */
export async function openPlaceBrowserAt(id) {
  state.placeBrowserOpen = true;
  await openRemotePlace(id);
}

/** 关闭云盘浏览器，回到本地文件，并退出当前位置。 */
export function closePlaceBrowser() {
  state.placeBrowserOpen = false;
  leaveRemotePlace();
}

/** 移除一个远程位置；若正在浏览它，先退回到位置列表。 */
export async function removeRemotePlace(id) {
  await api.remotePlaceRemove(id);
  if (state.remotePlace === id) leaveRemotePlace();
  await reloadRemotePlaces();
}

/** 进入远程子目录。 */
export async function enterRemoteDir(id) {
  state.remoteDir = id;
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
  const i = cur.lastIndexOf('/');
  state.remoteDir = i > 0 ? cur.slice(0, i) : '';
  await reloadRemoteDir();
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
