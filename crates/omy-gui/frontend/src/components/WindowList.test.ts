/**
 * WindowList 边缘加载触发的回归测试。
 *
 * 这是「无限加载循环」的核心防线，专门钉住从 CDP 实测里总结出的行为
 * （几何数据取自真实 GUI：clientHeight≈660、阈值 600）：
 *
 * - 滚动**持续高速进行**时（引用跳转的程序化平滑滚动就是这样，每帧派发 scroll、
 *   scrollTop 跨越大几百 px），即使中途贴在底部阈值内，也**绝不**触发 reach-end；
 * - 滚动**静止**（连续 150ms 无 scroll 事件）且停在边缘时，才触发一次；
 * - 同一轮静止只触发一次，不会因为再检查/抖动重复加载；
 * - 离开边缘后复位，下次停靠能再次触发；
 * - 顶部同理（reach-start，消息时间线往更新翻）。
 *
 * 不碰真实布局：用 happy-dom 的真实 div 当滚动容器，几何属性直接赋值，
 * scroll 事件手动派发，计时器用 fake timer。virtua 本体 stub 掉（窗口化/测量
 * 不归这里测），只验证我们自己的边缘判定逻辑。
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { defineComponent, h } from 'vue';
import { mount } from '@vue/test-utils';
import { nextTick } from 'vue';

// virtua 替身：渲染一个普通 div 承载默认插槽，边缘检测由我们自己监听 scrollParent。
// 顺带按 virtua 0.52 Virtualizer 的真实规则记下每行 key（lib/vue/index.js 里的 U：
// 插槽只渲染出一个 vnode 且它有 key 时用它，否则用 `_下标`），供行 key 用例断言。
// 用 vi.mock 而不是 stubs：virtua 的组件没有 name，按名字 stub 不会生效。
const rowKeys = vi.hoisted(() => [] as unknown[]);
vi.mock('virtua/vue', async () => {
  const { defineComponent: dc, h: hh } = await import('vue');
  return {
    Virtualizer: dc({
      props: ['data', 'scrollRef', 'bufferSize', 'shift'],
      setup(props, { slots }) {
        return () =>
          hh('div', { class: 'virtua-stub' },
            (props.data as unknown[]).flatMap((item, index) => {
              const r = slots.default?.({ item, index }) ?? [];
              rowKeys.push(r.length === 1 && r[0].key != null ? r[0].key : '_' + index);
              return r;
            }));
      },
    }),
  };
});

import WindowList from './WindowList.vue';

const SETTLE_MS = 150;

/** 造一个可手动驱动几何与 scroll 事件的「滚动容器」（真实 happy-dom div）。 */
function makeScroller(): HTMLDivElement {
  const el = document.createElement('div');
  // happy-dom 不做布局，这些属性可写
  let st = 0;
  let sh = 3000;
  const ch = 660;
  Object.defineProperty(el, 'clientHeight', { get: () => ch, configurable: true });
  Object.defineProperty(el, 'scrollHeight', { get: () => sh, set: (v) => { sh = v; }, configurable: true });
  Object.defineProperty(el, 'scrollTop', {
    get: () => st,
    set: (v) => { st = v; },
    configurable: true,
  });
  return el;
}

function mountList(scroller: HTMLDivElement) {
  return mount(WindowList, {
    props: {
      items: Array.from({ length: 50 }, (_, i) => ({ id: i })),
      scrollParent: scroller,
    },
  });
}

function fireScroll(el: HTMLElement, top: number) {
  (el as HTMLDivElement).scrollTop = top;
  el.dispatchEvent(new Event('scroll', { bubbles: false }));
}

describe('WindowList 行 key（防滚动时整行重建闪烁）', () => {
  it('每行 key 取自条目的 key / id，互不相同', () => {
    rowKeys.length = 0;
    mount(WindowList, {
      props: {
        items: [{ key: 'm:1' }, { key: 'm:2' }, { id: 'tg:9:3' }, { other: true }],
        scrollParent: makeScroller(),
      },
    });
    // 不这样会怎样：<slot> 渲染成的 Fragment 默认 key 恒为 `_default`，四行 key 全相同，
    // 可视范围一变 Vue 就把行卸掉重建，缩略图反复空白——即用户看到的滚动闪烁。
    // 下标兜底在 Vue 的 renderSlot 里会被拼成字符串，所以是 '3' 而不是 3
    expect(rowKeys).toEqual(['m:1', 'm:2', 'tg:9:3', '3']);
  });
});

