/** 高级搜索弹窗的编排：条件 → 查询、分页续扫、全选后批量动作带的范围。
 *  后端 IPC 整个 mock 掉；布局与滚动几何不在这里测。 */
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';
import type { TgSearchHit } from '../types';

const h = vi.hoisted(() => ({ search: vi.fn(), batch: vi.fn(), state: null as any }));

vi.mock('../store', async () => {
  const { reactive } = await import('vue');
  h.state = reactive({
    telegramAdvancedSearch: { placeId: 'p1', dir: 'tg:-100', dirName: '测试群', protected: false },
    showThumbnails: false,
    error: '',
  });
  return {
    state: h.state,
    searchTelegramAdvanced: h.search,
    startTelegramAdvancedBatch: h.batch,
    placeThumbUrl: (t: string) => `/pthumb/${t}`,
  };
});
vi.mock('../i18n', () => ({
  __v_isRef: false,
  lang: { value: 'zh-CN' },
  t: (key: string, p?: Record<string, unknown>) => (p ? `${key}${JSON.stringify(p)}` : key),
  tn: (key: string, n: number) => `${key}:${n}`,
  te: (_c: string, fb: string) => fb,
  formatSize: (n: number) => `${n}B`,
}));

import TelegramAdvancedSearchDialog from './TelegramAdvancedSearchDialog.vue';

const hit = (n: number): TgSearchHit => ({
  id: `tg:-100:${n}`, message: n, date: 1_760_000_000, text: '', media: 'video',
  file_name: `v${n}.mp4`, file_size: 1000, thumb_token: null,
});

beforeEach(() => {
  h.state.telegramAdvancedSearch = { placeId: 'p1', dir: 'tg:-100', dirName: '测试群', protected: false };
  h.search.mockReset();
  h.batch.mockReset().mockResolvedValue(true);
});

// 下拉与日历的面板 Teleport 到 body，打桩成原地渲染才能在 wrapper 里找到
const mountDialog = () => mount(TelegramAdvancedSearchDialog, { global: { stubs: { teleport: true } } });

async function searchVideos() {
  const w = mountDialog();
  await w.find('[data-adv-media="video"]').trigger('click');
  await w.find('form').trigger('submit');
  await flushPromises();
  return w;
}

