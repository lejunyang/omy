import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import { ref } from 'vue';

const h = vi.hoisted(() => ({
  configSet: vi.fn(),
  systemProxy: vi.fn(),
  checkConnection: vi.fn(),
}));

const makeConfig = () => ({
  ui: { language: 'auto', theme: 'auto', startup: 'last', view: 'grid' },
  remote: {
    telegram_proxy_mode: 'system',
    telegram_proxy: '',
    scan_omy_only: true,
    scan_concurrency: 8,
    transfer_concurrency: 3,
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
  telegramCheckConnection: h.checkConnection,
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
  te: (key: string, fallback?: string) => key || fallback || '',
  tn: (key: string, count: number) => `${key}:${count}`,
  formatSize: (n: number) => String(n),
}));

vi.mock('../viewport', () => {
  const isMobile = ref(false);
  return {
    isMobile,
    setTestMobile: (value: boolean) => { isMobile.value = value; },
  };
});
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
import { isMobile } from '../viewport';

function setTestMobile(value: boolean) {
  (isMobile as unknown as { value: boolean }).value = value;
}

beforeEach(() => {
  setTestMobile(false);
  h.configSet.mockReset().mockResolvedValue(undefined);
  h.systemProxy.mockReset().mockResolvedValue('socks5://127.0.0.1:6480');
  h.checkConnection.mockReset().mockResolvedValue({ status: 'ok', elapsed_ms: 1, via_proxy: true });
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
    await wrapper.find('[data-sf="telegram_proxy_check"]').trigger('click');
    await flushPromises();

    expect(h.configSet).toHaveBeenCalledTimes(1);
    expect(h.checkConnection).toHaveBeenCalledWith();
    expect(wrapper.find('[data-sf="telegram_proxy_result"]').text()).toBe('settings.telegram_proxy_result_ok');

    await wrapper.find('[data-si="close"]').trigger('click');
    await flushPromises();

    expect(h.configSet).toHaveBeenCalledTimes(2);
    const saved = h.configSet.mock.calls[0][0];
    expect(saved.remote.telegram_proxy_mode).toBe('manual');
    expect(saved.remote.telegram_proxy).toBe('http://127.0.0.1:7897');
    expect(saved.remote.places).toEqual([]);

    wrapper.unmount();
  });

  it('桌面远程设置可配置批量传输并发数', async () => {
    const wrapper = mount(SettingsDialog);
    await flushPromises();
    await wrapper.find('[data-sp="remote"]').trigger('click');
    const input = wrapper.find('[data-sf="transfer_concurrency"]');
    expect(input.exists()).toBe(true);
    await input.setValue('6');
    await wrapper.find('[data-si="close"]').trigger('click');
    await flushPromises();
    expect(h.configSet.mock.calls.at(-1)?.[0].remote.transfer_concurrency).toBe(6);
    wrapper.unmount();
  });

  it('移动端首页有独立代理入口并能进入共用设置页', async () => {
    setTestMobile(true);
    const wrapper = mount(SettingsDialog);
    await flushPromises();

    const proxyEntry = wrapper.find('[data-sp="telegram_proxy_mobile"]');
    expect(proxyEntry.exists()).toBe(true);
    await proxyEntry.trigger('click');

    expect(wrapper.find('[data-sf="telegram_proxy_mode"]').exists()).toBe(true);
    expect(wrapper.find('[data-sf="telegram_system_proxy"]').text()).toBe('socks5://127.0.0.1:6480');
    wrapper.unmount();
  });
});

describe('SettingsDialog 通用组', () => {
  const fieldsIn = (w: ReturnType<typeof mount>, sel: string) =>
    w.findAll(`${sel} select`).map((s) => ({
      sf: (s.attributes('data-sf') || '').replace(/^m_/, ''),
      options: s.findAll('option').map((o) => o.attributes('value')),
    }));

  it('桌面面板与移动端平铺列出同样的字段、同样的顺序与选项', async () => {
    // 不这样会怎样：两处模板各写一份，曾经顺序就已经对不上；给一边加了
    // 选项，另一边的用户永远选不到，也没有任何检查会报错
    const desk = mount(SettingsDialog);
    await flushPromises();
    const deskFields = fieldsIn(desk, '[data-pane="general"]');
    desk.unmount();

    setTestMobile(true);
    const mob = mount(SettingsDialog);
    await flushPromises();
    const mobFields = fieldsIn(mob, '.mlist .mqrow');
    mob.unmount();

    expect(deskFields.map((f) => f.sf)).toEqual(['language', 'theme', 'view', 'startup']);
    expect(mobFields).toEqual(deskFields);
  });

  it('移动端改语言立即生效并写进配置', async () => {
    setTestMobile(true);
    const w = mount(SettingsDialog);
    await flushPromises();
    await w.find('[data-sf="m_language"]').setValue('en');
    // 不这样会怎样：两处共用一套字段后，语言若走了「只写配置」的分支，
    // 界面要等重启才切过去，用户以为没选上
    expect(w.emitted('lang')?.at(-1)).toEqual(['en']);
    expect((w.find('[data-sf="m_language"]').element as HTMLSelectElement).value).toBe('en');
    w.unmount();
  });
});

describe('SettingsDialog 移动端底栏离开', () => {
  it('离开前保存在设置里做的改动', async () => {
    setTestMobile(true);
    const w = mount(SettingsDialog);
    await flushPromises();
    await w.find('[data-sf="m_view"]').setValue('list');
    await w.find('[data-si="nav-files"]').trigger('click');
    await flushPromises();
    // 不这样会怎样：手机上没有关闭按钮，底栏是主要出口；不保存的话改的
    // 默认视图、自动锁定、加密默认值全部丢失，语言主题重启后也变回去
    expect(h.configSet).toHaveBeenCalledTimes(1);
    expect(h.configSet.mock.calls[0][0].ui.view).toBe('list');
    expect(w.emitted('close')).toBeTruthy();
    w.unmount();
  });

  it('保存失败时留在设置页并显示错误', async () => {
    setTestMobile(true);
    h.configSet.mockRejectedValue(new Error('disk full'));
    const w = mount(SettingsDialog);
    await flushPromises();
    await w.find('[data-si="nav-places"]').trigger('click');
    await flushPromises();
    // 不这样会怎样：页面关掉了，用户以为改好了，下次启动才发现没生效
    expect(w.emitted('places')).toBeFalsy();
    expect(w.find('[data-sf="m_error"]').text()).toBe('settings.save_failed');
    w.unmount();
  });
});
