<script setup>
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
import * as i18n from '../i18n.js';
import { fileUrl, remoteFileUrl, plainUrl } from '../store.js';

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
   * 同样只影响 URL 前缀。三种来源（本地加密 / 远端加密 / 本地明文）
   * 共用这一个组件，是为了让它们的播放行为**不可能**产生差异——
   * 分成三个组件的话，迟早有人只修其中一个。
   */
  plain: { type: Boolean, default: false },
});

const emit = defineEmits(['close', 'external']);

const media = useTemplateRef('media');
const text = ref('');
const mediaError = ref('');

const src = computed(() => {
  if (props.plain) return plainUrl(props.file.id);
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

/** 把媒体错误码翻译成人话。 */
function onMediaError() {
  const code = media.value?.error?.code ?? 0;
  mediaError.value = i18n.te(`media_error_${code}`, i18n.te('preview_failed'));
}

function onKey(e) {
  if (e.key === 'Escape') emit('close');
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
  // 先停掉媒体再移除节点
  const el = media.value;
  if (el && (kind.value === 'video' || kind.value === 'audio')) {
    el.pause?.();
    el.removeAttribute('src');
    el.load?.();
  }
});
</script>

<template>
  <div class="overlay">
    <div class="overlay-bar">
      <span class="title">{{ file.name }}</span>
      <span class="spacer"></span>
      <button class="iconbtn" :aria-label="i18n.t('actions.close')" @click="$emit('close')">
        ✕
      </button>
    </div>
    <div class="overlay-body">
      <div v-if="mediaError" class="overlay-msg">{{ mediaError }}</div>

      <video
        v-else-if="kind === 'video'"
        ref="media"
        :src="src"
        controls
        autoplay
        crossorigin="anonymous"
        playsinline
        preload="metadata"
        @error="onMediaError"
      ></video>

      <audio
        v-else-if="kind === 'audio'"
        ref="media"
        :src="src"
        controls
        autoplay
        crossorigin="anonymous"
        @error="onMediaError"
      ></audio>

      <!-- SVG 也走 <img>：浏览器的受限模式会禁掉其中的脚本。
           绝不用 v-html 插入 SVG——那等于执行未知代码 -->
      <img
        v-else-if="kind === 'image'"
        ref="media"
        :src="src"
        :alt="file.name"
        crossorigin="anonymous"
      />

      <!-- {{ }} 是 textContent 语义，文件内容不会被当成 HTML 解析 -->
      <pre v-else-if="kind === 'text'">{{ text }}</pre>

      <!-- 应用内看不了的类型：给一条出路，而不是一句「不支持」就完事。
           PDF、压缩包、Office 文档都会走到这里（文档 §8 的既定设计） -->
      <div v-else class="overlay-msg">
        <p>{{ i18n.t('playback.cannot_preview') }}</p>
        <button v-if="plain" class="btn primary" @click="$emit('external')">
          📤 {{ i18n.t('file.open_external') }}
        </button>
      </div>
    </div>
  </div>
</template>
