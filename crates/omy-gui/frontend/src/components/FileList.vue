<script setup>
/** 列表视图。
 *
 * 锁定行同样走独立分支，理由见 `FileCard.vue`。
 */

import * as i18n from '../i18n.js';

defineProps({
  files: { type: Array, required: true },
});

defineEmits(['open']);

const ICONS = {
  video: '🎬',
  audio: '🎵',
  image: '🖼️',
  text: '📄',
  other: '📦',
};
</script>

<template>
  <table class="list">
    <thead>
      <tr>
        <th>{{ i18n.t('view.name') }}</th>
        <th>{{ i18n.t('view.size') }}</th>
        <th>{{ i18n.t('view.kind') }}</th>
      </tr>
    </thead>
    <tbody>
      <template v-for="f in files" :key="f.id">
        <tr v-if="!f.unlocked" data-locked="1">
          <td>🔒 {{ i18n.t('file.locked_name') }}</td>
          <td>{{ i18n.t('file.locked_meta') }}</td>
          <td></td>
        </tr>
        <tr
          v-else
          tabindex="0"
          @dblclick="$emit('open', f.id)"
          @keydown.enter.prevent="$emit('open', f.id)"
          @keydown.space.prevent="$emit('open', f.id)"
        >
          <td :title="f.name">{{ ICONS[f.kind || 'other'] }} {{ f.name }}</td>
          <td>{{ i18n.formatSize(f.size) }}</td>
          <td>{{ i18n.t(`kind.${f.kind || 'other'}`) }}</td>
        </tr>
      </template>
    </tbody>
  </table>
</template>
