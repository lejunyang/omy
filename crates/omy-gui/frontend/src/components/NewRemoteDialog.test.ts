import { describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';

vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
}));

import NewRemoteDialog from './NewRemoteDialog.vue';

describe('NewRemoteDialog', () => {
  it.each(['webdav', 'telegram', 'virtual'] as const)('选择 %s 时上报对应类型', async (kind) => {
    const wrapper = mount(NewRemoteDialog);
    await wrapper.find(`[data-new-remote="${kind}"]`).trigger('click');
    // 不这样会怎样：三张卡片可能被误接到同一个后续流程，用户选 Telegram 却看到 WebDAV 表单。
    expect(wrapper.emitted('pick')).toEqual([[kind]]);
  });

  it('点击取消关闭选择弹窗', async () => {
    const wrapper = mount(NewRemoteDialog);
    await wrapper.find('[data-new-remote="cancel"]').trigger('click');
    // 不这样会怎样：移动端没有其它稳定退出入口，用户会被困在选择层。
    expect(wrapper.emitted('cancel')).toHaveLength(1);
  });
});
