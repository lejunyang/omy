/**
 * 位置列表与父组件的接口：进入位置只发 open，由 PlaceBrowser 负责切换目录浏览。
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { mount } from '@vue/test-utils';

const hv = vi.hoisted(() => ({ mobile: null as any, state: null as any }));

vi.mock('../viewport', async () => {
  const { ref: r } = await import('vue');
  hv.mobile = r(false);
  return { isMobile: hv.mobile, MOBILE_MAX: 768 };
});
vi.mock('../store', async () => {
  const { reactive: re } = await import('vue');
  hv.state = re({ remotePlaces: [], virtualPlaces: [] });
  return {
    state: hv.state,
    removeRemotePlace: vi.fn(),
    detachTelegramPlace: vi.fn(),
    deleteTelegramAccount: vi.fn(),
    renameTelegramPlace: vi.fn(),
  };
});
vi.mock('../ask', () => ({ askConfirm: vi.fn(async () => true), askText: vi.fn(async () => null) }));

import PlaceList from './PlaceList.vue';

const tg = { id: 'p1', kind: 'telegram', name: 'TG' };

beforeEach(() => {
  hv.mobile.value = false;
  hv.state.remotePlaces = [tg];
  hv.state.virtualPlaces = [{ id: 'v1', name: '精选' }];
});

describe('PlaceList', () => {
  it('桌面：单击行不进入，「进入」按钮与双击才进入', async () => {
    const w = mount(PlaceList);
    await w.find('.placerow').trigger('click');
    // 不这样会怎样：桌面上想点行尾「改名」时一不小心点到行，就被带进了位置
    expect(w.emitted('open')).toBeFalsy();
    await w.find('[data-pb-enter="p1"]').trigger('click');
    await w.find('[data-pb-virtual="v1"]').trigger('dblclick');
    expect(w.emitted('open')).toEqual([[tg], [hv.state.virtualPlaces[0]]]);
  });

  it('手机：单击整行即进入', async () => {
    hv.mobile.value = true;
    const w = mount(PlaceList);
    await w.find('.placerow').trigger('click');
    // 不这样会怎样：触屏没有双击，手机上的位置列表点什么都进不去
    expect(w.emitted('open')).toEqual([[tg]]);
  });

  it('列表不空时也有新建入口', async () => {
    const w = mount(PlaceList);
    await w.find('[data-pb="new-remote"]').trigger('click');
    // 不这样会怎样：手机上加第二个位置得先打开抽屉才找得到入口
    expect(w.emitted('new-remote')).toHaveLength(1);
  });
});
