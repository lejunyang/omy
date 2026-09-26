<script setup lang="ts">
/** omy 的统一视频播放器适配层。
 *
 * 这里只包 xgplayer，不处理文件来源。PreviewOverlay 继续负责把本地加密、远端、
 * 明文、容器内、云盘五种来源变成同一种 URL；播放器因此不可能出现「本地能拖、
 * 远端不能拖」的实现分叉。
 *
 * 三条硬约束：
 * 1. 底层 video 必须带 crossorigin=anonymous，否则 canvas 读像素会被污染；
 * 2. 移动交互只认项目的 isMobile，不让第三方库再按 UA 判断一次；
 * 3. 卸载先暂停、清 src、load，再 destroy，避免 WebView 继续持有解密缓冲。
 */

import { onBeforeUnmount, onMounted, watch, useTemplateRef } from 'vue';
import type Player from 'xgplayer';
import { isMobile } from '../viewport';
import * as i18n from '../i18n';

const props = defineProps({
  src: { type: String, required: true },
  autoplay: { type: Boolean, default: true },
});

const emit = defineEmits<{
  error: [code: number];
  play: [];
  pause: [];
}>();

const host = useTemplateRef('host');
let player: Player | null = null;
let mountGeneration = 0;

function mediaElement(): HTMLMediaElement | null {
  return player?.media instanceof HTMLMediaElement ? player.media : null;
}

function mediaErrorCode(): number {
  return mediaElement()?.error?.code ?? 0;
}

function destroyPlayer() {
  mountGeneration += 1;
  const current = player;
  player = null;
  if (!current) return;

  // 不能只依赖 destroy 移除 DOM：xgplayer 的 destroy 不负责清 src，WebView
  // 可能继续持有已解密缓冲，也可能继续向 omystream 请求后续 Range。
  const media = current.media instanceof HTMLMediaElement ? current.media : null;
  if (media) {
    media.pause();
    media.removeAttribute('src');
    media.load();
  }
  current.destroy();
}

async function mountPlayer() {
  if (!host.value || !props.src) return;
  destroyPlayer();
  const generation = ++mountGeneration;
  // 播放器只在打开视频预览时加载；主界面启动与图片/文本预览不需要承担这份体积。
  const [{ default: PlayerCtor, Events }, { createOmyVideoPlayerOptions }] = await Promise.all([
    import('xgplayer'),
    import('../video-player-options'),
    import('xgplayer/dist/index.min.css'),
  ]);
  if (generation !== mountGeneration || !host.value) return;
  player = new PlayerCtor({
    ...createOmyVideoPlayerOptions({
      el: host.value,
      url: props.src,
      mobile: isMobile.value,
      lang: i18n.lang.value,
      rewindLabel: i18n.t('playback.rewind_10'),
      forwardLabel: i18n.t('playback.forward_10'),
    }),
    autoplay: props.autoplay,
  });
  player.on(Events.ERROR, () => emit('error', mediaErrorCode()));
  player.on(Events.PLAY, () => emit('play'));
  player.on(Events.PAUSE, () => emit('pause'));
  player.on(Events.ENDED, () => emit('pause'));
}

onMounted(mountPlayer);

// URL、语言、断点发生变化时重建：xgplayer 的移动插件只在初始化时选择事件模型，
// 单改 class 会留下旧监听器。转屏是低频动作，完整重建比动态拆插件更可靠。
watch([() => props.src, isMobile, i18n.lang], mountPlayer);

onBeforeUnmount(destroyPlayer);
</script>

<template>
  <div ref="host" class="omy-video-player" data-player="xgplayer"></div>
</template>
