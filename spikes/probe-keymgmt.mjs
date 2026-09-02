/** 验证密码管理（增/删/改密码）在真实界面里确实生效。
 *
 * 用法：node --experimental-websocket probe-keymgmt.mjs <port> <目录>
 *
 * # 为什么单测和 CLI 验证都不够
 *
 * core 的单测直接调 rewrite_slots；CLI 的 35 项走命令行。两者都绕过了
 * 「按钮出现条件 → 对话框字段 → invoke 参数名 → 后端分支」这条链。
 * 任何一环把字段名写错（比如 next 写成 new），单测和 CLI 照样全绿，
 * 但用户在界面上点「应用」什么也不会发生。
 *
 * # 判据是"能不能开"，不是"有没有报错"
 *
 * 每次操作后都用 probe_one 实际验证：该开的能开、该不能开的真打不开。
 * 只看 toast 有没有出现是不够的——后端可能报成功但实际没改对。
 *
 * # 元素定位一律按结构
 *
 * 按类名和 value 定位，不按文案：文案会随语言变，而且名字会互相包含
 * （`keymgmt.title` 的文案「密码管理」里含「密码」，而对话框里到处
 * 都是「密码」），文字匹配会命中错的元素。
 */

const port = process.argv[2] || '9360';
const workDir = process.argv[3];
const base = `http://127.0.0.1:${port}`;

