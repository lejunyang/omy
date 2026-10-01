/** 密码管理器选择器只验证状态分支与事件；秘密根本不属于组件 props。 */
import { mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';

vi.mock('../i18n.js', () => ({
  __v_isRef: false,
  t: (key: string) => key,
}));

import PasswordManagerDialog from './PasswordManagerDialog.vue';

const connected = {
  provider: 'keepassxc' as const,
  installed: true,
  running: true,
  database_open: true,
  associated: true,
  database_hash: 'db',
  version: '2.7.4',
  detail: null,
};

describe('PasswordManagerDialog', () => {
  it('未关联时只给关联入口，不伪装成空数据库', async () => {
    const wrapper = mount(PasswordManagerDialog, {
      props: { purpose: 'unlock', status: { ...connected, associated: false }, entries: [] },
    });
    const button = wrapper.find('[data-pm="connect"]');
    expect(button.exists()).toBe(true);
    await button.trigger('click');
    // 不这样会怎样：未关联被误报成“没有密钥”，用户会反复创建重复条目。
    expect(wrapper.emitted('connect')).toHaveLength(1);
    expect(wrapper.text()).not.toContain('password_manager.none');
  });

  it('点击摘要只回传 UUID，不存在可泄露的密码字段', async () => {
    const entry = { id: 'uuid-1', name: 'omy', login: '个人', group: 'omy' };
    const wrapper = mount(PasswordManagerDialog, {
      props: { purpose: 'unlock', status: connected, entries: [entry] },
    });
    await wrapper.find('[data-pm="entry"]').trigger('click');
    expect(wrapper.emitted('select')?.[0]?.[0]).toEqual(entry);
    // 不这样会怎样：一旦组件契约出现 password，Vue DevTools 就能读出同步密钥。
    expect(JSON.stringify(wrapper.props())).not.toContain('password');
  });

  it('解锁用途不显示创建表单', () => {
    const wrapper = mount(PasswordManagerDialog, {
      props: { purpose: 'unlock', status: connected, entries: [] },
    });
    // 不这样会怎样：用户在“解不开旧文件”的路径里误建一把新钥匙，当然永远打不开。
    expect(wrapper.find('.pm-create').exists()).toBe(false);
  });

  it('加密用途允许命名并生成', async () => {
    const wrapper = mount(PasswordManagerDialog, {
      props: { purpose: 'encrypt', status: connected, entries: [] },
    });
    await wrapper.find('#pm-label').setValue('工作资料');
    await wrapper.find('.pm-create').trigger('submit');
    expect(wrapper.emitted('generate')?.[0]).toEqual(['工作资料']);
  });

  it('Android 不可用时不误报成未安装 KeePassXC', () => {
    const wrapper = mount(PasswordManagerDialog, {
      props: {
        purpose: 'unlock',
        status: {
          ...connected,
          provider: 'android_credential_manager',
          installed: false,
          running: false,
          database_open: false,
          associated: false,
        },
        entries: [],
      },
    });
    // 不这样会怎样：Android 13 用户会被要求安装一个根本不存在于该平台的
    // KeePassXC 桌面程序，而不是得知应改用 Autofill。
    expect(wrapper.text()).toContain('password_manager.requires_android_14');
    expect(wrapper.text()).not.toContain('password_manager.not_installed');
  });

  it('Android 不显示无意义的 KeePassXC 取消关联按钮', () => {
    const wrapper = mount(PasswordManagerDialog, {
      props: {
        purpose: 'unlock',
        status: { ...connected, provider: 'android_credential_manager' },
        entries: [],
      },
    });
    // 不这样会怎样：按钮看似能撤销 Credential Manager 授权，实际后端什么
    // 都没做，会让用户误以为系统 provider 已被解绑。
    expect(wrapper.text()).not.toContain('password_manager.forget');
  });
});
