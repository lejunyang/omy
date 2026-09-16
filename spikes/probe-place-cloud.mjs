/** 远程位置（WebDAV 云盘）GUI 端到端探针。
 *
 * 与 crates/omy-remote/tests/webdav_server.rs 的分工：
 * - 那个测试在 RemoteSource 层对着真服务器验点播 / seek / 缓存；
 * - 本探针在**完整 GUI 装配层**验证：Tauri 命令（add/browse/open）、
 *   omystream 自定义协议 pfile 的 Range 响应、密文缓存落盘与清理、
 *   锁定后旧 token 失效。任何一处接线错误（命令没注册、协议没路由、
 *   会话 KEK 取不到、缓存目录建不起来）这里都会红。
 *
 * 关键夹具：云端的 movie.mp4.omy 与本地库是**同一份密文字节**，
 * 因此本地解锁后会话里就有它 vault_salt 对应的 KEK，云端 open 才能解锁。
 *
 * 用法：node --experimental-websocket probe-place-cloud.mjs <cdpPort> <davUrl> <guiDir> <password>
 */

const cdpPort = process.argv[2] || '9466';
const davUrl = process.argv[3] || 'http://127.0.0.1:8799/';
const guiDir = process.argv[4];
const password = process.argv[5] || 'cloud-test-password';

// 与编排脚本里生成的明文严格一致：3.5 MiB、字节 = i % 251（不可压缩，跨多个 1MiB 缓存块）
const PLAIN_LEN = 3 * 1024 * 1024 + 512 * 1024;
const HEAD_LEN = 1024;
const TAIL_LEN = 64 * 1024;

const pass = [];
const fail = [];
function ok(n) { pass.push(n); console.log(`  PASS  ${n}`); }
function bad(n, d) { fail.push(`${n}: ${d}`); console.log(`  FAIL  ${n} — ${d}`); }
function check(c, n, d = '') { if (c) ok(n); else bad(n, d || '断言为假'); }
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
}

