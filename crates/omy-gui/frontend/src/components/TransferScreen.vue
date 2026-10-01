<script setup lang="ts">
/**
 * 统一传输管理页：下载 / 上传 / 永久保留三类任务的汇总。
 *
 * # 为什么是一个顶层页而不是画在各个对话里
 *
 * 任务本身是**跨对话**的——用户可能同时在往一个对话传文件、从另一个
 * 缓存视频、又给第三个对话里的文件做永久保留。进度只画在「当前所在的
 * 对话」里的话，他一旦切走就看不到也管不了。
 *
 * 还有一层：这三类任务的失败原因互不相同（限流 / 分片过期 / 空间不足），
 * 只有汇总在一处才看得出「是不是所有任务都卡在同一个原因上」。一次限流
 * 会让全部任务同时进入等待，分散显示时看起来像三个互不相关的故障。
 */
import { computed, onMounted, onBeforeUnmount, ref } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import { state } from '../store';
import AppShell from './AppShell.vue';

const emit = defineEmits([
  'pick', 'devices', 'lang', 'add-place', 'telegram', 'settings',
  'lock', 'quick-unlock', 'files', 'places',
]);

/** 当前筛选：all | active | done。 */
const filter = ref('all');
/** 是否处于「全部暂停」状态。 */
const paused = ref(false);

const tasks = computed(() => state.transfers || []);

function isActive(t) {
  return t.state === 'running' || t.state === 'waiting';
}

const shown = computed(() => {
  const all = tasks.value;
  if (filter.value === 'active') return all.filter(isActive);
  if (filter.value === 'done') return all.filter((t) => !isActive(t));
  return all;
});

/** 按类型分组，空组不渲染——留一个空标题会让人以为加载失败。 */
const groups = computed(() => {
  const order = [
    ['upload', 'xfer.group_upload'],
    ['download', 'xfer.group_download'],
    ['pin', 'xfer.group_pin'],
  ];
  return order
    .map(([kind, label]) => ({
      kind,
      label,
      items: shown.value.filter((t) => t.kind === kind),
    }))
    .filter((g) => g.items.length > 0);
});

const summary = computed(() => {
  const all = tasks.value;
  return {
    running: all.filter((t) => t.state === 'running').length,
    waiting: all.filter((t) => t.state === 'waiting').length,
    failed: all.filter((t) => t.state === 'failed').length,
  };
});

function pct(t) {
  if (!t.total) return 0;
  return Math.min(100, Math.round((t.done / t.total) * 100));
}

/** 状态文案。限流等待要带上秒数——「等待中」和「还要等 30 秒」
 *  对用户的意义完全不同，后者他知道该不该继续等。 */
function stateText(t) {
  if (t.state === 'waiting') {
    return t.until_secs
      ? i18n.t('xfer.st_waiting_secs', { n: t.until_secs })
      : i18n.t('xfer.st_waiting');
  }
  if (t.state === 'failed') {
    // 带上错误码对应的可读文案：只说「失败」用户不知道能不能重试
    return `${i18n.t('xfer.st_failed')} · ${i18n.te(t.code)}`;
  }
  return i18n.t(`xfer.st_${t.state}`);
}

function icon(kind) {
  if (kind === 'upload') return '⬆️';
  if (kind === 'download') return '⬇️';
  return '📌';
}

/** 上一次采样：id -> {done, at}。用来算速度。 */
const lastSample = new Map();
/** 每个任务的速度（字节/秒）。 */
const rates = ref(new Map());

async function refresh() {
  try {
    const rows = await api.transferList();
    // 速度在前端按两次快照算，不在后端存时间戳：界面本来就每隔约 1.2 秒
    // 刷一次，多一个字段只是让后端也要关心「上次是什么时候」
    const now = Date.now();
    const next = new Map();
    for (const t of rows) {
      const prev = lastSample.get(t.id);
      if (prev && now > prev.at && t.done >= prev.done) {
        const bps = ((t.done - prev.done) * 1000) / (now - prev.at);
        // 平滑一下，否则数字每秒乱跳、读不出来
        const old = rates.value.get(t.id) || bps;
        next.set(t.id, old * 0.6 + bps * 0.4);
      } else if (rates.value.has(t.id)) {
        next.set(t.id, rates.value.get(t.id));
      }
      lastSample.set(t.id, { done: t.done, at: now });
    }
    // 结束的任务不再留采样，否则 Map 会一直涨
    for (const id of [...lastSample.keys()]) {
      if (!rows.some((t) => t.id === id)) lastSample.delete(id);
    }
    rates.value = next;
    state.transfers = rows;
  } catch {
    // 查不到就保持上一次的快照，不清空——清空会让界面闪一下空态
  }
}

