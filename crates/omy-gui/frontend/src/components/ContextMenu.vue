<script setup lang="ts">
/** 右键菜单（触屏上是长按菜单）。
 *
 * # 为什么菜单项由父组件传进来
 *
 * 可用的操作取决于选中了什么：目录不能预览、锁着的加密文件不能改密码、
 * 密文目录不能重命名。这些判断依赖 store 里的 computed，本组件只负责
 * 「在哪显示、怎么显示、点了通知谁」。把判断也放进来会让它同时依赖
 * store 与 props，两处都能改可用项，早晚不一致。
 *
 * # 定位
 *
 * 菜单必须完整落在视口内。在靠右下角的条目上右键时，如果直接用鼠标坐标
 * 当左上角，菜单会有一部分被裁掉——而被裁掉的往往正是最后一项（删除）。
 */

import { ref, computed, onMounted, onBeforeUnmount, nextTick, useTemplateRef } from 'vue';
import { isMobile } from '../viewport';
import { isAndroid, registerMobileBack } from '../mobile-platform';
import * as i18n from '../i18n';

/** 右键菜单里一项。分隔线用 key 'sep'，其余字段按场景可选。 */
interface MenuItem {
  key: string;
  label?: string;
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  hint?: string;
  note?: string;
}

const props = defineProps({
  /** 菜单项：`{ key, label, icon, danger?, disabled?, hint?, note? }`。
   *  hint 是原生悬浮短标注；note 是显示在标签下方、与标签左对齐的长说明。 */
  items: { type: Array as () => MenuItem[], required: true },
  /** 触发位置（视口坐标）。 */
  x: { type: Number, required: true },
  y: { type: Number, required: true },
  /** 移动端底部弹层的标题（通常是被操作条目的名字）。
   *  弹层从屏幕底部升起、离手指很远，没有标题的话用户不确定菜单针对的是哪一项。 */
  title: { type: String, default: '' },
});

const emit = defineEmits<{
  (e: 'pick', key: string): void;
  (e: 'close'): void;
}>();

const menu = useTemplateRef('menu');
/** 实际渲染位置。先按触发点摆，测到尺寸后再夹进视口。 */
const modalMode = computed(() => isAndroid || isMobile.value);
const pos = ref({ left: props.x, top: props.y });
/** 尚未量完尺寸：此时菜单隐身，避免先在越界位置闪一帧再跳回来。 */
const measuring = ref(!modalMode.value);
/** 展开的锚点角。菜单翻到光标左/上方时，从对应的那一角长出来。 */
const origin = ref('top left');

/** 只显示分隔线之间真正有内容的段。
 *
 * 直接渲染传进来的数组会在「某一段全部不可用」时留下两条相邻的分隔线，
 * 或者菜单以分隔线开头/结尾。
 *
 * 顺带给每项编一个跨段的序号 `seq`，用于错峰浮现：CSS 的 nth-child 会把分隔线
 * 也数进去，分组后序号会断。
 */
const groups = computed(() => {
  const out: (MenuItem & { seq: number })[][] = [];
  let cur: (MenuItem & { seq: number })[] = [];
  let seq = 0;
  for (const it of props.items) {
    if (it.key === 'sep') {
      if (cur.length) out.push(cur);
      cur = [];
    } else {
      cur.push({ ...it, seq: seq++ });
    }
  }
  if (cur.length) out.push(cur);
  return out;
});

function pick(item: MenuItem) {
  if (item.disabled) return;
  emit('pick', item.key);
}

/** Esc 关闭。菜单是模态的，键盘用户必须有退出手段。 */
function onKey(ev: KeyboardEvent) {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    emit('close');
  }
}

/** 忽略长按手势尾巴上那次合成 click 的截止时刻。
 *
 * 触屏没有右键，长按是唯一的菜单入口；而浏览器在 touchend 之后会补发一次
 * click。菜单恰好在长按到点时弹出，于是这次 click 落在刚铺开的全屏关闭层
 * 上——实测 pointerup 与 click 相差 1ms，菜单在手指抬起的瞬间就被关掉，
 * 用户看到的是「长按好像闪过什么，然后什么也没有」。
 *
 * 取 600ms：合成 click 紧跟 touchend，而用户真想点空白处关闭，
 * 至少得先看见菜单再移过去，不会这么快。 */
