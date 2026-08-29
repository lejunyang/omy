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
 * WebSocket 为什么手写而不用 Node 内置：
 * Chromium 会校验握手的 Origin 头，Node 内置 WebSocket 不允许省略它，
 * 不在 --remote-allow-origins 列表时直接 403。手写握手才能完全不发 Origin。
 *
 * 用法: node spikes/cdp-drive.mjs [port]
 */

import net from 'node:net';
import crypto from 'node:crypto';
import { EventEmitter } from 'node:events';

const PORT = process.argv[2] || '9222';
const BASE = `http://127.0.0.1:${PORT}`;

const sleep = ms => new Promise(r => setTimeout(r, ms));

/** 最小 WebSocket 客户端：只支持文本帧，够 CDP 用。 */
class MiniWs extends EventEmitter {
  constructor(url) {
    super();
    const u = new URL(url);
    this.buf = Buffer.alloc(0);
    this.frag = [];
    this.sock = net.connect(
      { host: u.hostname, port: Number(u.port) || 80 },
      () => {
        const key = crypto.randomBytes(16).toString('base64');
        this.expectAccept = crypto
          .createHash('sha1')
          .update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
          .digest('base64');
        // 刻意不发 Origin 头
        this.sock.write(
          `GET ${u.pathname}${u.search} HTTP/1.1\r\n` +
          `Host: ${u.host}\r\n` +
          'Upgrade: websocket\r\n' +
          'Connection: Upgrade\r\n' +
          `Sec-WebSocket-Key: ${key}\r\n` +
          'Sec-WebSocket-Version: 13\r\n\r\n'
        );
      }
    );
    this.handshook = false;
    this.sock.on('data', d => this.onData(d));
    this.sock.on('error', e => this.emit('error', e));
    this.sock.on('close', () => this.emit('close'));
  }

  onData(chunk) {
    this.buf = Buffer.concat([this.buf, chunk]);
    if (!this.handshook) {
      const idx = this.buf.indexOf('\r\n\r\n');
      if (idx < 0) return;
      const head = this.buf.subarray(0, idx).toString('latin1');
      const status = head.split('\r\n')[0];
      if (!/ 101 /.test(status)) {
        this.emit('error', new Error(`握手失败: ${status}`));
        this.sock.destroy();
        return;
      }
      this.handshook = true;
      this.buf = this.buf.subarray(idx + 4);
      this.emit('open');
    }
    // 解析帧
    for (;;) {
      if (this.buf.length < 2) return;
      const b0 = this.buf[0], b1 = this.buf[1];
      const fin = (b0 & 0x80) !== 0;
      const op = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f;
      let off = 2;
      if (len === 126) {
        if (this.buf.length < off + 2) return;
        len = this.buf.readUInt16BE(off); off += 2;
      } else if (len === 127) {
        if (this.buf.length < off + 8) return;
        len = Number(this.buf.readBigUInt64BE(off)); off += 8;
      }
      if (masked) off += 4; // 服务端不该掩码，容错跳过
      if (this.buf.length < off + len) return;
      const payload = this.buf.subarray(off, off + len);
      this.buf = this.buf.subarray(off + len);

      if (op === 0x8) { this.sock.end(); this.emit('close'); return; }
      if (op === 0x9) { this.pong(payload); continue; }
      if (op === 0xa) continue;
      this.frag.push(payload);
      if (fin) {
        const full = Buffer.concat(this.frag).toString('utf8');
        this.frag = [];
        this.emit('message', full);
      }
    }
  }

  frame(op, data) {
    const payload = Buffer.from(data, 'utf8');
    const mask = crypto.randomBytes(4);
    const n = payload.length;
    let head;
    if (n < 126) {
      head = Buffer.alloc(2); head[1] = 0x80 | n;
    } else if (n < 65536) {
      head = Buffer.alloc(4); head[1] = 0x80 | 126; head.writeUInt16BE(n, 2);
    } else {
      head = Buffer.alloc(10); head[1] = 0x80 | 127;
      head.writeBigUInt64BE(BigInt(n), 2);
    }
    head[0] = 0x80 | op;
    const masked = Buffer.allocUnsafe(n);
    for (let i = 0; i < n; i++) masked[i] = payload[i] ^ mask[i & 3];
    return Buffer.concat([head, mask, masked]);
  }
  send(text) { this.sock.write(this.frame(0x1, text)); }
  pong(p) { this.sock.write(this.frame(0xa, p.toString('utf8'))); }
  close() { try { this.sock.end(); } catch {} }
}

