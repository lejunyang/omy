<script setup lang="ts">
/** 视频处理对话框。
 *
 * 取代原先加密对话框里那个孤零零的「取帧时间点」输入框：这里能看着画面
 * 选封面帧，也能在加密前把视频转成更通用的容器。
 *
 * # 三件必须知道的事
 *
 * 1. **可选项由能力检测决定，不写死。** 内置的 FFmpeg 是按「探测 / 抽帧 /
 *    转封装」裁剪的，一个视频编码器都没有，所以「压缩」默认不可用；但用户
 *    随时可以换成完整版（`OMY_FFMPEG` 排在查找顺序最前）。写死成任一答案
 *    都会错，所以进来先问后端要 caps。
 *
 * 2. **预览走 `omystream://plain/<token>`，不是文件路径。** 直接用路径需要
 *    开启 Tauri 的 asset 协议，那等于把整个文件系统暴露给 WebView 里的 JS。
 *    `crossorigin="anonymous"` 不能省——页面与协议不同源，不声明的话画布
 *    会被污染，任何读像素的操作都抛 SecurityError（详见 PreviewOverlay）。
 *
 * 3. **进度条、时间码、画面是同一个值的三个视图。** 改任意一个其余跟着走，
 *    所以没有「取当前画面」这种确认按钮——有它反而暗示三者可能不同步。
 */

import { ref, computed, watch, onMounted, onBeforeUnmount, useTemplateRef } from 'vue';
import { playing as mediaActive } from '../autolock';
import * as i18n from '../i18n';
import * as api from '../api';
import { plainUrl } from '../store';

const props = defineProps({
  /** 要处理的条目，需含 `token`（由 browse_directory 下发）与 `name`。 */
  entry: { type: Object, required: true },
  /** 已经选好的封面帧秒数，`null` 表示自动。 */
  frame: { type: Number, default: null },
});

const emit = defineEmits(['cancel', 'apply']);

const video = useTemplateRef('video');

/* ---------------- 状态 ---------------- */

const caps = ref(null);
const info = ref(null);
const loadError = ref('');
const tab = ref('frame');

/** 当前时间点（秒）。空串表示「自动取十分之一处」。 */
const at = ref(props.frame ?? 0);
/** 时间码输入框的文本。与 `at` 联动，但允许中途是非法值。 */
const frameText = ref('');
/** 用户是否显式选过帧。没选过就提交「自动」，而不是提交 0——
 *  那两者不一样：0 是片头黑帧，自动是十分之一处。 */
const framePicked = ref(props.frame != null);
const playing = ref(false);
const duration = ref(0);

/** 处理方式：`none` / `remux` / `compress`。 */
const proc = ref('none');
const container = ref('mp4');
const faststart = ref(true);
const encoder = ref('');
const crf = ref(23);
const height = ref(0);
const fps = ref(0);

/** 字幕怎么办：`auto` 按容器能力处理，`keep_style` 保样式（要用 MKV），
 *  `drop` 完全不要。
 *
 * 默认 auto：目标容器是用户先选的，为字幕样式擅自改容器会让
 * 「我选的 MP4 怎么变成 MKV 了」更费解。代价写在选项文案里。
 */
const subMode = ref('auto');

const converting = ref(false);
const convertError = ref('');
/** 转换产物。非空时「继续加密」用它替代原文件。 */
const converted = ref(null);

/* ---------------- 载入 ---------------- */

const src = computed(() => (props.entry?.token ? plainUrl(props.entry.token) : ''));

onMounted(async () => {
  try {
    caps.value = await api.videoCapabilities(false);
  } catch {
    // 探测失败等价于「什么都不能做」，不是错误状态：
    // 界面照常显示，只是处理方式里只剩「不处理」
    caps.value = { video_encoders: [], audio_encoders: [], muxers: [], has_ffmpeg: false };
  }
  if (caps.value.video_encoders?.length) {
    encoder.value = caps.value.video_encoders[0];
  }
  try {
    info.value = await api.videoInfo(props.entry.token);
  } catch (e) {
    loadError.value = i18n.te(api.errCode(e));
  }
});

