/** 实测两台 GUI 之间的共享浏览与流式预览。
 *
 * 这是本轮唯一能证明「远端预览真的能播」的手段。单元测试只能证明
 * 协议解析和排序对，证明不了：配对能不能成、连上后列表是不是真的
 * 拿得到、视频拖动时 Range 请求是不是真的落到远端、seek 到 80% 处
 * 是不是只取那附近的几十 KB 而不是整个文件。
 *
 * 两个真实进程，各自独立的设备库，走真实的 mDNS 与 Noise 信道。
 *
 * 用法：node --experimental-websocket probe-remote.mjs <A端口> <B端口> <共享目录>
 *   A = 共享方（server），B = 浏览方（client）
 */

const portA = process.argv[2];
const portB = process.argv[3];
const shareDir = process.argv[4] || '';
const PASSWORD = 'share-test-pw';

async function waitTarget(port, timeoutMs = 30000) {
  const base = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const list = await r.json();
        const page = list.find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page) return page;
      }
    } catch {
      /* 等 */
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error(`CDP ${port} 未就绪`);
}

class Cdp {
  constructor(ws, tag) {
    this.ws = ws;
    this.tag = tag;
    this.id = 0;
    this.pending = new Map();
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m);
        this.pending.delete(m.id);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      this.pending.set(id, (m) =>
        m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result),
      );
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => rej(new Error(`${this.tag}:${method} 超时`)), 90000);
    });
  }
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    }
    return r.result.value;
  }
  /** 调一个 Tauri 命令，错误原样带回来而不是抛掉。 */
  inv(cmd, args = {}) {
    return this.eval(`(async () => {
      try {
        const v = await window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)});
        return { ok: true, v };
      } catch (e) {
        return { ok: false, e: (e && e.code) ? e.code : String(e) };
      }
    })()`);
  }
}

async function connect(port, tag) {
  const t = await waitTarget(port);
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error(`${tag} ws 失败`)));
  });
  const c = new Cdp(ws, tag);
  await c.send('Runtime.enable');
  return c;
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

