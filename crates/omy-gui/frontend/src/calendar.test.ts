/** 日期选择的日期运算。错一天的缺陷只在月末、年末、闰年这些边界才出现，
 *  这里每条断言都钉在一个边界上。 */
import { describe, expect, it } from 'vitest';
import { addDays, addMonths, localDayEnd, localDayStart, monthGrid, parseYmd, weekStartFor } from './calendar';

describe('calendar', () => {
  it('严格解析：不存在的日子不自动进位', () => {
    // 不这样会怎样：2 月 30 日被 Date 进位成 3 月 2 日，筛选区间悄悄多出两天
    expect(parseYmd('2026-02-30')).toBeNull();
    expect(parseYmd('2024-02-29')).toEqual({ y: 2024, m: 2, d: 29 });
    expect(parseYmd('2026-2-3')).toBeNull();
  });

  it('跨月、跨年加减天数', () => {
    expect(addDays('2026-12-31', 1)).toBe('2027-01-01');
    expect(addDays('2026-03-01', -1)).toBe('2026-02-28');
  });

  it('加减月份时日子夹到月底', () => {
    // 不这样会怎样：1 月 31 日点「下个月」跳到 3 月 3 日，二月整个被跳过
    expect(addMonths('2026-01-31', 1)).toBe('2026-02-28');
    expect(addMonths('2026-01-15', -1)).toBe('2025-12-15');
    expect(addMonths('2024-02-29', 12)).toBe('2025-02-28');
  });

  it('月历网格按周起始日对齐，固定 42 格', () => {
    // 2026-11-01 是周日
    const mon = monthGrid(2026, 11, 1);
    expect(mon).toHaveLength(42);
    // 周一起：首格是前一天周一 10-26，11-01 落在第 7 列
    expect(mon[0]).toBe('2026-10-26');
    expect(mon[6]).toBe('2026-11-01');
    // 周日起：11-01 就在首格
    expect(monthGrid(2026, 11, 0)[0]).toBe('2026-11-01');
    expect(weekStartFor('zh-CN')).toBe(1);
    expect(weekStartFor('en')).toBe(0);
  });

  it('当天首末秒覆盖整整一天', () => {
    const s = localDayStart('2026-06-15')!;
    const e = localDayEnd('2026-06-15')!;
    expect(new Date(s * 1000).getDate()).toBe(15);
    expect(new Date(e * 1000).getDate()).toBe(15);
    // 再多一秒就是次日
    expect(new Date((e + 1) * 1000).getDate()).toBe(16);
    expect(localDayStart('bad')).toBeNull();
  });
});
