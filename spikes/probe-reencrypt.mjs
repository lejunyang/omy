/** 验证「重新加密」在真实界面里确实换掉了文件密钥。
 *
 * 用法：node --experimental-websocket probe-reencrypt.mjs <port> <目录>
 *
 * # 为什么单测和 CLI 的 23 项都不够
 *
 * core 的单测直接调 rotate_fek，CLI 走命令行。两者都绕过了
 * 「单选项 → 可选新密码的启用条件 → invoke 参数名 → 后端分支」这条链。
 * 尤其是「新密码可留空」这条：后端允许、前端却把按钮禁着的话，单测和
 * CLI 全绿，而用户在界面上根本提交不了。
 *
 * # 为什么要专门验进度条
 *
 * 轮换是唯一耗时与文件大小成正比的密码操作，而对话框写着「期间请不要
 * 关闭程序」。进度条不出来的话，用户面对静止界面又被告知不能关，无从
 * 判断是在跑还是卡死——而这一点截图和 CSS 审查都发现不了。
 *
 * # 元素定位一律按结构
 *
 * 按类名和 value 定位，不按文案：文案随语言变，且「重新加密」这几个字
 * 在对话框里出现多次（选项名、说明、警示、底部注释），文字匹配会命中
 * 错的元素。
 */

const port = process.argv[2] || '9362';
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
      const timer = setTimeout(() => rej(new Error(`${method} 超时`)), 60000);
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

/** 加密行：按结构定位。锁定时它的文本是占位名，不含磁盘文件名。 */
const ENC_ROW = `${ROWS}.find(r => !r.textContent.includes('plain.txt'))`;

/** 直接问后端：这个密码能不能打开这个文件。
 *
 * 每次都先 lock：否则会话里残留的旧凭据会让「旧密码已失效」假通过——
 * 文件其实打不开了，但会话里还有别的能开它的密钥。
 */
async function canOpen(c, path, password) {
  await c.eval(inv('lock', {}));
  await sleep(300);
  const dir = path.replace(/\\[^\\]+$/, '');
  await c.eval(inv('unlock_directory', { dir, password })).catch(() => null);
  await sleep(400);
  const p = await c.eval(inv('probe_one', { path }));
  return p && p.unlocked === true;
}

/** 关掉所有已打开的对话框，并等到遮罩真的消失。
 *
 * 必须在 unlockViaUi 之前调用。原因有两层：
 * 1) unlockViaUi 用 `.dlg .acts .btn.primary` 找提交按钮，若此时别的对话框
 *    开着，这个选择器可能命中那个对话框的主按钮——等于拿解锁密码去执行
 *    另一个操作；
 * 2) 两层 `.overlay.dlg-overlay` 叠起来时，下层对话框里的按钮几何算得出来、
 *    但点击会被上层遮罩接住，`elementFromPoint` 报「被 overlay 盖住」，
 *    看起来像布局缺陷，实际是探针把对话框叠起来了。
 */
async function closeDialogs(c) {
  for (let i = 0; i < 4; i++) {
    const n = await c.eval(`(() => {
      const ovs = [...document.querySelectorAll('.overlay.dlg-overlay')];
      if (!ovs.length) return 0;
      // 点最上面那一层的取消键；没有取消键就点遮罩自身（@click.self 会关）
      const top = ovs[ovs.length - 1];
      const cancel = [...top.querySelectorAll('.acts .btn')]
        .find(b => !b.classList.contains('primary'));
      if (cancel) cancel.click();
      else top.click();
      return ovs.length;
    })()`);
    if (n === 0) return 'closed';
    await sleep(400);
  }
  const left = await c.eval(`document.querySelectorAll('.overlay.dlg-overlay').length`);
  return left === 0 ? 'closed' : `still-open:${left}`;
}

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
  await sleep(4500);
  return 'ok';
}

/** 选中一行。桌面和移动端的手势**不是同一个**。
 *
 * 桌面：click 即选中。
 * 移动端：click 的语义是「打开」，进入选择模式的唯一手势是长按 500ms
 * （EntryCard 的 pointerdown 起计时器，期间移动不超过 10px 就触发 select）。
 *
 * 在移动端发 click 的后果是把文件打开了，选中数始终为 0，于是 🔑 按钮
 * 不出现、报 no-button——看起来像「移动端没有密码管理入口」这个产品缺陷，
 * 实际入口一直在。AGENTS.md 里记着这条：涉及打开、多选、拖拽的交互，
 * 两端语义不同，探针也得分别处理。
 *
 * 已经选中的行不再重复触发：移动端已在选择模式时，再点/再长按是**取消**选中。
 */
