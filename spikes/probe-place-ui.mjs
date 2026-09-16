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
import crypto from 'node:crypto';

const cdpPort = process.argv[2] || '9467';
const davUrl = process.argv[3] || 'http://127.0.0.1:8799/';
const guiDir = process.argv[4];
const password = process.argv[5] || 'cloud-test-password';
const shotsDir = process.argv[6] || path.join(process.cwd(), 'spikes', 'shots');
fs.mkdirSync(shotsDir, { recursive: true });

// 源明文（guiDir 是 .../vault，ffmpeg 造的 movie.mp4 在其父目录）：跨块 seek 与
// 解密到本地都用它逐字节核对 app 输出的明文是否正确。
const workDir = path.dirname(guiDir);
const srcMoviePath = path.join(workDir, 'movie.mp4');
const srcBuf = fs.readFileSync(srcMoviePath);
const srcSha = crypto.createHash('sha256').update(srcBuf).digest('hex');
const srcSize = srcBuf.length;

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
    // 先清掉别的测试或用户留下的位置。
    //
    // 远程位置现在会持久化，机器上很可能还留着上一轮测试的条目（指向
    // 已经停掉的服务器）。不清的话截图里会混进无关位置，更糟的是
    // 「进入第一个位置」会进错地方，卡在「正在读取」——看起来像产品
    // 缺陷，实际只是进了个没人应答的位置。
    const old = await window.__p.invoke('remote_place_list');
    for (const p of (Array.isArray(old) ? old : [])) {
      if (p.name !== '自测NAS') {
        await window.__p.invoke('remote_place_remove', { id: p.id });
      }
    }
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

  // 这两步的返回值必须检查。原先直接丢弃，于是点击没生效时也照样往下跑，
  // 最后失败在「找不到骨架」那一步——而真正的原因是上一步压根没进目录，
  // 两者指向的修法完全不同。
  const tabR = await click(`(()=>{ const b=document.querySelectorAll('.pnav .pnavi')[1]; if(!b) return 'no-tab'; b.click(); return 'ok'; })()`);
  if (tabR !== 'ok') throw new Error(`移动端切到远程标签失败: ${tabR}`);
  await cdp.shot('m01-places');

  const enterR = await click(`(()=>{
    // 必须按 id 精确定位，不能取「第一个」位置。
    //
    // 远程位置现在会持久化，机器上可能留着别的测试或用户自己加的位置，
    // 取第一个会进错地方——表现为列表空白、卡在「正在读取」，
    // 看起来像产品缺陷，实际是进了一个指向已停服务器的位置。
    const b=document.querySelector('[data-pb-enter="'+window.__rp+'"]');
    if(!b) return 'no-enter:'+window.__rp;
    b.click(); return 'ok';
  })()`, 450);
  if (enterR !== 'ok') throw new Error(`移动端进入云盘目录失败: ${enterR}`);
  await cdp.shot('m02-dir');

  // —— 边扫边出：slow.omy 服务端延迟 2.2s，进目录后它必须先停在「识别中」骨架 ——
  const probing = await cdp.eval(`(async()=>{
    const sleep=ms=>new Promise(r=>setTimeout(r,ms));
    for(let i=0;i<20;i++){
      const cards=[...document.querySelectorAll('.content .card.probing, .content .lrow.probing')];
      const slow=cards.find(x=>(x.querySelector('.cname,.nm')?.textContent||'').trim()==='slow.omy');
      if(slow) return JSON.stringify({probing:true,meta:(slow.querySelector('.cmeta,.tg')?.textContent||'').trim()});
      const settled=[...document.querySelectorAll('.content .card, .content .lrow')]
        .find(x=>(x.querySelector('.cname,.nm')?.textContent||'').trim()==='slow.omy');
      if(settled && i>3) return JSON.stringify({probing:false,reason:'slow-already-final'});
      await sleep(100);
    }
    return JSON.stringify({probing:false,reason:'timeout'});
  })()`);
  console.log('       [边扫边出] 骨架态', probing);
  const pb = JSON.parse(probing);
  // 不这样会怎样：慢文件会把整屏列表堵到全部识别完才显示，失去边扫边出的意义
  if (pb.probing !== true || !pb.meta) {
    // 失败时把真实 DOM 打出来。只报「没找到」无法区分三种情况：
    // 选择器写错了、条目压根没渲染、还是识别确实太快——
    // 而这三种的修法完全不同。
    const dump = await cdp.eval(`(() => {
      const els = [...document.querySelectorAll('.card, .lrow')];
      return JSON.stringify({
        总数: els.length,
        视口宽: window.innerWidth,
        条目: els.slice(0, 10).map(e => ({
          cls: e.className,
          name: (e.querySelector('.cname,.nm')?.textContent || '').trim(),
          meta: (e.querySelector('.cmeta,.tg')?.textContent || '').trim(),
        })),
      });
    })()`);
    console.log('       [诊断] 当前 DOM:', dump);
    throw new Error('未出现 slow.omy 的识别中骨架: ' + probing);
  }
  await cdp.shot('m02a-probing');

  // —— 失败条目「点击重试」——
  // retry.omy 带 .failmarker，dav 自测服务器把它的读请求改写成 404，首次浏览应是
  // 「未能读取」；删掉故障标记后点击该卡，应就地重试成功，密文名变成唯一明文名。
  // 边扫边出后 remote_browse 立即返回的是「识别中」骨架，拿不到最终三态；
  // 这里用单条目探测命令（不经过事件流、不改界面）取权威结果。
  const retryBefore = await cdp.eval(`(async()=>{
    const skel=await window.__p.invoke('remote_browse',{placeId:window.__rp,dir:''});
    const s=skel.find(x=>/retry\\.omy$/.test(x.id));
    if(!s) return 'missing';
    const r=await window.__p.invoke('remote_probe_entry',{placeId:window.__rp,id:s.id,size:Number(s.size)});
    return JSON.stringify({id:r.id,probe_failed:r.probe_failed,unlocked:r.unlocked,is_encrypted:r.is_encrypted});
  })()`);
  console.log('       [重试] 首次探测', retryBefore);
  const rb = JSON.parse(retryBefore);
  // 不这样会怎样：网络失败被混进「密码不对」，用户会去反复试密码而不是检查网络
  if (rb.probe_failed !== true || rb.unlocked !== false || rb.is_encrypted !== false) {
    throw new Error('失败条目初始态错误（应是未能读取，而非锁定/非加密）: ' + retryBefore);
  }
  // 截图前先在真实界面上等到失败卡出现「点击重试」，避免截到还在识别中的骨架
  const failedShown = await cdp.eval(`(async()=>{
    const sleep=ms=>new Promise(r=>setTimeout(r,ms));
    for(let i=0;i<40;i++){
      const card=[...document.querySelectorAll('.content .lrow, .content .card')]
        .find(x=>(x.querySelector('.nm,.cname')?.textContent||'').trim()==='retry.omy');
      if(card && card.querySelector('.retryhint')) return 'shown';
      await sleep(150);
    }
    return 'timeout';
  })()`);
  if (failedShown !== 'shown') throw new Error('失败卡未出现点击重试提示: ' + failedShown);
  await cdp.shot('m02b-failed');
  // 删掉故障注入标记（davDir 是第 7 个参数），再在真实界面上点该失败卡触发重试
  fs.unlinkSync(path.join(process.argv[7] || '', 'retry.omy.failmarker'));
  const retryClick = await click(`(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').trim()==='retry.omy');
    if(!r) return 'no-retry-row';
    r.click(); return 'clicked';
  })()`, 400);
  if (retryClick !== 'clicked') throw new Error('没点到失败条目: ' + retryClick);
  const retryOk = await cdp.eval(`(async()=>{
    const sleep=ms=>new Promise(r=>setTimeout(r,ms));
    const rows=()=>[...document.querySelectorAll('.content .lrow, .content .card')]
      .map(x=>(x.querySelector('.nm,.cname')?.textContent||'').trim());
    for(let i=0;i<40;i++){
      const ts=rows();
      if(ts.includes('retry-src.mp4') && !ts.includes('retry.omy')) return 'retried';
      await sleep(200);
    }
    return 'timeout:'+JSON.stringify(rows());
  })()`);
  console.log('       [重试] 点击后', retryOk);
  if (retryOk !== 'retried') throw new Error('失败条目点击重试未恢复: ' + retryOk);
  await cdp.shot('m02c-retried');

  // slow.omy 在约 2.2s 后应收敛成最终条目 slow-src.mp4，且全列表不再有识别中骨架
  const slowDone = await cdp.eval(`(async()=>{
    const sleep=ms=>new Promise(r=>setTimeout(r,ms));
    for(let i=0;i<40;i++){
      const names=[...document.querySelectorAll('.content .lrow, .content .card')]
        .map(x=>(x.querySelector('.nm,.cname')?.textContent||'').trim());
      const probing=document.querySelectorAll('.content .card.probing, .content .lrow.probing').length;
      if(names.includes('slow-src.mp4') && !names.includes('slow.omy') && probing===0) return 'settled';
      await sleep(150);
    }
    return 'timeout';
  })()`);
  console.log('       [边扫边出] 收敛', slowDone);
  if (slowDone !== 'settled') throw new Error('识别中骨架未收敛为最终条目: ' + slowDone);

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
    // 跨 1 MiB 块边界的中部 seek：块缓存按 1 MiB 对齐，区间故意从边界前 128 KiB
    // 起、跨到边界后 128 KiB，最容易暴露「块拼接错位 / 偏移算错」。返回这段明文
    // 的 SHA-256，node 侧用源明文同区间逐字节核对（夹具视频 >2 MiB 才执行）。
    let span=null;
    const MI_B=1024*1024;
    if(r.size > MI_B+262144){
      const start=MI_B-131072, len=262144, end=start+len-1;
      const sr=await fetch(url,{headers:{Range:'bytes='+start+'-'+end}});
      const ab=await sr.arrayBuffer();
      const digest=await crypto.subtle.digest('SHA-256',ab);
      const sha=[...new Uint8Array(digest)].map(b=>b.toString(16).padStart(2,'0')).join('');
      span={status:sr.status,cr:sr.headers.get('Content-Range'),
        cl:sr.headers.get('Content-Length'),bytes:ab.byteLength,start,end,sha256:sha};
    }
    // 模拟 crossorigin media 触发的 CORS 预检
    try{
      const op=await fetch(url,{method:'OPTIONS',headers:{'Access-Control-Request-Method':'GET','Access-Control-Request-Headers':'range'}});
      out.push({label:'options',status:op.status,am:op.headers.get('Access-Control-Allow-Methods'),ah:op.headers.get('Access-Control-Allow-Headers'),ao:op.headers.get('Access-Control-Allow-Origin')});
    }catch(e){out.push({label:'options',error:String(e)});}
    // 远程缩略图：边扫边出后 browse 先回骨架（无 token），对带缩略图的条目
    // 用单条目探测取权威结果，应直接给 pthumb token，凭它取文件头里的缩略图，
    // 不再依赖整屏 browse 是否已经探测到它。
    let thumbDiag=null;
    const ts=list.find(x=>(x.real_name||x.name||'').includes('thumb.mp4'));
    if(ts){
      const tt=await window.__p.invoke('remote_probe_entry',{placeId:window.__rp,id:ts.id,size:Number(ts.size)});
      if(tt.thumb_token){
        const tr=await fetch(base+'/pthumb/'+tt.thumb_token);
        thumbDiag={hasToken:true,status:tr.status,ct:tr.headers.get('Content-Type'),
          bytes:tr.status<400?(await tr.arrayBuffer()).byteLength:0};
      }else{ thumbDiag={hasToken:false,unlocked:tt.unlocked,is_encrypted:tt.is_encrypted,probe_failed:tt.probe_failed,real_name:tt.real_name,size:tt.size}; }
    }
    return JSON.stringify({kind:r.kind,mime:r.mime,total:r.size,out,span,thumbDiag});
  })()`);
  console.log('       [移动预览诊断]', mdiag);
  // 缩略图夹具存在（有 ffmpeg 才造）时，pthumb 必须回 200 且是图片字节；
  // 夹具不存在则 thumbDiag=null，跳过不断言（无 ffmpeg 环境本就抽不了帧）。
  const md=JSON.parse(mdiag);
  if(md.thumbDiag!==null){
    const t=md.thumbDiag;
    if(!t.hasToken || t.status!==200 || !(t.ct||'').startsWith('image/') || !(t.bytes>0)){
      throw new Error('远程缩略图链路失败: '+JSON.stringify(t));
    }
    console.log('       [远程缩略图] pthumb 200', t.ct, t.bytes+'B');
  }else{
    console.log('       [远程缩略图] 夹具无 thumb.mp4.omy（可能无 ffmpeg），跳过');
  }
  // 跨 1 MiB 块边界的中部 seek：app 返回的明文段必须与源明文同区间逐字节一致
  if (md.span) {
    const s = md.span;
    if (s.status !== 206 || s.bytes !== (s.end - s.start + 1)) {
      throw new Error('跨块 seek 响应异常: ' + JSON.stringify(s));
    }
    if (!(s.cr || '').endsWith('/' + md.total)) {
      throw new Error('跨块 seek Content-Range 总长度不符: ' + s.cr + ' total=' + md.total);
    }
    const want = crypto.createHash('sha256').update(srcBuf.subarray(s.start, s.end + 1)).digest('hex');
    if (s.sha256 !== want) {
      throw new Error('跨块 seek 明文段与源明文不一致（块拼接错位）');
    }
    console.log('       [跨块 seek]', s.cr, '明文段 SHA-256 与源一致');
  } else {
    throw new Error('夹具视频不足 2 MiB，跨块 seek 路径未被覆盖（应造数 MB 高熵视频）');
  }
  await cdp.shot('m03-preview');

  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(800);
  await cdp.shot('m04-back-to-dir');

  // 状态栏按原型给出 🔓已解锁 / 🔒不可解锁计数。纯读 DOM 对账（绝不反复
  // invoke remote_browse——那会重新扫描整目录、把列表打回 probing 永不收敛）：
  // 轮询等「识别中」骨架收敛，再比对状态栏 data-stat 与卡片锁定/解锁 class。
  let statCounts = null;
  for (let i = 0; i < 40; i++) {
    statCounts = await cdp.eval(`(()=>{
      const root=document.querySelector('[data-ui="place-browser"]');
      const probing=root.querySelectorAll('.card.probing, .lrow.probing').length;
      const n=sel=>{ const el=root.querySelector('.statusbar [data-stat="'+sel+'"]'); return el?Number(el.dataset.n):0; };
      return JSON.stringify({probing,domUnlocked:n('unlocked'),domLocked:n('locked'),
        lockedCards:root.querySelectorAll('.card.locked, .lrow.locked').length,
        unlockedCards:root.querySelectorAll('.card.is-unlocked, .lrow.is-unlocked').length});
    })()`);
    const s = JSON.parse(statCounts);
    if (s.probing === 0) break;
    await sleep(250);
  }
  console.log('       [状态栏解锁计数]', statCounts);
  const sc = JSON.parse(statCounts);
  if (sc.probing !== 0
      || sc.domLocked !== sc.lockedCards
      || sc.domUnlocked !== sc.unlockedCards) {
    throw new Error('状态栏 🔓/🔒 计数与卡片锁定态不一致或骨架未收敛: ' + statCounts);
  }
  // 移动长按条目应弹出条目菜单（触屏没有右键），含「预览/解密到本地」
  const mm = await click(`(async()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('movie.mp4'));
    if(!r) return 'no-movie';
    const b=r.getBoundingClientRect();
    r.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,cancelable:true,pointerType:'touch',clientX:b.x+15,clientY:b.y+15}));
    await new Promise(res=>setTimeout(res,520));
    r.dispatchEvent(new PointerEvent('pointerup',{bubbles:true,cancelable:true,pointerType:'touch'}));
    await new Promise(res=>setTimeout(res,200));
    return document.querySelector('.ctxmenu')?'menu':'none';
  })()`, 100);
  // 菜单的「从缓存中移除」是打开菜单时异步查单文件缓存状态后才响应式加入的，
  // m03 预览已缓存该文件头部若干密文块，故轮询等待该项出现（最多 4 秒）。
  const mmenu = await cdp.eval(`(async()=>{
    for(let i=0;i<40;i++){
      const m=document.querySelector('.ctxmenu');
      const keys=m?[...m.querySelectorAll('.mi')].map(x=>x.getAttribute('data-mi')):[];
      if(keys.includes('remove-cache')) return JSON.stringify(keys);
      await new Promise(r=>setTimeout(r,100));
    }
    const m=document.querySelector('.ctxmenu');
    return JSON.stringify(m?[...m.querySelectorAll('.mi')].map(x=>x.getAttribute('data-mi')):[]);
  })()`);
  console.log('       移动长按菜单 ->', mmenu);
  const mmkeys = JSON.parse(mmenu);
  if (!mmkeys.includes('open') || !mmkeys.includes('decrypt-local')
      || !mmkeys.includes('remove-cache')) {
    throw new Error('移动长按菜单应含 预览/解密到本地/从缓存中移除（预览后有密文缓存）: ' + mmenu);
  }
  await cdp.shot('m10-menu');
  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(300);

  // 退回位置列表再退回本地
  await click(`(()=>{ const s=document.querySelector('.crumbpath .crumbseg'); if(s){s.click();return 'list';} return 'none'; })()`, 600);
  await click(`(()=>{ const b=document.querySelector('.crumb .crumbbtn'); if(b){b.click();return 'close';} return 'none'; })()`, 700);

  // 设置（底栏第 4 项）
  await click(`(()=>{ const b=document.querySelectorAll('.pnav .pnavi')[3]; if(!b) return 'no-settings'; b.click(); return 'ok'; })()`, 800);
  // 移动设置主页：语言/主题/默认视图/启动位置直接平铺可改；其余按
  // 「远程位置/安全/其他」分组，每行带当前值摘要，缓存可一键直达二级页。
  const mhome = await cdp.eval(`(()=>{
    const q=s=>!!document.querySelector(s);
    const rv=sp=>document.querySelector('.mlist [data-sp="'+sp+'"] .mrv')?.textContent || '';
    return JSON.stringify({
      lang:q('.mlist [data-sf="m_language"]'), theme:q('.mlist [data-sf="m_theme"]'),
      view:q('.mlist [data-sf="m_view"]'), startup:q('.mlist [data-sf="m_startup"]'),
      generalEntry:q('.mlist [data-sp="general"]'),
      remote:q('.mlist [data-sp="remote"]'), security:q('.mlist [data-sp="security"]'),
      encrypt:q('.mlist [data-sp="encrypt"]'), devices:q('.mlist [data-sp="devices"]'),
      playback:q('.mlist [data-sp="playback"]'), about:q('.mlist [data-sp="about"]'),
      cacheEntry:q('.mlist [data-sf="cache_entry"]'),
      groups:document.querySelectorAll('.mlist .mgh').length,
      remoteCount:rv('remote'), securityVal:rv('security'), aboutVer:rv('about'),
    });
  })()`);
  console.log('       [移动设置主页]', mhome);
  const mh = JSON.parse(mhome);
  if (!mh.lang || !mh.theme || !mh.view || !mh.startup || mh.generalEntry
      || !mh.remote || !mh.security || !mh.encrypt || !mh.devices || !mh.playback
      || !mh.about || !mh.cacheEntry || mh.groups !== 4
      || mh.remoteCount !== '1' || !/0\.0\.1/.test(mh.aboutVer)) {
    throw new Error('移动设置主页分组/入口/摘要不符合原型: ' + mhome);
  }
  // 语言在主页切换必须即时生效（选 English 组标题应变，再切回中文），不留下语言污染
  const langDiag = await cdp.eval(`(async()=>{
    const sleep=ms=>new Promise(r=>setTimeout(r,ms));
    const sel=document.querySelector('.mlist [data-sf="m_language"]');
    const gh=()=>document.querySelector('.mlist .mgh')?.textContent;
    const before=gh();
    sel.value='en'; sel.dispatchEvent(new Event('change',{bubbles:true}));
    await sleep(180);
    const en=gh();
    sel.value='zh-CN'; sel.dispatchEvent(new Event('change',{bubbles:true}));
    await sleep(180);
    const zh=gh();
    return JSON.stringify({before,en,zh});
  })()`);
  console.log('       [主页语言即时切换]', langDiag);
  const ld = JSON.parse(langDiag);
  if (!ld.before || ld.en === ld.before || ld.zh !== ld.before) {
    throw new Error('移动主页语言切换未即时生效或未还原: ' + langDiag);
  }
  await cdp.shot('m05-settings');
  await click(`(()=>{ const n=document.querySelector('[data-sp="security"]'); if(!n) return 'no-security-nav'; n.click(); return 'ok'; })()`, 500);
  await cdp.shot('m08-security');
  await cdp.eval(`document.querySelector('[data-si="back"]')?.click(); 'back'`);
  await sleep(300);
  await click(`(()=>{ const n=document.querySelector('[data-sp="remote"]'); if(!n) return 'no-remote-nav'; n.click(); return 'ok'; })()`, 600);
  await cdp.shot('m06-remote');
  await click(`(()=>{ const b=document.querySelector('[data-sf="cache_entry"]'); if(!b) return 'no-cache-entry'; b.click(); return 'ok'; })()`, 700);
  await cdp.shot('m07-cache');
  // 缓存是「远程位置」下的二级页，back 一次只回到远程详情；
  // 直接关掉重开设置回到主列表，再进「关于」，回归移动二级页外壳
  await cdp.eval(`document.querySelector('[data-si="close"]')?.click(); 'closed'`);
  await sleep(600);
  await click(`(()=>{ const b=document.querySelectorAll('.pnav .pnavi')[3]; if(!b) return 'no-settings'; b.click(); return 'ok'; })()`, 800);
  await click(`(()=>{ const n=document.querySelector('[data-sp="about"]'); if(!n) return 'no-about-nav'; n.click(); return 'ok'; })()`, 500);
  await cdp.shot('m09-about');
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

  // 桌面网格：带缩略图的远程卡片应真正渲染出 pthumb 图片（naturalWidth>0），
  // 而不只是有个 img 标签。仅有缩略图夹具时断言。
  if (md.thumbDiag) {
    const dthumb = await cdp.eval(`(async()=>{
      const sleep=ms=>new Promise(r=>setTimeout(r,ms));
      for(let i=0;i<30;i++){
        const c=[...document.querySelectorAll('.content .card')].find(x=>(x.querySelector('.cname')?.textContent||'').includes('thumb.mp4'));
        if(c){ const img=c.querySelector('.thumb img');
          if(img && img.src.includes('/pthumb/') && img.naturalWidth>0) return {ok:true,w:img.naturalWidth,h:img.naturalHeight};
          if(i>4 && !img) return {ok:false,reason:'grid-card-has-no-pthumb-img'};
        }
        await sleep(150);
      }
      return {ok:false,reason:'timeout'};
    })()`);
    console.log('       [桌面网格缩略图]', JSON.stringify(dthumb));
    if (!dthumb.ok) throw new Error('桌面网格未渲染远程缩略图: ' + JSON.stringify(dthumb));
  }

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

  // ===== 远程「解密到本地」（只读位置也保留的主要用途）=====
  console.log('\n[解密到本地]');
  const decDir = process.argv[8];
  fs.mkdirSync(decDir, { recursive: true });

  // 取 movie 条目引用，供缓存 stat/移除断言复用
  const dmov = JSON.parse(await cdp.eval(`(async()=>{
    const list=await window.__p.invoke('remote_browse',{placeId:window.__rp,dir:''});
    const m=list.find(x=>/movie\\.mp4\\.omy$/.test(x.id));
    return JSON.stringify({id:m.id,size:Number(m.size)});
  })()`));

  // 桌面右键 movie 条目：菜单必须含「打开/预览」「解密到本地」与「从缓存中移除」
  // （d02 预览已缓存头部密文块，该项由打开菜单时的异步 stat 响应式加入）。
  const menuOpen = await click(`(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('movie.mp4'));
    if(!r) return 'no-movie';
    const b=r.getBoundingClientRect();
    r.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,button:2,clientX:b.x+20,clientY:b.y+20}));
    return 'ok';
  })()`, 500);
  const menuDiag = await cdp.eval(`(async()=>{
    for(let i=0;i<40;i++){
      const m=document.querySelector('.ctxmenu');
      const keys=m?[...m.querySelectorAll('.mi')].map(x=>x.getAttribute('data-mi')):[];
      if(keys.includes('remove-cache')){
        const dl=m.querySelector('[data-mi="decrypt-local"]');
        return JSON.stringify({open:true,keys,decryptDisabled:dl?dl.disabled:null});
      }
      await new Promise(r=>setTimeout(r,100));
    }
    const m=document.querySelector('.ctxmenu');
    if(!m) return JSON.stringify({open:false});
    const keys=[...m.querySelectorAll('.mi')].map(x=>x.getAttribute('data-mi'));
    const dl=m.querySelector('[data-mi="decrypt-local"]');
    return JSON.stringify({open:true,keys,decryptDisabled:dl?dl.disabled:null});
  })()`);
  console.log('       桌面右键菜单 ->', menuDiag);
  await cdp.shot('d10-menu');
  const dmm = JSON.parse(menuDiag);
  if (!dmm.open || !dmm.keys.includes('open') || !dmm.keys.includes('decrypt-local')
      || !dmm.keys.includes('remove-cache') || dmm.decryptDisabled === true) {
    throw new Error('桌面右键菜单缺少可用的「解密到本地 / 从缓存中移除」项: ' + menuDiag);
  }

  // 真实点击菜单项：pick_folder 自动化旁路返回 davDir，前端 action 应在该目录
  // 落一份明文。这一步覆盖菜单点击→选目录→invoke→进度→落盘的完整前端链路。
  const davDir = process.argv[7];
  fs.rmSync(path.join(davDir, 'movie.mp4'), { force: true });
  await click(`(()=>{ const b=document.querySelector('.ctxmenu [data-mi="decrypt-local"]');
    if(!b) return 'no-item'; b.click(); return 'ok'; })()`, 2800);
  const viaMenu = path.join(davDir, 'movie.mp4');
  if (!fs.existsSync(viaMenu)) throw new Error('菜单「解密到本地」未在所选目录落盘');
  const viaBuf = fs.readFileSync(viaMenu);
  if (viaBuf.length !== srcSize
      || crypto.createHash('sha256').update(viaBuf).digest('hex') !== srcSha) {
    throw new Error('菜单解密出的明文与源明文不一致');
  }
  console.log('       菜单解密落盘校验通过', viaBuf.length + 'B');
  fs.rmSync(viaMenu, { force: true });
  fs.rmSync(path.join(davDir, 'movie.mp4.part'), { force: true });

  // 独立目录直接 invoke：验证后端流式落盘字节正确，且默认不覆盖（第二次 target_exists）
  const decDiag = await cdp.eval(`(async()=>{
    const list=await window.__p.invoke('remote_browse',{placeId:window.__rp,dir:''});
    const m=list.find(x=>/movie\\.mp4\\.omy$/.test(x.id));
    const args={req:{place_id:window.__rp,path:m.id,size:Number(m.size)},destDir:${JSON.stringify(decDir)}};
    const r1=await window.__p.invoke('remote_decrypt_to_local',args);
    let second='';
    try{ await window.__p.invoke('remote_decrypt_to_local',args); second='no-error'; }
    catch(e){ second=(e && typeof e==='object' && e.code) ? e.code : String(e); }
    return JSON.stringify({r1,second});
  })()`);
  console.log('       后端流式解密/覆盖 ->', decDiag);
  const dd = JSON.parse(decDiag);
  if (!dd.r1 || dd.r1.bytes !== srcSize || !/movie\.mp4$/.test(dd.r1.saved_path || '')) {
    throw new Error('远程解密到本地返回不符合预期: ' + decDiag);
  }
  if (!/target_exists/.test(dd.second)) {
    throw new Error('目标已存在时未拒绝覆盖: ' + decDiag);
  }
  const decBuf = fs.readFileSync(path.join(decDir, 'movie.mp4'));
  if (decBuf.length !== srcSize
      || crypto.createHash('sha256').update(decBuf).digest('hex') !== srcSha) {
    throw new Error('独立目录解密明文与源明文不一致');
  }
  console.log('       流式解密字节/哈希一致，重复解密被 target_exists 拒绝');

  // ===== 单文件密文块缓存：完整覆盖 → 前端菜单移除 → 清空 → 菜单隐藏 =====
  console.log('\n[单文件缓存]');
  const fileRef = `{place_id:window.__rp,path:${JSON.stringify(dmov.id)},size:${dmov.size}}`;
  // 解密读了全文，该文件所有密文块都应已缓存；夹具是跨 5 块的高熵大片，
  // total_blocks 必须 >=2，否则说明根本没走到跨块路径
  const st1 = JSON.parse(await cdp.eval(`(async()=>JSON.stringify(
    await window.__p.invoke('remote_cache_file_stat',{req:${fileRef}})))()`));
  console.log('       解密后缓存覆盖 ->', JSON.stringify(st1));
  if (!(st1.total_blocks >= 2) || st1.cached_blocks !== st1.total_blocks
      || st1.fully_cached !== true || st1.cached_bytes === 0) {
    throw new Error('解密读全文后应完整缓存且跨多块: ' + JSON.stringify(st1));
  }

  // 前端真实链路：右键 → 点「从缓存中移除」→ 菜单关闭并清空该文件密文块
  const ctxMovie = `(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('movie.mp4'));
    if(!r) return 'no-movie';
    const b=r.getBoundingClientRect();
    r.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,button:2,clientX:b.x+20,clientY:b.y+20}));
    return 'ok';
  })()`;
  await click(ctxMovie, 500);
  await click(`(async()=>{
    for(let i=0;i<40;i++){
      const b=document.querySelector('.ctxmenu [data-mi="remove-cache"]');
      if(b){ b.click(); return 'clicked'; }
      await new Promise(r=>setTimeout(r,100));
    }
    return 'no-remove-item';
  })()`, 1500);
  await sleep(800);
  const st2 = JSON.parse(await cdp.eval(`(async()=>JSON.stringify(
    await window.__p.invoke('remote_cache_file_stat',{req:${fileRef}})))()`));
  console.log('       前端移除后 ->', JSON.stringify(st2));
  if (st2.cached_blocks !== 0 || st2.fully_cached !== false || st2.cached_bytes !== 0) {
    throw new Error('前端「从缓存中移除」后该文件缓存应清零: ' + JSON.stringify(st2));
  }
  // 后端移除幂等：已清空再删返回 freed_bytes=0，同时验证返回结构含该字段
  const freedAgain = JSON.parse(await cdp.eval(`(async()=>JSON.stringify(
    await window.__p.invoke('remote_cache_remove_file',{req:${fileRef}})))()`));
  if (freedAgain.freed_bytes !== 0) {
    throw new Error('缓存已空时移除应幂等返回 0: ' + JSON.stringify(freedAgain));
  }
  // 缓存清零后，菜单不应再出现「从缓存中移除」（避免空操作）
  await click(ctxMovie, 500);
  const hiddenDiag = await cdp.eval(`(async()=>{
    for(let i=0;i<12;i++){
      const b=document.querySelector('.ctxmenu [data-mi="remove-cache"]');
      if(b) return 'still-present';
      await new Promise(r=>setTimeout(r,100));
    }
    const m=document.querySelector('.ctxmenu');
    return JSON.stringify(m?[...m.querySelectorAll('.mi')].map(x=>x.getAttribute('data-mi')):[]);
  })()`);
  console.log('       清空后菜单 ->', hiddenDiag);
  if (hiddenDiag === 'still-present' || /remove-cache/.test(hiddenDiag)) {
    throw new Error('缓存已清空的文件不应再显示「从缓存中移除」: ' + hiddenDiag);
  }
  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(300);

  await cdp.eval(`document.dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true})); 'esc'`);
  await sleep(300);

  await click(`(()=>{ const s=document.querySelector('.crumbpath .crumbseg'); if(s){s.click();return 'list';} return 'none'; })()`, 500);
  await click(`(()=>{ const b=document.querySelector('.crumb .crumbbtn'); if(b){b.click();return 'close';} return 'none'; })()`, 600);

  // PC 设置弹窗
  await click(`(()=>{ const b=document.querySelector('[data-tb="settings"]'); if(!b) return 'no-settings'; b.click(); return 'ok'; })()`, 800);
  await cdp.shot('d03-settings');
  // 缩略图开关已从「通用」迁到「播放与预览」
  const geOk = await cdp.eval(`!document.querySelector('[data-pane="general"] [data-sf="thumbnails"]')`);
  await click(`(()=>{ const n=document.querySelector('[data-sp="security"]'); if(!n) return 'no-security-nav'; n.click(); return 'ok'; })()`, 500);
  await cdp.shot('d06-security');
  // 加密文件「外部程序打开后擦除临时明文」尚未实现，不能放假开关
  const seOk = await cdp.eval(`!document.querySelector('[data-pane="security"] [data-sf="wipe_temp_plaintext"]')`);
  await click(`(()=>{ const n=document.querySelector('[data-sp="remote"]'); if(!n) return 'no-remote-nav'; n.click(); return 'ok'; })()`, 600);
  await cdp.shot('d04-remote');
  await click(`(()=>{ const b=document.querySelector('[data-sf="cache_entry"]'); if(!b) return 'no-cache-entry'; b.click(); return 'ok'; })()`, 700);
  const pcCacheDiag = await cdp.eval(`(()=>({
    panes: [...document.querySelectorAll('[data-pane]')].map(e=>e.getAttribute('data-pane')),
    titles: [...document.querySelectorAll('.sethn')].map(e=>e.textContent.trim()),
    paneSectionCount: document.querySelectorAll('.setbody section.pane').length,
    hasNav: !!document.querySelector('.setnav'),
    hasMlist: !!document.querySelector('.mlist'),
    hasCacheLimit: !!document.querySelector('[data-sf="cache_limit"]'),
    hasScanScope: !!document.querySelector('[data-sf="scan_omy_only"]'),
    w: window.innerWidth,
    mq: window.matchMedia('(max-width:768px)').matches
  }))()`);
  console.log('[PC缓存诊断] ' + JSON.stringify(pcCacheDiag));
  await cdp.shot('d05-cache');

  // 播放与预览 / 设备与共享 / 关于（分类归位回归）。
  // 注意整个设置只有一个 [data-pane]，其值是「当前」分类，所以必须在
  // 每一页激活的当下断言该页控件，不能切走后再回头查。
  await click(`(()=>{ const n=document.querySelector('[data-sp="playback"]'); if(!n) return 'no'; n.click(); return 'ok'; })()`, 400);
  await cdp.shot('d07-playback');
  const pbOk = await cdp.eval(`!!document.querySelector('[data-pane="playback"] [data-sf="thumbnails"]')`);
  await click(`(()=>{ const n=document.querySelector('[data-sp="devices"]'); if(!n) return 'no'; n.click(); return 'ok'; })()`, 400);
  await cdp.shot('d08-devices');
  const dvOk = await cdp.eval(`(()=>{ const d=document.querySelector('[data-pane="devices"]'); return d&&!!d.querySelector('[data-sf="device_name"]')&&!!d.querySelector('[data-sf="autostart"]')&&!!d.querySelector('[data-sf="manage_devices"]'); })()`);
  await click(`(()=>{ const n=document.querySelector('[data-sp="about"]'); if(!n) return 'no'; n.click(); return 'ok'; })()`, 500);
  await cdp.shot('d09-about');
  const aboutText = await cdp.eval(`(document.querySelector('[data-pane="about"]')||{textContent:''}).textContent.replace(/\\s+/g,' ').trim()`);
  console.log('[设置分类诊断] ' + JSON.stringify({ geOk, seOk, pbOk, dvOk, aboutText }));
  if (geOk !== true || seOk !== true || pbOk !== true || dvOk !== true ||
      !/omy\s+0\.0\.1/.test(aboutText) ||
      !/OMYFILE\s+v1\.0/.test(aboutText) || !aboutText.includes('GPL-3.0')) {
    throw new Error('播放预览/设备/关于页不符合预期: ' + JSON.stringify({ geOk, seOk, pbOk, dvOk, aboutText }));
  }

  await cdp.eval(`document.querySelector('[data-si="close"]')?.click(); 'closed'`);
  // 关闭走 onClose→save（落盘 + 应用缓存设置）是异步的，没等它真正关闭就点
  // 齿轮，点击会被尚未消失的遮罩吞掉、设置窗不再打开。
  await sleep(1200);

  // ===== 加密默认值驱动对话框（设置 → 加密默认值应成为对话框初始值）=====
  console.log('\n[加密默认值]');
  const plainDir = process.argv[7];
  // 1) 备份配置，写入一组显眼的非默认值后导航到含明文 plain.txt 的目录
  await cdp.eval(`(async()=>{
    const cfg = await window.__p.invoke('config_get');
    window.__savedCfg = JSON.parse(JSON.stringify(cfg));
    cfg.defaults.kdf_profile='sensitive';
    cfg.defaults.chunk_size='4M';
    cfg.defaults.name_mode='plain';
    cfg.defaults.original_action='delete';
    cfg.compress.enabled=false;
    await window.__p.invoke('config_set',{config:cfg});
    return 'set';
  })()`);
  // 点侧栏「打开文件夹…」。原生选择器在自动化下由 OMY_GUI_PICK_FOLDER
  //  环境变量旁路（见 commands.rs），直接返回明文目录并驱动前端 navigate。
  const nav = await click(`(()=>{
    const b=document.querySelector('[data-side="pick-folder"]');
    if(!b) return 'no-pick';
    b.click(); return 'clicked';
  })()`, 1500);
  console.log('       导航到明文目录 ->', nav);
  await sleep(1200);
  // 2) 选中明文文件并打开加密对话框
  const encDiag = await cdp.eval(`(()=>{
    const inPlace=!!document.querySelector('[data-pb-enter],[data-pb="add"]');
    const all=[...document.querySelectorAll('.card,.lrow')];
    return JSON.stringify({inPlace, n:all.length, names:all.map(x=>x.querySelector('.nm,.cname')?.textContent||'').filter(Boolean).slice(0,12)});
  })()`);
  console.log('       加密前诊断 ->', encDiag);
  const pick = await click(`(()=>{
    const rows=[...document.querySelectorAll('.content .lrow, .content .card, .lrow, .card')];
    const r=rows.find(x=>(x.querySelector('.nm,.cname')?.textContent||'').includes('plain.txt'));
    if(!r) return 'no-plain';
    r.click(); return 'ok';
  })()`, 500);
  console.log('       选中 plain.txt ->', pick);
  const encBtn = await click(`(()=>{ const b=document.querySelector('.vtoggle button.primary'); if(!b) return 'no-encrypt-btn'; b.click(); return 'ok'; })()`, 1000);
  const encState = await cdp.eval(`(()=>{
    const q=s=>document.querySelector('.dlg '+s);
    return {
      dlg: !!document.querySelector('.dlg'),
      btn: ${JSON.stringify(encBtn)},
      sensitive: !!(q('input[type=radio][value=sensitive]')||{}).checked,
      plain: !!(q('input[type=radio][value=plain]')||{}).checked,
      del: !!(q('input[type=radio][value=delete]')||{}).checked,
      keepExt: !!(q('input[type=radio][value=keep_ext]')||{}).checked,
      compress: (q('[data-sf=enc_compress]')||{}).checked ?? null,
      chunk: (q('[data-sf=enc_chunk]')||{}).value ?? null
    };
  })()`);
  console.log('       加密对话框默认值 ->', JSON.stringify(encState));
  await cdp.shot('e01-encrypt-defaults');
  // 3) 取消对话框、恢复配置
  await click(`(()=>{ const b=document.querySelector('.dlg .acts button:not(.primary)'); if(!b) return 'no-cancel'; b.click(); return 'ok'; })()`, 500);
  await cdp.eval(`(async()=>{ if(window.__savedCfg){ await window.__p.invoke('config_set',{config:window.__savedCfg}); } return 'restored'; })()`);
  if (!encState.dlg || encState.btn !== 'ok' || !encState.sensitive || !encState.plain
      || !encState.del || encState.keepExt || encState.compress !== false || String(encState.chunk) !== '6') {
    throw new Error('加密默认值未驱动对话框初始值: ' + JSON.stringify(encState));
  }

  // ===== 视图切换持久化 + 上次目录记忆（通用页默认视图/启动位置的真实链路）=====
  console.log('\n[视图/起始目录]');
  await click(`(()=>{ const b=document.querySelector('[data-tb="view-list"]'); if(!b) return 'no-list-btn'; b.click(); return 'ok'; })()`, 500);
  await sleep(300);
  const vList = await cdp.eval(`window.__p.invoke('config_get').then(c=>c.ui.view)`);
  await click(`(()=>{ const b=document.querySelector('[data-tb="view-grid"]'); if(!b) return 'no-grid-btn'; b.click(); return 'ok'; })()`, 500);
  await sleep(300);
  const vGrid = await cdp.eval(`window.__p.invoke('config_get').then(c=>c.ui.view)`);
  // 点侧栏「主目录」走真实 store.navigate，去抖 800ms 后 last_dir 应落盘为该路径
  const homePath = await cdp.eval(`document.querySelector('[data-side="place-home"]')?.getAttribute('title')||''`);
  await click(`(()=>{ const b=document.querySelector('[data-side="place-home"]'); if(!b) return 'no-home'; b.click(); return 'ok'; })()`, 1200);
  await sleep(1000);
  const lastDir = await cdp.eval(`window.__p.invoke('config_get').then(c=>c.ui.last_dir||'')`);
  console.log('[视图/起始目录诊断] ' + JSON.stringify({ vList, vGrid, homePath, lastDir }));
  if (vList !== 'list' || vGrid !== 'grid' || !homePath || lastDir !== homePath) {
    throw new Error('视图持久化/上次目录记忆不符合预期: ' + JSON.stringify({ vList, vGrid, homePath, lastDir }));
  }
  // 还原配置（last_dir 被这一步写成主目录，view 虽已点回网格，仍整体还原一次）
  await cdp.eval(`(async()=>{ if(window.__savedCfg){ await window.__p.invoke('config_set',{config:window.__savedCfg}); } return 'restored'; })()`);

  // 安全页「立即锁定」真实验证：重开设置 → 安全 → 点锁定，
  // 断言会话密码数归零、设置弹窗被关闭（走 App 统一锁定收尾）。
  console.log('\n[立即锁定]');
  const reopened = await click(`(()=>{ const b=document.querySelector('[data-tb="settings"]'); if(!b) return 'no-settings'; b.click(); return String(!!document.querySelector('.setdlg')); })()`, 800);
  const dlgOpen = await cdp.eval(`String(!!document.querySelector('.setdlg'))`);
  console.log('       重开设置 ->', reopened, 'dlgOpen=', dlgOpen);
  const before = await cdp.eval(`window.__p.invoke('credential_count').then(n=>String(n))`);
  await click(`(()=>{ const n=document.querySelector('[data-sp="security"]'); if(n) n.click(); return 'ok'; })()`, 400);
  const clicked = await click(`(()=>{ const b=document.querySelector('[data-sf="lock_now"]'); if(!b) return 'no-lock-btn'; if(b.disabled) return 'disabled'; b.click(); return 'clicked'; })()`, 1200);
  await sleep(600);
  const after = await cdp.eval(`window.__p.invoke('credential_count').then(n=>String(n))`);
  const dlgGone = await cdp.eval(`String(!document.querySelector('.setdlg'))`);
  const lockAssert = { before, clicked, after, dlgGone };
  console.log('[立即锁定诊断] ' + JSON.stringify(lockAssert));
  // 夹具本地 vault 可能含多个不同 salt 的库（如额外的缩略图样例），
  // 锁定前凭据数只要求 >0，关键是锁定后必须全部归零、弹窗关闭。
  if (!(Number(before) > 0) || clicked !== 'clicked' || after !== '0' || dlgGone !== 'true') {
    throw new Error('立即锁定未生效: ' + JSON.stringify(lockAssert));
  }

  console.log('\n截图目录:', shotsDir);
  shots.forEach((s) => console.log('  ', path.basename(s)));
  console.log('UI_SHOTS_OK');
  ws.close();
}

main().catch((e) => { console.error('截图探针异常:', e); process.exit(1); });
