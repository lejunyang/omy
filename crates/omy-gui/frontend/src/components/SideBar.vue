<script setup>
/** 侧栏：位置 + 附近设备。 */

import { ref, computed } from 'vue';
import * as i18n from '../i18n.js';
import * as api from '../api.js';
import {
  leaveOverlays,
  state,
  navigate,
  grantStorageAccess,
  openPlaceBrowserAt,
  renameTelegramPlace,
  detachTelegramPlace,
  deleteTelegramAccount,
  encryptTelegramPlace,
  decryptTelegramPlace,
  removeRemotePlace,
  openTransfers,
  activeLocation,
  createVirtualPlace,
} from '../store.js';
import ContextMenu from './ContextMenu.vue';

/** 这一项是不是当前位置。
 *
 * 判据只有一个来源 `activeLocation`，本地与远程都问它。
 * 原来两处各自判断（本地比 cwd、远程比 remotePlace），而 cwd 进远程后
 * 不会清空，于是两个条件同时成立、侧栏同时高亮本地和远程两项。
 * 走同一个派生值之后「不会同时亮两个」是结构上的保证。 */
function isActive(kind, key) {
  return activeLocation.value.kind === kind && activeLocation.value.key === key;
}

defineProps({
  /** 移动端（抽屉形态）。抽屉里点完一项要自动收起，
   *  否则它盖住半个屏幕，用户看不到刚打开的目录。 */
  mobile: { type: Boolean, default: false },
});

const emit = defineEmits(['pick', 'devices', 'navigate', 'lang', 'add-place', 'telegram']);

/** 正在等用户在系统设置页里操作。 */
const granting = ref(false);

/** 申请存储权限。
 *
 * 安卓上会跳到系统设置页，用户回来才 resolve，中间可能几十秒——
 * 必须置忙，否则用户会以为没点上而反复点，跳出去一堆设置页。
 */
async function onGrant() {
  granting.value = true;
  try {
    await grantStorageAccess();
  } finally {
    granting.value = false;
  }
}

/** 进入某个本地位置，并通知父组件（抽屉据此收起）。 */
function go(path) {
  // 退出所有整屏覆盖的视图（云盘浏览、传输管理）。不退的话本地目录
  // 已经载入、界面却还停在那一屏，用户看到的是「点了本地盘符没反应」，
  // 而实际上导航已经发生了，只是被盖住。
  //
  // 用 leaveOverlays 而不是逐个点名：原来这里只关云盘视图，后来加了
  // 传输管理页没跟着改，于是从传输页点本地位置界面不动
  leaveOverlays();
  navigate(path);
  emit('navigate');
}

/** 右键菜单：`{ place, x, y }`，没有就是不显示。 */
const rmenu = ref(null);

/** 未结束的任务数，用于角标。
 *
 * 只数未结束的——完成的也算进去的话，数字只增不减，用户会以为有一堆
 * 任务卡着。0 时整个角标不渲染，而不是显示一个「0」。 */
const activeTransfers = computed(
  () => (state.transfers || []).filter(
    (t) => t.state === 'running' || t.state === 'waiting',
  ).length,
);

/** 这个位置有没有「本机登录态」这回事。
 *
 * 只有 Telegram 分得出「摘掉位置」与「删掉登录态」；WebDAV 的凭据跟着
 * 位置配置走，摘掉就没了，给它一个「删除账号」纯属多余且会让人误解。
 * 与 PlaceBrowser 里同名判断保持一致。 */
function hasLocalSession(p) {
  return p.kind === 'telegram';
}

/** 右键菜单的条目。
 *
 * 两种删除**分开列出并各自写明后果**，不合并成一个含糊的「删除」：
 * 一个保留登录态、可以直接加回来，另一个要重新扫码，代价差得远。
 * note 显示在标签下方，让用户在点之前就看到区别。 */
const rmenuItems = computed(() => {
  const p = rmenu.value?.place;
  if (!p) return [];
  const tg = hasLocalSession(p);
  const items = [{ key: 'open', label: i18n.t('rplace.menu_open'), icon: '📂' }];
  if (tg) {
    items.push({ key: 'rename', label: i18n.t('rplace.rename'), icon: '✏️' });
    // 加密是可选功能：已加密显示「取消加密」，未加密显示「加密此位置」。
    // rmenu.encrypted 在打开菜单时异步查得（见 onPlaceContext）。
    if (rmenu.value?.encrypted) {
      items.push({ key: 'decrypt', label: i18n.t('rplace.decrypt'), icon: '🔓' });
    } else {
      items.push({ key: 'encrypt', label: i18n.t('rplace.encrypt'), icon: '🔒', note: i18n.t('rplace.encrypt_note') });
    }
  }
  items.push({ key: 'sep' });
  items.push({
    key: 'detach',
    label: tg ? i18n.t('rplace.detach') : i18n.t('rplace.remove'),
    icon: '➖',
    note: tg ? i18n.t('rplace.detach_note') : undefined,
  });
  if (tg) {
    items.push({
      key: 'delacct',
      label: i18n.t('rplace.delete_account'),
      icon: '🗑️',
      danger: true,
      note: i18n.t('rplace.delete_account_note'),
    });
  }
  return items;
});

