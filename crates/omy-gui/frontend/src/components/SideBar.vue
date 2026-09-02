<script setup>
/** 侧栏：位置 + 附近设备。 */

import * as i18n from '../i18n.js';
import { state, navigate } from '../store.js';

defineProps({
  /** 移动端（抽屉形态）。抽屉里点完一项要自动收起，
   *  否则它盖住半个屏幕，用户看不到刚打开的目录。 */
  mobile: { type: Boolean, default: false },
});

const emit = defineEmits(['pick', 'devices', 'navigate', 'lang']);

/** 进入某个位置，并通知父组件（抽屉据此收起）。 */
function go(path) {
  navigate(path);
  emit('navigate');
}

/** 常用目录的标签要翻译，磁盘根用原名。 */
function labelOf(place) {
  const known = ['home', 'desktop', 'documents', 'downloads', 'pictures', 'videos'];
  return known.includes(place.name) ? i18n.t(`places.${place.name}`) : place.name;
}

function iconOf(place) {
  const icons = {
    home: '🏠',
    desktop: '🖥️',
    documents: '📄',
    downloads: '⬇️',
    pictures: '🖼️',
    videos: '🎬',
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

    <button class="sitem" @click="$emit('pick')">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('nav.pick_folder') }}</span>
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
