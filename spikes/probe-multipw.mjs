/** 验证「一个会话同时装多个密码」在真实界面里生效。
 *
 * 用法：node --experimental-websocket probe-multipw.mjs <port> <目录>
 *
 * # 为什么 core 单测不够
 *
 * core 的 6 项集成测试直接调 `SessionKeys::add_password`，整条
 * 「对话框 → invoke unlock_directory → commands::unlock → add_password
 * → scan → 列表 unlocked 字段 → 界面渲染」都没走到。链上任何一环退回
 * 替换语义（比如有人把 unlock 里的 add_password 改回 unlock_password），
 * core 的测试照样全绿，而用户在界面上输第二个密码时第一批文件会当场
 * 锁上——正是这个功能要解决的问题本身。
 *
 * # 判据是界面上**同时**能看见两批文件
 *
 * 不看 toast，也不看 credential_count 的数字：后端可以报「2 个密码」
 * 却因为扫描没重跑而列表照旧。真正的判据是那两行的 unlocked 都为 true。
 *
 * # 元素定位按结构
 *
 * 加密文件解锁后显示的是**真实文件名**（real-a.txt / real-b.txt），
 * 锁着时是占位名。所以「解开了几个」要数 DOM 上的真名，不能按磁盘名找。
 */

const port = process.argv[2] || '9362';
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
      const d = m.result.exceptionDetails;
      return 'JS-ERR: ' + String(d.exception?.description || d.text || '?')
        .split('\n')[0].slice(0, 120);
    }
    return m.result?.result?.value;
  }
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok: !!ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const inv = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

/** 走真实界面解锁：点密码药丸 → 填框 → 提交。
 *
 * 不直接 invoke：那会跳过 store.tryUnlock 里的 reload()，而列表的
 * unlocked 字段正是靠那一步回填的。跳过后界面永远显示锁定，
 * 看起来像后端没生效。
 */
async function unlockViaUi(c, password) {
  const opened = await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (!b) return 'no-pill';
    b.click();
    return 'clicked';
  })()`);
  if (opened !== 'clicked') return `pill-failed:${opened}`;
  await sleep(700);
  const filled = await c.eval(`(() => {
    const el = document.getElementById('u-pass');
    if (!el) return 'no-input';
    el.value = ${JSON.stringify(password)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return 'ok';
  })()`);
  if (filled !== 'ok') return `fill-failed:${filled}`;
  await sleep(300);
  const sub = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b || b.disabled) return 'no-button';
    b.click();
    return 'clicked';
  })()`);
  if (sub !== 'clicked') return `submit-failed:${sub}`;
  // Argon2 + 扫描
  await sleep(4000);
  return 'ok';
}

/** 界面上当前能看见哪些真实文件名。
 *
 * 这是最终判据：不管后端报了什么，用户看到的就是这些。
 */
async function visibleNames(c) {
  const txt = await c.eval(`${ROWS}.map(r => r.textContent).join('|')`);
  if (typeof txt !== 'string') return [];
  return ['real-a.txt', 'real-b.txt'].filter((n) => txt.includes(n));
}

