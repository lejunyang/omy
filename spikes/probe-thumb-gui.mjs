/** 定位「GUI 里看不到缩略图」。
 *
 * 用法：node --experimental-websocket probe-thumb-gui.mjs <port> <目录>
 *
 * # 为什么要分层断言
 *
 * 从「加密时写了缩略图」到「用户看见图」中间有六层：后端 TLV →
 * has_thumbnail 字段 → 前端 state.known → 条目的 entry_id →
 * img 元素 → 真正解码。只断言最后一层的话，任何一层断了都报同一个
 * 失败，还得手工二分。所以每层各一条断言。
 *
 * # 已经踩过的三个真缺陷都在这条链上
 *
 *   1. --thumbnail auto 一律走视频抽帧，图片永远拿不到缩略图；
 *   2. GUI 从不调用 prepare，thumbnail 字段恒为空；
 *   3. 缩略图实际是 WebP，而 /thumb 硬编码 image/jpeg。
 *
 * 第 3 个说明为什么必须验到用户看到的那一层，又为什么单看渲染不够：
 * 本地 WebView 会自己嗅探真实格式照样显示，所以 MIME 写错时图片
 * 看起来完全正常。L6 是它唯一能被自动抓到的地方。
 *
 * # 元素定位一律按结构
 *
 * 不按文案：文案随语言变，且名字会互相包含（AGENTS.md 记过
 * `omy-folder-test` 包含 `my-folder` 这个坑）。
 */

