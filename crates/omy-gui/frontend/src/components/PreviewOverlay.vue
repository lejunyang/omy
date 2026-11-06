<script setup lang="ts">
/** 预览层。
 *
 * # 三条来自实测的硬约束，改动前请先读完
 *
 * 1. **`crossorigin="anonymous"` 不是可选项**。页面在 `tauri.localhost`，
 *    协议在 `omystream.localhost`，两者不同源。不声明的话浏览器不走
 *    CORS 校验，直接把媒体标记为「跨源」，于是任何
 *    `canvas.drawImage(video)` 之后的 `getImageData` 都会抛 SecurityError
 *    （画布被污染）。影响的不只是自动化测试——应用内截图、生成缩略图
 *    都要读像素。
 *
 * 2. **文本用 `textContent`**。文件内容完全不可信，`v-html` 等于执行
 *    未知代码。模板里的 `{{ }}` 插值本身就是 textContent 语义，
 *    所以这里直接绑定即可，比重构前手写 `pv.textContent = text` 更难写错。
 *
 * 3. **关闭前必须先停媒体**。否则 WebView 可能继续持有已解密的缓冲区，
 *    也会继续向协议发请求。`onBeforeUnmount` 里做，保证任何关闭路径
 *    （点 ✕、按 Esc、切换文件）都覆盖到。
 */

import { ref, computed, onMounted, onBeforeUnmount, useTemplateRef } from 'vue';
import { playing } from '../autolock';
import OmyVideoPlayer from './OmyVideoPlayer.vue';
import OmyAudioPlayer from './OmyAudioPlayer.vue';
import * as i18n from '../i18n';
import {
  fileUrl,
  remoteFileUrl,
  plainUrl,
  containerItemUrl,
  placeFileUrl,
} from '../store';

const props = defineProps({
  file: { type: Object, required: true },
  /** 内容来自远端设备。
   *
   * 只影响 URL 前缀——播放、seek、文本加载全都不变。
   * 这正是把远端做成同一个协议的收益：远端视频的拖动行为
   * 与本地**必然**一致，不会出现「本地能拖远端不能」。
   */
  remote: { type: Boolean, default: false },
  /** 内容是磁盘上的未加密文件。
   *
   * 同样只影响 URL 前缀。四种来源（本地加密 / 远端加密 / 本地明文 /
   * 容器内文件）共用这一个组件，是为了让它们的播放行为**不可能**产生
   * 差异——分成四个组件的话，迟早有人只修其中一个。
   */
  plain: { type: Boolean, default: false },
  /** 内容是加密文件夹（容器）里的一个文件。
   *
   * 同样只影响 URL 前缀。刻意与 `plain` 分开：那个不需要密钥，这个需要；
   * 而且这个**没有**「用外部应用打开」的出路——容器内的条目只是载荷里
   * 的一段区间，磁盘上没有对应文件可交给系统程序。
   */
  inContainer: { type: Boolean, default: false },
  /** 内容来自远程存储位置（WebDAV 等云盘）。
   *
   * 只影响 URL 前缀（`/pfile/`）。播放、seek、Range 行为与本地/远端设备
   * 完全一致——这正是所有来源共用同一个 omystream 协议的目的。
   */
  place: { type: Boolean, default: false },
  /** 同一目录里还有上一个 / 下一个可预览的文件（相册式翻看）。 */
  hasPrev: { type: Boolean, default: false },
  hasNext: { type: Boolean, default: false },
});

const emit = defineEmits(['close', 'external', 'external-readonly', 'external-edit', 'prev', 'next']);

/* ---- 相册式翻看与图片缩放 ----
 *
 * 原来预览只能看一个文件：看下一张照片得关掉、再在列表里找、再打开。
 * 现在左右箭头键 / 两侧按钮 / 手机左右滑动切换同目录的上一个、下一个；
 * 图片支持双指缩放、双击（双点）放大与拖动。
 *
 * 音视频不接管方向键：播放器自己用它们快进快退。 */
const scale = ref(1);
const tx = ref(0);
const ty = ref(0);
const imgStyle = computed(() => ({
  transform: `translate(${tx.value}px, ${ty.value}px) scale(${scale.value})`,
}));

