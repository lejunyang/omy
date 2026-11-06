/** 移动端断点：CSS 与 JS 共用的唯一来源。
 *
 * 原来 768 同时硬写在 viewport.ts 和十几处 `@media (max-width: 768px)` 里，
 * 规则只能靠「改一处必须改两处」的约定维持。漏改任何一处都会落到最难查
 * 的状态：界面已经是移动版，交互却还是桌面版（双击才能打开，触屏上点
 * 什么都没反应）。
 *
 * 现在样式里统一写 `@media (--mobile)`，构建时由 vite.config.ts 挂的
 * `mobileMediaPlugin` 展开成这里的媒体查询；viewport.ts 的 matchMedia
 * 也用同一个字符串。改断点只改 `MOBILE_MAX` 一处。
 *
 * 本文件不能引用 DOM 或 Vue：vite.config.ts 在 Node 里加载它。
 */

/** 移动端断点，单位 px（含）。 */
export const MOBILE_MAX = 768;

/** 移动端媒体查询，JS 的 matchMedia 与 CSS 展开结果都用它。 */
export const MOBILE_MEDIA = `(max-width: ${MOBILE_MAX}px)`;

/** 样式里使用的别名，写法沿用 CSS Custom Media 草案的 `(--name)`。 */
export const MOBILE_ALIAS = '(--mobile)';

/** 把媒体查询参数里的别名展开成真实条件；不含别名时原样返回。 */
export function expandMobileMedia(params: string): string {
  return params.split(MOBILE_ALIAS).join(MOBILE_MEDIA);
}

/** 给 Vite 的 PostCSS 管线用的插件：只改 `@media` 的参数。
 *
 * 不引入 postcss-custom-media：它要求先写 `@custom-media` 声明，
 * 那样断点值就又回到了 CSS 里，和这里成了两份。 */
export function mobileMediaPlugin() {
  return {
    postcssPlugin: 'omy-mobile-media',
    AtRule: {
      media(at: { params: string }) {
        at.params = expandMobileMedia(at.params);
      },
    },
  };
}
