/**
 * Shift 区间选择与键盘移动。
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('./api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

import { state, toggleSelect, selectRange, moveSelection } from './store';

describe('选择', () => {
  beforeEach(() => {
    state.container = null;
    state.query = '';
    state.searchMode = 'local';
    state.sortKey = 'name';
    state.sortDir = 'asc';
    state.entries = ['a', 'b', 'c', 'd', 'e'].map((n) => ({ path: '/d/' + n, name: n }));
    state.selected = [];
  });

  it('Shift 点击选中起点到这一项之间的全部', () => {
    // 不这样会怎样：Shift 与 Ctrl 一样只加选一项，选一段连续文件只能逐个点
    toggleSelect('/d/b', false);
    selectRange('/d/d');
    expect(state.selected).toEqual(['/d/b', '/d/c', '/d/d']);
    selectRange('/d/a');
    expect(state.selected).toEqual(['/d/a', '/d/b']);
  });

  it('方向键移动当前项，Shift 时扩展', () => {
    moveSelection(1, false);
    expect(state.selected).toEqual(['/d/a']);
    moveSelection(2, false);
    expect(state.selected).toEqual(['/d/c']);
    moveSelection(1, true);
    expect(new Set(state.selected)).toEqual(new Set(['/d/c', '/d/d']));
    // 不越界
    moveSelection(99, false);
    expect(state.selected).toEqual(['/d/e']);
  });
});
