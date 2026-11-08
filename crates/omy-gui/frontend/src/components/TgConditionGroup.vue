<script setup lang="ts">
/** 高级搜索的条件组（递归）。根组也用它渲染，只是根不给取反、不能删除。
 *
 *  同组的相邻条件之间画一个「且 / 或」连接词：只在组头写一次「全部 / 任一」时，
 *  嵌套两层后很难一眼看出某两条之间是什么关系。连接词本身可以点，点一下切换
 *  整组的组合方式——它说的就是整组，不是这两条之间单独的关系。 */
import { computed } from 'vue';
import * as i18n from '../i18n';
import {
  MAX_GROUP_DEPTH,
  isGroup,
  newGroup,
  newRow,
  type GroupRow,
  type RowError,
} from '../tg-advanced-search';
import AppSelect from './AppSelect.vue';
import TgConditionGroup from './TgConditionGroup.vue';
import TgConditionRow from './TgConditionRow.vue';

const props = defineProps<{ group: GroupRow; depth: number; errors: RowError[] }>();
defineEmits<{ remove: [] }>();

const isRoot = computed(() => props.depth === 0);
const canNest = computed(() => props.depth < MAX_GROUP_DEPTH);

type Mode = 'all' | 'any' | 'not_all' | 'none';
const mode = computed<Mode>({
  get: () => {
    const g = props.group;
    if (!g.negate) return g.match;
    return g.match === 'all' ? 'not_all' : 'none';
  },
  set: (m) => {
    props.group.match = m === 'all' || m === 'not_all' ? 'all' : 'any';
    props.group.negate = m === 'not_all' || m === 'none';
  },
});
const modeOptions = computed(() => {
  const keys: Mode[] = isRoot.value ? ['all', 'any'] : ['all', 'any', 'not_all', 'none'];
  // 根组读作「满足以下 [全部条件] 的消息」，子组自成一句「以下全部满足」
  const prefix = isRoot.value ? 'adv_search.match_' : 'adv_search.group_';
  return keys.map((k) => ({ value: k, label: i18n.t(`${prefix}${k}`) }));
});

function errorsOf(id: number) {
  return props.errors.filter((e) => e.rowId === id).map((e) => e.key);
}

function remove(id: number) {
  const i = props.group.children.findIndex((c) => c.id === id);
  if (i >= 0) props.group.children.splice(i, 1);
}

function toggleMatch() {
  props.group.match = props.group.match === 'all' ? 'any' : 'all';
}
</script>

<template>
  <div class="cgroup" :class="{ root: isRoot, neg: group.negate }" :data-adv-group="depth">
    <div class="ghead">
      <span v-if="isRoot" class="glead">{{ i18n.t('adv_search.match_prefix') }}</span>
      <AppSelect v-model="mode" class="gmode" data-adv-match :options="modeOptions" :aria-label="i18n.t('adv_search.group_mode')" />
      <span v-if="isRoot" class="glead">{{ i18n.t('adv_search.match_suffix') }}</span>
      <span class="spacer" />
      <button
        v-if="!isRoot"
        type="button"
        class="iconbtn"
        data-adv-remove-group
        :aria-label="i18n.t('adv_search.remove_group')"
        @click="$emit('remove')"
      ><AppIcon name="trash" :size="16" /></button>
    </div>

    <div class="gbody">
      <template v-for="(c, i) in group.children" :key="c.id">
        <button
          v-if="i > 0"
          type="button"
          class="joiner"
          data-adv-joiner
          :title="i18n.t('adv_search.toggle_join')"
          @click="toggleMatch"
        >{{ i18n.t(group.match === 'all' ? 'adv_search.join_and' : 'adv_search.join_or') }}</button>
        <TgConditionGroup
          v-if="isGroup(c)"
          :group="c"
          :depth="depth + 1"
          :errors="errors"
          @remove="remove(c.id)"
        />
        <TgConditionRow v-else :row="c" :errors="errorsOf(c.id)" @remove="remove(c.id)" />
      </template>
      <p v-for="e in (isRoot ? [] : errorsOf(group.id))" :key="e" class="err">{{ i18n.t(e) }}</p>
    </div>

    <div class="gadd">
      <button type="button" class="btn small" data-adv-add @click="group.children.push(newRow('text'))">
        <AppIcon name="plus" :size="16" /> {{ i18n.t('adv_search.add_condition') }}
      </button>
      <button
        v-if="canNest"
        type="button"
        class="btn small"
        data-adv-add-group
        @click="group.children.push(newGroup(group.match === 'all' ? 'any' : 'all'))"
      >
        <AppIcon name="group" :size="16" /> {{ i18n.t('adv_search.add_group') }}
      </button>
    </div>
  </div>
</template>

<style scoped>
.cgroup { display: grid; gap: 8px; }
.cgroup:not(.root) {
  padding: 10px 10px 10px 12px;
  border: 1px dashed var(--border);
  border-left: 3px solid color-mix(in srgb, var(--accent) 55%, var(--border));
  border-radius: var(--r-m);
  background: color-mix(in srgb, var(--accent) 4%, transparent);
}
/* 取反的组换成警示色的左边线：「都不满足」和「全部满足」只差一个下拉，
   扫一眼时左边线是最显眼的区分 */
.cgroup.neg:not(.root) { border-left-color: color-mix(in srgb, var(--danger) 70%, var(--border)); }
.ghead { display: flex; align-items: center; gap: 8px; font-size: 13px; }
.glead { color: var(--fg2); white-space: nowrap; }
.gmode { min-width: 120px; }
.spacer { flex: 1; }
.gbody { display: grid; gap: 6px; }
.joiner {
  justify-self: start;
  margin-left: 14px;
  padding: 1px 10px;
  border: 1px solid var(--border);
  border-radius: 10px;
  background: var(--bg);
  color: var(--accent);
  font-size: 11.5px;
  font-weight: 600;
  letter-spacing: 0.5px;
}
.joiner:hover { border-color: var(--accent); }
.gadd { display: flex; flex-wrap: wrap; gap: 8px; }
.err { margin: 0; color: var(--danger); font-size: 12px; }
@media (--mobile) {
  .cgroup:not(.root) { padding: 8px 8px 8px 10px; }
  .gadd .btn { min-height: 40px; }
}
</style>
