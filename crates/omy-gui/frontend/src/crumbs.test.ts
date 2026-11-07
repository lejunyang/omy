/**
 * 面包屑：落在侧栏位置之下时，前缀合成一个带名字的段。
 *
 * 不这样会怎样：Android 上显示成「storage › emulated › 0 › Download › …」，
 * 手机一行放不下，当前目录被挤到屏幕外（真机上只露出一个字）。
 */
import { describe, it, expect, vi } from 'vitest';

vi.mock('./api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

import { state, crumbs } from './store';

describe('crumbs', () => {
  it('合并 Android 内部存储前缀，后续段路径正确', () => {
    state.places = [
      { name: 'internal_storage', path: '/storage/emulated/0' },
      { name: 'downloads', path: '/storage/emulated/0/Download' },
    ];
    state.container = null;
    state.cwd = '/storage/emulated/0/Download/相册';
    const cs = crumbs.value;
    // 取最长前缀：Download 自己就是侧栏位置
    expect(cs.map((c) => c.path)).toEqual(['/storage/emulated/0/Download', '/storage/emulated/0/Download/相册']);
    expect(cs[1].name).toBe('相册');
  });

  it('不在任何位置之下时按原样逐段列出', () => {
    state.places = [{ name: 'home', path: 'C:\\Users\\me' }];
    state.cwd = 'D:\\data\\x';
    expect(crumbs.value.map((c) => c.path)).toEqual(['D:\\', 'D:\\data', 'D:\\data\\x']);
  });

  it('前缀只在分隔符边界匹配', () => {
    // /home/me 不能吞掉 /home/meow
    state.places = [{ name: 'home', path: '/home/me' }];
    state.cwd = '/home/meow/a';
    expect(crumbs.value.map((c) => c.path)).toEqual(['/home', '/home/meow', '/home/meow/a']);
  });
});
