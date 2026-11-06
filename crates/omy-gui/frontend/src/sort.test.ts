/**
 * 本地列表排序：目录永远在前，同类按所选字段排，相同时按名称兜底。
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('./api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

import { state, visibleEntries, setSort } from './store';

const e = (name: string, extra = {}) => ({ path: '/d/' + name, name, ...extra });

describe('排序', () => {
  beforeEach(() => {
    state.container = null;
    state.query = '';
    state.searchMode = 'local';
    state.sortKey = 'name';
    state.sortDir = 'asc';
    state.entries = [
      e('b.txt', { size: 30, modified: 3 }),
      e('文件夹', { is_dir: true }),
      e('a10.txt', { size: 10, modified: 1 }),
      e('a2.mp4', { size: 20, modified: 2 }),
    ];
  });

  it('按名称时目录在前、数字按数值', () => {
    // 不这样会怎样：a10 排在 a2 前面；目录夹在文件中间
    expect(visibleEntries.value.map((x) => x.name)).toEqual(['文件夹', 'a2.mp4', 'a10.txt', 'b.txt']);
  });

  it('切到大小默认从大到小，再点一次反过来，目录仍在前', () => {
    setSort('size');
    expect(visibleEntries.value.map((x) => x.name)).toEqual(['文件夹', 'b.txt', 'a2.mp4', 'a10.txt']);
    setSort('size');
    expect(visibleEntries.value.map((x) => x.name)).toEqual(['文件夹', 'a10.txt', 'a2.mp4', 'b.txt']);
  });

  it('按修改时间从新到旧', () => {
    setSort('modified');
    expect(visibleEntries.value.map((x) => x.name)).toEqual(['文件夹', 'b.txt', 'a2.mp4', 'a10.txt']);
  });
});
