/** 实测 GUI 的设备与共享功能。
 *
 * 重点验证「真的能用」而不是「界面画出来了」：
 * 用真实的 Tauri 命令建设备库、发起配对、启动共享服务，
 * 并用**真实 TCP 连接**去验证服务端确实在监听。
 *
 * 用法：node --experimental-websocket probe-gui-devices.mjs <port> <共享目录>
 */

import net from 'node:net';

const port = process.argv[2] || '9348';
const shareDir = process.argv[3] || '';
const base = `http://127.0.0.1:${port}`;

async function waitTarget(timeoutMs = 30000) {
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
  throw new Error('CDP 未就绪');
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.id = 0;
    this.pending = new Map();
    this.logs = [];
    ws.addEventListener('message', (ev) => {
      const m = JSON.parse(ev.data);
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m);
        this.pending.delete(m.id);
      } else if (m.method === 'Runtime.exceptionThrown') {
        const d = m.params.exceptionDetails;
        this.logs.push(d.exception?.description || d.text);
      }
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((res, rej) => {
      this.pending.set(id, (m) => (m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => rej(new Error(`${method} 超时`)), 60000);
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
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}

/** 试着建立一条 TCP 连接，用来证明服务端真的在监听。 */
function canConnect(host, p, timeoutMs = 3000) {
  return new Promise((resolve) => {
    const sock = net.connect({ host, port: p });
    const done = (v) => {
      sock.destroy();
      resolve(v);
    };
    sock.setTimeout(timeoutMs);
    sock.on('connect', () => done(true));
    sock.on('error', () => done(false));
    sock.on('timeout', () => done(false));
  });
}

const page = await waitTarget();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => {
  ws.addEventListener('open', res);
  ws.addEventListener('error', rej);
});
const cdp = new Cdp(ws);
await cdp.send('Runtime.enable');
await new Promise((r) => setTimeout(r, 2000));

const inv = (cmd, args = {}) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;

console.log('=== 设备库 ===');

// 未打开时也要能回答状态，否则界面不知道该显示「设置」还是「输入」密码
const before = await cdp.eval(inv('device_status'));
check('未打开时能查状态', typeof before?.opened === 'boolean', JSON.stringify(before).slice(0, 100));
check(
  '未打开时不泄露身份（关键反证）',
  before.device_name === null && before.fingerprint === null,
  JSON.stringify(before).slice(0, 120),
);

// 建库
const opened = await cdp.eval(inv('open_device_store', { password: 'device-store-pw' }));
check('能创建设备库', opened?.opened === true, JSON.stringify(opened).slice(0, 140));
check('创建后有设备名', typeof opened?.device_name === 'string' && opened.device_name.length > 0, opened?.device_name);
check(
  '本机指纹是 16 位十六进制',
  typeof opened?.fingerprint === 'string' && /^[0-9a-f]{16}$/.test(opened.fingerprint),
  opened?.fingerprint,
);

// 二次打开必须是同一身份——每次换身份的话，已配对设备全部失效
const reopened = await cdp.eval(inv('open_device_store', { password: 'device-store-pw' }));
check(
  '重复打开保持同一身份（关键）',
  reopened?.fingerprint === opened?.fingerprint,
  `${opened?.fingerprint} vs ${reopened?.fingerprint}`,
);

const devices0 = await cdp.eval(inv('paired_devices'));
check('新库没有已配对设备', Array.isArray(devices0) && devices0.length === 0, `${devices0?.length} 台`);

// 改名后指纹不能变：名字只是展示，身份由密钥决定
const renamed = await cdp.eval(inv('rename_device', { name: '测试机' }));
check('能改设备名', renamed?.device_name === '测试机', renamed?.device_name);
check('改名不影响身份', renamed?.fingerprint === opened?.fingerprint);

console.log('');
console.log('=== 配对 ===');

const listen = await cdp.eval(inv('pair_listen', { port: 0, expiresDays: 0 }));
check('能发起配对并拿到配对码', listen?.phase === 'waiting', JSON.stringify(listen).slice(0, 100));
check(
  '配对码是 6 位数字',
  typeof listen?.pin === 'string' && /^\d{6}$/.test(listen.pin),
  listen?.pin,
);
check('返回了监听端口', typeof listen?.port === 'number' && listen.port > 0, String(listen?.port));

// 轮询要先于任何 TCP 探测：探测会被后台任务 accept，
// 从而把状态推进到握手阶段
const during = await cdp.eval(inv('pair_status'));
check('轮询能拿到进行中的状态', during?.phase === 'waiting', JSON.stringify(during).slice(0, 80));

await new Promise((r) => setTimeout(r, 800));
const still = await cdp.eval(inv('pair_status'));
check('等待期间状态保持不变', still?.phase === 'waiting' && still?.pin === listen.pin);

// 关键：端口必须真的在监听。只看返回值的话，
// 后端返回一个假端口号也能"通过"
const listening = await canConnect('127.0.0.1', listen.port);
check('配对端口真的在监听（真实 TCP）', listening, `127.0.0.1:${listen.port}`);

// 上面那次连接会被 accept 并握手失败。配对**不该**因此作废——
// 否则任何端口扫描都能让用户的配对码悄悄失效，而屏幕上还显示着它
await new Promise((r) => setTimeout(r, 1500));
const afterProbe = await cdp.eval(inv('pair_status'));
check(
  '陌生连接不会让配对作废（关键反证）',
  afterProbe?.phase === 'waiting' && afterProbe?.pin === listen.pin,
  JSON.stringify(afterProbe).slice(0, 90),
);

await cdp.eval(inv('pair_cancel'));
const after = await cdp.eval(inv('pair_status'));
check('取消后回到空闲', after?.phase === 'idle', JSON.stringify(after));

// 取消后端口必须释放，否则配对码作废了但端口还占着
await new Promise((r) => setTimeout(r, 600));
const stillOpen = await canConnect('127.0.0.1', listen.port, 1200);
check('取消后端口已释放（关键反证）', !stillOpen, stillOpen ? '端口仍在监听' : '已释放');

// 错误的配对码格式必须被拒绝
const badPin = await cdp.eval(`(async () => {
  try {
    await ${inv('pair_with', { addr: '127.0.0.1:1', pin: 'abc', expiresDays: 0 })};
    return 'no-error';
  } catch (e) { return e?.code ?? String(e); }
})()`);
check('非法配对码被拒绝（关键反证）', badPin !== 'no-error', String(badPin).slice(0, 80));

// 连不上对方要报「连不上」，不能报成「配对码错」——
// 用户据此判断该改地址还是改配对码
const unreachable = await cdp.eval(`(async () => {
  try {
    await ${inv('pair_with', { addr: '127.0.0.1:1', pin: '123456', expiresDays: 0 })};
    return 'no-error';
  } catch (e) { return e?.code ?? String(e); }
})()`);
check(
  '连不上时报 connect_failed 而非密码错',
  unreachable === 'connect_failed',
  String(unreachable).slice(0, 60),
);

console.log('');
console.log('=== 共享 ===');

if (shareDir) {
  const st0 = await cdp.eval(inv('share_status'));
  check('初始未共享', st0?.running === false);
  check('未共享时不泄露目录', st0?.dir === null);

  const started = await cdp.eval(
    inv('start_share', { dir: shareDir, port: 0, advertise: false }),
  );
  check('能启动共享', started?.running === true, JSON.stringify(started).slice(0, 160));
  check('报告了共享文件数', typeof started?.files === 'number', `${started?.files} 个`);

  // 同样要真连一下：服务端可能报告成功但根本没绑定
  const sharePort = Number(String(started?.addr || '').split(':').pop());
  const shareUp = await canConnect('127.0.0.1', sharePort);
  check('共享端口真的在监听（真实 TCP）', shareUp, started?.addr);

  // 重复启动必须失败，否则会起两个服务抢同一批文件
  const dup = await cdp.eval(`(async () => {
    try {
      await ${inv('start_share', { dir: shareDir, port: 0, advertise: false })};
      return 'no-error';
    } catch (e) { return e?.code ?? String(e); }
  })()`);
  check('重复启动被拒绝（关键反证）', dup !== 'no-error', String(dup).slice(0, 60));

  const stopped = await cdp.eval(inv('stop_share'));
  check('能停止共享', stopped?.running === false);

  await new Promise((r) => setTimeout(r, 600));
  const shareStillUp = await canConnect('127.0.0.1', sharePort, 1200);
  check('停止后端口已释放（关键反证）', !shareStillUp, shareStillUp ? '仍在监听' : '已释放');
} else {
  console.log('  (跳过：没有提供共享目录)');
}

console.log('');
console.log('=== 锁定必须一并关掉设备库 ===');

await cdp.eval(inv('open_device_store', { password: 'device-store-pw' }));
const beforeLock = await cdp.eval(inv('device_status'));
check('锁定前设备库是打开的', beforeLock?.opened === true);

await cdp.eval(inv('lock'));
const afterLock = await cdp.eval(inv('device_status'));
check('锁定后设备库也关了（关键反证）', afterLock?.opened === false, JSON.stringify(afterLock).slice(0, 100));
check(
  '锁定后不再泄露本机身份',
  afterLock?.device_name === null && afterLock?.fingerprint === null,
);

const devAfterLock = await cdp.eval(inv('paired_devices'));
check('锁定后取不到设备列表', Array.isArray(devAfterLock) && devAfterLock.length === 0);

console.log('');
console.log('=== 界面 ===');

const panelKeys = await cdp.eval(`(() => {
  const t = document.body.innerText;
  const m = t.match(/\\bdevice\\.[a-z_]+/g);
  return m ? m.slice(0, 3).join(', ') : '';
})()`);
check('界面上没有未翻译的设备键名', panelKeys === '', panelKeys);

const sidebarEntry = await cdp.eval(
  `[...document.querySelectorAll('.side .stext')].some(e => e.textContent.includes('设备') || e.textContent.includes('Devices'))`,
);
check('侧栏有设备入口', sidebarEntry);

const errors = cdp.logs.filter((l) => !String(l).includes('favicon'));
check('无运行时错误', errors.length === 0, errors.join(' | ').slice(0, 200));

ws.close();
const passed = results.filter((r) => r.ok).length;
console.log('');
console.log(`结果: ${passed} 通过, ${results.length - passed} 失败`);
process.exit(passed === results.length ? 0 : 1);
