#!/usr/bin/env node
/**
 * 通过 CDP 驱动 Spike S1/S5，无需人工点击。
 *
 * 为什么用 CDP 而不是 GUI 自动化：
 * - 能拿到确定的状态码、响应头与错误码，而不是靠截图猜；
 * - 能读取 <video> 的真实属性（duration/currentTime/buffered/error.code），
 *   这是判断「seek 真的生效」与「假通过」的唯一可靠依据；
 * - 可重复执行，便于修 bug 后立刻回归。
 *
 * 用法: node spikes/cdp-drive.mjs [port]
 * 前置: 以 TAURI_REMOTE_DEBUGGING_PORT=<port> 启动 spike。
 */

// 自建最小 WS 客户端：Node 20 的全局 WebSocket 需要
// --experimental-websocket，而 `ws` 包需要联网安装。
import { MiniWs } from './mini-ws.mjs';

const PORT = process.argv[2] || '9222';
const BASE = `http://127.0.0.1:${PORT}`;

function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }

/** 取目标页面列表，带重试——WebView 启动需要时间。 */
async function findTarget(retries = 30) {
  for (let i = 0; i < retries; i++) {
    try {
      const r = await fetch(`${BASE}/json/list`);
      const list = await r.json();
      const page = list.find(t => t.type === 'page' && t.webSocketDebuggerUrl);
      if (page) return page;
    } catch { /* 端口还没起来 */ }
    await sleep(500);
  }
  throw new Error(`无法连接到 CDP ${BASE}。请确认已用 TAURI_REMOTE_DEBUGGING_PORT=${PORT} 启动 spike。`);
}

/** 极简 CDP 客户端。 */
class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.events = [];
    this.listeners = [];
    ws.on('message', raw => {
      const msg = JSON.parse(raw);
      if (msg.id !== undefined) {
        const p = this.pending.get(msg.id);
        if (p) {
          this.pending.delete(msg.id);
          msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
        }
      } else {
        this.events.push(msg);
        for (const fn of this.listeners) fn(msg);
      }
    });
  }
  static async connect(url) {
    const ws = new MiniWs(url);
    await new Promise((res, rej) => {
      ws.once('open', res);
      ws.once('error', e => rej(new Error('WebSocket 连接失败: ' + e.message)));
    });
    return new Cdp(ws);
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      setTimeout(() => {
        if (this.pending.has(id)) {
          this.pending.delete(id);
          reject(new Error(`${method} 超时`));
        }
      }, 30000);
    });
  }
  on(fn) { this.listeners.push(fn); }
  /** 在页面里求值，返回 JSON 化的结果。 */
  async eval(expr, awaitPromise = true) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise,
    });
    if (r.exceptionDetails) {
      throw new Error('页面异常: ' + (r.exceptionDetails.exception?.description
        || r.exceptionDetails.text));
    }
    return r.result?.value;
  }
  close() { try { this.ws.close(); } catch {} }
}

const pass = [];
const fail = [];
function check(cond, label, detail = '') {
  (cond ? pass : fail).push(label);
  const mark = cond ? 'PASS' : 'FAIL';
  console.log(`  ${mark}  ${label}${detail ? '  ' + detail : ''}`);
  return cond;
}

