#!/usr/bin/env node
/**
 * 通过 CDP 驱动 omy GUI 做端到端验证。
 *
 * 这是这一轮唯一能证明「GUI 真的能用」的东西——
 * 编译通过、单测通过都不代表 WebView 里真的放得出画面。
 *
 * 验证策略上刻意做了两件事：
 *
 * 1. **从用户看得到的层验证**，而不是查内部状态。
 *    判断视频能播，看的是 `<video>` 的 readyState 与 canvas 取到的
 *    真实像素，而不是「协议返回了 200」。
 *
 * 2. **主动找反证**。锁定态那一组断言不是「检查有没有显示锁图标」，
 *    而是把整个 DOM 的文本抓下来，搜索绝不该出现的字符串。
 *    前者容易假通过（图标画对了但文件名也还在），后者不会。
 *
 * 用法: node spikes/cdp-gui.mjs [port]
 */

import net from 'node:net';
import crypto from 'node:crypto';
import { EventEmitter } from 'node:events';

const PORT = process.argv[2] || '9444';
const BASE = `http://127.0.0.1:${PORT}`;
const VAULT_DIR = process.argv[3] || '';
const PASSWORD = 'vault-test-password';
const SENTINEL = 'OMY-GUI-SENTINEL-7F3A';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 最小 WebSocket 客户端：只支持文本帧，够 CDP 用。
 *
 * 不用 Node 内置 WebSocket：Chromium 会校验握手的 Origin 头，
 * 而内置实现不允许省略它，不在 --remote-allow-origins 列表时直接 403。
 * 手写握手才能完全不发 Origin。
 */
class MiniWs extends EventEmitter {
  constructor(url) {
    super();
    const u = new URL(url);
    this.buf = Buffer.alloc(0);
    this.frag = [];
    this.handshook = false;
    this.sock = net.connect({ host: u.hostname, port: Number(u.port) || 80 }, () => {
      const key = crypto.randomBytes(16).toString('base64');
      this.sock.write(
        `GET ${u.pathname}${u.search} HTTP/1.1\r\n` +
          `Host: ${u.host}\r\n` +
          'Upgrade: websocket\r\n' +
          'Connection: Upgrade\r\n' +
          `Sec-WebSocket-Key: ${key}\r\n` +
          'Sec-WebSocket-Version: 13\r\n\r\n',
      );
    });
    this.sock.on('data', (d) => this.onData(d));
    this.sock.on('error', (e) => this.emit('error', e));
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
    for (;;) {
      if (this.buf.length < 2) return;
      const b0 = this.buf[0];
      const b1 = this.buf[1];
      const fin = (b0 & 0x80) !== 0;
      const op = b0 & 0x0f;
      const masked = (b1 & 0x80) !== 0;
      let len = b1 & 0x7f;
      let off = 2;
      if (len === 126) {
        if (this.buf.length < off + 2) return;
        len = this.buf.readUInt16BE(off);
        off += 2;
      } else if (len === 127) {
        if (this.buf.length < off + 8) return;
        len = Number(this.buf.readBigUInt64BE(off));
        off += 8;
      }
      if (masked) off += 4;
      if (this.buf.length < off + len) return;
      const payload = this.buf.subarray(off, off + len);
      this.buf = this.buf.subarray(off + len);
      if (op === 0x8) {
        this.sock.end();
        this.emit('close');
        return;
      }
      if (op === 0x9) {
        this.pong(payload);
        continue;
      }
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
      head = Buffer.alloc(2);
      head[1] = 0x80 | n;
    } else if (n < 65536) {
      head = Buffer.alloc(4);
      head[1] = 0x80 | 126;
      head.writeUInt16BE(n, 2);
    } else {
      head = Buffer.alloc(10);
      head[1] = 0x80 | 127;
      head.writeBigUInt64BE(BigInt(n), 2);
    }
    head[0] = 0x80 | op;
    const m = Buffer.allocUnsafe(n);
    for (let i = 0; i < n; i++) m[i] = payload[i] ^ mask[i & 3];
    return Buffer.concat([head, mask, m]);
  }

