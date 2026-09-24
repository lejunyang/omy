<script setup>
/** 窗口化列表封装：用 virtua 的 Virtualizer 把 omy 里的长列表窗口化，只渲染视口
 * 附近一批、滚出去的回收，DOM 节点数恒定（与总条数无关）——大群里往下翻几百上千
 * 条也不卡。三处长列表（消息 / 文件列表 / 文件网格按行分块）共用这一个封装。
 *
 * 为什么用 Virtualizer 而不是 VList：omy 的滚动容器是外层 `.content`（flex:1 +
 * overflow-y:auto，含搜索栏等固定内容），列表本身不另开滚动条。Virtualizer 支持
 * `scrollRef` 指到那个外层滚动容器，在它里面做窗口化；VList 会自建滚动容器、要求
 * 自己有确定高度，套进 `.content` 里高度塌成 0、什么都不渲染（实测踩过）。
 *
 * - `items`：数据数组；默认插槽 `{ item, index }` 渲染单条。
 * - `scrollParent`：外层滚动容器 DOM（不传则用直接父元素）。
 * - `@reach-end`：滚动接近底部触发一次，用于无限加载（分页往末尾追加更旧内容）。
 * - 暴露 `scrollToIndex`，供"定位到某条并高亮"。
 */
import { ref } from 'vue';
import { Virtualizer } from 'virtua/vue';

const props = defineProps({
  items: { type: Array, required: true },
  scrollParent: { type: Object, default: null },
  endThreshold: { type: Number, default: 600 },
  /**
   * 视口外多渲染多高的缓冲（px），透传给 virtua 的 bufferSize。
   *
   * 默认 200 是 virtua 的保守值：快速滚动（尤其触屏甩动）时，新进入的条目还没
   * 完成测量/挂载就进了视口，肉眼看到的就是「滚的时候上面一片空白、停下来条目
   * 才冒出来」。调大到约 1.5 个视口高度，让视口上下两侧常备一屏多的已挂载条目，
   * 快速滚动也不露白。多渲染的条目数量恒定（与列表总长无关），长列表依旧不卡。
   */
  buffer: { type: Number, default: 900 },
  /**
   * 列表数据是否会在**头部插入**（virtua 的 shift）。消息时间线往顶部续翻、把
   * 更新的消息 prepend 到数组开头时必须为 true：virtua 会按「距底部」锚定滚动，
   * 否则头部一插入内容，所有现有行被整体下推，视觉上就是列表猛地跳一下。
   *
   * 只有消息列表传 true（它唯一会 prepend）；文件列表只在末尾追加，保持 false。
   */
  shift: { type: Boolean, default: false },
  /** 距顶部多少 px 内视为「到顶」，触发 reach-start。 */
  startThreshold: { type: Number, default: 600 },
});
const emit = defineEmits(['reach-end', 'reach-start']);
const vh = ref(null);

function onScroll() {
  const h = vh.value;
  const sc = props.scrollParent;
  if (!h || !sc) return;
  const rest = sc.scrollHeight - (sc.scrollTop + sc.clientHeight);
  if (rest <= props.endThreshold) emit('reach-end');
  // 到顶：消息时间线往顶部滚是「往更新翻」，与到底（往更旧翻）对称。
  if (sc.scrollTop <= props.startThreshold) emit('reach-start');
}

function scrollToIndex(index, opts) {
  vh.value?.scrollToIndex(index, opts || { align: 'center' });
}
defineExpose({ scrollToIndex });
</script>

<template>
  <Virtualizer
    ref="vh"
    :data="items"
    :scroll-ref="scrollParent || undefined"
    :buffer-size="buffer"
    :shift="shift"
    @scroll="onScroll"
  >
    <template #default="{ item, index }">
      <slot :item="item" :index="index" />
    </template>
  </Virtualizer>
</template>
