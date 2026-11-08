<script setup lang="ts">
/** 高级搜索里的一行条件。字段全在同一个 ConditionRow 上，切换种类时不丢已填的值。 */
import { computed } from 'vue';
import * as i18n from '../i18n';
import { MEDIA_KINDS, type ConditionRow, type SizeUnit } from '../tg-advanced-search';
import type { TgMediaKind } from '../types';
import AppDatePicker from './AppDatePicker.vue';
import AppSelect from './AppSelect.vue';

const props = defineProps<{ row: ConditionRow; errors: string[] }>();
defineEmits<{ remove: [] }>();

const KINDS: ConditionRow['kind'][] = ['media', 'ext', 'size', 'text', 'date'];
const kindOptions = computed(() => KINDS.map((k) => ({ value: k, label: i18n.t(`adv_search.kind_${k}`) })));
// 取反做成「是 / 不是」二选一，而不是一个孤零零的复选框：条件读出来就是一句
// 完整的话，不用猜勾上代表什么
const negateOptions = computed(() => [
  { value: false, label: i18n.t(`adv_search.pos_${props.row.kind}`) },
  { value: true, label: i18n.t(`adv_search.neg_${props.row.kind}`) },
]);
const unitOptions: { value: SizeUnit; label: string }[] = (['KB', 'MB', 'GB'] as const).map((u) => ({ value: u, label: u }));

function toggleKind(k: TgMediaKind) {
  const i = props.row.types.indexOf(k);
  if (i >= 0) props.row.types.splice(i, 1);
  else props.row.types.push(k);
}
</script>

<template>
  <div class="cond" data-adv-cond>
    <div class="condhead">
      <AppSelect v-model="row.kind" class="kindsel" data-adv-kind :options="kindOptions" :aria-label="i18n.t('adv_search.kind')" />
      <AppSelect v-model="row.negate" class="negsel" data-adv-negate :options="negateOptions" :aria-label="i18n.t('adv_search.negate')" />
      <button
        type="button"
        class="iconbtn"
        data-adv-remove
        :aria-label="i18n.t('adv_search.remove_condition')"
        @click="$emit('remove')"
      ><AppIcon name="trash" :size="16" /></button>
    </div>

    <div v-if="row.kind === 'media'" class="chips">
      <button
        v-for="k in MEDIA_KINDS"
        :key="k"
        type="button"
        class="chip"
        :class="{ on: row.types.includes(k) }"
        :aria-pressed="row.types.includes(k)"
        :data-adv-media="k"
        @click="toggleKind(k)"
      >{{ i18n.t(`adv_search.media_${k}`) }}</button>
    </div>
    <input
      v-else-if="row.kind === 'ext'"
      v-model="row.exts"
      class="tin"
      type="text"
      data-adv-ext
      :placeholder="i18n.t('adv_search.ext_hint')"
    />
    <div v-else-if="row.kind === 'size'" class="range sizerow">
      <input v-model="row.min" class="tin" type="number" min="0" step="any" data-adv-min :placeholder="i18n.t('adv_search.size_min')" />
      <span class="dash">–</span>
      <input v-model="row.max" class="tin" type="number" min="0" step="any" data-adv-max :placeholder="i18n.t('adv_search.size_max')" />
      <AppSelect v-model="row.unit" class="unitsel" data-adv-unit :options="unitOptions" :aria-label="i18n.t('adv_search.size_unit')" />
    </div>
    <div v-else-if="row.kind === 'date'" class="range daterow">
      <AppDatePicker
        v-model="row.from"
        data-adv-from
        :max="row.to || undefined"
        :placeholder="i18n.t('adv_search.date_from')"
        :aria-label="i18n.t('adv_search.date_from')"
      />
      <span class="dash">–</span>
      <AppDatePicker
        v-model="row.to"
        data-adv-to
        :min="row.from || undefined"
        :placeholder="i18n.t('adv_search.date_to')"
        :aria-label="i18n.t('adv_search.date_to')"
      />
    </div>
    <input
      v-else
      v-model="row.text"
      class="tin"
      type="search"
      data-adv-text
      :placeholder="i18n.t('adv_search.text_hint')"
    />
    <p v-for="e in errors" :key="e" class="err">{{ i18n.t(e) }}</p>
  </div>
</template>

<style scoped>
.cond { display: grid; gap: 8px; padding: 10px; border: 1px solid var(--border); border-radius: var(--r-m); background: var(--bg2); }
.condhead { display: flex; gap: 8px; align-items: center; }
.kindsel { flex: 1 1 auto; min-width: 0; }
.negsel { flex: 0 0 auto; min-width: 104px; }
.chips { display: flex; flex-wrap: wrap; gap: 6px; }
.chip { min-height: 30px; padding: 0 12px; border: 1px solid var(--border); border-radius: 15px; background: var(--bg); color: var(--fg); font-size: 13px; }
.chip.on { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
.tin { width: 100%; min-width: 0; min-height: 34px; padding: 0 10px; border: 1px solid var(--border); border-radius: var(--r-m); background: var(--bg3); color: var(--fg); font-size: 13px; }
.tin:focus { border-color: var(--accent); outline: none; box-shadow: 0 0 0 3px var(--accent-soft); }
.range { display: grid; gap: 8px; align-items: center; }
.sizerow { grid-template-columns: 1fr auto 1fr 76px; }
.daterow { grid-template-columns: 1fr auto 1fr; }
.dash { color: var(--fg2); }
.err { margin: 0; color: var(--danger); font-size: 12px; }
@media (--mobile) {
  .tin { min-height: 40px; }
  .sizerow { grid-template-columns: 1fr auto 1fr; }
  .unitsel { grid-column: 1 / -1; }
  .daterow { grid-template-columns: 1fr; }
  .daterow .dash { display: none; }
}
</style>
