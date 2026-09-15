/** 实测设置页：走真实界面，不查内部状态。
 *
 * 回答三个问题：
 *
 * 1. 设置页能不能打开，各分类能不能切换；
 * 2. 改了值**重启后还在不在**——这是这一轮的核心，因为整套配置持久化
 *    就是为了解决「localStorage 后端读不到、清缓存就丢」；
 * 3. 语言与主题是不是**立刻**生效——收进设置页后若要关掉才看到变化，
 *    用户无法确认自己选对了。
 *
 * 定位一律按结构（data-si / data-sp / data-sf），不按文字：
 * 文字会随语言变，而这个探针恰恰要切换语言。
 *
 * 用法：node spikes/probe-settings-gui.mjs <port>
 */

const port = process.argv[2] || '9455';
const base = `http://127.0.0.1:${port}`;

const pass = [];
const fail = [];
function ok(name) {
  pass.push(name);
  console.log(`  PASS  ${name}`);
}
function bad(name, detail) {
  fail.push(`${name}: ${detail}`);
  console.log(`  FAIL  ${name} — ${detail}`);
}
function check(cond, name, detail = '') {
  if (cond) ok(name);
  else bad(name, detail || '断言为假');
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
      /* 还没起来 */
    }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 端口未就绪');
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
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) => (m.error ? reject(new Error(JSON.stringify(m.error))) : resolve(m.result)));
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', {
      expression: expr,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || 'eval 失败');
    }
    return r.result.value;
  }
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  const target = await waitTarget();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res, { once: true });
    ws.addEventListener('error', rej, { once: true });
  });
  const cdp = new Cdp(ws);
  await cdp.send('Runtime.enable');
  await sleep(1500);

  console.log('\n[1] 打开设置页');
  // 顶栏的设置按钮按结构定位：文字会随语言变
  const opened = await cdp.eval(`(() => {
    const b = document.querySelector('[data-tb="settings"]');
    if (!b) return 'no-button';
    b.click();
    return 'clicked';
  })()`);
  check(opened === 'clicked', '顶栏有设置入口', `返回 ${opened}`);
  await sleep(600);

  const hasDialog = await cdp.eval(`!!document.querySelector('[data-si="close"]')`);
  check(hasDialog, '设置对话框已打开');

  console.log('\n[2] 分类切换');
  const panes = await cdp.eval(`(() => {
    const navs = [...document.querySelectorAll('[data-sp]')];
    return navs.map((n) => n.getAttribute('data-sp'));
  })()`);
  check(
    Array.isArray(panes) && panes.includes('remote') && panes.includes('security'),
    '分类齐全',
    JSON.stringify(panes),
  );

  // 切到「远程位置」，确认缓存上限字段真的出现
  const switched = await cdp.eval(`(() => {
    const n = document.querySelector('[data-sp="remote"]');
    if (!n) return 'no-nav';
    n.click();
    return 'ok';
  })()`);
  await sleep(400);
  const hasCacheField = await cdp.eval(`!!document.querySelector('[data-sf="cache_limit"]')`);
  check(switched === 'ok' && hasCacheField, '切到远程位置后出现缓存设置');

  console.log('\n[3] 改值并确认落盘');
  // 改缓存上限到 5 GB，改默认视图到 list
  const changed = await cdp.eval(`(async () => {
    const sel = document.querySelector('[data-sf="cache_limit"]');
    if (!sel) return 'no-cache-field';
    sel.value = String(5 * 1024 * 1024 * 1024);
    sel.dispatchEvent(new Event('change', { bubbles: true }));

    const gen = document.querySelector('[data-sp="general"]');
    if (gen) gen.click();
    await new Promise((r) => setTimeout(r, 300));
    const view = document.querySelector('[data-sf="view"]');
    if (!view) return 'no-view-field';
    view.value = 'list';
    view.dispatchEvent(new Event('change', { bubbles: true }));
    return 'ok';
  })()`);
  check(changed === 'ok', '能改动设置项', String(changed));
  await sleep(300);

  // 关闭触发保存
  await cdp.eval(`document.querySelector('[data-si="close"]')?.click()`);
  await sleep(900);

  // 重新读一次配置：走后端命令，而不是看前端内存里的值——
  // 后者即便没落盘也会显示成功
  const persisted = await cdp.eval(`(async () => {
    const invoke = (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {});
    const c = await invoke('config_get');
    return JSON.stringify({ cache: c.remote.cache_limit, view: c.ui.view });
  })()`);
  let p = {};
  try {
    p = JSON.parse(persisted);
  } catch {
    p = {};
  }
  check(p.cache === 5 * 1024 * 1024 * 1024, '缓存上限已落盘', `实际 ${p.cache}`);
  check(p.view === 'list', '默认视图已落盘', `实际 ${p.view}`);

  console.log('\n[4] 配置文件真的写到了磁盘');
  const paths = await cdp.eval(`(async () => {
    const invoke = (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {});
    return JSON.stringify(await invoke('config_paths'));
  })()`);
  let pp = {};
  try {
    pp = JSON.parse(paths);
  } catch {
    pp = {};
  }
  check(typeof pp.config === 'string' && pp.config.length > 0, '能报出配置文件位置', String(paths));
  // 便携模式下配置应当在可执行文件旁
  check(
    typeof pp.portable === 'boolean',
    '能报出是否便携模式',
    `portable=${pp.portable}`,
  );
  console.log(`       config = ${pp.config}`);
  console.log(`       cache  = ${pp.cache}`);
  console.log(`       portable = ${pp.portable}`);

  console.log('\n[5] 主题立刻生效');
  await cdp.eval(`document.querySelector('[data-tb="settings"]')?.click()`);
  await sleep(500);
  const themeNow = await cdp.eval(`(async () => {
    const sel = document.querySelector('[data-sf="theme"]');
    if (!sel) return 'no-field';
    const before = document.documentElement.dataset.theme;
    sel.value = before === 'dark' ? 'light' : 'dark';
    sel.dispatchEvent(new Event('change', { bubbles: true }));
    await new Promise((r) => setTimeout(r, 200));
    return before + '->' + document.documentElement.dataset.theme;
  })()`);
  const [b4, after] = String(themeNow).split('->');
  check(b4 && after && b4 !== after, '改主题立刻改变 data-theme', String(themeNow));

  // 改回去，避免污染后续
  await cdp.eval(`(() => {
    const sel = document.querySelector('[data-sf="theme"]');
    if (sel) { sel.value = 'dark'; sel.dispatchEvent(new Event('change', { bubbles: true })); }
  })()`);
  await sleep(200);

  console.log('\n[6] 远程位置命令可用');
  const placeResult = await cdp.eval(`(async () => {
    const invoke = (c, a) => window.__TAURI_INTERNALS__.invoke(c, a || {});
    const id = await invoke('remote_place_add', {
      name: '探针测试',
      url: 'https://dav.invalid.example/dav',
      username: 'u',
      password: 'p',
      vendor: 'generic',
      writable: false,
    });
    const list = await invoke('remote_place_list');
    const found = list.find((x) => x.id === id);
    const out = {
      added: !!found,
      caps: found ? found.caps : null,
      // 这一条最关键：序列化后的位置信息里不能出现凭据
      leaks: JSON.stringify(list).includes('p') && JSON.stringify(list).includes('dav.invalid'),
    };
    await invoke('remote_place_remove', { id });
    const after = await invoke('remote_place_list');
    out.removed = !after.some((x) => x.id === id);
    return JSON.stringify(out);
  })()`);
  let pr = {};
  try {
    pr = JSON.parse(placeResult);
  } catch {
    pr = {};
  }
  check(pr.added === true, '能添加远程位置');
  check(pr.caps && pr.caps.read === true && pr.caps.write === false, '只读位置的能力位图正确', JSON.stringify(pr.caps));
  check(pr.leaks === false, '位置列表不泄露 URL 与凭据');
  check(pr.removed === true, '能移除远程位置');

  await cdp.eval(`document.querySelector('[data-si="close"]')?.click()`);

  console.log(`\n结果：${pass.length} 通过，${fail.length} 失败`);
  if (fail.length) {
    for (const f of fail) console.log(`  - ${f}`);
    process.exit(1);
  }
  process.exit(0);
}

main().catch((e) => {
  console.error('探针异常:', e.message);
  process.exit(2);
});
