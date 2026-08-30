<script setup>
/** 文件网格里的一项。
 *
 * # 三种形态，结构上互斥
 *
 * 目录、普通文件、加密文件的显示规则完全不同，用 `v-if` 分支而不是
 * 在一套模板里堆 `v-bind` 条件——后者很容易出现「改了解锁分支忘了
 * 改锁定分支」，而锁定分支多显示一个字段就是信息泄露。
 *
 * 锁定的加密文件：占位名 + 锁图案 + 「密码未解锁」，三者缺一不可。
 * 文档 §10 指出只隐藏文件名是不够的，缩略图和明文大小同样泄露信息。
 * 密文大小是例外——它在磁盘上本来就藏不住。
 */

import { computed } from 'vue';
import * as i18n from '../i18n.js';
import { thumbUrl } from '../store.js';

const props = defineProps({
  entry: { type: Object, required: true },
  selected: { type: Boolean, default: false },
});

defineEmits(['open', 'select']);

/** 按扩展名选图标。 */
const EXT_ICONS = {
  jpg: '🖼️', jpeg: '🖼️', png: '🖼️', gif: '🖼️', webp: '🖼️', bmp: '🖼️',
  svg: '🖼️', heic: '🖼️', heif: '🖼️', avif: '🖼️', apng: '🖼️',
  mp4: '🎬', mkv: '🎬', mov: '🎬', avi: '🎬', webm: '🎬', flv: '🎬', wmv: '🎬',
  mp3: '🎵', flac: '🎵', wav: '🎵', m4a: '🎵', aac: '🎵', ogg: '🎵', opus: '🎵',
  txt: '📄', md: '📄', log: '📄', json: '📄', xml: '📄', csv: '📄',
  pdf: '📕', doc: '📘', docx: '📘', xls: '📗', xlsx: '📗', ppt: '📙', pptx: '📙',
  zip: '🗜️', rar: '🗜️', '7z': '🗜️', gz: '🗜️', tar: '🗜️',
};

/** 播放分级的图标（文档 §4.4）。 */
const TIER_ICONS = { p1: '⚡', p2: '🔄', p3: '🐌' };

const icon = computed(() => {
  if (props.entry.is_dir) return '📁';
  return EXT_ICONS[props.entry.ext] || '📦';
});

/** 已解锁加密文件的真实名，其余用磁盘名。 */
const displayName = computed(() => props.entry.real_name || props.entry.name);

const sizeText = computed(() =>
  props.entry.size == null ? '' : i18n.formatSize(props.entry.size),
);

const known = computed(() => props.entry.meta || null);
const tierIcon = computed(() => TIER_ICONS[known.value?.tier]);
</script>

<template>
  <!-- 目录 -->
  <div
    v-if="entry.is_dir"
    class="card"
    :class="{ sel: selected }"
    tabindex="0"
    role="button"
    :aria-label="entry.name"
    @dblclick="$emit('open', entry)"
    @click="$emit('select', $event)"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb dir"><span aria-hidden="true">📁</span></div>
    <div class="cname" :title="entry.name">{{ entry.name }}</div>
    <div class="cmeta">{{ i18n.t('file.folder') }}</div>
  </div>

  <!-- 加密文件（锁定）：不显示任何内容线索 -->
  <div
    v-else-if="entry.is_encrypted && !entry.unlocked"
    class="card locked"
    :class="{ sel: selected }"
    tabindex="0"
    role="button"
    :aria-label="i18n.t('file.locked_name')"
    @dblclick="$emit('open', entry)"
    @click="$emit('select', $event)"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb lock"><span aria-hidden="true">🔒</span></div>
    <div class="cname">{{ i18n.t('file.locked_name') }}</div>
    <!-- 只显示密文大小：它在磁盘上本来就藏不住，
         但明文大小、类型、时长一律不显示 -->
    <div class="cmeta">{{ sizeText }} · {{ i18n.t('file.locked_meta') }}</div>
  </div>

  <!-- 加密文件（已解锁） -->
  <div
    v-else-if="entry.is_encrypted"
    class="card"
    :class="{ sel: selected }"
    tabindex="0"
    role="button"
    :aria-label="displayName"
    @dblclick="$emit('open', entry)"
    @click="$emit('select', $event)"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb">
      <img
        v-if="known?.has_thumbnail"
        :src="thumbUrl(entry.entry_id)"
        alt=""
        loading="lazy"
      />
      <span v-else aria-hidden="true">🔓</span>
      <span v-if="tierIcon" class="tier">{{ tierIcon }}</span>
    </div>
    <div class="cname" :title="displayName">{{ displayName }}</div>
    <div class="cmeta">
      {{ known?.size != null ? i18n.formatSize(known.size) : sizeText }}
      <span v-if="known?.duration_ms">· {{ i18n.formatDuration(known.duration_ms) }}</span>
    </div>
  </div>

  <!-- 普通文件 -->
  <div
    v-else
    class="card"
    :class="{ sel: selected }"
    tabindex="0"
    role="button"
    :aria-label="entry.name"
    @dblclick="$emit('open', entry)"
    @click="$emit('select', $event)"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb"><span aria-hidden="true">{{ icon }}</span></div>
    <div class="cname" :title="entry.name">{{ entry.name }}</div>
    <div class="cmeta">{{ sizeText }}</div>
  </div>
</template>