describe('TelegramAdvancedSearchDialog', () => {
  it('提交后按条件发首页请求，结果列在下方', async () => {
    h.search.mockResolvedValueOnce({ hits: [hit(9), hit(8)], cursor: 'c1', scanned: 50 });
    const w = await searchVideos();
    expect(h.search).toHaveBeenCalledWith(
      { match: 'all', conditions: [{ kind: 'media', types: ['video'], negate: false }] },
      null,
    );
    expect(w.findAll('[data-adv-hit]')).toHaveLength(2);
  });

  it('条件不完整时不发请求', async () => {
    const w = mountDialog();
    await w.find('form').trigger('submit');
    await flushPromises();
    // 不这样会怎样：一个没选媒体类型的条件被发出去，后端按「什么都不限」搜
    expect(h.search).not.toHaveBeenCalled();
    expect(w.text()).toContain('adv_search.err_media');
  });

  it('续扫带上一页的 cursor，扫完后不再请求', async () => {
    h.search
      .mockResolvedValueOnce({ hits: [hit(9)], cursor: 'c1', scanned: 50 })
      .mockResolvedValueOnce({ hits: [hit(3)], cursor: null, scanned: 90 });
    const w = await searchVideos();
    await w.find('[data-adv-more]').trigger('click');
    await flushPromises();
    // 不这样会怎样：续扫从头再扫，结果重复、永远扫不完
    expect(h.search.mock.calls[1][1]).toBe('c1');
    expect(w.findAll('[data-adv-hit]')).toHaveLength(2);
    expect(w.find('[data-adv-more]').exists()).toBe(false);
  });

  it('全选后取消一条再执行：发 all + except，而不是已加载的 id 列表', async () => {
    h.search.mockResolvedValueOnce({ hits: [hit(9), hit(8), hit(7)], cursor: 'c1', scanned: 50 });
    const w = await searchVideos();
    await w.find('[data-adv-select-all]').setValue(true);
    await w.findAll('[data-adv-hit] input')[1].setValue(false);
    // 还没扫完：数量必须显示成下界
    expect(w.find('[data-adv-picked]').text()).toContain('selected_at_least');
    await w.find('[data-adv-act="pin"]').trigger('click');
    await flushPromises();
    // 不这样会怎样：只把已加载的两条发给后端，「全选」实际只处理了首屏
    expect(h.batch).toHaveBeenCalledWith(
      { match: 'all', conditions: [{ kind: 'media', types: ['video'], negate: false }] },
      { mode: 'all', except: ['tg:-100:8'] },
      'pin',
    );
  });

  it('批量动作作用于上次搜索的查询，而不是之后改过但没重搜的表单', async () => {
    h.search.mockResolvedValueOnce({ hits: [hit(9)], cursor: null, scanned: 10 });
    const w = await searchVideos();
    await w.find('[data-adv-media="photo"]').trigger('click');
    await w.find('[data-adv-hit] input').setValue(true);
    await w.find('[data-adv-act="add_virtual"]').trigger('click');
    await flushPromises();
    // 不这样会怎样：屏幕上列的是视频，动作却按「视频或图片」去遍历
    expect(h.batch.mock.calls[0][0].conditions[0].types).toEqual(['video']);
    expect(h.batch.mock.calls[0][1]).toEqual({ mode: 'ids', ids: ['tg:-100:9'] });
  });

  it('受保护对话禁用转发，其它动作照常', async () => {
    h.state.telegramAdvancedSearch.protected = true;
    h.search.mockResolvedValueOnce({ hits: [hit(9)], cursor: null, scanned: 10 });
    const w = await searchVideos();
    await w.find('[data-adv-hit] input').setValue(true);
    expect(w.find('[data-adv-act="forward"]').attributes('disabled')).toBeDefined();
    expect(w.find('[data-adv-act="pin"]').attributes('disabled')).toBeUndefined();
  });

  it('添加子组并切换为「都不满足」：查询里是带取反的子组', async () => {
    h.search.mockResolvedValueOnce({ hits: [], cursor: null, scanned: 0 });
    const w = mountDialog();
    await w.find('[data-adv-media="video"]').trigger('click');
    await w.find('[data-adv-add-group]').trigger('click');
    const sub = w.find('[data-adv-group="1"]');
    await sub.find('[data-adv-media="photo"]').trigger('click');
    // 子组的组合方式下拉：全部 / 任一 / 不全 / 都不 → 选第 4 项
    await sub.find('[data-adv-match]').trigger('click');
    await sub.findAll('.osel-opt')[3].trigger('click');
    await w.find('form').trigger('submit');
    await flushPromises();
    // 不这样会怎样：组的取反没进查询，「视频 且 都不是图片」被当成「视频 且 图片」
    expect(h.search.mock.calls[0][0]).toEqual({
      match: 'all',
      conditions: [
        { kind: 'media', types: ['video'], negate: false },
        { kind: 'group', match: 'any', negate: true, conditions: [{ kind: 'media', types: ['photo'], negate: false }] },
      ],
    });
  });

  it('点「且」连接词切换整组为「或」', async () => {
    h.search.mockResolvedValueOnce({ hits: [], cursor: null, scanned: 0 });
    const w = mountDialog();
    await w.find('[data-adv-media="video"]').trigger('click');
    await w.find('[data-adv-add]').trigger('click');
    await w.find('[data-adv-text]').setValue('周末');
    const joiner = w.find('[data-adv-joiner]');
    expect(joiner.text()).toBe('adv_search.join_and');
    await joiner.trigger('click');
    await w.find('form').trigger('submit');
    await flushPromises();
    // 不这样会怎样：连接词只变了文字，查询仍按「且」发，显示与实际不符
    expect(h.search.mock.calls[0][0].match).toBe('any');
  });
});
