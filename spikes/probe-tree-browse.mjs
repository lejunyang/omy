/** 探针：进入 CLI 树形加密出来的目录，确认它能像普通文件夹一样访问。
 *
 * 关键断言是「双击就进去了」——不是「弹出了什么面板」。用户的要求是
 * 「能跟正常目录一样访问」，所以判据是导航路径变了、且里面列出了内容。
 *
 * 名字必须是解出来的中文，而不是磁盘上的 base32。
 *
 * 用法：node --experimental-websocket probe-tree-browse.mjs <port> <目录> <密码>
 */

const port = process.argv[2] || '9375';
const workDir = process.argv[3] || '';
const password = process.argv[4] || '';

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

/** 当前列表里的名字。按结构取，不按文字——名字会互相包含。 */
const NAMES_EXPR = `(() => {
  const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
  return rows.map((r) => (r.querySelector('.cname, .nm')?.textContent || '').trim());
})()`;

async function main() {
  let targets = [];
  for (let i = 0; i < 40; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json/list`);
      targets = await r.json();
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
  console.log('--- 1. 未输密码时：能看到加密目录，但名字是密文 ---');

  await evaluate(`(() => {
    const items = [...document.querySelectorAll('.side .pitem, .side button, .side li')];
    const t = items.find((e) => /文档|Documents/.test(e.textContent || ''));
    if (t) t.click();
    return 'ok';
  })()`);
  await sleep(900);

  const dirName = workDir.split(/[\\/]/).pop();
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
  check('进入测试目录', entered === 'ok', entered);
  await sleep(900);

  const lockedNames = await evaluate(NAMES_EXPR);
  const encName = lockedNames.find((n) => /^[A-Z2-7~]+\.omy$/.test(n));
  check(
    '锁定时加密目录显示为密文名（反证：没解锁就不该看懂）',
    Boolean(encName),
    `列表: ${JSON.stringify(lockedNames)}`,
  );
  check(
    '锁定时看不到原目录名',
    !lockedNames.includes('工作资料'),
    `列表: ${JSON.stringify(lockedNames)}`,
  );

  // 加密目录必须带锁标识，让用户看出它不是普通文件夹
  const badge = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card')];
    const hit = rows.find((r) => /^[A-Z2-7~]+\\.omy$/.test((r.querySelector('.cname')?.textContent || '').trim()));
    if (!hit) return 'no-row';
    return hit.querySelector('.encbadge') ? 'has-badge' : 'no-badge';
  })()`);
  check('加密目录带锁角标（UI 上能看出是加密目录）', badge === 'has-badge', badge);

  // 但它必须仍然被当成目录：图标是文件夹，副标题是「加密文件夹」
  const meta = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card')];
    const hit = rows.find((r) => /^[A-Z2-7~]+\\.omy$/.test((r.querySelector('.cname')?.textContent || '').trim()));
    if (!hit) return 'no-row';
    return (hit.querySelector('.cmeta')?.textContent || '').trim();
  })()`);
  check('副标题标明是加密文件夹', /加密文件夹|Encrypted folder/.test(meta), meta);

  console.log('');
  console.log('--- 2. 输密码解锁 ---');

  // 用列表里那个加密目录旁边的 .omy 文件来触发解锁对话框不可行
  // （树里的文件在子目录里），直接走顶栏的解锁入口
  const openedUnlock = await evaluate(`(() => {
    const btns = [...document.querySelectorAll('button')];
    const b = btns.find((x) => /解锁|Unlock|🔑/.test(x.textContent || ''));
    if (!b) return 'no-unlock-btn';
    b.click();
    return 'ok';
  })()`);
  check('打开解锁对话框', openedUnlock === 'ok', openedUnlock);
  await sleep(500);

  // 填值与点击必须分成两次 evaluate。
  //
  // Vue 的 DOM 更新在微任务里批处理，:disabled 不会在同一个同步块内刷新。
  // 塞进一个 evaluate 的话，click 发生时按钮还是禁用态，点击被吃掉，
  // 表现是「密码填了但什么都没发生」，极像产品缺陷。
  const filled = await evaluate(`(() => {
    const inp = document.querySelector('.dlg input[type="password"]');
    if (!inp) return 'no-input';
    inp.value = ${JSON.stringify(password)};
    inp.dispatchEvent(new Event('input', { bubbles: true }));
    return 'ok';
  })()`);
  check('填入密码', filled === 'ok', filled);
  await sleep(400);

  const submitted = await evaluate(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b) return 'no-submit';
    // 报出禁用状态：将来再失败时能一眼区分「没 flush」与「真禁用」
    if (b.disabled) return 'still-disabled';
    b.click();
    return 'ok';
  })()`);
  check('点到了提交按钮', submitted === 'ok', submitted);

  // 等 Argon2 派生完成
  for (let i = 0; i < 40; i++) {
    await sleep(1000);
    const gone = await evaluate(`document.querySelector('.dlg') ? 'open' : 'closed'`);
    if (gone === 'closed') break;
  }
  await sleep(1200);

  // 必须验解锁的**结果**而不是动作。
  //
  // 原来这里只断言「按钮被点到」，而实测中密码压根没进会话
  // （credential_count 为 0），那条断言依然是 PASS——于是一个真缺陷
  // （站在密文树父目录时 vault_params_of 找不到样本文件）看起来只是
  // 「后面几条莫名失败」。
  const creds = await evaluate(
    `window.__TAURI_INTERNALS__.invoke('credential_count', {}).then(String).catch((e) => 'ERR:' + e)`,
  );
  check('密码真的进了会话（解锁成功）', creds === '1', `credential_count=${creds}`);

  // 光看后端状态不够，必须看**用户看到的那一层**。
  //
  // 变异测试证过：把 tryUnlock 的判据改回「只数加密文件」后，密钥照样进了
  // 会话、目录名照样解出来了，只是界面同时弹一个红色「密码错误」——用户
  // 用正确的密码却被告知密码不对。只查 credential_count 的话这个缺陷会
  // 完全漏过（实测确实漏过了）。
  const errShown = await evaluate(`(() => {
    const el = document.querySelector('.errbox, .err, .error');
    return el ? el.textContent.replace(/\s+/g, ' ').trim() : '';
  })()`);
  check(
    '解锁成功后界面不显示密码错误',
    !/密码|password/i.test(errShown),
    errShown ? `界面报错: ${errShown.slice(0, 60)}` : '无报错',
  );

  const okNotice = await evaluate(`(() => {
    const els = [...document.querySelectorAll('.notice, .toast, .msg')];
    return els.map((e) => e.textContent.replace(/\s+/g, ' ').trim()).join(' | ');
  })()`);
  check(
    '界面给出解锁成功的提示',
    /解锁了|Unlocked/i.test(okNotice),
    okNotice.slice(0, 70) || '（没有任何提示）',
  );

  console.log('');
  console.log('--- 3. 解锁后目录名变成原名 ---');

  const unlockedNames = await evaluate(NAMES_EXPR);
  check(
    '加密目录显示为原名「工作资料」',
    unlockedNames.includes('工作资料'),
    `列表: ${JSON.stringify(unlockedNames)}`,
  );
  check(
    '不再显示 base32 密文名',
    !unlockedNames.some((n) => /^[A-Z2-7~]+\.omy$/.test(n)),
    `列表: ${JSON.stringify(unlockedNames)}`,
  );

  // 状态栏必须把加密目录算进「加密内容」。
  //
  // 这条原本没有任何断言覆盖：加密目录的 is_encrypted 是 false，于是
  // encryptedCount 过滤时漏掉它，一个只含树形加密目录的文件夹会显示
  // 「0 个加密文件」——而用户眼前明明有一个带锁的加密文件夹。
  const status = await evaluate(`(() => {
    const sb = document.querySelector('.statusbar');
    return sb ? sb.textContent.replace(/\s+/g, ' ').trim() : 'no-statusbar';
  })()`);
  check(
    '状态栏把加密目录算进加密内容',
    /1 个加密|1 encrypted/.test(status),
    status.slice(0, 90),
  );

  console.log('');
  console.log('--- 4. 双击进去，像普通文件夹一样 ---');

  const wentIn = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const hit = rows.find((r) => {
      const n = r.querySelector('.cname, .nm');
      return n && n.textContent.trim() === '工作资料';
    });
    if (!hit) return 'not-found';
    hit.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    return 'ok';
  })()`);
  check('双击加密目录', wentIn === 'ok', wentIn);
  await sleep(1500);

  // 进去之后：路径栏应当变了，且列出了里面的内容
  const inside = await evaluate(NAMES_EXPR);
  check(
    '进去后列出了里面的文件，名字是解出来的原名',
    inside.includes('readme.txt'),
    `列表: ${JSON.stringify(inside)}`,
  );
  check(
    '子目录也显示原名',
    inside.includes('文档'),
    `列表: ${JSON.stringify(inside)}`,
  );
  check(
    '里面看不到磁盘上的随机十六进制名',
    !inside.some((n) => /^[0-9a-f]{32}\.omy$/.test(n)),
    `列表: ${JSON.stringify(inside)}`,
  );

  // 再往下一层，证明不是只有第一层能进
  const deeper = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const hit = rows.find((r) => {
      const n = r.querySelector('.cname, .nm');
      return n && n.textContent.trim() === '文档';
    });
    if (!hit) return 'not-found';
    hit.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    return 'ok';
  })()`);
  check('进入加密子目录', deeper === 'ok', deeper);
  await sleep(1200);

  const deepNames = await evaluate(NAMES_EXPR);
  check(
    '第二层里的文件也显示原名',
    deepNames.includes('报告.md'),
    `列表: ${JSON.stringify(deepNames)}`,
  );

  console.log('');
  console.log(`  探针合计 ${pass} 通过 / ${fail} 失败`);
  if (fail === 0) console.log('PROBE_OK');
  process.exit(fail === 0 ? 0 : 1);
}

main().catch((e) => {
  console.log(`  FAIL  探针异常  ${e.message}`);
  process.exit(1);
});
