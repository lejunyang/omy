/** 验证右键菜单的「密码管理」「在文件管理器中显示」可用，
 * 以及播放分级角标带得出含义的悬停提示。
 *
 * 用法：node --experimental-websocket probe-ctx-fix.mjs <port>
 *
 * # 为什么要监听 console 里的 ReferenceError
 *
 * 这两项原来的缺陷不是「点了报错」，而是**点了完全没反应**：
 * keyManageable / revealEntry 由 store.js 导出但 App.vue 忘了 import，
 * 运行到那一行才抛 ReferenceError，而 onCtxPick 是 async 且调用处没有
 * catch，异常被吞掉。界面上没有任何迹象，构建也不报错。
 * 所以断言必须同时看「该发生的事发生了」和「控制台没有 ReferenceError」。
 */

const port = process.argv[2] || '9384';
const base = `http://127.0.0.1:${port}`;
const PW = 'ctxfix-pw';

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

const jsErrors = [];

class Cdp {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map();
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m); this.pending.delete(m.id);
      }
      // 未捕获异常与 console.error 都记下来
      if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params?.exceptionDetails;
        jsErrors.push(String(d?.exception?.description || d?.text || '?').split('\n')[0].slice(0, 160));
      }
      if (m.method === 'Runtime.consoleAPICalled' && m.params?.type === 'error') {
        jsErrors.push((m.params.args || []).map((a) => a.value || a.description || '?')
          .join(' ').split('\n')[0].slice(0, 160));
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 120000);
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
        .split('\n')[0].slice(0, 200);
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
    const items=[...document.querySelectorAll('.side .sitem')];
    const d=items.find(i=>i.textContent.includes('文档')||i.textContent.includes('Documents'));
    if(d) d.click();
    return true;
  })()`);
  await sleep(1200);
  await c.eval(`(() => {
    const rows=[...document.querySelectorAll('.grid .card, .list .lrow')];
    const row=rows.find(r=>{
      const a=r.getAttribute('aria-label');
      const e=r.querySelector('.cname, .nm');
      return (a||(e?e.textContent:'')).trim()==='omy-ctxfix-test';
    });
    if(row) row.dispatchEvent(new MouseEvent('dblclick',{bubbles:true,cancelable:true}));
    return true;
  })()`);
  await sleep(1800);
  // 走界面的统一密码入口：直接 invoke 会让 credentials 停在 0，
  // refreshKnown 把 known 清空，于是 meta 全没了
  await c.eval(`(() => { const b=document.querySelector('.pill'); if(b){b.click();return true;} return false; })()`);
  await sleep(900);
  await c.eval(`(() => {
    const ins=[...document.querySelectorAll('.dlg input[type=password]')];
    for(const i of ins){ i.value=${JSON.stringify(PW)}; i.dispatchEvent(new Event('input',{bubbles:true})); }
    return ins.length;
  })()`);
  await sleep(400);
  await c.eval(`(() => { const b=document.querySelector('.dlg .acts .btn.primary'); if(b){b.click();return true;} return false; })()`);
  await sleep(3000);
  const st = await c.eval(`(() => ({
    cards: document.querySelectorAll('.grid .card').length,
    locked: document.querySelectorAll('.grid .card.locked').length,
  }))()`);
  check('解锁成功', st && st.cards > 0 && st.locked === 0, JSON.stringify(st));

  console.log('\n--- 2. 分级角标带含义 ---');
  const badge = await c.eval(`(() => {
    const el=document.querySelector('.grid .card .tier');
    if(!el) return 'no-badge';
    return { icon: el.textContent.trim(), title: el.getAttribute('title')||'' };
  })()`);
  console.log('  [诊断] 角标 ' + JSON.stringify(badge));
  check('视频卡片上有分级角标', badge && badge !== 'no-badge', JSON.stringify(badge));
  // 光一个 🐌 用户看不懂，必须有文字说明
  check('角标有悬停说明', badge && badge.title && badge.title.length > 2,
    String(badge && badge.title).slice(0, 70));
  // 说明里要有「理由」，不只是分级代号——P1/P3 这种代号同样看不懂
  check('说明包含分级名称（不是裸代号）',
    badge && badge.title && !/^p[123]$/i.test(badge.title.trim()),
    String(badge && badge.title).slice(0, 70));
  // 必须带上后端算出的**理由**，不能只有前端写死的分级名。
  // 「可直接播放」告诉不了用户为什么，「容器与编码均被 WebView 原生支持」
  // 才有用；缺了它说明 tier_reason 没从后端传过来
  check('说明带后端给的理由（不只是分级名）',
    badge && badge.title && badge.title.includes('\u2014')
      && badge.title.split('\u2014')[1].trim().length > 4,
    String(badge && badge.title).slice(0, 70));

  console.log('\n--- 3. 右键菜单：在文件管理器中显示 ---');
  const errBefore = jsErrors.length;
  const revealed = await c.eval(`(() => {
    const card=document.querySelector('.grid .card');
    if(!card) return 'no-card';
    card.dispatchEvent(new MouseEvent('click',{bubbles:true}));
    card.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,clientX:200,clientY:200}));
    return 'opened';
  })()`);
  await sleep(700);
  const menuItems = await c.eval(`(() => {
    const m=document.querySelector('.ctxmenu, .ctx');
    if(!m) return 'no-menu';
    return [...m.querySelectorAll('.citem, .item, button')]
      .map(x=>x.textContent.trim().slice(0,20)).filter(Boolean);
  })()`);
  console.log('  [诊断] 菜单项 ' + JSON.stringify(menuItems));
  check('右键菜单已打开', Array.isArray(menuItems), String(revealed));

  // 点「在文件管理器中显示」。它会真的起 explorer，
  // 判据是「没有抛 ReferenceError」+「菜单关闭」
  const clickedReveal = await c.eval(`(() => {
    const m=document.querySelector('.ctxmenu, .ctx');
    if(!m) return 'no-menu';
    const it=[...m.querySelectorAll('.citem, .item, button')]
      .find(x=>/文件管理器|资源管理器|Reveal|Show in/i.test(x.textContent||''));
    if(!it) return 'no-item';
    it.click();
    return 'clicked';
  })()`);
  await sleep(1500);
  const revealErrs = jsErrors.slice(errBefore).filter(e=>/ReferenceError|is not defined/i.test(e));
  check('点「在文件管理器中显示」不抛 ReferenceError',
    clickedReveal === 'clicked' && revealErrs.length === 0,
    `点击=${clickedReveal}, 错误=${JSON.stringify(revealErrs)}`);

  console.log('\n--- 4. 右键菜单：密码管理 ---');
  const errBefore2 = jsErrors.length;
  // 必须确保卡片**处于**选中态，而不是「点一下」：上一步测 reveal 时已经
  // 点过一次，再点会把它取消选择（toggleSelect 对唯一选中项是反选）。
  // 那样「密码管理」会正确地 disabled，探针却会误判成产品缺陷。
  const selState = await c.eval(`(() => {
    const card=document.querySelector('.grid .card');
    if(!card) return 'no-card';
    if(!card.classList.contains('sel')) {
      card.dispatchEvent(new MouseEvent('click',{bubbles:true}));
    }
    return card.className;
  })()`);
  await sleep(500);
  // 自证前提：没选中的话下面测的就不是「密码管理能不能打开」
  const isSel = await c.eval(`(() => {
    const card=document.querySelector('.grid .card');
    return card ? card.classList.contains('sel') : null;
  })()`);
  check('卡片处于选中态（密码管理的前提）', isSel === true,
    `class=${selState}`);
  await c.eval(`(() => {
    const card=document.querySelector('.grid .card');
    card.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,clientX:200,clientY:200}));
    return 1;
  })()`);
  await sleep(700);
  const clickedKey = await c.eval(`(() => {
    const m=document.querySelector('.ctxmenu, .ctx');
    if(!m) return 'no-menu';
    const it=[...m.querySelectorAll('.citem, .item, button')]
      .find(x=>/密码管理|管理密码|Manage/i.test(x.textContent||''));
    if(!it) return 'no-item';
    // 菜单项自己可能是 disabled——那样点了本来就不该有反应，
    // 属于条件不满足而不是缺陷。必须区分这两种情况
    const dis = it.disabled || it.classList.contains('dis')
      || it.classList.contains('disabled')
      || it.getAttribute('aria-disabled')==='true';
    if(dis) return 'disabled:'+(it.getAttribute('title')||'').slice(0,60);
    it.click();
    return 'clicked';
  })()`);
  await sleep(1200);
  const keyErrs = jsErrors.slice(errBefore2).filter(e=>/ReferenceError|is not defined/i.test(e));
  check('点「密码管理」不抛 ReferenceError',
    clickedKey === 'clicked' && keyErrs.length === 0,
    `点击=${clickedKey}, 错误=${JSON.stringify(keyErrs)}`);

  // 真正的判据：弹窗开出来了，且是密码管理那个
  const dlg = await c.eval(`(() => {
    const d=document.querySelector('.dlg.keymgmt');
    if(!d) {
      // 报清楚为什么没开：keyManageable 要求「已解锁的单个加密文件被选中」，
      // 三个条件缺一个都不行，只说「没开」无法定位
      const cards=[...document.querySelectorAll('.grid .card')];
      return {
        open:false,
        any: !!document.querySelector('.dlg'),
        anyDlgClass: document.querySelector('.dlg')?.className || null,
        selectedCards: cards.filter(x=>x.classList.contains('sel')
          || x.classList.contains('selected')
          || x.getAttribute('aria-selected')==='true').length,
        cardClasses: cards.map(x=>x.className),
        menuOpen: !!document.querySelector('.ctxmenu, .ctx'),
      };
    }
    return { open:true, radios: d.querySelectorAll('.radio').length };
  })()`);
  console.log('  [诊断] 密码管理弹窗 ' + JSON.stringify(dlg));
  check('密码管理弹窗真的打开了', dlg && dlg.open === true, JSON.stringify(dlg));
  check('弹窗里有操作选项', dlg && dlg.radios >= 2, `radios=${dlg && dlg.radios}`);

  console.log('\n--- 5. 全程无未捕获异常 ---');
  const refErrs = jsErrors.filter(e=>/ReferenceError|is not defined/i.test(e));
  console.log('  [诊断] 收集到 ' + jsErrors.length + ' 条 console 错误');
  if (jsErrors.length) console.log('    ' + JSON.stringify(jsErrors.slice(0,5)));
  check('全程没有 ReferenceError', refErrs.length === 0, JSON.stringify(refErrs));

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
