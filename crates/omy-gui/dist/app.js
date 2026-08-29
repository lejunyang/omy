/** omy 主界面逻辑。
 *
 * # 渲染原则：数据驱动，绝不「可逆覆盖」
 *
 * 文档 §10 从原型验证中得出的结论：锁定态**不能**用「锁定时覆写
 * 样式、解锁时重设为正常」实现。因为有些文件本来就因为密码不匹配
 * 而处于锁定态，统一重设会把它们错误地显示成已解锁——这是信息泄露。
 *
 * 所以这里每次都从 `state.files` 完整重绘，`unlocked` 是数据字段。
 * 锁定时后端直接清空列表，前端拿到空数组，没有「还原」这回事。
 *
 * # 为什么不用框架
 *
 * CSP 是 `script-src 'self'`，不允许 eval 与内联脚本；多数框架的
 * 模板编译依赖其中之一，要么得预编译（引入构建步骤），要么放宽 CSP。
 * 而这个界面的状态很少（文件列表 + 预览目标 + 视图模式），
 * 手写渲染反而更直白。规模再涨时可以引入 Vite + 预编译。
 */

import { invoke } from './tauri.js';
import * as i18n from './i18n.js';

/** 应用状态。 */
const state = {
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
};

/** 拼出某个文件的内容 URL。 */
function fileUrl(id) {
  return `${state.streamBase}/file/${encodeURIComponent(id)}`;
}

/** 拼出某个文件的缩略图 URL。 */
function thumbUrl(id) {
  return `${state.streamBase}/thumb/${encodeURIComponent(id)}`;
}

/** 类型对应的占位图标。 */
const ICONS = {
  video: '🎬',
  audio: '🎵',
  image: '🖼️',
  text: '📄',
  other: '📦',
};

/** 播放分级的图标（文档 §4.4）。 */
const TIER_ICONS = { p1: '⚡', p2: '🔄', p3: '🐌' };

/** 转义 HTML，防止文件名里的尖括号破坏结构。
 *
 * 文件名完全由用户控制，可能包含 `<img onerror=...>`。
 * 虽然 CSP 挡住了脚本执行，但仍会破坏布局——而且不该指望
 * 单一防线。所有插入 DOM 的动态文本都必须过这一层。
 */
function esc(s) {
  return String(s ?? '')
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#39;');
}

/** 取 DOM 节点。 */
function $(sel) {
  return document.querySelector(sel);
}

/* ---------------- 主题 ---------------- */

function initTheme() {
  const saved = localStorage.getItem('omy.theme');
  if (saved) {
    document.documentElement.dataset.theme = saved;
  } else if (window.matchMedia('(prefers-color-scheme: light)').matches) {
    // 跟随系统（文档 §4.1）
    document.documentElement.dataset.theme = 'light';
  }
}

function toggleTheme() {
  const now = document.documentElement.dataset.theme === 'light' ? 'dark' : 'light';
  document.documentElement.dataset.theme = now;
  localStorage.setItem('omy.theme', now);
}

/* ---------------- 渲染 ---------------- */

/** 过滤并排序后的文件。 */
function visibleFiles() {
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
}

/** 渲染整个界面。 */
function render() {
  if (!state.unlocked) {
    renderUnlock();
    return;
  }
  renderMain();
}

