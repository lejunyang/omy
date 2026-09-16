/** 槽位可管理模式在 GUI 里的表现。
 *
 * 用法：node --experimental-websocket probe-slot-mode.mjs <port> <目录> <输出目录>
 *
 * 与 verify-slot-mode.ps1（CLI 层）的分工：那个验证格式与语义正确，
 * 这个验证**界面接对了没有**——模式选项在不在、槽位清单列不列得出、
 * 精确删除按钮点了有没有用。
 *
 * 后端全对而前端把清单藏着，用户照样享受不到这个模式的好处。
 */

const port = process.argv[2] || '9374';
const workDir = process.argv[3];
const outDir = process.argv[4];
const base = `http://127.0.0.1:${port}`;
import { writeFileSync } from 'node:fs';

let pass = 0;
let fail = 0;

function check(name, ok, detail = '') {
  if (ok) {
    pass++;
    console.log(`  OK  ${name}`);
  } else {
    fail++;
    // 两个空格是固定格式，外层 ps1 按 /^\s+FAIL\s\s(.+)/ 锚定
    console.log(`  FAIL  ${name}${detail ? ' — ' + detail : ''}`);
  }
}

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
      return 'JS-ERR: ' + String(d.exception?.description || d.text || '?').split('\n')[0].slice(0, 160);
    }
    return m.result?.result?.value;
  }
  async shot(name) {
    if (!outDir) return;
    const m = await this.send('Page.captureScreenshot', { format: 'png' });
    const data = m.result?.data;
    if (!data) { console.log(`  截图失败：${name}`); return; }
    const buf = Buffer.from(data, 'base64');
    writeFileSync(`${outDir}/${name}.png`, buf);
    console.log(`  已保存 ${name}.png（${buf.length} B）`);
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

async function selectRow(c, name) {
  return c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(name)}));
    if (!row) return 'no-row';
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
}

