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

import { computed, ref } from 'vue';
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
  canShowMessages,
  setRemoteViewMode,
} from '../store.js';

const emit = defineEmits(['open', 'add']);

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

/** 跳转到面包屑指定目录。 */
async function jump(c) {
  await enterRemoteDir(c.dir);
}

async function remove(p) {
  // 只删注册信息，不碰云端任何文件，所以不需要二次确认
  await removeRemotePlace(p.id);
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
  // 未解锁的 omy 打不开（本来就没有密码），其余都交给父组件预览：
  // 普通文件走原始字节转发，已解锁的 omy 走解密
  if (f.is_encrypted && !f.unlocked) return;
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
  }
}

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
  if (f.is_dir) return '📁';
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
  if (f.is_encrypted) return f.unlocked;
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
          <button class="crumbseg" @click="leaveToList">{{ currentPlace?.name }}</button>
          <template v-for="(c, i) in crumbs" :key="c.dir">
            <span class="sep">/</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1 }"
              @click="jump(c)"
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
        <button class="btn small" data-pb="add" @click="$emit('add')">
          ＋ {{ i18n.t('rplace.add_short') }}
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
              class="iconbtn"
              :aria-label="i18n.t('rplace.remove')"
              :title="i18n.t('rplace.remove')"
              @click.stop="remove(p)"
            >
              ✕
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
            @keydown.enter="state.searchMode === 'server' && runServerSearch()"
          />
          <!-- 文件 / 消息 切换。只在**对话内**且是 Telegram 时出现——
               位置列表那层没有「消息」这个概念，网盘更没有。
               与搜索那个分段控件同理：不支持就整个不出现，不是置灰。 -->
          <div v-if="canShowMessages" class="seg" data-tg="viewmode">
            <button
              :class="{ on: state.remoteViewMode === 'files' }"
              data-tg="vm-files"
              @click="setRemoteViewMode('files')"
            >
              {{ i18n.t('msgs.view_files') }}
            </button>
            <button
              :class="{ on: state.remoteViewMode === 'messages' }"
              data-tg="vm-messages"
              @click="setRemoteViewMode('messages')"
            >
              {{ i18n.t('msgs.view_messages') }}
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
        </div>

        <!-- 两种语义各自的提示条。服务端那条是警告色：
             搜索词已经离开本机，这件事必须显眼 -->
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
            <div
              v-for="m in state.remoteMessages"
              :key="m.message"
              class="msgrow"
              :class="{ out: m.outgoing, hasfile: !!m.file_id }"
              data-tg="msgrow"
            >
              <div class="msgmeta">
                <span class="msgid">#{{ m.message }}</span>
                <span class="msgdate">{{ fmtDate(m.date) }}</span>
                <span v-if="m.outgoing" class="msgout">{{ i18n.t('msgs.outgoing') }}</span>
              </div>
              <!-- 带文件的消息：点它就打开那个文件，走与文件视图完全相同的
                   那条路（同一个 id、同一套识别与预览），不另写一份 -->
              <button
                v-if="m.file_id"
                class="msgfile"
                :data-tg-file="m.file_id"
                @click="openFromMessage(m)"
              >
                <span aria-hidden="true">📎</span>
                <span class="mfname">{{ m.file_name }}</span>
                <span class="mfsize">{{ i18n.formatSize(m.file_size || 0) }}</span>
              </button>
              <!-- 纯文本消息是正常的一行，不是「缺了文件」的残缺条目 -->
              <div v-if="m.text" class="msgtext">{{ m.text }}</div>
              <div v-else-if="!m.file_id" class="msgtext dim">
                {{ i18n.t('msgs.no_file') }}
              </div>
            </div>
          </div>
        </template>

        <template v-else>
        <div v-if="state.searching" class="empty">
          <div class="icon" aria-hidden="true">⏳</div>
          <div class="title">{{ i18n.t('search.searching') }}</div>
        </div>
        <div v-else-if="state.busy" class="empty">
          <div class="icon" aria-hidden="true">⏳</div>
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
            {{ state.remoteItems.length ? i18n.t('view.no_match') : i18n.t('rplace.empty_dir') }}
          </div>
        </div>

        <div v-else-if="state.view === 'grid'" class="grid">
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
            </div>
            <div class="cname">{{ displayName(f) }}</div>
            <div class="cmeta">
              <template v-if="f.is_dir">📁</template>
              <template v-else-if="f.probing || isRetrying(f)">{{ i18n.t('rplace.probing') }}</template>
              <template v-else-if="f.probe_failed">
                <span class="retryhint">{{ i18n.t('rplace.probe_failed_hint') }}</span>
              </template>
              <template v-else>{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</template>
            </div>
          </div>
        </div>

        <div v-else class="list">
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
            <span class="tg">
              <template v-if="f.is_dir">{{ i18n.t('rplace.folder') }}</template>
              <template v-else-if="f.probing || isRetrying(f)">{{ i18n.t('rplace.probing') }}</template>
              <template v-else-if="f.probe_failed">{{ i18n.t('rplace.probe_failed') }}</template>
              <template v-else-if="!f.is_encrypted">{{ i18n.t('rplace.plain_file') }}</template>
              <template v-else-if="!f.unlocked">{{ i18n.t('kind.encrypted') }}</template>
            </span>
          </div>
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
/* 位置行：让操作按钮靠右排成一组 */
.placerow {
  cursor: pointer;
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
