import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const h = vi.hoisted(() => ({
  state: {} as any,
  targets: vi.fn(),
  create: vi.fn(),
  confirm: vi.fn(),
  cancel: vi.fn(),
}));

vi.mock('../store', async () => {
  const { reactive } = await import('vue');
  h.state = reactive({
    telegramForward: { placeId: 'p1', sourceDir: 'tg:-100', messageIds: [3, 9] },
    placeError: '',
  });
  return {
    state: h.state,
    cancelTelegramForward: h.cancel,
    confirmTelegramForward: h.confirm,
  };
});

vi.mock('../api', () => ({
  telegramForwardTargets: h.targets,
  telegramCreateSelfGroup: h.create,
  errCode: () => 'tg_forward_failed',
}));

vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
  tn: (key: string) => key,
  te: (_code: string, fallback: string) => fallback,
}));

import TelegramForwardDialog from './TelegramForwardDialog.vue';

beforeEach(() => {
  h.state.telegramForward = { placeId: 'p1', sourceDir: 'tg:-100', messageIds: [3, 9] };
  h.state.placeError = '';
  h.targets.mockReset();
  h.create.mockReset();
  h.confirm.mockReset();
  h.cancel.mockReset();
});

describe('TelegramForwardDialog', () => {
  it('渲染后端返回的可写群组或个人，并把所选目标交给 store', async () => {
    h.targets.mockResolvedValue([
      { dir_id: 'tg:1', title: '我的群', kind: 'group' },
      { dir_id: 'tg:2', title: '联系人', kind: 'user' },
    ]);
    h.confirm.mockResolvedValue(2);
    const wrapper = mount(TelegramForwardDialog);
    await flushPromises();

    expect(h.targets).toHaveBeenCalledWith('p1');
    expect(wrapper.findAll('.target-row')).toHaveLength(2);
    await wrapper.find('input[value="tg:2"]').setValue(true);
    await wrapper.find('form').trigger('submit');
    await flushPromises();
    expect(h.confirm).toHaveBeenCalledWith('tg:2');
  });

  it('新建仅自己群组后只选中新目标，不自动转发', async () => {
    h.targets.mockResolvedValue([]);
    h.create.mockResolvedValue({ dir_id: 'tg:9', title: '私有中转', kind: 'group' });
    const wrapper = mount(TelegramForwardDialog);
    await flushPromises();

    const input = wrapper.find('input[placeholder="tg_forward.group_name"]');
    await input.setValue('私有中转');
    await wrapper.find('.create-row button').trigger('click');
    await flushPromises();

    expect(h.create).toHaveBeenCalledWith('p1', '私有中转');
    const created = wrapper.find('input[value="tg:9"]');
    expect((created.element as HTMLInputElement).checked).toBe(true);
    expect(h.confirm).not.toHaveBeenCalled();
  });
});
