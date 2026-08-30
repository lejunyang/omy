/** 复现并验证用户报的缺陷：首次加密的文件用同一密码打不开。
 *
 * 用户的实际路径是「桌面上没有任何 omy 文件 → 加密一个 → 打不开」。
 * 关键在**首次**：目录里没有已存在的 vault 时才会走
 * `params_of(kdf_profile)` 那条分支，而缺陷正藏在那里。
 *
 * 第二次加密反而正常，因为它沿用了第一个文件头里那份（错误但自洽的）
 * 参数，恰好与 default 对上——所以只测第二次是测不出问题的。
 *
 * 用法：node --experimental-websocket probe-encrypt-kdf.mjs <port> <工作目录>
 */

const port = process.argv[2] || '9349';
const workDir = process.argv[3] || '';
const base = `http://127.0.0.1:${port}`;
const PASSWORD = '132';

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
      this.pending.set(id, (m) =>
        m.error ? rej(new Error(JSON.stringify(m.error))) : res(m.result),
      );
      this.ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => rej(new Error(`${method} 超时`)), 180000);
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

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}

(async () => {
  const t = await waitTarget();
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws 失败')));
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');

  const sep = workDir.includes('\\') ? '\\' : '/';

  // 每个档位都要单独验，且每次都在**空目录**里做——
  // 这样才走得到「首次加密」那条分支
  for (const profile of ['interactive', 'moderate', 'sensitive']) {
    console.log(`\n=== 档位 ${profile}（空目录，首次加密）===`);
    const dir = `${workDir}${sep}${profile}`;
    const src = `${dir}${sep}plain-${profile}.txt`;

    const enc = await c.inv('encrypt_paths', {
      req: {
        paths: [src],
        password: PASSWORD,
        encrypt_filename: true,
        preserve_extension: false,
        compress: true,
        chunk_size: 262144,
        kdf_profile: profile,
        original: 'keep',
      },
    });
    check(`${profile}: 加密成功`, enc.ok && enc.v.items.length === 1,
      enc.ok ? `失败项 ${JSON.stringify(enc.v.failed)}` : enc.e);
    if (!enc.ok || !enc.v.items.length) continue;

    const out = enc.v.items[0].output;

    // 用户看到的第一个现象：加密后状态栏说「1 个密码已解锁」，
    // 但文件其实是锁定的。判据是 probe_one
    const probe = await c.inv('probe_one', { path: out });
    check(`${profile}: 加密后文件立刻可读（用户报的核心问题）`,
      probe.ok && probe.v.is_omy && probe.v.unlocked,
      probe.ok ? JSON.stringify(probe.v) : probe.e);

    check(`${profile}: 能读回真实文件名`,
      probe.ok && probe.v.name === `plain-${profile}.txt`,
      probe.ok ? String(probe.v.name) : probe.e);

    // 凭据计数必须是 1，不能因为 label 不同而重复计数
    const cnt = await c.inv('credential_count');
    check(`${profile}: 凭据计数为 1`, cnt.ok && cnt.v === 1,
      cnt.ok ? `${cnt.v} 条` : cnt.e);

    // 锁定后重新用同一密码解锁，必须还能打开，且计数仍是 1
    await c.inv('lock');
    const un = await c.inv('unlock_directory', { dir, password: PASSWORD });
    check(`${profile}: 锁定后用同一密码能重新解锁`,
      un.ok && un.v.vaults_unlocked > 0,
      un.ok ? JSON.stringify(un.v) : un.e);

    const probe2 = await c.inv('probe_one', { path: out });
    check(`${profile}: 重新解锁后文件可读`,
      probe2.ok && probe2.v.unlocked,
      probe2.ok ? JSON.stringify(probe2.v) : probe2.e);

    const cnt2 = await c.inv('credential_count');
    check(`${profile}: 重新解锁后凭据仍是 1（不重复计数）`,
      cnt2.ok && cnt2.v === 1, cnt2.ok ? `${cnt2.v} 条` : cnt2.e);

    await c.inv('lock');
  }

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
