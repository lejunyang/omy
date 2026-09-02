/** 验证 GUI 加密进度条：事件真的到达前端，且进度条真的渲染出来。
 *
 * 用法：node --experimental-websocket probe-encrypt-progress.mjs <port> <目录>
 *
 * # 为什么要在加密**进行中**采样
 *
 * 加密结束后 `state.progress` 会被清成 null、进度条随之消失。只在结束后
 * 检查，看到的永远是「没有进度条」，分不清「功能没做」和「已经收尾」。
 * 所以这里用高频轮询在加密过程中抓快照。
 *
 * # 定位按结构不按文字
 *
 * 元素一律用类名（.prog / .prog-fill）定位。按文字找会踩到名字互相包含
 * 的坑，而且中英文界面下判据还会不一样。
 */

const port = process.argv[2] || '9353';
const workDir = process.argv[3];
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
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
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
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

async function main() {
  const page = await waitTarget();
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', rej);
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  console.log('=== 1. 通过侧栏真实进入测试目录 ===');
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

  const intoSub = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('omy-progress-test'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('双击进入测试目录', intoSub === 'dispatched', String(intoSub));
  await sleep(1400);

  const rows = await c.eval(`${ROWS}.map(r => r.textContent.trim().slice(0, 40))`);
  check('界面上能看到测试文件', Array.isArray(rows) && rows.length > 0, JSON.stringify(rows));

  console.log('\n=== 2. 选中大文件并发起加密 ===');
  const sel = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('big-file'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'clicked';
  })()`);
  check('已选中大文件', sel === 'clicked', String(sel));
  await sleep(400);

  const encBtn = await c.eval(`(() => {
    const b = [...document.querySelectorAll('button')]
      .find(x => x.textContent.includes('加密') || x.textContent.includes('Encrypt'));
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'clicked';
  })()`);
  check('加密按钮已点击', encBtn === 'clicked', String(encBtn));
  await sleep(800);

  await c.eval(`(() => {
    for (const i of document.querySelectorAll('input[type=password]')) {
      i.value = 'progress-pw';
      i.dispatchEvent(new Event('input', { bubbles: true }));
    }
    return 'filled';
  })()`);
  await sleep(300);

  // 装一个采样器：在加密过程中持续记录进度条状态。
  // 必须在点提交之前装好，否则可能错过前面的百分比。
  await c.eval(`(() => {
    window.__progSamples = [];
    window.__progTimer = setInterval(() => {
      const el = document.querySelector('.prog');
      const fill = document.querySelector('.prog-fill');
      const pct = document.querySelector('.prog-pct');
      if (el) {
        window.__progSamples.push({
          width: fill ? fill.style.width : null,
          text: pct ? pct.textContent.trim() : null,
          name: (document.querySelector('.prog-name') || {}).textContent || null,
        });
      }
    }, 30);
    return 'installed';
  })()`);

  const submitted = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.dlg button, .dialog button, .mask button')]
      .find(x => (x.textContent.includes('加密') || x.textContent.includes('Encrypt')) && !x.disabled);
    if (!b) return 'no-submit';
    b.click();
    return 'submitted';
  })()`);
  check('已提交加密', submitted === 'submitted', String(submitted));

  // 等加密跑完（大文件 + Argon2）
  await sleep(12000);
  await c.eval(`(() => { clearInterval(window.__progTimer); return 'stopped'; })()`);

  console.log('\n=== 3. 进度条实际表现 ===');
  const samples = await c.eval(`window.__progSamples || []`);
  const n = Array.isArray(samples) ? samples.length : 0;
  check('加密过程中进度条确实出现过', n > 0, `采样到 ${n} 帧`);

  const widths = (samples || [])
    .map((s) => parseInt(String(s.width || '').replace('%', ''), 10))
    .filter((v) => Number.isFinite(v));
  const uniq = [...new Set(widths)];
  check('进度条宽度随加密推进而变化（不是卡在一个值）',
    uniq.length > 1, `出现过的宽度: ${JSON.stringify(uniq.slice(0, 12))}`);

  // 必须先要求有样本：空数组的 every 恒为 true，
  // 不加这个前提，「一帧都没采到」也会显示成 PASS
  const nonDecreasing =
    widths.length >= 2 && widths.every((v, i) => i === 0 || v >= widths[i - 1]);
  check('进度单调不减（不会倒退）', nonDecreasing,
    widths.length >= 2
      ? `首 ${widths[0]}% 末 ${widths[widths.length - 1]}%`
      : `样本不足（${widths.length} 帧），无法判定`);

  const named = (samples || []).some((s) => s.name && s.name.includes('big-file'));
  check('进度条显示了正在处理的文件名', named,
    JSON.stringify((samples || []).slice(0, 2).map((s) => s.name)));

  console.log('\n=== 4. 收尾 ===');
  const after = await c.eval(`(() => ({
    prog: !!document.querySelector('.prog'),
    stillBusy: !!document.querySelector('.prog-fill'),
  }))()`);
  check('加密结束后进度条已消失', after && after.prog === false, JSON.stringify(after));

  const listing = await c.eval(inv('browse_directory', { dir: workDir }));
  const madeOmy = (listing || []).some((e) => e.name.endsWith('.omy'));
  check('确实产出了加密文件（证明上面测的是真加密）', madeOmy,
    JSON.stringify((listing || []).map((e) => e.name)));

  const bad = results.filter((r) => !r.ok);
  console.log(`\n=== 合计 ${results.length - bad.length}/${results.length} 通过 ===`);
  if (bad.length) {
    console.log('失败项：');
    for (const b of bad) console.log('  - ' + b.name);
  }
  ws.close();
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => {
  console.error('探针异常:', e.message);
  process.exit(2);
});
