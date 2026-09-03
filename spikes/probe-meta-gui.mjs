/** 验证 GUI 加密的文件夹，元数据真的进了容器。
 *
 * 用法：node --experimental-websocket probe-meta-gui.mjs <port> <目录>
 *
 * # 这个探针要回答的问题
 *
 * GUI 与 CLI 共用 omy_core::pack，理论上元数据一定被采集。但「理论上
 * 共用」和「实际写进去了」是两件事：encrypt.rs 里那段注释记着同一条
 * 共用路径上漏传 argon2 的后果——文件当场打不开，而且症状极具迷惑性。
 * 所以走一遍真实界面，再交给 CLI 解开核对磁盘上的时间戳。
 *
 * # 为什么 GUI 侧没有「还原元数据」可验
 *
 * GUI 没有把明文写到磁盘的路径（已实测：产品代码里只有两处写盘，
 * encrypt.rs 写密文、keymgmt.rs 改头部），预览走 citem 协议按需解密、
 * 不落盘。所以元数据还原只发生在 CLI 解密时。GUI 侧真正该验的是
 * 采集这一半——它加密出来的容器如果没带元数据，CLI 那 27 项验证再全
 * 也救不回来，因为值根本没存进去。
 *
 * # 元素定位一律按结构
 *
 * 不按文案：文案随语言变，且名字会互相包含（AGENTS.md 记过
 * `omy-folder-test` 包含 `my-folder` 这个坑）。
 */

const port = process.argv[2] || '9363';
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
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m); this.pending.delete(m.id);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 60000);
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
        .split('\n')[0].slice(0, 140);
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

/** 精确匹配一行的名字。
 *
 * 用 === 而不是 includes：`omy-meta-gui-test` 与 `folder` 这类名字会互相
 * 包含，includes 会命中错的行，把探针 bug 显示成产品缺陷（AGENTS.md 记过
 * `omy-folder-test` 包含 `my-folder` 这个坑）。
 *
 * 名字优先读 aria-label：EntryCard 的目录与文件分支都把 entry.name 原样
 * 放进了 aria-label，那是最干净的来源，无障碍工具读的也是它。退回时用
 * .cname（网格，EntryCard.vue）和 .nm（列表，MainScreen.vue）——**不能**
 * 退回整行文本，整行含图标与类型后缀（「📁xxx文件夹」），精确比较必然失败。
 */
const rowByName = (name) => `${ROWS}.find(r => {
  const aria = r.getAttribute('aria-label');
  const el = r.querySelector('.cname, .nm');
  const txt = (aria || (el ? el.textContent : '')).trim();
  return txt === ${JSON.stringify(name)};
})`;

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

  console.log('\n--- 1. 进入测试目录 ---');
  const side = await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const t = items.find(i => i.textContent.includes('文档')
                           || i.textContent.includes('Documents'));
    if (!t) return 'no-documents:' + items.length;
    t.click();
    return 'clicked';
  })()`);
  check('侧栏进入文档目录', side === 'clicked', String(side));
  await sleep(1300);

  const into = await c.eval(`(() => {
    const row = ${rowByName('omy-meta-gui-test')};
    if (!row) return 'not-found:' + ${ROWS}.map(r=>r.textContent.trim().slice(0,20)).join('|');
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  check('双击进入测试目录', into === 'ok', String(into));
  await sleep(1400);

  const seen = await c.eval(`(() => {
    const row = ${rowByName('folder')};
    return row ? 'found' : 'missing:' + ${ROWS}.map(r=>r.textContent.trim().slice(0,20)).join('|');
  })()`);
  check('看到待加密的文件夹', seen === 'found', String(seen));

  console.log('\n--- 2. 选中并加密 ---');
  const sel = await c.eval(`(() => {
    const row = ${rowByName('folder')};
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  check('选中文件夹', sel === 'ok', String(sel));
  await sleep(600);

  const encBtn = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.toolbar button, .tbar button, button')]
      .find(x => x.textContent.includes('加密') || x.textContent.includes('Encrypt'));
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  check('加密按钮可用并已点击', encBtn === 'ok', String(encBtn));
  await sleep(1000);

  const dlg = await c.eval(`!!document.querySelector('.dlg')`);
  check('加密对话框已弹出', dlg === true, String(dlg));

  const filled = await c.eval(`(() => {
    const ins = [...document.querySelectorAll('.dlg input[type=password]')];
    if (!ins.length) return 'no-input';
    for (const i of ins) {
      i.value = 'gui-meta-pw';
      i.dispatchEvent(new Event('input', { bubbles: true }));
    }
    return 'filled:' + ins.length;
  })()`);
  check('密码已填入', String(filled).startsWith('filled'), String(filled));
  await sleep(500);

  const sub = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b) return 'no-submit';
    if (b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  check('已提交加密', sub === 'ok', String(sub));

  console.log('\n--- 3. 产物确认 ---');
  // 通过后端确认，而不是看列表渲染：列表可能还没刷新，
  // 那会把「界面没刷新」报成「加密失败」，指错方向。
  //
  // browse_directory 返回的是 Vec<DirEntry>，也就是数组本身，
  // 不是 { entries: [...] }。早先按 .entries 取，拿到 undefined，
  // 于是把「产出了 folder.omy」报成失败，而前面 8 项全是通过的——
  // 两个结论矛盾时先怀疑脚本。
  const wd = process.argv[3];
  // 轮询而不是死等：文件夹加密要读全部文件再跑 Argon2，固定等待要么
  // 不够、要么白等。超时才报错，并打印实际看到的文件名
  //
  // 产物名**不是** folder.omy：GUI 默认开启文件名加密，落盘名是随机的
  // （实测 a4332ff02cbd0378.omy），原名存在加密的 TLV 里。这是有意设计，
  // 文件名本身就是元数据泄露面。CLI 产出 folder.omy 是因为它按 -o 显式
  // 指定了路径——两个入口默认行为不同，拿一个的行为推断另一个就会出错。
  //
  // 所以按扩展名找，不按名字找。断言反而更严：名字无关紧要，
  // 内容对不对才是要点
  let names = [];
  let encName = null;
  for (let i = 0; i < 40; i++) {
    const listed = await c.eval(inv('browse_directory', { dir: wd }));
    names = Array.isArray(listed) ? listed.map((e) => e.name) : [];
    encName = names.find((n) => n.endsWith('.omy')) || null;
    if (encName) break;
    await sleep(500);
  }
  check('产出了 .omy 文件', !!encName,
    '目录里实际有：' + JSON.stringify(names));

  // 后端自己也认它是加密文件。只看文件存在的话，
  // 「写出了一堆字节但头部是坏的」也能通过
  let probe = null;
  if (encName) {
    probe = await c.eval(inv('probe_one', { path: `${wd}\\${encName}` }));
  }
  check('后端识别为加密文件',
    probe && probe.is_omy === true,
    JSON.stringify(probe));

  // 把产物名交给驱动脚本，它要用 CLI 独立解开核对元数据。
  // 名字是随机的，脚本自己猜不出来
  if (encName) console.log('ENC_NAME=' + encName);

  const okAll = results.every((r) => r.ok);
  console.log(`\n  探针合计 ${results.filter((r) => r.ok).length} 通过 / ` +
    `${results.filter((r) => !r.ok).length} 失败`);
  if (okAll) console.log('PROBE_OK');
  ws.close();
  process.exit(okAll ? 0 : 1);
})().catch((e) => {
  console.log('探针异常: ' + e.message);
  process.exit(1);
});
