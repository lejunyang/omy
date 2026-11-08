<script setup lang="ts">
/** 日期选择。替代 <input type="date">：原生控件在 WebView2 里是系统样式的
 *  弹出日历，Android WebView 里是全屏系统对话框，都不跟随主题，且两端的显示格式、
 *  清空方式各不相同。
 *
 *  值是 'YYYY-MM-DD'（本地日历上的一天），空串表示未选。日期运算见 calendar.ts。
 *  min / max 用来让「起」「止」两个选择器互相限制，选不出颠倒的区间。 */
import { computed, nextTick, ref, useId } from 'vue';
import * as i18n from '../i18n';
import { addDays, addMonths, monthGrid, parseYmd, todayYmd, weekStartFor } from '../calendar';
import { usePopover } from '../popover';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  modelValue: string;
  min?: string;
  max?: string;
  placeholder?: string;
  ariaLabel?: string;
}>();
const emit = defineEmits<{ 'update:modelValue': [value: string] }>();

const trigger = ref<HTMLElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const pop = usePopover(trigger, panel);
const uid = useId();

/** 键盘焦点所在的那一天；面板显示它所在的月份。 */
const focusDay = ref(todayYmd());
const view = computed(() => parseYmd(focusDay.value) ?? parseYmd(todayYmd())!);

const locale = computed(() => i18n.lang.value || 'en');
const weekStart = computed(() => weekStartFor(locale.value));
const days = computed(() => monthGrid(view.value.y, view.value.m, weekStart.value));
const weekdays = computed(() => {
  const fmt = new Intl.DateTimeFormat(locale.value, { weekday: 'narrow' });
  // 2023-01-01 是周日，以它为基准取 7 个星期名
  return Array.from({ length: 7 }, (_, i) => fmt.format(new Date(2023, 0, 1 + ((weekStart.value + i) % 7), 12)));
});
const monthTitle = computed(() =>
  new Intl.DateTimeFormat(locale.value, { year: 'numeric', month: 'long' }).format(
    new Date(view.value.y, view.value.m - 1, 1, 12),
  ),
);
const display = computed(() => {
  const p = parseYmd(props.modelValue);
  if (!p) return '';
  return new Intl.DateTimeFormat(locale.value, { year: 'numeric', month: 'short', day: 'numeric' }).format(
    new Date(p.y, p.m - 1, p.d, 12),
  );
});
const today = computed(() => todayYmd());

// 'YYYY-MM-DD' 定宽，字符串比较就是日期比较
function disabledDay(d: string) {
  return (!!props.min && d < props.min) || (!!props.max && d > props.max);
}
function inMonth(d: string) {
  return parseYmd(d)?.m === view.value.m;
}

async function focusCell() {
  await nextTick();
  panel.value?.querySelector<HTMLElement>(`[data-day="${focusDay.value}"]`)?.focus();
}

async function openPanel() {
  focusDay.value = parseYmd(props.modelValue) ? props.modelValue : clamp(today.value);
  await pop.show();
  await focusCell();
}

function clamp(d: string) {
  if (props.min && d < props.min) return props.min;
  if (props.max && d > props.max) return props.max;
  return d;
}

function pick(d: string) {
  if (disabledDay(d)) return;
  emit('update:modelValue', d);
  closeAndReturn();
}

function clear() {
  emit('update:modelValue', '');
  closeAndReturn();
}

function closeAndReturn() {
  pop.hide();
  trigger.value?.focus();
}

function shiftMonth(n: number) {
  focusDay.value = addMonths(focusDay.value, n);
}

function onGridKey(e: KeyboardEvent) {
  const step: Record<string, () => string> = {
    ArrowLeft: () => addDays(focusDay.value, -1),
    ArrowRight: () => addDays(focusDay.value, 1),
    ArrowUp: () => addDays(focusDay.value, -7),
    ArrowDown: () => addDays(focusDay.value, 7),
    PageUp: () => addMonths(focusDay.value, e.shiftKey ? -12 : -1),
    PageDown: () => addMonths(focusDay.value, e.shiftKey ? 12 : 1),
  };
  const f = step[e.key];
  if (f) {
    e.preventDefault();
    focusDay.value = f();
    void focusCell();
  } else if (e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    pick(focusDay.value);
  }
}

function onPanelKey(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    // 同 AppSelect：只关日历，不让全局 Esc 再关掉外层对话框
    e.preventDefault();
    e.stopPropagation();
    closeAndReturn();
  }
}

function onTriggerKey(e: KeyboardEvent) {
  if (e.key === 'ArrowDown' && !pop.open.value) {
    e.preventDefault();
    void openPanel();
  }
}
</script>

