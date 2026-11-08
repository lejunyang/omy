<script setup lang="ts">
/** Telegram 对话高级搜索。
 *
 * 上半部分拼条件，下半部分即时列出命中并可批量处理。界面先行，后端命令
 * telegram_advanced_search / telegram_advanced_batch 尚未实现（契约见 types.ts）。
 *
 * 「全选」选的是**全部命中**而不只是已加载的这些：选择状态记成「全选 + 例外」，
 * 批量动作把 query 原样交给后端重新遍历，所以不必先把整段历史拉进界面。
 */
import { computed, reactive, ref } from 'vue';
import * as i18n from '../i18n';
import { placeThumbUrl, searchTelegramAdvanced, startTelegramAdvancedBatch, state } from '../store';
import {
  MEDIA_KINDS,
  buildQuery,
  hasSelection,
  isPicked,
  newRow,
  selectionCount,
  togglePick,
  toSelection,
  type ConditionRow,
  type RowError,
  type SelectionState,
} from '../tg-advanced-search';
import type { TgAdvancedQuery, TgBatchAction, TgMediaKind, TgSearchHit } from '../types';

const match = ref<TgAdvancedQuery['match']>('all');
const rows = reactive<ConditionRow[]>([newRow('media')]);
const errors = ref<RowError[]>([]);

/** 最近一次成功发起搜索时的查询。批量动作用它而不是当前表单：用户搜完又改了
 *  条件但没重新搜，按钮作用的必须是屏幕上列出来的这批结果。 */
const searched = ref<TgAdvancedQuery | null>(null);
const hits = ref<TgSearchHit[]>([]);
const cursor = ref<string | null>(null);
const scanned = ref(0);
const done = ref(false);
const loading = ref(false);
const searchError = ref('');
const acting = ref(false);
/** 每次新搜索递增；迟到的旧页按这个丢弃，避免两次搜索的结果混在一起。 */
let generation = 0;

const selection = reactive<SelectionState>({ all: false, ids: new Set<string>() });

const kindOptions: [ConditionRow['kind'], string][] = [
  ['media', 'adv_search.kind_media'],
  ['ext', 'adv_search.kind_ext'],
  ['size', 'adv_search.kind_size'],
  ['text', 'adv_search.kind_text'],
];

const actions: { key: TgBatchAction; icon: string; label: string; needsForward?: boolean }[] = [
  { key: 'pin', icon: 'pin-file', label: 'adv_search.act_pin' },
  { key: 'forward', icon: 'send', label: 'adv_search.act_forward', needsForward: true },
  { key: 'add_virtual', icon: 'folder', label: 'adv_search.act_add_virtual' },
  { key: 'upload_to', icon: 'upload', label: 'adv_search.act_upload_to' },
];

function rowErrors(id: number) {
  return errors.value.filter((e) => e.rowId === id);
}
const globalErrors = computed(() => errors.value.filter((e) => e.rowId == null));

function addRow() {
  rows.push(newRow('text'));
}

function removeRow(id: number) {
  const i = rows.findIndex((r) => r.id === id);
  if (i >= 0) rows.splice(i, 1);
}

function toggleKind(r: ConditionRow, k: TgMediaKind) {
  const i = r.types.indexOf(k);
  if (i >= 0) r.types.splice(i, 1);
  else r.types.push(k);
}

function resetResults() {
  hits.value = [];
  cursor.value = null;
  scanned.value = 0;
  done.value = false;
  searchError.value = '';
  selection.all = false;
  selection.ids.clear();
}

async function search() {
  const { query, errors: errs } = buildQuery(match.value, rows);
  errors.value = errs;
  if (!query) return;
  generation += 1;
  resetResults();
  searched.value = query;
  await loadMore();
}

/** 拉下一页。扫到对话开头（cursor 为 null）后标记 done，不再请求。 */
async function loadMore() {
  if (!searched.value || loading.value || done.value) return;
  const gen = generation;
  loading.value = true;
  try {
    const page = await searchTelegramAdvanced(searched.value, cursor.value);
    if (gen !== generation || !page) return;
    hits.value.push(...page.hits);
    scanned.value = page.scanned;
    cursor.value = page.cursor;
    done.value = page.cursor == null;
  } catch (e) {
    if (gen !== generation) return;
    // 不停在「加载中」：给出错误并允许重试，已拿到的结果保留
    searchError.value = i18n.te((e as { code?: string })?.code, i18n.t('adv_search.search_failed'));
  } finally {
    if (gen === generation) loading.value = false;
  }
}