function onPlaceContext(p, ev) {
  rmenu.value = { place: p, x: ev.clientX, y: ev.clientY, encrypted: false };
  // 只有 Telegram 位置才有加密概念；异步查一次盘上格式，回来若菜单还开着就更新
  if (p.kind === 'telegram') {
    api.telegramPlaceEncrypted(p.id).then((enc) => {
      if (rmenu.value?.place?.id === p.id) rmenu.value.encrypted = enc;
    }).catch(() => {});
  }
}

async function onPlaceMenuPick(key) {
  const p = rmenu.value?.place;
  rmenu.value = null;
  if (!p) return;
  if (key === 'open') {
    await goRemote(p);
    return;
  }
  if (key === 'rename') {
    const next = window.prompt(
      i18n.t('rplace.rename_prompt', { name: p.name }), p.name);
    if (next === null) return;
    await renameTelegramPlace(p.id, next);
    return;
  }
  if (key === 'detach') {
    // 即便菜单里已写明后果，仍然确认一次：它与「删除账号」相邻，
    // 点错的代价不对称
    if (hasLocalSession(p)) {
      if (!window.confirm(i18n.t('rplace.detach_confirm', { name: p.name }))) return;
      await detachTelegramPlace(p.id);
    } else {
      await removeRemotePlace(p.id);
    }
    return;
  }
  if (key === 'encrypt') {
    await encryptTelegramPlace(p.id);
    return;
  }
  if (key === 'decrypt') {
    await decryptTelegramPlace(p.id);
    return;
  }
  if (key === 'delacct') {
    if (!window.confirm(i18n.t('rplace.delete_account_confirm', { name: p.name }))) return;
    await deleteTelegramAccount(p.id);
  }
}

/** 进入一个远程位置。抽屉同样要收起。 */
async function goRemote(p) {
  // 必须经由 openPlaceBrowserAt 切到云盘视图；只调 openRemotePlace 会把
  // 目录数据加载好却仍停在本地界面，用户看到的就是「点了没反应」
  await openPlaceBrowserAt(p.id);
  emit('navigate');
}

/** 新建一个虚拟远程位置：弹名字→建→进入。虚拟位置复用 remotePlace 那套状态与
 *  云盘视图，所以进入方式和真实位置一样走 openPlaceBrowserAt。 */
async function onNewVirtual() {
  const name = window.prompt(i18n.t('virtual.name_prompt'));
  if (name === null) return; // 取消
  const id = await createVirtualPlace(name.trim());
  if (id) await openPlaceBrowserAt(id);
  emit('navigate');
}

/** 常用目录的标签要翻译，磁盘根用原名。
 *
 * `real_name` 优先：可移除卷（SD 卡、U 盘）的名字由系统给出，
 * 插两张卡时靠它区分，没有对应的翻译键。
 */
function labelOf(place) {
  if (place.real_name) return place.real_name;
  const known = [
    'home',
    'desktop',
    'documents',
    'downloads',
    'pictures',
    'videos',
    'cache',
    'camera',
    'documents_shared',
    'movies',
    'music',
    'internal_storage',
    'removable',
  ];
  return known.includes(place.name) ? i18n.t(`places.${place.name}`) : place.name;
}

function iconOf(place) {
  const icons = {
    home: '🏠',
    desktop: '🖥️',
    documents: '📄',
    documents_shared: '📄',
    downloads: '⬇️',
    pictures: '🖼️',
    videos: '🎬',
    movies: '🎬',
    music: '🎵',
    camera: '📷',
    cache: '🗑️',
    internal_storage: '📱',
    removable: '💳',
  };
  return icons[place.name] || '💾';
}
</script>