describe('WindowList 边缘加载触发（防无限循环）', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('持续高速滚动、即使贴着底部阈值也不触发（引用跳转平滑滚动场景）', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    // 模拟 CDP 实测的程序化平滑滚动：scrollTop 从 ~1189 一路涨，每帧 +~1400px，
    // 且始终满足 scrollHeight - (scrollTop+660) <= 600（贴底）。共派发 20 帧，
    // 帧间隔远小于 SETTLE_MS（用 16ms 模拟一帧）。
    let top = 1189;
    for (let i = 0; i < 20; i++) {
      top += 1400;
      // 内容随加载变高，保持始终贴底
      Object.defineProperty(sc, 'scrollHeight', { value: top + 700, configurable: true });
      fireScroll(sc, top);
      vi.advanceTimersByTime(16); // 不到 SETTLE_MS
    }
    // 关键回归断言：高速滚动期间一次都不该触发
    expect(w.emitted('reach-end')).toBeUndefined();
    expect(w.emitted('reach-start')).toBeUndefined();

    // 即便再推进很久——只要中途仍在持续滚动（每 <150ms 一次），就一直不触发
    for (let i = 0; i < 10; i++) {
      top += 1400;
      fireScroll(sc, top);
      vi.advanceTimersByTime(100);
    }
    expect(w.emitted('reach-end')).toBeUndefined();
  });

  it('滚动静止且停在底部阈值内 → 150ms 后触发一次 reach-end', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    // 用户翻到底：rest = 3000-(2450+660) = -110 <= 600
    fireScroll(sc, 2450);
    expect(w.emitted('reach-end')).toBeUndefined(); // 静止前不触发
    vi.advanceTimersByTime(SETTLE_MS - 1);
    expect(w.emitted('reach-end')).toBeUndefined();
    vi.advanceTimersByTime(2); // 满 150ms
    expect(w.emitted('reach-end')).toHaveLength(1);
  });

  it('同一轮静止只触发一次：静止后再无 scroll，不会重复 emit', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    fireScroll(sc, 2450);
    vi.advanceTimersByTime(SETTLE_MS);
    expect(w.emitted('reach-end')).toHaveLength(1);
    // 即使时间继续走、又碰巧有迟到的 check（无新 scroll 不会重排），也只一次
    vi.advanceTimersByTime(2000);
    expect(w.emitted('reach-end')).toHaveLength(1);
  });

  it('离开底部边缘后复位，再次停靠到底能再次触发（正常连续翻页）', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    // 第一次停到底
    fireScroll(sc, 2450);
    vi.advanceTimersByTime(SETTLE_MS);
    expect(w.emitted('reach-end')).toHaveLength(1);

    // 加载后内容变高，用户先向上离开边缘（reset 锁）
    Object.defineProperty(sc, 'scrollHeight', { value: 3600, configurable: true });
    fireScroll(sc, 1000);
    vi.advanceTimersByTime(SETTLE_MS); // 停在中间，不触发但复位
    expect(w.emitted('reach-end')).toHaveLength(1);

    // 用户再翻到新的底部
    fireScroll(sc, 2950); // rest = 3600-(2950+660) = -10
    vi.advanceTimersByTime(SETTLE_MS);
    expect(w.emitted('reach-end')).toHaveLength(2);
  });

  it('停在中间（两侧都不贴）静止后不触发任何方向', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    fireScroll(sc, 1200); // rest=1140>600，top=1200>600
    vi.advanceTimersByTime(SETTLE_MS);
    expect(w.emitted('reach-end')).toBeUndefined();
    expect(w.emitted('reach-start')).toBeUndefined();
  });

  it('静止在顶部阈值内 → 触发 reach-start（消息时间线往更新翻）', async () => {
    const sc = makeScroller();
    const w = mountList(sc);
    await nextTick();

    fireScroll(sc, 100); // <= startThreshold 600
    vi.advanceTimersByTime(SETTLE_MS);
    expect(w.emitted('reach-start')).toHaveLength(1);
    expect(w.emitted('reach-end')).toBeUndefined();
  });
});
