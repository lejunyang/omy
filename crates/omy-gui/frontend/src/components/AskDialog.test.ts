/**
 * 应用内确认 / 输入 / 密码框。替代 window.prompt 的直接理由是密码要遮蔽，
 * 所以重点钉住：密码模式的输入框类型、取消的返回值、新问题顶掉旧问题。
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { mount } from '@vue/test-utils';
import { nextTick } from 'vue';
import AskDialog from './AskDialog.vue';
import { askConfirm, askPassword, askText, askState } from '../ask';

const stubs = { AppIcon: true };

describe('AskDialog', () => {
  beforeEach(() => {
    askState.current = null;
  });

  it('密码模式的输入框必须是 password 类型', async () => {
    // 不这样会怎样：密码明文显示在屏幕上，这正是换掉 window.prompt 的原因
    const w = mount(AskDialog, { global: { stubs } });
    const p = askPassword({ title: '输入密码' });
    await nextTick();
    const input = w.find('input');
    expect(input.attributes('type')).toBe('password');
    await input.setValue(' 带空格的密码 ');
    await w.find('form').trigger('submit');
    // 密码原样返回，不 trim：密码可以以空格开头
    await expect(p).resolves.toBe(' 带空格的密码 ');
  });

  it('取消确认框返回 false，取消输入框返回 null', async () => {
    const w = mount(AskDialog, { global: { stubs } });
    const c = askConfirm({ title: '删除？', danger: true });
    await nextTick();
    await w.findAll('button')[0].trigger('click');
    await expect(c).resolves.toBe(false);

    const t = askText({ title: '名字' });
    await nextTick();
    await w.find('.ask-overlay').trigger('click');
    await expect(t).resolves.toBeNull();
  });

  it('新的问题顶掉旧的问题，旧的按取消处理', async () => {
    // 两个问题叠着弹时用户分不清在回答哪一个；旧的 Promise 也不能永远挂着
    const first = askConfirm({ title: '一' });
    const second = askText({ title: '二', initial: '  名字  ' });
    await expect(first).resolves.toBe(false);
    const w = mount(AskDialog, { global: { stubs } });
    await nextTick();
    await w.find('form').trigger('submit');
    await expect(second).resolves.toBe('名字');
  });
});
