/** 视口断点。
 *
 * # 为什么不只用 CSS media query
 *
 * 布局差异 CSS 能解决，但**交互差异**不行：桌面靠双击打开条目，
 * 触屏上根本没有双击这个手势——`dblclick` 在移动 WebView 里要么
 * 不触发，要么被系统的双击缩放吃掉。所以「现在是不是移动端」必须
 * 是 JS 能读到的响应式状态，组件据此换绑事件。
 *
 * 只写 CSS 的话，结果是移动端界面看着对，但**点什么都打不开**。
 *
 * # 断点取 768px
 *
 * 与文档 08 的「移动端：单栏 + 底部导航」一致：横屏手机与竖屏平板
 * 在 768 以上仍放得下侧栏。数值只定义在 breakpoint.ts：样式里写
 * `@media (--mobile)`，构建时展开成同一个媒体查询，CSS 与 JS 不会分叉。
 */

import { ref, readonly } from 'vue';
import { MOBILE_MAX, MOBILE_MEDIA } from './breakpoint';

export { MOBILE_MAX };

const mq = window.matchMedia(MOBILE_MEDIA);
const mobile = ref(mq.matches);

// 监听而不是只读一次：Android 上转屏会改变宽度，桌面窗口也能拖动。
// 只在启动时判断一次的话，转屏后交互方式就和界面对不上了
mq.addEventListener('change', (e) => {
  mobile.value = e.matches;
});

/** 当前是否为移动端视口（响应式，只读）。 */
export const isMobile = readonly(mobile);
