/** 远程位置 UI 目检探针：驱动真实界面并截图（移动 + 桌面）。
 *
 * 后端链路由 probe-place-cloud.mjs 断言；本探针只回答「界面长什么样、
 * 入口点不点得动」，截图交人眼核对，重点：
 *  - 移动底栏「远程」→ 位置列表 → 目录（真实名/锁态/只读徽标）→ 点播预览
 *  - 桌面侧栏云盘项能否真的切到云盘视图（曾只加载数据、不切视图）
 *  - 设置弹窗与缓存组（用量进度条）
 *
 * 用法：node --experimental-websocket probe-place-ui.mjs <cdpPort> <davUrl> <guiDir> <password> <shotsDir>
 */
import fs from 'node:fs';
import path from 'node:path';

const cdpPort = process.argv[2] || '9467';
const davUrl = process.argv[3] || 'http://127.0.0.1:8799/';
const guiDir = process.argv[4];
const password = process.argv[5] || 'cloud-test-password';
const shotsDir = process.argv[6] || path.join(process.cwd(), 'spikes', 'shots');
fs.mkdirSync(shotsDir, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const shots = [];

async function waitTarget() {
  const base = `http://127.0.0.1:${cdpPort}`;
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const list = await r.json();
        const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page) return page;
      }
    } catch { /* 未就绪 */ }
    await sleep(400);
  }
  throw new Error('CDP 端口未就绪');
}

class Cdp {
  constructor(ws) {
    this.ws = ws; this.id = 0; this.pending = new Map();
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) { this.pending.get(m.id)(m); this.pending.delete(m.id); }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) => (m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || 'eval 失败');
    return r.result.value;
  }
  async shot(name) {
    const { data } = await this.send('Page.captureScreenshot', { format: 'png' });
    const file = path.join(shotsDir, `${name}.png`);
    fs.writeFileSync(file, Buffer.from(data, 'base64'));
    shots.push(file);
    console.log('  shot', name);
  }
}

