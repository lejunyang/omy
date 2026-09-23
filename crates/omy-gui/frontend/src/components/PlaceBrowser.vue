<script setup>
/** 云盘视图：浏览远程位置（WebDAV / Telegram）里的文件并点播。
 *
 * # 与 RemoteScreen（局域网对端）的区别
 *
 * | | 局域网对端 RemoteScreen | 云盘 PlaceBrowser |
 * |---|---|---|
 * | 目录层级 | 没有，一份平铺清单 | 有，能逐层进出 |
 * | 定位 | 对端给的 handle | 位置 id + 完整路径 |
 * | 入口 | 设备面板「连接」 | 底部「远程」tab / 侧栏入口 |
 *
 * 所以它比 RemoteScreen 多一层「位置列表」：没进入某个位置时先列出
 * 已保存的位置，点进去才是目录浏览。
 *
 * # 能力位图投影
 *
 * 写类操作（上传等）按**能力位图**渲染，不给某个 provider 写特判。
 * 能力由后端按**目标目录**的真实情况给出：同一个 Telegram 位置里，
 * 根目录（对话列表）不可写、只读频道不可写、自己的群可写——所以判据必须是
 * 当前目录的 effective caps；用位置级的会在只读对话上放出一个点了必然
 * 失败的按钮。
 *
 * 不支持的能力**整项不出现而非置灰**：灰按钮会让人去找怎么启用它。
 */

import { computed, ref, watch, nextTick } from 'vue';
import * as i18n from '../i18n.js';
import { isMobile } from '../viewport.js';
import AppShell from './AppShell.vue';
import ContextMenu from './ContextMenu.vue';
import {
  state,
  currentCaps,
  openRemotePlace,
  leaveRemotePlace,
  reloadRemoteDir,
  enterRemoteDir,
  remoteGoUp,
  removeRemotePlace,
  detachTelegramPlace,
  deleteTelegramAccount,
  renameTelegramPlace,
  retryRemoteEntry,
  decryptRemoteToLocal,
  requestRemoteFileCache,
  uploadToRemote,
  pinRemoteFile,
  unpinRemoteFile,
  remoteFileCache,
  removeRemoteFileCache,
  placeThumbUrl,
  canServerSearch,
  setSearchMode,
  runServerSearch,
  onSearchQueryCleared,
  setRemoteTab,
  MEDIA_TABS,
  showMediaTabs,
  loadMoreMessages,
  loadMoreFiles,
  locateMessage,
} from '../store.js';

// 声明要写全：未声明的事件在生产构建里会静默落到 attrs 上，
// 碰巧也能冒泡，但看声明就不知道这个组件会发什么，
// 而且一旦事件名与原生事件撞上就会出问题
const emit = defineEmits([
  'open', 'close', 'pick', 'devices', 'lang',
  'telegram', 'settings', 'lock', 'quick-unlock', 'need-unlock',]);

/** 当前位置元信息。 */
const currentPlace = computed(() =>
  state.remotePlaces.find((p) => p.id === state.remotePlace),
);

/** 面包屑分段：每段带可跳转的绝对目录。 */
const crumbs = computed(() => {
  const out = [];
  // 有显示名就只画一段，用名字。
  //
  // 按 '/' 切段是**写死了 WebDAV 的路径形状**：Telegram 的 dir 是
  // `tg:<对话>`，里面没有斜杠，整串会被当成一段原样显示，用户看到
  // 「tg:-1003929965717」这种东西，完全不知道自己在哪个对话里。
  // 而那个名字进目录时本来就在手上。
  if (state.remoteDirName) {
    out.push({ name: state.remoteDirName, dir: state.remoteDir });
    return out;
  }
  let acc = '';
  for (const seg of state.remoteDir.split('/').filter(Boolean)) {
    acc += `/${seg}`;
    out.push({ name: seg, dir: acc });
  }
  return out;
});

const visible = computed(() => {
  // 服务端搜索模式下列的是候选集，不是当前目录的内容
  const source =
    state.searchMode === 'server' ? state.searchResults : state.remoteItems;
  const q = state.query.trim().toLowerCase();
  if (!q) return source;
  // 锁定项没有可搜的明文名，只用真实名/目录名匹配，避免「搜什么都有」。
  //
  // 服务端结果也要过这一遍：服务端搜的是消息文字，omy 加密文件的真实文件名
  // 它永远没有。少了本地这一段，搜索对加密文件就完全失效（原型 §5 的
  // 「两段式搜索」说的就是这件事）。
  return source.filter((f) => {
    const name = f.is_dir ? f.name : f.unlocked ? f.real_name || f.name : '';
    return (name || '').toLowerCase().includes(q);
  });
});

/** 空态文案：区分「搜索无匹配」「这一栏没内容」「整个对话空」。
 *
 * 在分栏下（Telegram 对话内），空表示这一栏没有该类型的内容，说「这一栏
 * 没有内容」——而不是笼统的「目录是空的」，那会让用户以为整个对话都没东西。 */
const emptyText = computed(() => {
  if (state.remoteItems.length) return i18n.t('view.no_match');
  if (showMediaTabs.value) return i18n.t('rplace.tab_empty');
  return i18n.t('rplace.empty_dir');
});

/** 被本地精筛挡掉的加密文件数。
 *
 * 要如实告诉用户「还有 N 个因为没解锁而没参与搜索」——不说的话他会以为
 * 那些文件不存在，而其实只差一个密码。
 */
const lockedOut = computed(() => {
  if (!state.query.trim()) return 0;
  const source =
    state.searchMode === 'server' ? state.searchResults : state.remoteItems;
  return source.filter((f) => !f.is_dir && f.is_encrypted && !f.unlocked).length;
});

/** 状态栏的 🔓已解锁 / 🔒未解锁计数：只数加密文件，明文与目录不计。
 *  跟随搜索过滤后的可见集合，和「N 个文件」口径一致。probing 中的条目
 *  is_encrypted 尚未确定（false），自然不计入，识别完会响应式更新。 */
const unlockedCount = computed(
  () => visible.value.filter((f) => !f.is_dir && f.is_encrypted && f.unlocked).length,
);
const lockedCount = computed(
  () => visible.value.filter((f) => !f.is_dir && f.is_encrypted && !f.unlocked).length,
);

/** 进入某个已保存位置。 */
async function open(p) {
  await openRemotePlace(p.id);
}

/** 从位置面包屑退回位置列表（仍停留在云盘视图）。 */
function leaveToList() {
  leaveRemotePlace();
}

/** 顶部返回：在目录里逐级上级，在位置列表则由父组件切回本地。 */
async function onBack() {
  if (state.remotePlace) await remoteGoUp();
  else emit('close');
}

async function reload() {
  if (state.remotePlace) await reloadRemoteDir();
}

/** 点位置名：回到**该位置的根**（对话列表），而不是退到位置选择那层。
 *
 * 用户报过：点地址栏 Telegram 名字本以为回对话列表，却跳到了位置选择。
 * 回根 = 进空目录；要离开这个位置到列表另有顶部返回。 */
async function jumpToPlaceRoot() {
  await enterRemoteDir('');
}

/** 跳转到面包屑指定目录。
 *
 * 点**当前段**（最后一段，就是眼前这个目录/对话）时不导航、只刷新：
 * 导航到「自己」在 Telegram 对话上会走进畸形分支（dir 变成 tg:-数字 再报
 * 远程操作失败）。点当前对话名的语义本就是「刷新当前」。 */
async function jump(c, isCurrent) {
  if (isCurrent) {
    await reloadRemoteDir();
    return;
  }
  await enterRemoteDir(c.dir);
}

/** 消息按日期分组，组内由新到旧。
 *
 * 每行都带完整日期是冗余噪声，而没有分组时长列表里又失去时间锚点。
 * 所以日期提到组标题上，组内只显示时刻。
 *
 * 分组键用**本地日期**而不是 UTC：用户看到的「今天」得是他自己的今天。 */
