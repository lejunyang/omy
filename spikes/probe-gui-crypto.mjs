/** 实测加密与解锁两条链路。
 *
 * 上一个脚本验证了「能进门、能浏览」，这个验证「能干活」：
 * 通过真实的 Tauri 命令加密文件、用密码解锁、确认解锁后能看到真名。
 *
 * 用法：node --experimental-websocket probe-gui-crypto.mjs <port> <目录>
 */

const port = process.argv[2] || '9347';
const dir = process.argv[3];
const base = `http://127.0.0.1:${port}`;

if (!dir) {
  console.error('需要提供测试目录');
  process.exit(2);
}

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

const page = await waitTarget();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => {
  ws.addEventListener('open', res);
  ws.addEventListener('error', rej);
});
const cdp = new Cdp(ws);
await cdp.send('Runtime.enable');
await new Promise((r) => setTimeout(r, 2000));

/** 拼一个 invoke 表达式。参数走 JSON.stringify，路径里的反斜杠自动转义。 */
const inv = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;

console.log('=== 加密链路 ===');

// 加密目录里的普通文件
const encResult = await cdp.eval(`(async () => {
  const list = await ${inv('browse_directory', { dir })};
  const plain = list.filter(e => !e.is_dir && !e.is_encrypted).map(e => e.path);
  if (!plain.length) return { error: 'no plain files' };
  return await window.__TAURI_INTERNALS__.invoke('encrypt_paths', {
    req: {
      paths: plain,
      password: 'gui-test-pw',
      label: 'gui',
      encrypt_filename: true,
      preserve_extension: false,
      compress: true,
      chunk_size: 262144,
      kdf_profile: 'interactive',
      original: 'keep',
    },
  });
})()`);

check(
  '通过 GUI 命令加密成功',
  encResult && Array.isArray(encResult.items) && encResult.items.length > 0,
  encResult?.items ? `${encResult.items.length} 个` : JSON.stringify(encResult).slice(0, 120),
);
check(
  '加密没有失败项',
  encResult && Array.isArray(encResult.failed) && encResult.failed.length === 0,
  JSON.stringify(encResult?.failed || []).slice(0, 160),
);

// 加密后凭据应自动进入会话——用户不该被要求输入三秒前刚打过的密码
const credAfter = await cdp.eval(inv('credential_count', {}));
check('加密后密码自动进入会话', credAfter > 0, `${credAfter} 个凭据`);

// 刚加密的文件应当立刻可见真名
const visibleAfter = await cdp.eval(`(async () => {
  const scan = await ${inv('scan_directory', { dir, recursive: false })};
  const unlocked = scan.filter(f => f.unlocked);
  return { total: scan.length, unlocked: unlocked.length,
           names: unlocked.map(f => f.name).slice(0, 5) };
})()`);
check(
  '刚加密的文件立刻显示真名',
  visibleAfter.unlocked > 0,
  `${visibleAfter.unlocked}/${visibleAfter.total}：${visibleAfter.names.join(', ')}`,
);

console.log('');
console.log('=== 锁定与解锁链路 ===');

await cdp.eval(inv('lock', {}));
const credLocked = await cdp.eval(inv('credential_count', {}));
check('锁定后凭据清零', credLocked === 0);

const afterLock = await cdp.eval(`(async () => {
  const list = await ${inv('browse_directory', { dir })};
  const enc = list.filter(e => e.is_encrypted);
  return { enc: enc.length,
           leaked: enc.filter(e => e.unlocked || e.real_name).length };
})()`);
check('锁定后加密文件全部回到锁定态（关键反证）', afterLock.leaked === 0, `${afterLock.enc} 个加密文件`);

// 用错误密码解锁必须失败
const wrongPw = await cdp.eval(`(async () => {
  try {
    const r = await ${inv('unlock_directory', { dir, label: 'x', password: 'definitely-wrong' })};
    const scan = await ${inv('scan_directory', { dir, recursive: false })};
    return { threw: false, unlocked: scan.filter(f => f.unlocked).length };
  } catch (e) {
    return { threw: true, code: e?.code ?? String(e) };
  }
})()`);
check(
  '错误密码解不开任何文件（关键反证）',
  wrongPw.threw || wrongPw.unlocked === 0,
  JSON.stringify(wrongPw).slice(0, 120),
);

await cdp.eval(inv('lock', {}));

// 正确密码必须解开
const rightPw = await cdp.eval(`(async () => {
  await ${inv('unlock_directory', { dir, label: 'gui', password: 'gui-test-pw' })};
  const scan = await ${inv('scan_directory', { dir, recursive: false })};
  const un = scan.filter(f => f.unlocked);
  return { unlocked: un.length, total: scan.length, names: un.map(f => f.name).slice(0, 5) };
})()`);
check(
  '正确密码解开文件并显示真名',
  rightPw.unlocked > 0,
  `${rightPw.unlocked}/${rightPw.total}：${rightPw.names.join(', ')}`,
);

// probe_one：已解锁时不该再要求输密码
const probe = await cdp.eval(`(async () => {
  const list = await ${inv('browse_directory', { dir })};
  const enc = list.find(e => e.is_encrypted);
  if (!enc) return { none: true };
  return await window.__TAURI_INTERNALS__.invoke('probe_one', { path: enc.path });
})()`);
check('probe_one 识别出已解锁状态', probe?.is_omy === true && probe?.unlocked === true, JSON.stringify(probe).slice(0, 120));
check('probe_one 返回真实文件名', typeof probe?.name === 'string' && probe.name.length > 0, probe?.name || '');

// 普通文件不能被误判成加密文件
const probePlain = await cdp.eval(`(async () => {
  const list = await ${inv('browse_directory', { dir })};
  const plain = list.find(e => !e.is_dir && !e.is_encrypted);
  if (!plain) return { none: true };
  return await window.__TAURI_INTERNALS__.invoke('probe_one', { path: plain.path });
})()`);
check('普通文件不被误判为加密文件（关键反证）', probePlain?.is_omy === false, JSON.stringify(probePlain).slice(0, 100));

ws.close();
const passed = results.filter((r) => r.ok).length;
console.log('');
console.log(`结果: ${passed} 通过, ${results.length - passed} 失败`);
process.exit(passed === results.length ? 0 : 1);