function resetZoom() {
  scale.value = 1;
  tx.value = 0;
  ty.value = 0;
}

function clampScale(v: number) {
  return Math.min(6, Math.max(1, v));
}

/** 正在按着的触点：id -> 坐标。两个触点时是双指缩放。 */
const pointers = new Map<number, { x: number; y: number }>();
let pinchStart = 0;
let pinchScale = 1;
let swipeStartX = 0;
let swipeStartY = 0;
let lastTap = 0;

function dist() {
  const [a, b] = [...pointers.values()];
  return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
}

function onPointerDown(ev: PointerEvent) {
  pointers.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
  if (pointers.size === 2) {
    pinchStart = dist();
    pinchScale = scale.value;
  } else if (pointers.size === 1) {
    swipeStartX = ev.clientX;
    swipeStartY = ev.clientY;
  }
}

function onPointerMove(ev: PointerEvent) {
  const prev = pointers.get(ev.pointerId);
  if (!prev) return;
  pointers.set(ev.pointerId, { x: ev.clientX, y: ev.clientY });
  if (pointers.size === 2 && pinchStart > 0 && kind.value === 'image') {
    scale.value = clampScale(pinchScale * (dist() / pinchStart));
    if (scale.value === 1) {
      tx.value = 0;
      ty.value = 0;
    }
  } else if (pointers.size === 1 && scale.value > 1) {
    // 放大后单指拖动查看局部
    tx.value += ev.clientX - prev.x;
    ty.value += ev.clientY - prev.y;
  }
}

function onPointerUp(ev: PointerEvent) {
  const wasSingle = pointers.size === 1;
  pointers.delete(ev.pointerId);
  if (pointers.size < 2) pinchStart = 0;
  // 音视频播放器自己用横向拖动调进度，不在上面翻页
  if (!wasSingle || scale.value > 1 || kind.value === 'video' || kind.value === 'audio') return;
  // 未放大时的水平快滑 = 翻页；竖向位移大的是滚动或误触，不算
  const dx = ev.clientX - swipeStartX;
  const dy = ev.clientY - swipeStartY;
  if (Math.abs(dx) > 60 && Math.abs(dx) > Math.abs(dy) * 1.5) {
    if (dx < 0 && props.hasNext) emit('next');
    else if (dx > 0 && props.hasPrev) emit('prev');
  }
}

/** 双击（鼠标）/ 双点（触屏）在原尺寸与 2.5 倍之间切换。 */
function toggleZoom() {
  if (scale.value > 1) resetZoom();
  else scale.value = 2.5;
}

function onImgPointerUp(ev: PointerEvent) {
  if (ev.pointerType === 'mouse') return;
  const now = performance.now();
  if (now - lastTap < 300) {
    toggleZoom();
    lastTap = 0;
  } else {
    lastTap = now;
  }
}

/** 桌面 Ctrl + 滚轮缩放（与浏览器、看图软件的习惯一致）。 */
function onWheel(ev: WheelEvent) {
  if (!ev.ctrlKey || kind.value !== 'image') return;
  ev.preventDefault();
  scale.value = clampScale(scale.value * (ev.deltaY < 0 ? 1.15 : 1 / 1.15));
  if (scale.value === 1) resetZoom();
}

function onKey(ev: KeyboardEvent) {
  if (kind.value === 'video' || kind.value === 'audio') return;
  const t = ev.target as HTMLElement | null;
  if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA')) return;
  if (ev.key === 'ArrowRight' && props.hasNext) {
    ev.preventDefault();
    emit('next');
  } else if (ev.key === 'ArrowLeft' && props.hasPrev) {
    ev.preventDefault();
    emit('prev');
  }
}

const media = useTemplateRef('media');
const text = ref('');
const mediaError = ref('');

const src = computed(() => {
  if (props.inContainer) return containerItemUrl(props.file.id);
  if (props.plain) return plainUrl(props.file.id);
  if (props.place) return placeFileUrl(props.file.id);
  return props.remote ? remoteFileUrl(props.file.id) : fileUrl(props.file.id);
});
const kind = computed(() => props.file.kind || 'other');

