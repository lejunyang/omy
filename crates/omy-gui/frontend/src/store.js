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
  /** 已识别的加密文件，按路径索引，用于预览。 */
  known: {},
  /** 选中的路径集合。 */
  selected: [],
  /** 视图模式。 */
  view: 'grid',
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
    // 锁定的加密文件没有可搜的名字，搜索时直接排除：
    // 用磁盘文件名去匹配会泄露信息
    if (e.is_encrypted && !e.unlocked) return false;
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

/** 当前目录里加密文件的数量。容器内恒为 0：里面的东西已经解出来了。 */
export const encryptedCount = computed(() =>
  state.container ? 0 : state.entries.filter((e) => e.is_encrypted).length,
);

/** 其中还锁着的数量。 */
export const lockedCount = computed(() =>
  state.container
    ? 0
    : state.entries.filter((e) => e.is_encrypted && !e.unlocked).length,
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
 */
export const keyManageable = computed(() => {
  if (state.container) return null;
  if (state.selected.length !== 1) return null;
  const e = state.entries.find((x) => x.path === state.selected[0]);
  return e && e.is_encrypted && e.unlocked ? e : null;
});

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
    const r = await api.manageKey(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await reload();
    setNotice(i18n.t(`keymgmt.done_${r.action}`));
    return r;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
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
    const opened = state.entries.filter((e) => e.is_encrypted && e.unlocked).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      // 派生成功但一个文件也没解开 = 密码不对。
      // 这个区分很重要：KEK 派生几乎总是"成功"的，
      // 真正的判据是有没有文件被解开
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
