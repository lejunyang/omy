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
import { isAndroid, requestVideoLandscape } from '../mobile-platform';
import { generateTimelineThumbnail, type TimelineThumbnail } from '../timeline-thumbnail';
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
let thumbnailAbort: AbortController | null = null;

function mediaElement(): HTMLMediaElement | null {
  return player?.media instanceof HTMLMediaElement ? player.media : null;
}

function mediaErrorCode(): number {
  return mediaElement()?.error?.code ?? 0;
}

function destroyPlayer() {
  mountGeneration += 1;
  thumbnailAbort?.abort();
  thumbnailAbort = null;
  if (isAndroid) requestVideoLandscape(false);
  host.value?.removeAttribute('data-timeline-thumbnail');
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

function installTimelineThumbnail(current: Player, thumbnail: TimelineThumbnail) {
  const thumbnailPlugin = current.getPlugin('thumbnail');
  thumbnailPlugin?.setConfig(thumbnail);
  // 两端插件在初始化时只会注册已有的缩略图，运行时生成完成后要显式补注册。
  current.getPlugin('progresspreview')?.registerThumbnail(thumbnail);
  current.getPlugin('mobile')?.registerThumbnail();
  host.value?.setAttribute('data-timeline-thumbnail', 'ready');
}

async function prepareTimelineThumbnail(current: Player, generation: number) {
  const abort = new AbortController();
  thumbnailAbort?.abort();
  thumbnailAbort = abort;
  try {
    const thumbnail = await generateTimelineThumbnail(props.src, abort.signal);
    if (!thumbnail || abort.signal.aborted || generation !== mountGeneration || player !== current) return;
    installTimelineThumbnail(current, thumbnail);
  } catch (error) {
    // 不支持随机 seek 的媒体或临时网络失败只关闭画面预览，播放本身不应失败。
    if (!(error instanceof DOMException && error.name === 'AbortError')) {
      console.debug('[omy] timeline thumbnail unavailable', error);
    }
  } finally {
    if (thumbnailAbort === abort) thumbnailAbort = null;
  }
}

async function mountPlayer() {
  if (!host.value || !props.src) return;
  destroyPlayer();
  const generation = ++mountGeneration;
  const touchMode = isAndroid || isMobile.value;
  if (isAndroid) requestVideoLandscape(true);
  // 播放器只在打开视频预览时加载；主界面启动与图片/文本预览不需要承担这份体积。
  const [{ default: PlayerCtor, Events }, { createOmyVideoPlayerOptions }] = await Promise.all([
    import('xgplayer'),
    import('../video-player-options'),
    import('xgplayer/dist/index.min.css'),
  ]);
  if (generation !== mountGeneration || !host.value) return;
  const current = new PlayerCtor({
    ...createOmyVideoPlayerOptions({
      el: host.value,
      url: props.src,
      mobile: touchMode,
      lang: i18n.lang.value,
      rewindLabel: i18n.t('playback.rewind_10'),
      forwardLabel: i18n.t('playback.forward_10'),
    }),
    autoplay: props.autoplay,
  });
  player = current;
  current.on(Events.ERROR, () => emit('error', mediaErrorCode()));
  current.on(Events.PLAY, () => emit('play'));
  current.on(Events.PAUSE, () => emit('pause'));
  current.on(Events.ENDED, () => emit('pause'));
  // Android WebView/设备常只给应用一个硬件视频解码器。主播放器尚未进入
  // canplay 就再开隐藏 video 抽帧，会让两边互相等，表现为手机本地 MP4 永远转圈。
  // 等主视频能播之后再做低优先级抽帧；桌面端也复用同一时序，减少无谓竞争。
  current.once(Events.CANPLAY, () => {
    if (generation === mountGeneration && player === current) {
      window.setTimeout(() => void prepareTimelineThumbnail(current, generation), 300);
    }
  });
}

onMounted(mountPlayer);

// URL、语言、桌面端断点发生变化时重建。Android 自动转横屏会改变视口宽度，
// 但它仍然必须保持 touch 模式；若继续监听 isMobile 会先销毁（恢复竖屏）再重建，
// 形成方向振荡。Android 因此用稳定的 true 作为事件模型来源。
watch([() => props.src, () => (isAndroid ? true : isMobile.value), i18n.lang], mountPlayer);

onBeforeUnmount(destroyPlayer);
</script>

<template>
  <div ref="host" class="omy-video-player" data-player="xgplayer"></div>
</template>
