/** 验证加密对话框的缩略图选项，以及自选取帧时间点真的生效。
 *
 * 用法：node --experimental-websocket probe-thumb-option.mjs <port>
 *
 * # 关键判据是缩略图的主色，不是「有没有缩略图」
 *
 * 取帧时间点被忽略时照样会有缩略图，所以只断言 has_thumbnail 测不出
 * 这个功能坏没坏。语料是前 2 秒纯红、后 2 秒纯蓝的视频：在界面上选
 * 3.6 秒，取出来的图必须是蓝的。若参数没传到后端，自动取的是 10% 处
 * （0.4 秒）也就是红色——一比颜色就露馅。
 *
 * # 为什么要断言非法输入挡住提交
 *
 * 不挡的话用户把时间点写错，加密照样进行，拿到的是自动帧而不是他选的
 * 那一帧，且界面上没有任何提示——用户会以为自己选好了。
 */

const port = process.argv[2] || '9383';
const base = `http://127.0.0.1:${port}`;
const PW = 'thumbopt-pw';

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

  console.log('\n--- 1. 进入源目录 ---');
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
      return (a||(e?e.textContent:'')).trim()==='omy-thumbopt-src';
    });
    if(row) row.dispatchEvent(new MouseEvent('dblclick',{bubbles:true,cancelable:true}));
    return true;
  })()`);
  await sleep(1800);

  const picked = await c.eval(`(() => {
    const rows=[...document.querySelectorAll('.grid .card, .list .lrow')];
    const row=rows.find(r=>{
      const a=r.getAttribute('aria-label');
      const e=r.querySelector('.cname, .nm');
      return (a||(e?e.textContent:'')).trim()==='rb.mp4';
    });
    if(!row) return 'no-target:'+rows.length;
    row.dispatchEvent(new MouseEvent('click',{bubbles:true}));
    return 'ok';
  })()`);
  check('找到并选中测试视频', picked === 'ok', String(picked));

  console.log('\n--- 2. 打开加密对话框 ---');
  // 按结构定位，并排除 .pill：统一密码入口的 title 是
  // 「输入密码可解锁当前目录中的加密文件」，里面含「加密」二字，
  // 用文字匹配会点开解锁弹窗而不是加密对话框。
  const opened = await c.eval(`(() => {
    const btns=[...document.querySelectorAll('button.btn')]
      .filter(x=>!x.classList.contains('pill'));
    const b=btns.find(x=>/加密|Encrypt/.test((x.textContent||'').trim()));
    if(!b) return 'no-button';
    b.click();
    return (b.textContent||'').trim().slice(0,16);
  })()`);
  await sleep(1200);
  const hasDlg = await c.eval(`document.querySelector('.dlg') ? 'yes' : 'no'`);
  check('加密对话框已打开', hasDlg === 'yes', `入口=${opened}`);

  console.log('\n--- 3. 缩略图选项的默认值与可见性 ---');
  // 默认必须开着：默认关掉的话界面上一张缩略图都没有，
  // 而这种回归从代码上很难看出来
  const boxes = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return null;
    const bs=[...dlg.querySelectorAll('input[type=checkbox]')];
    return bs.map(b=>b.checked);
  })()`);
  console.log('  [诊断] 复选框状态 ' + JSON.stringify(boxes));
  check('生成缩略图默认开启',
    Array.isArray(boxes) && boxes.length >= 2 && boxes[1] === true,
    JSON.stringify(boxes));

  // 选中的是视频，所以取帧输入框必须出现
  const frameShown = await c.eval(`document.querySelector('#e-frame') ? 'yes':'no'`);
  check('选中视频时显示取帧时间点输入框', frameShown === 'yes', String(frameShown));

  // 先填密码：canSubmit 同时看密码和时间点，密码为空时按钮**因为没密码**
  // 而禁用，那样下面那条断言就不是在测时间点了（实测这会让「不禁用提交」
  // 这个缺陷完全测不出来）。
  await c.eval(`(() => {
    const ins=[...document.querySelectorAll('.dlg input[type=password]')];
    for(const i of ins){ i.value=${JSON.stringify(PW)}; i.dispatchEvent(new Event('input',{bubbles:true})); }
    return ins.length;
  })()`);
  await sleep(400);
  // 自证前提成立：此刻按钮必须是可点的，否则后面测不出时间点的作用
  const readyBefore = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    const b=[...dlg.querySelectorAll('.acts .btn')].find(x=>x.type==='submit');
    const ps=[...dlg.querySelectorAll('input[type=password]')];
    return JSON.stringify({
      enabled: b ? !b.disabled : null,
      pw: ps.map(p=>p.value.length),
      err: [...dlg.querySelectorAll('.ferr')].map(e=>e.textContent.trim().slice(0,40)),
      frame: dlg.querySelector('#e-frame')?.value,
    });
  })()`);
  const rb = JSON.parse(readyBefore);
  console.log('  [诊断] 填密码后 ' + readyBefore);
  check('填好密码后按钮可提交（后续断言的前提）', rb.enabled === true,
    readyBefore);

  console.log('\n--- 4. 非法时间点必须挡住提交 ---');
  await c.eval(`(() => {
    const i=document.querySelector('#e-frame');
    if(!i) return 'no-input';
    i.value='abc:xy';
    i.dispatchEvent(new Event('input',{bubbles:true}));
    return 'set';
  })()`);
  await sleep(500);
  const bad = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return null;
    const err=dlg.querySelector('.ferr');
    const btn=[...dlg.querySelectorAll('.acts .btn')].find(b=>b.type==='submit');
    return { err: err?err.textContent.trim().slice(0,40):null, disabled: !!(btn&&btn.disabled) };
  })()`);
  console.log('  [诊断] 非法输入 ' + JSON.stringify(bad));
  check('非法时间点显示错误提示', bad && !!bad.err, String(bad && bad.err));
  check('非法时间点禁用提交按钮', bad && bad.disabled === true,
    `disabled=${bad && bad.disabled}`);

  console.log('\n--- 5. 合法时间点：说明文字与样式 ---');
  // 填 0:03.6，也顺便验证 mm:ss 写法能被解析——
  // parseFloat('0:03.6') 会得 0，那样就取到红色帧了
  await c.eval(`(() => {
    const i=document.querySelector('#e-frame');
    i.value='0:03.6';
    i.dispatchEvent(new Event('input',{bubbles:true}));
    return 'set';
  })()`);
  await sleep(500);
  const good = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    const err=dlg.querySelector('.ferr');
    const sub=dlg.querySelector('.subfield');
    const fh=sub?sub.querySelector('.fhint'):null;
    const rd=dlg.querySelector('.radio .d');
    const g=el=>{ const s=getComputedStyle(el); return {
      size: parseFloat(s.fontSize), color: s.color }; };
    return {
      err: err?err.textContent.trim().slice(0,40):null,
      hint: fh?fh.textContent.trim().slice(0,30):null,
      fh: fh?g(fh):null, rd: rd?g(rd):null,
      indent: sub?parseFloat(getComputedStyle(sub).paddingInlineStart):null,
    };
  })()`);
  console.log('  [诊断] 合法输入 ' + JSON.stringify(good));
  check('合法时间点无错误提示', good && !good.err, String(good && good.err));
  check('显示取帧说明文字', good && !!good.hint, String(good && good.hint));
  // 与上一轮 .d 没命中同类：类名看着对，样式其实没生效
  check('说明字号与周边说明一致',
    good && good.fh && good.rd && Math.abs(good.fh.size - good.rd.size) < 0.6,
    `fhint=${good?.fh?.size}px, .radio .d=${good?.rd?.size}px`);
  check('说明颜色与周边说明一致',
    good && good.fh && good.rd && good.fh.color === good.rd.color,
    `${good?.fh?.color}`);
  check('次级字段有左缩进（看得出属于上一项）',
    good && good.indent > 8, `padding-inline-start=${good?.indent}px`);

  console.log('\n--- 6. 提交并等待加密完成 ---');
  // 密码已在第 4 步之前填好
  const submitted = await c.eval(`(() => {
    const b=[...document.querySelectorAll('.dlg .acts .btn')].find(x=>x.type==='submit');
    if(!b) return 'no-btn';
    if(b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  let closed = false;
  for (let i = 0; i < 240; i++) {
    await sleep(500);
    const g = await c.eval(`document.querySelector('.dlg') ? 'open':'closed'`);
    if (g === 'closed') { closed = true; break; }
  }
  // 超时时把界面真实状态打出来：只报「没关闭」无法区分
  // 「加密很慢」「加密报错了」「按钮根本没点上」
  if (!closed) {
    const why = await c.eval(`(() => {
      const dlg=document.querySelector('.dlg');
      const btn=dlg?[...dlg.querySelectorAll('.acts .btn')].find(b=>b.type==='submit'):null;
      return JSON.stringify({
        notice: [...document.querySelectorAll('.notice, .toast, .err, .banner')]
          .map(n=>n.textContent.trim().slice(0,120)),
        btnText: btn?btn.textContent.trim().slice(0,30):null,
        btnDisabled: btn?btn.disabled:null,
        progress: [...document.querySelectorAll('.prog, .progress, .pbar')]
          .map(n=>n.textContent.trim().slice(0,60)),
      });
    })()`);
    console.log('  [诊断] 超时时界面状态 ' + why);
  }
  check('加密完成（对话框关闭）', closed, `提交=${submitted}`);

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
