/** 验证「加密后移到回收站」在真实界面里确实生效。
 *
 * 用法：node --experimental-websocket probe-trash.mjs <port> <目录>
 *
 * # 为什么单测不够
 *
 * 单测直接调 handle_original，绕过了「对话框选项 → 请求字段 → 后端分支」
 * 这条链。任何一环把 original 传丢（比如字段名写错），单测照样全绿，
 * 但用户点了「移到回收站」原件却还在。
 *
 * # 判据
 *
 * 原件消失 + 回收站里能找到同名条目。只判「原件消失」是不够的——
 * 永久删除也满足，而那是数据丢失。
 */

const port = process.argv[2] || '9356';
const workDir = process.argv[3];
const base = `http://127.0.0.1:${port}`;

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
      return { __err: m.result.exceptionDetails.exception?.description || m.result.exceptionDetails.text };
    }
    return m.result?.result?.value;
  }
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const inv = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

async function main() {
  const page = await waitTarget();
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  console.log('=== 1. 进入测试目录 ===');
  const side = await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (!t) return 'no-documents';
    t.click();
    return 'clicked';
  })()`);
  check('侧栏能点到「文档」', side === 'clicked', String(side));
  await sleep(1200);

  const into = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('omy-trash-test'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('进入测试目录', into === 'dispatched', String(into));
  await sleep(1400);

  console.log('\n=== 2. 选中文件，选「移到回收站」后加密 ===');
  const sel = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('to-trash'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'clicked';
  })()`);
  check('选中待加密文件', sel === 'clicked', String(sel));
  await sleep(400);

  await c.eval(`(() => {
    const b = [...document.querySelectorAll('button')]
      .find(x => x.textContent.includes('加密') || x.textContent.includes('Encrypt'));
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(800);

  await c.eval(`(() => {
    for (const i of document.querySelectorAll('input[type=password]')) {
      i.value = 'trash-pw';
      i.dispatchEvent(new Event('input', { bubbles: true }));
    }
    return 'ok';
  })()`);
  await sleep(300);

  // 按 value 选中回收站选项。按结构定位，不按文案——
  // 文案会随语言变，value 是稳定的
  const picked = await c.eval(`(() => {
    const r = document.querySelector('input[type=radio][value="trash"]');
    if (!r) return 'no-radio';
    r.click();
    return r.checked ? 'checked' : 'click-had-no-effect';
  })()`);
  check('「移到回收站」选项已选中', picked === 'checked', String(picked));
  await sleep(300);

  const submitted = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.dlg button, .dialog button, .mask button')]
      .find(x => (x.textContent.includes('加密') || x.textContent.includes('Encrypt')) && !x.disabled);
    if (!b) return 'no-submit';
    b.click();
    return 'submitted';
  })()`);
  check('已提交加密', submitted === 'submitted', String(submitted));
  await sleep(6000);

  console.log('\n=== 3. 结果检查 ===');
  const listing = await c.eval(inv('browse_directory', { dir: workDir }));
  const names = (listing || []).map((e) => e.name);
  check('产出了加密文件', names.some((n) => n.endsWith('.omy')), JSON.stringify(names));
  check('原件已不在原目录（回收站选项真的生效了）',
    !names.some((n) => n === 'to-trash.txt'), JSON.stringify(names));

  const bad = results.filter((r) => !r.ok);
  console.log(`\n=== 合计 ${results.length - bad.length}/${results.length} 通过 ===`);
  if (bad.length) {
    console.log('失败项：');
    for (const b of bad) console.log('  - ' + b.name);
  }
  ws.close();
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => { console.error('探针异常:', e.message); process.exit(2); });
