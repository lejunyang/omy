/**
 * 桌面顶栏的网格/列表切换必须写进配置。
 *
 * 不这样会怎样：AppShell 曾自带一个只改内存的 setView，桌面上切成列表、
 * 重启后又回到网格，设置里的「默认视图」也跟着失真。
 */
import { describe, it, expect, vi } from 'vitest';
import { mount, flushPromises } from '@vue/test-utils';
import { defineComponent, h } from 'vue';

vi.mock('../api.js', () => ({
  configGet: vi.fn(async () => ({ ui: { view: 'grid' } })),
  configSet: vi.fn(async () => null),
  errCode: () => undefined,
  uiLog: vi.fn(),
}));
vi.mock('./SideBar.vue', () => ({
  default: defineComponent({ setup: () => () => h('aside') }),
}));

import * as api from '../api';
import AppShell from './AppShell.vue';
import { state } from '../store';

describe('AppShell 视图切换', () => {
  it('切到列表视图会持久化到配置', async () => {
    const w = mount(AppShell, { global: { stubs: { AppIcon: true } } });
    await w.find('[data-tb="view-list"]').trigger('click');
    await flushPromises();
    expect(state.view).toBe('list');
    expect(api.configSet).toHaveBeenCalledWith(expect.objectContaining({ ui: { view: 'list' } }));
  });
});
