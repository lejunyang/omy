/** 验证「加密文件夹」在真实界面里的浏览与单文件预览。
 *
 * 单元测试能证明「打包函数对」「索引区间对」「偏移算得对」，但证明不了：
 *
 * - 在真实界面里选中一个文件夹、点加密，到底走不走得通
 * - 加密完的文件夹在列表里显示成什么样（是文件还是文件夹？）
 * - 双击它会不会被当成单个文件送去预览（那会得到一堆拼接字节）
 * - 产物有没有落在正被打包的目录里
 * - 进到容器里之后，双击里面的**单个文件**能不能真的预览出内容
 * - 容器视图用的是不是主列表那套渲染（而不是另一个弹窗）
 *
 * 所以这里全程走真实 DOM：点侧栏进目录、点选文件夹、点工具栏加密、
 * 在真实对话框里填密码、双击产物、再双击容器内的文件。
 *
 * 判据取用户看得见的那层：图标、面包屑、列表行、预览层里真实解码出的
 * 像素与文字。容器内文件预览是本轮新增能力，重点验「读出的是它自己的
 * 内容」——偏移算错不会报错，只会安静地显示相邻文件的字节。
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

/** 主列表里的所有行。网格与列表两种视图的类名不同，都要覆盖。 */
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

/** 预览层。
 *
 * 不能只写 `.overlay`：加密/解锁对话框与设备面板也用 `.overlay dlg-overlay`，
 * 那样「锁定后还有没有预览层」会被一个无关对话框判成失败。
 */
const PREVIEW = '.overlay:not(.dlg-overlay)';

/** 派发真实双击到包含指定文字的那一行。 */
const dbl = (name) => `(() => {
  const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(name)}));
  if (!row) return 'not-found:' + ${ROWS}.length;
  row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
  return 'dispatched';
})()`;

