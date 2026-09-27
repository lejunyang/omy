import { Plugin } from 'xgplayer';
import type { IPlayerOptions } from 'xgplayer';

/** 播放器可选倍速。把常用的 1.25x 留在一级列表里，避免每次都多点一次。 */
export const PLAYBACK_RATES = [0.5, 0.75, 1, 1.25, 1.5, 2, 3] as const;

export type OmyVideoPlayerOptionArgs = {
  el: HTMLElement;
  url: string;
  mobile: boolean;
  lang: string;
  rewindLabel: string;
  forwardLabel: string;
};

/**
 * 前后跳转按钮共用的行为。
 *
 * 不直接写两份点击处理：以后若调整步长、无障碍键盘操作或边界规则，
 * 两个方向必须一起改变，否则会出现「前进能按空格、后退不能」之类的分叉。
 */
abstract class SkipButton extends Plugin {
  abstract seconds: number;

  override afterCreate() {
    const root = this.root;
    if (!root) return;
    const label = String(this.config.label || '');
    root.setAttribute('role', 'button');
    root.setAttribute('tabindex', '0');
    root.setAttribute('aria-label', label);
    root.setAttribute('title', label);
    this.bind(['click', 'keydown'], (event: MouseEvent | KeyboardEvent) => {
      if (event.type === 'keydown') {
        const key = (event as KeyboardEvent).key;
        if (key !== 'Enter' && key !== ' ') return;
      }
      event.preventDefault();
      event.stopPropagation();
      const duration = Number(this.player.duration) || 0;
      const target = Math.max(0, Number(this.player.currentTime) + this.seconds);
      this.player.currentTime = duration > 0 ? Math.min(duration, target) : target;
    });
  }
}

export class OmyRewindButton extends SkipButton {
  override seconds = -10;

  static override get pluginName() {
    return 'omyrewind';
  }

  static override get defaultConfig() {
    return {
      position: Plugin.POSITIONS.CONTROLS_LEFT,
      index: 1,
      label: '',
    };
  }

  override render() {
    return '<xg-icon class="omy-skip omy-skip-rewind"><span aria-hidden="true">−10</span></xg-icon>';
  }
}

export class OmyForwardButton extends SkipButton {
  override seconds = 10;

  static override get pluginName() {
    return 'omyforward';
  }

  static override get defaultConfig() {
    return {
      position: Plugin.POSITIONS.CONTROLS_LEFT,
      index: 2,
      label: '',
    };
  }

  override render() {
    return '<xg-icon class="omy-skip omy-skip-forward"><span aria-hidden="true">+10</span></xg-icon>';
  }
}

/**
 * xgplayer 的单一配置入口。
 *
 * `isMobileSimulateMode` 与 `domEventType` 必须由项目自己的 `isMobile` 决定，
 * 不能退回播放器的 UA 判断：omy 的布局和交互都以 768px 响应式断点为准，
 * 两套判断分叉时会出现移动布局配桌面手势。
 */
export function createOmyVideoPlayerOptions(args: OmyVideoPlayerOptionArgs): IPlayerOptions {
  return {
    el: args.el,
    url: args.url,
    width: '100%',
    height: '100%',
    autoplay: true,
    playsinline: true,
    fluid: false,
    videoFillMode: 'contain',
    lang: args.lang.toLowerCase().startsWith('zh') ? 'zh-cn' : 'en',
    isMobileSimulateMode: args.mobile ? 'mobile' : 'pc',
    domEventType: args.mobile ? 'touch' : 'mouse',
    // 默认预设同时包含 PC 与 Mobile 插件；明确移除另一端，避免同一次点击
    // 被两套状态机处理。转屏时组件会销毁并按新模式重建。
    ignores: [args.mobile ? 'pc' : 'mobile'],
    playbackRate: [...PLAYBACK_RATES],
    // 播放=0、后退=1、前进=2、时间=3。默认时间也是 2，会与 +10
    // 同索引导致排序不稳定，出现截图中 −10 与 +10 被时间码隔开的错位。
    time: { index: 3 },
    keyShortcut: true,
    pip: true,
    cssFullscreen: true,
    fullscreen: true,
    videoAttributes: {
      // 保留已有端到端探针使用的结构标识；探针定位媒体元素不依赖文案。
      id: 'pv',
      // omy 页面与 omystream 媒体协议不同源。没有这个属性，视频一旦画到
      // canvas 上就会污染画布，截图和封面取帧都会抛 SecurityError。
      crossorigin: 'anonymous',
      preload: 'metadata',
    },
    mobile: {
      disableGesture: !args.mobile,
      gestureX: args.mobile,
      gestureY: args.mobile,
      disablePress: !args.mobile,
      pressRate: 2,
      hideControlsActive: true,
      hideControlsEnd: false,
      // 移动端横滑只在松手后 seek，避免每一帧手势都发 Range 请求。
      isTouchingSeek: false,
    },
    plugins: [OmyRewindButton, OmyForwardButton],
    omyrewind: { label: args.rewindLabel },
    omyforward: { label: args.forwardLabel },
  };
}
