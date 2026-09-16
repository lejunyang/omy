/** 树形加密文件夹在 GUI 里的多密码与恢复码。
 *
 * 用法：node --experimental-websocket probe-tree-gui.mjs <port> <目录> <输出目录>
 *
 * 与 verify-tree-multikey.ps1（CLI 层）的分工：那个验证格式与语义正确，
 * 这个验证**界面接对了没有**——菜单项亮不亮、对话框给不给这些操作、
 * 边车告知有没有真的显示出来。
 *
 * 后端全对而前端把选项灰着，用户照样用不了。
 */

const port = process.argv[2] || '9372';
const workDir = process.argv[3];
const outDir = process.argv[4];
const base = `http://127.0.0.1:${port}`;
import { writeFileSync } from 'node:fs';

let pass = 0;
let fail = 0;
const failures = [];

function check(name, ok, detail = '') {
  if (ok) {
    pass++;
    console.log(`  OK  ${name}`);
  } else {
    fail++;
    failures.push(name);
    // 两个空格分隔是固定格式，外层 ps1 用 /^\s+FAIL\s\s(.+)/ 锚定它。
    // 宽松匹配会把 cargo 的 "test result: FAILED" 也吃进来，把失败归因
    // 到完全无关的地方
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

/** 按 data-mi 定位菜单项——不能按文字：「生成恢复码」与「用恢复码打开」
 *  互相包含，文字匹配会点中错的那个，把探针 bug 显示成产品缺陷。 */
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

  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(1300);
  // workDir 是 .../omy-tree-gui-test/vault，要从文档根逐层双击进去。
  // 只进一层会停在文档根目录，那里没有我们的密文树——密码自然解不开，
  // 而报错「此文件夹中没有用这个密码加密的文件」会被误读成产品缺陷
  const parts = workDir.split('\\');
  const hops = [parts[parts.length - 2], parts[parts.length - 1]];
  for (const hop of hops) {
    const r = await c.eval(`(() => {
      const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(hop)}));
      if (!row) return 'no-row';
      row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
      return 'ok';
    })()`);
    if (r !== 'ok') {
      console.log(`  FAIL  进不去目录 ${hop} — ${r}`);
      process.exit(2);
    }
    await sleep(1800);
  }

  console.log('\n--- 一、解锁后能看到加密文件夹 ---');
  await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(800);
  await fill(c, 'u-pass', 'pw-tree');
  await sleep(200);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(4500);

  const names = await c.eval(`${ROWS}.map(r => r.textContent.trim().replace(/\\s+/g, ' ')).join(' | ')`);
  check('解锁后能看到加密文件夹的真名', String(names).includes('机密项目'), String(names).slice(0, 120));
  await c.shot('01-tree-unlocked');

  console.log('\n--- 二、密码管理对四个操作都放行 ---');
  await selectRow(c, '机密项目');
  await sleep(400);
  await openMenu(c, '机密项目');
  await sleep(600);
  const pickedKey = await pickMenu(c, 'manage-key');
  check('能对文件夹打开密码管理', pickedKey === 'ok', String(pickedKey));
  await sleep(900);

  const actions = await c.eval(`
    [...document.querySelectorAll('.keymgmt input[type=radio]')].map(r => r.value).join(',')
  `);
  // 这是解禁的核心断言：改造前树只给 change / reencrypt
  check('树形也能 add', String(actions).includes('add'), String(actions));
  check('树形也能 remove', String(actions).includes('remove'), String(actions));

  const dlgText = await c.eval(`(() => {
    const d = document.querySelector('.dlg.keymgmt');
    return d ? d.textContent.replace(/\\s+/g, ' ').trim() : '';
  })()`);
  check('提示说明文件夹可以挂多个密码', dlgText.includes('多个密码'), dlgText.slice(0, 100));
  check('告知了 .omy-keys 的存在与删除后果',
    dlgText.includes('omy-keys') && dlgText.includes('解不开'), dlgText.slice(0, 160));
  check('说明换密码不会改文件夹名',
    dlgText.includes('名字不会变') || dlgText.includes('不会变'), dlgText.slice(0, 160));
  await c.shot('02-tree-keymgmt');

  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.dlg.keymgmt .acts .btn')]
      .find(x => !x.classList.contains('primary'));
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(600);

  console.log('\n--- 三、树形恢复码 ---');
  await selectRow(c, '机密项目');
  await sleep(400);
  await openMenu(c, '机密项目');
  await sleep(600);
  const menuKeys = await c.eval(`
    [...document.querySelectorAll('.ctxmenu .mi')]
      .map(i => (i.dataset.mi || '?') + (i.disabled ? ':off' : ':on')).join(' ')
  `);
  check('「生成恢复码」对文件夹可用',
    String(menuKeys).includes('recovery-generate:on'), String(menuKeys));
  await c.shot('03-tree-menu');

  const pickedReco = await pickMenu(c, 'recovery-generate');
  check('能点开「生成恢复码」', pickedReco === 'ok', String(pickedReco));
  await sleep(900);

  await fill(c, 'r-cur', 'pw-tree');
  await sleep(300);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(8000);

  const words = await c.eval(`
    [...document.querySelectorAll('.dlg.recovery .word')].map(w => w.textContent.trim())
  `);
  check('整棵树生成了一份 26 词恢复码',
    Array.isArray(words) && words.length === 26,
    Array.isArray(words) ? `实际 ${words.length} 个` : String(words));
  await c.shot('04-tree-recovery-words');

  const saved = Array.isArray(words) ? words.join(' ') : '';

  await c.eval(`(() => {
    const cb = document.querySelector('.dlg.recovery .ack input');
    if (cb) cb.click();
    return 'ok';
  })()`);
  await sleep(300);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(1500);

  console.log('\n--- 四、用恢复码重设整棵树的密码 ---');
  await selectRow(c, '机密项目');
  await sleep(400);
  await openMenu(c, '机密项目');
  await sleep(600);
  await pickMenu(c, 'recovery-restore');
  await sleep(900);

  await fill(c, 'r-code', saved);
  await fill(c, 'r-new', 'pw-tree-new');
  await fill(c, 'r-new2', 'pw-tree-new');
  await sleep(300);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);

  // 提示条只活 4 秒，轮询着抢在它消失前读
  let toast = '';
  let dlgGone = false;
  for (let i = 0; i < 60; i++) {
    await sleep(250);
    if (!dlgGone) {
      dlgGone = (await c.eval(`!document.querySelector('.dlg.recovery')`)) === true;
    }
    if (dlgGone && !toast) {
      toast = (await c.eval(`(() => {
        const t = document.querySelector('.toast');
        return t ? t.textContent.replace(/\\s+/g, ' ').trim() : '';
      })()`)) || '';
      if (toast) break;
    }
  }
  check('恢复码重设整棵树的密码成功', dlgGone === true, String(dlgGone));
  check('提示说明恢复码仍然有效', toast.includes('仍然有效'), toast.slice(0, 80) || '(没抓到提示条)');
  await c.shot('05-tree-restored');

  console.log('\n--- 五、文件夹名没有因为换密码而改变 ---');
  const after = await c.eval(`${ROWS}.map(r => r.textContent.trim().replace(/\\s+/g, ' ')).join(' | ')`);
  check('换完密码文件夹仍在原处、名字未变',
    String(after).includes('机密项目'), String(after).slice(0, 120));

  console.log('');
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  if (failures.length) console.log('失败清单：' + failures.join('；'));

  ws.close();
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('探针异常：', e.message);
  process.exit(2);
});
