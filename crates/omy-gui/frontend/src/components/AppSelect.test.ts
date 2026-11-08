/** 自绘下拉的交互：点开选值、键盘选值、Esc 只关下拉不外泄。
 *  面板 Teleport 到 body，测试里把 teleport 打桩成原地渲染。 */
import { describe, expect, it } from 'vitest';
import { mount } from '@vue/test-utils';
import { defineComponent, h, ref } from 'vue';
import AppSelect from './AppSelect.vue';

function harness(initial: unknown = false) {
  const value = ref(initial);
  const Host = defineComponent(() => () =>
    h(AppSelect, {
      modelValue: value.value,
      'onUpdate:modelValue': (v: unknown) => (value.value = v),
      options: [
        { value: false, label: '是' },
        { value: true, label: '不是' },
        { value: 'x', label: '其它' },
      ],
      'data-probe': '1',
    }),
  );
  const w = mount(Host, { global: { stubs: { teleport: true } }, attachTo: document.body });
  return { w, value };
}

describe('AppSelect', () => {
  it('显示当前值，点开后点选项更新 v-model 并收起', async () => {
    const { w, value } = harness(false);
    const btn = w.find('button.osel');
    expect(btn.text()).toBe('是');
    // 不这样会怎样：外部属性（探针用的 data-*）落不到按钮上，按结构定位不到控件
    expect(btn.attributes('data-probe')).toBe('1');
    await btn.trigger('click');
    const opts = w.findAll('.osel-opt');
    expect(opts).toHaveLength(3);
    await opts[1].trigger('click');
    // 布尔值必须原样回传，不能变成字符串 'true'（原生 select 的老问题）
    expect(value.value).toBe(true);
    expect(w.find('.osel-panel').exists()).toBe(false);
    w.unmount();
  });

  it('键盘：↓ 打开并从当前值起步，再 ↓ 回车选下一项', async () => {
    const { w, value } = harness(true);
    const btn = w.find('button.osel');
    await btn.trigger('keydown', { key: 'ArrowDown' });
    expect(btn.attributes('aria-expanded')).toBe('true');
    await btn.trigger('keydown', { key: 'ArrowDown' });
    await btn.trigger('keydown', { key: 'Enter' });
    // 不这样会怎样：高亮从第一项起步，↓ 一次选到的是「不是」而非下一项「其它」
    expect(value.value).toBe('x');
    w.unmount();
  });

  it('Esc 关下拉时标记已处理，外层对话框不会跟着关', async () => {
    const { w } = harness();
    const btn = w.find('button.osel');
    await btn.trigger('click');
    let outerSaw = false;
    let prevented = false;
    const onWin = (e: KeyboardEvent) => {
      outerSaw = true;
      prevented = e.defaultPrevented;
    };
    window.addEventListener('keydown', onWin);
    btn.element.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
    window.removeEventListener('keydown', onWin);
    await w.vm.$nextTick();
    expect(w.find('.osel-panel').exists()).toBe(false);
    // App 的全局 Esc 看 defaultPrevented 决定要不要关最上层浮层
    expect(!outerSaw || prevented).toBe(true);
    w.unmount();
  });

  it('点面板外面收起', async () => {
    const { w } = harness();
    await w.find('button.osel').trigger('click');
    document.body.dispatchEvent(new Event('pointerdown', { bubbles: true }));
    await w.vm.$nextTick();
    expect(w.find('.osel-panel').exists()).toBe(false);
    w.unmount();
  });

  it('外层滚动时收起，面板内部滚动不收起', async () => {
    const { w } = harness();
    await w.find('button.osel').trigger('click');
    // 监听挂在 window 的 capture 阶段，面板内的 scroll 也会经过它
    w.find('.osel-panel').element.dispatchEvent(new Event('scroll'));
    await w.vm.$nextTick();
    // 不这样会怎样：选项多到要滚动时，一滚列表就关掉，后面的选项永远选不到
    expect(w.find('.osel-panel').exists()).toBe(true);
    document.body.dispatchEvent(new Event('scroll'));
    await w.vm.$nextTick();
    // 不这样会怎样：对话框滚动后触发按钮已滚出可视区，面板还悬在无关的内容上
    expect(w.find('.osel-panel').exists()).toBe(false);
    w.unmount();
  });
});