async function findTarget(retries = 40) {
  for (let i = 0; i < retries; i++) {
    try {
      const r = await fetch(`${BASE}/json/list`);
      const list = await r.json();
      const page = list.find(t => t.type === 'page' && t.webSocketDebuggerUrl);
      if (page) return page;
    } catch { /* 端口还没起来 */ }
    await sleep(500);
  }
  throw new Error(`无法连接到 CDP ${BASE}`);
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.listeners = [];
    ws.on('message', data => {
      let msg;
      try { msg = JSON.parse(data); } catch { return; }
      if (msg.id !== undefined) {
        const p = this.pending.get(msg.id);
        if (p) {
          this.pending.delete(msg.id);
          msg.error ? p.reject(new Error(JSON.stringify(msg.error))) : p.resolve(msg.result);
        }
      } else {
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
      }, 60000);
    });
  }
  on(fn) { this.listeners.push(fn); }
  async eval(expr, awaitPromise = true) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr, returnByValue: true, awaitPromise,
    });
    if (r.exceptionDetails) {
      throw new Error('页面异常: ' +
        (r.exceptionDetails.exception?.description || r.exceptionDetails.text));
    }
    return r.result?.value;
  }
  close() { this.ws.close(); }
}

const pass = [], fail = [];
function check(cond, label, detail = '') {
  (cond ? pass : fail).push(label);
  console.log(`  ${cond ? 'PASS' : 'FAIL'}  ${label}${detail ? '  ' + detail : ''}`);
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

  const responses = [], failures = [], consoleMsgs = [];
  cdp.on(msg => {
    if (msg.method === 'Network.responseReceived') {
      const r = msg.params.response;
      responses.push({ url: r.url, status: r.status, mime: r.mimeType,
                       fromCache: r.fromDiskCache, headers: r.headers });
    }
    if (msg.method === 'Network.loadingFailed') {
      failures.push({ text: msg.params.errorText, type: msg.params.type,
                      blocked: msg.params.blockedReason });
    }
    if (msg.method === 'Log.entryAdded') {
      consoleMsgs.push(`[${msg.params.entry.level}] ${msg.params.entry.text}`);
    }
  });

  const URL_V = 'http://omystream.localhost/video';

  // ---------- 阶段 1：协议层（绕开 <video>）----------
  console.log('\n=== 阶段 1：协议层（fetch 直连）===');
  const probe = await cdp.eval(`(async () => {
    const cases = [
      ['无 Range', null], ['bytes=0-1023', 'bytes=0-1023'],
      ['bytes=1024-2047', 'bytes=1024-2047'], ['bytes=-512', 'bytes=-512'],
      ['越界 bytes=999999999-', 'bytes=999999999-'],
    ];
    const out = [];
    for (const [label, range] of cases) {
      try {
        const r = await fetch(${JSON.stringify(URL_V)},
          range ? { headers: { Range: range } } : {});
        const b = await r.arrayBuffer();
        out.push({ label, ok: true, status: r.status, len: b.byteLength,
          contentRange: r.headers.get('content-range'),
          acceptRanges: r.headers.get('accept-ranges'),
          cacheControl: r.headers.get('cache-control'),
          contentType: r.headers.get('content-type') });
      } catch (e) { out.push({ label, ok: false, error: String(e) }); }
    }
    return out;
  })()`);

  for (const p of probe) {
    if (!p.ok) { console.log(`  ${p.label}: 请求失败 ${p.error}`); continue; }
    console.log(`  ${p.label.padEnd(22)} HTTP ${p.status}  ${String(p.len).padStart(7)} B  ` +
                `CR=${p.contentRange || '-'}`);
  }
  const g = l => probe.find(p => p.label === l);
  const full = g('无 Range'), r1 = g('bytes=0-1023'),
        r2 = g('bytes=1024-2047'), suf = g('bytes=-512'),
        oob = g('越界 bytes=999999999-');

  check(full?.ok && full.status === 200, '无 Range 返回 200');
  check(r1?.ok && r1.status === 206 && r1.len === 1024,
        'bytes=0-1023 → 206 且恰好 1024 字节',
        r1?.ok ? `实际 ${r1.status}/${r1.len}B` : '');
  check(r2?.ok && r2.status === 206 && r2.len === 1024,
        'bytes=1024-2047 → 206 且恰好 1024 字节',
        r2?.ok ? `实际 ${r2.status}/${r2.len}B` : '');
  check(suf?.ok && suf.status === 206 && suf.len === 512,
        'bytes=-512 → 最后 512 字节',
        suf?.ok ? `实际 ${suf.status}/${suf.len}B` : '');
  check(oob?.ok && oob.status === 416, '越界 Range → 416',
        oob?.ok ? `实际 ${oob.status}` : '');
  check(!!r1?.acceptRanges, '响应含 Accept-Ranges', r1?.acceptRanges || '缺失');
  check(!!r1?.cacheControl && r1.cacheControl.includes('no-store'),
        '响应含 Cache-Control: no-store（S5 前置）', r1?.cacheControl || '缺失');

  // ---------- 阶段 2：<video> 元数据 ----------
  console.log('\n=== 阶段 2：<video> 元数据 ===');
  const meta = await cdp.eval(`(async () => {
    const v = document.getElementById('v');
    v.src = ${JSON.stringify(URL_V)};
    v.load();
    return await new Promise(resolve => {
      let done = false;
      const fin = o => { if (!done) { done = true; resolve(o); } };
      v.addEventListener('loadedmetadata', () => fin({ ok: true,
        duration: v.duration, w: v.videoWidth, h: v.videoHeight }), { once: true });
      v.addEventListener('error', () => fin({ ok: false,
        code: v.error?.code, message: v.error?.message || '' }), { once: true });
      setTimeout(() => fin({ ok: false, code: v.error?.code ?? null,
        message: '超时', networkState: v.networkState,
        readyState: v.readyState }), 20000);
    });
  })()`);

  if (!meta.ok) {
    console.log(`  FAIL  <video> 加载失败 code=${meta.code} ${meta.message} ` +
                `networkState=${meta.networkState ?? '-'} readyState=${meta.readyState ?? '-'}`);
    fail.push('<video> 加载元数据');
  } else {
    check(true, '<video> 加载元数据成功',
          `${meta.duration?.toFixed(2)}s ${meta.w}x${meta.h}`);
    check(Math.abs(meta.duration - 60) < 1, '时长约 60s',
          `${meta.duration?.toFixed(2)}s`);
    check(meta.w === 640 && meta.h === 360, '分辨率 640x360',
          `${meta.w}x${meta.h}`);
  }

  // ---------- 阶段 3：乱序 seek ----------
  if (meta.ok) {
    console.log('\n=== 阶段 3：乱序 seek（S1 核心）===');
    const seeks = await cdp.eval(`(async () => {
      const v = document.getElementById('v');
      const fracs = [0.85, 0.15, 0.60, 0.05, 0.95, 0.35];
      const out = [];
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
            v.removeEventListener('seeked', h); res(false); } }, 10000);
        });
        out.push({ frac: f, target, ok, landed: +v.currentTime.toFixed(2),
                   ms: Math.round(performance.now() - t0), rs: v.readyState });
      }
      return out;
    })()`);

    for (const s of seeks) {
      console.log(`  ${String(Math.round(s.frac * 100)).padStart(3)}% → ` +
        `${String(s.target).padStart(6)}s  ${s.ok ? '成功' : '超时'}  ` +
        `落点 ${String(s.landed).padStart(6)}s  偏差 ` +
        `${Math.abs(s.landed - s.target).toFixed(2)}s  ${s.ms}ms  rs=${s.rs}`);
    }
    check(seeks.every(s => s.ok), '6 次 seek 全部触发 seeked',
          `${seeks.filter(s => s.ok).length}/6`);
    check(seeks.every(s => Math.abs(s.landed - s.target) < 1.5),
          '落点均接近目标（<1.5s）',
          `最大偏差 ${Math.max(...seeks.map(s => Math.abs(s.landed - s.target))).toFixed(2)}s`);
    check(seeks.every(s => s.rs >= 2), 'seek 后均有可播放数据（readyState>=2）',
          `最小 ${Math.min(...seeks.map(s => s.rs))}`);
  }

  // ---------- 阶段 4：真实解码（canvas 取帧）----------
  if (meta.ok) {
    console.log('\n=== 阶段 4：真实解码验证（canvas 取帧）===');
    const frames = await cdp.eval(`(async () => {
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
            v.removeEventListener('seeked', h); res(); } }, 10000);
        });
        await new Promise(r => setTimeout(r, 500));
        try {
          const c = document.createElement('canvas');
          c.width = v.videoWidth; c.height = v.videoHeight;
          c.getContext('2d').drawImage(v, 0, 0);
          const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
          const seen = new Set(); let sum = 0, n = 0;
          for (let i = 0; i < d.length; i += 4 * 97) {
            seen.add((d[i] << 16) | (d[i+1] << 8) | d[i+2]);
            sum += (d[i] + d[i+1] + d[i+2]) / 3; n++;
          }
          shots.push({ t, at: +v.currentTime.toFixed(2), ok: true,
                       colors: seen.size, brightness: Math.round(sum / n) });
        } catch (e) {
          // 跨源污染会在这里抛 SecurityError——这本身是重要发现
          shots.push({ t, at: +v.currentTime.toFixed(2), ok: false,
                       error: e.name + ': ' + e.message });
        }
      }
      return shots;
    })()`);

    for (const s of frames) {
      if (!s.ok) { console.log(`  t=${s.t}s 取帧失败 ${s.error}`); continue; }
      console.log(`  t=${String(s.t).padStart(2)}s 落点 ${s.at}s  ` +
                  `不同颜色 ${s.colors}  平均亮度 ${s.brightness}`);
    }
    const decoded = frames.filter(s => s.ok);
    if (decoded.length === frames.length) {
      check(decoded.every(s => s.colors > 20),
            '各位置都解码出有内容的画面（颜色数>20）',
            `最少 ${Math.min(...decoded.map(s => s.colors))} 种`);
      check(new Set(decoded.map(s => `${s.colors}:${s.brightness}`)).size > 1,
            '不同时间点画面确实不同（排除卡帧）');
    } else {
      console.log('  注意：canvas 取帧受限，改由 readyState 与 seek 落点判断');
    }
  }

  // ---------- 网络观察 ----------
  console.log('\n=== 网络观察 ===');
  const mine = responses.filter(r => r.url.includes('omystream'));
  const c206 = mine.filter(r => r.status === 206).length;
  const c200 = mine.filter(r => r.status === 200).length;
  const c416 = mine.filter(r => r.status === 416).length;
  console.log(`  omystream 响应 ${mine.length} 条（206:${c206} 200:${c200} 416:${c416}）`);
  const cached = mine.filter(r => r.fromCache);
  check(cached.length === 0, '无响应来自磁盘缓存（S5 佐证）',
        `fromDiskCache=${cached.length}`);
  if (failures.length) {
    console.log('  加载失败事件:');
    for (const f of failures.slice(0, 10))
      console.log(`    ${f.type} ${f.text} ${f.blocked || ''}`);
  }
  if (consoleMsgs.length) {
    console.log('  控制台:');
    for (const m of consoleMsgs.slice(0, 15)) console.log('    ' + m);
  }

  console.log('\n' + '='.repeat(64));
  console.log(`结果: ${pass.length} 通过, ${fail.length} 失败`);
  if (fail.length) { console.log('失败项:'); for (const f of fail) console.log('  - ' + f); }
  console.log('='.repeat(64));
  cdp.close();
  process.exit(fail.length ? 1 : 0);
};

main().catch(e => { console.error('驱动失败:', e.message); process.exit(2); });
