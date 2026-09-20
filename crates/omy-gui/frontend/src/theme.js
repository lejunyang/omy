/** 明暗主题。
 *
 * # 三态而不是两态
 *
 * `auto` / `dark` / `light`。原先只有明暗两态，`localStorage` 里没有值
 * 就跟随系统——那等于把「跟随系统」做成了一个无法主动选回的隐藏状态：
 * 用户点过一次切换按钮之后，再也回不到跟随系统。
 *
 * 现在 `auto` 是一个显式选项，与配置文件里的 `ui.theme` 三态一一对应。
 *
 * # 仍然写 localStorage
 *
 * 配置文件是权威来源，但主题要在**界面出现之前**就定下来，否则会闪一下
 * 白底。读配置要走 IPC，那时页面已经渲染了。所以这里保留一份本地副本
 * 用于启动时的即时应用，配置文件里的值在加载完成后覆盖它。
 *
 * 两处都要写——只写一处的表现是「设置里选了浅色，重启却闪一下深色」。
 */

import { ref } from 'vue';

/** 当前生效的外观：`dark` | `light`。这是**解析后**的结果，不含 auto。 */
export const theme = ref('dark');

/** 用户的选择：`auto` | `dark` | `light`。设置页显示的是它。 */
export const themePref = ref('auto');

/** 系统当前是否偏好浅色。 */
function systemPrefersLight() {
  return window.matchMedia('(prefers-color-scheme: light)').matches;
}

/** 把偏好解析成实际外观。 */
function resolve(pref) {
  if (pref === 'light' || pref === 'dark') return pref;
  return systemPrefersLight() ? 'light' : 'dark';
}

/** 启动时确定初始主题。 */
export function initTheme() {
  const saved = localStorage.getItem('omy.theme');
  themePref.value = saved === 'light' || saved === 'dark' || saved === 'auto' ? saved : 'auto';
  theme.value = resolve(themePref.value);
  apply();

  // 跟随系统时要响应系统切换。不监听的话，用户在系统里切成夜间模式，
  // 应用要等下次重启才跟上
  window.matchMedia('(prefers-color-scheme: light)').addEventListener('change', () => {
    if (themePref.value === 'auto') {
      theme.value = resolve('auto');
      apply();
    }
  });
}

/** 用配置文件里的值校正主题。
 *
 * # 为什么需要单独一步
 *
 * `initTheme` 必须在首帧之前跑完，否则会闪一下白底；而配置要走 IPC 读，
 * 那时页面已经渲染了。所以启动分两步：先用 `localStorage` 里的副本即时
 * 应用，拿到配置后再用这里校正。
 *
 * # 不这样会怎样
 *
 * 两者平时总是一起写所以看不出问题，但配置从别处来时就会分叉——同步过来、
 * 外部编辑、或者换台机器带着配置。实测过的分叉表现是：配置里写着 `auto`、
 * `localStorage` 里是 `dark`，于是**设置页下拉框显示「跟随系统」，界面却
 * 是深色且系统切换时不跟随**。用户看到的是「选了跟随系统，没用」，而下拉
 * 框本身显示得没错，从界面上根本看不出是两个来源在打架。
 */
export function syncThemeFromConfig(pref) {
  if (pref !== 'auto' && pref !== 'dark' && pref !== 'light') return;
  if (pref === themePref.value) return;
  setTheme(pref);
}

/** 设置主题偏好。 */
export function setTheme(pref) {
  themePref.value = pref === 'light' || pref === 'dark' ? pref : 'auto';
  theme.value = resolve(themePref.value);
  localStorage.setItem('omy.theme', themePref.value);
  apply();
}

/** 在明暗之间切换。
 *
 * 从 `auto` 切换时以**当前实际外观**为基准取反，而不是固定跳到某一个：
 * 否则在浅色系统上点「切换」可能毫无变化，用户会以为按钮坏了。
 */
export function toggleTheme() {
  setTheme(theme.value === 'light' ? 'dark' : 'light');
}

function apply() {
  document.documentElement.dataset.theme = theme.value;
}
