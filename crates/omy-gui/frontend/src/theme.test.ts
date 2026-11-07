import { beforeEach, describe, expect, it, vi } from 'vitest';

// 每条用例重新导入，模块级的 ref 不在用例之间串状态
async function fresh() {
  vi.resetModules();
  return import('./theme');
}

describe('主题色', () => {
  beforeEach(() => {
    localStorage.clear();
    delete document.documentElement.dataset.accent;
  });

  it('首帧按本地副本应用，没有副本时是默认蓝', async () => {
    let t = await fresh();
    t.initAccent();
    expect(document.documentElement.dataset.accent).toBe('blue');

    localStorage.setItem('omy.accent', 'rose');
    t = await fresh();
    t.initAccent();
    // 不这样会怎样：每次启动先闪一下蓝再变成用户选的颜色
    expect(document.documentElement.dataset.accent).toBe('rose');
    expect(t.accent.value).toBe('rose');
  });

  it('不认识的值回落到蓝，不写进 data-accent', async () => {
    const t = await fresh();
    t.setAccent('neon');
    // 不这样会怎样：未知值写进去后设置页没有任何色块处于选中态，
    // 用户看不出当前用的是哪个颜色
    expect(t.accent.value).toBe('blue');
    expect(document.documentElement.dataset.accent).toBe('blue');
    expect(localStorage.getItem('omy.accent')).toBe('blue');
  });

  it('配置文件校正本地副本；旧配置缺这一项时不动', async () => {
    localStorage.setItem('omy.accent', 'teal');
    const t = await fresh();
    t.initAccent();
    t.syncAccentFromConfig(undefined);
    // 不这样会怎样：升级前的配置没有 accent，若当成「蓝」处理，
    // 用户本机选过的颜色每次启动都会被冲掉
    expect(t.accent.value).toBe('teal');

    t.syncAccentFromConfig('violet');
    // 不这样会怎样：配置从别处来（同步、手改）时界面与设置页各说各话
    expect(document.documentElement.dataset.accent).toBe('violet');
    expect(localStorage.getItem('omy.accent')).toBe('violet');
  });
});
