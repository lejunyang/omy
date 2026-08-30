<script setup>
/** 根组件。
 *
 * 只做三件事：按解锁状态选界面、挂预览层、注册全局快捷键。
 */

import { ref, computed, onMounted, onBeforeUnmount } from 'vue';
import { state, lock } from './store.js';
import UnlockScreen from './components/UnlockScreen.vue';
import MainScreen from './components/MainScreen.vue';
import PreviewOverlay from './components/PreviewOverlay.vue';

const previewId = ref(null);

/** 预览目标。从 `state.files` 现查，而不是把整个对象存起来——
 * 锁定时列表被清空，现查会自然得到 undefined 从而关闭预览。 */
const previewFile = computed(() => {
  if (!previewId.value) return null;
  const f = state.files.find((x) => x.id === previewId.value);
  return f && f.unlocked ? f : null;
});

function onKey(e) {
  // 全局快捷键：Ctrl/Cmd+L 锁定（文档 §8）
  if ((e.ctrlKey || e.metaKey) && e.key === 'l') {
    e.preventDefault();
    if (state.unlocked) doLock();
  }
}

async function doLock() {
  previewId.value = null;
  await lock();
}

onMounted(() => document.addEventListener('keydown', onKey));
onBeforeUnmount(() => document.removeEventListener('keydown', onKey));
</script>

<template>
  <UnlockScreen v-if="!state.unlocked" />
  <MainScreen v-else @open="previewId = $event" @lock="doLock" />

  <PreviewOverlay
    v-if="previewFile"
    :key="previewFile.id"
    :file="previewFile"
    @close="previewId = null"
  />
</template>
