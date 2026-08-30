/** 明暗主题。
 *
 * 存 localStorage，未设置时跟随系统（文档 §4.1）。
 */

import { ref } from 'vue';

export const theme = ref('dark');

/** 启动时确定初始主题。 */
export function initTheme() {
  const saved = localStorage.getItem('omy.theme');
  if (saved === 'light' || saved === 'dark') {
    theme.value = saved;
  } else if (window.matchMedia('(prefers-color-scheme: light)').matches) {
    theme.value = 'light';
  }
  apply();
}

/** 切换。 */
export function toggleTheme() {
  theme.value = theme.value === 'light' ? 'dark' : 'light';
  localStorage.setItem('omy.theme', theme.value);
  apply();
}

function apply() {
  document.documentElement.dataset.theme = theme.value;
}
