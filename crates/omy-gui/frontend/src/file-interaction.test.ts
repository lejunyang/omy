import { describe, expect, it } from 'vitest';
import { fileClickAction } from './file-interaction';

describe('文件条目点击语义', () => {
  it('移动端单击始终打开，不受已有选择态影响', () => {
    // 选择态不再作为输入：不这样会导致打开文件返回后，下一次单击变成误选。
    expect(fileClickAction(true, false)).toBe('open');
  });

  it('长按后浏览器补发的 click 必须吞掉', () => {
    // 不吞掉会在长按选中并弹出菜单后，又立即打开同一个文件。
    expect(fileClickAction(true, true)).toBe('ignore');
  });

  it('桌面端单击仍然选择', () => {
    // 不保留桌面语义会破坏 Ctrl/Shift 多选与双击打开的常见文件管理器交互。
    expect(fileClickAction(false, false)).toBe('select');
  });
});