const main = async () => {
  console.log(`连接 CDP ${BASE} ...`);
  const target = await findTarget();
  console.log(`目标: ${target.title}  ${target.url}`);

  const cdp = await Cdp.connect(target.webSocketDebuggerUrl);
  await cdp.send('Runtime.enable');
  await cdp.send('Network.enable');
  await cdp.send('Log.enable');
  await cdp.send('Page.enable');

  // 收集网络与控制台事件——这是判断请求是否到达、状态码为何的依据
  const responses = [];
  const failures = [];
  const consoleMsgs = [];
  cdp.on(msg => {
    if (msg.method === 'Network.responseReceived') {
      const r = msg.params.response;
      responses.push({ url: r.url, status: r.status, headers: r.headers,
                       mime: r.mimeType, fromCache: r.fromDiskCache });
    }
    if (msg.method === 'Network.loadingFailed') {
      failures.push({ text: msg.params.errorText, type: msg.params.type,
                      blocked: msg.params.blockedReason });
    }
    if (msg.method === 'Log.entryAdded') {
      consoleMsgs.push(`[${msg.params.entry.level}] ${msg.params.entry.text}`);
    }
    if (msg.method === 'Runtime.consoleAPICalled') {
      consoleMsgs.push('[console] ' + msg.params.args
        .map(a => a.value ?? a.description ?? '').join(' '));
    }
  });

  // ---------- 阶段 1：直接用 fetch 探协议层 ----------
  // 先绕开 <video>：若 fetch 都拿不到 206，问题在协议层而非媒体解码。
  console.log('\n=== 阶段 1：协议层（fetch 直连，绕开 <video>）===');
  const probeUrl = 'http://omystream.localhost/video';
  const probe = await cdp.eval(`(async () => {
    const out = [];
    const cases = [
      ['无 Range',    null],
      ['bytes=0-1023', 'bytes=0-1023'],
      ['bytes=1024-2047', 'bytes=1024-2047'],
      ['bytes=-512',  'bytes=-512'],
      ['bytes=999999999-', 'bytes=999999999-'],
    ];
    for (const [label, range] of cases) {
      try {
        const opt = range ? { headers: { Range: range } } : {};
        const r = await fetch(${JSON.stringify(probeUrl)}, opt);
        const b = await r.arrayBuffer();
        out.push({ label, ok: true, status: r.status, len: b.byteLength,
          contentRange: r.headers.get('content-range'),
          acceptRanges: r.headers.get('accept-ranges'),
          cacheControl: r.headers.get('cache-control'),
          contentType: r.headers.get('content-type') });
      } catch (e) {
        out.push({ label, ok: false, error: String(e) });
      }
    }
    return out;
  })()`);

  for (const p of probe) {
    if (!p.ok) { console.log(`  ${p.label}: 失败 ${p.error}`); continue; }
    console.log(`  ${p.label.padEnd(20)} HTTP ${p.status}  ${p.len} B  ` +
                `CR=${p.contentRange || '-'}  AR=${p.acceptRanges || '-'}`);
  }
  const full = probe.find(p => p.label === '无 Range');
  const r1 = probe.find(p => p.label === 'bytes=0-1023');
  const r2 = probe.find(p => p.label === 'bytes=1024-2047');
  const suf = probe.find(p => p.label === 'bytes=-512');
  const oob = probe.find(p => p.label === 'bytes=999999999-');

  check(full?.ok && full.status === 200, '无 Range 返回 200',
        full?.ok ? `实际 ${full.status}` : '');
  check(r1?.ok && r1.status === 206 && r1.len === 1024,
        'bytes=0-1023 返回 206 且恰好 1024 字节',
        r1?.ok ? `实际 ${r1.status}/${r1.len}B` : '');
  check(r2?.ok && r2.status === 206 && r2.len === 1024,
        'bytes=1024-2047 返回 206 且恰好 1024 字节',
        r2?.ok ? `实际 ${r2.status}/${r2.len}B` : '');
  check(suf?.ok && suf.status === 206 && suf.len === 512,
        'bytes=-512 返回最后 512 字节',
        suf?.ok ? `实际 ${suf.status}/${suf.len}B` : '');
  check(oob?.ok && oob.status === 416, '越界 Range 返回 416',
        oob?.ok ? `实际 ${oob.status}` : '');
  check(r1?.cacheControl?.includes('no-store'),
        '响应头含 Cache-Control: no-store（S5 前置条件）',
        r1?.cacheControl || '');

  // ---------- 阶段 2：<video> 能否加载元数据 ----------
  console.log('\n=== 阶段 2：<video> 元数据 ===');
  const meta = await cdp.eval(`(async () => {
    const v = document.getElementById('v');
    v.src = ${JSON.stringify(probeUrl)};
    v.load();
    return await new Promise(resolve => {
      let done = false;
      const ok = () => { if (done) return; done = true; resolve({
        ok: true, duration: v.duration, w: v.videoWidth, h: v.videoHeight }); };
      const bad = () => { if (done) return; done = true; resolve({
        ok: false, code: v.error?.code, message: v.error?.message || '' }); };
      v.addEventListener('loadedmetadata', ok, { once: true });
      v.addEventListener('error', bad, { once: true });
      setTimeout(() => { if (!done) { done = true; resolve({ ok: false,
        code: v.error?.code ?? null, message: '超时未触发任何事件',
        networkState: v.networkState, readyState: v.readyState }); } }, 15000);
    });
  })()`);

  if (!meta.ok) {
    console.log(`  FAIL  <video> 加载失败: code=${meta.code} ${meta.message}`);
    console.log(`        networkState=${meta.networkState} readyState=${meta.readyState}`);
    fail.push('<video> 加载元数据');
  } else {
    check(true, '<video> 成功加载元数据',
          `时长 ${meta.duration?.toFixed(2)}s, ${meta.w}x${meta.h}`);
    check(Math.abs(meta.duration - 60) < 1, '时长约为 60s',
          `实际 ${meta.duration?.toFixed(2)}s`);
    check(meta.w === 640 && meta.h === 360, '分辨率 640x360',
          `实际 ${meta.w}x${meta.h}`);
  }

  // ---------- 阶段 3：乱序 seek ----------
  let seekResult = null;
  if (meta.ok) {
    console.log('\n=== 阶段 3：乱序 seek（S1 核心）===');
    seekResult = await cdp.eval(`(async () => {
      const v = document.getElementById('v');
      // 乱序且跨度大：顺序递增可能被顺序读取蒙对，证明不了随机访问
      const fracs = [0.85, 0.15, 0.60, 0.05, 0.95, 0.35];
      const results = [];
      for (const f of fracs) {
        const target = +(v.duration * f).toFixed(2);
        const t0 = performance.now();
        const ok = await new Promise(res => {
          let done = false;
          const h = () => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(true); } };
          v.addEventListener('seeked', h);
          v.currentTime = target;
          setTimeout(() => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(false); } }, 8000);
        });
        results.push({ frac: f, target, ok,
          landed: +v.currentTime.toFixed(2),
          ms: Math.round(performance.now() - t0),
          readyState: v.readyState });
      }
      return results;
    })()`);

    for (const s of seekResult) {
      const drift = Math.abs(s.landed - s.target);
      console.log(`  ${(s.frac * 100).toFixed(0).padStart(3)}% → ${String(s.target).padStart(6)}s  ` +
        `${s.ok ? '成功' : '超时'}  落点 ${s.landed}s  偏差 ${drift.toFixed(2)}s  ${s.ms}ms`);
    }
    const allOk = seekResult.every(s => s.ok);
    check(allOk, '全部 6 次 seek 均触发 seeked 事件',
          `${seekResult.filter(s => s.ok).length}/6`);
    // 落点必须接近目标：seeked 触发但位置没动是典型的假通过
    const accurate = seekResult.every(s => Math.abs(s.landed - s.target) < 1.5);
    check(accurate, '每次 seek 的落点都接近目标（<1.5s）',
          `最大偏差 ${Math.max(...seekResult.map(s =>
            Math.abs(s.landed - s.target))).toFixed(2)}s`);
    // readyState >= 2 表示当前位置有可播放数据，能排除「跳过去但没数据」
    const hasData = seekResult.every(s => s.readyState >= 2);
    check(hasData, 'seek 后均有可播放数据（readyState>=2）',
          `最小 readyState ${Math.min(...seekResult.map(s => s.readyState))}`);
  }

  // ---------- 阶段 4：能否真正解码出画面 ----------
  // 这是排除「假通过」的关键：把当前帧画到 canvas 上取像素。
  // 若返回全黑或纯色，说明只是元数据可读、实际没解码出画面。
  if (meta.ok) {
    console.log('\n=== 阶段 4：真实解码验证（canvas 取帧）===');
    const frame = await cdp.eval(`(async () => {
      const v = document.getElementById('v');
      const shots = [];
      for (const t of [5, 30, 50]) {
        await new Promise(res => {
          let done = false;
          const h = () => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(); } };
          v.addEventListener('seeked', h);
          v.currentTime = t;
          setTimeout(() => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(); } }, 8000);
        });
        await new Promise(r => setTimeout(r, 400)); // 等帧渲染
        const c = document.createElement('canvas');
        c.width = v.videoWidth; c.height = v.videoHeight;
        const ctx = c.getContext('2d');
        ctx.drawImage(v, 0, 0);
        // 取像素可能因跨源污染失败。不能让它中断整个验证——
        // 其余阶段的结论仍然有效，所以捕获后如实标记。
        let d;
        try {
          d = ctx.getImageData(0, 0, c.width, c.height).data;
        } catch (e) {
          shots.push({ t, at: +v.currentTime.toFixed(2), tainted: true,
                       err: String(e.name || e) });
          continue;
        }
        // 统计不同颜色数与平均亮度，判断是否真有画面
        const seen = new Set();
        let sum = 0;
        for (let i = 0; i < d.length; i += 4 * 97) { // 稀疏采样
          seen.add((d[i] << 16) | (d[i+1] << 8) | d[i+2]);
          sum += (d[i] + d[i+1] + d[i+2]) / 3;
        }
        const n = Math.ceil(d.length / (4 * 97));
        shots.push({ t, at: +v.currentTime.toFixed(2),
          colors: seen.size, brightness: Math.round(sum / n) });
      }
      return shots;
    })()`);

    const tainted = frame.filter(s => s.tainted);
    if (tainted.length > 0) {
      console.log(`  ⚠ canvas 被跨源污染，无法取像素：${tainted[0].err}`);
      console.log('    需要服务端回 Access-Control-Allow-Origin');
      console.log('    且 <video> 带 crossorigin="anonymous"');
      check(false, 'canvas 可取帧（跨源配置正确）', tainted[0].err);
    } else {
      for (const s of frame) {
        console.log(`  t=${String(s.t).padStart(2)}s 落点 ${s.at}s  ` +
                    `不同颜色 ${s.colors}  平均亮度 ${s.brightness}`);
      }
      check(frame.every(s => s.colors > 20), '各位置都解码出有内容的画面（颜色数>20）',
            `最少 ${Math.min(...frame.map(s => s.colors))} 种`);
      // 不同时间点的画面应当不同——否则可能是同一帧反复显示
      const sigs = new Set(frame.map(s => `${s.colors}:${s.brightness}`));
      check(sigs.size > 1, '不同时间点画面确实不同（排除卡帧）',
            `${sigs.size} 种特征`);
    }
  }

  // ---------- 汇总网络与缓存观察 ----------
  console.log('\n=== 网络观察 ===');
  const mine = responses.filter(r => r.url.includes('omystream'));
  const p206 = mine.filter(r => r.status === 206);
  const p200 = mine.filter(r => r.status === 200);
  const p416 = mine.filter(r => r.status === 416);
  console.log(`  omystream 响应总数 ${mine.length}（206: ${p206.length}, ` +
              `200: ${p200.length}, 416: ${p416.length}）`);
  const cached = mine.filter(r => r.fromCache);
  check(cached.length === 0, '没有任何响应来自磁盘缓存（S5 佐证）',
        `fromDiskCache=${cached.length}`);

  // 按需解密不能退化成「几乎全量」：WebView 的 seek 请求是开放结尾
  // （bytes=起点-），若服务端老实返回到文件末尾，跳到 35% 就要解密
  // 65% 的文件，大文件上会直接卡死。服务端必须限制单次响应上限。
  // 这里用 Content-Range 的实际返回区间来验证，而不是信任请求头。
  const spans = [];
  for (const r of p206) {
    // responses 里存的是原始 headers 对象，键名大小写不固定
    let cr = null;
    for (const kv of Object.entries(r.headers || {})) {
      if (kv[0].toLowerCase() === 'content-range') { cr = kv[1]; break; }
    }
    const m = /bytes (\d+)-(\d+)\/(\d+)/.exec(cr || '');
    if (m) spans.push({ span: +m[2] - +m[1] + 1, total: +m[3] });
  }
  if (spans.length) {
    const maxSpan = Math.max(...spans.map(s => s.span));
    const total = spans[0].total;
    const ratio = maxSpan / total;
    console.log(`  单次 206 最大返回 ${maxSpan} B，占全文 ${(ratio * 100).toFixed(1)}%`);
    check(ratio < 0.5, '单次响应未退化成近全量（<50% 全文）',
          `最大 ${(ratio * 100).toFixed(1)}%`);
  }
  if (failures.length) {
    console.log('  加载失败事件:');
    for (const f of failures.slice(0, 10)) {
      console.log(`    ${f.type} ${f.text} ${f.blocked || ''}`);
    }
  }
  if (consoleMsgs.length) {
    console.log('  控制台消息:');
    for (const m of consoleMsgs.slice(0, 15)) console.log('    ' + m);
  }

  console.log('\n' + '='.repeat(64));
  console.log(`结果: ${pass.length} 项通过, ${fail.length} 项失败`);
  if (fail.length) {
    console.log('\n失败明细:');
    for (const f of fail) console.log('  - ' + f);
  }
  console.log('='.repeat(64));

  cdp.close();
  process.exit(fail.length ? 1 : 0);
};

main().catch(e => {
  console.error('驱动失败:', e.message);
  process.exit(2);
});