const SYNTHETIC_CLICK_MS = 600;
let openedAt = 0;
let unregisterBack: (() => void) | null = null;

/** 点击关闭层。长按刚结束时的那一次不算，见 SYNTHETIC_CLICK_MS。 */
function onLayerClick() {
  if (performance.now() - openedAt < SYNTHETIC_CLICK_MS) return;
  emit('close');
}

onMounted(async () => {
  openedAt = performance.now();
  window.addEventListener('keydown', onKey);
  unregisterBack = registerMobileBack(() => {
    emit('close');
    return true;
  });
  await nextTick();
  const el = menu.value;
  if (!el) return;
  if (modalMode.value) {
    // 移动端是底部弹层，不读触发坐标。
    // 不自动聚焦第一项：触屏上焦点环会让第一项看起来像「已被选中」，
    // 而且会唤起某些 WebView 的选中高亮。
    pos.value = { left: 0, top: 0 };
    return;
  }
  const r = el.getBoundingClientRect();
  const margin = 8;
  let left = props.x;
  let top = props.y;
  let fromRight = false;
  let fromBottom = false;
  // 右/下越界时翻到触发点的另一侧，而不是简单减去溢出量——
  // 后者会让菜单盖住用户刚点的那个条目
  if (left + r.width + margin > window.innerWidth) {
    left = Math.max(margin, props.x - r.width);
    fromRight = true;
  }
  if (top + r.height + margin > window.innerHeight) {
    top = Math.max(margin, props.y - r.height);
    fromBottom = true;
  }
  pos.value = { left, top };
  origin.value = `${fromBottom ? 'bottom' : 'top'} ${fromRight ? 'right' : 'left'}`;
  measuring.value = false;
  // 聚焦第一项可用项，让键盘能接着操作
  (el.querySelector<HTMLElement>('.mi:not([disabled])'))?.focus({ preventScroll: true });
});

onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey);
  unregisterBack?.();
});
</script>

<template>
  <!-- 铺满全屏的透明层：点任何地方都关掉菜单。
       只在菜单自身上监听 blur 是不够的——点到别的条目上不会触发 -->
  <div class="ctxlayer" :class="{ mobile: modalMode }" @click="onLayerClick" @contextmenu.prevent="$emit('close')">
    <div
      ref="menu"
      class="ctxmenu"
      :class="{ mobile: modalMode, measuring }"
      role="menu"
      :style="modalMode ? undefined : { left: pos.left + 'px', top: pos.top + 'px', '--origin': origin }"
      @click.stop
    >
      <!-- 移动端底部弹层的头部：拖拽条只是视觉提示「这是一块可以收起的面板」，
           标题说明菜单针对哪一项（弹层离手指很远，没有它容易误以为点错了条目） -->
      <template v-if="modalMode">
        <div class="sheet-grip" aria-hidden="true"></div>
        <div v-if="title" class="sheet-title">{{ title }}</div>
      </template>
      <template v-for="(g, gi) in groups" :key="gi">
        <div v-if="gi > 0" class="msep"></div>
        <button
          v-for="it in g"
          :key="it.key"
          class="mi"
          :class="{ danger: it.danger }"
          :data-mi="it.key"
          role="menuitem"
          type="button"
          :disabled="it.disabled"
          :title="it.hint || ''"
          :style="{ '--i': it.seq }"
          @click="pick(it)"
        >
          <span class="mic" aria-hidden="true"><AppIcon :name="it.icon" /></span>
          <span class="mit">
            <span class="mil">{{ it.label }}</span>
            <!-- 长说明放标签下方并与之左对齐：右侧只剩百来像素，
                 右对齐换行会把一句话读断（原型 §2） -->
            <span v-if="it.note" class="minote">{{ it.note }}</span>
          </span>
        </button>
      </template>
    </div>
  </div>
</template>
