<script setup lang="ts">
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
import * as i18n from '../i18n';
import { thumbUrl, state } from '../store';
import { useThumbLoad } from '../thumbload';
import { isMobile } from '../viewport';
import { fileClickAction } from '../file-interaction';

const props = defineProps({
  entry: { type: Object, required: true },
  selected: { type: Boolean, default: false },
});

const emit = defineEmits(['open', 'select', 'menu']);

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
    // 先选中再弹菜单：菜单里的操作都作用于「选中项」，
    // 不先选中的话长按弹出的菜单会作用在别的条目上
    //
    // 传 true 当作「加选」：长按的语义就是多选，
    // 若按单选处理，长按第二个会把第一个取消掉
    if (!props.selected) emit('select', { ctrlKey: true });
    // 触屏没有右键，长按是唯一的菜单入口。只做「加选」的话
    // 手机上完全没有办法删除或重命名文件
    emit('menu', { entry: props.entry, x: startX, y: startY });
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
  const action = fileClickAction(isMobile.value, suppressClick.value);
  suppressClick.value = false;
  if (action === 'ignore') return;
  // 移动端单击永远是打开；进入或扩展选择只能再次长按。
  // 不能让已有选择态改变单击语义，否则打开一个文件后很容易误选下一项。
  if (action === 'open') emit('open', props.entry);
  else emit('select', ev);
}

/** 桌面右键。
 *
 * 先选中再弹菜单：菜单里的操作作用于「选中项」，在一个未选中的条目上
 * 右键却对之前选中的东西生效，是最容易造成误删的一种交互。
 *
 * 已经在多选里的条目不重置选择——用户选了 5 个文件再右键，意图显然是
 * 对这 5 个一起操作。
 */
function onContextMenu(ev) {
  if (!props.selected) emit('select', { ctrlKey: false });
  emit('menu', { entry: props.entry, x: ev.clientX, y: ev.clientY });
}

onBeforeUnmount(clearTimer);

/** 按扩展名选图标。取值为 AppIcon 的语义名。 */
const EXT_ICONS = {
  jpg: 'image', jpeg: 'image', png: 'image', gif: 'image', webp: 'image', bmp: 'image',
  svg: 'image', heic: 'image', heif: 'image', avif: 'image', apng: 'image',
  mp4: 'video', mkv: 'video', mov: 'video', avi: 'video', webm: 'video', flv: 'video', wmv: 'video',
  mp3: 'music', flac: 'music', wav: 'music', m4a: 'music', aac: 'music', ogg: 'music', opus: 'music',
  txt: 'file', md: 'file', log: 'file', json: 'file', xml: 'file', csv: 'file',
  pdf: 'file', doc: 'file', docx: 'file', xls: 'spreadsheet', xlsx: 'spreadsheet', ppt: 'file', pptx: 'file',
  zip: 'archiveFile', rar: 'archiveFile', '7z': 'archiveFile', gz: 'archiveFile', tar: 'archiveFile',
};

/** 播放分级的图标（文档 §4.4）。取值为 AppIcon 的语义名。 */
const TIER_ICONS = { p1: 'fast', p2: 'refresh', p3: 'turtle' };

const icon = computed(() => {
  if (props.entry.is_dir) return 'folder';
  const byExt = EXT_ICONS[props.entry.ext];
  if (byExt) return byExt;
  // 后缀不认识时，退回后端算出的预览类别。容器内的条目常常是
  // 没列进 EXT_ICONS 的后缀，但后端已经按类别分好了；
  // 一律回落到 package 图标会让容器里的图片全部显示成「包」
  return (
    { image: 'image', video: 'video', audio: 'music', text: 'file' }[props.entry.preview] ||
    'package'
  );
});

/** 已解锁加密文件的真实名，其余用磁盘名。 */
const displayName = computed(() => props.entry.real_name || props.entry.name);

const sizeText = computed(() =>
  props.entry.size == null ? '' : i18n.formatSize(props.entry.size),
);

const known = computed(() => props.entry.meta || null);

// 缩略图按可见性加载：一次性渲染几千张图会让 WebView 吃掉几 GB 内存
// （每张解码后约 300 KB，与压缩后的几 KB 完全是两回事），同时几千个
// /thumb 请求会把后端协议线程占满，表现是整个界面卡住。
// 细节与两个边距的取舍见 thumbload.js
const { thumbEl, shouldLoad } = useThumbLoad();
const tierIcon = computed(() => TIER_ICONS[known.value?.tier]);

/** 角标的悬停说明。
 *
 * 光一个分级图标用户无从理解，实测就有人问「这是什么意思」。后端已经算好
 * tier_reason（如「容器与编码均被 WebView 原生支持」），优先用它；
 * 拿不到时退回按分级给一句通用解释，不能让 title 是空的。
 */
const tierTitle = computed(() => {
  const tier = known.value?.tier;
  if (!tier) return '';
  const label = i18n.t(`tier.${tier}`);
  const reason = known.value?.tier_reason;
  return reason ? `${label} — ${reason}` : label;
});
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
    @contextmenu.prevent="onContextMenu"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb dir">
      <AppIcon name="folder" />
      <!-- 加密目录挂一个角标而不是换成锁图标：它首先是个文件夹，
           「能像普通文件夹一样进」是这里要传达的第一件事 -->
      <span v-if="entry.is_encrypted_dir" class="encbadge" aria-hidden="true"><AppIcon name="lock" :size="13" /></span>
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
    :data-entry-id="entry.entry_id || ''"
    data-locked="1"
    @dblclick="$emit('open', entry)"
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @contextmenu.prevent="onContextMenu"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb lock"><AppIcon name="lock" /></div>
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
    :data-entry-id="entry.entry_id || ''"
    data-locked="0"
    @dblclick="$emit('open', entry)"
    @click="onClick"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerUp"
    @contextmenu.prevent="onContextMenu"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div ref="thumbEl" class="thumb">
      <img
        v-if="state.showThumbnails && known?.has_thumbnail && shouldLoad"
        :src="thumbUrl(entry.entry_id)"
        alt=""
        loading="lazy"
      />
      <AppIcon v-else name="unlock" />
      <span v-if="tierIcon" class="tier" :title="tierTitle"><AppIcon :name="tierIcon" :size="14" /></span>
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
    @contextmenu.prevent="onContextMenu"
    @keydown.enter.prevent="$emit('open', entry)"
  >
    <div class="thumb"><AppIcon :name="icon" /></div>
    <div class="cname" :title="entry.name">{{ entry.name }}</div>
    <div class="cmeta">{{ sizeText }}</div>
  </div>
</template>