/** 解锁界面。 */
function renderUnlock() {
  const err = state.error
    ? `<div class="errbox" role="alert">${esc(state.error)}</div>`
    : '';
  const folder = state.folder
    ? `<div class="path" title="${esc(state.folder)}">${esc(state.folder)}</div>`
    : `<div class="path">${esc(i18n.t('unlock.pick_folder'))}</div>`;

  $('#app').innerHTML = `
    <div class="unlock-screen">
      <form class="unlock-box" id="unlock-form">
        <div class="lock-icon" aria-hidden="true">🔒</div>
        <h1>${esc(i18n.t('app.name'))}</h1>
        <div class="hint">${esc(i18n.t('unlock.pick_folder_hint'))}</div>
        ${err}
        <div class="folder-row">
          ${folder}
          <button type="button" class="btn" id="btn-browse">
            ${esc(i18n.t('unlock.browse'))}
          </button>
        </div>
        <div class="field">
          <label for="u-label">${esc(i18n.t('unlock.label'))}</label>
          <input id="u-label" type="text" value="main" autocomplete="off" />
        </div>
        <div class="field">
          <label for="u-pass">${esc(i18n.t('unlock.password'))}</label>
          <input id="u-pass" type="password" autocomplete="current-password" />
        </div>
        <button type="submit" class="btn primary" id="btn-unlock"
                ${state.vaults ? '' : 'disabled'}>
          ${esc(state.busy ? i18n.t('unlock.deriving') : i18n.t('unlock.submit'))}
        </button>
      </form>
    </div>`;

  $('#btn-browse').addEventListener('click', pickFolder);
  $('#unlock-form').addEventListener('submit', (e) => {
    e.preventDefault();
    doUnlock();
  });
  // 选完文件夹后焦点直接落到密码框，少一次点击
  if (state.vaults) $('#u-pass')?.focus();
}

/** 主界面。 */
function renderMain() {
  const files = visibleFiles();
  $('#app').innerHTML = `
    <div class="topbar">
      <span class="brand">${esc(i18n.t('app.name'))}</span>
      <input class="search" id="search" type="search"
             placeholder="${esc(i18n.t('view.search'))}"
             value="${esc(state.query)}"
             aria-label="${esc(i18n.t('view.search'))}" />
      <span class="spacer"></span>
      <span class="lockstate" data-on="1">🔓 ${esc(i18n.t('status.unlocked'))}</span>
      <button class="iconbtn" id="btn-grid" aria-pressed="${state.view === 'grid'}"
              title="${esc(i18n.t('view.grid'))}"
              aria-label="${esc(i18n.t('view.grid'))}">⊞</button>
      <button class="iconbtn" id="btn-list" aria-pressed="${state.view === 'list'}"
              title="${esc(i18n.t('view.list'))}"
              aria-label="${esc(i18n.t('view.list'))}">☰</button>
      <button class="iconbtn" id="btn-lang"
              title="${esc(i18n.t('lang.toggle'))}"
              aria-label="${esc(i18n.t('lang.toggle'))}">🌐</button>
      <button class="iconbtn" id="btn-theme"
              title="${esc(i18n.t('theme.toggle'))}"
              aria-label="${esc(i18n.t('theme.toggle'))}">◐</button>
      <button class="iconbtn" id="btn-lock"
              title="${esc(i18n.t('status.lock_now'))}"
              aria-label="${esc(i18n.t('status.lock_now'))}">🔒</button>
    </div>
    <div class="body">
      <aside class="sidebar">
        <div class="side-title">${esc(i18n.t('app.tagline'))}</div>
        ${state.roots
          .map(
            (r) =>
              `<button class="side-item" title="${esc(r)}">📂 ${esc(
                r.split(/[\\/]/).filter(Boolean).pop() || r,
              )}</button>`,
          )
          .join('')}
        <button class="side-item" id="btn-add-folder">➕ ${esc(
          i18n.t('unlock.browse'),
        )}</button>
      </aside>
      <div class="main">
        <div class="toolbar">
          <span>${esc(i18n.tn('status.files', files.length))}</span>
        </div>
        <div class="content" id="content">
          ${files.length ? renderFiles(files) : renderEmpty()}
        </div>
      </div>
    </div>
    <div class="statusbar">
      <span>${esc(i18n.tn('status.files', state.files.length))}</span>
      <span>${esc(
        i18n.t('status.total_size', { size: i18n.formatSize(totalSize()) }),
      )}</span>
      <span class="spacer"></span>
      <span>${esc(i18n.tn('status.credentials', state.credentials))}</span>
    </div>`;

  wireMain();
}

/** 已解锁文件的明文总大小。 */
function totalSize() {
  return state.files.reduce((sum, f) => sum + (f.size || 0), 0);
}

