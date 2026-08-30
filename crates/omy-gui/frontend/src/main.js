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
import { initTheme } from './theme.js';
import { state } from './store.js';
import './styles/app.css';

async function boot() {
  initTheme();

  state.streamBase = await api.streamBase().catch(() => 'omystream://localhost');

  // 语言优先级：用户手动设置 > 系统语言（文档 §7.1）
  const saved = localStorage.getItem('omy.lang');
  const lang = saved || (await api.getLanguage().catch(() => 'en'));
  await i18n.load(lang);

  // 会话里可能已经有凭据（比如上一次没锁就关了窗口）
  state.credentials = await api.credentialCount().catch(() => 0);

  createApp(App).mount('#app');
}

boot();