/** 读密码药丸上显示的凭据数。 */
async function credCount(c) {
  return await c.eval(inv('credential_count', {}));
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
  await sleep(500);

  console.log('=== 1. 进入测试目录 ===');
  // 导航方式与 probe-keymgmt 一致：点侧栏「文档」再双击目录行。
  // 不能只 invoke('list_dir')——那只是后端列目录，前端的 state.cwd
  // 不会变，而解锁作用于**当前目录**，站错地方会解不开任何东西
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(1300);
  const dirName = workDir.split('\\').pop();
  const into = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(dirName)}));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('进入测试目录', into === 'dispatched', String(into));
  await sleep(1600);
  const listed = await c.eval(`${ROWS}.length`);
  check('目录里能看到两个加密文件', listed === 2, `${listed} 项`);

  console.log('');
  console.log('=== 2. 起始状态：锁定，两批文件都看不见 ===');
  await c.eval(inv('lock', {}));
  await sleep(600);
  const c0 = await credCount(c);
  check('锁定后凭据数为 0', c0 === 0, `实得 ${c0}`);

  console.log('');
  console.log('=== 3. 装入第一个密码 pw-alpha ===');
  const u1 = await unlockViaUi(c, 'pw-alpha');
  check('界面解锁流程跑通（pw-alpha）', u1 === 'ok', String(u1));
  const c1 = await credCount(c);
  check('凭据数变为 1', c1 === 1, `实得 ${c1}`);
  const v1 = await visibleNames(c);
  check('只看得见 real-a.txt', v1.length === 1 && v1[0] === 'real-a.txt',
    `实得 [${v1.join(', ')}]`);

  console.log('');
  console.log('=== 4. 再装第二个密码 pw-beta（关键：第一个不能被顶掉）===');
  const u2 = await unlockViaUi(c, 'pw-beta');
  check('界面解锁流程跑通（pw-beta）', u2 === 'ok', String(u2));
  const c2 = await credCount(c);
  check('凭据数变为 2（两个密码并存）', c2 === 2, `实得 ${c2}`);
  const v2 = await visibleNames(c);
  // 这是整个功能的核心判据。退回替换语义时这里会是 ['real-b.txt']——
  // 数量仍是 1，所以必须断言**具体是哪两个**，只数个数会漏掉
  check('两批文件同时可见', v2.length === 2, `实得 [${v2.join(', ')}]`);
  check('real-a.txt 没有被第二个密码顶掉', v2.includes('real-a.txt'),
    `实得 [${v2.join(', ')}]`);
  check('real-b.txt 也解开了', v2.includes('real-b.txt'),
    `实得 [${v2.join(', ')}]`);

  console.log('');
  console.log('=== 5. 重复输入 pw-alpha：计数不该虚高 ===');
  const u3 = await unlockViaUi(c, 'pw-alpha');
  check('重复解锁不报错', u3 === 'ok', String(u3));
  const c3 = await credCount(c);
  check('凭据数仍是 2（同一密码只算一条）', c3 === 2, `实得 ${c3}`);
  const v3 = await visibleNames(c);
  check('重复输入后两批文件仍都可见', v3.length === 2, `实得 [${v3.join(', ')}]`);

  console.log('');
  console.log('=== 6. 后端直接确认 added 标志 ===');
  const vaults = await c.eval(inv('vault_params_of', { dir: workDir }));
  const okVaults = Array.isArray(vaults) && vaults.length >= 1;
  check('能探测到 vault 参数', okVaults, `实得 ${JSON.stringify(vaults).slice(0, 80)}`);
  if (okVaults) {
    // 已经装过的密码：added 必须是 false
    const dup = await c.eval(inv('unlock', { label: 'main', password: 'pw-alpha', vaults }));
    check('重复密码的 added 为 false',
      dup && dup.added === false,
      `实得 ${JSON.stringify(dup)}`);
    // 第三个全新密码：added 必须是 true，且计数涨到 3
    const fresh = await c.eval(inv('unlock', { label: 'main', password: 'pw-gamma', vaults }));
    check('新密码的 added 为 true', fresh && fresh.added === true,
      `实得 ${JSON.stringify(fresh)}`);
    const c4 = await credCount(c);
    check('装入第三个密码后计数为 3', c4 === 3, `实得 ${c4}`);
  }

  console.log('');
  console.log('=== 7. 锁定必须一次清空全部密码 ===');
  // 点界面上真正的锁定按钮，不要 invoke('lock')。
  //
  // 两者不等价：后端的 lock 只抹密钥，而列表是 store.lock() 里的
  // reload() 重新拉的。直接 invoke 会留着上一次的渲染结果，于是断言
  // 报「锁定后仍看得见文件名」——那是探针绕过了产品代码，不是产品
  // 泄露。第一版就这么写的，差点把它当成真缺陷。
  //
  // 按结构定位：状态栏里 class 为 iconbtn 且文本是 🔒 的那个。
  // 不按 title 文案找——文案随语言变
  const clicked = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.titlebar .iconbtn')]
      .find(x => x.textContent.trim() === '🔒');
    if (!b) return 'no-lock-button';
    b.click();
    return 'clicked';
  })()`);
  check('找得到并点了锁定按钮', clicked === 'clicked', String(clicked));
  await sleep(2000);
  const c5 = await credCount(c);
  check('锁定后凭据数归零', c5 === 0, `实得 ${c5}`);
  const v5 = await visibleNames(c);
  check('锁定后两批文件都看不见', v5.length === 0, `实得 [${v5.join(', ')}]`);

  ws.close();
  const failed = results.filter((r) => !r.ok);
  console.log('');
  console.log(`共 ${results.length} 项，失败 ${failed.length} 项`);
  process.exit(failed.length === 0 ? 0 : 1);
})().catch((e) => {
  console.error('探针异常：', e.message);
  process.exit(1);
});
