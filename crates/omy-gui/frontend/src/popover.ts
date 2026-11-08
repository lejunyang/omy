/** 锚定在某个触发元素上的小浮层（下拉选择、日期选择共用）。
 *
 * 浮层一律 Teleport 到 body、用 fixed 定位：它们多半出现在可滚动的对话框里，
 * 留在原处会被 `overflow: auto` 裁掉一半。代价是要自己处理三件事——按触发元素
 * 的位置摆放（下方放不下就翻到上方）、点外面关闭、祖先滚动时收起。三处放在
 * 一起，两个组件就不会各写一份、各漏一种情况。 */
import { nextTick, onBeforeUnmount, ref, type Ref } from 'vue';

const GAP = 4;
const MARGIN = 8;

export function usePopover(
  trigger: Ref<HTMLElement | null>,
  panel: Ref<HTMLElement | null>,
  opts: { matchWidth?: boolean; onClose?: () => void } = {},
) {
  const open = ref(false);
  const style = ref<Record<string, string>>({});
  /** 首帧还不知道浮层多高，先隐身量尺寸再摆，避免先闪在下方再跳到上方。 */
  const measuring = ref(false);

  function place() {
    const t = trigger.value;
    const p = panel.value;
    if (!t || !p) return;
    const r = t.getBoundingClientRect();
    const vw = window.innerWidth;
    const vh = window.innerHeight;
    const below = vh - r.bottom - GAP - MARGIN;
    const above = r.top - GAP - MARGIN;
    const ph = p.scrollHeight;
    const up = ph > below && above > below;
    const room = Math.max(120, up ? above : below);
    const h = Math.min(ph, room);
    const w = Math.max(p.offsetWidth, opts.matchWidth ? r.width : 0);
    const left = Math.max(MARGIN, Math.min(r.left, vw - MARGIN - w));
    style.value = {
      top: `${Math.max(MARGIN, up ? r.top - GAP - h : r.bottom + GAP)}px`,
      left: `${left}px`,
      maxHeight: `${room}px`,
      ...(opts.matchWidth ? { minWidth: `${r.width}px` } : {}),
      '--origin': up ? 'bottom left' : 'top left',
    };
  }

  function onPointerDown(e: PointerEvent) {
    const target = e.target as Node | null;
    if (!target) return;
    if (trigger.value?.contains(target) || panel.value?.contains(target)) return;
    hide();
  }

  function onScroll(e: Event) {
    // 浮层自己内部滚动（选项很多时）不用管
    if (panel.value && e.target instanceof Node && panel.value.contains(e.target)) return;
    // 外层一滚就收起，和原生下拉一致。跟着重摆不行：触发元素滚出对话框的
    // 可视区后，浮层会悬在一块和它无关的内容上（实测踩到）
    hide();
  }

  function listen(on: boolean) {
    const m = on ? 'addEventListener' : 'removeEventListener';
    document[m]('pointerdown', onPointerDown as EventListener, true);
    window[m]('scroll', onScroll, true);
    window[m]('resize', place);
  }

  async function show() {
    if (open.value) return;
    open.value = true;
    measuring.value = true;
    listen(true);
    await nextTick();
    place();
    measuring.value = false;
  }

  function hide() {
    if (!open.value) return;
    open.value = false;
    listen(false);
    opts.onClose?.();
  }

  onBeforeUnmount(() => {
    if (open.value) listen(false);
  });

  return { open, style, measuring, show, hide, toggle: () => (open.value ? hide() : void show()), place };
}
