/**
 * 文件网格「请求条数按列数对齐」的单元测试。
 *
 * 用户报的现象：文件分栏单次加载数量固定、不是每行卡片数的整数倍，最后一行
 * 缺几块、右侧留空。修复是把首屏/续翻请求量向上取整到整行。这里直接测导出的
 * 纯函数 alignToGrid，并通过 setGridLayout 模拟不同列数，覆盖常见分辨率与
 * 移动端 2 列的情况。
 *
 * viewportFillCount 依赖 DOM 几何（happy-dom 不做布局），不在这断言具体数值，
 * 只断言它在有/无布局信息时返回的都是列数的整数倍（这是“不留空位”的核心保证）。
 */
import { describe, it, expect, afterEach } from 'vitest';
import { alignToGrid, setGridLayout } from './store';

describe('alignToGrid 请求条数对齐整行', () => {
  afterEach(() => setGridLayout({ cols: 4, rowH: 156 }));

  it('已是整数倍时不变', () => {
    expect(alignToGrid(24, 4)).toBe(24);
    expect(alignToGrid(20, 5)).toBe(20);
    expect(alignToGrid(2, 2)).toBe(2);
  });

  it('不是整数倍时向上取整到整行（典型空位场景）', () => {
    // 5 列：估出 23 条 → 应取 25（5 整行），而不是 23 留 2 个空位
    expect(alignToGrid(23, 5)).toBe(25);
    // 7 列：30 条 → 35
    expect(alignToGrid(30, 7)).toBe(35);
    // 3 列：16 条 → 18
    expect(alignToGrid(16, 3)).toBe(18);
  });

  it('移动端 2 列也对齐（永远是偶数）', () => {
    expect(alignToGrid(15, 2)).toBe(16);
    expect(alignToGrid(21, 2)).toBe(22);
  });

  it('cols 给 0/负数/NaN 时退化为 1 列，不返回 NaN/0', () => {
    expect(alignToGrid(10, 0)).toBe(10);
    expect(alignToGrid(10, -3)).toBe(10);
    expect(alignToGrid(10, Number.NaN)).toBe(10);
  });

  it('n 为负/0 时返回 0（不制造无意义请求）', () => {
    expect(alignToGrid(0, 4)).toBe(0);
    expect(alignToGrid(-5, 4)).toBe(0);
  });

  it('不传 cols 时用 setGridLayout 设置的当前列数', () => {
    setGridLayout({ cols: 7, rowH: 156 });
    expect(alignToGrid(30)).toBe(35);
    setGridLayout({ cols: 2, rowH: 156 });
    expect(alignToGrid(21)).toBe(22);
  });
});
