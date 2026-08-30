/** 会话与文件列表状态。
 *
 * # 锁定态是数据，不是视图
 *
 * 文档 §10 从原型验证中得出的结论：锁定**不能**用「锁定时覆写样式、
 * 解锁时重设为正常」实现。因为有些文件本来就因为密码不匹配而处于
 * 锁定态，统一重设会把它们错误地显示成已解锁——这是信息泄露。
 *
 * 所以 `unlocked` 是每个文件自己的字段，界面完全由数据推导。
 * 锁定时后端清空列表，前端拿到空数组，没有「还原」这回事。
 *
 * 用 Vue 之后这条原则更容易守住：模板里 `v-if="file.unlocked"`
 * 是声明式的，不存在「忘记重置某个 class」的可能。
 */

import { reactive, computed } from 'vue';
import * as api from './api.js';
import * as i18n from './i18n.js';

export const state = reactive({
  files: [],
  view: 'grid',
  query: '',
  unlocked: false,
  credentials: 0,
  roots: [],
  vaults: null,
  folder: '',
  busy: false,
  error: '',
  /** 自定义协议的 URL 前缀，由后端按平台下发。
   *
   * 不能写死：Windows/Android 的 WebView2 不认自定义 scheme，
   * Tauri 会把它映射成 `http://omystream.localhost`；
   * 而 macOS/Linux 用原生的 `omystream://localhost`。
   * 写死任何一种，另一半平台就完全加载不了内容。
   */
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

/** 过滤并排序后的文件。 */
export const visibleFiles = computed(() => {
  const q = state.query.trim().toLowerCase();
  const list = q
    ? state.files.filter((f) => f.unlocked && f.name.toLowerCase().includes(q))
    : state.files.slice();

  // locale-aware 排序：中文按拼音，数字按数值（文档 §7.4）
  const c = i18n.collator();
  list.sort((a, b) => {
    // 锁定的排在后面——它们没有可读的名字，混在中间很碍眼
    if (a.unlocked !== b.unlocked) return a.unlocked ? -1 : 1;
    return c.compare(a.name, b.name);
  });
  return list;
});

/** 已解锁文件的明文总大小。 */
export const totalSize = computed(() =>
  state.files.reduce((sum, f) => sum + (f.size || 0), 0),
);

/* ---------------- 动作 ---------------- */

/** 选择文件夹并探测其中的 vault。 */
export async function pickFolder() {
  // 原生目录选择器由后端弹出（前端没有 dialog 权限，只能走这个入口）。
  // 用户取消时返回 null，是正常操作而非错误，静默返回
  let dir;
  try {
    dir = await api.pickFolder(i18n.t('unlock.browse'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
    return;
  }
  if (!dir) return;

  state.error = '';
  try {
    // 后端返回的是一组 vault（一个文件夹里可能混着多个独立的库，
    // 每个库有自己的 salt），解锁时对每个都派生一次
    state.vaults = await api.vaultParamsOf(dir);
    state.folder = dir;
    if (state.unlocked) await scan(dir);
  } catch (e) {
    state.vaults = null;
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  }
}

/** 执行解锁。 */
export async function unlock(label, password) {
  if (state.busy || !state.vaults) return;
  state.busy = true;
  state.error = '';
  try {
    const r = await api.unlock(label || 'main', password, state.vaults);
    state.credentials = r.credentials;
    state.unlocked = true;
    state.busy = false;
    await scan(state.folder);
  } catch (e) {
    state.busy = false;
    state.error = i18n.te(api.errCode(e));
  }
}

/** 扫描目录。 */
export async function scan(dir) {
  try {
    state.files = await api.scanDirectory(dir, true);
    state.roots = await api.listRoots();
    // 元信息按需补：列表先出来，避免大目录卡住首屏（文档 §9）
    enrichVisible();
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.scan_failed'));
  }
}

/** 逐个补齐媒体元信息。 */
async function enrichVisible() {
  const targets = state.files.filter((f) => f.unlocked && !f.kind);
  for (const f of targets) {
    // 锁定后停止：用户可能在补元信息的过程中锁定，
    // 继续下去就是往已清空的列表里塞数据
    if (!state.unlocked) return;
    try {
      const updated = await api.enrichFile(f.id);
      if (!updated) continue;
      const i = state.files.findIndex((x) => x.id === updated.id);
      if (i >= 0) state.files[i] = updated;
    } catch {
      // 单个文件补不上不影响其余——它仍会以通用图标显示
    }
  }
}

/** 锁定。 */
export async function lock() {
  await api.lock();
  // 后端已清空列表，前端同步清空。
  // 不做「保留数据只切视图」——那样锁定就是假象
  state.unlocked = false;
  state.files = [];
  state.credentials = 0;
  state.query = '';
}

/** 切换语言。 */
export async function switchLanguage() {
  const next = i18n.nextLang();
  await i18n.load(next);
  await api.setLanguage(next);
  localStorage.setItem('omy.lang', next);
}