async function waitTarget(timeoutMs = 30000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${base}/json/list`);
      if (r.ok) {
        const page = (await r.json()).find((t) => t.type === 'page' && t.webSocketDebuggerUrl);
        if (page) return page;
      }
    } catch { /* 等 */ }
    await new Promise((r) => setTimeout(r, 400));
  }
  throw new Error('CDP 未就绪');
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
    return new Promise((res, rej) => {
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 30000);
      this.pending.set(id, (m) => { clearTimeout(timer); res(m); });
      this.ws.send(JSON.stringify({ id, method, params }));
    });
  }
  async eval(expr) {
    const m = await this.send('Runtime.evaluate', {
      expression: `(async () => { return (${expr}); })()`,
      awaitPromise: true, returnByValue: true,
    });
    if (m.result?.exceptionDetails) {
      // 返回字符串而不是对象：对象插值成 [object Object]，
      // 报错信息全丢，定位问题要多花一整轮
      const d = m.result.exceptionDetails;
      return 'JS-ERR: ' + String(d.exception?.description || d.text || '?')
        .split('\n')[0].slice(0, 120);
    }
    return m.result?.result?.value;
  }
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok: !!ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const inv = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})`;
const ROWS = `[...document.querySelectorAll('.grid .card, .list .lrow')]`;

/** 直接问后端：这个密码能不能打开这个文件。
 *
 * 每次都先 lock 再用目标密码解锁，否则会话里残留的旧凭据会让
 * 「旧密码已失效」这类断言假通过——文件其实打不开了，但会话里
 * 还有别的能开它的密钥。
 */
async function canOpen(c, path, password) {
  await c.eval(inv('lock', {}));
  await sleep(300);
  const dir = path.replace(/\\[^\\]+$/, '');
  await c.eval(inv('unlock_directory', { dir, password })).catch(() => null);
  await sleep(300);
  const p = await c.eval(inv('probe_one', { path }));
  return p && p.unlocked === true;
}

/** 走真实界面解锁：填密码框 + 点提交。
 *
 * 不能直接 invoke('unlock_directory')：那只派生密钥，而列表里的
 * `unlocked` 要靠 scan_directory 的结果回填（store.refreshKnown /
 * 后端 annotate_unlocked）。跳过这一步，加密文件会一直显示成
 * 「🔒 加密文件」，后续按名字定位全都找不到。
 */
async function unlockViaUi(c, password) {
  const opened = await c.eval(`(() => {
    const b = document.querySelector('.titlebar .pill');
    if (!b) return 'no-pill';
    b.click();
    return 'clicked';
  })()`);
  if (opened !== 'clicked') return `pill-failed:${opened}`;
  await sleep(700);
  const filled = await c.eval(`(() => {
    const el = document.getElementById('u-pass');
    if (!el) return 'no-input';
    el.value = ${JSON.stringify(password)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
    return 'ok';
  })()`);
  if (filled !== 'ok') return `fill-failed:${filled}`;
  await sleep(300);
  const sub = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b || b.disabled) return 'no-button';
    b.click();
    return 'clicked';
  })()`);
  if (sub !== 'clicked') return `submit-failed:${sub}`;
  // 解锁要跑 Argon2 再扫描，给足时间
  await sleep(4000);
  return 'ok';
}

/** 找那一行加密文件。
 *
 * 按结构定位，不按文件名：加密文件的行文本是占位名「🔒 加密文件」，
 * 根本不含磁盘文件名 secret.omy。按名字找必然失败，而那看起来像
 * 「列表里没有这个文件」，很容易误判成产品缺陷。
 */
const ENC_ROW = `${ROWS}.find(r => !r.textContent.includes('plain.txt'))`;

/** 在对话框里填密码并提交。
 *
 * 按 id 定位输入框（k-cur / k-new / k-new2），不按出现顺序——
 * 顺序会随「操作」选项变化（remove 时后两个框根本不渲染）。
 */
async function fillAndSubmit(c, action, current, next) {
  const r = await c.eval(`(() => {
    const radio = document.querySelector('.keymgmt input[type=radio][value="${action}"]');
    if (!radio) return 'no-radio:' + ${JSON.stringify(action)};
    radio.click();
    return radio.checked ? 'ok' : 'radio-not-checked';
  })()`);
  if (r !== 'ok') return `radio-failed:${r}`;
  await sleep(300);

  const filled = await c.eval(`(() => {
    const set = (id, v) => {
      const el = document.getElementById(id);
      if (!el) return false;
      el.value = v;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    };
    if (!set('k-cur', ${JSON.stringify(current)})) return 'no-current';
    ${next ? `
    if (!set('k-new', ${JSON.stringify(next)})) return 'no-next';
    if (!set('k-new2', ${JSON.stringify(next)})) return 'no-next2';` : `
    if (document.getElementById('k-new')) return 'next-field-should-not-exist';`}
    return 'ok';
  })()`);
  if (filled !== 'ok') return `fill-failed:${filled}`;
  await sleep(300);

  // 提交按钮按结构定位：对话框里的 .btn.primary
  const sub = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'clicked';
  })()`);
  if (sub !== 'clicked') return `submit-failed:${sub}`;
  await sleep(2500);
  return 'ok';
}

