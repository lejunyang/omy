/**
 * 网格填充条数计算（与 store.viewportFillCount 同源公式的等价校验）。
 *
 * store.viewportFillCount 依赖 DOM 几何（happy-dom 不做布局），无法直接单测，
 * 这里用它内部完全相同的公式：clamp(alignToGrid(cols * visibleRows * 2, cols),
 * 15, 100)，覆盖真实桌面/移动几何，验证「铺满可见 + 对齐整行 + 不超后端上限」。
 */
import { describe, it, expect } from 'vitest';
import { alignToGrid } from './store';

const MIN = 15;
const MAX = 100; // 后端单页上限
const calc = (cols: number, visibleRows: number): number =>
  Math.min(MAX, Math.max(MIN, alignToGrid(cols * visibleRows * 2, cols)));

describe('文件网格首屏填充条数', () => {
  it('桌面 7 列约 5 可见行：铺满且是 7 的整数倍', () => {
    const n = calc(7, 5); // 70
    expect(n % 7).toBe(0);
    expect(n).toBeGreaterThanOrEqual(7 * 5);
  });

  it('宽屏 8 列 7 行：超过 100 时截到后端上限', () => {
    expect(calc(8, 7)).toBe(100); // 112 -> 100
  });

  it('窄窗口 3 列 3 行：取 18（3*3*2），铺满 6 行', () => {
    expect(calc(3, 3)).toBe(18);
    expect(calc(3, 3) % 3).toBe(0);
  });

  it('移动端 2 列：结果恒为偶数且不小于下限', () => {
    const n = calc(2, 6);
    expect(n % 2).toBe(0);
    expect(n).toBeGreaterThanOrEqual(MIN);
  });

  it('任何列数/行数组合结果都在 [15,100] 内', () => {
    for (let cols = 1; cols <= 10; cols++) {
      for (let rows = 1; rows <= 12; rows++) {
        const n = calc(cols, rows);
        expect(n).toBeGreaterThanOrEqual(MIN);
        expect(n).toBeLessThanOrEqual(MAX);
      }
    }
  });
});
