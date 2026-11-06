import { beforeEach, describe, expect, it, vi } from 'vitest';

describe('Android 返回处理栈', () => {
  beforeEach(() => {
    vi.resetModules();
    delete window.omyAndroid;
    delete window.__omyHandleAndroidBack;
  });

  it('后注册的弹层先处理返回', async () => {
    const { registerMobileBack } = await import('./mobile-platform');
    const calls: string[] = [];
    registerMobileBack(() => { calls.push('root'); return true; });
    registerMobileBack(() => { calls.push('dialog'); return true; });

    expect(window.__omyHandleAndroidBack?.()).toBe(true);
    // 不这样会先关底层页面，再关弹窗，一次返回造成两层跳转。
    expect(calls).toEqual(['dialog']);
  });

  it('Esc 与返回键走同一个栈，并把来源告诉处理器', async () => {
    const { registerMobileBack, dispatchBack } = await import('./mobile-platform');
    const seen: string[] = [];
    // 底层页面：只处理返回键（后退目录），不处理 Esc
    registerMobileBack((source) => { seen.push('page:' + source); return source === 'back'; });
    registerMobileBack((source) => { seen.push('dialog:' + source); return true; });
    expect(dispatchBack('escape')).toBe(true);
    // 不这样会怎样：Esc 不分发时，大多数对话框按 Esc 毫无反应
    expect(seen).toEqual(['dialog:escape']);
  });

  it('未处理时返回 false 交给 Activity 退出', async () => {
    const { registerMobileBack } = await import('./mobile-platform');
    registerMobileBack(() => false);
    expect(window.__omyHandleAndroidBack?.()).toBe(false);
  });

  it('注销后不再收到返回事件', async () => {
    const { registerMobileBack } = await import('./mobile-platform');
    const handler = vi.fn(() => true);
    const unregister = registerMobileBack(handler);
    unregister();
    expect(window.__omyHandleAndroidBack?.()).toBe(false);
    expect(handler).not.toHaveBeenCalled();
  });

  it('视频横屏同时切换根样式和原生方向', async () => {
    const setVideoLandscape = vi.fn();
    window.omyAndroid = { setVideoLandscape };
    const { requestVideoLandscape } = await import('./mobile-platform');

    requestVideoLandscape(true);
    expect(document.documentElement.classList.contains('omy-video-landscape')).toBe(true);
    expect(setVideoLandscape).toHaveBeenLastCalledWith(true);

    requestVideoLandscape(false);
    expect(document.documentElement.classList.contains('omy-video-landscape')).toBe(false);
    expect(setVideoLandscape).toHaveBeenLastCalledWith(false);
  });
});
