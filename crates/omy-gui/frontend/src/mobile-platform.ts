/** Android 原生桥与返回分发。
 *
 * Android 返回键必须先交给当前最上层 UI；只有所有处理器都拒绝时才退出 Activity。
 * 处理器倒序执行，后挂载的弹层天然优先于底层页面。
 */

type BackHandler = () => boolean;

type AndroidBridge = {
  setVideoLandscape(enabled: boolean): void;
};

declare global {
  interface Window {
    omyAndroid?: AndroidBridge;
    __omyHandleAndroidBack?: () => boolean;
  }
}

const handlers: BackHandler[] = [];

export const isAndroid = typeof window.omyAndroid !== 'undefined';

export function registerMobileBack(handler: BackHandler): () => void {
  handlers.push(handler);
  return () => {
    const index = handlers.lastIndexOf(handler);
    if (index >= 0) handlers.splice(index, 1);
  };
}

export function requestVideoLandscape(enabled: boolean) {
  document.documentElement.classList.toggle('omy-video-landscape', enabled);
  window.omyAndroid?.setVideoLandscape(enabled);
}

window.__omyHandleAndroidBack = () => {
  for (let index = handlers.length - 1; index >= 0; index -= 1) {
    try {
      if (handlers[index]()) return true;
    } catch {
      // 某一层处理失败不能吞掉系统返回；继续交给下一层。
    }
  }
  return false;
};
