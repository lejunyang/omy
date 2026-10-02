import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { ref } from 'vue';

const h = vi.hoisted(() => ({
  configSet: vi.fn(),
  systemProxy: vi.fn(),
}));

const makeConfig = () => ({
  ui: { language: 'auto', theme: 'auto', startup: 'last', view: 'grid' },
  remote: {
    telegram_proxy_mode: 'system',
    telegram_proxy: '',
    scan_omy_only: true,
    scan_concurrency: 8,
    cache_limit: 2 * 1024 * 1024 * 1024,
    cache_dir: null,
    clear_cache_on_exit: false,
    cache_wifi_only: false,
    places: [],
  },
  security: { auto_lock_secs: 0, lock_on_background: false, wipe_temp_plaintext: true },
  defaults: { kdf_profile: 'interactive', name_mode: 'encrypt', original_action: 'keep' },
  playback: {},
  devices: {},
  password_managers: { keepassxc: { proxy_path: null, associations: [] } },
});

vi.mock('../api', () => ({
  configGet: vi.fn(async () => makeConfig()),
  configPaths: vi.fn(async () => ({ config: '', cache: '', data: '', log: '', portable: false })),
  telegramSystemProxy: h.systemProxy,
  deviceKeyStatus: vi.fn(async () => ({ available: false, enrolled: false })),
  remoteCacheUsage: vi.fn(async () => ({ used: 0, limit: 0, root: '', pinned_used: 0, pinned_files: 0 })),
  credentialCount: vi.fn(async () => 0),
  appAbout: vi.fn(async () => ({ app_version: 'test' })),
  remotePlaceList: vi.fn(async () => []),
  configSet: h.configSet,
  remoteCacheApply: vi.fn(async () => {}),
  listFileAssociations: vi.fn(async () => ({ supported: false, items: [] })),
  errCode: vi.fn(() => ''),
}));

vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
  te: (key: string) => key,
  tn: (key: string, count: number) => `${key}:${count}`,
  formatSize: (n: number) => String(n),
}));

vi.mock('../viewport', () => ({ isMobile: ref(false) }));
vi.mock('../mobile-platform', () => ({
  isAndroid: false,
  registerMobileBack: vi.fn(() => () => {}),
}));
vi.mock('../theme', () => ({ theme: ref('auto'), setTheme: vi.fn() }));
vi.mock('../store', () => ({
  state: { cwd: '', remotePlaces: [] },
  setNotice: vi.fn(),
  reloadRemotePlaces: vi.fn(async () => {}),
}));

import SettingsDialog from './SettingsDialog.vue';

beforeEach(() => {
  h.configSet.mockReset().mockResolvedValue(undefined);
  h.systemProxy.mockReset().mockResolvedValue('socks5://127.0.0.1:6480');
});

describe('SettingsDialog Telegram 全局代理', () => {
  it('自动模式展示当前系统代理，手动模式保存全局地址', async () => {
    const wrapper = mount(SettingsDialog);
    await flushPromises();

    await wrapper.find('[data-sp="remote"]').trigger('click');
    expect(wrapper.find('[data-sf="telegram_system_proxy"]').text()).toBe('socks5://127.0.0.1:6480');
    expect(wrapper.find('[data-sf="telegram_proxy"]').exists()).toBe(false);

    await wrapper.find('[data-sf="telegram_proxy_mode"]').setValue('manual');
    await wrapper.find('[data-sf="telegram_proxy"]').setValue('http://127.0.0.1:7897');
    await wrapper.find('[data-si="close"]').trigger('click');
    await flushPromises();

    expect(h.configSet).toHaveBeenCalledTimes(1);
    const saved = h.configSet.mock.calls[0][0];
    expect(saved.remote.telegram_proxy_mode).toBe('manual');
    expect(saved.remote.telegram_proxy).toBe('http://127.0.0.1:7897');
    expect(saved.remote.places).toEqual([]);

    wrapper.unmount();
  });
});
