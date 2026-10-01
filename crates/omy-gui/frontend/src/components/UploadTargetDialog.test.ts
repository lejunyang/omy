import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const h = vi.hoisted(() => ({
  state: {} as any,
  confirm: vi.fn(),
  cancel: vi.fn(),
  browse: vi.fn(),
  caps: vi.fn(),
}));

vi.mock('../store', async () => {
  const { reactive } = await import('vue');
  h.state = reactive({
    uploadTo: {
      source: 'remote',
      item: { placeId: 'src', path: 'tg:1:2', name: 'a.bin', size: 10 },
    },
    remotePlaces: [
      { id: 'p1', name: 'WebDAV', kind: 'webdav', caps: { write: true } },
      { id: 'p2', name: '只读位置', kind: 'webdav', caps: { write: false } },
    ],
  });
  return {
    state: h.state,
    cancelUploadTo: h.cancel,
    confirmUploadTo: h.confirm,
  };
});

vi.mock('../api', () => ({
  remoteBrowse: h.browse,
  remoteEffectiveCaps: h.caps,
  remotePlaceList: vi.fn(),
  errCode: () => 'remote_failed',
}));

vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
  te: (_code: string, fallback: string) => fallback,
}));

import UploadTargetDialog from './UploadTargetDialog.vue';

beforeEach(() => {
  h.state.uploadTo = {
    source: 'remote',
    item: { placeId: 'src', path: 'tg:1:2', name: 'a.bin', size: 10 },
  };
  h.state.remotePlaces = [
    { id: 'p1', name: 'WebDAV', kind: 'webdav', caps: { write: true } },
    { id: 'p2', name: '只读位置', kind: 'webdav', caps: { write: false } },
  ];
  h.browse.mockReset();
  h.caps.mockReset();
  h.confirm.mockReset();
  h.cancel.mockReset();
});

describe('UploadTargetDialog', () => {
  it('只列位置级可能可写的目标，并以目录有效能力决定是否能确认', async () => {
    h.browse.mockResolvedValue([]);
    h.caps.mockResolvedValue({ write: false, rename: false, delete: false });
    const wrapper = mount(UploadTargetDialog);
    await flushPromises();

    expect(wrapper.findAll('.place-list .target-row')).toHaveLength(1);
    expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeDefined();
    expect(wrapper.text()).toContain('upload_to.readonly');
  });

  it('进入子目录后重新查询该目录能力，可写时允许提交', async () => {
    h.browse
      .mockResolvedValueOnce([{ id: '/docs', name: 'docs', is_dir: true }])
      .mockResolvedValueOnce([]);
    h.caps
      .mockResolvedValueOnce({ write: false, rename: false, delete: false })
      .mockResolvedValueOnce({ write: true, rename: true, delete: true });
    h.confirm.mockResolvedValue(true);

    const wrapper = mount(UploadTargetDialog);
    await flushPromises();
    await wrapper.find('.folder.target-row').trigger('click');
    await flushPromises();

    expect(h.caps).toHaveBeenLastCalledWith('p1', '/docs');
    expect(wrapper.find('button[type="submit"]').attributes('disabled')).toBeUndefined();
  });

  it('切换位置后丢弃旧位置晚到的目录结果', async () => {
    h.state.remotePlaces = [
      { id: 'p1', name: '旧位置', kind: 'webdav', caps: { write: true } },
      { id: 'p2', name: '新位置', kind: 'webdav', caps: { write: true } },
    ];
    let resolveOld: (value: unknown[]) => void = () => {};
    const oldRows = new Promise<unknown[]>((resolve) => { resolveOld = resolve; });
    h.browse
      .mockImplementationOnce(() => oldRows)
      .mockResolvedValueOnce([{ id: '/new', name: 'new', is_dir: true }]);
    h.caps.mockResolvedValue({ write: true, rename: false, delete: false });

    const wrapper = mount(UploadTargetDialog);
    await Promise.resolve();
    await wrapper.findAll('.place-list .target-row')[1].trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('new');

    resolveOld([{ id: '/old', name: 'old', is_dir: true }]);
    await flushPromises();
    expect(wrapper.text()).not.toContain('old');
    expect(wrapper.text()).toContain('new');
  });

  it('内存模式原样提交给 store，避免静默改回磁盘缓存', async () => {
    h.browse.mockResolvedValue([]);
    h.caps.mockResolvedValue({ write: true, rename: false, delete: false });
    h.confirm.mockResolvedValue(true);

    const wrapper = mount(UploadTargetDialog);
    await flushPromises();
    await wrapper.find('input[value="memory"]').setValue(true);
    await wrapper.find('form').trigger('submit');
    await flushPromises();

    expect(h.confirm).toHaveBeenCalledWith('p1', '', 'memory');
  });
});
