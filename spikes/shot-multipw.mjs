/** 截取多密码界面的真实状态。
 *
 * 用法：node --experimental-websocket shot-multipw.mjs <port> <目录> <输出目录>
 *
 * 不摆拍：每一张都是真的走完解锁流程之后截的，不是手工构造 DOM。
 * 摆拍出来的截图在「这个提示到底会不会出现」这件事上毫无证明力。
 */

const port = process.argv[2] || '9368';
const workDir = process.argv[3];
const outDir = process.argv[4];
const base = `http://127.0.0.1:${port}`;
import { writeFileSync } from 'node:fs';

async function waitTarget(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const page = (await r.json()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page) return page;
      }
    } catch { /* 等 */ }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 未就绪');
}

class Cdp {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map();
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) { this.pending.get(m.id)(m); this.pending.delete(m.id); }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 30000);
      this.pending.set(id, (m) => { clearTimeout(timer); res(m); });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expr) {
    const m = await this.send('Runtime.evaluate', {
      expression: `(async () => { return (${expr}); })()`,
      awaitPromise: true, returnByValue: true,
    });
    if (m.result?.exceptionDetails) {
      const d = m.result.exceptionDetails;
      return 'JS-ERR: ' + String(d.exception?.description || d.text || '?').split('\n')[0].slice(0, 120);
    }
    return m.result?.result?.value;
  }
  async shot(name) {
    const m = await this.send('Page.captureScreenshot', { format: 'png' });
    const data = m.result?.data;
    if (!data) { console.log(`  截图失败：${name}`); return; }
    const buf = Buffer.from(data, 'base64');
    writeFileSync(`${outDir}/${name}.png`, buf);
    console.log(`  已保存 ${name}.png（${buf.length} B）`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const inv = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

/** 打开密码对话框（不提交）。 */
async function openDialog(c) {
  await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(800);
}

/** 在已打开的对话框里填密码并提交。 */
async function submitPassword(c, password) {
  await c.eval(`(() => {
    const el = document.getElementById('u-pass');
    if (!el) return 'no-input';
    el.value = ${JSON.stringify(password)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return 'ok';
  })()`);
  await sleep(300);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(4000);
}

(async () => {
  const target = await waitTarget();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('WS 连接失败')));
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(1500);

  // 进入测试目录
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(1300);
  const dirName = workDir.split('\\').pop();
  await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(dirName)}));
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  await sleep(1800);

  console.log('1. 锁定态：三个文件都看不见真名');
  await c.eval(inv('lock', {}));
  await sleep(600);
  await c.eval(`(() => { const b = [...document.querySelectorAll('.titlebar .iconbtn')].find(x => x.textContent.trim() === '🔒'); if (b) b.click(); return 'ok'; })()`);
  await sleep(1500);
  await c.shot('01-locked');

  console.log('2. 首次解锁：对话框说「输入密码可解锁」');
  await openDialog(c);
  await c.shot('02-dialog-first');

  console.log('3. 装入密码 A 之后');
  await submitPassword(c, 'pw-alpha');
  await c.shot('03-after-alpha');

  console.log('4. 再次打开对话框：提示变成「已装入 1 个密码，再输一个不会顶掉」');
  await openDialog(c);
  await c.shot('04-dialog-add-more');

  console.log('5. 装入密码 B：两批文件同时可见，状态栏显示 2 个密码');
  await submitPassword(c, 'pw-beta');
  await c.shot('05-both-visible');

  console.log('6. 重复输入 A：提示「这个密码已经在用了」');
  await openDialog(c);
  await submitPassword(c, 'pw-alpha');
  await c.shot('06-duplicate-notice');

  const pill = await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    return b ? b.textContent.trim().replace(/\\s+/g, ' ') : 'no-pill';
  })()`);
  const names = await c.eval(`${ROWS}.map(r => r.textContent.trim().replace(/\\s+/g, ' ')).join(' | ')`);
  console.log('');
  console.log(`状态栏：${pill}`);
  console.log(`列表：${names}`);

  ws.close();
  process.exit(0);
})().catch((e) => {
  console.error('截图失败：', e.message);
  process.exit(1);
});
