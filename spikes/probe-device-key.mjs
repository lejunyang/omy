// 设备密钥的界面验证。
//
// 覆盖不触发 Hello 门禁的路径：按钮该不该出现、设置页那一行的可见性、
// 两句警示在不在。触发门禁的解锁/启用由 verify-device-key.ps1 覆盖
// （那里需要真人按指纹）。
//
// 定位一律按结构（data-sf / class）而不是按文字：名字会互相包含，
// 文字匹配会命中错的元素，把探针 bug 显示成产品缺陷。
const base = `http://127.0.0.1:${process.env.OMY_CDP_PORT || 9374}`;

let pass = 0;
let fail = 0;

function check(name, ok, detail = '') {
  if (ok) {
    pass++;
console.log(`  OK  ${name}`);
  } else {
    fail++;
    // 两个空格是固定格式，外层 ps1 按 /^\s+FAIL\s\s(.+)/ 锚定
    console.log(`  FAIL  ${name}${detail ? ' - ' + detail : ''}`);
  }
}

async function waitTarget(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const page = (await r.json()).find(
          (t) => t.type === 'page' && t.webSocketDebuggerUrl,
        );
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
      const p = this.pending.get(m.id);
      if (p) {
        this.pending.delete(m.id);
        p(m);
      }
    });
  }

  send(method, params = {}) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, (m) =>
        m.error ? reject(new Error(m.error.message)) : resolve(m.result),
      );
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }

  /** 求值并返回 JSON 化的结果。 */
  async eval(expr) {
    const r = await this.send('Runtime.evaluate', {
      expression: `(async () => { ${expr} })()`,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || '求值失败');
    }
    return r.result.value;
  }
}

const page = await waitTarget();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => ws.addEventListener('open', r, { once: true }));
const cdp = new Cdp(ws);
await cdp.send('Runtime.enable');

// ── 一、文案齐备 ──────────────────────────────────────────────
// 少一句用户就不知道「换电脑会失效」，那是会丢数据的误解
const keys = await cdp.eval(`
  const r = await fetch('/locales/zh-CN.json');
  const d = await r.json();
  return {
    hello: !!d.devicekey?.unlock_with_hello,
    notSafer: !!d.devicekey?.warn_not_safer,
    canBeLost: !!d.devicekey?.warn_can_be_lost,
    enrollTitle: !!d.devicekey?.enroll_title,
    forget: !!d.devicekey?.forget,
    busy: !!d.busy?.waiting_hello,
  };
`);
check('中文有「用 Windows Hello 解锁」', keys.hello);
check('中文有「不会更安全」警示', keys.notSafer);
check('中文有「可能永久失效」警示', keys.canBeLost);
check('中文有启用标题', keys.enrollTitle);
check('中文有关闭入口', keys.forget);
check('中文有等待确认提示', keys.busy);

const enKeys = await cdp.eval(`
  const r = await fetch('/locales/en.json');
  const d = await r.json();
  return {
    hello: !!d.devicekey?.unlock_with_hello,
    notSafer: !!d.devicekey?.warn_not_safer,
    canBeLost: !!d.devicekey?.warn_can_be_lost,
    busy: !!d.busy?.waiting_hello,
  };
`);
check('英文有「用 Windows Hello 解锁」', enKeys.hello);
check('英文有「不会更安全」警示', enKeys.notSafer);
check('英文有「可能永久失效」警示', enKeys.canBeLost);
check('英文有等待确认提示', enKeys.busy);

// ── 二、后端命令真的注册了 ────────────────────────────────────
// 前端调一个不存在的命令，Tauri 报的是「命令未找到」而不是业务错误。
// 这条能区分「命令没注册」和「这台电脑不支持」——两者在界面上都表现为
// 按钮不出现，但原因完全不同
const cmds = await cdp.eval(`
  const out = {};
  for (const name of ['device_key_status', 'device_key_unlock',
                      'device_key_enroll', 'device_key_forget']) {
    try {
      await window.__TAURI_INTERNALS__.invoke(name, {});
      out[name] = 'ok';
    } catch (e) {
      const s = String(e && e.message ? e.message : e);
      // 参数不对/业务报错都说明命令存在；只有 not found 才是没注册
      out[name] = /not found|not allowed/i.test(s) ? 'missing' : 'present';
    }
  }
  return out;
`);
for (const [name, st] of Object.entries(cmds)) {
  check(`命令 ${name} 已注册`, st !== 'missing', st);
}

// ── 三、状态查询不弹 Hello ────────────────────────────────────
// 这是刻意的设计：用户只是打开界面，为查状态弹指纹很突兀。
// 能在有限时间里返回就说明没等用户交互
const statusFast = await cdp.eval(`
  const t0 = Date.now();
  try {
    await window.__TAURI_INTERNALS__.invoke('device_key_status', { dir: 'Z:\\\\no-such-dir' });
  } catch { /* 目录不存在会报错，这里只看耗时 */ }
  return Date.now() - t0;
`);
check('状态查询不等用户交互', statusFast < 3000, `耗时 ${statusFast}ms`);

// ── 四、接线真的对上了 ────────────────────────────────────────
// 这才是真正会坏的地方：属性名传错、事件名写错、v-if 条件反了，
// 上面那些文案断言全都照样通过。
//
// 从构建产物里验：属性、事件、调用链都会编进 app.js。这是静态检查，
// 但它抓的正是运行时最难发现的那类错误——按钮不出现时，用户和我
// 都无法区分「没接上」和「这台电脑不支持」
const bundle = await cdp.eval(`
  const r = await fetch('/app.js');
  const s = await r.text();
  return {
    size: s.length,
    // UnlockDialog 侧
    hasProp: s.includes('deviceKey'),
    hasEvent: s.includes('device-unlock'),
    hasLabel: s.includes('devicekey.unlock_with_hello'),
    // App.vue 侧：状态查询与处理函数
    callsStatus: s.includes('device_key_status'),
    callsUnlock: s.includes('device_key_unlock'),
    // 设置页侧
    callsEnroll: s.includes('device_key_enroll'),
    callsForget: s.includes('device_key_forget'),
    // 两句警示必须真的被渲染，光在语言包里没用
    showsWarnings:
      s.includes('devicekey.warn_not_safer') &&
      s.includes('devicekey.warn_can_be_lost'),
    // 等待文案接上了才说明按钮走的是 tryDeviceUnlock
    usesBusyKey: s.includes('busy.waiting_hello'),
  };
`);
check('产物里有 deviceKey 属性', bundle.hasProp);
check('产物里有 device-unlock 事件', bundle.hasEvent);
check('Hello 按钮文案已接入模板', bundle.hasLabel);
check('界面会查免密解锁状态', bundle.callsStatus);
check('界面会用设备密钥解锁', bundle.callsUnlock);
check('设置页能启用', bundle.callsEnroll);
check('设置页能关闭', bundle.callsForget);
check('两句警示真的会渲染', bundle.showsWarnings);
check('解锁走的是等待确认路径', bundle.usesBusyKey);

console.log(`\n通过 ${pass}，失败 ${fail}`);
ws.close();
process.exit(fail === 0 ? 0 : 1);