/** 单击选中包含指定文字的那一行。 */
const click = (name) => `(() => {
  const row = ${ROWS}.find(r => r.textContent.includes(${JSON.stringify(name)}));
  if (!row) return 'not-found:' + ${ROWS}.length;
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
    `${ROWS}.map(r => r.textContent.trim().slice(0,40))`);
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

  console.log('\n=== 5. 双击进入容器（复用文件夹的渲染，不是弹窗）===');
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

  // 双击前先看清条目上的真实字段——进不去时，
  // 要能一眼分辨是「字段没传到」还是「分流逻辑没接住」
  const beforeDbl = await c.eval(inv('browse_directory', { dir: workDir }));
  const encRow = (beforeDbl || []).find((e) => e.is_encrypted);
  console.log('    [诊断] 列表条目:', JSON.stringify({
    name: encRow?.name, unlocked: encRow?.unlocked,
    entry_id: encRow?.entry_id, is_container: encRow?.is_container,
    real_name: encRow?.real_name,
  }));

  // 注意：原文件夹保留着（original=keep），列表里同时有
  // 📁my-folder（原目录）和 🔒加密文件（产物）。
  // 按名字匹配会命中原目录，双击它只是进目录，什么都测不到——
  // 必须按锁图标定位加密产物那一行
  const dblEnc = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('🔓')
                              || r.textContent.includes('🔒'));
    if (!row) return 'not-found:' + ${ROWS}.map(r => r.textContent.trim().slice(0,20)).join('|');
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击加密文件夹', dblEnc === 'dispatched', String(dblEnc));
  await sleep(1800);

  // 关键：容器内容必须渲染在**主列表**里，而不是一个独立弹窗。
  // 这是本轮改造的核心诉求——「就像正常打开文件夹一样」
  const view = await c.eval(`(() => {
    const rows = ${ROWS}.map(r => r.textContent.trim());
    return {
      rows,
      crumbs: [...document.querySelectorAll('.crumbpath .crumbseg')]
                .map(b => b.textContent.trim()),
      badge: document.querySelector('.statusbar .cbadge')?.textContent.trim() || '',
      // 老的独立面板：现在必须已经不存在
      legacyPanel: !!document.querySelector('.panel .crumbs'),
    };
  })()`);
  check('容器内容渲染在主列表里（不是独立弹窗）',
    Array.isArray(view.rows) && view.rows.some((r) => r.includes('hello.txt')),
    JSON.stringify(view.rows));
  check('旧的独立容器面板已不存在（反证）',
    view.legacyPanel === false, String(view.legacyPanel));
  check('面包屑把容器显示为一段路径',
    Array.isArray(view.crumbs) && view.crumbs.some((s) => s.includes('my-folder')),
    JSON.stringify(view.crumbs));
  check('状态栏标明这是加密文件夹的内容',
    view.badge.length > 0, JSON.stringify(view.badge));
  check('也列出了子目录',
    Array.isArray(view.rows) && view.rows.some((r) => r.includes('inner')),
    JSON.stringify(view.rows));

  // 反证：容器本身绝不能被当成单个文件送去预览
  const wrongOverlay = await c.eval(
    `!!document.querySelector('${PREVIEW} img, ${PREVIEW} video, ${PREVIEW} pre')`);
  check('容器没有被误当成单个文件预览（反证）',
    wrongOverlay === false, String(wrongOverlay));

  console.log('\n=== 6. 预览容器内的单个文件（本轮新增能力）===');
  // 文本文件：能拿到内容，才算真的解出来了
  const dblTxt = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('hello.txt'));
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击容器内的 hello.txt', dblTxt === 'dispatched', String(dblTxt));
  await sleep(2000);

  const txtView = await c.eval(`(() => {
    const ov = document.querySelector('${PREVIEW}');
    if (!ov) return { present: false };
    return {
      present: true,
      title: ov.querySelector('.title')?.textContent.trim() || '',
      text: ov.querySelector('pre')?.textContent || '',
      msg: ov.querySelector('.overlay-msg')?.textContent.trim() || '',
      // 容器内的条目没有磁盘文件，不该出现「用其他应用打开」
      hasExternalBtn: !!ov.querySelector('.btn.primary'),
    };
  })()`);
  check('预览层出现了', txtView.present === true, String(txtView.present));
  check('标题是容器内文件名',
    (txtView.title || '').includes('hello.txt'), JSON.stringify(txtView.title));
  // 这一项是整条链路的最终判据：内容对，说明偏移、密钥、Range 全对
  check('解出的正文内容正确（偏移与解密都对）',
    (txtView.text || '').includes('hello from folder'),
    JSON.stringify((txtView.text || '').slice(0, 60) + ' msg=' + txtView.msg));
  check('容器内文件不提供「用其他应用打开」（反证：无磁盘文件可开）',
    txtView.hasExternalBtn === false, String(txtView.hasExternalBtn));

  // 关掉预览，回到容器列表
  await c.eval(`(() => {
    const b = document.querySelector('${PREVIEW} .overlay-bar .iconbtn');
    if (b) b.click();
    return 'closed';
  })()`);
  await sleep(900);

  // 图片：验证解出的是真图（能被浏览器解码出宽高），
  // 而不是「有响应但内容是别的文件的字节」
  const dblPng = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('pic.png'));
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击容器内的 pic.png', dblPng === 'dispatched', String(dblPng));
  await sleep(2200);

  const imgView = await c.eval(`(async () => {
    const img = document.querySelector('${PREVIEW} img');
    if (!img) return { present: false,
      msg: document.querySelector('${PREVIEW} .overlay-msg')?.textContent.trim() || '' };
    // 等它真的解码完，否则拿到的是 0x0
    for (let i = 0; i < 40 && !(img.naturalWidth > 0); i++) {
      await new Promise(r => setTimeout(r, 100));
    }
    return { present: true, w: img.naturalWidth, h: img.naturalHeight,
             src: (img.currentSrc || img.src).slice(0, 48) };
  })()`);
  check('容器内的图片能真正解码出来（4x4）',
    imgView.present === true && imgView.w === 4 && imgView.h === 4,
    JSON.stringify(imgView));
  check('图片走的是容器专用协议路径 /citem/',
    String(imgView.src || '').includes('/citem/'), JSON.stringify(imgView.src));

  await c.eval(`(() => {
    const b = document.querySelector('${PREVIEW} .overlay-bar .iconbtn');
    if (b) b.click();
    return 'closed';
  })()`);
  await sleep(900);

  console.log('\n=== 7. 容器内的子目录导航 ===');
  const intoInner = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('inner'));
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击容器内的子目录', intoInner === 'dispatched', String(intoInner));
  await sleep(1200);

  const innerView = await c.eval(`(() => ({
    rows: ${ROWS}.map(r => r.textContent.trim()),
    crumbs: [...document.querySelectorAll('.crumbpath .crumbseg')]
              .map(b => b.textContent.trim()),
  }))()`);
  check('子目录内容已显示',
    innerView?.rows?.some((r) => r.includes('deep.txt')),
    JSON.stringify(innerView?.rows));
  check('面包屑追加了容器内的层级',
    Array.isArray(innerView?.crumbs) && innerView.crumbs.some((s) => s.includes('inner')),
    JSON.stringify(innerView?.crumbs));
  // 只显示直接子项：根目录的 hello.txt 不该出现在 inner 里
  check('只列出直接子项（根目录的文件不串进来）',
    !innerView?.rows?.some((r) => r.includes('hello.txt')),
    JSON.stringify(innerView?.rows));

  // 深一层的文件同样要能预览：证明偏移不是只对第一层碰巧算对
  const dblDeep = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('deep.txt'));
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  await sleep(2000);
  const deepText = await c.eval(
    `document.querySelector('${PREVIEW} pre')?.textContent || 'none'`);
  check('子目录里的文件也能预览出正确内容',
    dblDeep === 'dispatched' && String(deepText).includes('deep file'),
    JSON.stringify(String(deepText).slice(0, 40)));

  await c.eval(`(() => {
    const b = document.querySelector('${PREVIEW} .overlay-bar .iconbtn');
    if (b) b.click();
    return 'closed';
  })()`);
  await sleep(700);

  console.log('\n=== 8. 用面包屑退回容器根，再退回磁盘目录 ===');
  // 按 .box 类定位容器那一段，**不能**按文字找：
  // 测试目录名 'omy-folder-test' 本身就包含 'my-folder'，
  // 按文字匹配会命中磁盘那一段，一点就退出了容器，后面全跟着错
  const backToRoot = await c.eval(`(() => {
    const box = document.querySelector('.crumbpath .crumbseg.box');
    if (!box) return 'no-container-crumb';
    box.click();
    return 'clicked';
  })()`);
  await sleep(1000);
  const rootRows = await c.eval(`${ROWS}.map(r => r.textContent.trim())`);
  check('点容器面包屑回到容器根',
    backToRoot === 'clicked' && rootRows.some((r) => r.includes('hello.txt')),
    JSON.stringify(rootRows));

  // ↑ 在容器根上应当退出容器，回到磁盘目录
  const upOut = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.crumbbtn')]
                .find(x => x.textContent.includes('↑'));
    if (!b) return 'no-up';
    b.click();
    return 'clicked';
  })()`);
  await sleep(1400);
  const outView = await c.eval(`(() => ({
    rows: ${ROWS}.map(r => r.textContent.trim().slice(0, 24)),
    badge: !!document.querySelector('.statusbar .cbadge'),
  }))()`);
  check('在容器根按 ↑ 退出容器回到磁盘目录',
    upOut === 'clicked' && outView.badge === false
      && outView.rows.some((r) => r.includes('my-folder')),
    JSON.stringify(outView));

  console.log('\n=== 9. 锁定后容器内容要立刻不可见 ===');
  // 先重新进容器，才能验证锁定会把它收掉
  await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('🔓')
                              || r.textContent.includes('🔒'));
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  await sleep(1800);
  const inAgain = await c.eval(
    `${ROWS}.some(r => r.textContent.includes('hello.txt'))`);
  check('重新进入容器成功（为锁定测试做准备）', inAgain === true, String(inAgain));

  // 锁定按钮是个 🔒 图标，没有文字，按内容找
  const lockClicked = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.titlebar .iconbtn')]
                .find(x => x.textContent.includes('🔒'));
    if (!b) return 'no-lock-button';
    b.click();
    return 'clicked';
  })()`);
  check('点到了锁定按钮', lockClicked === 'clicked', String(lockClicked));
  await sleep(1800);

  const afterLock = await c.eval(`(() => ({
    stillListing: ${ROWS}.some(r => r.textContent.includes('hello.txt')),
    badge: !!document.querySelector('.statusbar .cbadge'),
    overlay: !!document.querySelector('${PREVIEW}'),
  }))()`);
  check('锁定后容器内容不再可见',
    afterLock.stillListing === false && afterLock.badge === false,
    JSON.stringify(afterLock));
  check('锁定后没有残留的预览层', afterLock.overlay === false,
    String(afterLock.overlay));

  // 后端保证：锁定后同一个 token 必须失效。
  // 前端不显示不算数——真正的判据是协议层还给不给数据
  const tokenAfterLock = await c.eval(
    `${inv('list_container', { id: enc?.entry_id })}
       .then(() => 'still-works')
       .catch(e => 'rejected:' + (e?.code || e?.message || e))`,
  );
  check('锁定后后端拒绝再列出容器内容（后端保证）',
    String(tokenAfterLock).startsWith('rejected'), String(tokenAfterLock));

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
