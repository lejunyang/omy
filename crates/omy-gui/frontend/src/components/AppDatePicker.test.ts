/** 自绘日期选择：选中、清除、min/max 约束、键盘移动。 */
import { describe, expect, it, vi } from 'vitest';
import { mount } from '@vue/test-utils';
import { defineComponent, h, ref } from 'vue';

vi.mock('../i18n', async () => {
  const { computed } = await import('vue');
  return { __v_isRef: false, lang: computed(() => 'zh-CN'), t: (k: string) => k };
});

import AppDatePicker from './AppDatePicker.vue';

function harness(initial = '', extra: Record<string, unknown> = {}) {
  const value = ref(initial);
  const Host = defineComponent(() => () =>
    h(AppDatePicker, {
      modelValue: value.value,
      'onUpdate:modelValue': (v: string) => (value.value = v),
      placeholder: '开始',
      ...extra,
    }),
  );
  const w = mount(Host, { global: { stubs: { teleport: true } }, attachTo: document.body });
  return { w, value };
}

describe('AppDatePicker', () => {
  it('打开时显示已选日期所在月份，点某天写回并收起', async () => {
    const { w, value } = harness('2026-03-14');
    await w.find('button.odate').trigger('click');
    await w.vm.$nextTick();
    // 不这样会怎样：总是打开到当月，改一个去年的日期要翻十几页
    expect(w.find('[data-day="2026-03-14"]').classes()).toContain('odate-sel');
    await w.find('[data-day="2026-03-20"]').trigger('click');
    expect(value.value).toBe('2026-03-20');
    expect(w.find('.odate-panel').exists()).toBe(false);
    w.unmount();
  });

  it('未选时显示占位，清除写回空串', async () => {
    const { w, value } = harness('2026-03-14');
    await w.find('button.odate').trigger('click');
    await w.find('[data-clear]').trigger('click');
    // 不这样会怎样：清不掉已选日期，日期条件只能整行删掉重建
    expect(value.value).toBe('');
    expect(w.find('button.odate').text()).toBe('开始');
    w.unmount();
  });

  it('超出 min / max 的日子不可选', async () => {
    const { w, value } = harness('', { min: '2026-03-10', max: '2026-03-20' });
    await w.find('button.odate').trigger('click');
    await w.vm.$nextTick();
    const early = w.find('[data-day="2026-03-09"]');
    expect(early.attributes('disabled')).toBeDefined();
    await early.trigger('click');
    // 不这样会怎样：结束日能选到开始日之前，拼出一个颠倒、永远零命中的区间
    expect(value.value).toBe('');
    w.unmount();
  });

  it('键盘方向键按天/周移动并跨月翻页，回车选中', async () => {
    const { w, value } = harness('2026-03-31');
    await w.find('button.odate').trigger('click');
    await w.vm.$nextTick();
    const grid = w.find('.odate-grid');
    await grid.trigger('keydown', { key: 'ArrowRight' });
    await grid.trigger('keydown', { key: 'ArrowDown' });
    await grid.trigger('keydown', { key: 'Enter' });
    // 3/31 → 4/1 → 4/8：跨月后面板必须跟着翻到四月，否则焦点落在看不见的格子上
    expect(value.value).toBe('2026-04-08');
    w.unmount();
  });

  it('翻月按钮切换月份', async () => {
    const { w } = harness('2026-01-31');
    await w.find('button.odate').trigger('click');
    await w.find('[data-nav="1"]').trigger('click');
    // 1/31 下个月夹到 2/28，网格里应出现二月的格子
    expect(w.find('[data-day="2026-02-28"]').classes()).not.toContain('odate-out');
    w.unmount();
  });
});
