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

import { computed, ref, onBeforeUnmount } from 'vue';
import * as i18n from '../i18n.js';
import { thumbUrl } from '../store.js';
import { isMobile } from '../viewport.js';

const props = defineProps({
  entry: { type: Object, required: true },
  selected: { type: Boolean, default: false },
  /** 当前列表里是否已有选中项。
   *
   * 不能用 selected 代替：别的卡片被选中时，本卡片的 selected 仍是
   * false，而此时点本卡片应当是「加选」而不是「打开」。 */
  selectionActive: { type: Boolean, default: false },
});

const emit = defineEmits(['open', 'select']);

/* ---- 打开手势：桌面双击，移动端单击 ----
 *
 * 触屏上不存在「双击打开」这个约定，而且移动 WebView 会把快速两次
 * 点击当成缩放手势吃掉。沿用 dblclick 的直接后果是移动端**点什么
 * 都打不开**——界面看着完全正常，所以这类 bug 很难从截图上发现。
 *
 * 长按改为进入选择：移动端没有 Ctrl 键，不给长按的话就完全无法多选，
 * 「加密选中项」这个主功能在手机上就用不了。
 */

const LONG_PRESS_MS = 500;
/** 手指移动超过这个距离就判定为滚动，不是长按。
 *  没有这个判定的话，滑动列表会频繁误触发选择。 */
const MOVE_TOLERANCE = 10;

let timer = null;
let startX = 0;
let startY = 0;
/** 长按已触发，随后的 click 要吞掉，否则长按选中之后又立刻打开。 */
const suppressClick = ref(false);

function clearTimer() {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
}

function onPointerDown(ev) {
  if (!isMobile.value || ev.pointerType === 'mouse') return;
  startX = ev.clientX;
  startY = ev.clientY;
  suppressClick.value = false;
  clearTimer();
  timer = setTimeout(() => {
    suppressClick.value = true;
    // 传 true 当作「加选」：长按的语义就是多选，
    // 若按单选处理，长按第二个会把第一个取消掉
    emit('select', { ctrlKey: true });
  }, LONG_PRESS_MS);
}

function onPointerMove(ev) {
  if (!timer) return;
  if (
    Math.abs(ev.clientX - startX) > MOVE_TOLERANCE ||
    Math.abs(ev.clientY - startY) > MOVE_TOLERANCE
  ) {
    clearTimer();
  }
}

function onPointerUp() {
  clearTimer();
}

function onClick(ev) {
  if (suppressClick.value) {
    suppressClick.value = false;
    return;
  }
  // 移动端：已经有选中项时，点击继续做多选而不是打开——
  // 否则用户长按选了第一个，想点第二个加选，结果直接打开了文件
  if (isMobile.value) {
    if (props.selected || props.selectionActive) emit('select', { ctrlKey: true });
    else emit('open', props.entry);
    return;
  }
  emit('select', ev);
}

onBeforeUnmount(clearTimer);

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
  const byExt = EXT_ICONS[props.entry.ext];
  if (byExt) return byExt;
  // 后缀不认识时，退回后端算出的预览类别。容器内的条目常常是
  // 没列进 EXT_ICONS 的后缀，但后端已经按类别分好了；
  // 一律回落到 📦 会让容器里的图片全部显示成「包」
  return (
    { image: '🖼️', video: '🎬', audio: '🎵', text: '📄' }[props.entry.preview] ||
    '📦'
  );
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
    :aria-label="displayName"
    @dblclick="$emit('open', entry)"
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb dir">
      <span aria-hidden="true">📁</span>
      <!-- 加密目录挂一个角标而不是换成锁图标：它首先是个文件夹，
           「能像普通文件夹一样进」是这里要传达的第一件事 -->
      <span v-if="entry.is_encrypted_dir" class="encbadge" aria-hidden="true">🔒</span>
    </div>
    <div class="cname" :title="displayName">{{ displayName }}</div>
    <div class="cmeta">
      {{ entry.is_encrypted_dir ? i18n.t('file.folder_encrypted') : i18n.t('file.folder') }}
    </div>
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
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
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
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
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
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb"><span aria-hidden="true">{{ icon }}</span></div>
    <div class="cname" :title="entry.name">{{ entry.name }}</div>
    <div class="cmeta">{{ sizeText }}</div>
  </div>
</template>
