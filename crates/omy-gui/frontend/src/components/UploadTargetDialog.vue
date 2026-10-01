<script setup lang="ts">
/** “上传至”目标选择器。
 *
 * 位置级 caps 只是上界；每进入一个目录都必须重新查询 effective_capabilities。
 * Telegram 的根是对话列表，根本身不可写，用户需进入具体对话后再确认。
 */
import { computed, onMounted, ref } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import { state, cancelUploadTo, confirmUploadTo } from '../store';
import type { RemoteEntry } from '../types';

interface TargetPlace {
  id: string;
  name: string;
  kind: string;
  caps?: Record<string, boolean>;
}

const places = computed<TargetPlace[]>(() =>
  (state.remotePlaces || []).filter((place) => place?.caps?.write),
);
const placeId = ref('');
const dir = ref('');
const trail = ref<{ id: string; name: string }[]>([]);
const rows = ref<RemoteEntry[]>([]);
const caps = ref<Record<string, boolean> | null>(null);
const busy = ref(false);
const loading = ref(false);
const error = ref('');
const mode = ref<'permanent_cache' | 'memory'>('permanent_cache');
/** 每次目录加载递增；旧请求晚到时不得覆盖用户已经切换到的新目标。 */
let loadSeq = 0;

const pending = computed(() => state.uploadTo);
const remoteSource = computed(() => pending.value?.source === 'remote');
const currentPlace = computed(() => places.value.find((place) => place.id === placeId.value));
const canConfirm = computed(() => caps.value?.write === true && !loading.value && !busy.value);

async function openPlace(place: TargetPlace) {
  placeId.value = place.id;
  dir.value = '';
  trail.value = [];
  await load();
}

async function load() {
  if (!placeId.value) return;
  const seq = ++loadSeq;
  const loadingPlace = placeId.value;
  const loadingDir = dir.value;
  loading.value = true;
  error.value = '';
  caps.value = null;
  try {
    const [items, effective] = await Promise.all([
      api.remoteBrowse(loadingPlace, loadingDir),
      api.remoteEffectiveCaps(loadingPlace, loadingDir),
    ]);
    if (seq !== loadSeq || placeId.value !== loadingPlace || dir.value !== loadingDir) return;
    rows.value = (items || []).filter((entry) => entry.is_dir);
    caps.value = effective;
  } catch (e) {
    if (seq !== loadSeq || placeId.value !== loadingPlace || dir.value !== loadingDir) return;
    rows.value = [];
    error.value = i18n.te(api.errCode(e), i18n.t('upload_to.load_failed'));
  } finally {
    if (seq === loadSeq) loading.value = false;
  }
}

async function enter(entry: RemoteEntry) {
  trail.value.push({ id: dir.value, name: entry.name });
  dir.value = entry.id;
  await load();
}

async function up() {
  const previous = trail.value.pop();
  if (!previous) return;
  dir.value = previous.id;
  await load();
}