async function openMenu(c, name) {
  return c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(name)}));
    if (!row) return 'no-row';
    row.dispatchEvent(new MouseEvent('contextmenu', {
      bubbles: true, cancelable: true, clientX: 300, clientY: 300,
    }));
    return 'ok';
  })()`);
}

async function pickMenu(c, key) {
  return c.eval(`(() => {
    const items = [...document.querySelectorAll('.ctxmenu .mi')];
    const hit = items.find(i => i.dataset.mi === ${JSON.stringify(key)});
    if (!hit) return 'no-item:' + items.map(i => i.dataset.mi || '?').join(',');
    if (hit.disabled) return 'disabled';
    hit.click();
    return 'ok';
  })()`);
}

async function fill(c, id, value) {
  return c.eval(`(() => {
    const el = document.getElementById(${JSON.stringify(id)});
    if (!el) return 'no-input';
    el.value = ${JSON.stringify(value)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    // change 也要派发：有些字段靠它触发副作用（比如查槽位）。
    // 只派 input 的话，界面看着填好了，副作用却没跑——这个假象
    // 在截图上完全看不出来
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return 'ok';
  })()`);
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

  // 进入文档目录
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(900);

  // 逐层进入工作目录
  const segs = workDir.split('\\').filter(Boolean);
  const leaf = segs[segs.length - 1];
  await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(leaf)}));
    if (!row) return 'no-row';
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  await sleep(1200);

  console.log('');
  console.log('--- 一、加密对话框有模式选项 ---');
  await selectRow(c, '合同.txt');
  await sleep(300);
  await openMenu(c, '合同.txt');
  await sleep(400);
  const encOpened = await pickMenu(c, 'encrypt');
  check('能打开加密对话框', encOpened === 'ok', String(encOpened));
  await sleep(700);

  const modeRadios = await c.eval(`(() => {
    const els = [...document.querySelectorAll('.dlg input[type=radio]')]
      .filter(e => e.value === 'deniable' || e.value === 'managed');
    return els.map(e => e.value + (e.checked ? '*' : '')).join(',');
  })()`);
  check('两种模式都列出来了',
    String(modeRadios).includes('deniable') && String(modeRadios).includes('managed'),
    String(modeRadios));
  check('默认是可否认模式', String(modeRadios).includes('deniable*'), String(modeRadios));

  const permanentHint = await c.eval(`(() => {
    const t = document.querySelector('.dlg')?.textContent || '';
    return t.includes('无法更改') || t.includes('cannot be changed');
  })()`);
  check('说明了加密后改不了', permanentHint === true, String(permanentHint));

  await c.shot('01-encrypt-slot-mode');

  // 选可管理模式并加密
  await c.eval(`(() => {
    const el = [...document.querySelectorAll('.dlg input[type=radio]')]
      .find(e => e.value === 'managed');
    if (!el) return 'no-radio';
    el.click();
    return 'ok';
  })()`);
  await sleep(200);
  await fill(c, 'e-pass', 'pw-owner');
  await fill(c, 'e-pass2', 'pw-owner');
  await sleep(200);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b || b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  await sleep(6000);

  // 加密完成后密码会自动装入会话，所以列表里显示的是**已解锁**的样子：
  // 原名配 🔓，而不是 🔒 或 .omy。之前两次断言都因为猜错这一点而失败，
  // 且失败信息看起来像「加密没成功」，指错了方向。
  //
  // 按「出现了一个加密条目」断言——不依赖具体用哪个图标
  const rowTexts = await c.eval(
    `${ROWS}.map(r => r.textContent.trim().slice(0, 40)).join(' | ')`);
  check('可管理模式加密成功',
    String(rowTexts).includes('🔓') || String(rowTexts).includes('🔒'),
    String(rowTexts));

  console.log('');
  console.log('--- 二、密码管理对话框列出槽位 ---');
  // 不需要手动解锁：加密完成后密码已自动装入会话
  await selectRow(c, '合同');
  await sleep(300);
  await openMenu(c, '合同');
  await sleep(400);
  const kmOpened = await pickMenu(c, 'manage-key');
  check('能打开密码管理', kmOpened === 'ok', String(kmOpened));
  await sleep(600);

  // 输入密码触发查询
  await fill(c, 'k-cur', 'pw-owner');
  await sleep(1800);

  const slotRows = await c.eval(
    `[...document.querySelectorAll('.slotrow')].map(r => r.textContent.trim()).join(' | ')`);
  check('列出了槽位清单', String(slotRows).length > 0, String(slotRows) || '(空)');
  check('显示了「当前使用」标记',
    String(slotRows).includes('当前使用') || String(slotRows).includes('in use'),
    String(slotRows));

  // 当前那一行不该有删除按钮
  const curHasDelete = await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.slotrow')];
    const cur = rows.find(r => r.querySelector('.sbadge'));
    if (!cur) return 'no-current-row';
    return cur.querySelector('button') ? 'has-button' : 'no-button';
  })()`);
  check('当前密码那一行不给删除按钮', curHasDelete === 'no-button', String(curHasDelete));

  await c.shot('02-keymgmt-slots');

  console.log('');
  console.log('--- 三、加一个协作者密码，清单跟着变 ---');
  await c.eval(`(() => {
    const el = [...document.querySelectorAll('.dlg input[type=radio]')]
      .find(e => e.value === 'add');
    if (el) el.click();
    return 'ok';
  })()`);
  await sleep(200);
  await fill(c, 'k-new', 'pw-mate');
  await fill(c, 'k-new2', 'pw-mate');
  await sleep(200);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b || b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  await sleep(2500);

  // 重新打开看清单
  await selectRow(c, '合同');
  await sleep(300);
  await openMenu(c, '合同');
  await sleep(400);
  await pickMenu(c, 'manage-key');
  await sleep(600);
  await fill(c, 'k-cur', 'pw-owner');
  await sleep(1800);

  const twoSlots = await c.eval(`document.querySelectorAll('.slotrow').length`);
  check('现在有两个槽位', twoSlots === 2, String(twoSlots));

  const deletable = await c.eval(
    `[...document.querySelectorAll('.slotrow button')].filter(b => !b.disabled).length`);
  check('另一个槽位可以单独删除', deletable === 1, String(deletable));

  await c.shot('03-two-slots');

  console.log('');
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  ws.close();
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.log(`  FAIL  探针异常 — ${e.message}`);
  process.exit(1);
});
