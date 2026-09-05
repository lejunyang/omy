/** 验证密码管理弹窗里字段说明的样式与周围一致，且文案不写死操作数量。
 *
 * 用法：node --experimental-websocket probe-keydialog-style.mjs <port> <目录>
 *
 * # 为什么要断言「计算后的样式」而不是「类名对不对」
 *
 * 原缺陷正是类名看起来没问题：那段文字写的是 class="d"，而 CSS 里
 * 确实有 `.d` 的定义——但选择器是 `.radio .d`，那段文字不在 `.radio`
 * 里，于是一条规则都没命中，字号、颜色、间距全是浏览器默认值。
 * 只检查类名或只看 CSS 文件都发现不了，必须读 getComputedStyle。
 *
 * # 为什么文案不能写死数量
 *
 * 选项数量是变的：单文件 4 个（添加/修改/只保留/重新加密），密文树
 * 2 个（修改/重新加密）。原文案写「三种操作」，两种情况下都是错的。
 */

const port = process.argv[2] || '9381';
const WORK = process.argv[3];
const base = `http://127.0.0.1:${port}`;

async function waitTarget(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const p = (await r.json()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (p) return p;
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

(async () => {
  const t = await waitTarget();
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws 失败')));
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await sleep(2200);

  console.log('\n--- 1. 进入测试目录并解锁 ---');
  await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const d = items.find(i => i.textContent.includes('文档')
                           || i.textContent.includes('Documents'));
    if (d) d.click();
    return true;
  })()`);
  await sleep(1200);
  await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const row = rows.find(r => {
      const a = r.getAttribute('aria-label');
      const e = r.querySelector('.cname, .nm');
      return (a || (e ? e.textContent : '')).trim() === 'omy-keystyle-test';
    });
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return true;
  })()`);
  await sleep(1800);
  // 走界面的统一密码入口，让 store 自己累加 credentials；
  // 直接 invoke 的话 refreshKnown 会因 credentials==0 把索引清空
  await c.eval(`(() => { const b=document.querySelector('.pill'); if(b){b.click();return true;} return false; })()`);
  await sleep(900);
  await c.eval(`(() => {
    const ins=[...document.querySelectorAll('.dlg input[type=password]')];
    for(const i of ins){ i.value='keystyle-pw'; i.dispatchEvent(new Event('input',{bubbles:true})); }
    return ins.length;
  })()`);
  await sleep(400);
  await c.eval(`(() => { const b=document.querySelector('.dlg .acts .btn.primary'); if(b){b.click();return true;} return false; })()`);
  await sleep(2500);
  const st = await c.eval(`(() => ({
    cards: document.querySelectorAll('.grid .card').length,
    locked: document.querySelectorAll('.grid .card.locked').length,
  }))()`);
  check('解锁成功', st && st.cards > 0 && st.locked === 0, JSON.stringify(st));

  console.log('\n--- 2. 选中加密文件并打开密码管理 ---');
  const picked = await c.eval(`(() => {
    const cards=[...document.querySelectorAll('.grid .card')];
    const c0=cards.find(x=>{
      const n=x.getAttribute('aria-label')||'';
      return n.endsWith('.png');
    }) || cards[0];
    if(!c0) return 'no-card';
    c0.dispatchEvent(new MouseEvent('click',{bubbles:true}));
    return c0.getAttribute('aria-label');
  })()`);
  await sleep(700);
  // 按结构找密码管理入口：文案会随语言变
  const opened = await c.eval(`(() => {
    const btns=[...document.querySelectorAll('button')];
    const b=btns.find(x=>/密码管理|管理密码|Manage/.test(x.textContent||'')
                        || /密码管理|Manage/.test(x.getAttribute('title')||''));
    if(b){ b.click(); return (b.textContent||b.getAttribute('title')||'').trim().slice(0,20); }
    return 'no-button';
  })()`);
  await sleep(1200);
  check('密码管理弹窗已打开',
    opened !== 'no-button', `选中=${picked}，入口=${opened}`);

  console.log('\n--- 3. 操作选项数量与文案 ---');
  const opts = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return null;
    const radios=[...dlg.querySelectorAll('.radio')];
    return {
      count: radios.length,
      labels: radios.map(r=>{
        const sp=r.querySelector('span');
        return sp ? sp.textContent.trim().split(String.fromCharCode(10))[0].trim() : '?';
      }),
    };
  })()`);
  console.log('  [诊断] 选项 ' + JSON.stringify(opts));
  // 单文件应当有 4 个操作。这条同时锁住「文案不能说三种」
  check('单文件有 4 个操作选项', opts && opts.count === 4,
    JSON.stringify(opts && opts.count));

  const hintText = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return null;
    const el=dlg.querySelector('.fhint');
    return el ? el.textContent.trim() : 'no-fhint';
  })()`);
  console.log('  [诊断] 说明文案 ' + JSON.stringify(hintText));
  check('字段说明用了 .fhint 类', hintText && hintText !== 'no-fhint',
    String(hintText).slice(0, 60));
  // 文案不得写死数量：数量随单文件/密文树变化，写死必然在一种情况下是错的
  check('文案不写死操作数量',
    hintText && !/三种|三个|all three|3 (actions|operations)/i.test(hintText),
    String(hintText).slice(0, 80));

  console.log('\n--- 4. 样式与周围一致（计算后的值）---');
  const style = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return null;
    const fh=dlg.querySelector('.fhint');
    const rd=dlg.querySelector('.radio .d');
    const lb=dlg.querySelector('.flabel');
    if(!fh) return 'no-fhint';
    const g=el=>{ const s=getComputedStyle(el); return {
      size: parseFloat(s.fontSize),
      color: s.color,
      mt: parseFloat(s.marginTop),
    }; };
    return { fhint:g(fh), radioD: rd?g(rd):null, flabel: lb?g(lb):null };
  })()`);
  console.log('  [诊断] 样式 ' + JSON.stringify(style));
  // 原缺陷：一条规则都没命中，字号退回默认（约 13~16px），
  // 明显大于周围说明文字的 11.5px
  check('字号与选项说明一致（不是浏览器默认值）',
    style && style.fhint && style.radioD
      && Math.abs(style.fhint.size - style.radioD.size) < 0.6,
    `fhint=${style?.fhint?.size}px, .radio .d=${style?.radioD?.size}px`);
  check('颜色是次要文字色（与选项说明一致）',
    style && style.fhint && style.radioD
      && style.fhint.color === style.radioD.color,
    `fhint=${style?.fhint?.color}, .radio .d=${style?.radioD?.color}`);
  // 原缺陷的另一半：紧贴输入框、没有间距
  check('与输入框之间有间距',
    style && style.fhint && style.fhint.mt >= 4,
    `margin-top=${style?.fhint?.mt}px`);

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
