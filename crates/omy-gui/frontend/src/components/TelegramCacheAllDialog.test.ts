import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const h = vi.hoisted(() => ({ start: vi.fn(), state: null as any }));

vi.mock('../store', async () => {
  const { reactive } = await import('vue');
  h.state = reactive({
    telegramCacheAll: { placeId: 'p1', dir: 'tg:-100', dirName: '测试群' },
    error: '',
  });
  return { state: h.state, startTelegramCacheAll: h.start };
});
vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
}));

import TelegramCacheAllDialog from './TelegramCacheAllDialog.vue';

beforeEach(() => {
  h.state.telegramCacheAll = { placeId: 'p1', dir: 'tg:-100', dirName: '测试群' };
  h.start.mockReset().mockResolvedValue(true);
});

describe('TelegramCacheAllDialog', () => {
  it('按类型、时间与关键字同时提交筛选条件', async () => {
    const wrapper = mount(TelegramCacheAllDialog);
    await wrapper.findAll('input[type="checkbox"]')[1].setValue(true);
    await wrapper.find('[data-cache-range]').setValue('custom');
    const dates = wrapper.findAll('input[type="date"]');
    await dates[0].setValue('2026-09-01');
    await dates[1].setValue('2026-09-30');
    await wrapper.find('input[type="search"]').setValue('报告');
    await wrapper.find('form').trigger('submit');
    await flushPromises();

    expect(h.start).toHaveBeenCalledTimes(1);
    const req = h.start.mock.calls[0][0];
    expect(req.mediaTypes).toEqual(['media', 'audio']);
    expect(req.keyword).toBe('报告');
    expect(req.from).toBeLessThan(req.to);
  });

  it('未选择类型时禁止开始', async () => {
    const wrapper = mount(TelegramCacheAllDialog);
    await wrapper.findAll('input[type="checkbox"]')[0].setValue(false);
    expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeDefined();
    expect(wrapper.text()).toContain('cache_all.type_required');
  });
});