/** 空状态。 */
function renderEmpty() {
  const hasFiles = state.files.length > 0;
  return `
    <div class="empty">
      <div class="icon" aria-hidden="true">📂</div>
      <div class="title">${esc(
        hasFiles ? i18n.t('view.no_match') : i18n.t('view.empty_title'),
      )}</div>
      ${hasFiles ? '' : `<div>${esc(i18n.t('view.empty_hint'))}</div>`}
    </div>`;
}

/** 文件区域。 */
function renderFiles(files) {
  return state.view === 'grid' ? renderGrid(files) : renderList(files);
}

/** 网格视图。 */
function renderGrid(files) {
  const cards = files.map((f) => {
    // 锁定的文件：占位名 + 锁图案 + 「密码未解锁」。
    // 三者缺一不可——文档 §10 指出只隐藏文件名是不够的，
    // 缩略图和文件大小同样泄露信息。
    if (!f.unlocked) {
      return `
        <div class="card" data-locked="1" tabindex="0">
          <div class="thumb" aria-hidden="true">🔒</div>
          <div class="info">
            <div class="name">${esc(i18n.t('file.locked_name'))}</div>
            <div class="meta">${esc(i18n.t('file.locked_meta'))}</div>
          </div>
        </div>`;
    }

    const kind = f.kind || 'other';
    const thumb = f.has_thumbnail
      ? `<img src="${thumbUrl(f.id)}" alt="" loading="lazy" />`
      : `<span aria-hidden="true">${ICONS[kind] || ICONS.other}</span>`;

    const bits = [i18n.formatSize(f.size)];
    if (f.duration_ms) bits.push(i18n.formatDuration(f.duration_ms));
    const tier = f.tier && TIER_ICONS[f.tier]
      ? `<span class="tier" title="${esc(
          i18n.t(`playback.tier_${f.tier}_desc`),
        )}">${TIER_ICONS[f.tier]}</span>`
      : '';

    return `
      <div class="card" data-locked="0" data-id="${esc(f.id)}" tabindex="0"
           role="button" aria-label="${esc(f.name)}">
        <div class="thumb">${thumb}</div>
        <div class="info">
          <div class="name" title="${esc(f.name)}">${esc(f.name)}</div>
          <div class="meta">${esc(bits.join(' · '))}${tier}</div>
        </div>
      </div>`;
  });
  return `<div class="grid">${cards.join('')}</div>`;
}

/** 列表视图。 */
function renderList(files) {
  const rows = files.map((f) => {
    if (!f.unlocked) {
      return `<tr data-locked="1">
        <td>🔒 ${esc(i18n.t('file.locked_name'))}</td>
        <td>${esc(i18n.t('file.locked_meta'))}</td>
        <td></td></tr>`;
    }
    const kindLabel = i18n.t(`kind.${f.kind || 'other'}`);
    return `<tr data-id="${esc(f.id)}" tabindex="0">
      <td title="${esc(f.name)}">${esc(
        ICONS[f.kind || 'other'],
      )} ${esc(f.name)}</td>
      <td>${esc(i18n.formatSize(f.size))}</td>
      <td>${esc(kindLabel)}</td>
    </tr>`;
  });
  return `
    <table class="list">
      <thead><tr>
        <th>${esc(i18n.t('view.name'))}</th>
        <th>${esc(i18n.t('view.size'))}</th>
        <th>${esc(i18n.t('view.kind'))}</th>
      </tr></thead>
      <tbody>${rows.join('')}</tbody>
    </table>`;
}

/** 绑定主界面事件。 */
function wireMain() {
  $('#btn-grid').addEventListener('click', () => {
    state.view = 'grid';
    render();
  });
  $('#btn-list').addEventListener('click', () => {
    state.view = 'list';
    render();
  });
  $('#btn-theme').addEventListener('click', toggleTheme);
  $('#btn-lang').addEventListener('click', switchLanguage);
  $('#btn-lock').addEventListener('click', doLock);
  $('#btn-add-folder').addEventListener('click', pickFolder);

  const search = $('#search');
  search.addEventListener('input', (e) => {
    state.query = e.target.value;
    // 只重绘内容区，避免每敲一个字就重建整个界面导致输入框失焦
    $('#content').innerHTML = (() => {
      const f = visibleFiles();
      return f.length ? renderFiles(f) : renderEmpty();
    })();
    wireContent();
  });

  wireContent();
}

