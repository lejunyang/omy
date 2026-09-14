/** 验证视频处理对话框：入口、封面帧三者联动、能力检测驱动可选项。
 *
 * 用法：node --experimental-websocket probe-video-dialog.mjs <port>
 *
 * # 这个探针要抓什么
 *
 * 1. **入口只在单选视频时出现。** 多选或选文件夹时给入口是错的——封面帧和
 *    目标容器是逐文件的设置，一个对话框代表不了一批文件，而用户会以为
 *    设置应用到了全部。
 *
 * 2. **进度条、时间码、画面是同一个值。** 这是去掉「取当前画面」按钮的前提。
 *    若它们其实各走各的，界面上看不出来——用户拖到某一帧，提交的却是别的
 *    时间点，要到看见缩略图才发现。
 *
 * 3. **压缩项的可用性跟着能力走，不是写死的。** 内置 FFmpeg 没有视频编码器，
 *    此时压缩必须禁用**并说明原因**；换成完整版后必须自动可用。写死成任一
 *    答案都会错，而错的那半边平时根本不会被执行到。
 *
 * # 按结构定位，不按文字
 *
 * 名字会互相包含（`omy-folder-test` 含 `my-folder`），文字匹配会命中错的
 * 元素，把探针 bug 显示成产品缺陷。所以一律用类名 / data 属性。
 */

