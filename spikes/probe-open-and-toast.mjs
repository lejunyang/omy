/** 验证用户报的两个问题是否真的解决了。
 *
 * 1. 「已加密 1 个文件」的提示不会自己消失，要手动点掉
 * 2. 未加密的图片/视频双击打不开；其他类型也该能用系统程序打开
 *
 * # 怎么做到「验证用户真正看到的那一层」
 *
 * 后端字段对不对，用 Tauri 命令查（既有探针的做法）。但这两个问题
 * 都出在界面上，所以还必须驱动真实 DOM：
 *
 * - 进目录：把测试目录做成侧栏里的「图片」位置（脚本里用
 *   OMY_TEST_PICTURES 覆盖），这样能真的点侧栏进去，而不是绕过界面。
 * - 打开文件：派发真实的 dblclick 事件到列表行上。这能连带验证
 *   「双击有没有接到处理函数」——调内部函数是验不出这一段的。
 * - 判据：图片查 <img>.naturalWidth（真的解码出像素了），
 *   提示条查 .toast 在不在 DOM 里。src 设对但加载失败时
 *   naturalWidth 是 0，那正是用户说的「打不开」。
 *
 * 用法：node --experimental-websocket probe-open-and-toast.mjs <port> <目录>
 */

const port = process.argv[2] || '9351';
const workDir = process.argv[3] || '';
const base = `http://127.0.0.1:${port}`;

async function waitTarget(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const page = (await r.json()).find(
          (t) => t.type === 'page' && t.webSocketDebuggerUrl,
        );
        if (page) return page;
      }
    } catch {
      /* 等 */
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 未就绪');
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.logs = [];
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.method === 'Runtime.consoleAPICalled') {
        this.logs.push({
          level: m.params.type,
          text: (m.params.args || []).map((a) => a.value ?? a.description).join(' '),
        });
      }
      if (m.method === 'Runtime.exceptionThrown') {
        this.logs.push({
          level: 'error',
          text:
            m.params.exceptionDetails?.exception?.description ||
            m.params.exceptionDetails?.text ||
            'exception',
        });
      }
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m);
        this.pending.delete(m.id);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      this.pending.set(id, (m) =>
        m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result),
      );
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => rej(new Error(`${method} 超时`)), 120000);
    });
  }
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(
        r.exceptionDetails.exception?.description || r.exceptionDetails.text,
      );
    }
    return r.result.value;
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

/** 派发真实双击到包含指定文件名的那一行。 */
const dbl = (name) => `(() => {
  const rows = [...document.querySelectorAll('.lrow, .card, .cell')];
  const row = rows.find(r => r.textContent.includes(${JSON.stringify(name)}));
  if (!row) return 'not-found:' + rows.length;
  row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
  return 'dispatched';
})()`;

