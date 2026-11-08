<script setup lang="ts" generic="T">
/** 下拉选择。替代原生 <select>：原生下拉的弹出列表由系统绘制，WebView2 / Android
 *  WebView 里是一块和应用主题无关的灰白列表，深色模式下尤其突兀，也没法跟随强调色。
 *
 *  键盘行为按 WAI-ARIA combobox（只选不输）模式：焦点始终留在触发按钮上，
 *  用 aria-activedescendant 指向当前高亮项。这样 Esc / Tab 的处理都在一处，
 *  也不会和外层对话框的焦点管理抢焦点。 */
import { computed, ref, useId } from 'vue';
import { usePopover } from '../popover';

defineOptions({ inheritAttrs: false });

const props = defineProps<{
  modelValue: T;
  options: { value: T; label: string }[];
  ariaLabel?: string;
  disabled?: boolean;
}>();
const emit = defineEmits<{ 'update:modelValue': [value: T] }>();

const trigger = ref<HTMLElement | null>(null);
const panel = ref<HTMLElement | null>(null);
const pop = usePopover(trigger, panel, { matchWidth: true });
const uid = useId();
const active = ref(-1);

const selectedIndex = computed(() => props.options.findIndex((o) => Object.is(o.value, props.modelValue)));
const current = computed(() => props.options[selectedIndex.value]);

function openAt(i: number) {
  active.value = i >= 0 ? i : 0;
  void pop.show().then(scrollActive);
}

function choose(i: number) {
  const o = props.options[i];
  if (!o) return;
  if (!Object.is(o.value, props.modelValue)) emit('update:modelValue', o.value);
  pop.hide();
  trigger.value?.focus();
}

function scrollActive() {
  panel.value?.querySelector<HTMLElement>(`[data-i="${active.value}"]`)?.scrollIntoView?.({ block: 'nearest' });
}

function move(delta: number) {
  const n = props.options.length;
  if (!n) return;
  active.value = (active.value + delta + n) % n;
  scrollActive();
}

function onKey(e: KeyboardEvent) {
  if (props.disabled) return;
  if (!pop.open.value) {
    if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) {
      e.preventDefault();
      openAt(selectedIndex.value);
    }
    return;
  }
  switch (e.key) {
    case 'ArrowDown': e.preventDefault(); move(1); break;
    case 'ArrowUp': e.preventDefault(); move(-1); break;
    case 'Home': e.preventDefault(); active.value = 0; scrollActive(); break;
    case 'End': e.preventDefault(); active.value = props.options.length - 1; scrollActive(); break;
    case 'Enter':
    case ' ':
      e.preventDefault();
      choose(active.value);
      break;
    case 'Escape':
      // 只关下拉本身。preventDefault 让 App 的全局 Esc 知道已经处理过，
      // 否则一次 Esc 会把下拉和外面的对话框一起关掉
      e.preventDefault();
      e.stopPropagation();
      pop.hide();
      break;
    case 'Tab':
      pop.hide();
      break;
  }
}

function onClick() {
  if (props.disabled) return;
  if (pop.open.value) pop.hide();
  else openAt(selectedIndex.value);
}
</script>

<template>
  <button
    ref="trigger"
    type="button"
    class="osel"
    role="combobox"
    aria-haspopup="listbox"
    :aria-expanded="pop.open.value"
    :aria-controls="`${uid}-list`"
    :aria-activedescendant="pop.open.value && active >= 0 ? `${uid}-${active}` : undefined"
    :aria-label="ariaLabel"
    :disabled="disabled"
    :class="{ 'osel-open': pop.open.value }"
    v-bind="$attrs"
    @click="onClick"
    @keydown="onKey"
  >
    <span class="osel-label">{{ current?.label ?? '' }}</span>
    <AppIcon name="chevronDown" :size="14" class="osel-caret" />
  </button>
  <Teleport to="body">
    <ul
      v-if="pop.open.value"
      :id="`${uid}-list`"
      ref="panel"
      class="osel-panel"
      :class="{ 'pop-measuring': pop.measuring.value }"
      role="listbox"
      :aria-label="ariaLabel"
      :style="pop.style.value"
    >
      <li
        v-for="(o, i) in options"
        :id="`${uid}-${i}`"
        :key="i"
        class="osel-opt"
        :class="{ 'osel-active': i === active, 'osel-sel': i === selectedIndex }"
        role="option"
        :aria-selected="i === selectedIndex"
        :data-i="i"
        @pointermove="active = i"
        @click="choose(i)"
      >
        <span>{{ o.label }}</span>
        <AppIcon v-if="i === selectedIndex" name="check" :size="14" />
      </li>
    </ul>
  </Teleport>
</template>

<style>
/* 修饰类一律带 osel- 前缀：app.css 里有 .empty / .sel 这类全局类，裸名会被
   套上无关的布局（实测 .empty 把日期按钮变成了竖排的大块）。
   不加 scoped：面板 Teleport 到了 body，scoped 的属性选择器仍然生效，但调用方
   需要用自己的类名覆写尺寸（.kindsel { flex: 1 }），全局类更直接。 */
.osel {
  display: inline-flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  min-height: 32px;
  padding: 0 8px 0 10px;
  border: 1px solid var(--border);
  border-radius: var(--r-m);
  background: var(--bg3);
  color: var(--fg);
  font-size: 13px;
  text-align: start;
  cursor: pointer;
  transition: border-color var(--dur-1) ease, background-color var(--dur-1) ease;
}
.osel:hover:not(:disabled) { border-color: color-mix(in srgb, var(--accent) 60%, var(--border)); }
.osel.osel-open, .osel:focus-visible { border-color: var(--accent); outline: none; box-shadow: 0 0 0 3px var(--accent-soft); }
.osel:disabled { opacity: 0.55; cursor: default; }
.osel-label { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.osel-caret { color: var(--fg2); transition: transform var(--dur-2) var(--ease-out); }
.osel.osel-open .osel-caret { transform: rotate(180deg); }

.osel-panel {
  position: fixed;
  z-index: 1400;
  margin: 0;
  padding: 4px;
  list-style: none;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: var(--r-m);
  background: var(--bg2);
  box-shadow: var(--shadow-pop);
  transform-origin: var(--origin, top left);
  animation: omy-menu-in var(--dur-2) var(--ease-out) backwards;
}
.osel-panel.pop-measuring { visibility: hidden; animation: none; }
.osel-opt {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  min-height: 32px;
  padding: 0 10px;
  border-radius: var(--r-s);
  color: var(--fg);
  font-size: 13px;
  white-space: nowrap;
  cursor: pointer;
}
.osel-opt.osel-active { background: var(--bg3); }
.osel-opt.osel-sel { color: var(--accent); }
@media (--mobile) {
  .osel { min-height: 40px; }
  .osel-opt { min-height: 44px; font-size: 14px; }
}
</style>