/** 绑定内容区的点击。 */
function wireContent() {
  for (const el of document.querySelectorAll('[data-id]')) {
    const open = () => openPreview(el.dataset.id);
    el.addEventListener('dblclick', open);
    el.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        open();
      }
    });
  }
}

/* ---------------- 动作 ---------------- */

/** 选择文件夹。 */
async function pickFolder() {
  // Tauri v2 的对话框插件需要额外依赖与权限配置。
  // 这里先用 prompt 让链路能跑通，接入 dialog 插件后替换。
  const dir = window.prompt(i18n.t('unlock.browse'), state.folder || '');
  if (!dir) return;
  state.error = '';
  try {
    // 后端返回的是一组 vault（一个文件夹里可能混着多个独立的库，
    // 每个库有自己的 salt），解锁时对每个都派生一次
    state.vaults = await invoke('vault_params_of', { dir });
    state.folder = dir;
    if (state.unlocked) {
      await doScan(dir);
    }
  } catch (e) {
    state.vaults = null;
    state.error = i18n.te(e?.code, i18n.t('errors.load_failed'));
  }
  render();
}

/** 执行解锁。 */
async function doUnlock() {
  if (state.busy || !state.vaults) return;
  const label = $('#u-label').value.trim() || 'main';
  const password = $('#u-pass').value;
  state.busy = true;
  state.error = '';
  render();

  try {
    const r = await invoke('unlock', { label, password, vaults: state.vaults });
    state.credentials = r.credentials;
    state.unlocked = true;
    state.busy = false;
    await doScan(state.folder);
  } catch (e) {
    state.busy = false;
    state.error = i18n.te(e?.code);
    render();
  }
}

/** 扫描目录。 */
async function doScan(dir) {
  try {
    state.files = await invoke('scan_directory', { dir, recursive: true });
    state.roots = await invoke('list_roots');
    render();
    // 元信息按需补：列表先出来，避免大目录卡住首屏（文档 §9）
    enrichVisible();
  } catch (e) {
    state.error = i18n.te(e?.code, i18n.t('errors.scan_failed'));
    render();
  }
}

/** 逐个补齐媒体元信息。 */
async function enrichVisible() {
  const targets = state.files.filter((f) => f.unlocked && !f.kind);
  for (const f of targets) {
    try {
      const updated = await invoke('enrich_file', { id: f.id });
      if (!updated) continue;
      const i = state.files.findIndex((x) => x.id === updated.id);
      if (i >= 0) state.files[i] = updated;
    } catch {
      // 单个文件补不上不影响其余——它仍会以通用图标显示
    }
  }
  if (state.unlocked) render();
}

/** 锁定。 */
async function doLock() {
  await invoke('lock');
  // 后端已清空列表，前端同步清空。
  // 不做「保留数据只切视图」——那样锁定就是假象
  state.unlocked = false;
  state.files = [];
  state.credentials = 0;
  state.query = '';
  closePreview();
  render();
}

/** 切换语言。 */
async function switchLanguage() {
  const next = i18n.lang() === 'zh-CN' ? 'en' : 'zh-CN';
  await i18n.load(next);
  await invoke('set_language', { lang: next });
  localStorage.setItem('omy.lang', next);
  render();
}

/* ---------------- 预览 ---------------- */