async function submit() {
  if (!canConfirm.value || !placeId.value) return;
  busy.value = true;
  error.value = '';
  try {
    const ok = await confirmUploadTo(placeId.value, dir.value, mode.value);
    if (!ok && state.error) error.value = state.error;
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  if (!state.remotePlaces?.length) {
    await api.remotePlaceList().then((list) => { state.remotePlaces = list || []; }).catch(() => {});
  }
  const first = places.value[0];
  if (first) await openPlace(first);
});
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="cancelUploadTo">
    <form class="dlg upload-dlg" @submit.prevent="submit">
      <h3>{{ i18n.t('upload_to.title') }}</h3>
      <div class="hint">{{ i18n.t('upload_to.hint') }}</div>

      <div v-if="!places.length" class="empty-target">
        {{ i18n.t('upload_to.no_writable_place') }}
      </div>

      <div v-else class="target-layout">
        <div class="place-list">
          <button
            v-for="place in places"
            :key="place.id"
            type="button"
            class="target-row"
            :class="{ picked: place.id === placeId }"
            @click="openPlace(place)"
          >
            <span>{{ place.kind === 'telegram' ? 'Telegram' : 'WebDAV' }}</span>
            <strong>{{ place.name }}</strong>
          </button>
        </div>

        <div class="folder-panel">
          <div class="folder-head">
            <button v-if="trail.length" type="button" class="btn small" @click="up">
              {{ i18n.t('upload_to.up') }}
            </button>
            <span>{{ currentPlace?.name }}</span>
            <span v-for="part in trail" :key="part.id + part.name"> / {{ part.name }}</span>
          </div>
          <div v-if="loading" class="folder-note">{{ i18n.t('upload_to.loading') }}</div>
          <div v-else-if="error" class="folder-note error">{{ error }}</div>
          <div v-else-if="!rows.length" class="folder-note">{{ i18n.t('upload_to.no_subfolders') }}</div>
          <button
            v-for="entry in rows"
            v-else
            :key="entry.id"
            type="button"
            class="target-row folder"
            @click="enter(entry)"
          >
            <span aria-hidden="true">›</span>
            <strong>{{ entry.real_name || entry.name }}</strong>
          </button>
        </div>
      </div>

      <div v-if="caps" class="cap-line">
        <span :class="{ yes: caps.write }">{{ i18n.t(caps.write ? 'upload_to.writable' : 'upload_to.readonly') }}</span>
        <span>{{ i18n.t(caps.rename ? 'upload_to.rename_yes' : 'upload_to.rename_no') }}</span>
        <span>{{ i18n.t(caps.delete ? 'upload_to.delete_yes' : 'upload_to.delete_no') }}</span>
      </div>

      <fieldset v-if="remoteSource" class="mode-list">
        <legend>{{ i18n.t('upload_to.mode') }}</legend>
        <label class="mode-row">
          <input v-model="mode" type="radio" value="permanent_cache" />
          <span>
            <strong>{{ i18n.t('upload_to.mode_cache') }}</strong>
            <small>{{ i18n.t('upload_to.mode_cache_hint') }}</small>
          </span>
        </label>
        <label class="mode-row">
          <input v-model="mode" type="radio" value="memory" />
          <span>
            <strong>{{ i18n.t('upload_to.mode_memory') }}</strong>
            <small>{{ i18n.t('upload_to.mode_memory_hint') }}</small>
          </span>
        </label>
      </fieldset>

      <div v-if="error" class="errline">{{ error }}</div>
      <div class="acts">
        <button type="button" class="btn" @click="cancelUploadTo">{{ i18n.t('actions.cancel') }}</button>
        <button type="submit" class="btn primary" :disabled="!canConfirm">
          {{ busy ? i18n.t('busy.working') : i18n.t('upload_to.confirm') }}
        </button>
      </div>
    </form>
  </div>
</template>

<style scoped>
.upload-dlg { width: min(760px, calc(100vw - 32px)); }
.target-layout { display: grid; grid-template-columns: 210px minmax(0, 1fr); min-height: 260px; border: 1px solid var(--border); border-radius: var(--r-s); overflow: hidden; margin: 12px 0; }
.place-list { border-inline-end: 1px solid var(--border); background: var(--bg2); }
.folder-panel { min-width: 0; }
.target-row { display: flex; align-items: center; gap: 8px; width: 100%; min-height: 42px; padding: 8px 12px; border: 0; border-bottom: 1px solid var(--border); background: transparent; color: var(--fg); text-align: start; cursor: pointer; }
.target-row:hover, .target-row.picked { background: var(--accent-soft); }
.target-row span { color: var(--fg2); font-size: 11px; }
.target-row strong { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.folder-head { min-height: 42px; display: flex; align-items: center; gap: 5px; padding: 6px 10px; border-bottom: 1px solid var(--border); color: var(--fg2); font-size: 12px; overflow: hidden; }
.folder-note, .empty-target { padding: 24px; color: var(--fg2); text-align: center; }
.folder-note.error, .errline { color: var(--danger); }
.cap-line { display: flex; flex-wrap: wrap; gap: 8px; font-size: 12px; color: var(--fg2); }
.cap-line span { padding: 3px 8px; border-radius: 999px; background: var(--bg2); }
.cap-line .yes { color: var(--ok); }
.mode-list { margin: 14px 0 0; border: 1px solid var(--border); border-radius: var(--r-s); }
.mode-row { display: flex; gap: 10px; align-items: flex-start; padding: 10px; cursor: pointer; }
.mode-row span { display: flex; flex-direction: column; gap: 3px; }
.mode-row small { color: var(--fg2); line-height: 1.4; }
.errline { margin-top: 10px; font-size: 12px; }
@media (max-width: 768px) {
  .upload-dlg { width: 100%; }
  .target-layout { grid-template-columns: 1fr; }
  .place-list { border-inline-end: 0; border-bottom: 1px solid var(--border); max-height: 140px; overflow: auto; }
  .folder-panel { min-height: 220px; }
}
</style>
