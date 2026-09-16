<script setup>
/** 侧栏：位置 + 附近设备。 */

import { ref } from 'vue';
import * as i18n from '../i18n.js';
import { state, navigate, grantStorageAccess, openPlaceBrowserAt } from '../store.js';

defineProps({
  /** 移动端（抽屉形态）。抽屉里点完一项要自动收起，
   *  否则它盖住半个屏幕，用户看不到刚打开的目录。 */
  mobile: { type: Boolean, default: false },
});

const emit = defineEmits(['pick', 'devices', 'navigate', 'lang', 'add-place']);

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

/** 进入某个位置，并通知父组件（抽屉据此收起）。 */
function go(path) {
  navigate(path);
  emit('navigate');
}

/** 进入一个远程位置。抽屉同样要收起。 */
async function goRemote(p) {
  // 必须经由 openPlaceBrowserAt 切到云盘视图；只调 openRemotePlace 会把
  // 目录数据加载好却仍停在本地界面，用户看到的就是「点了没反应」
  await openPlaceBrowserAt(p.id);
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
      :class="{ sel: state.cwd === p.path }"
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
      :class="{ sel: state.remotePlace === p.id }"
      :data-rp="p.id"
      :title="p.name"
      @click="goRemote(p)"
    >
      <span aria-hidden="true">☁️</span>
      <span class="stext">{{ p.name }}</span>
      <span v-if="!p.caps.write" class="ro">{{ i18n.t('rplace.readonly_badge') }}</span>
    </button>
    <button class="sitem" data-rp="add" @click="$emit('add-place')">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('rplace.add') }}</span>
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