/** 选中一个文件并打开密码管理对话框。 */
async function openDialog(c) {
  const sel = await c.eval(`(() => {
    const row = ${ENC_ROW};
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'clicked';
  })()`);
  if (sel !== 'clicked') return `select-failed:${sel}`;
  await sleep(500);

  // 按钮文案含 🔑，但用类名 + 文案里的 emoji 更稳：工具栏里
  // 只有这一个按钮带钥匙图标
  const btn = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.vtoggle .btn')]
      .find(x => x.textContent.includes('🔑'));
    if (!b) return 'no-button';
    b.click();
    return 'clicked';
  })()`);
  if (btn !== 'clicked') return `button-failed:${btn}`;
  await sleep(700);

  const open = await c.eval(`document.querySelector('.dlg.keymgmt') ? 'open' : 'closed'`);
  return open === 'open' ? 'ok' : `dialog-not-open:${open}`;
}

async function main() {
  const page = await waitTarget();
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res); ws.addEventListener('error', rej);
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  const target = `${workDir}\\secret.omy`;

  console.log('=== 1. 进入测试目录并解锁 ===');
  // 顺序不能反：解锁作用于「当前目录」，在文档目录解锁对子目录无效
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(1300);
  const into = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('omy-keymgmt-test'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('进入测试目录', into === 'dispatched', String(into));
  await sleep(1600);

  const listed = await c.eval(`${ROWS}.length`);
  check('目录里能看到条目', listed > 0, `${listed} 项`);

  const u0 = await unlockViaUi(c, 'pw-one');
  check('走界面解锁成功', u0 === 'ok', String(u0));
  // 自证解锁真的生效了：行上要出现真实文件名而不是占位名。
  // 这一条是整个探针的地基——它不过，后面按结构定位到的都是锁定行，
  // 密码管理按钮永远不会出现
  const shown = await c.eval(`(() => {
    const row = ${ENC_ROW};
    return row ? row.textContent.trim().replace(/\\s+/g, ' ').slice(0, 40) : 'no-row';
  })()`);
  check('解锁后加密文件显示真实名（不再是占位名）',
    !shown.includes('加密文件') && !shown.includes('Encrypted'), shown);

  console.log('\n=== 2. 入口出现条件 ===');
  // 什么都没选时不该有按钮：点了会报错的按钮比没有按钮更糟
  await c.eval(`(() => {
    const b = [...document.querySelectorAll('.vtoggle .btn')]
      .find(x => x.textContent.includes('清除') || x.textContent.includes('Clear'));
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(400);
  const noSel = await c.eval(`[...document.querySelectorAll('.vtoggle .btn')]
    .filter(x => x.textContent.includes('🔑')).length`);
  check('未选中任何文件时没有密码管理按钮', noSel === 0, `找到 ${noSel} 个`);

  // 选中一个未加密文件时也不该有：它还没有密码可管
  const plainSel = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('plain.txt'));
    if (!row) return 'not-found';
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'clicked';
  })()`);
  await sleep(500);
  if (plainSel === 'clicked') {
    const onPlain = await c.eval(`[...document.querySelectorAll('.vtoggle .btn')]
      .filter(x => x.textContent.includes('🔑')).length`);
    check('选中未加密文件时没有密码管理按钮', onPlain === 0, `找到 ${onPlain} 个`);
  } else {
    check('选中未加密文件时没有密码管理按钮', false, '找不到 plain.txt');
  }

  console.log('\n=== 3. add：加一个密码，两个都能开 ===');
  const o1 = await openDialog(c);
  check('能打开密码管理对话框', o1 === 'ok', String(o1));

  // 常驻说明必须在：用户会去界面上找「有几个密码」，找不到会以为是 bug
  const hint = await c.eval(`(() => {
    const h = document.querySelector('.dlg.keymgmt .hint');
    return h ? h.textContent.trim().slice(0, 40) : 'no-hint';
  })()`);
  check('对话框说明了"密码数量查不出来"',
    hint !== 'no-hint' && (hint.includes('查不出来') || hint.includes('cannot be determined')),
    hint);

  if (o1 === 'ok') {
    const r1 = await fillAndSubmit(c, 'add', 'pw-one', 'pw-two');
    check('提交 add 成功', r1 === 'ok', String(r1));
    await sleep(1200);
    check('add 后原密码仍能打开', await canOpen(c, target, 'pw-one'));
    check('add 后新密码能打开', await canOpen(c, target, 'pw-two'));
    check('add 后无关密码打不开', !(await canOpen(c, target, 'pw-nope')));
  }

  console.log('\n=== 4. change：旧密码作废 ===');
  // 重新回到目录并解锁（上面的 canOpen 会 lock）
  const u2 = await unlockViaUi(c, 'pw-two');
  check('用新密码走界面解锁', u2 === 'ok', String(u2));
  const o2 = await openDialog(c);
  check('再次打开对话框', o2 === 'ok', String(o2));
  if (o2 === 'ok') {
    const r2 = await fillAndSubmit(c, 'change', 'pw-two', 'pw-three');
    check('提交 change 成功', r2 === 'ok', String(r2));
    await sleep(1200);
    check('change 后新密码能打开', await canOpen(c, target, 'pw-three'));
    check('change 后旧密码打不开', !(await canOpen(c, target, 'pw-two')));
    // pw-one 是 add 时留下的，change 只保留新密码，所以它也该失效
    check('change 后 add 留下的密码也失效', !(await canOpen(c, target, 'pw-one')));
  }

  console.log('\n=== 5. remove：不要新密码框，且有醒目警示 ===');
  const u3 = await unlockViaUi(c, 'pw-three');
  check('用 pw-three 走界面解锁', u3 === 'ok', String(u3));
  const o3 = await openDialog(c);
  check('第三次打开对话框', o3 === 'ok', String(o3));
  if (o3 === 'ok') {
    const st = await c.eval(`(() => {
      const r = document.querySelector('.keymgmt input[type=radio][value="remove"]');
      if (!r) return 'no-radio';
      r.click();
      return 'ok';
    })()`);
    await sleep(400);
    check('能选中 remove', st === 'ok', String(st));
    // remove 不需要新密码，框不该出现——出现了用户会以为必须编一个
    const noNext = await c.eval(`document.getElementById('k-new') ? 'exists' : 'absent'`);
    check('remove 时新密码框不出现', noNext === 'absent', String(noNext));
    // 这是最重要的一条：不说的话用户会以为"删掉密码"= 对方再也看不到
    const warn = await c.eval(`(() => {
      const w = document.querySelector('.keymgmt .warnbox');
      return w ? w.textContent.trim() : 'no-warnbox';
    })()`);
    check('remove 有"旧副本仍可用旧密码打开"的警示',
      warn !== 'no-warnbox'
      && (warn.includes('副本') || warn.includes('copies')),
      warn.slice(0, 50));
    // 按钮要变红：走到这一步用户已看过文字，但按钮长得一样时手快就点下去了
    const danger = await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      return b ? (b.classList.contains('danger') ? 'danger' : 'normal') : 'no-button';
    })()`);
    check('remove 的确认按钮标记为危险', danger === 'danger', String(danger));

    const r3 = await fillAndSubmit(c, 'remove', 'pw-three', null);
    check('提交 remove 成功', r3 === 'ok', String(r3));
    await sleep(1200);
    check('remove 后当前密码仍能打开', await canOpen(c, target, 'pw-three'));
  }

  console.log('\n=== 6. 错误处理走到界面上 ===');
  const u4 = await unlockViaUi(c, 'pw-three');
  check('错误处理前先解锁', u4 === 'ok', String(u4));
  const o4 = await openDialog(c);
  if (o4 === 'ok') {
    // 新旧相同：前端就该拦住，按钮保持禁用
    await c.eval(`(() => {
      const r = document.querySelector('.keymgmt input[type=radio][value="change"]');
      if (r) r.click();
      const set = (id, v) => {
        const el = document.getElementById(id);
        if (el) { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); }
      };
      set('k-cur', 'pw-three'); set('k-new', 'pw-three'); set('k-new2', 'pw-three');
      return 'ok';
    })()`);
    await sleep(500);
    const dis = await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      return b ? (b.disabled ? 'disabled' : 'enabled') : 'no-button';
    })()`);
    check('新旧密码相同时提交按钮禁用', dis === 'disabled', String(dis));
    const sameMsg = await c.eval(`document.querySelector('.keymgmt .ferr')
      ? 'shown' : 'absent'`);
    check('并给出了原因提示', sameMsg === 'shown', String(sameMsg));

    // 两次新密码不一致：同样该拦住
    await c.eval(`(() => {
      const set = (id, v) => {
        const el = document.getElementById(id);
        if (el) { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); }
      };
      set('k-new', 'aaaa'); set('k-new2', 'bbbb');
      return 'ok';
    })()`);
    await sleep(400);
    const dis2 = await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      return b ? (b.disabled ? 'disabled' : 'enabled') : 'no-button';
    })()`);
    check('两次新密码不一致时提交按钮禁用', dis2 === 'disabled', String(dis2));

    // 当前密码错：这个前端拦不住，必须由后端报错并显示在对话框里
    await c.eval(`(() => {
      const set = (id, v) => {
        const el = document.getElementById(id);
        if (el) { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); }
      };
      set('k-cur', 'totally-wrong'); set('k-new', 'zzzz'); set('k-new2', 'zzzz');
      return 'ok';
    })()`);
    await sleep(400);
    await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      if (b && !b.disabled) b.click();
      return 'ok';
    })()`);
    await sleep(2500);
    const errShown = await c.eval(`(() => {
      const e = document.querySelector('.keymgmt .errbox');
      return e ? e.textContent.trim().slice(0, 40) : 'no-errbox';
    })()`);
    check('当前密码错误时对话框内显示错误', errShown !== 'no-errbox', errShown);
    const stillOpen = await c.eval(`document.querySelector('.dlg.keymgmt')
      ? 'open' : 'closed'`);
    check('出错后对话框不关闭（可以改了重试）', stillOpen === 'open', String(stillOpen));
    // 出错后文件必须没被改动：仍能用原密码打开
    check('出错后文件未被改动', await canOpen(c, target, 'pw-three'));
  }

  console.log('\n=== 7. 移动端可用 ===');
  // 只做 CSS 适配是不够的：桌面靠 dblclick 打开，触屏没有这个手势。
  // 密码管理是按钮点击，但对话框宽度必须自适应——440px 固定宽会把
  // 「应用」按钮推到 360px 屏幕外，界面看着正常却点不到
  await c.send('Emulation.setDeviceMetricsOverride', {
    width: 390, height: 844, deviceScaleFactor: 2, mobile: true,
  });
  await sleep(1200);
  const mob = await c.eval(`window.innerWidth`);
  check('已切到移动端视口', mob <= 768, `${mob}px`);
  const dlgFits = await c.eval(`(() => {
    const d = document.querySelector('.dlg.keymgmt');
    if (!d) return 'no-dialog';
    const r = d.getBoundingClientRect();
    return JSON.stringify({
      w: Math.round(r.width), vw: window.innerWidth,
      overflow: r.right > window.innerWidth + 1 || r.left < -1,
    });
  })()`);
  const parsed = dlgFits && dlgFits !== 'no-dialog' ? JSON.parse(dlgFits) : null;
  check('移动端对话框不横向溢出', parsed && !parsed.overflow, String(dlgFits));
  const btnReachable = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    if (!b) return 'no-button';
    // 先自证只有一层遮罩。上一节故意留着对话框不关，若这里又叠了第二个
    // 对话框（例如中途调过解锁），下层按钮的几何算得出来但点击会被上层
    // 遮罩接住，报成 covered-by——那是探针的状态问题，不是布局缺陷。
    // 分开报，免得又花一轮查错方向
    const ovs = document.querySelectorAll('.overlay.dlg-overlay').length;
    if (ovs !== 1) return 'stacked-overlays:' + ovs;
    // 对话框比屏幕高时要能滚到按钮——这才是移动端真实的操作方式
    b.scrollIntoView({ block: 'center' });
    const r = b.getBoundingClientRect();
    const cx = Math.round(r.left + r.width / 2);
    const cy = Math.round(r.top + r.height / 2);
    if (cx < 0 || cx > window.innerWidth || cy < 0 || cy > window.innerHeight) {
      return 'offscreen:' + cx + ',' + cy;
    }
    // 该点上真正接收点击的是不是这个按钮——被别的层盖住也算点不到
    const hit = document.elementFromPoint(cx, cy);
    return b.contains(hit) || hit === b ? 'reachable' : 'covered-by:' + (hit?.className || '?');
  })()`);
  check('移动端「应用」按钮点得到', btnReachable === 'reachable', String(btnReachable));
  await c.send('Emulation.clearDeviceMetricsOverride');

  const bad = results.filter((r) => !r.ok);
  console.log(`\n=== 合计 ${results.length - bad.length} 通过 / ${bad.length} 失败 ===`);
  ws.close();
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => { console.error('探针异常:', e.message); process.exit(1); });
