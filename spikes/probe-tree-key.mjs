/** 树形改密码端到端探针。
 *
 * # 这里踩过的坑（改探针前先读）
 *
 * 1. **不要 import('/src/store.js')**。release 构建里前端是打包过的，
 *    `/src/` 根本不存在，探针会崩在 import 上；就算 dev 模式能加载，
 *    那也是**另一个模块实例**，读到的状态跟界面上的不是同一份。
 *    要读状态就从 DOM 读，要验后端就直接 invoke。
 * 2. **填表单值与点提交必须分成两次 evaluate**。Vue 的 DOM 更新在微任务里
 *    批处理，`:disabled` 不会在同一同步块内刷新，点击会被吃掉。
 * 3. **按结构定位，不按文字**。
 * 4. **核对提示的内容而非有无**。`.toast` 4 秒才消失，只查「有没有提示」
 *    会被上一步的过期提示满足。
 */

const PORT = process.argv[2] || '9378';
const WORK_DIR = process.argv[3] || '';
const ENC_NAME = process.argv[4] || '';
const results = [];

// 参数没传到就直接说清楚：否则后面是一句莫名其妙的
// "Cannot read properties of undefined"，看着像产品缺陷
if (!WORK_DIR || !ENC_NAME) {
  console.log('FAIL 参数缺失: argv=' + JSON.stringify(process.argv.slice(2)));
  process.exit(1);
}

const ok = (name, extra) => results.push({ pass: true, name, extra: extra || '' });
const bad = (name, extra) => results.push({ pass: false, name, extra: extra || '' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function cdp() {
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
    ws.onopen = res;
    ws.onerror = () => rej(new Error('ws 连接失败'));
  });
  let seq = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) {
      const p = pending.get(d.id);
      pending.delete(d.id);
      if (d.error) p.rej(new Error(JSON.stringify(d.error)));
      else p.res(d.result);
    }
  };
  const send = (method, params) =>
    new Promise((res, rej) => {
      const id = ++seq;
      pending.set(id, { res, rej });
      ws.send(JSON.stringify({ id, method, params }));
      setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          rej(new Error(`${method} 超时`));
        }
      }, 30000);
    });
  await send('Runtime.enable', {});
  const evaluate = async (expression) => {
    const r = await send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.exceptionDetails) {
      throw new Error(
        r.exceptionDetails.exception?.description ?? JSON.stringify(r.exceptionDetails),
      );
    }
    return r.result.value;
  };
  return { evaluate, close: () => ws.close() };
}