/** 这个任务当前的速度文本；算不出来就空。 */
function rateText(t) {
  const bps = rates.value.get(t.id);
  if (!bps || t.state !== 'running') return '';
  return i18n.t('xfer.speed', { rate: i18n.formatSize(Math.round(bps)) });
}

/** 有没有任务慢到值得提一句「这是服务端限速」。
 *
 * 阈值取 256 KB/s：Telegram 对非会员的下载限速通常在这个量级以下。
 * 只在**真的在传**且确实慢时才说——无条件显示等于噪音，
 * 而说「你不是会员」是我们无法可靠判断的事，不能替用户下结论。 */
const slowHint = computed(() =>
  (state.transfers || []).some(
    (t) => t.state === 'running' && (rates.value.get(t.id) || 0) > 0
      && (rates.value.get(t.id) || 0) < 256 * 1024,
  ),
);

async function cancel(t) {
  await api.transferCancel(t.id).catch(() => {});
  await refresh();
}

/** 这条失败任务能不能重试。后端在失败态里下发 retryable——界面不自己
 *  按错误码再推一遍，判据只该有一处。 */
function canRetry(t) {
  return t.state === 'failed' && t.retryable === true;
}

/** 重试一条失败任务。是否复用进度由任务自身决定：永久缓存会复用已有分块，
 * 内存中转与 Telegram 目标上传会从头开始。 */
async function retry(t) {
  await api.transferRetry(t.id).catch(() => {});
  await refresh();
}

async function togglePauseAll() {
  paused.value = !paused.value;
  await api.transferPauseAll(paused.value).catch(() => {});
  await refresh();
}

async function clearDone() {
  await api.transferClearDone().catch(() => {});
  await refresh();
}

let timer = null;
onMounted(async () => {
  await refresh();
  // 事件之外再轮询一次兜底：只靠事件的话，错过一条就永远对不上了
  timer = setInterval(refresh, 1200);
});
onBeforeUnmount(() => {
  if (timer) clearInterval(timer);
});
</script>

<template>
  <!-- 活在主界面外壳里，侧栏常驻。整屏互斥替换会让侧栏消失，
       用户进了传输页就切不回文件浏览——这一轮已经因为同样的原因
       修过一次（远程位置那条）。 -->
  <AppShell
    :search="false"
    @pick="emit('pick')"
    @devices="emit('devices')"
    @lang="emit('lang')"
    @add-place="emit('add-place')"
    @telegram="emit('telegram')"
    @settings="emit('settings')"
    @lock="emit('lock')"
    @quick-unlock="emit('quick-unlock')"
    @files="emit('files')"
    @places="emit('places')"
  >
  <div class="xfer main" data-ui="transfers">
    <div class="xhead">
      <div class="seg" data-xf="seg">
        <button
          v-for="f in ['all', 'active', 'done']"
          :key="f"
          :class="{ on: filter === f }"
          :data-xf-f="f"
          @click="filter = f"
        >
          {{ i18n.t(`xfer.filter_${f}`) }}
        </button>
      </div>
      <span class="xsum" data-xf="summary">
        {{ i18n.t('xfer.summary', summary) }}
      </span>
      <span class="spacer"></span>
      <button class="btn small" data-xf="pauseall" @click="togglePauseAll">
        {{ i18n.t(paused ? 'xfer.resume_all' : 'xfer.pause_all') }}
      </button>
      <button class="btn small" data-xf="clrdone" @click="clearDone">
        {{ i18n.t('xfer.clear_done') }}
      </button>
    </div>

    <!-- 非会员限速说明。只在真的慢时出现——无条件显示是噪音。
         不说「你不是会员」：会员身份我们判断不可靠，猜错很尴尬；
         陈述「Telegram 对非会员有限速」这个事实就够了 -->
    <div v-if="slowHint" class="slownote" data-xf="slow">
      {{ i18n.t('xfer.slow_note') }}
    </div>

    <div v-if="!shown.length" class="empty" data-xf="empty">
      <div class="icon" aria-hidden="true">🔀</div>
      <div class="title">{{ i18n.t('xfer.empty') }}</div>
      <div class="d">{{ i18n.t('xfer.empty_hint') }}</div>
    </div>

    <div v-else class="xlist">
      <template v-for="g in groups" :key="g.kind">
        <div class="tskgrp">{{ i18n.t(g.label) }}</div>
        <div
          v-for="t in g.items"
          :key="t.id"
          class="tsk"
          :data-st="t.state"
          :data-xf-task="t.id"
        >
          <span aria-hidden="true">{{ icon(t.kind) }}</span>
          <div class="tskn">
            <div class="tsknm">{{ t.name }}</div>
            <div class="tsksrc">{{ t.target }}</div>
          </div>
          <div class="tskbarwrap">
            <div class="tskbar"><i :style="{ width: pct(t) + '%' }"></i></div>
          </div>
          <div class="tskv" data-xf="state">
            {{ stateText(t) }}<template v-if="rateText(t)">
              · <span data-xf="rate">{{ rateText(t) }}</span></template>
          </div>
          <div class="tskact">
            <button
              v-if="isActive(t)"
              class="btn small"
              data-xf="cancel"
              :title="i18n.t('xfer.act_cancel')"
              @click="cancel(t)"
            >
              ✕
            </button>
            <button
              v-else-if="canRetry(t)"
              class="btn small"
              data-xf="retry"
              :title="i18n.t('xfer.act_retry')"
              @click="retry(t)"
            >
              ↻
            </button>
          </div>
        </div>
      </template>
    </div>
  </div>
  </AppShell>
