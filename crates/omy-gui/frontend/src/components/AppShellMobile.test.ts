/**
 * 手机顶栏：密码胶囊与「立即锁定」合并成一个入口。
 *
 * 不这样会怎样：两把长得差不多的锁并排，分不清哪个是状态、哪个会把密码清掉。
 */
import { describe, it, expect, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { defineComponent, h } from 'vue';

vi.mock('../viewport.js', async () => {
  const { ref } = await import('vue');
  return { isMobile: ref(true) };
});
vi.mock('../api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));
vi.mock('./SideBar.vue', () => ({ default: defineComponent({ setup: () => () => h('aside') }) }));

import AppShell from './AppShell.vue';
import { state } from '../store';

describe('手机顶栏的密码入口', () => {
  it('已有密码时只有一个入口，菜单里能锁定', async () => {
    state.credentials = 2;
    const w = mount(AppShell, { global: { stubs: { AppIcon: true, Transition: false } }, attachTo: document.body });
    expect(w.find('#btn-lock').exists()).toBe(false);
    await w.find('[data-tb="vault"]').trigger('click');
    await flushPromises();
    const lockItem = document.querySelector('[data-mi="lock"]') as HTMLElement | null;
    expect(lockItem).not.toBeNull();
    lockItem?.click();
    await flushPromises();
    expect(w.emitted('lock')).toHaveLength(1);
    w.unmount();
  });

  it('没有密码时直接弹解锁框', async () => {
    state.credentials = 0;
    const w = mount(AppShell, { global: { stubs: { AppIcon: true } } });
    await w.find('[data-tb="vault"]').trigger('click');
    expect(w.emitted('quick-unlock')).toHaveLength(1);
  });
});
