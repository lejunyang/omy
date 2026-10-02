import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const h = vi.hoisted(() => ({
  suggest: vi.fn(),
  system: vi.fn(),
  check: vi.fn(),
  cancel: vi.fn(),
}));

vi.mock('../api', () => ({
  TG_PENDING_ACCOUNT: '__pending__',
  onTelegramLogin: vi.fn(async () => () => {}),
  remotePlaceList: vi.fn(async () => [{ id: 'old', kind: 'telegram' }]),
  telegramSuggestProxy: h.suggest,
  telegramSystemProxy: h.system,
  telegramCheckConnection: h.check,
  telegramCanPersist: vi.fn(async () => true),
  telegramHasSession: vi.fn(async () => false),
  telegramApiIdStatus: vi.fn(async () => ({ builtin: true, configured: true })),
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
  h.suggest.mockReset().mockResolvedValue('socks5://127.0.0.1:7897');
  h.system.mockReset().mockResolvedValue('socks5://127.0.0.1:6480');
  h.check.mockReset().mockResolvedValue({ status: 'ok', elapsed_ms: 1, via_proxy: true });
  h.cancel.mockReset().mockResolvedValue(undefined);
});

describe('TelegramLoginDialog 代理来源', () => {
  it('读取系统代理按钮绕过已有位置的历史代理', async () => {
    const wrapper = mount(TelegramLoginDialog);
    await flushPromises();

    await wrapper.find('[data-tg="m-qr"]').trigger('click');
    const input = wrapper.find('[data-tg="proxy"]');
    expect((input.element as HTMLInputElement).value).toBe('socks5://127.0.0.1:7897');

    await wrapper.find('[data-tg="px-sys"]').trigger('click');
    await flushPromises();

    expect(h.system).toHaveBeenCalledTimes(1);
    expect(h.suggest).toHaveBeenCalledTimes(1);
    expect((input.element as HTMLInputElement).value).toBe('socks5://127.0.0.1:6480');
    expect(h.check).toHaveBeenLastCalledWith('socks5://127.0.0.1:6480');

    wrapper.unmount();
  });
});