const port = process.argv[2] || '9379';
const WORK = process.argv[3];
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
        .split('\n')[0].slice(0, 160);
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
    const row = ${rowByName('omy-thumb-gui-test')};
    if (!row) return 'not-found:' + ${ROWS}.map(r=>r.textContent.trim().slice(0,18)).join('|');
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  check('双击进入测试目录', into === 'ok', String(into));
  await sleep(1500);

  console.log('\n--- 2. 在界面里加密一张真图片 ---');
  const sel = await c.eval(`(() => {
    const row = ${rowByName('shot.png')};
    if (!row) return 'not-found:' + ${ROWS}.map(r=>r.textContent.trim().slice(0,18)).join('|');
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  check('选中图片', sel === 'ok', String(sel));
  await sleep(600);

  const encBtn = await c.eval(`(() => {
    const b = [...document.querySelectorAll('button')]
      .find(x => x.textContent.includes('加密') || x.textContent.includes('Encrypt'));
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  check('加密按钮可用并已点击', encBtn === 'ok', String(encBtn));
  await sleep(1000);

  const filled = await c.eval(`(() => {
    const ins = [...document.querySelectorAll('.dlg input[type=password]')];
    if (!ins.length) return 'no-input';
    for (const i of ins) {
      i.value = 'thumb-pw';
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

  // 轮询等产物：Argon2 耗时不定，固定等待要么不够要么白等
  let encName = null;
  for (let i = 0; i < 40; i++) {
    const listed = await c.eval(inv('browse_directory', { dir: WORK }));
    const names = Array.isArray(listed) ? listed.map((e) => e.name) : [];
    encName = names.find((n) => n.endsWith('.omy')) || null;
    if (encName) break;
    await sleep(500);
  }
  check('产出了 .omy 文件', !!encName, String(encName));
  const path = encName ? `${WORK}\\${encName}` : null;

  console.log('\n--- L1 后端：缩略图真的写进文件了吗 ---');
  let entry = null;
  if (path) {
    const vp = await c.eval(inv('vault_params_of', { path }));
    const vaults = Array.isArray(vp) ? vp : (vp ? [vp] : []);
    const un = await c.eval(inv('unlock', {
      label: 'probe', password: 'thumb-pw', vaults,
    }));
    check('解锁成功', un && !String(un).startsWith('JS-ERR'),
      JSON.stringify(un).slice(0, 150));
    await sleep(800);
    const files = await c.eval(inv('list_files', {}));
    const arr = Array.isArray(files) ? files : [];
    entry = arr.find((e) => String(e.path || '') === path) || null;
  }
  check('L1 后端报告 has_thumbnail=true',
    entry && entry.has_thumbnail === true,
    JSON.stringify(entry && {
      name: entry.name, unlocked: entry.unlocked,
      has_thumbnail: entry.has_thumbnail, kind: entry.kind,
    }));

  console.log('\n--- L2/L3 前端状态：known 与 entry_id ---');
  // 走界面自己的刷新路径（重新进目录），而不是直接调后端：
  // 要验的正是「界面刷新后 state.known 有没有这条」。
  await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const t = items.find(i => i.textContent.includes('文档')
                           || i.textContent.includes('Documents'));
    if (t) t.click();
    return true;
  })()`);
  await sleep(1200);
  await c.eval(`(() => {
    const row = ${rowByName('omy-thumb-gui-test')};
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return true;
  })()`);
  await sleep(1800);

  // state.known 与条目都从 DOM 侧观察不到，只能看渲染结果；
  // 但 entry_id 会出现在 img 的 src 里，所以用「有没有 img」间接判断。
  // 这里额外用后端返回的 id 做交叉核对。
  const dom = await c.eval(`(() => {
    const cards = [...document.querySelectorAll('.grid .card')];
    const imgs = [...document.querySelectorAll('.grid .card .thumb img')];
    const locked = document.querySelectorAll('.grid .card.locked').length;
    return {
      view: document.querySelector('.grid') ? 'grid' : 'list',
      cards: cards.length, imgs: imgs.length, locked,
      // 每张卡的缩略图区域里到底是什么：img 还是 emoji
      thumbs: cards.map(x => {
        const im = x.querySelector('.thumb img');
        if (im) return 'img';
        const sp = x.querySelector('.thumb span');
        return sp ? sp.textContent.trim() : '?';
      }),
      names: cards.map(x => (x.getAttribute('aria-label') || '').slice(0, 22)),
    };
  })()`);
  console.log('  [诊断] DOM 现状 ' + JSON.stringify(dom));
  check('L2 视图是网格（缩略图只在网格里渲染）',
    dom && dom.view === 'grid', JSON.stringify(dom && dom.view));
  check('L3 加密文件已显示为已解锁（不是锁定卡片）',
    dom && dom.locked === 0,
    `锁定卡片数=${dom && dom.locked}，thumbs=${JSON.stringify(dom && dom.thumbs)}`);

  console.log('\n--- L4/L5 渲染：img 存在且真的解码成功 ---');
  let img = null;
  for (let i = 0; i < 24; i++) {
    img = await c.eval(`(() => {
      const ims = [...document.querySelectorAll('.grid .card .thumb img')];
      if (!ims.length) return { found: false };
      const im = ims[0];
      return { found: true, complete: im.complete, n: ims.length,
               w: im.naturalWidth, h: im.naturalHeight,
               src: String(im.getAttribute('src') || '').slice(0, 70) };
    })()`);
    if (img && img.found && img.complete && img.w > 0) break;
    await sleep(500);
  }
  check('L4 网格里出现了缩略图 img', img && img.found === true, JSON.stringify(img));
  check('L5 缩略图真的解码成功（naturalWidth>0）',
    img && img.w > 0 && img.h > 0, JSON.stringify(img));

  console.log('\n--- L6 /thumb 的 Content-Type 与真实字节一致 ---');
  if (entry && entry.id) {
    const base2 = await c.eval(inv('stream_base', {}));
    const mime = await c.eval(`(async () => {
      const r = await fetch(${JSON.stringify(String(base2))} + '/thumb/'
        + encodeURIComponent(${JSON.stringify(entry.id)}));
      const ct = r.headers.get('content-type');
      const b = new Uint8Array(await r.arrayBuffer());
      const isWebp = b.length > 12 && b[0]===0x52 && b[1]===0x49 && b[2]===0x46 &&
                     b[3]===0x46 && b[8]===0x57 && b[9]===0x45 && b[10]===0x42 && b[11]===0x50;
      const isJpeg = b.length > 3 && b[0]===0xFF && b[1]===0xD8 && b[2]===0xFF;
      return { status: r.status, ct, bytes: b.length,
               actual: isWebp ? 'image/webp' : (isJpeg ? 'image/jpeg' : 'other') };
    })()`);
    check('L6 /thumb 返回 200 且字节非空',
      mime && mime.status === 200 && mime.bytes > 100, JSON.stringify(mime));
    check('L6 Content-Type 与真实字节一致',
      mime && mime.ct === mime.actual, JSON.stringify(mime));
  }

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
