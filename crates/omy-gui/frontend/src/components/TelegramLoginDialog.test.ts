import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const h = vi.hoisted(() => ({
  suggest: vi.fn(),
  check: vi.fn(),
  loginStart: vi.fn(),
  cancel: vi.fn(),
}));

vi.mock('../api', () => ({
  TG_PENDING_ACCOUNT: '__pending__',
  onTelegramLogin: vi.fn(async () => () => {}),
  remotePlaceList: vi.fn(async () => [{ id: 'old', kind: 'telegram' }]),
  telegramSuggestProxy: h.suggest,
  telegramCheckConnection: h.check,
  telegramCanPersist: vi.fn(async () => true),
  telegramHasSession: vi.fn(async () => false),
  telegramApiIdStatus: vi.fn(async () => ({ builtin: true, configured: true })),
  telegramLoginStart: h.loginStart,
  telegramLoginCancel: h.cancel,
  telegramPhoneCancel: vi.fn(async () => {}),
  errCode: vi.fn(() => ''),
}));

vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
  te: (key: string) => key,
}));

vi.mock('../mobile-platform', () => ({ isAndroid: false }));

import TelegramLoginDialog from './TelegramLoginDialog.vue';

beforeEach(() => {
  h.suggest.mockReset().mockResolvedValue('socks5://127.0.0.1:6480');
  h.check.mockReset().mockResolvedValue({ status: 'ok', elapsed_ms: 1, via_proxy: true });
  h.loginStart.mockReset().mockResolvedValue(undefined);
  h.cancel.mockReset().mockResolvedValue(undefined);
});

describe('TelegramLoginDialog 全局代理', () => {
  it('只读展示全局代理，连接检查和登录都不传临时代理', async () => {
    const wrapper = mount(TelegramLoginDialog);
    await flushPromises();

    await wrapper.find('[data-tg="m-qr"]').trigger('click');
    const proxy = wrapper.find('[data-tg="proxy"]');
    expect(proxy.exists()).toBe(true);
    expect(proxy.element.tagName).toBe('DIV');
    expect(proxy.text()).toBe('socks5://127.0.0.1:6480');
    expect(wrapper.find('[data-tg="px-sys"]').exists()).toBe(false);
    expect(wrapper.find('input[data-tg="proxy"]').exists()).toBe(false);

    await wrapper.find('[data-tg="conn-retry"]').trigger('click');
    await flushPromises();
    expect(h.check).toHaveBeenCalledTimes(1);
    expect(h.check).toHaveBeenCalledWith();

    await wrapper.find('[data-tg="start"]').trigger('click');
    await flushPromises();
    expect(h.loginStart).toHaveBeenCalledTimes(1);
    expect(h.loginStart).toHaveBeenCalledWith();
    expect(h.suggest).toHaveBeenCalledTimes(1);

    wrapper.unmount();
  });
});