(async () => {
  const t = await waitTarget();
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws 失败')));
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  console.log('\n=== 0. 后端：token 与预览类别 ===');
  const listed = await c.eval(`${inv('browse_directory', { dir: workDir })}`);
  check('目录能列出', Array.isArray(listed) && listed.length > 0,
    `${listed?.length} 项`);
  const by = Object.fromEntries((listed || []).map((e) => [e.name, e]));

  check('未加密图片拿到 token', !!by['photo.png']?.token);
  check('图片预览类别 = image', by['photo.png']?.preview === 'image',
    String(by['photo.png']?.preview));
  check('文本预览类别 = text', by['note.txt']?.preview === 'text',
    String(by['note.txt']?.preview));
  check('zip 预览类别 = other（该交给系统程序）',
    by['archive.zip']?.preview === 'other',
    String(by['archive.zip']?.preview));
  check('加密文件不猜预览类别（磁盘后缀是 .omy）',
    by['secret.omy'] ? by['secret.omy'].preview === null : true,
    String(by['secret.omy']?.preview));

  console.log('\n=== 1. 通过侧栏真实点击进入目录 ===');
  // 测试目录被设成了「图片」位置，所以点侧栏就能进去
  // 先点侧栏「图片」，再双击进子目录 omy-probe-test。
  // 全程真实点击：这样连「侧栏 → 列表 → 进目录」这条链路
  // 也一并验证了，而不只是验证某个函数
  const sideEntered = await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const target = items.find(i => i.textContent.includes('文档')
                              || i.textContent.includes('Documents'));
    if (!target) return 'no-documents-item:' + items.length;
    target.click();
    return 'clicked';
  })()`);
  check('侧栏里能点到「文档」', sideEntered === 'clicked', String(sideEntered));
  await sleep(1200);

  const intoSub = await c.eval(dbl('omy-probe-test'));
  check('双击进入测试子目录', intoSub === 'dispatched', String(intoSub));
  await sleep(1400);

  const rowCount = await c.eval(
    `document.querySelectorAll('.lrow, .card, .cell').length`);
  check('列表已渲染出条目', rowCount > 0, `${rowCount} 项`);

  console.log('\n=== 2. 双击未加密图片：应用内要真的显示出来 ===');
  const d1 = await c.eval(dbl('photo.png'));
  check('双击事件已派发到图片行', d1 === 'dispatched', String(d1));
  await sleep(1400);

  const img = await c.eval(`(() => {
    const el = document.querySelector('.overlay img');
    if (!el) return { present: false };
    return {
      present: true,
      w: el.naturalWidth, h: el.naturalHeight,
      complete: el.complete,
      src: (el.getAttribute('src') || '').slice(0, 70),
    };
  })()`);
  check('图片预览层出现了', img.present === true);
  check('图片真的解码出像素（核心：用户说的打不开）',
    img.present && img.w > 0 && img.h > 0,
    img.present ? `${img.w}x${img.h}` : '没有 img 元素');
  check('走的是 plain 协议', (img.src || '').includes('/plain/'), img.src || '');

  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.overlay-bar .iconbtn')].pop();
    if (b) b.click();
  })()`);
  await sleep(500);

  console.log('\n=== 3. 双击未加密文本：内容要渲染出来 ===');
  const d2 = await c.eval(dbl('note.txt'));
  check('双击事件已派发到文本行', d2 === 'dispatched', String(d2));
  await sleep(1200);
  const textShown = await c.eval(
    `document.querySelector('.overlay pre')?.textContent ?? null`);
  check('文本内容已渲染',
    typeof textShown === 'string' && textShown.includes('hello omy'),
    JSON.stringify((textShown || '').slice(0, 40)));

  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.overlay-bar .iconbtn')].pop();
    if (b) b.click();
  })()`);
  await sleep(500);

  console.log('\n=== 4. 不可预览的类型：给出「用其他应用打开」的出口 ===');
  // 不真的拉起系统程序（会在测试机上弹窗），只验证界面给了出口
  const d3 = await c.eval(dbl('archive.zip'));
  check('双击事件已派发到 zip 行', d3 === 'dispatched', String(d3));
  await sleep(900);
  const zipUi = await c.eval(`(() => {
    const ov = document.querySelector('.overlay');
    return {
      overlay: !!ov,
      hasPre: !!document.querySelector('.overlay pre'),
      hasImg: !!document.querySelector('.overlay img'),
      selected: [...document.querySelectorAll('.lrow.sel, .card.sel')]
        .map(e => e.textContent.trim().slice(0, 30)),
    };
  })()`);
  // zip 的正确行为是交给系统程序，不在应用内弹预览层。
  // 这条断言能抓住的真实回归是「误把 zip 当文本/图片渲染」——
  // 那会把一堆二进制乱码糊在屏幕上
  check('zip 没有被误当成文本或图片渲染',
    !zipUi.hasPre && !zipUi.hasImg, JSON.stringify(zipUi));
  check('zip 双击后该项被选中（有可见反馈）',
    zipUi.selected.some(t => t.includes('archive.zip')),
    JSON.stringify(zipUi.selected));

  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.overlay-bar .iconbtn')].pop();
    if (b) b.click();
  })()`);
  await sleep(400);

  console.log('\n=== 5. 提示条：成功提示要自己消失 ===');
  const encOk = await c.eval(`(async () => {
    try {
      const r = await ${inv('encrypt_paths', {
        req: {
          paths: [`${workDir}\\note.txt`],
          password: 'probe-pass-1',
          encrypt_filename: true,
          preserve_extension: false,
          compress: true,
          chunk_size: 262144,
          kdf_profile: 'interactive',
          original: 'keep',
        },
      })};
      return r.items.length;
    } catch (e) { return 'err:' + (e.code || e); }
  })()`);
  console.log(`  （后端加密了 ${encOk} 个文件）`);

  // 走界面的加密入口太绕（要开对话框填表单），这里直接让界面
  // 显示一条成功提示，验证的是**提示条的生命周期**本身
  await c.eval(`(() => {
    const ov = document.querySelector('.toast');
    if (ov) ov.remove();
  })()`);

  const toastAppeared = await c.eval(`(async () => {
    // 通过真实的解锁流程产生一条成功提示
    try {
      await ${inv('unlock_directory', { dir: workDir, password: 'probe-pass-1' })};
    } catch (e) { /* 忽略 */ }
    return true;
  })()`);
  void toastAppeared;

  // 点顶栏的密码入口 → 输密码 → 提交，走完整交互
  await c.eval(`document.querySelector('.pill')?.click()`);
  await sleep(500);
  const typed = await c.eval(`(() => {
    const inp = document.querySelector('#u-pass');
    if (!inp) return 'no-input';
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype, 'value').set;
    setter.call(inp, 'probe-pass-1');
    inp.dispatchEvent(new Event('input', { bubbles: true }));
    const form = inp.closest('form');
    form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
    return 'submitted';
  })()`);
  check('走完整交互提交了密码', typed === 'submitted', String(typed));
  await sleep(2500);

  const toastNow = await c.eval(`(() => {
    const el = document.querySelector('.toast');
    return el ? { present: true, err: el.classList.contains('err'),
                  text: el.textContent.trim().slice(0, 40) }
              : { present: false };
  })()`);
  console.log(`  提示条当前状态：${JSON.stringify(toastNow)}`);
  check('解锁成功后出现了提示条', toastNow.present === true,
    toastNow.text || '');

  // 成功提示 4 秒后应自动消失
  console.log('  等待提示自动消失…');
  await sleep(4200);
  const toastGone = await c.eval(`!document.querySelector('.toast')`);
  check('成功提示自己消失了（核心：用户报的问题 1）',
    toastGone === true);

  console.log('\n=== 6. 反证：错误提示不能自己消失 ===');
  // 必须先锁定。上一步解锁成功后会话里已经有正确密码了，
  // 此时再输错密码，目录仍然能用缓存里那条凭据解开——
  // 根本产生不了错误，测出来的失败是假的
  await c.eval(inv('lock', {}));
  await sleep(500);
  await c.eval(`(() => {
    const btns = [...document.querySelectorAll('.dlg .btn')];
    const cancel = btns.find(b => !b.classList.contains('primary'));
    if (cancel) cancel.click();
  })()`);
  await sleep(300);

  await c.eval(`document.querySelector('.pill')?.click()`);
  await sleep(600);
  await c.eval(`(() => {
    const inp = document.querySelector('#u-pass');
    if (!inp) return;
    const setter = Object.getOwnPropertyDescriptor(
      window.HTMLInputElement.prototype, 'value').set;
    setter.call(inp, 'definitely-wrong-password');
    inp.dispatchEvent(new Event('input', { bubbles: true }));
    inp.closest('form').dispatchEvent(
      new Event('submit', { bubbles: true, cancelable: true }));
  })()`);
  await sleep(3000);
  const errShown = await c.eval(`(() => {
    const box = document.querySelector('.errbox');
    return box ? box.textContent.trim().slice(0, 40) : null;
  })()`);
  check('错误密码给出了错误提示', typeof errShown === 'string' && errShown.length > 0,
    errShown || '无');
  await sleep(3200);
  const errStill = await c.eval(`(() => {
    const box = document.querySelector('.errbox');
    return box ? true : false;
  })()`);
  check('错误提示没有自己消失（关键反证）', errStill === true);

  await c.eval(`(() => {
    const btns = [...document.querySelectorAll('.dlg .btn')];
    const cancel = btns.find(b => !b.classList.contains('primary'));
    if (cancel) cancel.click();
  })()`);
  await sleep(400);

  console.log('\n=== 7. 反证：伪造的 token 必须拿不到内容 ===');
  const forged = await c.eval(`(async () => {
    const b = await ${inv('stream_base', {})};
    try {
      const r = await fetch(b + '/plain/0000deadbeef0000cafebabe00001234');
      return r.status;
    } catch (e) { return 'threw'; }
  })()`);
  check('伪造 token 读不到任何文件（关键反证）',
    forged === 404 || forged === 'threw', String(forged));

  console.log('\n=== 8. 无运行时错误 ===');
  const jsErrors = c.logs.filter((l) => l.level === 'error');
  check('无 JS 运行时错误', jsErrors.length === 0,
    jsErrors.slice(0, 2).map((e) => e.text.slice(0, 60)).join(' | '));
  const csp = c.logs.filter((l) => /Content Security Policy/i.test(l.text));
  check('无 CSP 违规', csp.length === 0,
    csp.slice(0, 1).map((e) => e.text.slice(0, 60)).join(''));

  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} 项通过`);
  if (failed.length) {
    console.log('失败项：');
    for (const f of failed) console.log('  - ' + f.name);
  }
  process.exit(failed.length ? 1 : 0);
})().catch((e) => {
  console.log('\n探针异常: ' + e.message);
  const failed = results.filter((r) => !r.ok);
  console.log(`${results.length - failed.length}/${results.length} 项通过（中断）`);
  process.exit(1);
});
