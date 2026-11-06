/** Android 原生桥与返回分发。
 *
 * Android 返回键必须先交给当前最上层 UI；只有所有处理器都拒绝时才退出 Activity。
 * 处理器倒序执行，后挂载的弹层天然优先于底层页面。
 *
 * 桌面的 Esc 走同一个栈（source = 'escape'）：「关掉最上层」只该有一份实现。
 * 以前只有三个组件自己监听 Esc，其余对话框按 Esc 毫无反应。两者唯一的区别是
 * Esc 不做页面级后退（返回上级目录、退出远程位置），处理器据 source 自行判断。
 */

export type BackSource = 'back' | 'escape';
type BackHandler = (source: BackSource) => boolean;

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

/** 把一次返回 / Esc 交给最上层处理器；有人处理返回 true。 */
export function dispatchBack(source: BackSource): boolean {
  for (let index = handlers.length - 1; index >= 0; index -= 1) {
    try {
      if (handlers[index](source)) return true;
    } catch {
      // 某一层处理失败不能吞掉系统返回；继续交给下一层。
    }
  }
  return false;
}

window.__omyHandleAndroidBack = () => dispatchBack('back');
