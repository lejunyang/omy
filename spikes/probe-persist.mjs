// 远程位置持久化探针：分两个阶段跑，中间会重启 GUI。
//
//   node probe-persist.mjs <cdp端口> add    <dav端口>   第一次：添加位置
//   node probe-persist.mjs <cdp端口> verify <dav端口>   第二次：验证恢复
//
// 必须分两次进程调用，因为要验的正是「关掉应用再打开」。
// 写在一个脚本里会让人误以为同一个进程内就能验完。

const [, , cdpPortArg, phase, davPortArg] = process.argv;
const cdpPort = Number(cdpPortArg || 9471);
const davPort = Number(davPortArg || 8793);

const PLACE_NAME = 'omy-persist-test';
const PASSWORD = 'verify-secret-pw';
const USERNAME = 'testuser';

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
    return r.result?.value;
  }
}

async function main() {
  const target = await waitTarget();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res, { once: true });
    ws.addEventListener('error', rej, { once: true });
  });
  const cdp = new Cdp(ws);
  await cdp.send('Runtime.enable');
  await sleep(1200);
  await cdp.eval(`window.__p = { invoke: (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {}) }; true`);

  const url = `http://127.0.0.1:${davPort}/`;

  if (phase === 'add') {
    console.log('\n[添加阶段]');

    const status = await cdp.eval(`window.__p.invoke('remote_secret_status')`);
    console.log(`        凭据保护状态: ${status}`);
    // 两种状态都是合法的，断言的是「必须是这两者之一」而不是「必须可用」：
    // 凭据库可能真的用不了（Linux 无 Secret Service、Windows 凭据管理器
    // 存满），那时正确行为是降级而不是报错。后面按状态分别断言。
    check(
      status === 'protected' || status === 'unavailable',
      '能报出明确的凭据保护状态',
      String(status),
    );
    // 把状态写到 stdout 供外层脚本按环境选择断言
    console.log(`SECRET_STATUS=${status}`);

    const id = await cdp.eval(`(async () => {
      try {
        return await window.__p.invoke('remote_place_add', {
          name: ${JSON.stringify(PLACE_NAME)},
          url: ${JSON.stringify(url)},
          username: ${JSON.stringify(USERNAME)},
          password: ${JSON.stringify(PASSWORD)},
          vendor: 'generic',
          writable: false,
        });
      } catch (e) { return 'ERR:' + e; }
    })()`);
    check(typeof id === 'string' && !id.startsWith('ERR'), '位置已添加', String(id));

    // 非法地址要在添加这一步就被拒，而不是等浏览时才失败
    const badAdd = await cdp.eval(`(async () => {
      try {
        await window.__p.invoke('remote_place_add', {
          name: 'bad', url: 'not a url at all', username: '', password: '',
          vendor: 'generic', writable: false,
        });
        return 'NO_ERROR';
      } catch (e) { return 'REJECTED'; }
    })()`);
    check(badAdd === 'REJECTED', '非法地址在添加时就被拒绝', String(badAdd));

    console.log(`\n${pass.length} 通过 / ${fail.length} 失败`);
    if (fail.length === 0) console.log('ADD_OK');
    ws.close();
    process.exit(fail.length === 0 ? 0 : 1);
  }

  if (phase === 'verify') {
    console.log('\n[恢复阶段]');

    const status = await cdp.eval(`window.__p.invoke('remote_secret_status')`);
    const protectedNow = status === 'protected';
    console.log(`        凭据保护状态: ${status}`);

    const list = await cdp.eval(`window.__p.invoke('remote_place_list')`);
    const arr = Array.isArray(list) ? list : [];
    console.log(`        恢复到 ${arr.length} 个位置: ${arr.map((p) => p.name).join(', ')}`);

    const place = arr.find((p) => p.name === PLACE_NAME);
    // 这一条与凭据库可用与否无关：位置本身必须回来。
    // 凭据库用不了时也要让用户看到「这个位置还在，只是要重新登录」，
    // 而不是一片空白让人以为配置丢了。
    check(!!place, '重启后位置仍在', JSON.stringify(arr).slice(0, 200));

    // 那条测试时加进去的坏地址不该被保存——它当初就没建成功
    check(!arr.some((p) => p.name === 'bad'), '被拒绝的位置没有混进配置');

    if (place) {
      check(place.caps && place.caps.read === true, '能力位图一并恢复');

      // 真正的验证：用恢复出来的密码去连一次。
      // 只看「位置还在」不够——密码没解回来的话列表照样有这一项，
      // 但一浏览就 401。必须真的发一次请求才算验过。
      const browsed = await cdp.eval(`(async () => {
        try {
          const r = await window.__p.invoke('remote_browse', {
            placeId: ${JSON.stringify(place.id)},
            dir: '',
          });
          return JSON.stringify({ ok: true, n: Array.isArray(r) ? r.length : -1 });
        } catch (e) { return JSON.stringify({ ok: false, e: String(e) }); }
      })()`);
      let br = {};
      try { br = JSON.parse(browsed); } catch { br = {}; }

      if (protectedNow) {
        check(
          br.ok === true,
          '用恢复出来的凭据能真正连上服务端（证明密码解密正确）',
          String(browsed).slice(0, 200),
        );
        check(br.n >= 1, '能列出云端文件', `条目数 ${br.n}`);
      } else {
        // 凭据库不可用时密码本就没保存，连不上是**预期行为**。
        // 这里要验的是「优雅降级」：位置还在、能报错，而不是崩溃。
        console.log('        （凭据库不可用，跳过密码可用性断言——密码本就未保存）');
        check(
          typeof br.ok === 'boolean',
          '凭据缺失时浏览有明确结果而不是崩溃',
          String(browsed).slice(0, 160),
        );
      }
    }

    // 前端拿到的列表里依然不能有凭据
    const j = JSON.stringify(arr);
    check(!j.includes(PASSWORD), '恢复后的列表不泄露密码');
    check(!j.includes(USERNAME), '恢复后的列表不泄露用户名');

    console.log(`\n${pass.length} 通过 / ${fail.length} 失败`);
    if (fail.length === 0) console.log('VERIFY_OK');
    ws.close();
    process.exit(fail.length === 0 ? 0 : 1);
  }

  throw new Error(`未知阶段：${phase}`);
}

main().catch((e) => { console.error('探针异常:', e.message); process.exit(1); });
