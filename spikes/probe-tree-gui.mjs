/** GUI 树形模式探针：走真实界面选文件夹、选逐个加密、确认落盘。
 *
 * 关键是**只有在选了文件夹时**才该出现模式选择，且选 tree 时必须弹出泄露
 * 提示。后者是 N6 要求的告知——CSS 审查和截图都看不出「提示到底显示了没」，
 * 只有真的查 DOM 才算。
 *
 * 用法：node --experimental-websocket probe-tree-gui.mjs <port> <目录>
 */

const port = process.argv[2] || '9374';
const workDir = process.argv[3] || '';

let pass = 0;
let fail = 0;
function check(name, cond, detail) {
  if (cond) {
    pass++;
    console.log(`  PASS  ${name}  ${detail ?? ''}`);
  } else {
    fail++;
    console.log(`  FAIL  ${name}  ${detail ?? ''}`);
  }
}

async function cdpTargets() {
  const r = await fetch(`http://127.0.0.1:${port}/json/list`);
  return r.json();
}

let ws;
let msgId = 0;
const pending = new Map();

function send(method, params) {
  const id = ++msgId;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((resolve, reject) => {
    pending.set(id, { resolve, reject });
    setTimeout(() => {
      if (pending.has(id)) {
        pending.delete(id);
        reject(new Error(`${method} 超时`));
      }
    }, 30000);
  });
}

