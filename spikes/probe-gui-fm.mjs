/** 实测新的文件管理器交互。
 *
 * 重点回答一个问题：**没有密码的新用户能不能用这个应用**。
 * 这正是重构前的缺陷所在——启动就是密码框，新用户被挡在门外。
 *
 * 用法：node --experimental-websocket probe-gui-fm.mjs <port> <测试目录>
 */

const port = process.argv[2] || '9345';
const testDir = process.argv[3] || '';
const base = `http://127.0.0.1:${port}`;

async function waitTarget(timeoutMs = 30000) {
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
      /* 还没起来 */
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 端口未就绪');
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.logs = [];
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m);
        this.pending.delete(m.id);
      } else if (m.method === 'Log.entryAdded') {
        this.logs.push(m.params.entry);
      } else if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params.exceptionDetails;
        this.logs.push({ level: 'error', text: d.exception?.description || d.text });
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      this.pending.set(id, (m) => (m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => rej(new Error(`${method} 超时`)), 25000);
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
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}

const page = await waitTarget();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => {
  ws.addEventListener('open', res);
  ws.addEventListener('error', rej);
});
const cdp = new Cdp(ws);
await cdp.send('Runtime.enable');
await cdp.send('Log.enable');
await new Promise((r) => setTimeout(r, 2500));

console.log('=== 新交互：不用密码就能进门 ===');

// 这是本次重构要解决的核心缺陷
const hasPasswordGate = await cdp.eval(
  '!!document.querySelector(".unlock-screen") || !!document.querySelector(".unlock-box")',
);
check('启动时没有密码门槛（核心）', !hasPasswordGate);

const hasFileManager = await cdp.eval('!!document.querySelector(".titlebar") && !!document.querySelector(".side")');
check('直接进入文件管理器', hasFileManager);

const places = await cdp.eval('document.querySelectorAll(".side .sitem").length');
check('侧栏列出了可浏览的位置', places > 1, `${places} 项`);

const placeText = await cdp.eval(
  '[...document.querySelectorAll(".side .stext")].map(e=>e.textContent.trim()).slice(0,4).join(" / ")',
);
check('位置名已翻译（非原始键名）', !placeText.includes('places.'), placeText);

const deviceSection = await cdp.eval(
  '[...document.querySelectorAll(".sgrp")].some(e=>e.textContent.includes("设备")||e.textContent.includes("device")||e.textContent.includes("Nearby"))',
);
check('侧栏有「附近设备」分区', deviceSection);

console.log('');
console.log('=== 浏览真实目录 ===');

if (testDir) {
  const escaped = testDir.replace(/\\/g, '\\\\');
  const navOk = await cdp.eval(`(async () => {
    const m = await import('/app.js').catch(() => null);
    // app.js 是打包产物，没有导出。改走界面：点侧栏进不去指定目录，
    // 所以直接调 Tauri 命令验证后端，再看界面能否渲染
    const r = await window.__TAURI_INTERNALS__.invoke('browse_directory', { dir: '${escaped}' });
    return Array.isArray(r) ? r.length : -1;
  })()`);
  check('browse_directory 能列出目录', navOk >= 0, `${navOk} 项`);

  const encInfo = await cdp.eval(`(async () => {
    const r = await window.__TAURI_INTERNALS__.invoke('browse_directory', { dir: '${escaped}' });
    const enc = r.filter(e => e.is_encrypted);
    const plain = r.filter(e => !e.is_dir && !e.is_encrypted);
    return { total: r.length, enc: enc.length, plain: plain.length,
             encLocked: enc.filter(e => !e.unlocked).length };
  })()`);
  check('能识别出加密文件', encInfo.enc > 0, `加密 ${encInfo.enc} / 普通 ${encInfo.plain}`);
  check('加密文件默认是锁定态', encInfo.encLocked === encInfo.enc, `${encInfo.encLocked}/${encInfo.enc}`);

  // 关键反证：锁定的加密文件不能泄露明文信息
  const leak = await cdp.eval(`(async () => {
    const r = await window.__TAURI_INTERNALS__.invoke('browse_directory', { dir: '${escaped}' });
    const locked = r.filter(e => e.is_encrypted && !e.unlocked);
    return locked.some(e => e.real_name !== null || e.entry_id !== null);
  })()`);
  check('锁定文件不返回真实名（关键反证）', leak === false);

  const places2 = await cdp.eval(`(async () => {
    const p = await window.__TAURI_INTERNALS__.invoke('parent_of', { path: '${escaped}' });
    return typeof p === 'string' && p.length > 0;
  })()`);
  check('parent_of 能取到上级目录', places2);
} else {
  console.log('  (跳过：没有提供测试目录)');
}

console.log('');
console.log('=== 界面完整性 ===');

const langOk = await cdp.eval('document.documentElement.lang');
check('语言已设置', langOk === 'zh-CN' || langOk === 'en', langOk);

const noRawKeys = await cdp.eval(`(() => {
  const t = document.body.innerText;
  // 漏翻译会以 "view.xxx" / "status.xxx" 这种形式出现在界面上
  const m = t.match(/\\b(view|status|places|nav|file|encrypt|unlock|device)\\.[a-z_]+/g);
  return m ? m.slice(0, 3).join(', ') : '';
})()`);
check('界面上没有未翻译的键名', noRawKeys === '', noRawKeys);

const pill = await cdp.eval('document.querySelector(".pill")?.textContent?.trim() ?? ""');
check('顶栏有密码状态入口', pill.length > 0, pill);

const errors = cdp.logs.filter((l) => l.level === 'error' && !String(l.text).includes('favicon'));
check('无运行时错误', errors.length === 0, errors.map((e) => e.text).join(' | ').slice(0, 240));

const csp = cdp.logs.filter((l) => String(l.text).includes('Content Security Policy'));
check('无 CSP 违规', csp.length === 0, csp.map((e) => e.text).join(' | ').slice(0, 200));

ws.close();
const passed = results.filter((r) => r.ok).length;
console.log('');
console.log(`结果: ${passed} 通过, ${results.length - passed} 失败`);
process.exit(passed === results.length ? 0 : 1);