</template>

<style scoped>
.xfer {
  display: flex;
  flex-direction: column;
  height: 100%;
  overflow: hidden;
}
.xhead {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: calc(var(--sp) * 2) calc(var(--sp) * 3);
  border-bottom: 1px solid var(--border);
  flex: 0 0 auto;
}
.xsum {
  font-size: 12px;
  color: var(--fg2);
}
.spacer {
  flex: 1;
}
.slownote {
  padding: 8px calc(var(--sp) * 3);
  font-size: 12px;
  color: var(--fg2);
  border-bottom: 1px solid var(--border);
}
.xlist {
  overflow: auto;
  flex: 1;
}
.tskgrp {
  padding: calc(var(--sp) * 2) calc(var(--sp) * 3) calc(var(--sp));
  font-size: 11px;
  color: var(--fg2);
  letter-spacing: 0.04em;
}
.tsk {
  display: grid;
  grid-template-columns: 20px minmax(0, 1fr) 160px auto 36px;
  align-items: center;
  gap: 10px;
  padding: calc(var(--sp) * 1.5) calc(var(--sp) * 3);
  border-bottom: 1px solid var(--border);
  font-size: 12px;
}
.tsknm {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tsksrc {
  color: var(--fg2);
  font-size: 11px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.tskbar {
  height: 4px;
  border-radius: 2px;
  background: var(--bg3);
  overflow: hidden;
}
.tskbar i {
  display: block;
  height: 100%;
  background: var(--accent);
  transition: width 0.2s;
}
.tskv {
  color: var(--fg2);
  white-space: nowrap;
}
/* 失败要看得出来，但不用整行变红——那会让一排失败的任务糊成一片 */
.tsk[data-st='failed'] .tskv {
  color: var(--danger);
}
.tsk[data-st='done'] .tskbar i {
  background: var(--ok);
}
.tsk[data-st='waiting'] .tskbar i {
  background: var(--warn);
}
.empty {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  color: var(--fg2);
}
.empty .icon {
  font-size: 30px;
}
.empty .title {
  color: var(--fg);
}

/* 移动端：160px 的进度列挤不下，换成两行 */
@media (max-width: 768px) {
  .tsk {
    grid-template-columns: 20px minmax(0, 1fr) auto;
    grid-template-areas:
      'ico name act'
      'bar bar bar';
    row-gap: 6px;
  }
  .tskbarwrap {
    grid-area: bar;
  }
  .tskv {
    grid-area: bar;
    text-align: end;
    font-size: 11px;
  }
}
</style>