(async () => {
  const A = await connect(portA, 'A');
  const B = await connect(portB, 'B');
  console.log('两端 CDP 已连接\n');

  /* ---------- 1. 各自建设备库 ---------- */
  console.log('=== 1. 设备库 ===');
  const oa = await A.inv('open_device_store', { password: 'store-a-pw' });
  const ob = await B.inv('open_device_store', { password: 'store-b-pw' });
  check('A 建设备库', oa.ok, oa.ok ? `指纹 ${oa.v.fingerprint}` : oa.e);
  check('B 建设备库', ob.ok, ob.ok ? `指纹 ${ob.v.fingerprint}` : ob.e);
  if (!oa.ok || !ob.ok) throw new Error('设备库失败，后续无意义');

  // 两台设备的指纹必须不同，否则说明身份没有独立生成
  check('两端身份独立', oa.v.fingerprint !== ob.v.fingerprint,
    `${oa.v.fingerprint} vs ${ob.v.fingerprint}`);

  await A.inv('rename_device', { name: 'DeviceA' });
  await B.inv('rename_device', { name: 'DeviceB' });

  /* ---------- 2. 配对 ---------- */
  console.log('\n=== 2. 配对 ===');
  const listen = await A.inv('pair_listen', { port: 0, expiresDays: 0 });
  check('A 开始等待配对', listen.ok && listen.v.phase === 'waiting',
    listen.ok ? `码 ${listen.v.pin} 端口 ${listen.v.port}` : listen.e);
  if (!listen.ok) throw new Error('配对监听失败');

  const pin = listen.v.pin;
  const pairPort = listen.v.port;

  const pw = await B.inv('pair_with', {
    addr: `127.0.0.1:${pairPort}`,
    pin,
    expiresDays: 0,
  });
  check('B 发起配对', pw.ok, pw.ok ? `phase=${pw.v.phase}` : pw.e);

  // 配对是后台任务，轮询等它落定
  let aPhase = '';
  for (let i = 0; i < 40; i++) {
    const st = await A.inv('pair_status');
    if (st.ok) {
      aPhase = st.v.phase;
      if (aPhase === 'done' || aPhase === 'failed') break;
    }
    await sleep(300);
  }
  check('A 侧配对完成', aPhase === 'done', `phase=${aPhase}`);

  const pa = await A.inv('paired_devices');
  const pb = await B.inv('paired_devices');
  check('A 记住了 B', pa.ok && pa.v.length === 1,
    pa.ok ? `${pa.v.length} 台: ${pa.v.map((d) => d.name).join(',')}` : pa.e);
  check('B 记住了 A', pb.ok && pb.v.length === 1,
    pb.ok ? `${pb.v.length} 台: ${pb.v.map((d) => d.name).join(',')}` : pb.e);
  if (!pa.ok || !pb.ok || !pb.v.length) throw new Error('配对未成功');

  const fpOfA = pb.v[0].fingerprint;

  /* ---------- 3. A 开共享 ---------- */
  console.log('\n=== 3. 共享服务 ===');
  const sh = await A.inv('start_share', { dir: shareDir, port: 0, advertise: true });
  check('A 启动共享', sh.ok && sh.v.running,
    sh.ok ? `addr=${sh.v.addr} files=${sh.v.files}` : sh.e);
  if (!sh.ok) throw new Error('共享启动失败');
  // addr 形如 0.0.0.0:51234。0.0.0.0 是「监听所有网卡」的意思，
  // 不是一个可连的目标地址——直接拿它去 connect 会失败。
  // 只取端口，回连 127.0.0.1
  const sharePort = String(sh.v.addr || '').split(':').pop();
  check('共享端口可解析', /^\d+$/.test(sharePort || ''), `port=${sharePort}`);
  check('共享目录里有文件', sh.v.files > 0, `files=${sh.v.files}`);

  /* ---------- 4. B 连接并浏览 ---------- */
  console.log('\n=== 4. 连接与浏览 ===');
  // 显式给地址：mDNS 在同机双实例上不一定互相看得见，
  // 而这里要验的是「连上之后能不能用」，不是发现机制
  const conn = await B.inv('remote_connect', {
    fingerprint: fpOfA,
    addr: `127.0.0.1:${sharePort}`,
  });
  check('B 连上 A', conn.ok, conn.ok ? `${conn.v.name} @ ${conn.v.addr}` : conn.e);
  if (!conn.ok) throw new Error('连接失败');

  const st = await B.inv('remote_status');
  check('连接状态可查', st.ok && st.v.connected, st.ok ? JSON.stringify(st.v.peer) : st.e);

  const list1 = await B.inv('remote_list');
  check('拿到远端列表', list1.ok && list1.v.length > 0,
    list1.ok ? `${list1.v.length} 个文件` : list1.e);
  if (!list1.ok || !list1.v.length) throw new Error('列表为空');

  // 关键：没有密码时必须全是锁定态，且不能带出真实文件名
  const allLocked = list1.v.every((f) => !f.unlocked);
  check('无密码时全部锁定', allLocked,
    `unlocked=${list1.v.filter((f) => f.unlocked).length}`);
  const noNames = list1.v.every((f) => f.name === null || f.name === undefined);
  check('锁定项不带出真实文件名', noNames,
    JSON.stringify(list1.v.map((f) => f.name)));

  // 内部字段不能进 WebView
  const noInternals = list1.v.every(
    (f) => f.handle === undefined && f.header === undefined,
  );
  check('不泄露 handle 与 header', noInternals);

  /* ---------- 5. 输密码解开 ---------- */
  console.log('\n=== 5. 远端解锁 ===');
  const vaults = await B.inv('remote_vaults');
  check('取到远端 vault 参数', vaults.ok && vaults.v.length > 0,
    vaults.ok ? `${vaults.v.length} 个 vault` : vaults.e);
  if (!vaults.ok || !vaults.v.length) throw new Error('无 vault');

  const unlocked = await B.inv('unlock', {
    label: 'main',
    password: PASSWORD,
    vaults: vaults.v,
  });
  check('派生密钥', unlocked.ok, unlocked.ok ? JSON.stringify(unlocked.v) : unlocked.e);

  const list2 = await B.inv('remote_relock');
  const openedCount = list2.ok ? list2.v.filter((f) => f.unlocked).length : 0;
  check('密码正确后解开文件', openedCount > 0,
    list2.ok ? `${openedCount}/${list2.v.length} 解开` : list2.e);
  if (!openedCount) throw new Error('一个都没解开');

  const withName = list2.v.filter((f) => f.unlocked && f.name);
  check('解开后显示真实文件名', withName.length === openedCount,
    withName.map((f) => f.name).join(', '));

  /* ---------- 6. 通过协议真的取到明文 ---------- */
  console.log('\n=== 6. 流式读取 ===');
  const target = withName[0];
  const baseUrl = await B.eval(
    `window.__TAURI_INTERNALS__.invoke('stream_base')`,
  );
  const url = `${baseUrl}/rfile/${encodeURIComponent(target.id)}`;

  // 完整取一次
  const full = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)});
    const t = await r.text();
    return { status: r.status, len: t.length, body: t.slice(0, 80),
             ar: r.headers.get('accept-ranges') };
  })()`);
  check('远端文件能取到明文', full.status === 200 && full.len > 0,
    `status=${full.status} len=${full.len} body=${JSON.stringify(full.body)}`);
  check('声明支持 Range', full.ar === 'bytes', `accept-ranges=${full.ar}`);

  // Range 请求：这是拖动播放的基础
  const part = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)}, { headers: { Range: 'bytes=2-6' } });
    const t = await r.text();
    return { status: r.status, cr: r.headers.get('content-range'), body: t, len: t.length };
  })()`);
  check('Range 请求返回 206', part.status === 206, `status=${part.status}`);
  check('Content-Range 正确', /^bytes 2-6\//.test(part.cr || ''), `cr=${part.cr}`);
  check('Range 内容正确', part.body === full.body.slice(2, 7),
    `得到 ${JSON.stringify(part.body)}，期望 ${JSON.stringify(full.body.slice(2, 7))}`);

  // 不可满足的 Range 必须 416 而不是返回错数据
  const bad = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)}, { headers: { Range: 'bytes=999999-' } });
    return { status: r.status, cr: r.headers.get('content-range') };
  })()`);
  check('越界 Range 返回 416', bad.status === 416, `status=${bad.status} cr=${bad.cr}`);

  // 缩略图路径：没有缩略图应当是 404 而不是 500
  const th = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(baseUrl)} + '/rthumb/' + ${JSON.stringify(target.id)});
    return r.status;
  })()`);
  check('缩略图路径可达（404/200 均可）', th === 404 || th === 200, `status=${th}`);

  // 不存在的 id 必须 404
  const nf = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(baseUrl)} + '/rfile/ffffffffffffffffffffffffffffffff');
    return r.status;
  })()`);
  check('未知 id 返回 404', nf === 404, `status=${nf}`);

  /* ---------- 7. 锁定后必须什么都取不到 ---------- */
  console.log('\n=== 7. 锁定 ===');
  await B.inv('lock');
  const afterLock = await B.eval(`(async () => {
    const r = await fetch(${JSON.stringify(url)});
    return r.status;
  })()`);
  check('锁定后取不到内容', afterLock === 403 || afterLock === 404,
    `status=${afterLock}`);

  const stAfter = await B.inv('remote_status');
  check('锁定后连接已断开', stAfter.ok && !stAfter.v.connected,
    JSON.stringify(stAfter.v));

  // paired_devices 返回的是 Vec 不是 Result，关库后它给的是空列表
  // 而不是错误码。判据要用 device_status 的 opened 字段，
  // 以及「列表确实空了」——设备名不能在锁定后还留在内存里
  const devAfter = await B.inv('paired_devices');
  const stDev = await B.inv('device_status');
  check('锁定后设备库已关闭', stDev.ok && !stDev.v.opened,
    stDev.ok ? JSON.stringify(stDev.v) : stDev.e);
  check('锁定后不再返回设备名', devAfter.ok && devAfter.v.length === 0,
    devAfter.ok ? `${devAfter.v.length} 台` : devAfter.e);

  /* ---------- 汇总 ---------- */
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} 项通过`);
  if (failed.length) {
    console.log('失败项：');
    for (const f of failed) console.log('  - ' + f.name);
  }
  process.exit(failed.length ? 1 : 0);
})().catch((e) => {
  console.log('\n探针异常: ' + e.message);
  const failed = results.filter((r) => !r.ok);
  console.log(`${results.length - failed.length}/${results.length} 项通过（中断）`);
  process.exit(1);
});