const messageGroups = computed(() => {
  const out = [];
  let cur = null;
  for (const m of state.remoteMessages) {
    const key = dayKey(m.date);
    if (!cur || cur.key !== key) {
      cur = { key, label: dayLabel(m.date), rows: [] };
      out.push(cur);
    }
    cur.rows.push(m);
  }
  return out;
});

/** 本地日期的分组键（同一天的消息归一组）。 */
function dayKey(unixSecs) {
  const d = new Date((unixSecs || 0) * 1000);
  return `${d.getFullYear()}-${d.getMonth() + 1}-${d.getDate()}`;
}

/** 组标题：当天与昨天给相对说法，更早给日期。
 *
 * 相对说法只用在这两天：再往前用「3 天前」还要用户自己换算，
 * 不如直接给日期。 */
function dayLabel(unixSecs) {
  const d = new Date((unixSecs || 0) * 1000);
  const today = new Date();
  const same = (a, b) => dayKey(a.getTime() / 1000) === dayKey(b.getTime() / 1000);
  if (same(d, today)) return i18n.t('msgs.today');
  const y = new Date(today.getTime() - 86400000);
  if (same(d, y)) return i18n.t('msgs.yesterday');
  return d.toLocaleDateString();
}

/** 组内每行只显示时刻——日期已经在组标题上了。 */
function fmtTime(unixSecs) {
  return new Date((unixSecs || 0) * 1000)
    .toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

/** 本屏消息里带文件的条数。
 *
 * 把它显示出来，是为了让「筛选判据有没有退化成文件视图」这件事
 * **从界面上就能看出来**（§7.8）：消息数应当大于带文件数，
 * 两者相等就说明纯文本消息被漏掉了。此前这个事实只存在于测试输出里。 */
/** 缩略图字节转 data URL。
 *
 * 后端给的是 Vec<u8>，序列化成 JS 数组。几 KB 的小图，内联开销可以忽略。 */
function thumbUrl(bytes) {
  if (!bytes || !bytes.length) return '';
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return `data:image/jpeg;base64,${btoa(bin)}`;
}

/** 秒数转 m:ss。 */
function fmtDur(secs) {
  const s = Math.max(0, Math.round(secs));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

const messageWithFile = computed(
  () => state.remoteMessages.filter((m) => !!m.file_id).length,
);

/** 当前打开的是不是 Telegram 位置。
 *
 * 用来决定「目录」该显示成什么：Telegram 的根目录列出的是对话，
 * 不是文件夹。与 hasLocalSession 同一个判据来源（位置的 kind）。
 */
const isTelegram = computed(
  () => state.remotePlaces.find((p) => p.id === state.remotePlace)?.kind === 'telegram',
);

/** 这个位置有没有「本机登录态」这回事。
 *
 * 只有 Telegram 才分得出「摘掉位置」与「删掉登录态」；WebDAV 的凭据跟着
 * 位置配置走，摘掉就没了，给它一个「删除账号」纯属多余且会让人误解。 */
function hasLocalSession(p) {
  return p.kind === 'telegram';
}

async function remove(p) {
  // 原本这里不做确认（只删注册信息、不碰云端文件）。多账号之后它与
  // 「删除账号」并排出现，点错的代价不对称，所以也确认一次，并顺便说清
  // 登录态和永久缓存都还在——否则用户以为摘掉位置就干净了
  if (hasLocalSession(p)) {
    if (!window.confirm(i18n.t('rplace.detach_confirm', { name: p.name }))) return;
    await detachTelegramPlace(p.id);
    return;
  }
  await removeRemotePlace(p.id);
}

/** 删除账号：连本机登录态一起清掉，下次要重新扫码或导入 tdata。 */
async function deleteAccount(p) {
  if (!window.confirm(i18n.t('rplace.delete_account_confirm', { name: p.name }))) return;
  await deleteTelegramAccount(p.id);
}

/** 改本机显示名。两个账号昵称相同时，这是唯一能分辨它们的办法。 */
async function rename(p) {
  const next = window.prompt(i18n.t('rplace.rename_prompt', { name: p.name }), p.name);
  if (next === null) return;
  await renameTelegramPlace(p.id, next);
}

/** 移动端单击：目录进入、可播放文件打开。桌面靠双击。 */
function onEntryClick(f) {
  if (!isMobile.value) return;
  // 长按刚弹了菜单，浏览器补来的 click 要吃掉，否则会同时触发打开
  if (longFired) {
    longFired = false;
    return;
  }
  activate(f);
}

function onEntryDbl(f) {
  activate(f);
}

/** 打开一个条目的统一入口。
 *
 * 目录下钻；普通文件与已解锁的加密文件都交给父组件预览（前者走原始字节
 * 转发、后者走解密）；只有**未解锁的加密文件**打不开，因为确实没有密码。
 * 「未能读取」条目点击则就地重试。 */
function activate(f) {
  if (f.probing || isRetrying(f)) return;
  // 「未能读取（网络）」整卡/整行可点：只重试这一条，不重载整个目录，
  // 也绝不能把它当成「密码不对」——那是 probe_failed 与锁定的根本区别。
  if (f.probe_failed) {
    retryRemoteEntry(f);
    return;
  }
  if (f.is_dir) {
    enterRemoteDir(f.id, displayName(f));
    return;
  }
  // 锁着的 omy：弹解锁框，而不是什么都不做。
  //
  // 原先这里直接 return，注释说「本来就没有密码」——那是首期只做浏览
  // 点播时的前提，现在远程位置已经有完整的解锁通道了。静默 return 的
  // 表现是**双击一个带锁标记的文件毫无反应，连错误都没有**，用户只能
  // 猜是不是坏了；而本地视图在同样情形下是弹框问密码的，两边不一致。
  if (f.is_encrypted && !f.unlocked) {
    emit('need-unlock');
    return;
  }
  // 其余都交给父组件预览：普通文件走原始字节转发，已解锁的 omy 走解密
  emit('open', f);
}

/** 该条目是否正处于单条目重试中（此时显示 ⏳ 且不可再点）。 */
function isRetrying(f) {
  return state.remoteRetrying.includes(f.id);
}

/* ---------------- 条目右键 / 长按菜单 ---------------- */

/** 当前打开的条目菜单：`{ entry, x, y }`，null 表示关闭。 */
const rmenu = ref(null);

/** 只有「能对它做点什么」的正常条目才给菜单：骨架、未能读取、重试中
 *  都直接走整卡点击（重试），不进菜单。 */
function menuable(f) {
  return !f.probing && !f.probe_failed && !isRetrying(f) && activatable(f);
}

function openMenuAt(f, x, y) {
  if (!menuable(f)) return;
  rmenu.value = { entry: f, x, y };
  // 打开菜单时按需查该文件的密文缓存覆盖情况，查到有缓存块才让
  // 「从缓存中移除」出现；不 await，菜单先弹，结果回来后响应式补项。
  requestRemoteFileCache(f);
}

/** 桌面右键。 */
/** 所有条目的缓存标识：id -> {kind,cls,icon,title}。
 *
 * 用 computed map 而不是「模板里调 cacheMark(f) 函数」：
 * 函数版在卡片 render 时读 `state.remoteCacheStat[key]`，而首次求值时
 * 那个 key 还不存在（读到 undefined），这次 render 的依赖收集**不一定**
 * 精确地把这张卡片的 effect 绑到那个 key 上。于是 pin 之后（即便替换
 * 整个 remoteCacheStat 对象）那张卡片也不重渲染——实测 pin 后菜单变了、
 * 卡片标识不变，就是这个。
 *
 * computed 在求值时整体读了 state.remoteCacheStat 与 visible，两者任一
 * 变化都必然让它重算，卡片再从这个 map 取值，依赖稳定不再靠碰运气。
 *
 * 只画两种值得画的：**永久保留**（不会被淘汰、离线一定还在）与
 * **已全部缓存**（在本地、但随时可能被 LRU 淘汰）。目录、半缓存、
 * 未缓存都不画——满屏小圆点会淹没那两个有意义的状态。 */
const cacheMarks = computed(() => {
  const m = {};
  // 顶部碰一下整个对象，让这个 computed 依赖它：pin/unpin 替换整个
  // remoteCacheStat 时必然重算。具体取值仍走 remoteFileCache（与右键
  // 菜单同一份数据）
  void state.remoteCacheStat;
  for (const f of visible.value) {
    if (f.is_dir) continue;
    const st = remoteFileCache(f);
    if (!st) continue;
    if (st.pinned) {
      // 永久保留：蓝色 📌，与「已缓存」刻意用不同标识、不合并——一个是
      // 「离线也在」，一个是「空间不足可能被清」，用户据此判断断网能不能打开
      m[f.id] = { kind: 'pinned', cls: 'pinned', icon: '📌',
                  title: i18n.t('rplace.mark_pinned') };
    } else if (st.fully_cached && st.total_blocks > 0) {
      // 整个文件都在本地：绿点
      m[f.id] = { kind: 'cached', cls: 'cached', icon: '●',
                  title: i18n.t('rplace.mark_cached') };
    } else if (st.cached_blocks > 0 && st.total_blocks > 0) {
      // 只缓存了一部分（大视频 seek 时下的那几块）：半实心点。
      // 与「整个都在」区分，是 pin 之前判断「断网还能不能完整打开」的依据。
      m[f.id] = { kind: 'partial', cls: 'partial', icon: '◐',
                  title: i18n.t('rplace.mark_partial', {
                    n: st.cached_blocks, total: st.total_blocks,
                  }) };
    }
  }
  return m;
});

/** 卡片右上角标识，从 cacheMarks 取值。 */
function cacheMark(f) {
  return cacheMarks.value[f.id] || null;
}

function onEntryContext(f, ev) {
  ev.preventDefault();
  ev.stopPropagation();
  openMenuAt(f, ev.clientX, ev.clientY);
}

/* 移动端长按：pointerdown 计时，移动超过容差取消，到点弹菜单。
   桌面右键走 contextmenu，这里只在触屏/主键按下时计时。 */
const LONGPRESS_MS = 450;
const PRESS_TOLERANCE = 10;
let pressTimer = null;
let pressX = 0;
let pressY = 0;
let longFired = false;

function clearPress() {
  if (pressTimer) {
    clearTimeout(pressTimer);
    pressTimer = null;
  }
}

function onPointerDown(f, ev) {
  if (ev.pointerType === 'mouse' && ev.button !== 0) return;
  longFired = false;
  pressX = ev.clientX;
  pressY = ev.clientY;
  clearPress();
  pressTimer = setTimeout(() => {
    longFired = true;
    openMenuAt(f, pressX, pressY);
  }, LONGPRESS_MS);
}

function onPointerMove(ev) {
  if (!pressTimer) return;
  if (
    Math.abs(ev.clientX - pressX) > PRESS_TOLERANCE ||
    Math.abs(ev.clientY - pressY) > PRESS_TOLERANCE
  ) {
    clearPress();
  }
}

function onPointerUp() {
  clearPress();
}

/** 菜单项按条目状态投影。
 *
 * 「打开/预览」人人有；「解密到本地」仅已解锁的加密单文件；缓存三项
 * （转永久 / 取消永久 / 从缓存移除）对普通文件同样适用——远程位置里
 * 普通文件是主体内容，离线看视频正是永久缓存最典型的场景。
 *
 * 缓存类操作**只读位置也允许**：它们只清本机、不碰云端。
 * 不支持的能力整项不出现而非置灰，不放假控件。 */
const rmenuItems = computed(() => {
  const f = rmenu.value?.entry;
  if (!f) return [];
  const items = [];
  items.push({
    key: 'open',
    icon: f.is_dir ? '📁' : '👁️',
    label: f.is_dir ? i18n.t('rplace.menu_open') : i18n.t('rplace.menu_preview'),
  });
  if (!f.is_dir && f.is_encrypted && f.unlocked) {
    items.push({
      key: 'decrypt-local',
      icon: '📥',
      label: i18n.t('rplace.menu_decrypt_local'),
    });
  }
  // 「定位源消息」：从文件/媒体条目跳回消息时间线并高亮那条。
  // 只在能解析出消息号（id 形如 tg:chat:msg 三段）且当前不在消息栏时给。
  if (!f.is_dir && msgIdOf(f) != null && state.remoteTab !== 'messages') {
    items.push({
      key: 'locate-source',
      icon: '💬',
      label: i18n.t('rplace.menu_locate_source'),
    });
  }
  // 缓存相关。状态是打开菜单时异步查的，查到之前不出现（避免空操作）。
  //
  // 「转为永久」在产品上是**一次真实的下载任务**（要先把整个文件预热到
  // 本地），不是一个点完就静默生效的开关——不预热的话永久层里只有碰巧
  // 缓存过的那几块，而用户以为整个文件都留下了，直到离线打开失败才发现。
  const cstat = !f.is_dir ? remoteFileCache(f) : null;
  if (cstat) {
    if (cstat.pinned) {
      items.push({
        key: 'unpin',
        icon: '📌',
        label: i18n.t('rplace.menu_unpin'),
      });
    } else {
      items.push({
        key: 'pin',
        icon: '📌',
        label: i18n.t('rplace.menu_pin'),
      });
    }
    if (cstat.cached_blocks > 0) {
      items.push({
        key: 'remove-cache',
        icon: '🗄️',
        label: i18n.t('rplace.menu_remove_cache'),
      });
    }
  }
  return items;
});

async function onMenuPick(key) {
  const f = rmenu.value?.entry;
  rmenu.value = null;
  if (!f) return;
  if (key === 'open') {
    activate(f);
  } else if (key === 'decrypt-local') {
    await decryptRemoteToLocal(f);
  } else if (key === 'remove-cache') {
    await removeRemoteFileCache(f);
  } else if (key === 'pin') {
    await pinRemoteFile(f);
  } else if (key === 'unpin') {
    await unpinRemoteFile(f);
  } else if (key === 'locate-source') {
    const mid = msgIdOf(f);
    if (mid != null) await locateMessage(mid);
  }
}

/** 从一个 Telegram 文件/媒体条目里解析出它所在消息的消息号。
 *
 * id 形如 `tg:<chat>:<msg>`（三段）；对话列表的 `tg:<chat>`（两段）没有消息号，
 * 返回 null。按结构（段数）判，不猜——与后端 encode() 的形状对应。 */
function msgIdOf(f) {
  if (!f || typeof f.id !== 'string') return null;
  const parts = f.id.split(':');
  if (parts.length !== 3 || parts[0] !== 'tg') return null;
  const n = Number(parts[2]);
  return Number.isInteger(n) ? n : null;
}

/** 高亮某条消息时滚动到它并做一次脉冲高亮，然后清掉高亮标记。
 *
 * 滚动放在视图层：这里有 DOM。用 [data-msgid] 结构定位（不按文字），nextTick
 * 等消息行渲染完再滚。脉冲高亮靠 .msgrow.hl 的 CSS 动画，动画跑完把 highlightMsg
 * 清掉，避免同一条永远挂着高亮态。 */
watch(
  () => state.highlightMsg,
  async (mid) => {
    if (mid == null) return;
    await nextTick();
    const el = document.querySelector(`.msgrow[data-msgid="${mid}"]`);
    if (el) el.scrollIntoView({ block: 'center', behavior: 'smooth' });
    // 高亮保留一段时间做脉冲，再清掉（CSS 动画 ~1.6s）
    window.setTimeout(() => {
      if (state.highlightMsg === mid) state.highlightMsg = null;
    }, 1800);
  },
);

/** 当前目录可写吗（决定上传按钮出不出现）。
 *
 * 用**当前目录**的能力而不是位置级能力：同一个 Telegram 位置里，
 * 根目录（对话列表）不可写、只读频道不可写、自己的群可写。
 * 用位置级的会在只读对话上放出一个点了必然失败的按钮。
 */
const canWrite = computed(() => !!currentCaps.value?.write && !!state.remoteDir);

const uploading = ref(false);

/** 选本地文件上传到当前对话。 */
async function doUpload() {
  if (uploading.value) return;
  uploading.value = true;
  try {
    await uploadToRemote();
  } finally {
    uploading.value = false;
  }
}

/** 列表/网格里显示的名字。 */
function displayName(f) {
  if (f.is_dir) return f.name;
  if (f.is_encrypted && f.unlocked) return f.real_name || f.name;
  return f.name;
}

/** 按真实文件名后缀给一个粗图标；识别不了就用通用文件图标。 */
function icon(f) {
  if (f.is_dir) {
    // Telegram 位置的根目录列出的**全是对话**，不是文件夹。
    // 用 📁 会让人以为里面是目录结构，而它其实是一个群/频道/私聊。
    //
    // 判据用**行自身的 is_conversation 标记**，不是全局 state.remoteDir。
    // 为什么：双击进入某个群时 enterRemoteDir 会先把 state.remoteDir 设成该群
    // 的 id、**然后**才 await reloadRemoteDir 换掉列表。这两步之间屏上还渲染着
    // 旧的对话列表行，此刻 !state.remoteDir 已变 false —— 那些还没拿到头像
    // thumb_token 的对话就会从 💬 闪成 📁（用户报的「刷新中双击进入、其他群
    // 图标闪成文件夹」）。行级标记在建列表时就定死，不随导航状态漂移。
    if (f.is_conversation) return '💬';
    return '📁';
  }
  if (f.probing || isRetrying(f)) return '⏳';
  if (f.probe_failed) return '⚠️';
  if (f.is_encrypted && !f.unlocked) return '🔒';
  // 普通文件按**磁盘名**判类型，已解锁的 omy 按解密出的真实名。
  //
  // 原先普通文件在这里直接 return '📄'，于是 jpg 和 mp4 长得一模一样——
  // 用户报的「视频图片文件都是一个图标」就是这条。
  const n = (f.is_encrypted ? f.real_name || '' : f.name || '').toLowerCase();
  if (/\.(mp4|mkv|webm|mov|avi|m4v|flv|wmv|ts)$/.test(n)) return '🎬';
  if (/\.(mp3|flac|aac|m4a|ogg|opus|wav|wma)$/.test(n)) return '🎵';
  if (/\.(png|jpe?g|gif|webp|bmp|avif|svg|heic|heif)$/.test(n)) return '🖼️';
  if (/\.(txt|md|log|json|srt|vtt)$/.test(n)) return '📝';
  return '📄';
}

/** 条目能否被激活（决定可点击观感）。
 *  失败条目可点（=重试）；重试中转 ⏳ 暂时不可点；锁定项/普通文件不可点。 */
function activatable(f) {
  if (f.probing || isRetrying(f)) return false;
  // 普通文件也可打开。
  //
  // 原先这里只让目录与已解锁的 omy 文件可点，那是首期给网盘定的前提
  // （网盘里普通文件不是 omy 的事）。但远程位置的主体内容恰恰是普通文件
  // ——Telegram 频道里的图片、视频、文档——这个前提在那里是反的。
  //
  // 连带影响右键菜单：menuable() 要求 activatable()，所以之前
  // 每一个条目的菜单都被提前拦掉，代码里有 @contextmenu 却什么都不做。
  if (f.is_dir || f.probe_failed) return true;
  // 锁着的 omy 也算可激活：点它会弹解锁框。
  // 若仍返回 false，卡片会带上 off 样式且右键菜单被 menuable() 拦掉，
  // 用户连「这东西能交互」都看不出来
  return true;
}

// 加载失败的缩略图 token：回退到类型图标，不留一块破图。
// 用 token 而非文件 id 做键——刷新目录后 token 会换，失败状态不该带到新句柄。
const brokenThumbs = ref(new Set());
function onThumbError(token) {
  if (!token || brokenThumbs.value.has(token)) return;
  const next = new Set(brokenThumbs.value);
  next.add(token);
  brokenThumbs.value = next;
}

/** 位置类型的显示名。
 *
 * 认不出的 kind **原样显示它自己**，不要回落到某个具体 provider 的名字。
 * 这里原本的 else 分支硬写 'WebDAV'——在只有一个 provider 时看不出问题，
 * 加了 Telegram 之后就变成了谎话：一个叫 Telegram 的位置标着 WebDAV，
 * 用户会以为自己加错了东西。回落本身就是这个缺陷的根源。
 */
/** 消息时间戳 → 本地可读时间。 */
function fmtDate(sec) {
  try {
    return new Date(sec * 1000).toLocaleString();
  } catch {
    return String(sec);
  }
}

/** 从消息行打开那个文件。
 *
 * 复用 onOpen 那条路：同一个 id、同一套识别与预览。另写一份的话，
 * 文件视图修了预览、消息视图还是老样子。
 */
async function openFromMessage(m) {
  if (!m.file_id) return;
  // 走与文件视图**完全相同**的那条路（onEntryDbl）：同一个 id、同一套识别
  // 与预览。另写一份的话，文件视图修了预览、消息视图还是老样子。
  //
  // 消息里带的是后端给的 file_id，与文件视图里那个是同一个值——这正是
  // 「条目标识保留 (对话, 消息号)」那条设计的回报。
  const inList = state.remoteItems.find((f) => f.id === m.file_id);
  // 列表里有就用列表里那份：它已经过识别，带着 unlocked / real_name /
  // 缩略图。没有（比如消息比当前这页文件更早）才现造一个最小条目
  await onEntryDbl(
    inList || {
      id: m.file_id,
      name: m.file_name || '',
      size: m.file_size || 0,
      is_dir: false,
      is_encrypted: false,
      unlocked: false,
    },
  );
}

function kindLabel(p) {
  if (p.kind === 'nextcloud') return i18n.t('rplace.vendor_nextcloud');
  if (p.kind === 'telegram') return i18n.t('rplace.vendor_telegram');
  if (p.kind === 'webdav') return 'WebDAV';
  return p.kind || '';
}

/** 状态栏那枚能力徽标。
 *
 * 读的是**有效**能力（按当前目录），不是位置级上界：同一位置里有的目录可写、
 * 有的只读，按上界显示会告诉用户「可写」而他在这个目录里其实写不了。
 *
 * 能力还没查到时显示「只读」而不是「可写」：这一刻的真实情况是「不确定」，
 * 而两种猜法的代价不对称——猜只读只是少几个按钮，猜可写会让用户点下去才失败。
 */
function capsLabel() {
  const c = currentCaps.value;
  return c.write ? i18n.t('rplace.caps_writable') : i18n.t('rplace.readonly_badge');
}

/** 悬停标题：失败条目提示「点击重试」，重试中转「识别中」，其余不给标题。 */
function rowTitle(f) {
  if (f.probing || isRetrying(f)) return i18n.t('rplace.probing');
  if (f.probe_failed) return i18n.t('rplace.probe_failed_hint');
  return '';
}
</script>

<template>
  <!-- 活在主界面外壳里，侧栏常驻。
       原先这是一个整屏组件、与 MainScreen 互斥替换，进远程位置后侧栏
       整个消失，用户没法在位置之间切换，只能先退回去。 -->
  <AppShell
    :search="false"
    @pick="$emit('pick')"
    @devices="$emit('devices')"
    @lang="$emit('lang')"
    @add-place="$emit('add')"
    @telegram="$emit('telegram')"
    @settings="$emit('settings')"
    @lock="$emit('lock')"
    @quick-unlock="$emit('quick-unlock')"
    @files="$emit('close')"
    @places="leaveToList"
  >
  <div class="main" data-ui="place-browser">
    <div class="crumb">
      <button
        class="crumbbtn"
        :title="i18n.t('common.back')"
        :aria-label="i18n.t('common.back')"
        @click="onBack"
      >
        ←
      </button>
      <button
        v-if="state.remotePlace"
        class="crumbbtn"
        :title="i18n.t('nav.reload')"
        :aria-label="i18n.t('nav.reload')"
        @click="reload"
      >
        ⟳
      </button>

      <nav class="crumbpath">
        <span class="remotetag" aria-hidden="true">☁️</span>
        <template v-if="!state.remotePlace">
          <button class="crumbseg cur">{{ i18n.t('rplace.title') }}</button>
        </template>
        <template v-else>
          <button class="crumbseg" @click="jumpToPlaceRoot">{{ currentPlace?.name }}</button>
          <template v-for="(c, i) in crumbs" :key="c.dir">
            <span class="sep">/</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1 }"
              @click="jump(c, i === crumbs.length - 1)"
            >
              {{ c.name }}
            </button>
          </template>
        </template>
      </nav>

      <div class="vtoggle">
        <!-- 上传按钮按**能力位图**出现，不给 Telegram 写特判。
             同一个位置里根目录（对话列表）与只读频道都不可写，
             所以判据必须是当前目录的 effective caps 而非位置级 caps。
             不支持就整个不出现而非置灰——灰按钮会让人去找怎么启用。 -->
        <button
          v-if="canWrite"
          class="btn small"
          data-pb="upload"
          :disabled="uploading"
          @click="doUpload"
        >
          {{ uploading ? i18n.t('rplace.uploading') : '⬆️ ' + i18n.t('rplace.upload') }}
        </button>
      </div>
    </div>

    <div class="content">
      <!-- 一、位置列表 -->
      <div v-if="!state.remotePlace" class="list">
        <div v-if="!state.remotePlaces.length" class="empty">
          <div class="icon" aria-hidden="true">☁️</div>
          <div class="title">{{ i18n.t('rplace.empty') }}</div>
          <div class="sub">{{ i18n.t('rplace.empty_hint') }}</div>
          <button class="btn primary" style="margin-top: 12px" @click="$emit('add')">
            {{ i18n.t('rplace.add') }}
          </button>
        </div>

        <div
          v-for="p in state.remotePlaces"
          :key="p.id"
          class="lrow placerow"
          tabindex="0"
          @dblclick="open(p)"
          @keydown.enter.prevent="open(p)"
        >
          <span class="ic" aria-hidden="true">☁️</span>
          <span class="nm">{{ p.name }}</span>
          <span class="tg">{{ kindLabel(p) }}</span>
          <span class="rowactions">
            <button class="btn small" :data-pb-enter="p.id" @click.stop="open(p)">
              {{ i18n.t('rplace.enter') }}
            </button>
            <button
              v-if="hasLocalSession(p)"
              class="iconbtn"
              :data-pb-rename="p.id"
              :aria-label="i18n.t('rplace.rename')"
              :title="i18n.t('rplace.rename')"
              @click.stop="rename(p)"
            >
              ✎
            </button>
            <button
              class="iconbtn"
              :data-pb-detach="p.id"
              :aria-label="hasLocalSession(p) ? i18n.t('rplace.detach') : i18n.t('rplace.remove')"
              :title="hasLocalSession(p) ? i18n.t('rplace.detach') : i18n.t('rplace.remove')"
              @click.stop="remove(p)"
            >
              ✕
            </button>
            <button
              v-if="hasLocalSession(p)"
              class="iconbtn danger"
              :data-pb-delacct="p.id"
              :aria-label="i18n.t('rplace.delete_account')"
              :title="i18n.t('rplace.delete_account')"
              @click.stop="deleteAccount(p)"
            >
              🗑️
            </button>
          </span>
        </div>
      </div>

      <!-- 二、某个位置的目录浏览 -->
      <template v-else>
        <!-- 搜索栏。之前这一屏根本没有输入框（输入框在 MainScreen 顶栏，
             而这是一个独立整屏），于是本地过滤形同虚设——用户没法输入。 -->
        <div class="searchbar" data-pb="searchbar">
          <span aria-hidden="true">🔍</span>
          <input
            v-model="state.query"
            class="sinput"
            type="search"
            data-tg="searchinput"
            :placeholder="i18n.t('view.search')"
            :aria-label="i18n.t('view.search')"
            @input="onSearchQueryCleared($event)"
            @keydown.enter="state.searchMode === 'server' && runServerSearch()"
          />
          <!-- 分栏：媒体/文件/链接/音频/GIF/消息。前五栏用服务端类型 filter，
               「消息」是第六栏、走 getHistory 时间线（原来单独的「文件/消息」
               切换已并入这一排）。只在 Telegram 对话内出现。 -->
          <div v-if="showMediaTabs" class="seg mtabs" data-tg="mediatabs">
            <button
              v-for="t in MEDIA_TABS"
              :key="t.key"
              :class="{ on: state.remoteTab === t.key }"
              :data-tg-tab="t.key"
              @click="setRemoteTab(t.key)"
            >
              {{ i18n.t(t.i18n) }}
            </button>
          </div>

          <!-- 只在位置支持服务端搜索时出现，不支持时整个不出现而非置灰：
               网盘根本没有「搜索整个对话」这个概念 -->
          <div v-if="canServerSearch" class="seg" data-tg="searchmode">
            <button
              :class="{ on: state.searchMode === 'local' }"
              data-tg="mode-local"
              :title="i18n.t('search.local_hint')"
              @click="setSearchMode('local')"
            >
              {{ i18n.t('search.local') }}
            </button>
            <button
              :class="{ on: state.searchMode === 'server' }"
              data-tg="mode-server"
              :title="i18n.t('search.server_hint')"
              @click="setSearchMode('server')"
            >
              {{ i18n.t('search.server') }}
            </button>
          </div>
          <!-- 本账号内的后台刷新指示：蓝色转圈图标，放搜索栏右端，不占文字流、
               不推挤布局。只在**已有内容**时显示（空列表/首次用下面占满区域的
               大圆圈，不是这个）。 -->
          <span
            v-if="state.busy && visible.length"
            class="refspin"
            data-pb="refreshing"
            :title="i18n.t('rplace.refreshing')"
            aria-hidden="true"
          ></span>
        </div>

        <!-- 两种语义各自的提示条。服务端那条是警告色：
             搜索词已经离开本机，这件事必须显眼 -->
        <!-- 受保护内容：低权重一行，不是警告。
             实测 noforwards 拦的是转发、不拦取字节（DEC-20），
             所以读取与播放都正常，用 .d 这一档而不是 .warnbox——
             做成警告样式会让用户以为有什么坏了。 -->
        <div v-if="state.remoteProtected" class="d protnote" data-pb="protected">
          🔒 {{ i18n.t('rplace.protected_note') }}
        </div>
        <div
          v-if="state.query.trim() && canServerSearch && state.searchMode === 'local'"
          class="banner info"
          data-tg="banner-local"
        >
          <span aria-hidden="true">🔎</span>
          <div class="bx">{{ i18n.t('search.local_banner') }}</div>
        </div>
        <div
          v-if="state.searchMode === 'server' && state.searchedQuery"
          class="banner warn"
          data-tg="banner-server"
        >
          <span aria-hidden="true">☁️</span>
          <div class="bx">
            {{ i18n.t('search.server_banner', { q: state.searchedQuery }) }}
          </div>
        </div>
        <div v-if="lockedOut > 0" class="banner info" data-tg="banner-locked">
          <span aria-hidden="true">🔒</span>
          <div class="bx">{{ i18n.tn('search.locked_out', lockedOut) }}</div>
        </div>

        <!-- 消息时间线。与文件网格并列的一种渲染，不是另一个页面：
             对话是同一个、面包屑同一条、返回行为也一样。 -->
        <template v-if="state.remoteViewMode === 'messages'">
          <div v-if="state.loadingMessages" class="empty">
            <div class="icon" aria-hidden="true">⏳</div>
            <div class="title">{{ i18n.t('msgs.loading') }}</div>
          </div>
          <div v-else-if="state.placeError" class="empty" data-tg="msg-error">
            <div class="icon" aria-hidden="true">⚠️</div>
            <div class="title">{{ state.placeError }}</div>
          </div>
          <div v-else-if="!state.remoteMessages.length" class="empty">
            <div class="icon" aria-hidden="true">📭</div>
            <div class="title">{{ i18n.t('msgs.empty') }}</div>
          </div>
          <div v-else class="msglist" data-tg="msglist">
            <!-- 本屏统计。消息数与带文件数的差值就是纯文本消息数——
                 把它显示出来，用户才能看出筛选判据没有退化成文件视图 -->
            <div class="msgstat" data-tg="msgstat">
              {{ i18n.t('msgs.stat', {
                total: state.remoteMessages.length,
                files: messageWithFile,
                texts: state.remoteMessages.length - messageWithFile,
              }) }}
            </div>
            <template v-for="g in messageGroups" :key="g.key">
              <div class="msggroup" data-tg="msggroup">{{ g.label }}</div>
              <div
                v-for="m in g.rows"
                :key="m.message"
                class="msgrow"
                :class="{ out: m.outgoing, hasfile: !!m.file_id, hl: m.message === state.highlightMsg }"
                data-tg="msgrow"
                :data-msgid="m.message"
              >
              <div class="msgmeta">
                <span class="msgid">#{{ m.message }}</span>
                <span class="msgdate">{{ fmtTime(m.date) }}</span>
                <span v-if="m.outgoing" class="msgout">{{ i18n.t('msgs.outgoing') }}</span>
                <!-- 回复引用：点它跳到被引用的原消息并高亮（reply_to 是对话内消息号）。
                     做成可点的小条而不是纯文字，让「这是能跳的」显而易见。 -->
                <button
                  v-if="m.reply_to"
                  type="button"
                  class="msgreply"
                  data-tg="msgreply"
                  @click="locateMessage(m.reply_to)"
                >
                  {{ i18n.t('msgs.reply_to', { id: m.reply_to }) }}
                </button>
              </div>
              <!-- 带文件的消息：点它就打开那个文件，走与文件视图完全相同的
                   那条路（同一个 id、同一套识别与预览），不另写一份 -->
              <button
                v-if="m.file_id"
                class="msgfile"
                :data-tg-file="m.file_id"
                @click="openFromMessage(m)"
              >
                <!-- 有缩略图就用图，没有回落到回形针。
                     缩略图是服务端已有的小图（几 KB），直接内联；
                     不为它再建一套 token 通道——那是文件视图那条路径的做法，
                     在这里只会多一处要维护的东西 -->
                <span v-if="!m.thumb" aria-hidden="true">📎</span>
                <span v-else class="mfthumb" data-tg="msgthumb">
                  <img :src="thumbUrl(m.thumb)" alt="" loading="lazy" />
                  <!-- 时长角标压在缩略图右下角，与各家客户端一致 -->
                  <i v-if="m.duration" class="mfdur" data-tg="msgdur">
                    {{ fmtDur(m.duration) }}
                  </i>
                </span>
                <span class="mfname">{{ m.file_name }}</span>
                <span class="mfsize">{{ i18n.formatSize(m.file_size || 0) }}</span>
              </button>
              <!-- 纯文本消息是正常的一行，不是「缺了文件」的残缺条目 -->
              <div v-if="m.text" class="msgtext">{{ m.text }}</div>
              <div v-else-if="!m.file_id" class="msgtext dim">
                {{ i18n.t('msgs.no_file') }}
              </div>
              </div>
            </template>

            <!-- 加载更早。桌面给显式按钮，移动端靠滚动触底——
                 小屏上常驻按钮会一直吃掉可视高度 -->
            <div v-if="state.hasMoreMessages" class="msgmore">
              <button
                v-if="!isMobile"
                class="btn small"
                data-tg="msgmore"
                :disabled="state.loadingMore"
                @click="loadMoreMessages"
              >
                {{ i18n.t(state.loadingMore ? 'msgs.loading_more' : 'msgs.load_more') }}
              </button>
              <span v-else class="d" data-tg="msgmore-auto">
                {{ i18n.t(state.loadingMore ? 'msgs.loading_more' : 'msgs.load_more') }}
              </span>
            </div>
            <div v-else-if="state.remoteMessages.length" class="msgmore d"
                 data-tg="msgnomore">
              {{ i18n.t('msgs.no_more') }}
            </div>
          </div>

        </template>

        <template v-else>
        <div v-if="state.searching" class="empty">
          <div class="icon" aria-hidden="true">⏳</div>
          <div class="title">{{ i18n.t('search.searching') }}</div>
        </div>
        <!-- 整屏「加载中」只在**手里一点内容都没有**时出现。
             加上 !visible.length 这个条件才让「再进目录先摆上次的结果」
             真正起作用：否则列表虽然已经在 state 里，却被这个占满整屏的
             ⏳ 盖着，直到 browse 返回才第一次画出来——表现和完全没有缓存
             一模一样。
             有旧内容时改用下面那条不挡视线的细提示：用户能一边看已有内容、
             一边知道在刷新。 -->
        <div v-else-if="state.busy && !visible.length" class="empty">
          <div class="bigspin" aria-hidden="true"></div>
          <div class="title">{{ i18n.t(state.busyKey || 'busy.loading') }}</div>
        </div>

        <div v-else-if="state.placeError" class="empty">
          <div class="icon" aria-hidden="true">⚠️</div>
          <div class="title">{{ state.placeError }}</div>
          <button class="btn" style="margin-top: 12px" @click="reload">
            {{ i18n.t('rplace.retry') }}
          </button>
        </div>

        <div v-else-if="!visible.length" class="empty">
          <div class="icon" aria-hidden="true">📭</div>
          <div class="title">
            {{ emptyText }}
          </div>
        </div>

        <div v-if="state.view === 'grid'" class="grid">
          <div
            v-for="f in visible"
            :key="f.id"
            class="card"
            :class="{ locked: f.is_encrypted && !f.unlocked, 'is-unlocked': f.is_encrypted && f.unlocked, off: !activatable(f), probing: !f.is_dir && f.probing }"
            :title="rowTitle(f)"
            tabindex="0"
            @dblclick="onEntryDbl(f)"
            @click="onEntryClick(f)"
            @keydown.enter.prevent="onEntryDbl(f)"
            @contextmenu.prevent="onEntryContext(f, $event)"
            @pointerdown="onPointerDown(f, $event)"
            @pointermove="onPointerMove($event)"
            @pointerup="onPointerUp"
            @pointercancel="onPointerUp"
          >
            <div class="thumb">
              <img
                v-if="state.showThumbnails && f.thumb_token && !brokenThumbs.has(f.thumb_token)"
                :src="placeThumbUrl(f.thumb_token)"
                alt=""
                loading="lazy"
                @error="onThumbError(f.thumb_token)"
              />
              <span v-else aria-hidden="true">{{ icon(f) }}</span>
              <!-- 缓存标识。只画「永久」与「已缓存」两种：
                   半缓存画出来是噪音（用户对「缓存了 3/17 块」无法做
                   任何决定），而满屏小圆点会淹没那两个有意义的状态。
                   两者必须区分——都显示成一样的话，用户无从判断哪些
                   内容离线时真的还在。 -->
              <span
                v-if="cacheMark(f)"
                class="cmark"
                :class="cacheMark(f).cls"
                :data-pb-cache="cacheMark(f).kind"
                :title="cacheMark(f).title"
                aria-hidden="true"
              >{{ cacheMark(f).icon }}</span>
            </div>
            <div class="cname">{{ displayName(f) }}</div>
            <div class="cmeta">
              <!-- 目录这里留空：缩略图区已经有图标了，再放一个 📁 等于把同
                   一件事说两遍（实测卡片文本是「📁omytest📁」）。而且这一行
                   对文件显示的是大小，对目录塞图标会让两种条目的信息层次错位 -->
              <template v-if="f.is_dir"></template>
              <template v-else-if="f.probing || isRetrying(f)">{{ i18n.t('rplace.probing') }}</template>
              <template v-else-if="f.probe_failed">
                <span class="retryhint">{{ i18n.t('rplace.probe_failed_hint') }}</span>
              </template>
              <template v-else>{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</template>
            </div>
          </div>
        </div>

        <div v-else-if="state.view !== 'grid'" class="list">
          <div
            v-for="f in visible"
            :key="f.id"
            class="lrow"
            :class="{ locked: f.is_encrypted && !f.unlocked, 'is-unlocked': f.is_encrypted && f.unlocked, off: !activatable(f), probing: !f.is_dir && f.probing }"
            :title="rowTitle(f)"
            tabindex="0"
            @dblclick="onEntryDbl(f)"
            @click="onEntryClick(f)"
            @keydown.enter.prevent="onEntryDbl(f)"
            @contextmenu.prevent="onEntryContext(f, $event)"
            @pointerdown="onPointerDown(f, $event)"
            @pointermove="onPointerMove($event)"
            @pointerup="onPointerUp"
            @pointercancel="onPointerUp"
          >
            <span class="ic">{{ icon(f) }}</span>
            <span class="nm">{{ displayName(f) }}</span>
            <span class="sz">
              <template v-if="!f.is_dir">{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</template>
            </span>
            <!-- 上传/发送时间：列表视图是看详情的地方，媒体条目带消息日期。
                 网格视图为保持简洁不显示，这里补上。目录没有时间 -->
            <span class="tm">
              <template v-if="!f.is_dir && f.mtime">{{ fmtDate(f.mtime) }}</template>
            </span>
            <span class="tg">
              <template v-if="f.is_dir">{{ i18n.t('rplace.folder') }}</template>
              <template v-else-if="f.probing || isRetrying(f)">{{ i18n.t('rplace.probing') }}</template>
              <template v-else-if="f.probe_failed">{{ i18n.t('rplace.probe_failed') }}</template>
              <template v-else-if="!f.is_encrypted">{{ i18n.t('rplace.plain_file') }}</template>
              <template v-else-if="!f.unlocked">{{ i18n.t('kind.encrypted') }}</template>
            </span>
          </div>
        </div>

        <!-- 加载更多文件。只在对话内层出现——根目录列的是对话、不分页。
             没有更多时不画按钮而是明说，免得放一个点了没反应的入口 -->
        <div v-if="state.remoteDir && visible.length" class="msgmore">
          <button
            v-if="state.hasMoreFiles"
            class="btn small"
            data-pb="loadmore"
            :disabled="state.loadingMoreFiles"
            @click="loadMoreFiles"
          >
            {{ i18n.t(state.loadingMoreFiles
                ? 'msgs.loading_more' : 'rplace.load_more_files') }}
          </button>
          <span v-else class="d" data-pb="nomorefiles">
            {{ i18n.t('rplace.no_more_files') }}
          </span>
        </div>
        </template>
      </template>
    </div>

    <div class="statusbar">
      <span v-if="state.remotePlace">
        ☁️ {{ currentPlace?.name }}
      </span>
      <span v-else>☁️ {{ i18n.t('rplace.title') }}</span>
      <span v-if="state.remotePlace">{{ i18n.tn('status.files', visible.length) }}</span>
      <span v-if="state.remotePlace && unlockedCount > 0" data-stat="unlocked" :data-n="unlockedCount">🔓 {{ unlockedCount }}</span>
      <span v-if="state.remotePlace && lockedCount > 0" data-stat="locked" :data-n="lockedCount">🔒 {{ lockedCount }}</span>
      <span class="spacer"></span>
      <span v-if="state.remotePlace" class="readonly">{{ capsLabel() }}</span>
    </div>

    <ContextMenu
      v-if="rmenu"
      :items="rmenuItems"
      :x="rmenu.x"
      :y="rmenu.y"
      @pick="onMenuPick"
      @close="rmenu = null"
    />
  </div>
  </AppShell>
</template>

<style scoped>
/* 消息缩略图：小方图 + 右下角时长角标，与各家客户端一致 */
.mfthumb {
  position: relative;
  display: inline-flex;
  width: 36px;
  height: 36px;
  border-radius: var(--r-s);
  overflow: hidden;
  flex: 0 0 auto;
}
.mfthumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.mfdur {
  position: absolute;
  right: 2px;
  bottom: 1px;
  padding: 0 3px;
  border-radius: 3px;
  /* 固定深色底 + 白字：它压在缩略图上，而缩略图的明暗不可预测，
     跟着主题走反而会在浅色图上看不清 */
  background: rgb(0 0 0 / 62%);
  color: #fff;
  font-size: 9px;
  font-style: normal;
  line-height: 1.5;
}
/* 后台刷新指示：搜索栏右端的蓝色转圈小图标，不占文字流、不推挤布局 */
.refspin {
  flex: none;
  width: 16px;
  height: 16px;
  border: 2px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: pb-spin 0.7s linear infinite;
}
@keyframes pb-spin {
  to {
    transform: rotate(360deg);
  }
}
/* 首次/空列表的大号居中加载圈，替换原来那个 ⏳ emoji（emoji 不转、也不够
   「加载中」的分量）。用在占满内容区的 .empty 里 */
.bigspin {
  width: 38px;
  height: 38px;
  border: 3px solid var(--border);
  border-top-color: var(--accent);
  border-radius: 50%;
  animation: pb-spin 0.8s linear infinite;
  margin: 0 auto 10px;
}
/* 媒体分栏：复用全局 .seg 的观感（app.css 里已定义底色/选中/触控下限），
   这里只处理「五个栏在窄屏可能放不下」——允许横向滚动而不是撑破工具条。
   不另造一套按钮样式，免得和 .seg 在深浅主题下走形。 */
.mtabs {
  max-width: 100%;
  overflow-x: auto;
  /* 不显示滚动条占位，滑动即可；桌面一般放得下也就不出现 */
  scrollbar-width: none;
}
.mtabs::-webkit-scrollbar {
  display: none;
}
.mtabs button {
  white-space: nowrap;
}
.cmark {
  position: absolute;
  inset-block-start: 4px;
  inset-inline-end: 4px;
  font-size: 11px;
  line-height: 1;
  padding: 2px 4px;
  border-radius: var(--r-s);
  background: var(--bg);
  border: 1px solid var(--border);
}
.cmark.cached {
  color: var(--ok);
}
/* 永久保留：蓝色，与「已缓存」的绿区分开 */
.cmark.pinned {
  color: var(--accent);
}
/* 部分缓存：中性灰的半实心点，比绿点弱——它不是「完整可离线」的状态，
   不该和「已缓存」一样醒目，只是提示「下过一部分」 */
.cmark.partial {
  color: var(--fg2);
}
.msgmore {
  display: flex;
  justify-content: center;
  padding: calc(var(--sp) * 2) 0;
}

/* 受保护内容告知：刻意做得低权重。
   它说的是「这个群开了保护，但不影响你在这里用」，不是出错也不是风险，
   所以没有底色和边框——给它警告样式会让用户以为有什么坏了。 */
.protnote {
  display: block;
  padding: calc(var(--sp) * 1.5) 0;
  color: var(--fg2);
  font-size: 12px;
}

/* 受保护内容告知：刻意做得低权重。
   它说的是「这个群开了保护，但不影响你在这里用」，不是出错也不是风险，
   所以没有底色和边框——给它警告样式会让用户以为有什么坏了。 */
.protnote {
  display: block;
  padding: calc(var(--sp) * 1.5) 0;
  color: var(--fg2);
  font-size: 12px;
}

/* 受保护内容告知：刻意做得低权重。
   它说的是「这个群开了保护，但不影响你在这里用」，不是出错也不是风险，
   所以没有底色和边框——给它警告样式会让用户以为有什么坏了。 */
.protnote {
  display: block;
  padding: calc(var(--sp) * 1.5) 0;
  color: var(--fg2);
  font-size: 12px;
}

/* 受保护内容告知：刻意做得低权重。
   它说的是「这个群开了保护，但不影响你在这里用」，不是出错也不是风险，
   所以没有底色和边框——给它警告样式会让用户以为有什么坏了。 */
.protnote {
  display: block;
  padding: calc(var(--sp) * 1.5) 0;
  color: var(--fg2);
  font-size: 12px;
}

/* ---------- 密度：与原型（appendix/telegram-remote-prototype.html）对齐 ----------
 *
 * 只在这里覆盖，不动全局 app.css：`.grid` / `.card` / `.thumb` 本地文件
 * 视图也在用，而原型那套尺寸是给远程场景定的。改全局会把本地一起改掉。
 *
 * 数值逐项对照原型：网格 minmax(122px)/gap 10px、缩略图 72px/26px、
 * 卡片 padding 9px、名字 12px、元信息 10.5px。
 */
.grid {
  grid-template-columns: repeat(auto-fill, minmax(122px, 1fr));
  gap: 10px;
}
.card {
  padding: 9px;
}
.card .thumb {
  /* 原来 104px，卡片整体 156px 高——一个文件夹图标就占掉半屏 */
  height: 72px;
  font-size: 26px;
  margin-bottom: 7px;
}
.cname {
  font-size: 12px;
  margin-block-start: 0;
}
.cmeta {
  font-size: 10.5px;
}

/* ---------- 搜索栏 ----------
 *
 * 原先 `.searchbar` / `.sinput` **完全没有样式**：输入框是浏览器原生外观、
 * 整行铺开，与周围组件不是一套视觉。
 */
.searchbar {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 2px 10px;
}
.sinput {
  /* 不铺满整行：搜索框占满一行会让它看起来像页面主体，
     而它只是一个过滤器。给一个上限，剩下的留白 */
  flex: 1;
  max-width: 320px;
  min-width: 140px;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  color: var(--fg);
  font-family: inherit;
  font-size: 12.5px;
  padding: 6px 10px;
  min-height: 32px;
}
.sinput:focus {
  outline: none;
  border-color: var(--accent);
}

/* ---------- 说明条 ----------
 *
 * 原型里这类说明是**低权重的一行**（`.note`：左侧一条 3px 竖线 + 灰字），
 * 不是通栏彩底。通栏蓝底长文字会把一句辅助说明做成页面最抢眼的东西，
 * 而它的重要性低于下面的文件列表。
 *
 * 唯一例外是服务端搜索那条：搜索词已经离开本机，那件事该显眼，
 * 所以 `.warn` 保留彩底。
 */
.banner {
  padding: 7px 11px;
  font-size: 12px;
  line-height: 1.45;
  border-block-end: 0;
  border-inline-start: 3px solid var(--border);
  border-radius: 0 var(--r-s) var(--r-s) 0;
  margin: 0 2px 8px;
  background: var(--bg2);
  color: var(--fg2);
}
.banner.info {
  border-inline-start-color: var(--accent);
  background: var(--bg2);
  color: var(--fg2);
}
/* 服务端搜索保持醒目：搜索词离开本机这件事不能低调处理 */
.banner.warn {
  border-inline-start-color: var(--warn);
  background: color-mix(in srgb, var(--warn) 10%, var(--bg2));
  color: var(--fg);
}

/* 位置行：让操作按钮靠右排成一组 */
.placerow {
  cursor: pointer;
}
/* 删除账号不可撤销，要和旁边的「移除」在视觉上区分开——
   两个相邻的灰图标，点错的代价却差很远 */
.iconbtn.danger:hover {
  background: #fee2e2;
  border-color: #fca5a5;
}
/* 组标题粘顶：长列表滚动时始终知道正在看哪一天 */
.msggroup {
  position: sticky;
  top: 0;
  z-index: 1;
  padding: 6px 10px;
  background: var(--bg2);
  border-bottom: 1px solid var(--border);
  font-size: 11px;
  font-weight: 600;
  color: var(--fg2);
}
.msgstat {
  padding: 6px 10px;
  font-size: 11.5px;
  color: var(--fg2);
}
.rowactions {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 6px;
}
.rowactions .btn {
  padding: 3px 10px;
  font-size: 12px;
  min-height: 30px;
}

/* 窄屏抬到触控下限。同一行里的图标钮已经是 44，只有「进入」是 30，
   既按不准也显得高度不齐。这条要写在这里而不是全局 app.css：
   `.rowactions .btn` 特异性更高，全局那条压不住它。 */
@media (max-width: 768px) {
  .rowactions .btn {
    min-height: 44px;
  }
  /* 工具条窄屏分层：五分栏 + 文件/消息 + 搜索模式 + 转圈一行放不下，
     必然换行错乱或溢出（用户点名「分栏多」担心的就是这个）。
     所以窄屏让 .searchbar 换行，各控件分层，且五分栏独占一行横向可滚——
     参照官方 Telegram 移动端媒体页那条可左右滑的 tab 条。 */
  .searchbar {
    flex-wrap: wrap;
    row-gap: 8px;
  }
  /* 输入框填满第一行，把 320px 上限去掉；仍 flex:1，给行末转圈留出位置 */
  .searchbar .sinput {
    max-width: none;
    flex: 1 1 140px;
  }
  /* 后台刷新转圈：移动端工具条会换行，若让它参与 flex 流，它会被挤到某一行
     末尾、还会随换行跳位。改为**绝对定位到工具条右上角**——不占流、不推挤
     tab、不随换行跳动、始终在第一行右端可见。搜索输入右侧留出内边距避免压字。
     对比过三种放法：①跟输入框行末（换行时跳到第二行、且把该行内容挤窄）；
     ②单独占一行（凭空多一行、且一个小圈占满宽显得空）；③绝对定位右上角
     （不占流、稳定）——选③。 */
  .searchbar {
    position: relative;
  }
  .searchbar .refspin {
    position: absolute;
    inset-block-start: 14px;
    /* 离输入框右缘留一点呼吸间距，不要贴边 */
    inset-inline-end: 12px;
    width: 18px;
    height: 18px;
  }
  /* 给输入框右侧留出转圈的位置，避免刷新时圈压在占位文字上 */
  .searchbar .sinput {
    padding-inline-end: 38px;
  }
  /* 三组分段控件各自独占整行：五分栏那行横向可滚（.mtabs 已有 overflow-x），
     文件/消息 与 搜索模式 两个按钮拉宽，触控目标更大。 */
  .searchbar .seg {
    flex-basis: 100%;
  }
  /* 文件/消息、搜索模式：两个按钮平分整行 */
  .searchbar [data-tg="viewmode"] button,
  .searchbar [data-tg="searchmode"] button {
    flex: 1;
  }
  /* 五分栏不平分（否则 5 个挤成一团、字被截）：保持按内容宽度 + 可横向滚动，
     一屏放不下就左右滑，每个 tab 完整可读可点。触控下限由全局 .seg button
     的 44px 保证。 */
  .searchbar .mtabs {
    display: flex;
    /* 让滚动容器自身占满整行，内部按钮溢出即可滑动 */
    -webkit-overflow-scrolling: touch;
  }
  /* 五个 tab 在放得下时平分整行、铺满不留白（否则 seg 框满宽、按钮却按
     内容宽，右侧会空出一块）；每个给 min-width 兜底，若某语言的字更长、
     五个加起来超过一行，就触发 .mtabs 的横向滚动而不是把字截断。 */
  .searchbar .mtabs button {
    flex: 1 0 auto;
    min-width: 56px;
  }
}
/* 不可激活的条目（锁定项、非加密文件、识别失败）降低对比并去掉指点光标 */
.lrow.off,
.card.off {
  opacity: 0.72;
}
.card.off {
  cursor: default;
}
/* 「未能读取」卡片上的点击重试提示：用警告色与普通大小区分，提示可点 */
.retryhint {
  color: #9a6a2f;
  font-size: 12px;
}
/* 边扫边出的「识别中」骨架：不沿用 .off 的灰化（它不是失败/不可用），
   只用缩略图区的轻微脉冲表达进行中；条目不可点（activatable=false） */
.card.probing,
.lrow.probing {
  opacity: 1;
  cursor: default;
}
.card.probing .thumb {
  animation: omy-probe-pulse 1.1s ease-in-out infinite;
}
@keyframes omy-probe-pulse {
  0%, 100% { opacity: 0.4; }
  50% { opacity: 1; }
}
@media (prefers-reduced-motion: reduce) {
  .card.probing .thumb { animation: none; }
}
/* 长按弹菜单：禁掉系统的文字选择/放大镜/图片保存气泡，
   否则触屏长按会先弹系统菜单而不是我们的条目菜单 */
.card,
.lrow {
  -webkit-touch-callout: none;
  -webkit-user-select: none;
  user-select: none;
}
</style>