async function main() {
  const target = await waitTarget();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res, { once: true }); ws.addEventListener('error', rej, { once: true }); });
  const cdp = new Cdp(ws);
  await cdp.send('Runtime.enable');
  await cdp.send('Page.enable');
  await sleep(1000);

  // 前置：解锁本地库 + 添加云盘位置（与 cloud 探针相同夹具）
  await cdp.eval(`window.__p = { invoke: (c,a)=>window.__TAURI_INTERNALS__.invoke(c,a||{}) }; true`);
  await cdp.eval(`(async()=>{ await window.__p.invoke('browse_directory',{dir:${JSON.stringify(guiDir)}});
    await window.__p.invoke('unlock_directory',{dir:${JSON.stringify(guiDir)},password:${JSON.stringify(password)}});
    const list = await window.__p.invoke('remote_place_list');
    if (!Array.isArray(list) || !list.some(p=>p.name==='自测NAS')) {
      await window.__p.invoke('remote_place_add',{name:'自测NAS',url:${JSON.stringify(davUrl)},username:'',password:'',vendor:'generic',writable:false});
    }
    window.__rp = (await window.__p.invoke('remote_place_list')).find(p=>p.name==='自测NAS').id;
    return 1; })()`);

  const click = async (expr, wait = 700) => { const r = await cdp.eval(expr); await sleep(wait); return r; };

  // ============ 移动端 ============
  console.log('\n[移动端]');
  await cdp.send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 2, mobile: true });
  await sleep(1000);

  await click(`(()=>{ const b=document.querySelectorAll('.pnav .pnavi')[1]; if(!b) return 'no-tab'; b.click(); return 'ok'; })()`);
  await cdp.shot('m01-places');

  await click(`(()=>{ const b=document.querySelector('[data-pb-enter]'); if(!b) return 'no-enter'; b.click(); return 'ok'; })()`, 1800);
  await cdp.shot('m02-dir');

  await click(`(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('movie.mp4'));
    if(!r) return 'no-movie';
    r.click(); return 'ok';
  })()`, 3800);
  const mdiag = await cdp.eval(`(async()=>{
    const list=await window.__p.invoke('remote_browse',{placeId:window.__rp,dir:''});
    const m=list.find(x=>(x.real_name||x.name||'').includes('movie.mp4'));
    const r=await window.__p.invoke('remote_place_open',{placeId:window.__rp,path:m.id,size:Number(m.size)});
    const base=await window.__p.invoke('stream_base');
    const url=base+'/pfile/'+r.token;
    async function probe(label,opts){
      try{
        const res=await fetch(url,Object.assign({method:'GET'},opts));
        return {label,status:res.status,
          cr:res.headers.get('Content-Range'),cl:res.headers.get('Content-Length'),
          ar:res.headers.get('Accept-Ranges'),ct:res.headers.get('Content-Type'),
          ao:res.headers.get('Access-Control-Allow-Origin'),body:res.status<400?(await res.arrayBuffer()).byteLength:0};
      }catch(e){return {label,error:String(e)};}
    }
    const out=[];
    out.push(await probe('no-range',{}));
    out.push(await probe('open-ended',{headers:{Range:'bytes=0-'}}));
    out.push(await probe('first2',{headers:{Range:'bytes=0-1'}}));
    // 模拟 crossorigin media 触发的 CORS 预检
    try{
      const op=await fetch(url,{method:'OPTIONS',headers:{'Access-Control-Request-Method':'GET','Access-Control-Request-Headers':'range'}});
      out.push({label:'options',status:op.status,am:op.headers.get('Access-Control-Allow-Methods'),ah:op.headers.get('Access-Control-Allow-Headers'),ao:op.headers.get('Access-Control-Allow-Origin')});
    }catch(e){out.push({label:'options',error:String(e)});}
    return JSON.stringify({kind:r.kind,mime:r.mime,total:r.size,out});
  })()`);
  console.log('       [移动预览诊断]', mdiag);
  await cdp.shot('m03-preview');

  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(800);
  await cdp.shot('m04-back-to-dir');

  // 退回位置列表再退回本地
  await click(`(()=>{ const s=document.querySelector('.crumbpath .crumbseg'); if(s){s.click();return 'list';} return 'none'; })()`, 600);
  await click(`(()=>{ const b=document.querySelector('.crumb .crumbbtn'); if(b){b.click();return 'close';} return 'none'; })()`, 700);

  // 设置（底栏第 4 项）
  await click(`(()=>{ const b=document.querySelectorAll('.pnav .pnavi')[3]; if(!b) return 'no-settings'; b.click(); return 'ok'; })()`, 800);
  await cdp.shot('m05-settings');
  await click(`(()=>{ const n=document.querySelector('[data-sp="remote"]'); if(!n) return 'no-remote-nav'; n.click(); return 'ok'; })()`, 600);
  await cdp.shot('m06-remote');
  await click(`(()=>{ const b=document.querySelector('[data-sf="cache_entry"]'); if(!b) return 'no-cache-entry'; b.click(); return 'ok'; })()`, 700);
  await cdp.shot('m07-cache');
  await cdp.eval(`document.querySelector('[data-si="close"]')?.click(); 'closed'`);
  await sleep(500);

  // ============ 桌面端 ============
  console.log('\n[桌面端]');
  await cdp.send('Emulation.clearDeviceMetricsOverride');
  await sleep(1300);

  // 侧栏云盘位置项：这一步同时回归「桌面入口能否切到云盘视图」
  const entered = await click(`(()=>{
    const items=[...document.querySelectorAll('.side .sitem')];
    const it=items.find(x=>(x.getAttribute('title')||x.textContent||'').includes('自测NAS'));
    if(!it) return 'no-side-item';
    it.click(); return 'ok';
  })()`, 2000);
  console.log('       侧栏进入 ->', entered);
  await cdp.shot('d01-dir');

  // 桌面双击打开（桌面默认网格视图，条目是 .card；同时兼容列表 .lrow）
  await click(`(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('movie.mp4'));
    if(!r) return 'no-movie';
    r.dispatchEvent(new MouseEvent('dblclick',{bubbles:true})); return 'ok';
  })()`, 3800);
  await cdp.shot('d02-preview');

  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(700);
  await click(`(()=>{ const s=document.querySelector('.crumbpath .crumbseg'); if(s){s.click();return 'list';} return 'none'; })()`, 500);
  await click(`(()=>{ const b=document.querySelector('.crumb .crumbbtn'); if(b){b.click();return 'close';} return 'none'; })()`, 600);

  // PC 设置弹窗
  await click(`(()=>{ const b=document.querySelector('[data-tb="settings"]'); if(!b) return 'no-settings'; b.click(); return 'ok'; })()`, 800);
  await cdp.shot('d03-settings');
  await click(`(()=>{ const n=document.querySelector('[data-sp="remote"]'); if(!n) return 'no-remote-nav'; n.click(); return 'ok'; })()`, 600);
  await cdp.shot('d04-remote');
  await click(`(()=>{ const b=document.querySelector('[data-sf="cache_entry"]'); if(!b) return 'no-cache-entry'; b.click(); return 'ok'; })()`, 700);
  await cdp.shot('d05-cache');
  await cdp.eval(`document.querySelector('[data-si="close"]')?.click(); 'closed'`);

  console.log('\n截图目录:', shotsDir);
  shots.forEach((s) => console.log('  ', path.basename(s)));
  console.log('UI_SHOTS_OK');
  ws.close();
}

main().catch((e) => { console.error('截图探针异常:', e); process.exit(1); });
