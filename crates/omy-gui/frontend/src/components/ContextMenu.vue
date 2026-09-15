<script setup>
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
import * as i18n from '../i18n.js';

const props = defineProps({
  /** 菜单项：`{ key, label, icon, danger?, disabled?, hint? }`。 */
  items: { type: Array, required: true },
  /** 触发位置（视口坐标）。 */
  x: { type: Number, required: true },
  y: { type: Number, required: true },
});

const emit = defineEmits(['pick', 'close']);

const menu = useTemplateRef('menu');
/** 实际渲染位置。先按触发点摆，测到尺寸后再夹进视口。 */
const pos = ref({ left: props.x, top: props.y });

/** 只显示分隔线之间真正有内容的段。
 *
 * 直接渲染传进来的数组会在「某一段全部不可用」时留下两条相邻的分隔线，
 * 或者菜单以分隔线开头/结尾。
 */
const groups = computed(() => {
  const out = [];
  let cur = [];
  for (const it of props.items) {
    if (it.key === 'sep') {
      if (cur.length) out.push(cur);
      cur = [];
    } else {
      cur.push(it);
    }
  }
  if (cur.length) out.push(cur);
  return out;
});

function pick(item) {
  if (item.disabled) return;
  emit('pick', item.key);
}

/** Esc 关闭。菜单是模态的，键盘用户必须有退出手段。 */
function onKey(ev) {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    emit('close');
  }
}

onMounted(async () => {
  window.addEventListener('keydown', onKey);
  await nextTick();
  const el = menu.value;
  if (!el) return;
  const r = el.getBoundingClientRect();
  const margin = 8;
  let left = props.x;
  let top = props.y;
  // 右/下越界时翻到触发点的另一侧，而不是简单减去溢出量——
  // 后者会让菜单盖住用户刚点的那个条目
  if (left + r.width + margin > window.innerWidth) {
    left = Math.max(margin, props.x - r.width);
  }
  if (top + r.height + margin > window.innerHeight) {
    top = Math.max(margin, props.y - r.height);
  }
  pos.value = { left, top };
  // 聚焦第一项可用项，让键盘能接着操作
  el.querySelector('.mi:not([disabled])')?.focus();
});

onBeforeUnmount(() => window.removeEventListener('keydown', onKey));
</script>

<template>
  <!-- 铺满全屏的透明层：点任何地方都关掉菜单。
       只在菜单自身上监听 blur 是不够的——点到别的条目上不会触发 -->
  <div class="ctxlayer" @click="$emit('close')" @contextmenu.prevent="$emit('close')">
    <div
      ref="menu"
      class="ctxmenu"
      role="menu"
      :style="{ left: pos.left + 'px', top: pos.top + 'px' }"
      @click.stop
    >
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
          @click="pick(it)"
        >
          <span class="mic" aria-hidden="true">{{ it.icon }}</span>
          <span class="mil">{{ it.label }}</span>
        </button>
      </template>
    </div>
  </div>
</template>