const INVOKE = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})
     .then((v) => 'OK:' + JSON.stringify(v))
     .catch((e) => 'ERR:' + (e && e.code ? e.code : JSON.stringify(e)))`;

async function main() {
  const { evaluate, close } = await cdp();
  try {
    /* ---------- 0. 进到测试目录 ---------- */
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
    await sleep(1200);

    /* ---------- 1. 加密目录在列表里 ---------- */
    const listed = await evaluate(`(() => {
      const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
      return rows.map(r => ({
        name: (r.querySelector('.cname, .nm')?.textContent || '').trim(),
        badge: !!r.querySelector('.encbadge'),
      }));
    })()`);
    const encRow = listed.find((e) => e.badge);
    if (encRow) ok('列表里能看到加密目录', encRow.name);
    else bad('列表里能看到加密目录', JSON.stringify(listed).slice(0, 240));

    /* ---------- 1.5 先解锁 ---------- */
    // keyManageable 要求 unlocked：改密码得先能打开这棵树。
    // 用户实际流程也是先解锁才看得到内容，探针不能跳过这一步
    const unlocked = await evaluate(
      INVOKE('unlock_directory', { dir: WORK_DIR, password: 'tree-old-pw' }),
    );
    if (unlocked.startsWith('OK:')) ok('用密码解锁这棵树', unlocked.slice(0, 80));
    else bad('用密码解锁这棵树', unlocked.slice(0, 200));
    await evaluate(`(() => {
      const b = [...document.querySelectorAll('button')].find(x => x.textContent.includes('⟳'));
      if (b) b.click();
      return 'ok';
    })()`);
    await sleep(1500);

    // 记下改密码**之前**的真实路径，后面比对是否改名。
    // 不能靠界面显示名：解锁后显示的是解密后的原名，改密码不会让它变
    const before = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
    let pathBefore = '';
    if (before.startsWith('OK:')) {
      const arr = JSON.parse(before.slice(3));
      pathBefore = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
    }
    if (pathBefore) ok('解锁后后端仍标记它是加密目录');
    else bad('解锁后后端仍标记它是加密目录', before.slice(0, 200));

    /* ---------- 2. 选中后 🔑 入口出现 ---------- */
    // 这是本次改动的核心：改之前 keyManageable 要求 is_encrypted，
    // 而加密目录那个字段恒为 false，按钮压根不出现
    // 按结构定位：解锁后这一行显示的是**解密后的原名**，
    // 按密文名找必然 not-found。加密条目带 .encbadge
    const clicked = await evaluate(`(() => {
      const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
      const hit = rows.find(r => r.querySelector('.encbadge'));
      if (!hit) return 'not-found:' + rows.length + '行都没有 .encbadge';
      hit.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      return 'ok';
    })()`);
    if (clicked === 'ok') ok('选中加密目录');
    else bad('选中加密目录', clicked);
    await sleep(500);

    // 按结构定位：文字里的 🔑 也出现在凭据指示器「🔑 未输入密码」上，
    // 按文字找会命中那个、点开解锁对话框，看着像「密码管理坏了」
    const btn = await evaluate(`(() => {
      const b = [...document.querySelectorAll('.vtoggle button')]
        .find(x => x.textContent.includes('🔑'));
      return { exists: !!b, disabled: b ? !!b.disabled : null, label: b?.textContent?.trim() ?? '' };
    })()`);
    if (btn.exists) ok('🔑 按钮出现（改动前不会出现）');
    else bad('🔑 按钮出现（改动前不会出现）', JSON.stringify(btn));
    if (btn.exists && !btn.disabled) ok('🔑 按钮可点击');
    else bad('🔑 按钮可点击', JSON.stringify(btn));

    /* ---------- 3. 对话框只给「更换密码」 ---------- */
    await evaluate(`(() => {
      const b = [...document.querySelectorAll('.vtoggle button')]
        .find(x => x.textContent.includes('🔑'));
      if (b) b.click();
      return 'ok';
    })()`);
    await sleep(700);
    const dlg = await evaluate(`(() => {
      const f = document.querySelector('.dlg.keymgmt');
      if (!f) return { open: false };
      return {
        open: true,
        radios: [...f.querySelectorAll('.radio input[type=radio]')].map(r => r.value),
        checked: f.querySelector('.radio input:checked')?.value ?? '',
        warnText: [...f.querySelectorAll('.warnbox')].map(w => w.textContent.trim()).join(' | '),
        tag: f.querySelector('.ktag')?.textContent?.trim() ?? '',
        hint: f.querySelector('.hint')?.textContent?.trim() ?? '',
      };
    })()`);
    if (dlg.open) {
      ok('密码管理对话框打开');
    } else {
      // 光报「没打开」查不出原因，把当时的界面状态一起抓下来
      const why = await evaluate(`(() => ({
        dialogs: [...document.querySelectorAll('.dlg')].map(d => d.className),
        overlays: document.querySelectorAll('.overlay').length,
        buttons: [...document.querySelectorAll('button')]
          .map(b => b.textContent.trim().slice(0, 14)).filter(Boolean).slice(0, 24),
        toast: document.querySelector('.toast')?.textContent?.trim() ?? '',
        selected: document.querySelectorAll('.grid .card.sel, .list .lrow.sel').length,
      }))()`);
      bad('密码管理对话框打开', JSON.stringify(why).slice(0, 400));
    }

    if (dlg.radios?.length === 1 && dlg.radios[0] === 'change') {
      ok('只提供 change 一个选项');
    } else {
      bad('只提供 change 一个选项', JSON.stringify(dlg.radios));
    }
    if (dlg.checked === 'change') ok('change 默认选中');
    else bad('change 默认选中', dlg.checked);
    if (dlg.tag) ok('目标行标出「整个文件夹」', dlg.tag);
    else bad('目标行标出「整个文件夹」', '没有 .ktag');
    // 警示必须真的讲了「名字会变」，只查有没有 warnbox 是不够的
    if (/名字|name/i.test(dlg.warnText ?? '')) ok('警示说明了目录名会变');
    else bad('警示说明了目录名会变', String(dlg.warnText ?? '(无)').slice(0, 140));
    if (/一个密码|one password/i.test(dlg.hint ?? '')) ok('说明整棵树只有一个密码');
    else bad('说明整棵树只有一个密码', String(dlg.hint ?? '(无)').slice(0, 140));
    // 反证：不该出现对树不成立的 slots_hidden 文案
    if (!/无法判断|查不出|cannot|impossible/i.test(dlg.hint ?? '')) {
      ok('没有显示对树不成立的 slots_hidden 文案');
    } else {
      bad('没有显示对树不成立的 slots_hidden 文案', String(dlg.hint ?? '').slice(0, 140));
    }

    /* ---------- 4. 走完整改密码流程 ---------- */
    await evaluate(`(() => {
      const f = document.querySelector('.dlg.keymgmt');
      if (!f) return 'no-dialog';
      const set = (el, v) => {
        el.value = v;
        el.dispatchEvent(new Event('input', { bubbles: true }));
      };
      const ins = [...f.querySelectorAll('input[type=password]')];
      set(ins[0], 'tree-old-pw');
      if (ins[1]) set(ins[1], 'tree-new-pw');
      if (ins[2]) set(ins[2], 'tree-new-pw');
      return 'ok';
    })()`);
    await sleep(600);
    const submitState = await evaluate(`(() => {
      const b = document.querySelector('.dlg.keymgmt .acts .btn.primary');
      return { disabled: b ? !!b.disabled : null };
    })()`);
    if (submitState.disabled === false) ok('填好后提交按钮可用');
    else bad('填好后提交按钮可用', JSON.stringify(submitState));

    await evaluate(`(() => {
      const b = document.querySelector('.dlg.keymgmt .acts .btn.primary');
      if (b) b.click();
      return 'ok';
    })()`);
    await sleep(4000);

    const after = await evaluate(`(() => {
      const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
      return {
        dlgOpen: !!document.querySelector('.dlg.keymgmt'),
        toast: document.querySelector('.toast')?.textContent?.trim() ?? '',
        isErr: !!document.querySelector('.toast.err'),
        names: rows.map(r => (r.querySelector('.cname, .nm')?.textContent || '').trim()),
        badges: rows.filter(r => r.querySelector('.encbadge')).length,
        selected: rows.filter(r => r.classList.contains('sel')).length,
      };
    })()`);
    if (!after.dlgOpen) ok('成功后对话框关闭');
    else bad('成功后对话框关闭', '对话框还开着');
    if (!after.isErr) ok('没有报错');
    else bad('没有报错', after.toast);
    // 核对内容而非有无：4 秒的提示条会让上一步的过期提示蒙混过关
    if (/改写|更换|rewritten|changed/i.test(after.toast)) ok('提示说明改了密码', after.toast);
    else bad('提示说明改了密码', after.toast);

    /* ---------- 5. 目录改名后界面指向新路径 ---------- */
    const stillOne = after.badges === 1;
    if (stillOne) ok('列表里仍有且只有一个加密目录');
    else bad('列表里仍有且只有一个加密目录', `badges=${after.badges}`);
    // 比对后端返回的真实路径，不看界面显示名——解锁后显示的是解密后的
    // 原名，它本来就不随密码变，拿它判断会得到一个恒真的假通过
    const midList = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
    let pathAfter = '';
    if (midList.startsWith('OK:')) {
      const arr = JSON.parse(midList.slice(3));
      pathAfter = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
    }
    if (pathAfter && pathBefore && pathAfter !== pathBefore) {
      ok('磁盘上的目录名确实变了');
    } else {
      bad('磁盘上的目录名确实变了', `前 ${pathBefore.slice(-24)} 后 ${pathAfter.slice(-24)}`);
    }
    // 这条最关键：选中项要跟到新路径，否则用户看到「文件夹不见了」
    if (after.selected === 1) ok('选中项跟随到新路径');
    else bad('选中项跟随到新路径', `选中 ${after.selected} 项`);

    /* ---------- 6. 后端直接校验 ---------- */
    // 只查前端会漏过整段后端校验被删的情况
    // 目标必须是一个**不含任何 .omy 的**目录：WORK_DIR 里装着密文子树，
    // find_any_file 会递归找到样本文件，于是先撞上密码校验、报
    // wrong_password——那样测的就不是 not_a_tree 这条路径了
    const mk = await evaluate(
      INVOKE('create_folder', { req: { parent: WORK_DIR, name: '普通目录' } }),
    );
    if (mk.startsWith('OK:')) ok('建出一个不含密文的普通目录');
    else bad('建出一个不含密文的普通目录', mk.slice(0, 160));
    const plainDir = WORK_DIR + '\\普通目录';
    const plain = await evaluate(
      INVOKE('manage_key', {
        req: { path: plainDir, action: 'change', current: 'tree-new-pw', next: 'zzz-123' },
      }),
    );
    if (plain.startsWith('ERR:')) ok('后端拒绝对普通目录改密码', plain);
    else bad('后端拒绝对普通目录改密码', plain);
    if (plain.includes('not_a_tree')) ok('错误码是 not_a_tree');
    else bad('错误码是 not_a_tree', plain);

    // 取新路径：从后端列目录拿，不猜名字
    const listed2 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
    let encPath = '';
    if (listed2.startsWith('OK:')) {
      const arr = JSON.parse(listed2.slice(3));
      encPath = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
    }
    if (encPath) ok('后端能列出改名后的加密目录');
    else bad('后端能列出改名后的加密目录', listed2.slice(0, 200));

    if (encPath) {
      const addRej = await evaluate(
        INVOKE('manage_key', {
          req: { path: encPath, action: 'add', current: 'tree-new-pw', next: 'second-9' },
        }),
      );
      if (addRej.startsWith('ERR:')) ok('后端拒绝对树 add 密码', addRej);
      else bad('后端拒绝对树 add 密码', addRej);
      if (addRej.includes('tree_only_change')) ok('错误码是 tree_only_change');
      else bad('错误码是 tree_only_change', addRej);

      // 反证：合法的 change 必须仍被接受，否则「一律拒绝」也能通过上面两条
      const good = await evaluate(
        INVOKE('manage_key', {
          req: { path: encPath, action: 'change', current: 'tree-new-pw', next: 'third-77' },
        }),
      );
      if (good.startsWith('OK:')) {
        ok('反证：合法的 change 仍被接受');
        const v = JSON.parse(good.slice(3));
        if (v.is_tree === true) ok('返回 is_tree=true');
        else bad('返回 is_tree=true', JSON.stringify(v));
        if (v.files_changed >= 2) ok('返回改写的文件数', String(v.files_changed));
        else bad('返回改写的文件数', JSON.stringify(v));
        if (v.new_path && v.new_path !== encPath) ok('返回了变化后的新路径');
        else bad('返回了变化后的新路径', JSON.stringify(v));
      } else {
        bad('反证：合法的 change 仍被接受', good);
      }
    }
  } finally {
    close();
    // 汇总放在 finally：中途抛异常时，前面已经查出来的结果同样要打出来。
    // 否则只剩一句异常信息，看不出是哪一步先失败的
    report();
  }
}

function report() {
  let pass = 0;
  for (const r of results) {
    if (r.pass) pass++;
    console.log(`  ${r.pass ? 'PASS' : 'FAIL'} ${r.name}${r.extra ? ' — ' + r.extra : ''}`);
  }
  console.log('');
  console.log(`通过 ${pass} / 失败 ${results.length - pass}`);
}

main()
  .then(() => {
    process.exit(results.some((r) => !r.pass) ? 1 : 0);
  })
  .catch((e) => {
    // 打完整栈：只打 message 的话看不出是哪一行，
    // 「Cannot read properties of undefined」可能来自十几个地方
    console.log('FAIL 探针异常: ' + e.message);
    console.log(String(e.stack).split('\n').slice(0, 6).join('\n'));
    process.exit(1);
  });
