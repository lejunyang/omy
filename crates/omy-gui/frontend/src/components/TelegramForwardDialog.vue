<script setup lang="ts">
/** Telegram 原生转发目标选择器。
 *
 * 目标列表由后端按当前账号与会话实际写权限筛选；前端不根据名称或类型猜权限。
 * 新建群使用空成员超级群，初始只有当前账号。创建成功后选中它，但仍需用户确认转发。
 */
import { computed, onMounted, ref } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import { state, cancelTelegramForward, confirmTelegramForward } from '../store';
import type { TelegramForwardTarget } from '../types';

const targets = ref<TelegramForwardTarget[]>([]);
const selected = ref('');
const query = ref('');
const groupName = ref('');
const loading = ref(true);
const forwarding = ref(false);
const creating = ref(false);
const error = ref('');

const pending = computed(() => state.telegramForward);
const visible = computed(() => {
  const q = query.value.trim().toLocaleLowerCase();
  if (!q) return targets.value;
  return targets.value.filter((t) => t.title.toLocaleLowerCase().includes(q));
});
const canForward = computed(() => !!selected.value && !loading.value && !forwarding.value && !creating.value);
const canCreate = computed(() => {
  const n = groupName.value.trim().length;
  return n > 0 && n <= 128 && !loading.value && !forwarding.value && !creating.value;
});

function icon(kind: TelegramForwardTarget['kind']) {
  return kind === 'user' ? '👤' : '👥';
}

async function load() {
  const p = pending.value;
  if (!p) return;
  loading.value = true;
  error.value = '';
  try {
    targets.value = await api.telegramForwardTargets(p.placeId) || [];
  } catch (e) {
    error.value = i18n.te(api.errCode(e), i18n.t('tg_forward.load_failed'));
  } finally {
    loading.value = false;
  }
}

async function createGroup() {
  const p = pending.value;
  const title = groupName.value.trim();
  if (!p || !canCreate.value) return;
  creating.value = true;
  error.value = '';
  try {
    const target = await api.telegramCreateSelfGroup(p.placeId, title) as TelegramForwardTarget;
    targets.value = [target, ...targets.value.filter((t) => t.dir_id !== target.dir_id)];
    selected.value = target.dir_id;
    groupName.value = '';
  } catch (e) {
    error.value = i18n.te(api.errCode(e), i18n.t('tg_forward.create_failed'));
  } finally {
    creating.value = false;
  }
}

async function submit() {
  if (!canForward.value) return;
  forwarding.value = true;
  error.value = '';
  const count = await confirmTelegramForward(selected.value);
  if (!count && state.placeError) error.value = state.placeError;
  forwarding.value = false;
}

onMounted(load);
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="cancelTelegramForward">
    <form class="dlg forward-dlg" @submit.prevent="submit">
      <h3>{{ i18n.t('tg_forward.title') }}</h3>
      <div class="hint">
        {{ i18n.tn('tg_forward.hint', pending?.messageIds?.length || 0,
          { count: pending?.messageIds?.length || 0 }) }}
      </div>

      <input
        v-model="query"
        class="text-input"
        type="search"
        :placeholder="i18n.t('tg_forward.search')"
      />

      <div class="target-list">
        <div v-if="loading" class="empty-target">{{ i18n.t('tg_forward.loading') }}</div>
        <div v-else-if="!visible.length" class="empty-target">{{ i18n.t('tg_forward.no_targets') }}</div>
        <label v-for="target in visible" v-else :key="target.dir_id" class="target-row">
          <input v-model="selected" type="radio" :value="target.dir_id" />
          <span class="target-icon" aria-hidden="true"><AppIcon :name="icon(target.kind)" /></span>
          <strong>{{ target.title }}</strong>
        </label>
      </div>

      <fieldset class="create-box">
        <legend>{{ i18n.t('tg_forward.create_title') }}</legend>
        <div class="create-row">
          <input
            v-model="groupName"
            class="text-input"
            maxlength="128"
            :placeholder="i18n.t('tg_forward.group_name')"
          />
          <button type="button" class="btn" :disabled="!canCreate" @click="createGroup">
            {{ creating ? i18n.t('busy.working') : i18n.t('tg_forward.create') }}
          </button>
        </div>
        <div class="hint">{{ i18n.t('tg_forward.create_hint') }}</div>
      </fieldset>

      <div v-if="error" class="errline">{{ error }}</div>
      <div class="acts">
        <button type="button" class="btn" @click="cancelTelegramForward">{{ i18n.t('actions.cancel') }}</button>
        <button type="submit" class="btn primary" :disabled="!canForward">
          {{ forwarding ? i18n.t('tg_forward.forwarding') : i18n.t('tg_forward.confirm') }}
        </button>
      </div>
    </form>
  </div>
</template>

<style scoped>
.forward-dlg { width: min(620px, calc(100vw - 32px)); }
.text-input { width: 100%; box-sizing: border-box; min-height: 42px; padding: 8px 10px; border: 1px solid var(--border); border-radius: var(--r-s); background: var(--bg); color: var(--fg); }
.target-list { min-height: 190px; max-height: min(42vh, 380px); overflow: auto; border: 1px solid var(--border); border-radius: var(--r-s); margin: 10px 0 14px; }
.target-row { display: flex; align-items: center; gap: 10px; min-height: 44px; padding: 6px 12px; border-bottom: 1px solid var(--border); cursor: pointer; }
.target-row:last-child { border-bottom: 0; }
.target-row:hover { background: var(--accent-soft); }
.target-row strong { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.target-icon { font-size: 18px; }
.empty-target { padding: 40px 20px; text-align: center; color: var(--fg2); }
.create-box { margin: 0; border: 1px solid var(--border); border-radius: var(--r-s); }
.create-row { display: flex; gap: 8px; }
.create-row .text-input { flex: 1; min-width: 0; }
.hint { color: var(--fg2); font-size: 12px; line-height: 1.45; }
.errline { margin-top: 10px; color: var(--danger); font-size: 12px; }
@media (max-width: 768px) {
  .forward-dlg { width: 100%; }
  .create-row { flex-direction: column; }
}
</style>