/** 结果区滚到底附近时自动续扫，用户不用反复点「继续」。 */
function onResultsScroll(ev: Event) {
  const el = ev.target as HTMLElement;
  if (el.scrollHeight - el.scrollTop - el.clientHeight < 120) void loadMore();
}

function toggleAll() {
  selection.all = !selection.all;
  selection.ids.clear();
}

const picked = computed(() => selectionCount(selection, hits.value.length, done.value));
const anyPicked = computed(() => hasSelection(selection, hits.value.length, done.value));
const pickedLabel = computed(() => {
  const { n, atLeast } = picked.value;
  return atLeast
    ? i18n.t('adv_search.selected_at_least', { n })
    : i18n.tn('adv_search.selected', n, { n });
});

async function runAction(action: TgBatchAction) {
  if (!searched.value || !anyPicked.value || acting.value) return;
  acting.value = true;
  try {
    await startTelegramAdvancedBatch(searched.value, toSelection(selection), action);
  } finally {
    acting.value = false;
  }
}

function mediaIcon(h: TgSearchHit) {
  switch (h.media) {
    case 'photo': return 'image';
    case 'video': return 'video';
    case 'audio': case 'voice': return 'music';
    case 'link': return 'globe';
    case null: return 'message';
    default: return 'file';
  }
}

function fmtDate(sec: number) {
  try {
    return new Date(sec * 1000).toLocaleString();
  } catch {
    return String(sec);
  }
}

function close() {
  if (!acting.value) state.telegramAdvancedSearch = null;
}
</script>