onBeforeUnmount(() => {
  // 不清空 src 的话，组件卸载后 WebView 可能继续向协议请求数据
  // （与 PreviewOverlay 同一条约束）
  if (video.value) {
    video.value.pause();
    video.value.removeAttribute('src');
    video.value.load();
  }
  // 关了窗口必须清掉活跃标记，否则自动锁定永远不触发，
  // 那个功能就默默失效了
  mediaActive.value = false;
});

/** 把本地播放状态同步给自动锁定。
 *
 * 跟随 `playing` 而不是在挂载时一律置真：这个对话框打开后用户可能只是
 * 在调编码参数，并没有真的播放。只有真在播的时候才该阻止锁定。
 */
watch(playing, (v) => {
  mediaActive.value = v;
});

/* ---------------- 时间码 ---------------- */

/** 把 `1:23.5` / `83.5` 解析成秒。非法或空串返回 null。
 *
 * 不能用 parseFloat 直接吞：`parseFloat('1:23')` 得 1，会把「1 分 23 秒」
 * 静默变成第 1 秒——用户拿到的缩略图不是他选的那一帧，却没有任何提示。
 * 这段逻辑与后端 `encrypt.rs` 的取帧解析对应，改动时两边都要看。
 */
function parseFrame(s) {
  const v = (s || '').trim();
  if (!v) return null;
  const parts = v.split(':');
  if (parts.length > 3) return null;
  let sec = 0;
  for (const p of parts) {
    if (!/^\d*\.?\d*$/.test(p) || p === '' || p === '.') return null;
    sec = sec * 60 + parseFloat(p);
  }
  if (!Number.isFinite(sec) || sec < 0) return null;
  return sec;
}

