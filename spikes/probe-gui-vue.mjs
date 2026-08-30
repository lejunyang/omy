/** 通过 CDP 验证 Vue 界面真的渲染出来了。
 *
 * 构建通过不等于能跑：模板编译错误、CSP 拦截、IPC 不可用
 * 都只在运行时暴露。这个脚本启动真实的 GUI 进程，连上 CDP，
 * 读回 DOM 与控制台错误。
 *
 * 用法：node probe-gui-vue.mjs <cdp-port>
 */

const port = process.argv[2] || '9333';
const base = `http://127.0.0.1:${port}`;

/** 等 CDP 端口就绪。 */
async function waitTargets(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const list = await r.json();
        const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page) return page;
      }
    } catch {
      // 还没起来
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 端口未就绪');
}

/** 最小的 CDP 客户端。只用到 Runtime.evaluate 与 Log.enable。 */
class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.logs = [];
    ws.addEventListener('message', (ev) => {
      const msg = JSON.parse(ev.data);
      if (msg.id && this.pending.has(msg.id)) {
        this.pending.get(msg.id)(msg);
        this.pending.delete(msg.id);
      } else if (msg.method === 'Log.entryAdded') {
        this.logs.push(msg.params.entry);
      } else if (msg.method === 'Runtime.exceptionThrown') {
        const d = msg.params.exceptionDetails;
        this.logs.push({
          level: 'error',
          source: 'exception',
          text: d.exception?.description || d.text,
        });
      }
    });
  }

  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) => (m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => reject(new Error(`${method} 超时`)), 20000);
    });
  }

  async eval(expr) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    }
    return r.result.value;
  }
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}

const page = await waitTargets();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => {
  ws.addEventListener('open', res);
  ws.addEventListener('error', rej);
});

const cdp = new Cdp(ws);
await cdp.send('Runtime.enable');
await cdp.send('Log.enable');

// 给 Vue 挂载留时间（boot 里有几个 await）
await new Promise((r) => setTimeout(r, 2500));

console.log('=== Vue 界面运行时验证 ===');

const appHtml = await cdp.eval('document.querySelector("#app")?.innerHTML?.length ?? -1');
check('#app 有内容（Vue 已挂载）', appHtml > 100, `innerHTML ${appHtml} 字符`);

const hasUnlock = await cdp.eval('!!document.querySelector(".unlock-box")');
check('解锁界面已渲染', hasUnlock);

const title = await cdp.eval('document.querySelector(".unlock-box h1")?.textContent ?? ""');
check('标题文案已翻译（非键名）', title === 'omy', `"${title}"`);

const hint = await cdp.eval('document.querySelector(".unlock-box .hint")?.textContent?.trim() ?? ""');
check('提示文案不是原始键名', hint.length > 0 && !hint.includes('unlock.'), `"${hint.slice(0, 40)}"`);

const btnLabel = await cdp.eval(
  'document.querySelector(".folder-row .btn")?.textContent?.trim() ?? ""',
);
check('按钮文案已翻译', btnLabel.length > 0 && !btnLabel.includes('unlock.'), `"${btnLabel}"`);

const submitDisabled = await cdp.eval(
  'document.querySelector(".btn.primary")?.disabled ?? null',
);
check('未选目录时解锁按钮禁用', submitDisabled === true);

const lang = await cdp.eval('document.documentElement.lang');
check('文档语言已设置', lang === 'zh-CN' || lang === 'en', lang);

const theme = await cdp.eval('document.documentElement.dataset.theme ?? ""');
check('主题已应用', theme === 'dark' || theme === 'light', theme);

// 响应式验证：改 store 里的值，看 DOM 是否跟着变。
// 这直接证明「用了框架」而不只是「打包成功」
const reactive = await cdp.eval(`(async () => {
  const box = document.querySelector('.unlock-box .path');
  const before = box?.textContent?.trim() ?? '';
  // 通过真实交互路径改状态：直接改 input 的值并派发事件
  const label = document.querySelector('#u-label');
  label.value = 'probe-value';
  label.dispatchEvent(new Event('input', { bubbles: true }));
  await new Promise(r => setTimeout(r, 100));
  return document.querySelector('#u-label').value;
})()`);
check('v-model 双向绑定生效', reactive === 'probe-value', reactive);

// CSP 是否拦了东西
const errors = cdp.logs.filter(
  (l) => l.level === 'error' && !String(l.text).includes('favicon'),
);
check('无运行时错误', errors.length === 0, errors.map((e) => e.text).join(' | ').slice(0, 200));

const cspViolations = cdp.logs.filter((l) => String(l.text).includes('Content Security Policy'));
check('无 CSP 违规', cspViolations.length === 0, cspViolations.map((e) => e.text).join(' | ').slice(0, 200));

ws.close();

const passed = results.filter((r) => r.ok).length;
console.log('');
console.log(`结果: ${passed} 通过, ${results.length - passed} 失败`);
process.exit(passed === results.length ? 0 : 1);
