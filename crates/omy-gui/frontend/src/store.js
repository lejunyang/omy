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

import { reactive, computed } from 'vue';
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
  /** 错误文案。 */
  error: '',
  /** 提示文案（成功类）。 */
  notice: '',
  /** 协议前缀，由后端按平台下发。 */
  streamBase: 'omystream://localhost',
});

/** 拼出某个文件的内容 URL。 */
export function fileUrl(id) {
  return `${state.streamBase}/file/${encodeURIComponent(id)}`;
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
  const q = state.query.trim().toLowerCase();
  if (!q) return state.entries;
  return state.entries.filter((e) => {
    // 锁定的加密文件没有可搜的名字，搜索时直接排除：
    // 用磁盘文件名去匹配会泄露信息
    if (e.is_encrypted && !e.unlocked) return false;
    const name = e.real_name || e.name;
    return name.toLowerCase().includes(q);
  });
});

/** 当前目录里加密文件的数量。 */
export const encryptedCount = computed(
  () => state.entries.filter((e) => e.is_encrypted).length,
);

/** 其中还锁着的数量。 */
export const lockedCount = computed(
  () => state.entries.filter((e) => e.is_encrypted && !e.unlocked).length,
);

/** 选中项里可加密的（排除已经是加密文件的）。 */
export const encryptable = computed(() =>
  state.selected
    .map((p) => state.entries.find((e) => e.path === p))
    .filter((e) => e && !e.is_encrypted),
);

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
    await refreshKnown();
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.busy = false;
  }
}

/** 回到上一级。 */
export async function goUp() {
  if (!state.cwd) return;
  const parent = await api.parentOf(state.cwd).catch(() => null);
  if (parent) await navigate(parent);
}

/** 刷新当前目录。 */
export async function reload() {
  if (state.cwd) await navigate(state.cwd);
}

/** 载入侧栏起点。 */
export async function loadPlaces() {
  state.places = await api.listPlaces().catch(() => []);
}

/** 面包屑的各段。 */
export const crumbs = computed(() => {
  if (!state.cwd) return [];
  // Windows 用反斜杠，Unix 用正斜杠。分割后重新拼接成可点击的路径
  const sep = state.cwd.includes('\\') ? '\\' : '/';
  const parts = state.cwd.split(/[\\/]/).filter(Boolean);
  const out = [];
  let acc = '';
  for (const [i, p] of parts.entries()) {
    if (i === 0) {
      // Windows 的 "C:" 要补上分隔符才是合法路径；
      // Unix 的第一段前面要加根斜杠
      acc = sep === '\\' ? `${p}${sep}` : `${sep}${p}`;
    } else {
      acc = `${acc}${acc.endsWith(sep) ? '' : sep}${p}`;
    }
    out.push({ name: p, path: acc });
  }
  return out;
});

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
      state.notice = i18n.tn('notice.encrypted', summary.items.length);
    }
    return summary;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.encrypt_failed'));
    return null;
  } finally {
    state.busy = false;
  }
}

/** 用一个密码试解锁当前目录。 */
export async function tryUnlock(password, label) {
  if (!state.cwd) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.unlockDirectory(state.cwd, label || 'main', password);
    state.credentials = r.credentials;
    await reload();
    const opened = state.entries.filter((e) => e.is_encrypted && e.unlocked).length;
    if (opened > 0) {
      state.notice = i18n.tn('notice.unlocked', opened);
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

    // 把 entry_id 补回列表，让预览能找到对应条目
    for (const e of state.entries) {
      const f = map[e.path];
      if (f && f.unlocked) {
        e.unlocked = true;
        e.real_name = f.name;
        e.entry_id = f.id;
      }
    }
  } catch {
    // 扫描失败不影响浏览——列表还在，只是加密文件显示成锁定
  }
}

/** 锁定：抹掉所有凭据。 */
export async function lock() {
  await api.lock();
  state.credentials = 0;
  state.known = {};
  // 重新载入让加密文件回到锁定显示。
  // 不能只改本地字段——那样万一漏改一处就是信息泄露
  await reload();
}

/** 切换语言。 */
export async function switchLanguage() {
  const next = i18n.nextLang();
  await i18n.load(next);
  await api.setLanguage(next);
  localStorage.setItem('omy.lang', next);
}

/** 补齐某个已解锁文件的媒体元信息。 */
export async function enrich(entryId) {
  try {
    return await api.enrichFile(entryId);
  } catch {
    return null;
  }
}