<template>
  <button
    ref="trigger"
    type="button"
    class="odate"
    :class="{ 'odate-open': pop.open.value, 'odate-empty': !display }"
    aria-haspopup="dialog"
    :aria-expanded="pop.open.value"
    :aria-label="ariaLabel"
    v-bind="$attrs"
    @click="pop.open.value ? pop.hide() : openPanel()"
    @keydown="onTriggerKey"
  >
    <AppIcon name="calendar" :size="15" class="odate-ico" />
    <span class="odate-label">{{ display || placeholder }}</span>
  </button>
  <Teleport to="body">
    <div
      v-if="pop.open.value"
      ref="panel"
      class="odate-panel"
      :class="{ 'pop-measuring': pop.measuring.value }"
      role="dialog"
      :aria-label="ariaLabel"
      :style="pop.style.value"
      @keydown="onPanelKey"
    >
      <div class="odate-head">
        <button type="button" class="odate-nav" data-nav="-12" :aria-label="i18n.t('datepicker.prev_year')" @click="shiftMonth(-12)">
          <AppIcon name="chevronsLeft" :size="16" />
        </button>
        <button type="button" class="odate-nav" data-nav="-1" :aria-label="i18n.t('datepicker.prev_month')" @click="shiftMonth(-1)">
          <AppIcon name="chevronLeft" :size="16" />
        </button>
        <span :id="`${uid}-title`" class="odate-title" aria-live="polite">{{ monthTitle }}</span>
        <button type="button" class="odate-nav" data-nav="1" :aria-label="i18n.t('datepicker.next_month')" @click="shiftMonth(1)">
          <AppIcon name="chevronRight" :size="16" />
        </button>
        <button type="button" class="odate-nav" data-nav="12" :aria-label="i18n.t('datepicker.next_year')" @click="shiftMonth(12)">
          <AppIcon name="chevronsRight" :size="16" />
        </button>
      </div>
      <div class="odate-grid" role="grid" :aria-labelledby="`${uid}-title`" @keydown="onGridKey">
        <span v-for="(w, i) in weekdays" :key="`w${i}`" class="odate-wd" role="columnheader">{{ w }}</span>
        <button
          v-for="d in days"
          :key="d"
          type="button"
          class="odate-day"
          role="gridcell"
          :class="{ 'odate-out': !inMonth(d), 'odate-today': d === today, 'odate-sel': d === modelValue }"
          :data-day="d"
          :tabindex="d === focusDay ? 0 : -1"
          :disabled="disabledDay(d)"
          :aria-selected="d === modelValue"
          @click="pick(d)"
        >{{ Number(d.slice(8)) }}</button>
      </div>
      <div class="odate-foot">
        <button type="button" class="odate-link" data-clear @click="clear">{{ i18n.t('datepicker.clear') }}</button>
        <button
          type="button"
          class="odate-link"
          data-today
          :disabled="disabledDay(today)"
          @click="pick(today)"
        >{{ i18n.t('datepicker.today') }}</button>
      </div>
    </div>
  </Teleport>
</template>

<style>
/* 修饰类带 odate- 前缀、不加 scoped：理由同 AppSelect.vue。 */
.odate {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
  min-height: 34px;
  padding: 0 10px;
  border: 1px solid var(--border);
  border-radius: var(--r-m);
  background: var(--bg3);
  color: var(--fg);
  font-size: 13px;
  text-align: start;
  cursor: pointer;
  transition: border-color var(--dur-1) ease;
}
.odate:hover { border-color: color-mix(in srgb, var(--accent) 60%, var(--border)); }
.odate.odate-open, .odate:focus-visible { border-color: var(--accent); outline: none; box-shadow: 0 0 0 3px var(--accent-soft); }
.odate.odate-empty .odate-label { color: var(--fg2); }
.odate-ico { color: var(--fg2); }
.odate-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.odate-panel {
  position: fixed;
  z-index: 1400;
  width: 272px;
  padding: 10px;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: var(--r-l);
  background: var(--bg2);
  box-shadow: var(--shadow-pop);
  transform-origin: var(--origin, top left);
  animation: omy-menu-in var(--dur-2) var(--ease-out) backwards;
}
.odate-panel.pop-measuring { visibility: hidden; animation: none; }
.odate-head { display: flex; align-items: center; gap: 2px; margin-bottom: 6px; }
.odate-title { flex: 1; text-align: center; font-size: 13px; font-weight: 600; }
.odate-nav {
  width: 28px; height: 28px; display: grid; place-items: center;
  border-radius: var(--r-s); color: var(--fg2);
}
.odate-nav:hover { background: var(--bg3); color: var(--fg); }
.odate-grid { display: grid; grid-template-columns: repeat(7, 1fr); gap: 2px; }
.odate-wd { height: 26px; display: grid; place-items: center; color: var(--fg2); font-size: 11px; }
.odate-day {
  height: 34px; display: grid; place-items: center;
  border-radius: var(--r-s); font-size: 13px; color: var(--fg);
  font-variant-numeric: tabular-nums;
}
.odate-day:hover:not(:disabled) { background: var(--bg3); }
.odate-day:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
.odate-day.odate-out { color: var(--fg2); opacity: 0.6; }
.odate-day.odate-today { box-shadow: inset 0 0 0 1px var(--accent); }
.odate-day.odate-sel { background: var(--accent-solid); color: #fff; opacity: 1; }
.odate-day:disabled { opacity: 0.3; cursor: default; }
.odate-foot { display: flex; justify-content: space-between; margin-top: 8px; padding-top: 8px; border-top: 1px solid var(--border); }
.odate-link { padding: 4px 8px; border-radius: var(--r-s); color: var(--accent); font-size: 12.5px; }
.odate-link:hover:not(:disabled) { background: var(--accent-soft); }
.odate-link:disabled { color: var(--fg-dim); cursor: default; }
@media (--mobile) {
  .odate { min-height: 40px; }
  .odate-panel { width: min(320px, calc(100vw - 16px)); }
  .odate-day { height: 40px; }
}
</style>
