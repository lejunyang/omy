/**
 * xgplayer 适配配置回归测试。
 *
 * 这些断言分别钉住真实风险：跨源属性丢失会污染 canvas；移动/桌面判断分叉会让
 * 布局和手势不一致；长按默认其实是禁用的；插件漏注册会让前后 10 秒按钮消失。
 */
import { describe, expect, it } from 'vitest';
import {
  createOmyVideoPlayerOptions,
  OmyForwardButton,
  OmyRewindButton,
  PLAYBACK_RATES,
} from './video-player-options';

function options(mobile: boolean) {
  return createOmyVideoPlayerOptions({
    el: document.createElement('div'),
    url: 'omystream://localhost/file/demo',
    mobile,
    lang: 'zh-CN',
    rewindLabel: '后退 10 秒',
    forwardLabel: '前进 10 秒',
  });
}

describe('xgplayer 配置', () => {
  it('把跨源和内联播放属性传给底层 video', () => {
    const actual = options(false);
    // 不这样会污染 canvas，封面取帧与截图都会抛 SecurityError。
    expect(actual.videoAttributes).toMatchObject({
      id: 'pv',
      crossorigin: 'anonymous',
      preload: 'metadata',
    });
    // 不这样移动 WebView 会强制切到系统全屏播放器。
    expect(actual.playsinline).toBe(true);
  });

  it('移动端明确启用触摸手势与长按 2 倍速', () => {
    const actual = options(true);
    // 不显式指定时 xgplayer 会退回 UA 判断，与项目 768px 断点发生分叉。
    expect(actual.isMobileSimulateMode).toBe('mobile');
    expect(actual.domEventType).toBe('touch');
    expect(actual.ignores).toEqual(['pc']);
    // xgplayer 的 disablePress 默认是 true，漏写这一项时长按没有任何效果。
    expect(actual.mobile).toMatchObject({
      disableGesture: false,
      gestureX: true,
      gestureY: true,
      disablePress: false,
      pressRate: 2,
      isTouchingSeek: false,
    });
  });

  it('桌面端禁用触摸手势但保留完整播放控件', () => {
    const actual = options(false);
    // 桌面窗口缩窄后若仍挂触摸事件，会与鼠标拖动和点击争抢同一手势。
    expect(actual.isMobileSimulateMode).toBe('pc');
    expect(actual.domEventType).toBe('mouse');
    expect(actual.ignores).toEqual(['mobile']);
    expect(actual.mobile).toMatchObject({
      disableGesture: true,
      disablePress: true,
    });
  });

  it('提供所需倍速并注册前后 10 秒插件', () => {
    const actual = options(false);
    // 少了 1.25x 会迫使最常用的轻度加速退回自定义代码或无法选择。
    expect(actual.playbackRate).toEqual([...PLAYBACK_RATES]);
    // 不注册其中任一插件，界面就只剩键盘 seek，触屏和鼠标用户没有明确入口。
    expect(actual.plugins).toEqual([OmyRewindButton, OmyForwardButton]);
    expect(actual.omyrewind).toEqual({ label: '后退 10 秒' });
    expect(actual.omyforward).toEqual({ label: '前进 10 秒' });
  });
});
