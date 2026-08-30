/** 验证文件夹加密的完整链路。
 *
 * 单元测试能证明「打包函数对」「索引区间对」，但证明不了：
 *
 * - 在真实界面里选中一个文件夹、点加密，到底走不走得通
 * - 加密完的文件夹在列表里显示成什么样（是文件还是文件夹？）
 * - 双击它会不会被当成单个文件送去预览（那会得到一堆拼接字节）
 * - 产物有没有落在正被打包的目录里
 *
 * 所以这里全程走真实 DOM：点侧栏进目录、点选文件夹、点工具栏加密、
 * 在真实对话框里填密码、双击产物。
 *
 * 判据取用户看得见的那层：
 * - 容器面板出现且列出了正确的文件名
 * - 面包屑能进下一层
 * - 列表里的图标是文件夹而不是文件
 *
 * 用法：node --experimental-websocket probe-folder-encrypt.mjs <port> <目录>
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
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m);
        this.pending.delete(m.id);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 30000);
      this.pending.set(id, (m) => {
        clearTimeout(timer);
        res(m);
      });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expr) {
    const m = await this.send('Runtime.evaluate', {
      expression: `(async () => { return (${expr}); })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    if (m.result?.exceptionDetails) {
      return {
        __err:
          m.result.exceptionDetails.exception?.description ||
          m.result.exceptionDetails.text,
      };
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

/** 派发真实双击到包含指定文字的那一行。 */
const dbl = (name) => `(() => {
  const rows = [...document.querySelectorAll('.lrow, .card, .cell')];
  const row = rows.find(r => r.textContent.includes(${JSON.stringify(name)}));
  if (!row) return 'not-found:' + rows.length;
  row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
  return 'dispatched';
})()`;