/** 文本内容取回后渲染。 */
async function loadText() {
  try {
    const r = await fetch(src.value);
    if (!r.ok) throw new Error(String(r.status));
    text.value = await r.text();
  } catch {
    text.value = i18n.te('preview_failed');
  }
}

function onVideoError(code: number) {
  mediaError.value = i18n.te(`media_error_${code}`, i18n.te('preview_failed'));
}

function onVideoPlay() {
  playing.value = true;
}

function onVideoPause() {
  playing.value = false;
}

onMounted(() => {
  document.addEventListener('keydown', onKey);
  if (kind.value === 'text') {
    text.value = i18n.t('playback.loading');
    loadText();
  }
});

onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKey);
  // 音视频的暂停与缓冲释放由各自播放器负责。
  // 无论从哪条路径关闭都要清掉，否则关了预览还一直算「在播放」，
  // 自动锁定就永远不会触发——那等于这个功能默默失效了
  playing.value = false;
});
</script>

<template>
  <div class="overlay">
    <div class="overlay-bar">
      <span class="title">{{ file.name }}</span>
      <span class="spacer"></span>
      <button v-if="place && file.externalAvailable" class="btn small" @click="$emit('external-readonly')">
        {{ i18n.t('rplace.open_readonly') }}
      </button>
      <button v-if="place && file.externalAvailable && file.editable" class="btn small primary" @click="$emit('external-edit')">
        {{ i18n.t('rplace.open_editable') }}
      </button>
      <button id="pv-close" class="iconbtn" :aria-label="i18n.t('actions.close')" @click="$emit('close')">
        <AppIcon name="close" />
      </button>
    </div>
    <div
      class="overlay-body"
      :class="{ 'pv-touch': kind === 'image' }"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
      @wheel="onWheel"
    >
      <div v-if="mediaError" class="overlay-msg">{{ mediaError }}</div>

      <OmyVideoPlayer
        v-else-if="kind === 'video'"
        :src="src"
        autoplay
        @error="onVideoError"
        @play="onVideoPlay"
        @pause="onVideoPause"
      />

      <OmyAudioPlayer
        v-else-if="kind === 'audio'"
        :src="src"
        :name="file.name"
        :mime="file.mime"
        :encrypted="!plain && !(place && file.isEncrypted === false)"
        autoplay
        @play="onVideoPlay"
        @pause="onVideoPause"
      />

      <!-- SVG 也走 <img>：浏览器的受限模式会禁掉其中的脚本。
           绝不用 v-html 插入 SVG——那等于执行未知代码 -->
      <img
        v-else-if="kind === 'image'"
        id="pv"
        ref="media"
        class="pv-img"
        :class="{ zoomed: scale > 1 }"
        :src="src"
        :alt="file.name"
        :style="imgStyle"
        crossorigin="anonymous"
        draggable="false"
        @dblclick="toggleZoom"
        @pointerup="onImgPointerUp"
      />

      <!-- {{ }} 是 textContent 语义，文件内容不会被当成 HTML 解析 -->
      <pre v-else-if="kind === 'text'" id="pv">{{ text }}</pre>

      <!-- 应用内看不了的类型：给一条出路，而不是一句「不支持」就完事。
           PDF、压缩包、Office 文档都会走到这里（文档 §8 的既定设计） -->
      <div v-else class="overlay-msg">
        <p>{{ i18n.t('playback.cannot_preview') }}</p>
        <button v-if="plain" class="btn primary" @click="$emit('external')">
          <AppIcon name="external" /> {{ i18n.t('file.open_external') }}
        </button>
      </div>

      <!-- 上一个 / 下一个。手机上也保留：滑动之外给一个看得见的入口 -->
      <button
        v-if="hasPrev"
        class="pv-nav prev"
        :aria-label="i18n.t('playback.prev_file')"
        @click.stop="$emit('prev')"
        @pointerdown.stop
      >
        <AppIcon name="arrowLeft" />
      </button>
      <button
        v-if="hasNext"
        class="pv-nav next"
        :aria-label="i18n.t('playback.next_file')"
        @click.stop="$emit('next')"
        @pointerdown.stop
      >
        <AppIcon name="arrowLeft" />
      </button>
    </div>
  </div>
</template>