async function selectRow(c, mobile) {
  return c.eval(`(() => {
    const row = ${ENC_ROW};
    if (!row) return 'not-found:' + ${ROWS}.length;
    if (row.classList.contains('sel')) return 'already';
    ${mobile ? `
    // 长按：pointerdown 之后不动，等计时器（500ms）触发 select。
    // 必须带 pointerId/isPrimary，否则 Vue 的 @pointerdown 收不到
    row.dispatchEvent(new PointerEvent('pointerdown', {
      bubbles: true, cancelable: true, pointerId: 1, isPrimary: true,
      clientX: 50, clientY: 50, pointerType: 'touch',
    }));
    return 'pressing';` : `
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'clicked';`}
  })()`);
}

/** 选中加密文件并打开密码管理对话框。 */
async function openDialog(c, mobile = false) {
  const sel = await selectRow(c, mobile);
  if (sel === 'pressing') {
    // 等长按计时器触发（500ms + 余量），再抬手
    await sleep(750);
    await c.eval(`(() => {
      const row = ${ENC_ROW};
      if (row) row.dispatchEvent(new PointerEvent('pointerup', {
        bubbles: true, cancelable: true, pointerId: 1, isPrimary: true,
        clientX: 50, clientY: 50, pointerType: 'touch',
      }));
      return 'ok';
    })()`);
  } else if (sel !== 'clicked' && sel !== 'already') {
    return `select-failed:${sel}`;
  }
  await sleep(500);
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

/** 选中 reencrypt 单选项。 */
async function pickReencrypt(c) {
  const r = await c.eval(`(() => {
    const radio = document.querySelector('.keymgmt input[type=radio][value="reencrypt"]');
    if (!radio) return 'no-radio';
    radio.click();
    return radio.checked ? 'ok' : 'not-checked';
  })()`);
  await sleep(400);
  return r;
}

/** 填字段。next 传 null 表示留空（测"可选"）。 */
async function fill(c, current, next) {
  return c.eval(`(() => {
    const set = (id, v) => {
      const el = document.getElementById(id);
      if (!el) return false;
      el.value = v;
      el.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    };
    if (!set('k-cur', ${JSON.stringify(current)})) return 'no-current';
    ${next === null ? '' : `
    if (!set('k-new', ${JSON.stringify(next)})) return 'no-next';
    if (!set('k-new2', ${JSON.stringify(next)})) return 'no-next2';`}
    return 'ok';
  })()`);
}

/** 点提交，并在等待期间轮询进度条是否出现过。
 *
 * 轮询而不是提交后 sleep 再查一次：进度条只在操作进行中存在，
 * 操作一结束 store 就把 progress 清成 null，事后查必然查不到。
 */
async function submitAndWatchProgress(c, maxMs = 90000) {
  const clicked = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    if (!b) return 'no-button';
    if (b.disabled) return 'disabled';
    b.click();
    return 'clicked';
  })()`);
  if (clicked !== 'clicked') return { ok: false, how: clicked, sawProgress: false };

  let sawProgress = false;
  let sawPct = -1;
  let sawName = '';
  let sawStage = '';
  let sawTotalStages = 0;
  const sawStages = new Set();
  const deadline = Date.now() + maxMs;
  while (Date.now() < deadline) {
    const snap = await c.eval(`(() => {
      const dlg = document.querySelector('.dlg.keymgmt');
      const prog = document.querySelector('.prog');
      const fill = document.querySelector('.prog-fill');
      const pctEl = document.querySelector('.prog-pct');
      const nameEl = document.querySelector('.prog-name');
      const subEl = document.querySelector('.prog-sub');
      return JSON.stringify({
        dlgOpen: !!dlg,
        prog: !!prog,
        width: fill ? fill.style.width : '',
        pct: pctEl ? pctEl.textContent.trim() : '',
        name: nameEl ? nameEl.textContent.trim() : '',
        // 渲染出来的文案，用来查占位符有没有被替换掉
        sub: subEl ? subEl.textContent.trim() : '',
      });
    })()`);
    let s;
    try { s = JSON.parse(snap); } catch { break; }
    if (s.prog) {
      sawProgress = true;
      const n = parseInt(s.pct, 10);
      if (!Number.isNaN(n) && n > sawPct) sawPct = n;
      if (s.name) sawName = s.name;
      if (s.sub) sawStage = s.sub;
      // 阶段数要看后端发来的真实值，不能靠解析渲染文本：
      // 文案换个语言或改个写法，"含 2" 这种断言就莫名失效了
      // 读 .prog 上的 data 属性：那是后端发来的原始值在 DOM 上的投影，
      // 与语言和文案写法无关（解析「第 1 / 2 个」这类文案必然脆）
      const raw = await c.eval(`(() => {
        const el = document.querySelector('.prog');
        return el ? JSON.stringify({ i: el.dataset.stage, n: el.dataset.stages }) : '';
      })()`);
      if (raw) {
        try {
          const o = JSON.parse(raw);
          if (o.n) sawTotalStages = Number(o.n);
          if (o.i) sawStages.add(Number(o.i));
        } catch { /* 忽略 */ }
      }
    }
    // 对话框关闭 = 操作完成
    if (!s.dlgOpen && sawProgress) break;
    if (!s.dlgOpen && Date.now() > deadline - maxMs + 3000) break;
    await sleep(120);
  }
  await sleep(1500);
  return {
    ok: true, how: 'clicked', sawProgress, sawPct, sawName, sawStage,
    sawTotalStages, sawStages: [...sawStages].sort().join(','),
  };
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
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(1300);
  const into = await c.eval(`(() => {
    const row = ${ROWS}.find(r => r.textContent.includes('omy-reenc-test'));
    if (!row) return 'not-found:' + ${ROWS}.length;
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'dispatched';
  })()`);
  check('进入测试目录', into === 'dispatched', String(into));
  await sleep(1600);

  const u0 = await unlockViaUi(c, 'pw-one');
  check('走界面解锁成功', u0 === 'ok', String(u0));
  const shown = await c.eval(`(() => {
    const row = ${ENC_ROW};
    return row ? row.textContent.trim().replace(/\\s+/g, ' ').slice(0, 40) : 'no-row';
  })()`);
  check('解锁后加密文件显示真实名', !shown.includes('加密文件') && !shown.includes('Encrypted'), shown);

  console.log('\n=== 2. 对话框里的 reencrypt 选项与说明 ===');
  const o1 = await openDialog(c);
  check('能打开密码管理对话框', o1 === 'ok', String(o1));

  const radios = await c.eval(`[...document.querySelectorAll('.keymgmt input[type=radio]')]
    .map(r => r.value).join(',')`);
  check('四个操作都在（含 reencrypt）',
    radios === 'add,change,remove,reencrypt', String(radios));

  const pick = await pickReencrypt(c);
  check('能选中 reencrypt', pick === 'ok', String(pick));

  // 两件事都必须说：慢，以及它同样收不回已流出的副本。
  // 只说前者会让用户以为轮换=彻底作废旧密码
  const warn = await c.eval(`(() => {
    const w = [...document.querySelectorAll('.keymgmt .warnbox')]
      .map(x => x.textContent.trim()).join(' | ');
    return w || 'no-warnbox';
  })()`);
  check('reencrypt 有耗时警示',
    warn.includes('大文件') || warn.includes('large files'), warn.slice(0, 60));
  check('reencrypt 说明了「已流出的副本仍能用旧密码打开」',
    warn.includes('副本') || warn.includes('copies'), warn.slice(0, 60));

  // 底部说明必须跟着操作变：对 reencrypt 说「不重新加密」是把实际情况说反了
  const note = await c.eval(`(() => {
    const n = document.querySelector('.keymgmt .note.if');
    return n ? n.textContent.trim() : 'no-note';
  })()`);
  check('底部说明改成了「内容会重新加密」',
    (note.includes('重新加密') || note.includes('re-encrypted'))
    && !note.includes('不重新加密'), note.slice(0, 55));

  const danger = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    return b ? (b.classList.contains('danger') ? 'danger' : 'normal') : 'no-button';
  })()`);
  check('reencrypt 的确认按钮标记为危险', danger === 'danger', String(danger));

  console.log('\n=== 3. 新密码是可选的 ===');
  // 这条最容易做错：后端允许留空，前端却把按钮禁着，
  // 于是「只想换文件密钥、密码不变」在界面上根本提交不了
  await fill(c, 'pw-one', null);
  await sleep(500);
  const canSubmitEmpty = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    return b ? (b.disabled ? 'disabled' : 'enabled') : 'no-button';
  })()`);
  check('只填当前密码就能提交（新密码可留空）', canSubmitEmpty === 'enabled', String(canSubmitEmpty));

  // 新密码框的标签要标明可选，否则用户不知道能留空
  const optLabel = await c.eval(`(() => {
    const l = document.querySelector('.keymgmt label[for="k-new"]');
    return l ? l.textContent.trim() : 'no-label';
  })()`);
  check('新密码标签标明「可留空」',
    optLabel.includes('可留空') || optLabel.toLowerCase().includes('optional'), optLabel);

  // 与当前密码相同也允许：轮换即使密码不变也有意义
  await fill(c, 'pw-one', 'pw-one');
  await sleep(500);
  const sameOk = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    return b ? (b.disabled ? 'disabled' : 'enabled') : 'no-button';
  })()`);
  check('新密码与当前相同时也允许（轮换换的是文件密钥）',
    sameOk === 'enabled', String(sameOk));

  // 但填了两次不一致仍要拦：那是打错了
  await c.eval(`(() => {
    const set = (id, v) => {
      const el = document.getElementById(id);
      if (el) { el.value = v; el.dispatchEvent(new Event('input', { bubbles: true })); }
    };
    set('k-new', 'aaaa'); set('k-new2', 'bbbb');
    return 'ok';
  })()`);
  await sleep(400);
  const misOk = await c.eval(`(() => {
    const b = document.querySelector('.keymgmt .acts .btn.primary');
    return b ? (b.disabled ? 'disabled' : 'enabled') : 'no-button';
  })()`);
  check('两次新密码不一致时仍然拦住', misOk === 'disabled', String(misOk));

  console.log('\n=== 4. 真正执行一次轮换（含进度条）===');
  await fill(c, 'pw-one', 'pw-two');
  await sleep(400);
  const run = await submitAndWatchProgress(c);
  check('提交 reencrypt 成功', run.ok, String(run.how));
  // 这一条只有真机/真文件才验得到：轮换慢，没有进度条用户不知道在跑
  check('轮换期间出现了进度条', run.sawProgress, `最高 ${run.sawPct}%`);
  check('进度条推进到较高百分比', run.sawPct >= 40, `最高 ${run.sawPct}%`);
  check('进度条显示了文件名', !!run.sawName, String(run.sawName));
  // 解密一遍 + 加密一遍 = 两个阶段。查后端发来的真实值，不解析渲染文本
  check('进度按两个阶段汇报（后端 total_files=2）',
    run.sawTotalStages === 2, `total_files=${run.sawTotalStages}`);
  // 文案里的占位符必须被替换掉。这是独立的一条：i18n 的 interpolate 只认
  // {{n}}，写成 {n} 时界面直接显示字面量，而进度本身是对的——两件事分开报，
  // 不然文案 bug 会伪装成阶段数 bug
  check('阶段文案里的占位符已被替换',
    !!run.sawStage && !/[{}]/.test(run.sawStage), String(run.sawStage));

  const closed = await c.eval(`document.querySelector('.dlg.keymgmt') ? 'open' : 'closed'`);
  check('成功后对话框关闭', closed === 'closed', String(closed));
  const notice = await c.eval(`(() => {
    const n = document.querySelector('.notice, .toast, .banner');
    return n ? n.textContent.trim().slice(0, 40) : 'no-notice';
  })()`);
  check('给出了完成提示', notice !== 'no-notice', notice);

  console.log('\n=== 5. 轮换后的密码状态 ===');
  check('新密码能打开', await canOpen(c, target, 'pw-two'));
  check('旧密码已失效', !(await canOpen(c, target, 'pw-one')));
  check('无关密码打不开', !(await canOpen(c, target, 'pw-nope')));

  console.log('\n=== 6. 留空新密码时沿用当前密码 ===');
  await closeDialogs(c);
  const u2 = await unlockViaUi(c, 'pw-two');
  check('用 pw-two 解锁', u2 === 'ok', String(u2));
  const o2 = await openDialog(c);
  check('再次打开对话框', o2 === 'ok', String(o2));
  if (o2 === 'ok') {
    await pickReencrypt(c);
    await fill(c, 'pw-two', null);
    await sleep(400);
    const run2 = await submitAndWatchProgress(c);
    check('留空新密码也能提交', run2.ok, String(run2.how));
    await sleep(1000);
    check('留空后原密码仍可用', await canOpen(c, target, 'pw-two'));
  }

  console.log('\n=== 7. 错误处理 ===');
  await closeDialogs(c);
  const u3 = await unlockViaUi(c, 'pw-two');
  check('错误处理前先解锁', u3 === 'ok', String(u3));
  const o3 = await openDialog(c);
  if (o3 === 'ok') {
    await pickReencrypt(c);
    await fill(c, 'totally-wrong', null);
    await sleep(400);
    await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      if (b && !b.disabled) b.click();
      return 'ok';
    })()`);
    await sleep(4000);
    const errShown = await c.eval(`(() => {
      const e = document.querySelector('.keymgmt .errbox');
      return e ? e.textContent.trim().slice(0, 40) : 'no-errbox';
    })()`);
    check('当前密码错误时对话框内显示错误', errShown !== 'no-errbox', errShown);
    const stillOpen = await c.eval(`document.querySelector('.dlg.keymgmt') ? 'open' : 'closed'`);
    check('出错后对话框不关闭', stillOpen === 'open', String(stillOpen));
    // 最重要的一条：轮换失败绝不能留下半个文件
    check('出错后文件未被改动（仍能用原密码打开）', await canOpen(c, target, 'pw-two'));
  }

  console.log('\n=== 8. 移动端可用 ===');
  // 第 7 节验的是「出错后对话框不关闭」，所以它结束时对话框还开着。
  // 不先关掉，下面的解锁会把第二层遮罩叠在它上面，后续的可点性判定
  // 就会命中上层遮罩而误报成布局缺陷
  const cl = await closeDialogs(c);
  check('进入移动端测试前对话框已关闭', cl === 'closed', String(cl));
  const u4 = await unlockViaUi(c, 'pw-two');
  check('移动端测试前先解锁', u4 === 'ok', String(u4));
  await c.send('Emulation.setDeviceMetricsOverride', {
    width: 390, height: 844, deviceScaleFactor: 2, mobile: true,
  });
  await sleep(1200);
  const mob = await c.eval(`window.innerWidth`);
  check('已切到移动端视口', mob <= 768, `${mob}px`);
  // 移动端必须走长按，click 会打开文件而不是选中
  const o4 = await openDialog(c, true);
  const selCount = await c.eval(`document.querySelectorAll('.card.sel, .lrow.sel').length`);
  check('移动端长按能进入选择模式', selCount > 0, `${selCount} 项已选`);
  if (o4 === 'ok') {
    await pickReencrypt(c);
    // 多了一个选项和一段警示，对话框更高了——移动端要能滚到按钮，
    // 否则界面看着正常但提交不了
    const reach = await c.eval(`(() => {
      const b = document.querySelector('.keymgmt .acts .btn.primary');
      if (!b) return 'no-button';
      // 自证只有一层遮罩：叠了两层时下层按钮点不到，但那是探针的问题，
      // 不是布局的问题——分开报，免得又花一轮去查错的方向
      const ovs = document.querySelectorAll('.overlay.dlg-overlay').length;
      if (ovs !== 1) return 'stacked-overlays:' + ovs;
      b.scrollIntoView({ block: 'center' });
      const r = b.getBoundingClientRect();
      const cx = Math.round(r.left + r.width / 2);
      const cy = Math.round(r.top + r.height / 2);
      if (cx < 0 || cx > window.innerWidth || cy < 0 || cy > window.innerHeight) {
        return 'offscreen:' + cx + ',' + cy;
      }
      const hit = document.elementFromPoint(cx, cy);
      return b.contains(hit) || hit === b ? 'reachable' : 'covered-by:' + (hit?.className || '?');
    })()`);
    check('移动端「应用」按钮点得到', reach === 'reachable', String(reach));
    const overflow = await c.eval(`(() => {
      const d = document.querySelector('.dlg.keymgmt');
      if (!d) return 'no-dialog';
      const r = d.getBoundingClientRect();
      return r.right > window.innerWidth + 1 || r.left < -1 ? 'overflow' : 'fits';
    })()`);
    check('移动端对话框不横向溢出', overflow === 'fits', String(overflow));
    // 四个选项在窄屏上不能挤成一团看不清
    const radioOk = await c.eval(`(() => {
      const rs = [...document.querySelectorAll('.keymgmt .radio')];
      if (rs.length !== 4) return 'count:' + rs.length;
      const bad = rs.filter(r => r.getBoundingClientRect().height < 24);
      return bad.length ? 'too-short:' + bad.length : 'ok';
    })()`);
    check('移动端四个选项都有可点面积', radioOk === 'ok', String(radioOk));
  } else {
    check('移动端「应用」按钮点得到', false, String(o4));
    check('移动端对话框不横向溢出', false, String(o4));
    check('移动端四个选项都有可点面积', false, String(o4));
  }
  await c.send('Emulation.clearDeviceMetricsOverride');

  const bad = results.filter((r) => !r.ok);
  console.log(`\n=== 合计 ${results.length - bad.length} 通过 / ${bad.length} 失败 ===`);
  ws.close();
  process.exit(bad.length ? 1 : 0);
}

main().catch((e) => { console.error('探针异常:', e.message); process.exit(1); });