/** 秒 → `00:00:12.40`。 */
function fmtTime(sec) {
  const s = Math.max(0, Number(sec) || 0);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${r.toFixed(2).padStart(5, '0')}`;
}

const frameInvalid = computed(
  () => frameText.value.trim().length > 0 && parseFrame(frameText.value) === null,
);

/** 用户敲时间码 → 视频跟着跳。 */
function onFrameText() {
  const v = parseFrame(frameText.value);
  if (v === null) return;
  framePicked.value = true;
  seekTo(v);
}

/** 拖进度条 / 点逐帧 → 输入框跟着变。 */
function seekTo(sec) {
  const d = duration.value || 0;
  const t = Math.min(d > 0 ? d : sec, Math.max(0, sec));
  at.value = t;
  framePicked.value = true;
  if (video.value && Number.isFinite(t)) {
    video.value.currentTime = t;
  }
  frameText.value = fmtTime(t);
}

/** 视频自己播到某处 → 同步显示。
 *
 * 这里**不置 framePicked**：播放经过某一帧不等于用户选了它。
 * 否则点一下播放就等于选了个随机帧，而用户以为还是自动。
 */
function onTimeUpdate() {
  if (!video.value) return;
  at.value = video.value.currentTime;
  if (framePicked.value) frameText.value = fmtTime(at.value);
}

function onLoadedMeta() {
  if (!video.value) return;
  duration.value = video.value.duration || 0;
  if (props.frame != null) {
    seekTo(props.frame);
  } else {
    // 预览默认停在十分之一处——与「自动取帧」的实际行为一致，
    // 让用户看到的就是不选时会得到的那一帧
    const t = duration.value * 0.1;
    at.value = t;
    if (Number.isFinite(t)) video.value.currentTime = t;
  }
}

function togglePlay() {
  if (!video.value) return;
  if (video.value.paused) {
    video.value.play().catch(() => {});
    playing.value = true;
  } else {
    video.value.pause();
    playing.value = false;
  }
}

/** 一帧的时长。拿不到帧率时按 30fps 估。 */
const frameStep = computed(() => {
  const fr = info.value?.frame_rate;
  if (typeof fr === 'string' && fr.includes('/')) {
    const [n, d] = fr.split('/').map(Number);
    if (n > 0 && d > 0) return d / n;
  }
  return 1 / 30;
});

/* ---------------- 能力 ---------------- */

const canCompress = computed(() => (caps.value?.video_encoders?.length ?? 0) > 0);
const canAac = computed(() => (caps.value?.audio_encoders ?? []).includes('aac'));

/** 转封装到 MP4 会不会丢音轨。
 *
 * 用后端算好的结论，不在前端按编码名重新判断一遍——两处实现迟早分叉，
 * 而分叉的后果是界面说会保留、实际丢了声音。
 */
const audioWillDrop = computed(
  () => container.value !== 'mkv' && info.value?.mp4_audio_plan === 'drop',
);

/* ---------------- 字幕 ---------------- */

const hasSubs = computed(() => (info.value?.subtitle_count ?? 0) > 0);
const hasBitmapSubs = computed(() => info.value?.has_bitmap_subtitles === true);
const hasStyledSubs = computed(() => info.value?.has_styled_subtitles === true);
const targetIsMkv = computed(() => container.value === 'mkv');

/** 字幕最终会怎样。
 *
 * 这段逻辑与后端 `plan_subtitles` 对应，两边都要看——这里算的是**说给
 * 用户听的话**，真正执行以后端为准（它按实际探测到的轨道算）。
 * 之所以前端也要算一遍，是因为用户改选项时文案要立刻跟着变，
 * 不能每次都往返一次后端。
 */
const subOutcome = computed(() => {
  if (!hasSubs.value) return 'none';
  if (subMode.value === 'drop') return 'drop';
  // MKV 什么都装得下
  if (targetIsMkv.value) return 'copy';
  // 位图字幕进 MP4 无解：既装不进去也转不成文本
  if (hasBitmapSubs.value) return 'bitmap_drop';
  if (hasStyledSubs.value) {
    return subMode.value === 'keep_style' ? 'style_drop' : 'to_mov_text_lossy';
  }
  return 'to_mov_text';
});

/** 只在「有 ASS 且目标不是 MKV」时才给这个选项。
 *
 * 对只有 srt 的文件显示「要不要保留 ASS 样式」只会让人困惑——
 * 他根本没有 ASS 字幕。
 */
const showAssChoice = computed(
  () => hasSubs.value && hasStyledSubs.value && !hasBitmapSubs.value && !targetIsMkv.value,
);

/** 选了「保样式」却仍停在 MP4 上时，引导改选 MKV。 */
function switchToMkv() {
  container.value = 'mkv';
}

/** 可选的目标容器，按后端报告的 muxer 过滤。 */
const containers = computed(() => {
  const m = caps.value?.muxers ?? [];
  return [
    { id: 'mp4', ok: m.includes('mp4') },
    { id: 'mov', ok: m.includes('mov') },
    { id: 'mkv', ok: m.includes('matroska') },
  ].filter((x) => x.ok);
});

/** 能力不足时不能停在「压缩」上。 */
watch(canCompress, (v) => {
  if (!v && proc.value === 'compress') proc.value = 'none';
});

async function recheck() {
  caps.value = await api.videoCapabilities(true);
  if (caps.value.video_encoders?.length && !encoder.value) {
    encoder.value = caps.value.video_encoders[0];
  }
}

/* ---------------- 展示 ---------------- */

function fmtSize(n) {
  return n == null ? '—' : i18n.formatSize(n);
}

const resolution = computed(() =>
  info.value?.width && info.value?.height ? `${info.value.width}×${info.value.height}` : null,
);

/** 帧率的可读值：`30000/1001` → `29.97`。 */
const fpsText = computed(() => {
  const fr = info.value?.frame_rate;
  if (typeof fr !== 'string' || !fr.includes('/')) return null;
  const [n, d] = fr.split('/').map(Number);
  if (!(n > 0 && d > 0)) return null;
  const v = n / d;
  return Number.isInteger(v) ? String(v) : v.toFixed(2);
});

const videoLine = computed(() => {
  if (!info.value?.video_codec) return null;
  return [
    info.value.video_codec,
    info.value.profile ? `(${info.value.profile})` : null,
    resolution.value,
    fpsText.value ? `${fpsText.value} fps` : null,
  ]
    .filter(Boolean)
    .join(' · ');
});

const audioLine = computed(() => {
  if (!info.value?.audio_codec) return null;
  return [
    info.value.audio_codec,
    info.value.sample_rate ? `${Math.round(info.value.sample_rate / 1000)} kHz` : null,
    info.value.audio_layout,
  ]
    .filter(Boolean)
    .join(' · ');
});

/* ---------------- 执行 ---------------- */

async function runConvert() {
  convertError.value = '';
  converting.value = true;
  try {
    const req = {
      token: props.entry.token,
      container: container.value,
      faststart: faststart.value,
      // 两个字幕开关分开传：ass_to_text 是「样式还是兼容性」的取舍，
      // drop_subtitles 是「要不要字幕」。合成一个枚举的话，后端得重新
      // 拆解，而拆解规则又要和前端保持一致
      ass_to_text: subMode.value !== 'keep_style',
      drop_subtitles: subMode.value === 'drop',
      encode:
        proc.value === 'compress'
          ? {
              encoder: encoder.value,
              crf: crf.value,
              height: height.value > 0 ? height.value : null,
              fps: fps.value > 0 ? fps.value : null,
            }
          : null,
    };
    converted.value = await api.convertVideo(req);
  } catch (e) {
    convertError.value = i18n.te(api.errCode(e));
  } finally {
    converting.value = false;
  }
}

/** 丢弃已产生的中间文件。
 *
 * 取消时必须做：不做的话用户的视频目录里会堆满 `.omytmp-` 文件，
 * 而它们可能各有几个 GB。用产物自己的 token，不是源文件的。
 */
async function discard() {
  const tk = converted.value?.token;
  if (!tk) return;
  try {
    await api.discardConverted(tk);
  } catch {
    // 删不掉不该阻塞关闭对话框——文件名带 .omytmp- 前缀，用户能认出来
  }
  converted.value = null;
}

async function cancel() {
  await discard();
  emit('cancel');
}

function apply() {
  emit('apply', {
    /** 封面帧秒数，null 表示自动。 */
    frame: framePicked.value ? at.value : null,
    /** 转换产物路径，null 表示用原文件。 */
    convertedPath: converted.value?.path ?? null,
  });
}

const busy = computed(() => converting.value);
const needsConvert = computed(() => proc.value !== 'none' && !converted.value);
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="cancel">
    <div class="dlg vdlg" role="dialog" :aria-label="i18n.t('video.title')">
      <div class="vdlg-head">
        <h3>{{ i18n.t('video.title') }}</h3>
        <span class="vdlg-sub" :title="entry.name">{{ entry.name }}</span>
        <span class="spacer"></span>
        <div class="tabs" role="tablist">
          <button
            class="tab"
            type="button"
            role="tab"
            :aria-selected="tab === 'frame'"
            @click="tab = 'frame'"
          >
            {{ i18n.t('video.tab_frame') }}
          </button>
          <button
            class="tab"
            type="button"
            role="tab"
            :aria-selected="tab === 'proc'"
            @click="tab = 'proc'"
          >
            {{ i18n.t('video.tab_proc') }}
          </button>
        </div>
      </div>

      <div class="vdlg-body">
        <div v-if="loadError" class="warnbox">{{ loadError }}</div>

        <!-- ============ 封面帧 ============ -->
        <section v-show="tab === 'frame'">
          <div class="vstage">
            <!-- crossorigin 不能省，理由见组件头部注释 -->
            <video
              ref="video"
              :src="src"
              crossorigin="anonymous"
              preload="metadata"
              playsinline
              @loadedmetadata="onLoadedMeta"
              @timeupdate="onTimeUpdate"
              @pause="playing = false"
              @play="playing = true"
            ></video>
            <div class="vbadge">{{ fmtTime(at) }}</div>
          </div>

          <div class="vscrub">
            <button class="btn icon" type="button" :title="i18n.t('video.play')" @click="togglePlay">
              {{ playing ? '❚❚' : '▶' }}
            </button>
            <input
              class="slider"
              type="range"
              min="0"
              :max="duration || 0"
              step="0.04"
              :value="at"
              :aria-label="i18n.t('video.seek')"
              @input="seekTo(Number(($event.target as HTMLInputElement).value))"
            />
            <span class="vtc">{{ fmtTime(at) }} / {{ fmtTime(duration) }}</span>
          </div>

          <div class="vframe-row">
            <div class="field">
              <label class="flabel" for="v-frame">{{ i18n.t('video.frame_at') }}</label>
              <input
                id="v-frame"
                v-model="frameText"
                type="text"
                :placeholder="i18n.t('video.frame_placeholder')"
                @input="onFrameText"
              />
            </div>
            <div class="vsteps">
              <button class="btn" type="button" @click="seekTo(at - frameStep)">
                {{ i18n.t('video.prev_frame') }}
              </button>
              <button class="btn" type="button" @click="seekTo(at + frameStep)">
                {{ i18n.t('video.next_frame') }}
              </button>
            </div>
          </div>

          <div v-if="frameInvalid" class="ferr">{{ i18n.t('encrypt.thumbnail_frame_bad') }}</div>
          <div v-else class="note if">{{ i18n.t('video.frame_hint') }}</div>
        </section>

        <!-- ============ 转换与压缩 ============ -->
        <section v-show="tab === 'proc'">
          <div v-if="info" class="field">
            <div class="flabel">{{ i18n.t('video.source_info') }}</div>
            <table class="vinfo">
              <tbody>
                <tr>
                  <th>{{ i18n.t('video.container') }}</th>
                  <td>{{ info.container_long || info.container }}</td>
                </tr>
                <tr v-if="videoLine">
                  <th>{{ i18n.t('video.video_track') }}</th>
                  <td>{{ videoLine }}</td>
                </tr>
                <tr v-if="audioLine">
                  <th>{{ i18n.t('video.audio_track') }}</th>
                  <td>{{ audioLine }}</td>
                </tr>
                <tr>
                  <th>{{ i18n.t('video.duration') }}</th>
                  <td>{{ info.duration_ms ? fmtTime(info.duration_ms / 1000) : '—' }}</td>
                </tr>
                <tr>
                  <th>{{ i18n.t('video.size') }}</th>
                  <td>{{ fmtSize(info.size) }}</td>
                </tr>
                <tr>
                  <th>{{ i18n.t('video.tier') }}</th>
                  <td>
                    <span class="tierbadge" :class="`tier-${info.tier}`">
                      {{ i18n.t(`video.tier_${info.tier}`) }}
                    </span>
                    <div v-if="info.tier_reason" class="d">{{ info.tier_reason }}</div>
                  </td>
                </tr>
              </tbody>
            </table>
          </div>

          <div class="hr"></div>

          <div class="field">
            <div class="flabel">{{ i18n.t('video.how') }}</div>

            <label class="vopt" :class="{ sel: proc === 'none' }">
              <input v-model="proc" type="radio" value="none" />
              <span>
                <span class="vopt-t">{{ i18n.t('video.none') }}</span>
                <div class="d">{{ i18n.t('video.none_desc') }}</div>
              </span>
            </label>

            <label class="vopt" :class="{ sel: proc === 'remux' }">
              <input v-model="proc" type="radio" value="remux" />
              <span>
                <span class="vopt-t">{{ i18n.t('video.remux') }}</span>
                <div class="d">{{ i18n.t('video.remux_desc') }}</div>
              </span>
            </label>

            <label class="vopt" :class="{ sel: proc === 'compress', disabled: !canCompress }">
              <input v-model="proc" type="radio" value="compress" :disabled="!canCompress" />
              <span>
                <span class="vopt-t">
                  {{ i18n.t('video.compress') }}
                  <span v-if="!canCompress" class="pill no">{{ i18n.t('video.unavailable') }}</span>
                </span>
                <div class="d">
                  {{ canCompress ? i18n.t('video.compress_desc') : i18n.t('video.compress_blocked') }}
                </div>
              </span>
            </label>
          </div>

          <!-- 转封装子选项 -->
          <div v-if="proc === 'remux'" class="vsub">
            <div class="field">
              <label class="flabel" for="v-container">{{ i18n.t('video.target_container') }}</label>
              <select id="v-container" v-model="container" data-container>
                <option v-for="c in containers" :key="c.id" :value="c.id">
                  {{ i18n.t(`video.container_${c.id}`) }}
                </option>
              </select>
            </div>
            <label v-if="container !== 'mkv'" class="radio">
              <input v-model="faststart" type="checkbox" />
              <span>
                {{ i18n.t('video.faststart') }}
                <div class="d">{{ i18n.t('video.faststart_desc') }}</div>
              </span>
            </label>
            <!-- 丢音轨这件事必须在动手之前说，不能等产物出来才发现没声音 -->
            <div v-if="audioWillDrop" class="warnbox">
              {{ i18n.t('video.audio_drop_warn', { codec: info?.audio_codec || '' }) }}
            </div>
            <!-- 字幕：按实际情况说话，而不是一句写死的「会丢弃字幕」。
                 没有字幕时整段不显示——对一个没字幕的文件解释字幕怎么处理
                 只会让人以为自己漏看了什么 -->
            <div v-if="hasSubs" class="field vsubs">
              <div class="flabel">
                {{ i18n.t('video.subtitles', { count: info.subtitle_count }) }}
              </div>

              <!-- 位图字幕：没有选项可给，只能说清楚并提供改用 MKV 的出路 -->
              <div v-if="hasBitmapSubs && !targetIsMkv" class="warnbox">
                {{ i18n.t('video.sub_bitmap_warn') }}
                <div class="vcaps-acts">
                  <button class="btn" type="button" @click="switchToMkv">
                    {{ i18n.t('video.switch_to_mkv') }}
                  </button>
                </div>
              </div>

              <!-- ASS：唯一交给用户决定的取舍 -->
              <template v-else-if="showAssChoice">
                <label class="radio">
                  <input v-model="subMode" type="radio" value="auto" />
                  <span>
                    {{ i18n.t('video.sub_to_text') }}
                    <div class="d">{{ i18n.t('video.sub_to_text_desc') }}</div>
                  </span>
                </label>
                <label class="radio">
                  <input v-model="subMode" type="radio" value="keep_style" />
                  <span>
                    {{ i18n.t('video.sub_keep_style') }}
                    <div class="d">{{ i18n.t('video.sub_keep_style_desc') }}</div>
                  </span>
                </label>
                <label class="radio">
                  <input v-model="subMode" type="radio" value="drop" />
                  <span>{{ i18n.t('video.sub_drop') }}</span>
                </label>
                <!-- 选了保样式却还停在 MP4 上：此时字幕其实会被丢掉，
                     必须说出来并给出一键切换 -->
                <div v-if="subOutcome === 'style_drop'" class="warnbox">
                  {{ i18n.t('video.sub_style_needs_mkv') }}
                  <div class="vcaps-acts">
                    <button class="btn" type="button" @click="switchToMkv">
                      {{ i18n.t('video.switch_to_mkv') }}
                    </button>
                  </div>
                </div>
              </template>

              <!-- 其余情况没有取舍可做，只说结果 -->
              <div v-else class="note if">
                {{ i18n.t(`video.sub_outcome_${subOutcome}`) }}
              </div>
            </div>
          </div>
          <!-- 压缩子选项 -->
          <div v-if="proc === 'compress'" class="vsub">
            <div class="vgrid2">
              <div class="field">
                <label class="flabel" for="v-res">{{ i18n.t('video.resolution') }}</label>
                <select id="v-res" v-model.number="height">
                  <option :value="0">{{ i18n.t('video.keep_original') }}</option>
                  <option :value="1080">1080p</option>
                  <option :value="720">720p</option>
                  <option :value="480">480p</option>
                </select>
              </div>
              <div class="field">
                <label class="flabel" for="v-fps">{{ i18n.t('video.fps') }}</label>
                <select id="v-fps" v-model.number="fps">
                  <option :value="0">{{ i18n.t('video.keep_original') }}</option>
                  <option :value="30">30</option>
                  <option :value="24">24</option>
                </select>
              </div>
              <div class="field">
                <label class="flabel" for="v-enc">{{ i18n.t('video.encoder') }}</label>
                <select id="v-enc" v-model="encoder">
                  <option v-for="e in caps?.video_encoders || []" :key="e" :value="e">{{ e }}</option>
                </select>
              </div>
              <div class="field">
                <label class="flabel" for="v-crf">{{ i18n.t('video.quality') }}</label>
                <select id="v-crf" v-model.number="crf">
                  <option :value="18">{{ i18n.t('video.crf_high') }}</option>
                  <option :value="23">{{ i18n.t('video.crf_balanced') }}</option>
                  <option :value="28">{{ i18n.t('video.crf_small') }}</option>
                </select>
              </div>
            </div>
            <div class="note if">{{ i18n.t('video.compress_slow') }}</div>
          </div>

          <!-- 能力不足的说明与出路 -->
          <div v-if="!canCompress" class="note if vcaps">
            {{ i18n.t('video.caps_note') }}
            <div class="vcaps-acts">
              <button class="btn" type="button" @click="recheck">
                {{ i18n.t('video.recheck') }}
              </button>
            </div>
          </div>

          <div v-if="convertError" class="warnbox">{{ convertError }}</div>

          <div v-if="converted" class="note ok">
            {{
              i18n.t('video.converted', {
                from: fmtSize(converted.original_size),
                to: fmtSize(converted.size),
              })
            }}
            <div v-if="converted.audio_plan === 'drop'" class="d">
              {{ i18n.t('video.converted_no_audio') }}
            </div>
            <!-- 字幕的实际结局以后端返回为准：用户选了「转文本」但源里是
                 位图时仍然只能丢，这里要说真话而不是复述他的选择 -->
            <div v-if="converted.subtitle_plan === 'drop'" class="d">
              {{ i18n.t('video.converted_no_subs') }}
            </div>
            <div v-else-if="converted.subtitle_plan === 'to_mov_text'" class="d">
              {{ i18n.t('video.converted_subs_text') }}
            </div>
            <ul v-if="converted.warnings?.length" class="vwarns">
              <li v-for="(w, i) in converted.warnings" :key="i">{{ w }}</li>
            </ul>
          </div>
        </section>
      </div>

      <div class="vdlg-foot">
        <span class="vfoot-note">
          {{
            framePicked
              ? i18n.t('video.foot_frame', { time: fmtTime(at) })
              : i18n.t('video.foot_frame_auto')
          }}
        </span>
        <span class="spacer"></span>
        <button class="btn" type="button" :disabled="busy" @click="cancel">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button
          v-if="needsConvert"
          class="btn primary"
          type="button"
          :disabled="busy"
          @click="runConvert"
        >
          {{ busy ? i18n.t('video.converting') : i18n.t('video.run') }}
        </button>
        <button v-else class="btn primary" type="button" :disabled="busy" @click="apply">
          {{ i18n.t('video.apply') }}
        </button>
      </div>
    </div>
  </div>
</template>
