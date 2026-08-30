<script setup>
/** 侧栏：位置 + 附近设备。
 *
 * # 设备区目前是什么状态
 *
 * 局域网共享的完整能力已经在 `omy-net` 里实现并通过端到端验证，
 * 但只有 CLI 接上了（`omy share pair / serve / connect`）。
 * GUI 这一侧还没接，所以这里**如实说明**并给出 CLI 用法，
 * 而不是画一个点了没反应的假按钮。
 */

import * as i18n from '../i18n.js';
import { state, navigate } from '../store.js';

defineEmits(['pick']);

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
      @click="navigate(p.path)"
    >
      <span aria-hidden="true">{{ iconOf(p) }}</span>
      <span class="stext">{{ labelOf(p) }}</span>
    </button>

    <button class="sitem" @click="$emit('pick')">
      <span aria-hidden="true">➕</span>
      <span class="stext">{{ i18n.t('nav.pick_folder') }}</span>
    </button>

    <div class="sgrp">{{ i18n.t('places.devices') }}</div>
    <div class="sidenote">
      {{ i18n.t('device.not_implemented') }}
      <code>{{ i18n.t('device.cli_hint') }}</code>
    </div>
  </aside>
</template>