/** 单击选中包含指定文字的那一行。 */
const click = (name) => `(() => {
  const rows = [...document.querySelectorAll('.lrow, .card, .cell')];
  const row = rows.find(r => r.textContent.includes(${JSON.stringify(name)}));
  if (!row) return 'not-found:' + rows.length;
  row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
  return 'clicked';
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

  console.log('\n=== 1. 通过侧栏真实进入测试目录 ===');
  const sideEntered = await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const target = items.find(i => i.textContent.includes('文档')
                              || i.textContent.includes('Documents'));
    if (!target) return 'no-documents:' + items.length;
    target.click();
    return 'clicked';
  })()`);
  check('侧栏里能点到「文档」', sideEntered === 'clicked', String(sideEntered));
  await sleep(1200);

  const intoSub = await c.eval(dbl('omy-folder-test'));
  check('双击进入测试目录', intoSub === 'dispatched', String(intoSub));
  await sleep(1400);

  const beforeRows = await c.eval(
    `[...document.querySelectorAll('.lrow, .card, .cell')].map(r => r.textContent.trim().slice(0,40))`);
  check('看到待加密的文件夹', 
    Array.isArray(beforeRows) && beforeRows.some((r) => r.includes('my-folder')),
    JSON.stringify(beforeRows));

  console.log('\n=== 2. 选中文件夹并加密 ===');
  const sel = await c.eval(click('my-folder'));
  check('单击选中文件夹', sel === 'clicked', String(sel));
  await sleep(500);

  // 点工具栏的加密按钮
  const encBtn = await c.eval(`(() => {
    const btns = [...document.querySelectorAll('button')];
    const b = btns.find(x => x.textContent.includes('加密')
                          || x.textContent.includes('Encrypt'));
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'clicked';
  })()`);
  check('加密按钮可用且已点击（文件夹不再被拒绝）',
    encBtn === 'clicked', String(encBtn));
  await sleep(900);

  const dialogUp = await c.eval(
    `!!document.querySelector('.dlg, .dialog, .mask')`);
  check('加密对话框已弹出', dialogUp === true, String(dialogUp));

  // 填密码并提交
  const filled = await c.eval(`(() => {
    const inputs = [...document.querySelectorAll('input[type=password]')];
    if (!inputs.length) return 'no-input';
    for (const i of inputs) {
      i.value = 'folder-pw';
      i.dispatchEvent(new Event('input', { bubbles: true }));
    }
    return 'filled:' + inputs.length;
  })()`);
  check('密码已填入', String(filled).startsWith('filled'), String(filled));
  await sleep(400);

  const submitted = await c.eval(`(() => {
    const btns = [...document.querySelectorAll('.dlg button, .dialog button, .mask button')];
    const b = btns.find(x => (x.textContent.includes('加密')
                           || x.textContent.includes('Encrypt'))
                          && !x.disabled);
    if (!b) return 'no-submit:' + btns.map(x=>x.textContent.trim()).join('|');
    b.click();
    return 'submitted';
  })()`);
  check('已提交加密', submitted === 'submitted', String(submitted));

  // 文件夹加密要读全部文件 + Argon2，给足时间
  await sleep(6000);

  console.log('\n=== 3. 产物检查 ===');
  const listing = await c.eval(inv('browse_directory', { dir: workDir }));
  const names = (listing || []).map((e) => e.name);
  check('产物落在被加密目录的父级（不在 my-folder 里面）',
    names.some((n) => n.endsWith('.omy')),
    JSON.stringify(names));

  // 反证：产物绝不能写进正被打包的目录
  const inside = await c.eval(
    inv('browse_directory', { dir: workDir + '\\\\my-folder' }));
  const insideNames = (inside || []).map((e) => e.name);
  check('被加密的目录里没有多出 .omy（反证）',
    !insideNames.some((n) => n.endsWith('.omy')),
    JSON.stringify(insideNames));

  console.log('\n=== 4. 加密后的文件夹在界面上的样子 ===');
  await sleep(800);
  const enc = (listing || []).find((e) => e.name.endsWith('.omy'));
  check('产物被识别为加密文件', enc?.is_encrypted === true, String(enc?.name));
  check('加密后立即处于解锁状态', enc?.unlocked === true, String(enc?.unlocked));

  // 补元信息才会带上 is_container
  const enriched = await c.eval(inv('enrich_file', { id: enc?.entry_id }));
  check('后端认出这是目录容器',
    enriched?.is_container === true, String(enriched?.is_container));
  check('真实文件夹名已还原',
    enriched?.name === 'my-folder', String(enriched?.name));

  console.log('\n=== 5. 双击进入容器浏览 ===');
  // 走真实导航让列表带上 is_container：上一层再进来一次。
  // 不直接调 invoke —— 那会绕过前端的 refreshKnown，而
  // 「字段有没有传到列表条目上」正是这里要验的东西
  const upDown = await c.eval(`(() => {
    const segs = [...document.querySelectorAll('.crumbpath .crumbseg')];
    if (segs.length < 2) return 'no-crumbs:' + segs.length;
    segs[segs.length - 2].click();
    return 'up';
  })()`);
  await sleep(1200);
  const backIn = await c.eval(dbl('omy-folder-test'));
  await sleep(2000);
  check('回到测试目录（真实导航，触发扫描回填）',
    upDown === 'up' && backIn === 'dispatched',
    `${upDown}/${backIn}`);

  // 双击前先看清条目上的真实字段——面板不出现时，
  // 要能一眼分辨是「字段没传到」还是「分流逻辑没接住」
  const beforeDbl = await c.eval(inv('browse_directory', { dir: workDir }));
  const encRow = (beforeDbl || []).find((e) => e.is_encrypted);
  console.log('    [诊断] 列表条目:', JSON.stringify({
    name: encRow?.name, unlocked: encRow?.unlocked,
    entry_id: encRow?.entry_id, is_container: encRow?.is_container,
    real_name: encRow?.real_name,
  }));
  const credCount = await c.eval(inv('credential_count', {}));
  console.log('    [诊断] 会话凭据数:', JSON.stringify(credCount));

  // 关键：界面渲染的是 state.entries（经 refreshKnown 回填过），
  // 不是 browse_directory 的直接返回。两者不一致时问题就在回填这一步
  const feRow = await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.lrow, .card, .cell')];
    const r = rows.find(x => x.textContent.includes('🔓')
                          || x.textContent.includes('🔒'));
    return r ? r.textContent.trim().slice(0, 60) : 'no-row';
  })()`);
  console.log('    [诊断] 界面上那一行:', JSON.stringify(feRow));

  // 注意：原文件夹保留着（original=keep），列表里同时有
  // 📁my-folder（原目录）和 🔒加密文件（产物）。
  // 按名字匹配会命中原目录，双击它只是进目录，什么都测不到——
  // 必须按锁图标定位加密产物那一行
  const dblEnc = await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.lrow, .card, .cell')];
    const row = rows.find(r => r.textContent.includes('🔓')
                            || r.textContent.includes('🔒'));
    if (!row) return 'not-found:' + rows.map(r => r.textContent.trim().slice(0,20)).join('|');
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击加密文件夹', dblEnc === 'dispatched', String(dblEnc));
  await sleep(1800);

  const panel = await c.eval(`(() => {
    // 收紧选择器：只认带容器面包屑的那个面板，
    // 避免撞上设备面板之类的其他 .panel
    const p = document.querySelector('.panel .crumbs')?.closest('.panel');
    if (!p) return { present: false };
    const rows = [...p.querySelectorAll('.row')].map(r => r.textContent.trim());
    return { present: true, rows, title: p.querySelector('.name')?.textContent || '' };
  })()`);
  check('容器面板出现了', panel.present === true, String(panel.present));
  check('面板标题是文件夹真名',
    (panel.title || '').includes('my-folder'), String(panel.title));
  check('列出了容器内的文件',
    Array.isArray(panel.rows) && panel.rows.some((r) => r.includes('hello.txt')),
    JSON.stringify(panel.rows));
  check('也列出了子目录',
    Array.isArray(panel.rows) && panel.rows.some((r) => r.includes('inner')),
    JSON.stringify(panel.rows));

  // 关键反证：容器绝不能被当成单个文件送去预览
  const wrongOverlay = await c.eval(
    `!!document.querySelector('.overlay img, .overlay video, .overlay pre')`);
  check('容器没有被误当成单个文件预览（反证）',
    wrongOverlay === false, String(wrongOverlay));

  console.log('\n=== 6. 进入子目录 ===');
  const intoInner = await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.panel .row')];
    const r = rows.find(x => x.textContent.includes('inner'));
    if (!r) return 'not-found';
    r.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击子目录', intoInner === 'dispatched', String(intoInner));
  await sleep(900);

  const innerRows = await c.eval(`(() => {
    const p = document.querySelector('.panel');
    if (!p) return null;
    return {
      rows: [...p.querySelectorAll('.row')].map(r => r.textContent.trim()),
      crumbs: [...p.querySelectorAll('.crumbs button')].map(b => b.textContent.trim()),
    };
  })()`);
  check('子目录内容已显示',
    innerRows?.rows?.some((r) => r.includes('deep.txt')),
    JSON.stringify(innerRows?.rows));
  check('面包屑显示了层级',
    Array.isArray(innerRows?.crumbs) && innerRows.crumbs.length >= 2,
    JSON.stringify(innerRows?.crumbs));

  // 只显示直接子项：根目录的 hello.txt 不该出现在 inner 里
  check('只列出直接子项（根目录的文件不串进来）',
    !innerRows?.rows?.some((r) => r.includes('hello.txt')),
    JSON.stringify(innerRows?.rows));

  console.log('\n=== 7. 锁定后容器面板要关掉 ===');
  // 锁定按钮是个 🔒 图标，没有文字，按 title / aria-label 找
  const lockClicked = await c.eval(`(() => {
    const btns = [...document.querySelectorAll('.iconbtn')];
    const b = btns.find(x => x.textContent.includes('🔒'));
    if (!b) return 'no-lock-button';
    b.click();
    return 'clicked';
  })()`);
  check('点到了锁定按钮', lockClicked === 'clicked', String(lockClicked));
  await sleep(1500);
  const afterLock = await c.eval(`!!document.querySelector('.panel .row')`);
  check('锁定后容器内容不再可见', afterLock === false, String(afterLock));

  const errs = c.logs.filter((l) => l.level === 'error');
  check('运行期没有前端异常', errs.length === 0,
    errs.slice(0, 2).map((e) => e.text.slice(0, 80)).join(' | '));

  const passed = results.filter((r) => r.ok).length;
  console.log(`\n================ ${passed}/${results.length} ================`);
  ws.close();
  process.exit(passed === results.length ? 0 : 1);
})().catch((e) => {
  console.error('探针失败:', e.message);
  process.exit(2);
});
