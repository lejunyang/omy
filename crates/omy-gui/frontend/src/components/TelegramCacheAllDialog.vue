<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import * as i18n from '../i18n';
import { state, startTelegramCacheAll } from '../store';

const busy = ref(false);
const form = reactive({
  mediaTypes: ['media'] as string[],
  range: 'all',
  from: '',
  to: '',
  keyword: '',
});

const typeOptions = [
  ['media', 'cache_all.type_media'],
  ['audio', 'cache_all.type_audio'],
  ['file', 'cache_all.type_file'],
  ['gif', 'cache_all.type_gif'],
];

const valid = computed(() => form.mediaTypes.length > 0 && (
  form.range !== 'custom' || (!form.from || !form.to || form.from <= form.to)
));

function toggleType(key: string) {
  const i = form.mediaTypes.indexOf(key);
  if (i >= 0) form.mediaTypes.splice(i, 1);
  else form.mediaTypes.push(key);
}

function dayStart(value: string): number | null {
  if (!value) return null;
  const ms = new Date(`${value}T00:00:00`).getTime();
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : null;
}

function dayEnd(value: string): number | null {
  if (!value) return null;
  const ms = new Date(`${value}T23:59:59.999`).getTime();
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : null;
}

function rangeValues() {
  const now = new Date();
  if (form.range === '7d' || form.range === '30d') {
    const days = form.range === '7d' ? 7 : 30;
    return { from: Math.floor((now.getTime() - days * 86400000) / 1000), to: null };
  }
  if (form.range === 'custom') return { from: dayStart(form.from), to: dayEnd(form.to) };
  return { from: null, to: null };
}

async function submit() {
  if (!valid.value || busy.value) return;
  busy.value = true;
  try {
    const range = rangeValues();
    await startTelegramCacheAll({
      mediaTypes: [...form.mediaTypes],
      from: range.from,
      to: range.to,
      keyword: form.keyword.trim(),
    });
  } finally {
    busy.value = false;
  }
}

function close() {
  if (!busy.value) state.telegramCacheAll = null;
}
</script>

<template>
  <div class="mask" data-cache-all @click.self="close">
    <form class="cacheall" @submit.prevent="submit">
      <header>
        <div>
          <h2>{{ i18n.t('cache_all.title') }}</h2>
          <p>{{ state.telegramCacheAll?.dirName }}</p>
        </div>
        <button type="button" class="iconbtn" :aria-label="i18n.t('actions.close')" @click="close">
          <AppIcon name="close" />
        </button>
      </header>

      <section>
        <h3>{{ i18n.t('cache_all.types') }}</h3>
        <div class="checks">
          <label v-for="[key, label] in typeOptions" :key="key" class="checkrow">
            <input
              type="checkbox"
              :value="key"
              :checked="form.mediaTypes.includes(key)"
              @change="toggleType(key)"
            />
            <span>{{ i18n.t(label) }}</span>
          </label>
        </div>
        <p v-if="!form.mediaTypes.length" class="err">{{ i18n.t('cache_all.type_required') }}</p>
      </section>

      <section>
        <h3>{{ i18n.t('cache_all.range') }}</h3>
        <select v-model="form.range" data-cache-range>
          <option value="all">{{ i18n.t('cache_all.range_all') }}</option>
          <option value="7d">{{ i18n.t('cache_all.range_7d') }}</option>
          <option value="30d">{{ i18n.t('cache_all.range_30d') }}</option>
          <option value="custom">{{ i18n.t('cache_all.range_custom') }}</option>
        </select>
        <div v-if="form.range === 'custom'" class="daterow">
          <label>{{ i18n.t('cache_all.from') }}<input v-model="form.from" type="date" /></label>
          <label>{{ i18n.t('cache_all.to') }}<input v-model="form.to" type="date" /></label>
        </div>
      </section>

      <section>
        <h3>{{ i18n.t('cache_all.keyword') }}</h3>
        <input v-model="form.keyword" type="search" :placeholder="i18n.t('cache_all.keyword_hint')" />
      </section>

      <p class="hint">{{ i18n.t('cache_all.intersection_hint') }}</p>
      <footer>
        <button type="button" class="btn" :disabled="busy" @click="close">{{ i18n.t('actions.cancel') }}</button>
        <button type="submit" class="btn primary" :disabled="!valid || busy">
          {{ busy ? i18n.t('cache_all.starting') : i18n.t('cache_all.start') }}
        </button>
      </footer>
    </form>
  </div>
</template>

<style scoped>
.mask { position: fixed; inset: 0; z-index: 1200; display: grid; place-items: center; padding: 16px; background: rgb(0 0 0 / 0.48); }
.cacheall { width: min(520px, 100%); max-height: min(720px, 92vh); overflow: auto; border: 1px solid var(--border); border-radius: 12px; background: var(--bg); box-shadow: 0 18px 60px rgb(0 0 0 / .25); }
header, section, footer { padding: 16px 20px; }
header { display: flex; align-items: flex-start; justify-content: space-between; border-bottom: 1px solid var(--border); }
h2, h3, p { margin: 0; } h2 { font-size: 18px; } h3 { margin-bottom: 10px; font-size: 13px; }
header p, .hint { margin-top: 4px; color: var(--fg2); font-size: 12px; }
section { border-bottom: 1px solid var(--border); }
.checks { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.checkrow { min-height: 42px; display: flex; align-items: center; gap: 9px; padding: 0 10px; border: 1px solid var(--border); border-radius: 8px; }
select, input[type='search'], input[type='date'] { width: 100%; min-height: 40px; }
.daterow { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; margin-top: 10px; }
.daterow label { display: grid; gap: 5px; color: var(--fg2); font-size: 12px; }
.hint { padding: 12px 20px 0; }
footer { display: flex; justify-content: flex-end; gap: 8px; }
.err { margin-top: 8px; color: var(--danger); font-size: 12px; }
@media (max-width: 768px) {
  .mask { padding: 0; align-items: end; }
  .cacheall { width: 100%; max-height: 94vh; border-radius: 16px 16px 0 0; padding-bottom: env(safe-area-inset-bottom); }
  .checks, .daterow { grid-template-columns: 1fr; }
  footer .btn { min-height: 44px; flex: 1; }
}
</style>