<template>
  <div class="mask" data-adv-search @click.self="close">
    <div class="advsearch" role="dialog" aria-modal="true" :aria-label="i18n.t('adv_search.title')">
      <header>
        <div>
          <h2>{{ i18n.t('adv_search.title') }}</h2>
          <p>{{ state.telegramAdvancedSearch?.dirName }}</p>
        </div>
        <button type="button" class="iconbtn" :aria-label="i18n.t('actions.close')" @click="close">
          <AppIcon name="close" />
        </button>
      </header>

      <form class="conds" @submit.prevent="search">
        <div class="matchrow">
          <span>{{ i18n.t('adv_search.match_prefix') }}</span>
          <select v-model="match" data-adv-match>
            <option value="all">{{ i18n.t('adv_search.match_all') }}</option>
            <option value="any">{{ i18n.t('adv_search.match_any') }}</option>
          </select>
          <span>{{ i18n.t('adv_search.match_suffix') }}</span>
        </div>

        <div v-for="r in rows" :key="r.id" class="cond" data-adv-cond>
          <div class="condhead">
            <select v-model="r.kind" class="kindsel" data-adv-kind :aria-label="i18n.t('adv_search.kind')">
              <option v-for="[k, label] in kindOptions" :key="k" :value="k">{{ i18n.t(label) }}</option>
            </select>
            <!-- 取反做成「是 / 不是」二选一，而不是一个孤零零的复选框：
                 条件读出来就是一句完整的话，不用猜勾上代表什么 -->
            <select v-model="r.negate" class="negsel" data-adv-negate :aria-label="i18n.t('adv_search.negate')">
              <option :value="false">{{ i18n.t(`adv_search.pos_${r.kind}`) }}</option>
              <option :value="true">{{ i18n.t(`adv_search.neg_${r.kind}`) }}</option>
            </select>
            <button
              type="button"
              class="iconbtn"
              data-adv-remove
              :aria-label="i18n.t('adv_search.remove_condition')"
              @click="removeRow(r.id)"
            ><AppIcon name="trash" :size="16" /></button>
          </div>

          <div v-if="r.kind === 'media'" class="chips">
            <button
              v-for="k in MEDIA_KINDS"
              :key="k"
              type="button"
              class="chip"
              :class="{ on: r.types.includes(k) }"
              :aria-pressed="r.types.includes(k)"
              :data-adv-media="k"
              @click="toggleKind(r, k)"
            >{{ i18n.t(`adv_search.media_${k}`) }}</button>
          </div>
          <input
            v-else-if="r.kind === 'ext'"
            v-model="r.exts"
            type="text"
            data-adv-ext
            :placeholder="i18n.t('adv_search.ext_hint')"
          />
          <div v-else-if="r.kind === 'size'" class="sizerow">
            <input v-model="r.min" type="number" min="0" step="any" data-adv-min :placeholder="i18n.t('adv_search.size_min')" />
            <span>–</span>
            <input v-model="r.max" type="number" min="0" step="any" data-adv-max :placeholder="i18n.t('adv_search.size_max')" />
            <select v-model="r.unit" data-adv-unit>
              <option value="KB">KB</option>
              <option value="MB">MB</option>
              <option value="GB">GB</option>
            </select>
          </div>
          <input
            v-else
            v-model="r.text"
            type="search"
            data-adv-text
            :placeholder="i18n.t('adv_search.text_hint')"
          />
          <p v-for="e in rowErrors(r.id)" :key="e.key" class="err">{{ i18n.t(e.key) }}</p>
        </div>

        <p v-for="e in globalErrors" :key="e.key" class="err">{{ i18n.t(e.key) }}</p>

        <div class="condactions">
          <button type="button" class="btn small" data-adv-add @click="addRow">
            <AppIcon name="plus" :size="16" /> {{ i18n.t('adv_search.add_condition') }}
          </button>
          <button type="submit" class="btn primary" data-adv-search-btn :disabled="loading && !hits.length">
            <AppIcon name="search" :size="16" /> {{ i18n.t('adv_search.search') }}
          </button>
        </div>
        <p class="hint">{{ i18n.t('adv_search.server_hint') }}</p>
      </form>

      <section v-if="searched" class="results" data-adv-results>
        <div class="reshead">
          <label class="selall">
            <input type="checkbox" :checked="selection.all" data-adv-select-all @change="toggleAll" />
            <span>{{ i18n.t('adv_search.select_all') }}</span>
          </label>
          <span class="status" data-adv-status>
            {{ i18n.t('adv_search.found', { n: hits.length, scanned }) }}
            <template v-if="loading"> · {{ i18n.t('adv_search.scanning') }}</template>
            <template v-else-if="done"> · {{ i18n.t('adv_search.done') }}</template>
          </span>
        </div>
        <p v-if="selection.all && !done" class="hint selhint">{{ i18n.t('adv_search.select_all_hint') }}</p>

        <div class="reslist" @scroll="onResultsScroll">
          <label
            v-for="h in hits"
            :key="h.id"
            class="hit"
            :class="{ sel: isPicked(selection, h.id) }"
            data-adv-hit
          >
            <input type="checkbox" :checked="isPicked(selection, h.id)" @change="togglePick(selection, h.id)" />
            <span class="hthumb">
              <img v-if="h.thumb_token && state.showThumbnails" :src="placeThumbUrl(h.thumb_token)" alt="" loading="lazy" />
              <AppIcon v-else :name="mediaIcon(h)" />
            </span>
            <span class="hbody">
              <span class="hname">{{ h.file_name || h.text || i18n.t('adv_search.no_text') }}</span>
              <span class="hmeta">
                <template v-if="h.file_size != null">{{ i18n.formatSize(h.file_size) }} · </template>{{ fmtDate(h.date) }} · #{{ h.message }}
              </span>
              <span v-if="h.file_name && h.text" class="htext">{{ h.text }}</span>
            </span>
          </label>
          <div v-if="!hits.length && done && !searchError" class="empty-mini">{{ i18n.t('adv_search.no_hits') }}</div>
          <div v-if="searchError" class="err retry">
            {{ searchError }}
            <button type="button" class="btn small" @click="loadMore">{{ i18n.t('rplace.retry') }}</button>
          </div>
          <button
            v-else-if="!done && !loading"
            type="button"
            class="btn small more"
            data-adv-more
            @click="loadMore"
          >{{ i18n.t('adv_search.scan_more') }}</button>
        </div>
      </section>

      <footer v-if="searched">
        <span class="picked" data-adv-picked>{{ pickedLabel }}</span>
        <div class="acts">
          <button
            v-for="a in actions"
            :key="a.key"
            type="button"
            class="btn small"
            :data-adv-act="a.key"
            :disabled="!anyPicked || acting || (a.needsForward && !!state.telegramAdvancedSearch?.protected)"
            @click="runAction(a.key)"
          ><AppIcon :name="a.icon" :size="16" /> {{ i18n.t(a.label) }}</button>
        </div>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.mask { position: fixed; inset: 0; z-index: 1200; display: grid; place-items: center; padding: 16px; background: rgb(0 0 0 / 0.48); }
