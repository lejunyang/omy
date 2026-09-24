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
 * - `@reach-end` / `@reach-start`：接近底部/顶部触发，用于双向无限加载。
 * - 暴露 `scrollToIndex`，供"定位到某条并高亮"。
 */
import { ref, computed, watch, onBeforeUnmount } from 'vue';
import { Virtualizer } from 'virtua/vue';

const props = defineProps({
  items: { type: Array, required: true },
  scrollParent: { type: Object, default: null },
  endThreshold: { type: Number, default: 600 },
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

/** 滚动容器当前视口高度（px）。缓冲大小按它动态估算，所以不能写死。 */
const viewportH = ref(0);
let resizeRO = null;

// scrollParent 在进入位置/挂载后才拿到（是个模板 ref），用 watch 而不是
// onMounted 接观察：元素一出现就量一次、窗口或容器尺寸变化时持续更新；
// 元素换掉（不同视图复用本组件）时断开旧的、改观察新的。
watch(
  () => props.scrollParent,
  (el) => {
    if (resizeRO) { resizeRO.disconnect(); resizeRO = null; }
    if (!el || typeof ResizeObserver === 'undefined') return;
    viewportH.value = el.clientHeight;
    resizeRO = new ResizeObserver(() => {
      viewportH.value = el.clientHeight;
    });
    resizeRO.observe(el);
  },
  { immediate: true, flush: 'post' },
);
onBeforeUnmount(() => {
  if (resizeRO) resizeRO.disconnect();
});

/** 缓冲下限/上限（px）。
 *
 * 为什么不能只写「1.5 个视口」而还要夹上下限：
 * - 窗口极矮（小窗、横屏手机 ~300px）时 1.5 倍只有四百多 px，快速甩动仍可能露白，
 *   所以抬到下限；
 * - 窗口极高（4K 全屏 ~1400px）时 1.5 倍会让视口外常备 2000+ px、三倍于视口的
 *   DOM，消息行又高低不齐，多渲染太多反而拖慢，所以截到上限。
 */
const MIN_BUFFER = 600;
const MAX_BUFFER = 2000;
/**
 * 视口外多渲染多高的缓冲（virtua 的 bufferSize），随滚动容器高度自动变化：
 *
 * virtua 默认只有 200px：快速滚动（尤其触屏甩动）时，新条目还没完成测量挂载就
 * 进了视口，肉眼看到「滚的时候上面一片空白、停下来条目才冒出来」。常备约 1.5 个
 * 视口高的缓冲，上下两侧都有一屏多已挂载条目，不同分辨率/窗口大小都不露白；
 * 多渲染的条目数仍与列表总长无关，长列表 DOM 节点数恒定。
 *
 * 还没量到高度（首帧）时用 MIN_BUFFER 兜底，避免先以 200 渲染再跳一次。
 */
const buffer = computed(() => {
  const h = viewportH.value;
  if (!h) return MIN_BUFFER;
  return Math.min(MAX_BUFFER, Math.max(MIN_BUFFER, Math.round(h * 1.5)));
});

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
