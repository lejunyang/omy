/** 右键菜单端到端探针。
 *
 * # 这里踩过的坑（改探针前先读）
 *
 * 1. **填表单值与点提交必须分成两次 evaluate**。Vue 的 DOM 更新在微任务里
 *    批处理，`:disabled` 不会在同一同步块内刷新，点击会被吃掉。表现是
 *    「值填进去了但什么都没发生」，极像产品缺陷。
 * 2. **按结构定位，不按文字**。菜单项文字会互相包含（「重命名」按钮与
 *    对话框标题同文），按文字匹配会命中错的元素。
 * 3. **断言要落在用户看到的那一层**。只查后端状态会让「操作成功了但界面
 *    报错」这类缺陷完全漏过——上一轮正是这样漏掉一个真漏洞。
 */

const PORT = process.argv[2] || '9377';
const WORK_DIR = process.argv[3] || '';
const results = [];

function ok(name, extra) {
  results.push({ pass: true, name, extra: extra || '' });
}
function bad(name, extra) {
  results.push({ pass: false, name, extra: extra || '' });
}

async function cdp() {
  // 用内置全局 WebSocket（驱动以 --experimental-websocket 启动 node），
  // 与仓库其它探针保持一致。改用 import('ws') 会直接崩在 import 上
  let targets = [];
  for (let i = 0; i < 40; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${PORT}/json/list`);
      targets = await r.json();
      if (targets.some((t) => t.webSocketDebuggerUrl)) break;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  const target = targets.find((t) => t.webSocketDebuggerUrl);
  if (!target) throw new Error('CDP 未就绪');

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', rej);
  });
  let id = 0;
  const waiting = new Map();
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && waiting.has(m.id)) {
      const { res, rej } = waiting.get(m.id);
      waiting.delete(m.id);
      if (m.error) rej(new Error(m.error.message));
      else res(m.result);
    }
  });
  const send = (method, params) =>
    new Promise((res, rej) => {
      const mid = ++id;
      waiting.set(mid, { res, rej });
      ws.send(JSON.stringify({ id: mid, method, params }));
      setTimeout(() => {
        if (waiting.has(mid)) {
          waiting.delete(mid);
          rej(new Error(method + ' 超时'));
        }
      }, 20000);
    });
  await send('Runtime.enable');
  const evaluate = async (expr) => {
    const r2 = await send('Runtime.evaluate', {
      expression: expr,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r2.exceptionDetails) {
      throw new Error('页面异常: ' + JSON.stringify(r2.exceptionDetails.exception));
    }
    return r2.result.value;
  };
  return { evaluate, close: () => ws.close() };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 在一个条目上派发右键事件。按结构定位（.card / .cname）。 */
const RIGHT_CLICK = (nameSubstr) => `
(() => {
  const cards = [...document.querySelectorAll('.grid .card')];
  const hit = cards.find((c) => (c.querySelector('.cname')?.textContent || '').includes(${JSON.stringify(nameSubstr)}));
  if (!hit) return { ok: false, why: 'no-card', names: cards.map(c => c.querySelector('.cname')?.textContent) };
  const r = hit.getBoundingClientRect();
  hit.dispatchEvent(new MouseEvent('contextmenu', {
    bubbles: true, cancelable: true,
    clientX: Math.round(r.left + r.width / 2),
    clientY: Math.round(r.top + r.height / 2),
  }));
  return { ok: true };
})()`;

/** 读出当前菜单的所有项：标签、是否禁用、是否危险色。 */
const READ_MENU = `
(() => {
  const menu = document.querySelector('.ctxmenu');
  if (!menu) return null;
  return {
    items: [...menu.querySelectorAll('.mi')].map((b) => ({
      label: (b.querySelector('.mil')?.textContent || '').trim(),
      disabled: b.disabled === true,
      danger: b.classList.contains('danger'),
      hint: b.getAttribute('title') || '',
    })),
    seps: menu.querySelectorAll('.msep').length,
    rect: (() => { const r = menu.getBoundingClientRect(); return { l: r.left, t: r.top, w: r.width, h: r.height }; })(),
  };
})()`;

/** 点菜单里第 n 个标签匹配的项（按结构取，再核对文字）。 */
const CLICK_ITEM = (label) => `
(() => {
  const items = [...document.querySelectorAll('.ctxmenu .mi')];
  const hit = items.find((b) => (b.querySelector('.mil')?.textContent || '').trim() === ${JSON.stringify(label)});
  if (!hit) return { ok: false, why: 'no-item', labels: items.map(b => (b.querySelector('.mil')?.textContent||'').trim()) };
  if (hit.disabled) return { ok: false, why: 'disabled' };
  hit.click();
  return { ok: true };
})()`;

const READ_STATUS = `
(() => ({
  // 成功提示与错误共用 .toast，靠 .err 类区分。
  // 分开读：只读 .toast 的话，一条错误会被当成「有成功提示」
  notice: document.querySelector('.toast:not(.err)')?.textContent?.trim() || '',
  error: document.querySelector('.toast.err')?.textContent?.trim()
      || document.querySelector('.errbox')?.textContent?.trim() || '',
  names: [...document.querySelectorAll('.grid .card .cname')].map(e => e.textContent.trim()),
  menuOpen: !!document.querySelector('.ctxmenu'),
  dlgOpen: !!document.querySelector('.dlg'),
}))()`;

async function main() {
  const { evaluate, close } = await cdp();
  try {
    /* ---------- 0. 先进到测试目录 ---------- */
    await evaluate(`(() => {
      const items = [...document.querySelectorAll('.side .pitem, .side button, .side li')];
      const t = items.find((e) => /文档|Documents/.test(e.textContent || ''));
      if (t) t.click();
      return 'ok';
    })()`);
    await sleep(900);
    const dirName = WORK_DIR.split(/[\\/]/).pop();
    const entered = await evaluate(`(() => {
      const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
      const hit = rows.find((r) => {
        const n = r.querySelector('.cname, .nm');
        return n && n.textContent.trim() === ${JSON.stringify(dirName)};
      });
      if (!hit) return 'not-found';
      hit.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
      return 'ok';
    })()`);
    if (entered === 'ok') ok('进入测试目录');
    else bad('进入测试目录', entered);
    await sleep(1000);

    /* ---------- 1. 菜单能在普通文件上打开 ---------- */
    const r1 = await evaluate(RIGHT_CLICK('plain.txt'));
    if (!r1?.ok) bad('在普通文件上右键', JSON.stringify(r1));
    else ok('在普通文件上右键');
    await sleep(250);

    const m1 = await evaluate(READ_MENU);
    if (!m1) {
      bad('右键弹出菜单', '没有 .ctxmenu');
    } else {
      ok('右键弹出菜单', `${m1.items.length} 项`);
      // 关键项必须都在
      const labels = m1.items.map((i) => i.label);
      for (const want of ['重命名', '新建文件夹', '移到回收站', '永久删除']) {
        if (labels.includes(want)) ok(`菜单含「${want}」`);
        else bad(`菜单含「${want}」`, '实际: ' + labels.join('/'));
      }
      // 永久删除要用危险色——它和「移到回收站」并列，颜色是唯一的即时区分
      const del = m1.items.find((i) => i.label === '永久删除');
      if (del?.danger) ok('永久删除用危险色');
      else bad('永久删除用危险色', JSON.stringify(del));
      // 菜单必须完整落在视口内
      const vw = await evaluate('window.innerWidth');
      const vh = await evaluate('window.innerHeight');
      const inView =
        m1.rect.l >= 0 && m1.rect.t >= 0 &&
        m1.rect.l + m1.rect.w <= vw + 1 && m1.rect.t + m1.rect.h <= vh + 1;
      if (inView) ok('菜单完整在视口内');
      else bad('菜单完整在视口内', `menu=${JSON.stringify(m1.rect)} vp=${vw}x${vh}`);
      // 普通文件不能改密码
      const keyItem = m1.items.find((i) => i.label.includes('密码'));
      if (keyItem && keyItem.disabled) ok('普通文件不能改密码（禁用）');
      else bad('普通文件不能改密码（禁用）', JSON.stringify(keyItem));
      if (keyItem && keyItem.hint) ok('禁用项给出原因', keyItem.hint);
      else bad('禁用项给出原因', '没有 title');
    }

    /* ---------- 2. 右键会先选中该项 ---------- */
    const sel = await evaluate(
      `[...document.querySelectorAll('.grid .card.sel .cname')].map(e => e.textContent.trim())`,
    );
    if (sel.length === 1 && sel[0].includes('plain.txt')) ok('右键先选中该项', sel[0]);
    else bad('右键先选中该项', JSON.stringify(sel));

    /* ---------- 3. 点空白关掉菜单 ---------- */
    await evaluate(`document.querySelector('.ctxlayer')?.click()`);
    await sleep(200);
    const closed = await evaluate(`!document.querySelector('.ctxmenu')`);
    if (closed) ok('点空白关闭菜单');
    else bad('点空白关闭菜单', '菜单还在');

    /* ---------- 4. 新建文件夹 ---------- */
    await evaluate(RIGHT_CLICK('plain.txt'));
    await sleep(250);
    const c4 = await evaluate(CLICK_ITEM('新建文件夹'));
    if (!c4?.ok) bad('点「新建文件夹」', JSON.stringify(c4));
    else ok('点「新建文件夹」');
    await sleep(300);

    // 菜单必须已经关掉：留着会悬在 reload 之后不存在的条目上
    const st4 = await evaluate(READ_STATUS);
    if (!st4.menuOpen) ok('选中菜单项后菜单关闭');
    else bad('选中菜单项后菜单关闭', '菜单还在');
    if (st4.dlgOpen) ok('弹出命名对话框');
    else bad('弹出命名对话框', '没有 .dlg');

    // 先填值
    await evaluate(`
      (() => {
        const i = document.querySelector('#name-input');
        if (!i) return false;
        i.value = '新目录';
        i.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      })()`);
    // 必须另起一次 evaluate：同一同步块里 :disabled 不刷新
    await sleep(400);
    const btn4 = await evaluate(`
      (() => {
        const b = document.querySelector('.dlg .acts .btn.primary');
        if (!b) return { ok: false, why: 'no-button' };
        if (b.disabled) return { ok: false, why: 'still-disabled' };
        b.click();
        return { ok: true };
      })()`);
    if (btn4?.ok) ok('提交新建文件夹');
    else bad('提交新建文件夹', JSON.stringify(btn4));
    await sleep(900);

    const st5 = await evaluate(READ_STATUS);
    if (st5.names.some((n) => n === '新目录')) ok('新文件夹出现在列表里');
    else bad('新文件夹出现在列表里', st5.names.join('/'));
    if (!st5.error) ok('新建后界面没有报错');
    else bad('新建后界面没有报错', st5.error);
    // 必须核对**内容**：提示条 4 秒后才消失，只问「有没有提示」会被
    // 上一步操作留下的过期提示满足（实测中删除那条断言正是这样假通过的）
    if (/文件夹|folder/i.test(st5.notice)) ok('新建后提示说的是新建文件夹', st5.notice);
    else bad('新建后提示说的是新建文件夹', `实际提示: ${st5.notice || '（无）'}`);

    /* ---------- 5. 非法名字要被拦在提交之前 ---------- */
    await evaluate(RIGHT_CLICK('plain.txt'));
    await sleep(250);
    await evaluate(CLICK_ITEM('新建文件夹'));
    await sleep(300);
    await evaluate(`
      (() => {
        const i = document.querySelector('#name-input');
        i.value = '../逃逸';
        i.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      })()`);
    await sleep(400);
    const bad5 = await evaluate(`
      (() => {
        const b = document.querySelector('.dlg .acts .btn.primary');
        const e = document.querySelector('.dlg .errbox');
        return { disabled: b?.disabled === true, err: e?.textContent?.trim() || '' };
      })()`);
    if (bad5.disabled) ok('含 ../ 的名字提交按钮禁用');
    else bad('含 ../ 的名字提交按钮禁用', '按钮是可点的——这是任意路径写入');
    if (bad5.err) ok('含 ../ 时给出原因', bad5.err);
    else bad('含 ../ 时给出原因', '没有提示');
    // 关掉对话框
    await evaluate(`[...document.querySelectorAll('.dlg .acts .btn')].find(b => !b.classList.contains('primary'))?.click()`);
    await sleep(250);

    /* ---------- 6. 重命名 ---------- */
    await evaluate(RIGHT_CLICK('plain.txt'));
    await sleep(250);
    const c6 = await evaluate(CLICK_ITEM('重命名'));
    if (!c6?.ok) bad('点「重命名」', JSON.stringify(c6));
    else ok('点「重命名」');
    await sleep(300);
    // 初始值必须是原名，且只选中主干
    const init6 = await evaluate(`
      (() => {
        const i = document.querySelector('#name-input');
        return { value: i?.value || '', selEnd: i?.selectionEnd ?? -1 };
      })()`);
    if (init6.value === 'plain.txt') ok('重命名初始值是原名', init6.value);
    else bad('重命名初始值是原名', init6.value);
    if (init6.selEnd === 'plain'.length) ok('只选中主干不含扩展名', String(init6.selEnd));
    else bad('只选中主干不含扩展名', `selectionEnd=${init6.selEnd}`);

    await evaluate(`
      (() => {
        const i = document.querySelector('#name-input');
        i.value = '改过的名字.txt';
        i.dispatchEvent(new Event('input', { bubbles: true }));
        return true;
      })()`);
    await sleep(400);
    const btn6 = await evaluate(`
      (() => {
        const b = document.querySelector('.dlg .acts .btn.primary');
        if (!b) return { ok: false, why: 'no-button' };
        if (b.disabled) return { ok: false, why: 'still-disabled' };
        b.click();
        return { ok: true };
      })()`);
    if (btn6?.ok) ok('提交重命名');
    else bad('提交重命名', JSON.stringify(btn6));
    await sleep(900);

    const st6 = await evaluate(READ_STATUS);
    if (st6.names.includes('改过的名字.txt')) ok('列表里出现新名字');
    else bad('列表里出现新名字', st6.names.join('/'));
    if (!st6.names.includes('plain.txt')) ok('旧名字已消失');
    else bad('旧名字已消失', '两个名字同时存在——可能是复制而不是改名');
    if (!st6.error) ok('重命名后界面没有报错');
    else bad('重命名后界面没有报错', st6.error);
    if (/重命名|Renamed/i.test(st6.notice)) ok('重命名后提示说的是重命名', st6.notice);
    else bad('重命名后提示说的是重命名', `实际提示: ${st6.notice || '（无）'}`);

    /* ---------- 7. 加密目录不能重命名 ---------- */
    const encName = await evaluate(`
      (() => {
        const c = [...document.querySelectorAll('.grid .card')].find(
          (x) => x.querySelector('.encbadge'));
        return c ? (c.querySelector('.cname')?.textContent || '').trim() : '';
      })()`);
    if (encName) {
      ok('找到加密目录', encName);
      await evaluate(RIGHT_CLICK(encName));
      await sleep(250);
      const m7 = await evaluate(READ_MENU);
      const rn = m7?.items.find((i) => i.label === '重命名');
      if (rn?.disabled) ok('加密目录的重命名被禁用');
      else bad('加密目录的重命名被禁用', JSON.stringify(rn));
      if (rn?.hint) ok('禁用原因说明了名字会解不开', rn.hint);
      else bad('禁用原因说明了名字会解不开', '没有 title');
      await evaluate(`document.querySelector('.ctxlayer')?.click()`);
      await sleep(200);
    } else {
      bad('找到加密目录', '列表里没有带 .encbadge 的卡片');
    }

    /* ---------- 8. 移到回收站 ---------- */
    await evaluate(RIGHT_CLICK('删我.txt'));
    await sleep(250);
    const c8 = await evaluate(CLICK_ITEM('移到回收站'));
    if (!c8?.ok) bad('点「移到回收站」', JSON.stringify(c8));
    else ok('点「移到回收站」');
    await sleep(1200);
    const st8 = await evaluate(READ_STATUS);
    if (!st8.names.some((n) => n.includes('删我'))) ok('文件已从列表消失');
    else bad('文件已从列表消失', st8.names.join('/'));
    if (!st8.error) ok('删除后界面没有报错');
    else bad('删除后界面没有报错', st8.error);
    // 同上：必须是「回收站」相关的提示，不能是上一步重命名留下的那条
    if (/回收站|Trash/i.test(st8.notice)) ok('删除后提示说的是回收站', st8.notice);
    else bad('删除后提示说的是回收站', `实际提示: ${st8.notice || '（无）'}`);

    /* ---------- 9. 后端自己必须拒绝非法输入（绕过前端校验） ----------
     *
     * 前端的校验只是提前告知，后端才是真正的防线：任何不走对话框的调用
     * 路径都会绕过前端。直接 invoke 命令，确认后端自己会拒。
     *
     * 变异测试证过：只查「提交按钮禁用」的话，把后端校验整段删掉，探针
     * 仍然全绿——因为按钮是前端自己算的。
     */
    const invoke = (cmd, args) => `
      window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})
        .then((v) => 'OK:' + JSON.stringify(v))
        .catch((e) => 'ERR:' + (e && e.code ? e.code : JSON.stringify(e)))`;

    const trav = await evaluate(
      invoke('create_folder', { req: { parent: WORK_DIR, name: '../逃逸' } }),
    );
    if (trav.startsWith('ERR:')) ok('后端拒绝含 ../ 的名字', trav);
    else bad('后端拒绝含 ../ 的名字', trav + ' —— 这是任意路径写入');

    const dots = await evaluate(
      invoke('create_folder', { req: { parent: WORK_DIR, name: '..' } }),
    );
    if (dots.startsWith('ERR:')) ok('后端拒绝 .. 作为名字', dots);
    else bad('后端拒绝 .. 作为名字', dots);

    const dev = await evaluate(
      invoke('create_folder', { req: { parent: WORK_DIR, name: 'CON' } }),
    );
    if (dev.startsWith('ERR:')) ok('后端拒绝 Windows 设备名', dev);
    else bad('后端拒绝 Windows 设备名', dev);

    // 加密目录的改名同理：菜单项禁用是前端算的，后端也必须自己拒
    const encPath = await evaluate(`
      (() => {
        const c = [...document.querySelectorAll('.grid .card')].find(
          (x) => x.querySelector('.encbadge'));
        return c ? (c.querySelector('.cname')?.textContent || '').trim() : '';
      })()`);
    if (encPath) {
      const rn = await evaluate(
        invoke('rename_path', {
          req: { path: WORK_DIR + '\\' + encPath, name: '改名试试' },
        }),
      );
      if (rn.includes('encrypted_dir_rename')) {
        ok('后端拒绝重命名加密目录', rn);
      } else {
        bad('后端拒绝重命名加密目录', rn + ' —— 改名后目录名将永远解不开');
      }
    } else {
      bad('后端拒绝重命名加密目录', '没找到加密目录，无法验证');
    }

    // 反证：合法名字后端必须接受，否则上面几条可能只是「什么都拒」
    const good = await evaluate(
      invoke('create_folder', { req: { parent: WORK_DIR, name: '正常目录' } }),
    );
    if (good.startsWith('OK:')) ok('后端接受合法名字（反证不是一律拒绝）', good);
    else bad('后端接受合法名字（反证不是一律拒绝）', good);

    /* ---------- 输出 ---------- */
    let pass = 0;
    for (const r of results) {
      if (r.pass) pass++;
      console.log(`${r.pass ? 'PASS' : 'FAIL'}  ${r.name}${r.extra ? '  ' + r.extra : ''}`);
    }
    console.log(`\n共 ${results.length} 项，通过 ${pass}，失败 ${results.length - pass}`);
    // 驱动靠这个字符串判定成败
    if (pass === results.length) console.log('PROBE_OK');
    process.exitCode = pass === results.length ? 0 : 1;
  } finally {
    close();
  }
}

main().catch((e) => {
  console.log('探针本身出错: ' + e.message);
  process.exitCode = 2;
});