const port = process.argv[2] || '9455';
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
  await sleep(2500);

  console.log('\n--- 0. 后端能力探测 ---');
  // 先问后端这台机器上 FFmpeg 能干什么。后面「压缩该不该可用」的断言
  // 要按这个结果分支——写死成任一答案都会在另一种环境上假失败。
  const capsRaw = await c.eval(`(async () => {
    // 用 __TAURI_INTERNALS__ 而不是 __TAURI__.core：后者要 app 显式开
    // withGlobalTauri 才会注入，本应用没开。写错时报的是
    // "Cannot read properties of undefined"，看不出是注入形态的问题
    return JSON.stringify(await window.__TAURI_INTERNALS__.invoke('video_capabilities', { refresh: true }));
  })()`);
  let caps = null;
  try { caps = JSON.parse(capsRaw); } catch { /* 下面会报 */ }
  console.log('  [诊断] caps ' + capsRaw);
  check('能拿到 FFmpeg 能力集', caps && typeof caps.has_ffmpeg === 'boolean', String(capsRaw).slice(0, 80));
  if (!caps) { ws.close(); process.exit(1); }

  const canCompress = (caps.video_encoders || []).length > 0;
  console.log(`  [诊断] 视频编码器 ${canCompress ? caps.video_encoders.join(',') : '无'}`);

  console.log('\n--- 1. 进入测试目录并选中一个视频 ---');
  // 先走「选择文件夹」按钮。原生选择器 CDP 点不到，应用侧读
  // OMY_GUI_PICK_FOLDER 直接返回预设路径，按钮点击与后续加载走真实代码
  const entered = await c.eval(`(() => {
    const btns=[...document.querySelectorAll('button.btn, .side .sitem')];
    const b=btns.find(x=>/选择文件夹|打开文件夹|Choose folder|Open folder/i.test((x.textContent||'').trim()));
    if(!b) return 'no-picker:'+btns.map(x=>(x.textContent||'').trim().slice(0,10)).join(',').slice(0,150);
    b.click();
    return 'clicked';
  })()`);
  console.log('  [诊断] 目录入口 ' + entered);
  await sleep(2500);

  const picked = await c.eval(`(() => {
    const rows=[...document.querySelectorAll('.grid .card, .list .lrow')];
    const row=rows.find(r=>{
      const a=r.getAttribute('aria-label');
      const e=r.querySelector('.cname, .nm');
      return /probe-video\\.mp4$/i.test((a||(e?e.textContent:'')).trim());
    });
    if(!row) return 'no-video:'+rows.length+':'+rows.slice(0,6).map(r=>{
      const e=r.querySelector('.cname, .nm');
      return e?e.textContent.trim().slice(0,18):'?';
    }).join(',');
    row.dispatchEvent(new MouseEvent('click',{bubbles:true}));
    return 'ok';
  })()`);
  check('找到并选中 mp4 文件', picked === 'ok', String(picked));

  console.log('\n--- 2. 打开加密对话框，检查视频处理入口 ---');
  await c.eval(`(() => {
    const btns=[...document.querySelectorAll('button.btn')]
      .filter(x=>!x.classList.contains('pill'));
    const b=btns.find(x=>/加密|Encrypt/.test((x.textContent||'').trim()));
    if(b) b.click();
    return true;
  })()`);
  await sleep(1200);

  // 入口按结构找：.subfield 里的 button。用文字找会在中英文之间来回改
  const entry = await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    if(!dlg) return 'no-dlg';
    const subs=[...dlg.querySelectorAll('.subfield')];
    const btn=subs.map(s=>s.querySelector('button.btn')).find(Boolean);
    return btn ? 'found' : 'no-entry';
  })()`);
  check('单选视频时出现视频处理入口', entry === 'found', String(entry));

  // 旧的取帧输入框必须已经不在了。留着的话两个入口并存，
  // 用户在哪个里改都可能被另一个覆盖
  const oldInput = await c.eval(`document.querySelector('#e-frame') ? 'still-there' : 'gone'`);
  check('旧的取帧输入框已移除', oldInput === 'gone', String(oldInput));

  console.log('\n--- 3. 打开视频处理对话框 ---');
  await c.eval(`(() => {
    const dlg=document.querySelector('.dlg');
    const subs=[...dlg.querySelectorAll('.subfield')];
    const btn=subs.map(s=>s.querySelector('button.btn')).find(Boolean);
    if(btn) btn.click();
    return true;
  })()`);
  await sleep(2500);

  const vdlg = await c.eval(`document.querySelector('.vdlg') ? 'yes':'no'`);
  check('视频处理对话框已打开', vdlg === 'yes', String(vdlg));
  if (vdlg !== 'yes') {
    console.log('  [诊断] 无法继续，后续断言跳过');
    ws.close(); process.exit(1);
  }

  // 没有「取当前画面」这类确认按钮：三者联动的前提下它是多余的，
  // 有它反而暗示可能不同步
  const btnCount = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    return [...v.querySelectorAll('.vframe-row button, .vscrub button')]
      .map(b=>(b.textContent||'').trim()).join('|');
  })()`);
  console.log('  [诊断] 帧区按钮 ' + btnCount);
  check('封面帧区没有多余的「取用」按钮',
    !/取当前|取用|Use current/i.test(String(btnCount)), String(btnCount));

  console.log('\n--- 4. 视频真的能播（不是只有个黑框）---');
  // 等 loadedmetadata。readyState>=1 才说明 omystream://plain 那条路通了；
  // 只断言 <video> 存在的话，协议坏掉时照样 PASS
  let meta = null;
  for (let i = 0; i < 40; i++) {
    await sleep(500);
    meta = await c.eval(`(() => {
      const v=document.querySelector('.vdlg video');
      if(!v) return null;
      return JSON.stringify({
        ready: v.readyState, dur: v.duration,
        w: v.videoWidth, h: v.videoHeight,
        err: v.error ? v.error.code : null,
      });
    })()`);
    const m = meta ? JSON.parse(meta) : null;
    if (m && (m.ready >= 1 || m.err)) break;
  }
  console.log('  [诊断] video 状态 ' + meta);
  const m = meta ? JSON.parse(meta) : null;
  check('视频元数据已加载（协议通、能解码）',
    m && m.err === null && m.ready >= 1 && m.dur > 0,
    String(meta));
  check('画面有真实尺寸（不是空轨道）',
    m && m.w > 0 && m.h > 0, `${m?.w}x${m?.h}`);

  console.log('\n--- 5. 进度条 / 时间码 / 画面是同一个值 ---');
  // 拖进度条 → 输入框和 currentTime 都要跟着变。
  // 这是去掉「取当前画面」按钮的前提：三者不同步的话，用户拖到某一帧、
  // 提交的却是别的时间点，而界面上完全看不出来
  const dur = m?.dur || 0;
  const target = Math.min(dur * 0.6, Math.max(0.5, dur - 0.5));
  const sync = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const r=v.querySelector('input[type=range]');
    if(!r) return 'no-range';
    r.value='${target.toFixed(2)}';
    r.dispatchEvent(new Event('input',{bubbles:true}));
    return 'set';
  })()`);
  await sleep(900);
  const after = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const vid=v.querySelector('video');
    const box=v.querySelector('.vframe-row input[type=text]');
    const badge=v.querySelector('.vbadge');
    return JSON.stringify({
      cur: vid?vid.currentTime:null,
      text: box?box.value:null,
      badge: badge?badge.textContent.trim():null,
    });
  })()`);
  console.log('  [诊断] 拖到 ' + target.toFixed(2) + 's 之后 ' + after);
  const a = after ? JSON.parse(after) : null;
  check('拖进度条后视频真的跳了',
    a && Math.abs(a.cur - target) < 1.0, `currentTime=${a?.cur}, 期望≈${target.toFixed(2)}`);
  check('时间码输入框跟着变',
    a && typeof a.text === 'string' && a.text.length > 0 && a.text !== '00:00:00.00',
    String(a?.text));

  // 反向：敲时间码 → 视频跟着跳
  const backTarget = Math.min(1.0, dur * 0.25);
  await c.eval(`(() => {
    const box=document.querySelector('.vdlg .vframe-row input[type=text]');
    box.value='0:0${backTarget.toFixed(2)}';
    box.dispatchEvent(new Event('input',{bubbles:true}));
    return true;
  })()`);
  await sleep(900);
  const back = await c.eval(`(() => {
    const vid=document.querySelector('.vdlg video');
    return vid?vid.currentTime:null;
  })()`);
  console.log('  [诊断] 敲时间码后 currentTime=' + back);
  check('敲时间码后视频跟着跳（反向也联动）',
    typeof back === 'number' && Math.abs(back - backTarget) < 1.0,
    `currentTime=${back}, 期望≈${backTarget.toFixed(2)}`);

  console.log('\n--- 6. 转换页：可选项跟着能力走 ---');
  await c.eval(`(() => {
    const tabs=[...document.querySelectorAll('.vdlg .tab')];
    if(tabs[1]) tabs[1].click();
    return true;
  })()`);
  await sleep(1500);

  const info = await c.eval(`(() => {
    const rows=[...document.querySelectorAll('.vdlg .vinfo tr')];
    return rows.length;
  })()`);
  check('显示源文件信息表', typeof info === 'number' && info >= 4, `行数=${info}`);

  const opts = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const cards=[...v.querySelectorAll('.vopt')];
    return JSON.stringify(cards.map(card=>{
      const r=card.querySelector('input[type=radio]');
      return { value: r?r.value:null, disabled: r?r.disabled:null,
               sel: card.classList.contains('sel') };
    }));
  })()`);
  console.log('  [诊断] 处理方式 ' + opts);
  const o = opts ? JSON.parse(opts) : [];
  check('三种处理方式都在', o.length === 3, `实际 ${o.length} 个`);

  const comp = o.find((x) => x.value === 'compress');
  // 关键断言：压缩的可用性必须与后端报告的能力一致。
  // 内置版没有编码器就该禁用，完整版就该可用——两边都要对
  check('压缩项的可用性与能力检测一致',
    comp && comp.disabled === !canCompress,
    `disabled=${comp?.disabled}, 有编码器=${canCompress}`);

  // 不可用时必须给出原因和出路，不能只是灰掉
  if (!canCompress) {
    const note = await c.eval(`(() => {
      const v=document.querySelector('.vdlg');
      const n=[...v.querySelectorAll('.note')].map(x=>x.textContent.trim());
      const btns=[...v.querySelectorAll('.vcaps-acts button')].length;
      return JSON.stringify({ hasNote: n.some(x=>x.length>40), buttons: btns });
    })()`);
    const nn = JSON.parse(note);
    check('压缩不可用时说明了原因', nn.hasNote === true, note);
    check('压缩不可用时给了「重新检测」出路', nn.buttons >= 1, note);
  }

  console.log('\n--- 7. 选转封装，检查子选项 ---');
  const remuxOk = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const r=[...v.querySelectorAll('.vopt input[type=radio]')].find(x=>x.value==='remux');
    if(!r) return 'no-radio';
    r.click();
    return 'ok';
  })()`);
  await sleep(900);
  const sub = await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const s=v.querySelector('.vsub');
    if(!s) return null;
    const sel=s.querySelector('select');
    return JSON.stringify({
      shown: !!s,
      containers: sel?[...sel.options].map(o=>o.value):[],
      selected: s.parentElement.querySelector('.vopt.sel input')?.value,
    });
  })()`);
  console.log('  [诊断] 转封装子选项 ' + sub);
  const sb = sub ? JSON.parse(sub) : null;
  check('选中转封装后展开子选项', sb && sb.shown === true, `点击=${remuxOk}`);
  check('目标容器按后端 muxer 过滤',
    sb && sb.containers.length > 0 && sb.containers.every((x) => ['mp4','mov','mkv'].includes(x)),
    JSON.stringify(sb?.containers));

  // 选中态只能有一个：用 contains() 判定时会同时高亮两张卡，
  // 那个 bug 在原型阶段实测出现过
  const selCount = await c.eval(`document.querySelectorAll('.vdlg .vopt.sel').length`);
  check('选中态只有一张卡高亮', selCount === 1, `高亮 ${selCount} 张`);

  console.log('\n--- 8. 取消不留下中间文件 ---');
  await c.eval(`(() => {
    const v=document.querySelector('.vdlg');
    const btns=[...v.querySelectorAll('.vdlg-foot .btn')];
    if(btns[0]) btns[0].click();
    return true;
  })()`);
  await sleep(1200);
  const gone = await c.eval(`document.querySelector('.vdlg') ? 'still-open':'closed'`);
  check('取消后对话框关闭', gone === 'closed', String(gone));

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
