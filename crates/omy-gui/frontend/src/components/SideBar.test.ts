import { beforeEach, describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { computed, reactive } from 'vue';

const h = vi.hoisted(() => ({ state: null as any }));

vi.mock('../store', async () => {
  h.state = reactive({
    places: [],
    storage: { granted: true, mode: 'not-applicable' },
    remotePlaces: [
      { id: 'w1', kind: 'webdav', name: 'NAS', caps: { write: false }, encrypted: false },
      { id: 't1', kind: 'telegram', name: 'TG', caps: { write: true }, encrypted: false },
    ],
    virtualPlaces: [{ id: 'v1', name: '收藏', encrypted: false }],
    transfers: [],
    pairedCount: 0,
    shareRunning: false,
  });
  const noop = vi.fn();
  return {
    state: h.state,
    activeLocation: computed(() => ({ kind: '', key: '' })),
    leaveOverlays: noop,
    navigate: noop,
    grantStorageAccess: noop,
    openPlaceBrowserAt: noop,
    renameTelegramPlace: noop,
    detachTelegramPlace: noop,
    deleteTelegramAccount: noop,
    encryptTelegramPlace: noop,
    decryptTelegramPlace: noop,
    removeRemotePlace: noop,
    openTransfers: noop,
    promptTgUnlock: noop,
    renameVirtualPlace: noop,
    deleteVirtualPlace: noop,
    promptVirtualEncrypt: noop,
    promptVirtualUnlock: noop,
    lockVirtualPlace: noop,
  };
});
vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
}));

import SideBar from './SideBar.vue';

describe('SideBar 远程位置新建入口', () => {
  beforeEach(() => vi.clearAllMocks());

  it('真实与虚拟位置之后只显示一个新建远程位置入口', () => {
    const wrapper = mount(SideBar, { global: { stubs: { ContextMenu: true } } });
    const remotes = wrapper.findAll('[data-rp]');
    // 不这样会怎样：新建入口插在真实位置中间，或旧入口残留，侧栏仍会出现多个入口。
    expect(remotes.map((node) => node.attributes('data-rp'))).toEqual(['w1', 't1', 'new']);
    expect(wrapper.find('[data-vp="v1"]').exists()).toBe(true);
    expect(wrapper.find('[data-vp="new"]').exists()).toBe(false);
    expect(wrapper.find('[data-tg="entry"]').exists()).toBe(false);
    expect(wrapper.find('[data-rp="new"]').text()).toContain('rplace.new');
  });

  it('点击唯一入口上报 new-remote', async () => {
    const wrapper = mount(SideBar, { global: { stubs: { ContextMenu: true } } });
    await wrapper.find('[data-rp="new"]').trigger('click');
    // 不这样会怎样：入口虽然显示，但父组件收不到事件，选择弹窗不会打开。
    expect(wrapper.emitted('new-remote')).toHaveLength(1);
  });
});
