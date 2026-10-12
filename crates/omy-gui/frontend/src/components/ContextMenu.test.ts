/**
 * ContextMenu 的模板与定位行为。
 *
 * 动画本身（时长、曲线）不在这里测：happy-dom 不跑动画也不做布局。
 * 这里只测动画依赖的「数据」是否正确——序号、展开角、测量期隐身——
 * 这些错了，动画看起来就会乱跳，而截图很难稳定地抓到。
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';

const hv = vi.hoisted(() => ({ mobile: null as any }));

vi.mock('../viewport', async () => {
  const { ref } = await import('vue');
  hv.mobile = ref(false);
  return { isMobile: hv.mobile, MOBILE_MAX: 768 };
});

vi.mock('../mobile-platform', () => ({
  isAndroid: false,
  registerMobileBack: () => () => {},
}));

vi.mock('../i18n', () => ({ __v_isRef: false, t: (k: string) => k }));

import ContextMenu from './ContextMenu.vue';

const ITEMS = [
  { key: 'a', label: 'A', icon: 'eye' },
  { key: 'b', label: 'B', icon: 'eye' },
  { key: 'sep' },
  { key: 'sep' },
  { key: 'c', label: 'C', icon: 'eye', danger: true },
];

function mountMenu(props: Record<string, unknown> = {}) {
  return mount(ContextMenu, {
    props: { items: ITEMS, x: 10, y: 10, ...props },
    global: { stubs: { AppIcon: true } },
    attachTo: document.body,
  });
}

beforeEach(() => {
  hv.mobile.value = false;
  Object.defineProperty(window, 'innerWidth', { value: 1000, configurable: true });
  Object.defineProperty(window, 'innerHeight', { value: 800, configurable: true });
});

describe('ContextMenu', () => {
  it('错峰序号跨分隔线连续编号', async () => {
    const w = mountMenu();
    await flushPromises();
    const seq = w.findAll('.mi').map((n) => n.attributes('style'));
    // 不这样会怎样：用 nth-child 或按组编号时，分隔线后的第一项会和前面
    // 某项同时出现，浮现顺序看起来是乱跳的。
    expect(seq).toEqual(['--i: 0;', '--i: 1;', '--i: 2;']);
    // 连续两条分隔线只渲染一条
    expect(w.findAll('.msep')).toHaveLength(1);
    w.unmount();
  });

  it('桌面：量完尺寸后去掉隐身类，未越界时从左上角展开', async () => {
    const w = mountMenu();
    await flushPromises();
    const el = w.find('.ctxmenu');
    // 不这样会怎样：measuring 留着，菜单一直 visibility:hidden，右键「没反应」。
    expect(el.classes()).not.toContain('measuring');
    expect(el.attributes('style')).toContain('--origin: top left');
    w.unmount();
  });

  it('桌面：靠右下角触发时翻转，并从右下角展开', async () => {
    // happy-dom 的 getBoundingClientRect 恒为 0；尺寸必须在 onMounted 测量前就位
    const proto = HTMLElement.prototype.getBoundingClientRect;
    HTMLElement.prototype.getBoundingClientRect = function () {
      return { width: 200, height: 300, top: 0, left: 0, right: 200, bottom: 300 } as DOMRect;
    };
    try {
      const w = mount(ContextMenu, {
        props: { items: ITEMS, x: 990, y: 790 },
        global: { stubs: { AppIcon: true } },
        attachTo: document.body,
      });
      await flushPromises();
      const style = w.find('.ctxmenu').attributes('style');
      // 不这样会怎样：菜单翻到光标左上方，却仍从左上角放大，视觉上是
      // 「从远离手指的一角冒出来再缩回去」，像是跳了一下。
      expect(style).toContain('--origin: bottom right');
      expect(style).toContain('left: 790px');
      expect(style).toContain('top: 490px');
      w.unmount();
    } finally {
      HTMLElement.prototype.getBoundingClientRect = proto;
    }
  });

  it('移动端：底部弹层带标题与拖拽条，且不读触发坐标', async () => {
    hv.mobile.value = true;
    const w = mountMenu({ title: '海边日落.jpg' });
    await flushPromises();
    const el = w.find('.ctxmenu');
    expect(el.classes()).toContain('mobile');
    // 不这样会怎样：弹层从屏幕底部升起、离手指很远，没有标题就不知道
    // 菜单针对的是哪一项，容易对错的文件执行删除。
    expect(w.find('.sheet-title').text()).toBe('海边日落.jpg');
    expect(w.find('.sheet-grip').exists()).toBe(true);
    // 内联坐标只属于桌面浮层；弹层带着它会被 left/top 拽离底部
    expect(el.attributes('style') || '').not.toContain('left:');
    w.unmount();
  });

  it('桌面不渲染弹层头部', async () => {
    const w = mountMenu({ title: 'x' });
    await flushPromises();
    // 不这样会怎样：桌面右键菜单顶部多出一条拖拽条和标题，白占两行。
    expect(w.find('.sheet-title').exists()).toBe(false);
    expect(w.find('.sheet-grip').exists()).toBe(false);
    w.unmount();
  });
});
