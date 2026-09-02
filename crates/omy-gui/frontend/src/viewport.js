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
 * 在 768 以上仍放得下侧栏。这个值同时用于 CSS 与 JS，改的时候
 * **两处都要改**（另一处在 styles/app.css 的 @media 块）。
 */

import { ref, readonly } from 'vue';

/** 移动端断点，单位 px。与 app.css 中的 @media 保持一致。 */
export const MOBILE_MAX = 768;

const mq = window.matchMedia(`(max-width: ${MOBILE_MAX}px)`);
const mobile = ref(mq.matches);

// 监听而不是只读一次：Android 上转屏会改变宽度，桌面窗口也能拖动。
// 只在启动时判断一次的话，转屏后交互方式就和界面对不上了
mq.addEventListener('change', (e) => {
  mobile.value = e.matches;
});

/** 当前是否为移动端视口（响应式，只读）。 */
export const isMobile = readonly(mobile);