  send(text) {
    this.sock.write(this.frame(0x1, text));
  }
  pong(p) {
    this.sock.write(this.frame(0xa, p.toString('utf8')));
  }
  close() {
    try {
      this.sock.end();
    } catch {
      /* 已关闭 */
    }
  }
}

async function findTarget(retries = 40) {
  for (let i = 0; i < retries; i++) {
    try {
      const r = await fetch(`${BASE}/json/list`);
      const list = await r.json();
      const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
      if (page) return page;
    } catch {
      /* 端口还没起来 */
    }
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
    ws.on('message', (data) => {
      let msg;
      try {
        msg = JSON.parse(data);
      } catch {
        return;
      }
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
      ws.once('error', (e) => rej(new Error('WebSocket 连接失败: ' + e.message)));
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
      }, 90000);
    });
  }
  on(fn) {
    this.listeners.push(fn);
  }
  async eval(expr, awaitPromise = true) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise,
    });
    if (r.exceptionDetails) {
      throw new Error(
        '页面异常: ' + (r.exceptionDetails.exception?.description || r.exceptionDetails.text),
      );
    }
    return r.result?.value;
  }
  close() {
    this.ws.close();
  }
}

const pass = [];
const fail = [];
function check(cond, label, detail = '') {
  (cond ? pass : fail).push(label);
  console.log(`  ${cond ? 'PASS' : 'FAIL'}  ${label}${detail ? '  ' + detail : ''}`);
  return cond;
}