async function main() {
  const target = await waitTarget();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res, { once: true }); ws.addEventListener('error', rej, { once: true }); });
  const cdp = new Cdp(ws);
  await cdp.send('Runtime.enable');
  await sleep(1200);

  // 页面里统一的 invoke / fetch 桥
  await cdp.eval(`window.__p = {
    invoke: (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {}),
  }; true`);

  console.log('\n[1] 导航到本地夹具目录并解锁（建立会话 KEK）');
  const nav = await cdp.eval(`(async () => {
    try { await window.__p.invoke('browse_directory', { dir: ${JSON.stringify(guiDir)} }); } catch (e) { return 'nav:' + e; }
    try {
      const r = await window.__p.invoke('unlock_directory', { dir: ${JSON.stringify(guiDir)}, password: ${JSON.stringify(password)} });
      return JSON.stringify(r);
    } catch (e) { return 'unlock:' + e; }
  })()`);
  console.log('       unlock ->', String(nav).slice(0, 200));
  check(!/^nav:|^unlock:/.test(nav), '本地目录解锁成功', String(nav));

  console.log('\n[2] 添加 WebDAV 位置（只读）');
  const placeId = await cdp.eval(`(async () => {
    try {
      return await window.__p.invoke('remote_place_add', {
        name: '自测NAS', url: ${JSON.stringify(davUrl)}, username: '', password: '',
        vendor: 'generic', writable: false,
      });
    } catch (e) { return 'ERR:' + e; }
  })()`);
  console.log('       placeId =', placeId);
  check(typeof placeId === 'string' && !placeId.startsWith('ERR'), '位置已添加', String(placeId));

  console.log('\n[3] 浏览云端根目录：加密文件 / 普通文件 / 中文目录');
  const rootList = await cdp.eval(`(async () => {
    try { return JSON.stringify(await window.__p.invoke('remote_browse', { placeId: ${JSON.stringify(placeId)}, dir: '' })); }
    catch (e) { return 'ERR:' + e; }
  })()`);
  let root = [];
  try { root = JSON.parse(rootList); } catch { root = []; }
  if (typeof root[0] === 'string') { /* 容错：若返回被双重包裹 */ }
  const names = root.map((e) => e.name);
  console.log('       ', names.join(', '));
  const movie = root.find((e) => e.name === 'movie.mp4.omy');
  check(Array.isArray(root) && root.length >= 2, '根目录列出条目', rootList.slice(0, 200));
  check(!!movie && movie.is_encrypted === true, 'movie.mp4.omy 识别为加密', movie ? JSON.stringify(movie).slice(0, 160) : '缺失');
  check(!!movie && movie.unlocked === true, '同库密文在本地解锁后显示为可打开（unlocked）', movie ? `unlocked=${movie.unlocked}` : '缺失');
  check(root.some((e) => e.name === 'plain.txt' && e.is_encrypted === false), '普通文件标记为非加密');
  const cnDir = root.find((e) => e.is_dir && e.name === '影视');
  check(!!cnDir, '中文目录可列出', names.join(','));

  console.log('\n[4] 进中文目录，验证中文 + 空格路径往返');
  const cnList = cnDir ? await cdp.eval(`(async () => {
    try { return JSON.stringify(await window.__p.invoke('remote_browse', { placeId: ${JSON.stringify(placeId)}, dir: ${JSON.stringify(cnDir.id)} })); }
    catch (e) { return 'ERR:' + e; }
  })()`) : '[]';
  let cn = [];
  try { cn = JSON.parse(cnList); } catch { cn = []; }
  check(cn.some((e) => e.name && e.name.endsWith('.omy')), '中文目录内列出加密文件', cnList.slice(0, 160));

  console.log('\n[5] 打开云端 movie，拿到播放 token');
  const opened = await cdp.eval(`(async () => {
    try {
      return JSON.stringify(await window.__p.invoke('remote_place_open', {
        placeId: ${JSON.stringify(placeId)}, path: ${JSON.stringify(movie?.id || '')}, size: ${movie?.size ?? 0},
      }));
    } catch (e) { return 'ERR:' + e; }
  })()`);
  let op = {};
  try { op = JSON.parse(opened); } catch { op = {}; }
  console.log('       ', opened.slice(0, 220));
  check(typeof op.token === 'string' && op.token.length > 0, 'open 返回不透明 token', opened.slice(0, 200));
  check(op.unlocked === true, '云端文件已用会话 KEK 解锁', `unlocked=${op.unlocked}`);
  check(op.name === 'movie.mp4', '头部还原真实文件名 movie.mp4', `name=${op.name}`);
  check(op.kind === 'video', '识别为视频类型', `kind=${op.kind}`);
  const token = op.token;

  const base = await cdp.eval(`(async () => {
    const b = await window.__p.invoke('stream_base');
    return typeof b === 'string' ? b : (b && b.base) || '';
  })()`);
  console.log('       stream base =', base);
  check(typeof base === 'string' && base.length > 0, '取到 omystream base', String(base));
  const url = `${base.replace(/\/$/, '')}/pfile/${token}`;

  console.log('\n[6] Range 拉片头 1 KiB，逐字节核对解密结果');
  const head = await cdp.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)}, { headers: { Range: 'bytes=0-${HEAD_LEN - 1}' } });
    const buf = new Uint8Array(await r.arrayBuffer());
    const cr = r.headers.get('Content-Range') || '';
    let ok = true; for (let i = 0; i < buf.length; i++) if (buf[i] !== i % 251) { ok = false; break; }
    return JSON.stringify({ status: r.status, len: buf.length, cr, bytesOk: ok });
  })()`);
  let h = {};
  try { h = JSON.parse(head); } catch { h = {}; }
  console.log('       ', head);
  check(h.status === 206, '片头返回 206 Partial', `status=${h.status}`);
  check(h.len === HEAD_LEN, '片头长度精确为 1 KiB', `len=${h.len}`);
  check(h.bytesOk === true, '片头解密逐字节正确', '');
  check(new RegExp(`0-${HEAD_LEN - 1}/${PLAIN_LEN}$`).test(h.cr || ''), 'Content-Range 的 total 是明文大小', `cr=${h.cr}`);

  console.log('\n[7] 直接跳到片尾 64 KiB（验证 seek 不从头下载、解密正确）');
  const tailStart = PLAIN_LEN - TAIL_LEN;
  const tail = await cdp.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)}, { headers: { Range: 'bytes=${tailStart}-${PLAIN_LEN - 1}' } });
    const buf = new Uint8Array(await r.arrayBuffer());
    let ok = true; for (let i = 0; i < buf.length; i++) if (buf[i] !== (${tailStart} + i) % 251) { ok = false; break; }
    return JSON.stringify({ status: r.status, len: buf.length, bytesOk: ok,
      cr: r.headers.get('Content-Range') || '' });
  })()`);
  let t = {};
  try { t = JSON.parse(tail); } catch { t = {}; }
  console.log('       ', tail);
  check(t.status === 206 && t.len === TAIL_LEN, '片尾返回 206 且长度 64 KiB', `status=${t.status} len=${t.len}`);
  check(t.bytesOk === true, '片尾解密逐字节正确（证明可直接 seek 到尾部）', '');

  console.log('\n[8] 密文块缓存落盘，用量为正、目录可报');
  const usage = await cdp.eval(`(async () => JSON.stringify(await window.__p.invoke('remote_cache_usage')))()`);
  let u = {};
  try { u = JSON.parse(usage); } catch { u = {}; }
  console.log('       ', usage);
  check(Number(u.used) > 0, '缓存占用为正（密文块已落盘）', `used=${u.used}`);
  check(Number(u.limit) > 0, '缓存上限已从配置读取', `limit=${u.limit}`);
  check(typeof u.root === 'string' && u.root.length > 0, '缓存根目录可报', `root=${u.root}`);

  console.log('\n[9] 越界 Range 返回 416');
  const badRange = await cdp.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)}, { headers: { Range: 'bytes=${PLAIN_LEN * 10}-' } });
    return r.status;
  })()`);
  check(Number(badRange) === 416, '超出明文大小的 Range 返回 416', `status=${badRange}`);

  console.log('\n[10] 关闭句柄后旧 token 失效（404）');
  await cdp.eval(`(async () => { await window.__p.invoke('remote_place_close', { token: ${JSON.stringify(token)} }); return 1; })()`);
  const afterClose = await cdp.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)});
    return r.status;
  })()`);
  check(Number(afterClose) === 404, 'close 后旧 token 返回 404', `status=${afterClose}`);

  console.log('\n[11] 清空缓存，占用归零');
  const cleared = await cdp.eval(`(async () => await window.__p.invoke('remote_cache_clear'))()`);
  check(Number(cleared) === 0, '清空后缓存占用为 0', `used=${cleared}`);

  console.log(`\n=== ${pass.length} 通过 / ${fail.length} 失败 ===`);
  if (fail.length) { console.log('FAILURES:\n' + fail.map((f) => '  - ' + f).join('\n')); process.exit(1); }
  console.log('PROBE_OK');
  ws.close();
}

main().catch((e) => { console.error('探针异常:', e); process.exit(1); });
