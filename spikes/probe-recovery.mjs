/** 恢复码 GUI 的端到端验证与截图。
 *
 * 用法：node --experimental-websocket probe-recovery.mjs <port> <目录> <输出目录>
 *
 * 每一项都走真实界面：真的右键、真的点菜单、真的填词、真的提交。
 * 不用 invoke 直接调后端——那样验证的是后端，而后端上一轮已经验过了。
 * 这一轮要回答的是「界面接对了没有」，只有走界面才算数。
 *
 * 断言优先查真实 DOM 文本，而不是「命令返回成功」：用户看到的是屏幕。
 */

const port = process.argv[2] || '9370';
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
    // 两个空格分隔是固定格式：外层 ps1 用 /^\s+FAIL\s\s(.+)/ 锚定它。
    // 上一轮用宽松的 FAIL\s+(.+) 会把 cargo 的 "test result: FAILED" 也
    // 匹配进来，把失败归因到完全无关的地方
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

/** 选中某个文件（按名字找行，单击）。 */
async function selectRow(c, name) {
  return c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(name)}));
    if (!row) return 'no-row';
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
}

/** 右键打开菜单。 */
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

/** 点菜单项。按 key 定位而不是按文字：文字会互相包含
 *  （「生成恢复码」与「用恢复码打开」都含「恢复码」），
 *  按文字匹配会点中错的那个，把探针 bug 显示成产品缺陷。 */
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

/** 读对话框里的可见文本。 */
async function dlgText(c) {
  return c.eval(`(() => {
    const d = document.querySelector('.dlg.recovery');
    return d ? d.textContent.replace(/\\s+/g, ' ').trim() : '';
  })()`);
}

/** 填一个输入框。 */
async function fill(c, id, value) {
  return c.eval(`(() => {
    const el = document.getElementById(${JSON.stringify(id)});
    if (!el) return 'no-input';
    el.value = ${JSON.stringify(value)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return 'ok';
  })()`);
}