const main = async () => {
  if (!VAULT_DIR) {
    console.error('用法: node cdp-gui.mjs <port> <vault-dir>');
    process.exit(2);
  }
  console.log(`连接 CDP ${BASE} ...`);
  const target = await findTarget();
  console.log(`目标: ${target.title}  ${target.url}\n`);

  const cdp = await Cdp.connect(target.webSocketDebuggerUrl);
  await cdp.send('Runtime.enable');
  await cdp.send('Network.enable');
  await cdp.send('Log.enable');

  const responses = [];
  const consoleMsgs = [];
  cdp.on((msg) => {
    if (msg.method === 'Network.responseReceived') {
      const r = msg.params.response;
      responses.push({ url: r.url, status: r.status, fromCache: r.fromDiskCache });
    }
    if (msg.method === 'Log.entryAdded') {
      consoleMsgs.push(`[${msg.params.entry.level}] ${msg.params.entry.text}`);
    }
  });

  // ---------- 阶段 1：启动与解锁界面 ----------
  console.log('=== 阶段 1：启动状态 ===');
  const boot = await cdp.eval(`(() => ({
    hasUnlockBox: !!document.querySelector('.unlock-box'),
    hasTopbar: !!document.querySelector('.topbar'),
    lang: document.documentElement.lang,
    theme: document.documentElement.dataset.theme || 'dark',
    text: document.body.innerText.slice(0, 200),
  }))()`);
  check(boot.hasUnlockBox, '启动后显示解锁界面');
  check(!boot.hasTopbar, '未解锁时不渲染主界面（不泄露任何文件信息）');
  check(!!boot.lang, '语言已确定', boot.lang);

  // 确认原生选择器命令**真的注册了**。
  //
  // 阶段 2 会把 pick_folder 拦在 IPC 层（原生窗口没法自动点），
  // 拦掉之后就再也测不到这个命令是否存在了——万一漏在
  // invoke_handler 里注册，测试全绿而用户点「浏览」毫无反应。
  //
  // 手段：调一个确定不存在的命令，比对两者的错误。Tauri 对未注册
  // 命令回的是固定文案，已注册的命令则会因为参数缺失或真的弹窗
  // 而给出不同结果。这里只发不等待，避免真把窗口弹出来。
  const cmdReg = await cdp.eval(`(async () => {
    const probe = async (cmd) => {
      try {
        // 故意不传参数：已注册的命令会报参数错误，
        // 未注册的命令报 "not found"，两者可区分
        await window.__TAURI_INTERNALS__.invoke(cmd, {});
        return 'resolved';
      } catch (e) { return String(e); }
    };
    const missing = await probe('definitely_not_a_real_command_xyz');
    return { missing };
  })()`);
  const notFoundPat = /not found|unknown command/i;
  check(notFoundPat.test(cmdReg.missing),
    '未注册命令会报 not found（作为下一条的判据基准）',
    String(cmdReg.missing).slice(0, 60));

  // ---------- 阶段 2：解锁 ----------
  console.log('\n=== 阶段 2：选目录并解锁 ===');
  // 原生目录选择器是 OS 窗口，CDP 点不到它。
  //
  // 试过在 JS 侧拦 __TAURI_INTERNALS__.invoke：不行。Tauri 把它
  // 定义成 writable:false + configurable:false，直接赋值被静默忽略
  // （非严格模式不报错，只是没生效），defineProperty 则抛
  // "Cannot redefine property"。这是 Tauri 有意的安全设计。
  //
  // 因此改由应用侧提供旁路：启动时设 OMY_GUI_PICK_FOLDER，
  // pick_folder 命令直接返回该路径而不弹窗。按钮点击、状态更新、
  // 错误处理、后续的 vault_params_of 调用全都走真实代码。
  const pickOk = await cdp.eval(`(async () => {
    try {
      const v = await window.__TAURI_INTERNALS__.invoke('pick_folder', { title: 't' });
      return { got: v };
    } catch (e) { return { err: String(e) }; }
  })()`);
  check(pickOk.got === VAULT_DIR, 'pick_folder 命令可用且返回预设目录',
    pickOk.got ? '' : JSON.stringify(pickOk).slice(0, 90));

  const picked = await cdp.eval(`(async () => {
    document.getElementById('btn-browse').click();
    // 等后端读完文件头
    for (let i = 0; i < 40; i++) {
      await new Promise(r => setTimeout(r, 100));
      const btn = document.getElementById('btn-unlock');
      if (btn && !btn.disabled) return { ok: true, tries: i };
      const err = document.querySelector('.errbox');
      if (err) return { ok: false, error: err.textContent.trim() };
    }
    return { ok: false, error: '超时' };
  })()`);
  check(picked.ok, '选目录后读到解锁参数', picked.ok ? '' : picked.error);

  if (!picked.ok) {
    console.log('\n无法继续，解锁参数未就绪');
    cdp.close();
    process.exit(1);
  }

  const unlocked = await cdp.eval(`(async () => {
    document.getElementById('u-pass').value = ${JSON.stringify(PASSWORD)};
    document.getElementById('unlock-form')
      .dispatchEvent(new Event('submit', { cancelable: true }));
    for (let i = 0; i < 150; i++) {
      await new Promise(r => setTimeout(r, 100));
      if (document.querySelector('.topbar')) {
        return { ok: true, ms: i * 100 };
      }
      const err = document.querySelector('.errbox');
      if (err) return { ok: false, error: err.textContent.trim() };
    }
    return { ok: false, error: '超时' };
  })()`);
  check(unlocked.ok, '密码正确时能解锁并进入主界面',
        unlocked.ok ? `${unlocked.ms}ms` : unlocked.error);

  if (!unlocked.ok) {
    cdp.close();
    process.exit(1);
  }

  // 等元信息补齐
  await sleep(2500);

  // ---------- 阶段 3：文件列表 ----------
  console.log('\n=== 阶段 3：文件列表 ===');
  const listing = await cdp.eval(`(() => {
    const cards = [...document.querySelectorAll('.card')];
    return {
      total: cards.length,
      locked: cards.filter(c => c.dataset.locked === '1').length,
      unlocked: cards.filter(c => c.dataset.locked === '0').length,
      names: cards.filter(c => c.dataset.locked === '0')
                  .map(c => c.querySelector('.name')?.textContent.trim()),
      metas: cards.filter(c => c.dataset.locked === '0')
                  .map(c => c.querySelector('.meta')?.textContent.trim()),
      thumbs: [...document.querySelectorAll('.card img')].length,
      bodyText: document.body.innerText,
    };
  })()`);

  console.log(`  卡片 ${listing.total} 张（解锁 ${listing.unlocked} / 锁定 ${listing.locked}）`);
  for (let i = 0; i < listing.names.length; i++) {
    console.log(`    ${listing.names[i]}  —  ${listing.metas[i]}`);
  }

  check(listing.total === 5, '扫描到全部 5 个 .omy 文件', `实得 ${listing.total}`);
  check(listing.unlocked === 4, '4 个文件被当前密码解开', `实得 ${listing.unlocked}`);
  check(listing.locked === 1, '1 个文件保持锁定（用了其他密码）', `实得 ${listing.locked}`);
  check(
    listing.names.includes('demo-video.mp4'),
    '显示的是解密后的真实文件名',
    listing.names.join(', '),
  );

  // ---------- 阶段 4：锁定态不泄露信息（主动找反证）----------
  console.log('\n=== 阶段 4：锁定态信息隐藏 ===');
  // 不检查「有没有画锁图标」——那容易假通过。
  // 直接在整个可见文本里搜绝不该出现的字符串。
  const leak = await cdp.eval(`(() => {
    const t = document.body.innerText;
    return {
      hasSecretName: t.includes('MUST-NOT-APPEAR'),
      hasOtherPwName: t.includes('other-password'),
      lockedPlaceholders: [...document.querySelectorAll('.card[data-locked="1"] .name')]
        .map(e => e.textContent.trim()),
      lockedMetas: [...document.querySelectorAll('.card[data-locked="1"] .meta')]
        .map(e => e.textContent.trim()),
      lockedThumbImgs: document.querySelectorAll('.card[data-locked="1"] img').length,
    };
  })()`);

  check(!leak.hasSecretName, '锁定文件的原始文件名未出现在界面上');
  check(!leak.hasOtherPwName, '锁定文件的磁盘文件名也未出现');
  check(
    leak.lockedMetas.every((m) => !/\d+(\.\d+)?\s*(B|KB|MB|GB)/.test(m)),
    '锁定文件不显示文件大小（大小+数量可推断库规模）',
    leak.lockedMetas.join(' | '),
  );
  check(leak.lockedThumbImgs === 0, '锁定文件不加载缩略图', `实得 ${leak.lockedThumbImgs} 个`);

  // ---------- 阶段 5：协议层 ----------
  console.log('\n=== 阶段 5：omystream 协议 ===');
  // 前缀由应用按平台决定（Windows 上是 http://omystream.localhost），
  // 从页面里取而不是自己写死——否则这个脚本会在换平台时静默失效
  const streamBase = await cdp.eval(`(async () => {
    return await window.__TAURI_INTERNALS__.invoke('stream_base', {});
  })()`);
  console.log(`  协议前缀: ${streamBase}`);

  const vidId = await cdp.eval(`(() => {
    const c = [...document.querySelectorAll('.card[data-locked="0"]')]
      .find(c => c.querySelector('.name')?.textContent.includes('demo-video'));
    return c ? c.dataset.id : null;
  })()`);
  check(!!vidId, '取到视频文件的 id');

  const proto = await cdp.eval(`(async () => {
    const url = ${JSON.stringify(streamBase)} + '/file/' + ${JSON.stringify(vidId)};
    const cases = [
      ['无 Range', null], ['bytes=0-1023', 'bytes=0-1023'],
      ['bytes=1024-2047', 'bytes=1024-2047'], ['bytes=-512', 'bytes=-512'],
      ['越界', 'bytes=999999999-'], ['开放结尾', 'bytes=1048576-'],
    ];
    const out = [];
    for (const [label, range] of cases) {
      try {
        const r = await fetch(url, range ? { headers: { Range: range } } : {});
        const b = await r.arrayBuffer();
        out.push({ label, ok: true, status: r.status, len: b.byteLength,
          cr: r.headers.get('content-range'),
          ar: r.headers.get('accept-ranges'),
          cc: r.headers.get('cache-control'),
          ct: r.headers.get('content-type') });
      } catch (e) { out.push({ label, ok: false, error: String(e) }); }
    }
    return out;
  })()`);

  for (const p of proto) {
    if (!p.ok) {
      console.log(`  ${p.label}: 失败 ${p.error}`);
      continue;
    }
    console.log(
      `  ${p.label.padEnd(14)} HTTP ${p.status}  ${String(p.len).padStart(8)} B  CR=${p.cr || '-'}`,
    );
  }
  const g = (l) => proto.find((p) => p.label === l);
  check(g('无 Range')?.status === 200, '无 Range → 200');
  check(
    g('bytes=0-1023')?.status === 206 && g('bytes=0-1023')?.len === 1024,
    'bytes=0-1023 → 206 且恰好 1024 字节',
  );
  check(g('bytes=-512')?.len === 512, 'suffix Range 返回最后 512 字节');
  check(g('越界')?.status === 416, '越界 Range → 416（而非钳到末尾）');
  check(
    g('开放结尾')?.len === 2 * 1024 * 1024,
    '开放结尾的 Range 被截断到 2 MiB 上限',
    `实得 ${g('开放结尾')?.len} B`,
  );
  check(
    g('bytes=0-1023')?.cc?.includes('no-store'),
    '响应带 no-store（明文不进磁盘缓存）',
    g('bytes=0-1023')?.cc || '缺失',
  );
  check(g('bytes=0-1023')?.ct === 'video/mp4', 'MIME 正确', g('bytes=0-1023')?.ct);

  // 锁定文件必须拒绝
  const forbidden = await cdp.eval(`(async () => {
    const card = [...document.querySelectorAll('.card[data-locked="1"]')][0];
    // 锁定卡片没有 data-id，用一个不存在的 id 验证 404，
    // 再用真实存在但锁定的文件验证 403 —— 后者需要从后端拿 id，
    // 而前端刻意不给锁定文件发 id，所以这里只能验证 404 路径
    try {
      const r = await fetch(${JSON.stringify(streamBase)} + '/file/nonexistent-id-xyz');
      return { status: r.status };
    } catch (e) { return { error: String(e) }; }
  })()`);
  check(forbidden.status === 404, '未知 id → 404', `实得 ${forbidden.status ?? forbidden.error}`);

  // ---------- 阶段 6：真实播放与 seek ----------
  console.log('\n=== 阶段 6：视频播放与 seek ===');
  const play = await cdp.eval(`(async () => {
    const card = [...document.querySelectorAll('.card[data-locked="0"]')]
      .find(c => c.querySelector('.name')?.textContent.includes('demo-video'));
    card.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await new Promise(r => setTimeout(r, 300));
    const v = document.getElementById('pv');
    if (!v) return { ok: false, error: '预览层未出现' };
    return await new Promise(resolve => {
      let done = false;
      const fin = o => { if (!done) { done = true; resolve(o); } };
      if (v.readyState >= 1) {
        fin({ ok: true, duration: v.duration, w: v.videoWidth, h: v.videoHeight });
        return;
      }
      v.addEventListener('loadedmetadata', () => fin({ ok: true,
        duration: v.duration, w: v.videoWidth, h: v.videoHeight }), { once: true });
      v.addEventListener('error', () => fin({ ok: false,
        code: v.error?.code, message: v.error?.message || '' }), { once: true });
      setTimeout(() => fin({ ok: false, message: '超时',
        networkState: v.networkState, readyState: v.readyState }), 25000);
    });
  })()`);

  if (play.ok) {
    check(true, '双击卡片后视频加载出元数据',
          `${play.duration?.toFixed(2)}s ${play.w}x${play.h}`);
    check(Math.abs(play.duration - 60) < 1.5, '时长约 60s', `${play.duration?.toFixed(2)}s`);
  } else {
    check(false, '视频加载元数据',
          `code=${play.code ?? '-'} ${play.message} rs=${play.readyState ?? '-'}`);
  }

  if (play.ok) {
    const seeks = await cdp.eval(`(async () => {
      const v = document.getElementById('pv');
      const out = [];
      for (const f of [0.85, 0.15, 0.60, 0.05, 0.95]) {
        const target = +(v.duration * f).toFixed(2);
        const t0 = performance.now();
        const ok = await new Promise(res => {
          let done = false;
          const h = () => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(true); } };
          v.addEventListener('seeked', h);
          v.currentTime = target;
          setTimeout(() => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(false); } }, 12000);
        });
        out.push({ frac: f, target, ok, landed: +v.currentTime.toFixed(2),
                   ms: Math.round(performance.now() - t0), rs: v.readyState });
      }
      return out;
    })()`);

    for (const s of seeks) {
      console.log(
        `  ${String(Math.round(s.frac * 100)).padStart(3)}% → ${String(s.target).padStart(6)}s  ` +
          `${s.ok ? '成功' : '超时'}  落点 ${String(s.landed).padStart(6)}s  ` +
          `偏差 ${Math.abs(s.landed - s.target).toFixed(2)}s  ${s.ms}ms`,
      );
    }
    check(seeks.every((s) => s.ok), '5 次乱序 seek 全部成功',
          `${seeks.filter((s) => s.ok).length}/5`);
    check(
      seeks.every((s) => Math.abs(s.landed - s.target) < 1.5),
      'seek 落点均接近目标',
      `最大偏差 ${Math.max(...seeks.map((s) => Math.abs(s.landed - s.target))).toFixed(2)}s`,
    );

    // canvas 取帧：这是「真的解码出画面」的唯一硬证据
    const frames = await cdp.eval(`(async () => {
      const v = document.getElementById('pv');
      const shots = [];
      for (const t of [5, 30, 50]) {
        await new Promise(res => {
          let done = false;
          const h = () => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(); } };
          v.addEventListener('seeked', h);
          v.currentTime = t;
          setTimeout(() => { if (!done) { done = true;
            v.removeEventListener('seeked', h); res(); } }, 12000);
        });
        await new Promise(r => setTimeout(r, 400));
        try {
          const c = document.createElement('canvas');
          c.width = v.videoWidth; c.height = v.videoHeight;
          const ctx = c.getContext('2d');
          ctx.drawImage(v, 0, 0);
          const d = ctx.getImageData(0, 0, c.width, c.height).data;
          const seen = new Set(); let sum = 0, n = 0;
          for (let i = 0; i < d.length; i += 4 * 97) {
            seen.add((d[i] << 16) | (d[i+1] << 8) | d[i+2]);
            sum += (d[i] + d[i+1] + d[i+2]) / 3; n++;
          }
          shots.push({ t, at: +v.currentTime.toFixed(2), ok: true,
                       colors: seen.size, brightness: Math.round(sum / n) });
        } catch (e) {
          shots.push({ t, ok: false, error: e.name + ': ' + e.message });
        }
      }
      return shots;
    })()`);

    for (const s of frames) {
      if (!s.ok) {
        console.log(`  t=${s.t}s 取帧失败 ${s.error}`);
        continue;
      }
      console.log(`  t=${String(s.t).padStart(2)}s 落点 ${s.at}s  颜色 ${s.colors} 种  亮度 ${s.brightness}`);
    }
    const decoded = frames.filter((s) => s.ok);
    check(decoded.length === frames.length, 'canvas 能取到帧（CORS 头生效，画布未被污染）');
    if (decoded.length === frames.length) {
      check(decoded.every((s) => s.colors > 20), '各位置都解码出有内容的画面',
            `最少 ${Math.min(...decoded.map((s) => s.colors))} 种颜色`);
      check(
        new Set(decoded.map((s) => `${s.colors}:${s.brightness}`)).size > 1,
        '不同时间点画面确实不同（排除卡在同一帧）',
      );
    }
  }

  // 关掉预览
  await cdp.eval(`(() => { document.getElementById('pv-close')?.click(); return 1; })()`, false);
  await sleep(300);

  // ---------- 阶段 7：图片与文本预览 ----------
  console.log('\n=== 阶段 7：图片与文本预览 ===');
  const img = await cdp.eval(`(async () => {
    const card = [...document.querySelectorAll('.card[data-locked="0"]')]
      .find(c => c.querySelector('.name')?.textContent.includes('demo-image'));
    if (!card) return { ok: false, error: '找不到图片卡片' };
    card.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    await new Promise(r => setTimeout(r, 300));
    const el = document.getElementById('pv');
    if (!el || el.tagName !== 'IMG') return { ok: false, error: '未用 <img> 渲染' };
    return await new Promise(resolve => {
      let done = false;
      const fin = o => { if (!done) { done = true; resolve(o); } };
      if (el.complete && el.naturalWidth > 0) {
        fin({ ok: true, w: el.naturalWidth, h: el.naturalHeight });
        return;
      }
      el.addEventListener('load', () => fin({ ok: true,
        w: el.naturalWidth, h: el.naturalHeight }), { once: true });
      el.addEventListener('error', () => fin({ ok: false, error: '加载失败' }), { once: true });
      setTimeout(() => fin({ ok: false, error: '超时' }), 15000);
    });
  })()`);
  check(img.ok, '图片能在 <img> 中显示', img.ok ? `${img.w}x${img.h}` : img.error);
  check(img.ok && img.w === 480 && img.h === 320, '图片尺寸正确（480x320）',
        img.ok ? `${img.w}x${img.h}` : '');

  await cdp.eval(`(() => { document.getElementById('pv-close')?.click(); return 1; })()`, false);
  await sleep(300);

  const txt = await cdp.eval(`(async () => {
    const card = [...document.querySelectorAll('.card[data-locked="0"]')]
      .find(c => c.querySelector('.name')?.textContent.includes('demo-notes'));
    if (!card) return { ok: false, error: '找不到文本卡片' };
    card.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    for (let i = 0; i < 60; i++) {
      await new Promise(r => setTimeout(r, 100));
      const pre = document.getElementById('pv');
      if (pre && pre.textContent && !pre.textContent.includes('…')) {
        return { ok: true, text: pre.textContent };
      }
    }
    return { ok: false, error: '超时' };
  })()`);
  check(txt.ok, '文本内容能取回并显示');
  check(
    txt.ok && txt.text.includes(SENTINEL),
    '读到的是解密后的真实内容（哨兵字符串命中）',
    txt.ok ? '' : txt.error,
  );
  check(
    txt.ok && txt.text.includes('中文') && txt.text.includes('日本語'),
    '多字节字符正确解码',
  );

  await cdp.eval(`(() => { document.getElementById('pv-close')?.click(); return 1; })()`, false);
  await sleep(200);

  // ---------- 阶段 8：多语言 ----------
  console.log('\n=== 阶段 8：多语言切换 ===');
  const langTest = await cdp.eval(`(async () => {
    const before = document.documentElement.lang;
    const beforeText = document.body.innerText;
    document.getElementById('btn-lang').click();
    await new Promise(r => setTimeout(r, 800));
    const after = document.documentElement.lang;
    const afterText = document.body.innerText;
    return { before, after, changed: beforeText !== afterText,
             afterHasEnglish: /file|password|Search/i.test(afterText),
             afterHasChinese: /文件|密码/.test(afterText) };
  })()`);
  check(langTest.before !== langTest.after, '语言标签切换',
        `${langTest.before} → ${langTest.after}`);
  check(langTest.changed, '界面文案确实随之改变');
  const toEn = langTest.after === 'en';
  check(
    toEn ? langTest.afterHasEnglish : langTest.afterHasChinese,
    `切换后显示${toEn ? '英文' : '中文'}文案`,
  );

  // 切回去
  await cdp.eval(`(() => { document.getElementById('btn-lang').click(); return 1; })()`, false);
  await sleep(600);

  // ---------- 阶段 9：锁定 ----------
  console.log('\n=== 阶段 9：锁定 ===');
  const locked = await cdp.eval(`(async () => {
    document.getElementById('btn-lock').click();
    await new Promise(r => setTimeout(r, 900));
    return {
      backToUnlock: !!document.querySelector('.unlock-box'),
      noTopbar: !document.querySelector('.topbar'),
      cards: document.querySelectorAll('.card').length,
      text: document.body.innerText,
    };
  })()`);
  check(locked.backToUnlock, '锁定后回到解锁界面');
  check(locked.cards === 0, '锁定后没有任何文件卡片残留', `实得 ${locked.cards}`);
  check(
    !locked.text.includes('demo-video') && !locked.text.includes('demo-notes'),
    '锁定后界面上不残留任何真实文件名',
  );

  // 锁定后协议必须拒绝——这是最关键的一条：
  // 若前端只是切了视图而后端仍持有密钥，这里会返回 200
  const afterLock = await cdp.eval(`(async () => {
    try {
      const r = await fetch(${JSON.stringify(streamBase)} + '/file/' + ${JSON.stringify(vidId)});
      return { status: r.status, len: (await r.arrayBuffer()).byteLength };
    } catch (e) { return { error: String(e) }; }
  })()`);
  check(
    afterLock.status === 404 || afterLock.status === 403,
    '锁定后协议拒绝提供内容（密钥真的被抹掉了）',
    `实得 ${afterLock.status ?? afterLock.error}`,
  );

  // ---------- 网络观察 ----------
  console.log('\n=== 网络观察 ===');
  const mine = responses.filter((r) => r.url.includes('omystream'));
  const c206 = mine.filter((r) => r.status === 206).length;
  const c200 = mine.filter((r) => r.status === 200).length;
  console.log(`  omystream 响应 ${mine.length} 条（206:${c206} 200:${c200}）`);
  const cached = mine.filter((r) => r.fromCache);
  check(cached.length === 0, '无响应来自磁盘缓存', `fromDiskCache=${cached.length}`);

  // 排除测试自己故意触发的错误响应。
  //
  // 阶段 5 主动请求了越界 Range（期望 416）和不存在的 id（期望 404），
  // 阶段 9 在锁定后又请求了一次（期望 404）。WebView 会把这些
  // 如实记进控制台，但它们恰恰是断言通过的证据，不是缺陷。
  //
  // 只过滤这两个特定状态码，别的错误一律照报——
  // 把整条规则放宽会让真正的问题溜过去。
  const EXPECTED = [
    /status of 416 \(Range Not Satisfiable\)/,
    /status of 404 \(Not Found\)/,
  ];
  const errs = consoleMsgs
    .filter((m) => m.startsWith('[error]'))
    .filter((m) => !EXPECTED.some((re) => re.test(m)));
  if (consoleMsgs.length) {
    console.log('  控制台:');
    for (const m of consoleMsgs.slice(0, 12)) console.log('    ' + m);
  }
  check(errs.length === 0, '控制台无预期外的错误', `${errs.length} 条`);

  console.log('\n' + '='.repeat(66));
  console.log(`结果: ${pass.length} 通过, ${fail.length} 失败`);
  if (fail.length) {
    console.log('失败项:');
    for (const f of fail) console.log('  - ' + f);
  }
  console.log('='.repeat(66));
  cdp.close();
  process.exit(fail.length ? 1 : 0);
};

main().catch((e) => {
  console.error('驱动失败:', e.message);
  process.exit(2);
});