/** 打开预览。 */
function openPreview(id) {
  const f = state.files.find((x) => x.id === id);
  if (!f || !f.unlocked) return;

  const src = fileUrl(f.id);
  const kind = f.kind || 'other';
  let bodyHtml = '';

  if (kind === 'video') {
    // crossorigin="anonymous" 不是可选项：页面在 tauri.localhost，
    // 协议在 omystream.localhost，两者不同源。不声明的话浏览器
    // 不走 CORS 校验，直接把媒体标记为「跨源」，于是任何
    // canvas.drawImage(video) 之后的 getImageData 都会抛
    // SecurityError（画布被污染）。
    // 影响的不只是测试——应用内截图、生成缩略图都要读像素。
    bodyHtml = `<video id="pv" src="${src}" controls autoplay
                       crossorigin="anonymous"
                       playsinline preload="metadata"></video>`;
  } else if (kind === 'audio') {
    bodyHtml = `<audio id="pv" src="${src}" controls autoplay
                       crossorigin="anonymous"></audio>`;
  } else if (kind === 'image') {
    // SVG 也走 <img>：浏览器的受限模式会禁掉其中的脚本。
    // 绝不 innerHTML 插入 SVG——那等于执行未知代码
    bodyHtml = `<img id="pv" src="${src}" alt="${esc(f.name)}"
                     crossorigin="anonymous" />`;
  } else if (kind === 'text') {
    bodyHtml = `<pre id="pv">${esc(i18n.t('playback.loading'))}</pre>`;
  } else {
    bodyHtml = `<div class="overlay-msg">${esc(
      i18n.t('playback.cannot_preview'),
    )}</div>`;
  }

  const el = document.createElement('div');
  el.className = 'overlay';
  el.id = 'overlay';
  el.innerHTML = `
    <div class="overlay-bar">
      <span class="title">${esc(f.name)}</span>
      <span class="spacer"></span>
      <button class="iconbtn" id="pv-close"
              aria-label="${esc(i18n.t('actions.close'))}">✕</button>
    </div>
    <div class="overlay-body">${bodyHtml}</div>`;
  document.body.appendChild(el);

  el.querySelector('#pv-close').addEventListener('click', closePreview);
  document.addEventListener('keydown', onPreviewKey);

  if (kind === 'text') loadText(src);
  if (kind === 'video' || kind === 'audio') wireMediaErrors();
}

/** 文本内容取回后渲染。 */
async function loadText(src) {
  try {
    const r = await fetch(src);
    if (!r.ok) throw new Error(String(r.status));
    const text = await r.text();
    const pv = document.querySelector('#pv');
    // 用 textContent 而非 innerHTML：文本内容完全不可信
    if (pv) pv.textContent = text;
  } catch {
    const pv = document.querySelector('#pv');
    if (pv) pv.textContent = i18n.te('preview_failed');
  }
}

/** 把媒体错误码翻译成人话。 */
function wireMediaErrors() {
  const pv = document.querySelector('#pv');
  if (!pv) return;
  pv.addEventListener('error', () => {
    const code = pv.error?.code ?? 0;
    const box = document.querySelector('.overlay-body');
    if (box) {
      box.innerHTML = `<div class="overlay-msg">${esc(
        i18n.te(`media_error_${code}`, i18n.te('preview_failed')),
      )}</div>`;
    }
  });
}

/** Esc 关闭预览。 */
function onPreviewKey(e) {
  if (e.key === 'Escape') closePreview();
}

/** 关闭预览。 */
function closePreview() {
  const el = document.querySelector('#overlay');
  if (!el) return;
  // 先停掉媒体再移除节点：否则 WebView 可能继续持有
  // 已解密的缓冲区，也会继续向协议发请求
  const pv = el.querySelector('video, audio');
  if (pv) {
    pv.pause?.();
    pv.removeAttribute('src');
    pv.load?.();
  }
  el.remove();
  document.removeEventListener('keydown', onPreviewKey);
}

/* ---------------- 启动 ---------------- */

async function boot() {
  initTheme();
  // 协议前缀必须在任何内容加载前拿到——各平台形式不同，
  // 用错的话所有内容都是静默的 ERR_UNKNOWN_URL_SCHEME
  state.streamBase = await invoke('stream_base').catch(() => 'omystream://localhost');
  // 语言优先级：用户手动设置 > 系统语言（文档 §7.1）
  const saved = localStorage.getItem('omy.lang');
  const lang = saved || (await invoke('get_language').catch(() => 'en'));
  await i18n.load(lang);

  // 全局快捷键：Ctrl/Cmd+L 锁定（文档 §8）
  document.addEventListener('keydown', (e) => {
    if ((e.ctrlKey || e.metaKey) && e.key === 'l') {
      e.preventDefault();
      if (state.unlocked) doLock();
    }
  });

  render();
}

boot();
