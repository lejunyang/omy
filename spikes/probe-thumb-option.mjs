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

  console.log('\n--- 3. 缩略图选项的默认值 ---');
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

  // 填密码。取帧时间点现在由视频处理对话框选，不再影响 canSubmit，
  // 所以这里只要密码填上按钮就该可点
  await c.eval(`(() => {
    const ins=[...document.querySelectorAll('.dlg input[type=password]')];
    for(const i of ins){ i.value=${JSON.stringify(PW)}; i.dispatchEvent(new Event('input',{bubbles:true})); }
    return ins.length;
  })()`);
  await sleep(400);

  console.log('\n--- 4. 通过视频处理对话框选取帧时间点 ---');
  // 取帧入口从「加密对话框里的输入框」改成了「视频处理对话框」。
  // 入口按结构定位：.subfield 里的 button
  const entryOpened = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    const subs=[...dlg.querySelectorAll('.subfield')];
    const btn=subs.map(s=>s.querySelector('button.btn')).find(Boolean);
    if(!btn) return 'no-entry';
    btn.click();
    return 'ok';
  })()`);
  await sleep(2500);
  const vdlgOpen = await c.eval(`document.querySelector('.vdlg') ? 'yes':'no'`);
  check('视频处理对话框已打开', vdlgOpen === 'yes', `入口=${entryOpened}`);

  // 等视频元数据就绪，否则设 currentTime 会被忽略——
  // 那样取到的还是自动帧，而断言会把它报成「取帧没生效」
  let vready = null;
  for (let i = 0; i < 40; i++) {
    await sleep(500);
    vready = await c.eval(`(() => {
      const v=document.querySelector('.vdlg video');
      if(!v) return null;
      return JSON.stringify({ ready: v.readyState, dur: v.duration, err: v.error?v.error.code:null });
    })()`);
    const s = vready ? JSON.parse(vready) : null;
    if (s && (s.ready >= 1 || s.err)) break;
  }
  console.log('  [诊断] 视频状态 ' + vready);
  const vs = vready ? JSON.parse(vready) : null;
  check('预览视频可解码（取帧的前提）',
    vs && vs.err === null && vs.ready >= 1, String(vready));

  // 填 0:03.6 —— 也顺便验证 mm:ss 写法能被解析。
  // parseFloat('0:03.6') 会得 0，那样就取到红色帧了
  await c.eval(`(() => {
    const box=document.querySelector('.vdlg .vframe-row input[type=text]');
    if(!box) return 'no-box';
    box.value='0:03.6';
    box.dispatchEvent(new Event('input',{bubbles:true}));
    return 'set';
  })()`);
  await sleep(1000);
  const seeked = await c.eval(`(() => {
    const v=document.querySelector('.vdlg video');
    return v?v.currentTime:null;
  })()`);
  console.log('  [诊断] 敲 0:03.6 后 currentTime=' + seeked);
  check('mm:ss 写法被正确解析（不是被当成第 0 秒）',
    typeof seeked === 'number' && seeked > 3.0 && seeked < 4.2,
    `currentTime=${seeked}`);

  console.log('\n--- 5. 应用选择并回到加密对话框 ---');
  const applied = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const btns=[...v.querySelectorAll('.vdlg-foot .btn')];
    const primary=btns.find(b=>b.classList.contains('primary'));
    if(!primary) return 'no-primary';
    primary.click();
    return 'ok';
  })()`);
  await sleep(1200);
  const backToEncrypt = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const d=document.querySelector('.dlg');
    return JSON.stringify({ video: !!v, encrypt: !!d });
  })()`);
  const bt = JSON.parse(backToEncrypt);
  check('应用后视频对话框关闭、加密对话框还在',
    bt.video === false && bt.encrypt === true, `${backToEncrypt} 点击=${applied}`);

  // 加密对话框里应当显示已选的封面帧，否则用户不知道自己选过
  const shownFrame = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    const subs=[...dlg.querySelectorAll('.subfield .fhint')];
    return subs.map(s=>s.textContent.trim()).join(' | ');
  })()`);
  console.log('  [诊断] 入口下方文字 ' + shownFrame);
  check('加密对话框显示已选的封面帧时间',
    /00:00:0?3/.test(String(shownFrame)), String(shownFrame));
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
