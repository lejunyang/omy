/**
 * 触屏长按：自己的计时器与 WebView 原生 contextmenu 必须合并成一次。
 *
 * 不这样会怎样：菜单被打开两次（第二次按非加选重置多选），或原生事件把刚
 * 打开的菜单关掉——真机上「按住不到一秒只选中、菜单闪一下就没了」。
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mount } from '@vue/test-utils';

vi.mock('../viewport.js', async () => {
  const { ref } = await import('vue');
  return { isMobile: ref(true) };
});
vi.mock('../api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

import EntryCard from './EntryCard.vue';

const entry = { path: '/d/a.jpg', name: 'a.jpg', ext: 'jpg', size: 10 };

describe('EntryCard 长按', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('计时器先到、原生 contextmenu 后到：只开一次菜单', async () => {
    const w = mount(EntryCard, { props: { entry }, global: { stubs: { AppIcon: true } } });
    await w.find('.card').trigger('pointerdown', { pointerType: 'touch', clientX: 5, clientY: 5 });
    vi.advanceTimersByTime(600);
    await w.find('.card').trigger('contextmenu');
    expect(w.emitted('menu')).toHaveLength(1);
    // 选择只发生一次，且是加选（不会把已有多选重置成单选）
    expect(w.emitted('select')).toEqual([[{ ctrlKey: true }]]);
  });

  it('原生 contextmenu 先到：当成长按，计时器不再重复触发', async () => {
    const w = mount(EntryCard, { props: { entry }, global: { stubs: { AppIcon: true } } });
    await w.find('.card').trigger('pointerdown', { pointerType: 'touch', clientX: 5, clientY: 5 });
    vi.advanceTimersByTime(400);
    await w.find('.card').trigger('contextmenu');
    vi.advanceTimersByTime(600);
    expect(w.emitted('menu')).toHaveLength(1);
  });
});
