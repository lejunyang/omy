/** 应用入口。
 *
 * 启动顺序有依赖关系，不能调换：
 * 1. 主题——先定下来，避免白底闪一下再变暗
 * 2. 协议前缀——必须在任何内容加载前拿到，各平台形式不同，
 *    用错的话所有内容都是静默的 ERR_UNKNOWN_URL_SCHEME
 * 3. 语言——文案加载完才挂载，否则会闪一下错误语言的文字
 *
 * 注意这里**不检查解锁状态**：应用启动就能用，密码是后面按需要才问的。
 */

import { createApp } from 'vue';
import App from './App.vue';
import * as api from './api.js';
import * as i18n from './i18n.js';
import { initTheme, syncThemeFromConfig } from './theme.js';
import * as store from './store.js';
import { state } from './store.js';
import './styles/app.css';

async function boot() {
  initTheme();

  state.streamBase = await api.streamBase().catch(() => 'omystream://localhost');

  // 语言优先级：用户手动设置 > 系统语言（文档 §7.1）
  const saved = localStorage.getItem('omy.lang');
  const lang = saved || (await api.getLanguage().catch(() => 'en'));
  await i18n.load(lang);

  // 配置文件是权威来源，用它校正 initTheme 那一步用的本地副本。
  // 两者平时一致，但配置从别处来时会分叉——那时界面会出现「设置里写着
  // 跟随系统、实际却固定深色」这种自相矛盾的状态
  try {
    const cfg = await api.configGet();
    syncThemeFromConfig(cfg?.ui?.theme);
  } catch {
    // 读不到配置就沿用本地副本，不影响启动
  }

  // 会话里可能已经有凭据（比如上一次没锁就关了窗口）
  state.credentials = await api.credentialCount().catch(() => 0);

  // 分页大小是编译期常量，启动时问一次即可
  void store.loadPageSize?.();

  createApp(App).mount('#app');
}

boot();
