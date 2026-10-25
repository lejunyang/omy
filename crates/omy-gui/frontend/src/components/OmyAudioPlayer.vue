<script setup lang="ts">
/** 音频只消费受控 URL；不下载整文件、不生成伪波形，也不因移动端切换另一套逻辑。 */
import { computed, onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue';
import * as i18n from '../i18n';

const props = withDefaults(defineProps<{
  src: string;
  name: string;
  mime?: string;
  encrypted?: boolean;
  autoplay?: boolean;
}>(), { encrypted: true, autoplay: true });
const emit = defineEmits<{ play: []; pause: [] }>();
const media = useTemplateRef<HTMLAudioElement>('media');
const paused = ref(true);
const loading = ref(true);
const errorCode = ref<number | null>(null);
const current = ref(0);
const duration = ref(0);
const speed = ref(1);
const volume = ref(1);
const muted = ref(false);
let generation = 0;
const buffered = ref<Array<{ start: number; end: number }>>([]);
const canSeek = computed(() => duration.value > 0 && errorCode.value === null);
const progress = computed(() => duration.value > 0 ? current.value / duration.value * 100 : 0);
const format = computed(() => props.name.split('.').pop()?.toUpperCase() || i18n.t('kind.audio'));
const status = computed(() => errorCode.value !== null ? i18n.t('audio.failed')
  : loading.value ? i18n.t('audio.buffering') : paused.value ? i18n.t('audio.paused') : i18n.t('audio.playing'));
const message = computed(() => i18n.te(`media_error_${errorCode.value}`, i18n.te('preview_failed')));
const time = (seconds: number) => i18n.formatDuration(Math.max(0, seconds) * 1000);

function sync() {
  const el = media.value;
  if (!el) return;
  current.value = Number.isFinite(el.currentTime) ? el.currentTime : 0;
  duration.value = Number.isFinite(el.duration) && el.duration > 0 ? el.duration : 0;
  paused.value = el.paused;
  volume.value = el.volume;
  muted.value = el.muted;
  speed.value = el.playbackRate;
  const spans: Array<{ start: number; end: number }> = [];
  if (duration.value > 0) {
    for (let i = 0; i < el.buffered.length; i++) {
      spans.push({ start: el.buffered.start(i) / duration.value * 100, end: el.buffered.end(i) / duration.value * 100 });
    }
  }
  buffered.value = spans;
}
function onPlay() { sync(); emit('play'); }
function onPause() { loading.value = false; sync(); emit('pause'); }
function onReady() { loading.value = false; sync(); }
function onError() {
  errorCode.value = media.value?.error?.code ?? 0;
  loading.value = false;
  emit('pause');
}
async function play() {
  const el = media.value;
  if (!el) return;
  const started = generation;
  try { await el.play(); }
  catch (error) {
    // 换源/关闭导致旧 play Promise 拒绝，不能把旧状态写回新的播放会话。
    if (started !== generation) return;
    // 自动播放被系统拒绝不是格式错误；保留播放按钮，等待一次真实用户点击。
    if (error instanceof DOMException && (error.name === 'NotAllowedError' || error.name === 'AbortError')) {
      loading.value = false;
      sync();
      return;
    }
    onError();
  }
}
function toggle() {
  if (media.value?.paused) void play();
  else media.value?.pause();
}
function seekTo(value: number) {
  if (!media.value || !canSeek.value || !Number.isFinite(value)) return;
  media.value.currentTime = Math.max(0, Math.min(duration.value, value));
  sync();
}
function seekInput(event: Event) { seekTo(Number((event.target as HTMLInputElement).value)); }
function changeSpeed(event: Event) {
  if (media.value) media.value.playbackRate = Number((event.target as HTMLSelectElement).value);
  sync();
}
function changeVolume(event: Event) {
  if (media.value) {
    media.value.volume = Number((event.target as HTMLInputElement).value);
    media.value.muted = false;
  }
  sync();
}
function toggleMute() {
  if (media.value) media.value.muted = !media.value.muted;
  sync();
}
function release() {
  generation += 1;
  const el = media.value;
  if (!el) return;
  el.pause();
  el.removeAttribute('src');
  el.load();
  emit('pause');
}
function load() {
  release();
  errorCode.value = null;
  current.value = 0;
  duration.value = 0;
  buffered.value = [];
  paused.value = true;
  loading.value = true;
  if (!media.value) return;
  media.value.src = props.src;
  media.value.load();
  if (props.autoplay) void play();
}
onMounted(load);
watch(() => props.src, load);
onBeforeUnmount(release);
</script>

<template>
  <section class="omy-audio-player" :aria-label="i18n.t('audio.title')">
    <audio id="pv" ref="media" crossorigin="anonymous" preload="metadata"
      @loadedmetadata="sync" @durationchange="sync" @timeupdate="sync" @progress="sync"
      @play="onPlay" @pause="onPause" @ended="onPause" @canplay="onReady" @playing="onReady"
      @waiting="loading = true" @seeking="loading = true" @seeked="onReady"
      @ratechange="sync" @volumechange="sync" @error="onError" />
    <header class="audio-heading">
      <span class="audio-art"><AppIcon name="music" :size="52" /></span>
      <div class="audio-identity">
        <p class="audio-eyebrow">{{ i18n.t('audio.title') }}</p>
        <h2>{{ name }}</h2>
        <p class="audio-format">{{ format }} · {{ duration ? time(duration) : '—' }}</p>
      </div>
    </header>
    <p class="audio-source"><AppIcon :name="encrypted ? 'shield' : 'radio'" :size="16" />
      {{ i18n.t(encrypted ? 'audio.encrypted_hint' : 'audio.direct_hint') }}
    </p>
    <div class="audio-timeline">
      <div class="audio-track" aria-hidden="true">
        <span v-for="(span, i) in buffered" :key="i" class="audio-buffer"
          :style="{ left: `${span.start}%`, width: `${span.end - span.start}%` }" />
        <span class="audio-progress" :style="{ width: `${progress}%` }" />
      </div>
      <input class="audio-seek" type="range" min="0" :max="duration || 1" step="0.1"
        :value="current" :disabled="!canSeek" :aria-label="i18n.t('audio.seek')"
        :aria-valuetext="`${time(current)} / ${duration ? time(duration) : '—'}`" @input="seekInput" />
    </div>
    <div class="audio-time"><span>{{ time(current) }}</span><span>{{ duration ? time(duration) : '—' }}</span></div>
    <div class="audio-transport">
      <button class="audio-jump" :disabled="!canSeek" :aria-label="i18n.t('playback.rewind_10')" @click="seekTo(current - 10)">−10<span>s</span></button>
      <button class="audio-toggle" :disabled="errorCode !== null" :aria-label="i18n.t(paused ? 'audio.play' : 'audio.pause')" @click="toggle">
        <AppIcon :name="paused ? 'play' : 'pause'" :size="28" />
      </button>
      <button class="audio-jump" :disabled="!canSeek" :aria-label="i18n.t('playback.forward_10')" @click="seekTo(current + 10)">+10<span>s</span></button>
    </div>
    <div class="audio-options">
      <label class="audio-rate">{{ i18n.t('audio.speed') }}
        <select :value="speed" @change="changeSpeed"><option v-for="rate in [0.5, 0.75, 1, 1.25, 1.5, 2]" :key="rate" :value="rate">{{ rate }}×</option></select>
      </label>
      <div class="audio-volume">
        <button :aria-label="i18n.t(muted ? 'audio.unmute' : 'audio.mute')" :aria-pressed="muted" @click="toggleMute"><AppIcon :name="muted || volume === 0 ? 'volume-off' : 'volume'" /></button>
        <input type="range" min="0" max="1" step="0.05" :value="muted ? 0 : volume" :aria-label="i18n.t('audio.volume')" @input="changeVolume" />
      </div>
    </div>
    <p class="audio-status" role="status" aria-live="polite">{{ status }}</p>
    <div v-if="errorCode !== null" class="audio-error" role="alert">
      <p>{{ message }}</p><p v-if="errorCode === 3 || errorCode === 4">{{ i18n.t('audio.compatibility_hint') }}</p>
      <button class="btn" @click="load">{{ i18n.t('actions.retry') }}</button>
    </div>
  </section>
</template>

<style scoped>
.omy-audio-player { width: min(100%, 620px); max-height: 100%; overflow: auto; box-sizing: border-box; padding: 32px; background: var(--bg2); color: var(--fg); border: 1px solid var(--border); border-radius: var(--r-l); padding-block-end: max(24px, env(safe-area-inset-bottom, 0px)); }
audio { display: none; }
.audio-heading { display: flex; align-items: center; gap: 20px; }
.audio-art { display: grid; place-items: center; flex: 0 0 96px; height: 96px; border-radius: var(--r-m); background: var(--bg3); color: var(--accent); border: 1px solid var(--border); }
.audio-identity { min-width: 0; }
h2 { margin: 8px 0; font-size: 22px; line-height: 1.4; overflow-wrap: anywhere; }
p { margin: 0; }
.audio-eyebrow, .audio-format, .audio-source, .audio-status { font-size: 14px; color: var(--fg2); }
.audio-source { display: flex; align-items: flex-start; gap: 8px; margin-block: 24px 12px; line-height: 1.6; }
.audio-source .app-icon { margin-block-start: 3px; }
.audio-timeline { position: relative; height: 44px; }
.audio-track { position: absolute; inset: 19px 0 auto; height: 6px; background: var(--border); border-radius: 3px; overflow: hidden; }
.audio-buffer, .audio-progress { position: absolute; inset-block: 0; }
.audio-buffer { background: var(--fg2); opacity: .55; }
.audio-progress { inset-inline-start: 0; background: var(--accent); }
.audio-seek { position: relative; width: 100%; height: 44px; margin: 0; appearance: none; background: transparent; cursor: pointer; }
.audio-seek::-webkit-slider-thumb { appearance: none; width: 16px; height: 16px; border-radius: 50%; background: var(--accent); }
.audio-seek::-moz-range-thumb { width: 16px; height: 16px; border: 0; border-radius: 50%; background: var(--accent); }
.audio-time { display: flex; justify-content: space-between; color: var(--fg2); font-size: 14px; font-variant-numeric: tabular-nums; }
.audio-transport { display: flex; justify-content: center; align-items: center; gap: 28px; margin-block: 24px; }
button { cursor: pointer; color: var(--fg); }
.audio-jump { min-width: 56px; min-height: 44px; font-size: 18px; border: 0; background: transparent; }
.audio-jump span { font-size: 12px; margin-inline-start: 2px; }
.audio-toggle { width: 64px; height: 64px; display: grid; place-items: center; border: 0; border-radius: 50%; color: white; background: var(--accent-solid); }
.audio-options { display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
.audio-rate { display: flex; align-items: center; gap: 12px; font-size: 14px; }
select { min-height: 44px; padding-inline: 12px; border: 1px solid var(--border); border-radius: var(--r-s); color: var(--fg); background: var(--bg3); }
.audio-volume { display: flex; align-items: center; gap: 8px; }
.audio-volume button { width: 44px; height: 44px; background: transparent; border: 0; }
.audio-volume input { width: 100px; height: 44px; accent-color: var(--accent); }
.audio-status { text-align: center; margin-block-start: 16px; }
.audio-error { margin-block-start: 20px; padding-block-start: 16px; border-block-start: 1px solid var(--border); color: var(--danger); font-size: 14px; line-height: 1.6; }
.audio-error p + p { color: var(--fg2); margin-block-start: 8px; }
.audio-error button { margin-block-start: 12px; }
button:disabled, input:disabled { opacity: .55; cursor: default; }
button:focus-visible, input:focus-visible, select:focus-visible { outline: 2px solid var(--accent); outline-offset: 4px; }
@media (max-width: 768px) { .omy-audio-player { padding: 20px; } .audio-art { flex-basis: 64px; height: 64px; } .audio-art .app-icon { width: 36px; height: 36px; } h2 { font-size: 18px; } .audio-heading { gap: 16px; } }
</style>