<template>
  <aside class="side">
    <div class="sgrp">{{ i18n.t('places.title') }}</div>
    <button
      v-for="p in state.places"
      :key="p.path"
      class="sitem"
      :class="{ sel: isActive('local', p.path) }"
      :data-side="'place-' + p.name"
      :title="p.path"
      @click="go(p.path)"
    >
      <span aria-hidden="true">{{ iconOf(p) }}</span>
      <span class="stext">{{ labelOf(p) }}</span>
    </button>

    <!-- 存储权限入口。仅安卓且未授权时出现：
         mode 为 not-applicable 的平台（桌面）根本没有可申请的东西，
         显示一个点了没反应的按钮比不显示更糟。 -->
    <button
      v-if="!state.storage.granted && state.storage.mode !== 'not-applicable'"
      class="sitem grant"
      :disabled="granting"
      @click="onGrant"
    >
      <span aria-hidden="true">🔓</span>
      <span class="stext">
        {{ granting ? i18n.t('places.granting') : i18n.t('places.grant_storage') }}
      </span>
    </button>

    <button class="sitem" data-side="pick-folder" @click="$emit('pick')">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('nav.pick_folder') }}</span>
    </button>

    <!-- 远程位置单独分组，不混进「位置」。
         两者的响应延迟差一个数量级，混在一起用户会以为点进去卡住了。
         只读位置在名字后面挂徽标，而不是进去以后才知道——「我要把这个
         文件加密到哪」这个决定发生在点击之前。 -->
    <div class="sgrp">{{ i18n.t('rplace.title') }}</div>
    <button
      v-for="p in state.remotePlaces"
      :key="p.id"
      class="sitem"
      :class="{ sel: isActive('remote', p.id) }"
      :data-rp="p.id"
      :title="p.name"
      @click="goRemote(p)"
      @contextmenu.prevent="onPlaceContext(p, $event)"
    >
      <span aria-hidden="true">☁️</span>
      <span class="stext">{{ p.name }}</span>
      <!-- 这里读的是位置级能力（上界），而不是目录级的有效能力——侧栏列的是
           位置，还没进任何目录，能拿到的只有上界。语义上也正好：上界都没有写
           就说明这个位置处处不可写，标「只读」是准的；上界有写则不下结论、
           不显示徽标，具体哪个目录能写进去之后由 `currentCaps` 说。 -->
      <span v-if="!p.caps.write" class="ro">{{ i18n.t('rplace.readonly_badge') }}</span>
    </button>
    <button class="sitem" data-rp="add" @click="$emit('add-place')">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('rplace.add') }}</span>
    </button>
    <!-- 虚拟远程位置：本地收藏夹式，只存对真实位置文件的引用，不连服务器。
         图标用 🗂️ 与真实位置的 ☁️ 区分，让用户一眼看出这是本地虚拟的。 -->
    <button
      v-for="v in state.virtualPlaces"
      :key="v.id"
      class="sitem"
      :class="{ sel: isActive('remote', v.id) }"
      :data-vp="v.id"
      :title="v.name"
      @click="goRemote(v)"
    >
      <span aria-hidden="true">🗂️</span>
      <span class="stext">{{ v.name }}</span>
    </button>
    <button class="sitem" data-vp="new" @click="onNewVirtual">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('virtual.add') }}</span>
    </button>
    <!-- Telegram 单独一个入口，不混进「连接远程位置」那个 WebDAV 表单：
         它的登录方式完全不同（扫码，不是填地址和密码），塞进同一个表单
         只能做成一个选了之后大半字段都灰掉的下拉，反而更难懂。 -->
    <!-- 传输管理与文件浏览并列，是一个顶层入口而非某个位置的子页：
         任务跨对话、跨位置，挂在某个位置下面用户切走就找不到了 -->
    <div class="sgrp">{{ i18n.t('nav.global') }}</div>
    <button
      class="sitem"
      :class="{ sel: isActive('transfers', '') }"
      data-side="transfers"
      @click="openTransfers(); emit('navigate')"
    >
      <span aria-hidden="true">🔀</span>
      <span class="stext">{{ i18n.t('xfer.title') }}</span>
      <span v-if="activeTransfers" class="badge" data-side="xferbadge">
        {{ activeTransfers }}
      </span>
    </button>

    <button class="sitem" data-tg="entry" @click="$emit('telegram')">
      <span aria-hidden="true">✈️</span>
      <span class="stext">{{ i18n.t('tg.login_title') }}</span>
    </button>

    <div class="sgrp">{{ i18n.t('places.devices') }}</div>

    <!-- 已配对设备直接列出来，点一下打开面板。
         数量为 0 时也要有入口，否则用户找不到从哪开始配对 -->
    <button class="sitem" @click="$emit('devices')">
      <span aria-hidden="true">📡</span>
      <span class="stext">{{ i18n.t('device.manage') }}</span>
      <span v-if="state.pairedCount" class="badge">{{ state.pairedCount }}</span>
    </button>

    <button
      v-if="state.shareRunning"
      class="sitem sharing"
      @click="$emit('devices')"
    >
      <span aria-hidden="true">🟢</span>
      <span class="stext">{{ i18n.t('device.sharing_now') }}</span>
    </button>
  
  <ContextMenu
    v-if="rmenu"
    :items="rmenuItems"
    :x="rmenu.x"
    :y="rmenu.y"
    @pick="onPlaceMenuPick"
    @close="rmenu = null"
  />
</aside>
</template>

<style scoped>
/* 授权入口要比普通位置显眼：未授权时侧栏几乎是空的，
   用户得能一眼看到下一步该点哪里。 */
.sitem.grant {
  color: var(--accent, #2563eb);
  font-weight: 600;
}

/* 等待用户从设置页回来期间置灰，配合 disabled 防重复点击。 */
.sitem.grant:disabled {
  opacity: 0.6;
  cursor: default;
}

/* 只读徽标：必须在侧栏就可见。用户决定「把文件加密到哪」是在点击
   之前，进去才发现不能写已经晚了。 */
.ro {
  font-size: 9.5px;
  border: 1px solid var(--warn);
  color: var(--warn);
  border-radius: 3px;
  padding: 1px 4px;
  flex: none;
  letter-spacing: 0.3px;
}
.sitem.sel .ro {
  border-color: #fff;
  color: #fff;
}
</style>