.advsearch { width: min(680px, 100%); max-height: min(860px, 94vh); display: flex; flex-direction: column; border: 1px solid var(--border); border-radius: 12px; background: var(--bg); box-shadow: 0 18px 60px rgb(0 0 0 / .25); overflow: hidden; }
header { display: flex; align-items: flex-start; justify-content: space-between; padding: 16px 20px; border-bottom: 1px solid var(--border); }
h2, p { margin: 0; } h2 { font-size: 18px; }
header p, .hint { margin-top: 4px; color: var(--fg2); font-size: 12px; }
.conds { padding: 14px 20px; border-bottom: 1px solid var(--border); overflow: auto; flex: 0 1 auto; max-height: 46vh; display: grid; gap: 10px; }
.matchrow { display: flex; align-items: center; gap: 8px; font-size: 13px; }
.matchrow select { min-height: 32px; }
.cond { display: grid; gap: 8px; padding: 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--bg2); }
.condhead { display: flex; gap: 8px; align-items: center; }
.kindsel { flex: 1 1 auto; min-height: 34px; }
.negsel { flex: 0 0 auto; min-height: 34px; }
.chips { display: flex; flex-wrap: wrap; gap: 6px; }
.chip { min-height: 30px; padding: 0 12px; border: 1px solid var(--border); border-radius: 15px; background: var(--bg); color: var(--fg); cursor: pointer; font-size: 13px; }
.chip.on { border-color: var(--accent); background: var(--accent-soft); color: var(--accent); }
.cond input[type='text'], .cond input[type='search'] { width: 100%; min-height: 36px; }
.sizerow { display: grid; grid-template-columns: 1fr auto 1fr auto; gap: 8px; align-items: center; }
.sizerow input { min-width: 0; min-height: 36px; }
.condactions { display: flex; justify-content: space-between; gap: 8px; }
.results { display: flex; flex-direction: column; min-height: 0; flex: 1 1 auto; }
.reshead { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 20px; border-bottom: 1px solid var(--border); font-size: 13px; }
.selall { display: flex; align-items: center; gap: 8px; cursor: pointer; }
.status { color: var(--fg2); font-size: 12px; }
.selhint { padding: 6px 20px 0; }
.reslist { overflow: auto; flex: 1 1 auto; min-height: 120px; padding: 6px 12px 12px; }
.hit { display: flex; align-items: center; gap: 10px; padding: 8px; border-radius: 8px; cursor: pointer; }
.hit:hover { background: var(--bg2); }
.hit.sel { background: var(--accent-soft); }
.hthumb { flex: 0 0 44px; width: 44px; height: 44px; display: grid; place-items: center; border-radius: 6px; background: var(--bg3); overflow: hidden; color: var(--fg2); }
.hthumb img { width: 100%; height: 100%; object-fit: cover; }
.hbody { min-width: 0; display: grid; gap: 2px; }
.hname, .htext { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.hname { font-size: 13px; }
.hmeta, .htext { color: var(--fg2); font-size: 12px; }
.more { display: block; margin: 10px auto 0; }
.empty-mini { padding: 24px 0; text-align: center; color: var(--fg2); font-size: 13px; }
.retry { display: flex; align-items: center; justify-content: center; gap: 10px; padding: 12px 0; }
footer { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 12px 20px; border-top: 1px solid var(--border); }
.picked { font-size: 13px; white-space: nowrap; }
.acts { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; }
.err { margin: 0; color: var(--danger); font-size: 12px; }
@media (--mobile) {
  .mask { padding: 0; align-items: end; }
  .advsearch { width: 100%; max-height: 96vh; border-radius: 16px 16px 0 0; }
  .conds { max-height: 42vh; padding: 12px 14px; }
  .sizerow { grid-template-columns: 1fr auto 1fr; }
  .sizerow select { grid-column: 1 / -1; }
  footer { flex-direction: column; align-items: stretch; padding-bottom: calc(12px + env(safe-area-inset-bottom)); }
  .acts { display: grid; grid-template-columns: 1fr 1fr; }
  .acts .btn { min-height: 44px; justify-content: center; }
}
</style>