async function evaluate(expr) {
  const r = await send('Runtime.evaluate', {
    expression: expr,
    returnByValue: true,
    awaitPromise: true,
  });
  if (r.exceptionDetails) {
    throw new Error(r.exceptionDetails.exception?.description || 'JS 异常');
  }
  return r.result?.value;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function main() {
  // 等 WebView 就绪
  let targets = [];
  for (let i = 0; i < 40; i++) {
    try {
      targets = await cdpTargets();
      if (targets.some((t) => t.webSocketDebuggerUrl)) break;
    } catch {}
    await sleep(500);
  }
  const target = targets.find((t) => t.webSocketDebuggerUrl);
  if (!target) {
    console.log('  FAIL  CDP 未就绪');
    process.exit(1);
  }

  ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', rej);
  });
  ws.addEventListener('message', (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      const { resolve, reject } = pending.get(m.id);
      pending.delete(m.id);
      if (m.error) reject(new Error(m.error.message));
      else resolve(m.result);
    }
  });

  await send('Runtime.enable');
  await sleep(600);

  console.log('');
  console.log('--- 1. 进入测试目录 ---');

  // 侧栏进「文档」
  const clicked = await evaluate(`(() => {
    const items = [...document.querySelectorAll('.side .pitem, .side button, .side li')];
    const t = items.find((e) => /文档|Documents/.test(e.textContent || ''));
    if (!t) return 'no-doc-entry';
    t.click();
    return 'clicked';
  })()`);
  check('侧栏进入文档目录', clicked === 'clicked', clicked);
  await sleep(900);

  // 双击进测试目录。按结构定位（.nm/.cname），不按整行文本——
  // 整行含图标与类型后缀，精确比较必然失败
  const dirName = workDir.split(/[\\/]/).pop();
  const entered = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const hit = rows.find((r) => {
      const n = r.querySelector('.cname, .nm');
      return n && n.textContent.trim() === ${JSON.stringify(dirName)};
    });
    if (!hit) {
      const names = rows.map((r) => (r.querySelector('.cname, .nm')?.textContent || '').trim());
      return 'not-found:' + JSON.stringify(names.slice(0, 8));
    }
    hit.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    return 'ok';
  })()`);
  check('双击进入测试目录', entered === 'ok', entered);
  await sleep(900);

  console.log('');
  console.log('--- 2. 只选文件（非文件夹）时不该出现模式选择 ---');

  const pickedFile = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const hit = rows.find((r) => {
      const n = r.querySelector('.cname, .nm');
      return n && n.textContent.trim() === 'loose.txt';
    });
    if (!hit) return 'no-file';
    hit.click();
    return 'ok';
  })()`);
  check('选中一个普通文件', pickedFile === 'ok', pickedFile);
  await sleep(400);

  const openedForFile = await evaluate(`(() => {
    const btns = [...document.querySelectorAll('button')];
    const b = btns.find((x) => /🔒/.test(x.textContent || ''));
    if (!b) return 'no-encrypt-btn';
    b.click();
    return 'ok';
  })()`);
  check('打开加密对话框', openedForFile === 'ok', openedForFile);
  await sleep(500);

  const modeAbsentForFile = await evaluate(`(() => {
    const dlg = document.querySelector('.dlg');
    if (!dlg) return 'no-dlg';
    return dlg.querySelector('input[value="tree"]') ? 'present' : 'absent';
  })()`);
  check('纯文件时模式选择不出现（反证）', modeAbsentForFile === 'absent', modeAbsentForFile);

  // 关掉对话框
  await evaluate(`(() => {
    const b = [...document.querySelectorAll('.dlg .acts .btn')].find(
      (x) => !x.classList.contains('primary'),
    );
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(400);

  console.log('');
  console.log('--- 3. 选文件夹后出现模式选择与泄露提示 ---');

  const pickedDir = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const hit = rows.find((r) => {
      const n = r.querySelector('.cname, .nm');
      return n && n.textContent.trim() === '工作资料';
    });
    if (!hit) {
      const names = rows.map((r) => (r.querySelector('.cname, .nm')?.textContent || '').trim());
      return 'not-found:' + JSON.stringify(names.slice(0, 8));
    }
    hit.click();
    return 'ok';
  })()`);
  check('选中文件夹', pickedDir === 'ok', pickedDir);
  await sleep(400);

  await evaluate(`(() => {
    const b = [...document.querySelectorAll('button')].find((x) => /🔒/.test(x.textContent || ''));
    if (b) b.click();
    return 'ok';
  })()`);
  await sleep(500);

  const modePresent = await evaluate(`(() => {
    const dlg = document.querySelector('.dlg');
    if (!dlg) return 'no-dlg';
    const c = dlg.querySelector('input[value="container"]');
    const t = dlg.querySelector('input[value="tree"]');
    if (!c || !t) return 'missing-radio';
    return c.checked ? 'container-default' : 'tree-default';
  })()`);
  // 默认必须是 container：它藏住结构，是更安全的那个
  check('模式选择出现且默认打包（更安全的默认值）', modePresent === 'container-default', modePresent);

  const leakHiddenAtFirst = await evaluate(`(() => {
    const dlg = document.querySelector('.dlg');
    return dlg && dlg.querySelector('.warnbox') ? 'shown' : 'hidden';
  })()`);
  check('默认（打包）时不显示泄露提示（反证）', leakHiddenAtFirst === 'hidden', leakHiddenAtFirst);

  // 切到 tree
  const switched = await evaluate(`(() => {
    const t = document.querySelector('.dlg input[value="tree"]');
    if (!t) return 'no-radio';
    t.click();
    return 'ok';
  })()`);
  check('切到逐个加密', switched === 'ok', switched);
  await sleep(400);

  const leakShown = await evaluate(`(() => {
    const w = document.querySelector('.dlg .warnbox');
    return w ? w.textContent.trim() : 'no-warnbox';
  })()`);
  check('选 tree 后弹出泄露提示（N6 要求的告知）', /多少文件|how many files/.test(leakShown), leakShown.slice(0, 70));

  console.log('');
  console.log('--- 4. 填密码并提交 ---');

  const filled = await evaluate(`(() => {
    const set = (el, v) => {
      el.value = v;
      el.dispatchEvent(new Event('input', { bubbles: true }));
    };
    const p1 = document.getElementById('e-pass');
    const p2 = document.getElementById('e-pass2');
    if (!p1 || !p2) return 'no-inputs';
    set(p1, 'tree-gui-pw');
    set(p2, 'tree-gui-pw');
    // 用最快的档位，否则 Argon2 会让探针等很久
    const fast = document.querySelector('.dlg input[value="interactive"]');
    if (fast) fast.click();
    return 'ok';
  })()`);
  check('填入密码', filled === 'ok', filled);
  await sleep(400);

  const submitted = await evaluate(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b) return 'no-submit';
    if (b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  check('已提交加密', submitted === 'ok', submitted);

  // 等加密完成（Argon2 + 逐个文件加密）
  let done = false;
  for (let i = 0; i < 60; i++) {
    await sleep(1000);
    const gone = await evaluate(`document.querySelector('.dlg') ? 'open' : 'closed'`);
    if (gone === 'closed') {
      done = true;
      break;
    }
  }
  check('加密完成、对话框已关闭', done, done ? '' : '超时仍未关闭');

  const notice = await evaluate(`(() => {
    const n = document.querySelector('.notice, .toast, .msg');
    return n ? n.textContent.trim() : 'no-notice';
  })()`);
  check('显示了结果提示', notice !== 'no-notice', notice.slice(0, 60));

  console.log('');
  console.log(`  探针合计 ${pass} 通过 / ${fail} 失败`);
  if (fail === 0) console.log('PROBE_OK');
  process.exit(fail === 0 ? 0 : 1);
}

main().catch((e) => {
  console.log(`  FAIL  探针异常  ${e.message}`);
  process.exit(1);
});