/** 点对话框的主按钮。 */
async function submitDlg(c) {
  return c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    if (!b) return 'no-btn';
    if (b.disabled) return 'disabled';
    b.click();
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

  console.log('\n--- 一、锁定态下菜单项的可用性 ---');

  // 先解锁，让文件显形，才能按真名找到它
  await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(800);
  await fill(c, 'u-pass', 'pw-main');
  await sleep(200);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(4000);

  const visible = await c.eval(`${ROWS}.map(r => r.textContent.trim().replace(/\\s+/g, ' ')).join(' | ')`);
  check('解锁后能看到测试文件', String(visible).includes('机密资料'), String(visible).slice(0, 120));

  await selectRow(c, '机密资料');
  await sleep(400);
  await openMenu(c, '机密资料');
  await sleep(600);

  const menuKeys = await c.eval(`
    [...document.querySelectorAll('.ctxmenu .mi')]
      .map(i => (i.dataset.mi || '?') + (i.disabled ? ':off' : ':on'))
      .join(' ')
  `);
  check('菜单里有「生成恢复码」且可用',
    String(menuKeys).includes('recovery-generate:on'), String(menuKeys));
  check('菜单里有「用恢复码打开」且可用',
    String(menuKeys).includes('recovery-restore:on'), String(menuKeys));
  await c.shot('01-menu');

  console.log('\n--- 二、生成恢复码 ---');

  const picked = await pickMenu(c, 'recovery-generate');
  check('能点开「生成恢复码」', picked === 'ok', String(picked));
  await sleep(900);

  const genText = await dlgText(c);
  check('生成对话框解释了恢复码是什么',
    genText.includes('26') && genText.includes('忘记密码'), genText.slice(0, 100));
  check('生成前要求输入当前密码',
    await c.eval(`!!document.getElementById('r-cur')`), '');
  await c.shot('02-generate-ask-password');

  // 密码没填时按钮应该是禁用的
  const beforeFill = await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    return b ? String(b.disabled) : 'no-btn';
  })()`);
  check('没填密码时不能提交', beforeFill === 'true', String(beforeFill));

  await fill(c, 'r-cur', 'pw-main');
  await sleep(300);
  const sub = await submitDlg(c);
  check('填完密码后可以提交', sub === 'ok', String(sub));
  await sleep(5000);

  const words = await c.eval(`
    [...document.querySelectorAll('.dlg.recovery .word')].map(w => w.textContent.trim())
  `);
  check('界面上显示了 26 个词',
    Array.isArray(words) && words.length === 26,
    Array.isArray(words) ? `实际 ${words.length} 个` : String(words));
  check('每个词都是小写字母',
    Array.isArray(words) && words.every(w => /^[a-z]{4,8}$/.test(w)),
    Array.isArray(words) ? words.slice(0, 3).join(',') : '');

  const genDone = await dlgText(c);
  check('警告了「只显示这一次」', genDone.includes('只显示这一次'), '');
  check('警告了它是安全性下限', genDone.includes('下限'), '');
  check('说明了不参与目录扫描', genDone.includes('目录扫描'), '');
  await c.shot('03-words-shown');

  // 未勾选时不能关闭——这是「这串词就此消失」之前唯一的闸门
  const beforeAck = await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    return b ? String(b.disabled) : 'no-btn';
  })()`);
  check('未勾选「已抄下」时完成按钮禁用', beforeAck === 'true', String(beforeAck));

  await c.eval(`(() => {
    const cb = document.querySelector('.dlg.recovery .ack input');
    if (!cb) return 'no-cb';
    cb.click();
    return 'ok';
  })()`);
  await sleep(300);
  const afterAck = await c.eval(`(() => {
    const b = document.querySelector('.dlg.recovery .acts .btn.primary');
    return b ? String(b.disabled) : 'no-btn';
  })()`);
  check('勾选后可以关闭', afterAck === 'false', String(afterAck));
  await c.shot('04-acknowledged');

  // 把词记下来，后面要用
  const saved = Array.isArray(words) ? words.join(' ') : '';
  await submitDlg(c);
  await sleep(1200);

  const closed = await c.eval(`!document.querySelector('.dlg.recovery')`);
  check('点完成后对话框关闭', closed === true, String(closed));

  // 生成恢复码只是加一条退路，绝不能把日常密码弄丢。
  //
  // 验证方式是锁定会话后用原密码重新解锁：如果 generate 把当前密码从
  // 槽位里抹掉了，这里会解不开、文件名不再显形。
  //
  // 这一条不能省：后面的流程全程用恢复码，最后的 CLI 复核又只看"新密码
  // 能开、旧密码已失效"——旧密码本来就该失效，所以整条链里没有任何一处
  // 会碰到"原密码在生成之后是否还活着"
  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.titlebar .iconbtn')]
      .find(x => x.textContent.trim() === '🔒');
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(1500);
  const lockedNames = await c.eval(`${ROWS}.map(r => r.textContent.trim()).join(' | ')`);
  check('锁定后文件名确实藏起来了',
    !String(lockedNames).includes('机密资料'), String(lockedNames).slice(0, 80));

  await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(800);
  await fill(c, 'u-pass', 'pw-main');
  await sleep(200);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (b && !b.disabled) b.click();
    return 'ok';
  })()`);
  await sleep(4000);
  const afterReunlock = await c.eval(`${ROWS}.map(r => r.textContent.trim()).join(' | ')`);
  check('生成恢复码后，原密码依然能打开这个文件',
    String(afterReunlock).includes('机密资料'), String(afterReunlock).slice(0, 80));

  console.log('\n--- 三、抄错时的提示 ---');

  await selectRow(c, '机密资料');
  await sleep(400);
  await openMenu(c, '机密资料');
  await sleep(600);
  await pickMenu(c, 'recovery-restore');
  await sleep(900);

  const restoreText = await dlgText(c);
  check('使用对话框提示输入 26 个词', restoreText.includes('26'), restoreText.slice(0, 80));
  await c.shot('05-restore-empty');

  // 词数不对
  await fill(c, 'r-code', 'academic acid acne');
  await fill(c, 'r-new', 'pw-new');
  await fill(c, 'r-new2', 'pw-new');
  await sleep(300);
  await submitDlg(c);
  await sleep(2500);
  const errCount = await c.eval(`(() => {
    const e = document.querySelector('.dlg.recovery .errbox');
    return e ? e.textContent.replace(/\\s+/g, ' ').trim() : '';
  })()`);
  check('词数不对时说清了差多少',
    errCount.includes('26') && (errCount.includes('3') || errCount.includes('个')),
    errCount.slice(0, 120));
  await c.shot('06-wrong-count');

  // 某个词不在词表里——把第 7 个词改成一个拼错的
  const typo = saved.split(' ');
  typo[6] = 'acadmic';
  await fill(c, 'r-code', typo.join(' '));
  await sleep(300);
  await submitDlg(c);
  await sleep(2500);
  const errWord = await c.eval(`(() => {
    const e = document.querySelector('.dlg.recovery .errbox');
    return e ? e.textContent.replace(/\\s+/g, ' ').trim() : '';
  })()`);
  check('能指出是第几个词错了', errWord.includes('7'), errWord.slice(0, 140));
  check('能给出拼写建议', errWord.includes('academic'), errWord.slice(0, 140));
  // 这条最要紧：core 的定位信息必须真的传到界面上，
  // 中间任何一层压成通用错误码，这里就会红
  check('错误里没有原样的占位符', !errWord.includes('{{detail}}'), errWord.slice(0, 80));
  await c.shot('07-typo-located');

  // 词序颠倒
  const swapped = saved.split(' ');
  [swapped[3], swapped[4]] = [swapped[4], swapped[3]];
  await fill(c, 'r-code', swapped.join(' '));
  await sleep(300);
  await submitDlg(c);
  await sleep(2500);
  const errOrder = await c.eval(`(() => {
    const e = document.querySelector('.dlg.recovery .errbox');
    return e ? e.textContent.replace(/\\s+/g, ' ').trim() : '';
  })()`);
  check('词序颠倒会被校验和抓到', errOrder.length > 0, errOrder.slice(0, 120));
  await c.shot('08-wrong-order');

  console.log('\n--- 四、正确的恢复码 ---');

  await fill(c, 'r-code', saved);
  await sleep(300);
  const okSub = await submitDlg(c);
  check('正确的码可以提交', okSub === 'ok', String(okSub));

  // 提示条只活 4 秒（setNotice 的默认值），所以要轮询着抢在它消失之前读。
  // 先 sleep 6 秒再读的话永远是空的，会把一次成功的操作报成缺陷
  let toast = '';
  let dlgGone = false;
  for (let i = 0; i < 40; i++) {
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
  check('成功后对话框关闭', dlgGone === true, String(dlgGone));
  check('提示说明恢复码仍然有效', toast.includes('仍然有效'), toast.slice(0, 80) || '(提示条没抓到)');
  await c.shot('09-restored');

  console.log('\n--- 五、大小写与空格应被归一化 ---');

  await selectRow(c, '机密资料');
  await sleep(400);
  await openMenu(c, '机密资料');
  await sleep(600);
  await pickMenu(c, 'recovery-restore');
  await sleep(900);

  // 同一份码，全大写 + 多余空格和换行
  const messy = saved.toUpperCase().split(' ').join('  \n ');
  await fill(c, 'r-code', messy);
  await fill(c, 'r-new', 'pw-third');
  await fill(c, 'r-new2', 'pw-third');
  await sleep(300);
  await submitDlg(c);
  await sleep(6000);
  const messyOk = await c.eval(`!document.querySelector('.dlg.recovery')`);
  check('大写与多余空白不影响识别', messyOk === true, String(messyOk));

  console.log('');
  console.log(`通过 ${pass} 项，失败 ${fail} 项`);
  if (failures.length) console.log('失败清单：' + failures.join('；'));

  ws.close();
  process.exit(fail === 0 ? 0 : 1);
})().catch((e) => {
  console.error('探针异常：', e.message);
  process.exit(2);
});
