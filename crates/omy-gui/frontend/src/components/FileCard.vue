<script setup>
/** 单个文件卡片（网格视图）。
 *
 * # 锁定态必须三者齐全
 *
 * 文档 §10：只隐藏文件名是不够的，缩略图和文件大小同样泄露信息。
 * 所以锁定卡片走**完全独立**的分支，不复用任何解锁态的字段——
 * 不是「把 name 换成占位符」，而是根本不渲染那些节点。
 *
 * 这也是用框架的好处：`v-if` / `v-else` 让两个分支在结构上就是
 * 互斥的，不存在漏改某个字段导致信息泄露的可能。
 */

import { computed } from 'vue';
import * as i18n from '../i18n.js';
import { thumbUrl } from '../store.js';

const props = defineProps({
  file: { type: Object, required: true },
});

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

const kind = computed(() => props.file.kind || 'other');
const icon = computed(() => ICONS[kind.value] || ICONS.other);
const tierIcon = computed(() => TIER_ICONS[props.file.tier]);

const meta = computed(() => {
  const bits = [i18n.formatSize(props.file.size)];
  if (props.file.duration_ms) bits.push(i18n.formatDuration(props.file.duration_ms));
  return bits.join(' · ');
});
</script>

<template>
  <div v-if="!file.unlocked" class="card" data-locked="1" tabindex="0">
    <div class="thumb" aria-hidden="true">🔒</div>
    <div class="info">
      <div class="name">{{ i18n.t('file.locked_name') }}</div>
      <div class="meta">{{ i18n.t('file.locked_meta') }}</div>
    </div>
  </div>

  <div
    v-else
    class="card"
    data-locked="0"
    tabindex="0"
    role="button"
    :aria-label="file.name"
  >
    <div class="thumb">
      <img v-if="file.has_thumbnail" :src="thumbUrl(file.id)" alt="" loading="lazy" />
      <span v-else aria-hidden="true">{{ icon }}</span>
    </div>
    <div class="info">
      <div class="name" :title="file.name">{{ file.name }}</div>
      <div class="meta">
        {{ meta
        }}<span
          v-if="tierIcon"
          class="tier"
          :title="i18n.t(`playback.tier_${file.tier}_desc`)"
          >{{ tierIcon }}</span
        >
      </div>
    </div>
  </div>
</template>
