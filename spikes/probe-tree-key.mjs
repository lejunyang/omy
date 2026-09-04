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

import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

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

/** 把一棵密文树里每个 .omy 文件的内容摘要列出来。
 *
 * 比对摘要而不是「文件是否存在」：轮换成功与否的唯一可观察证据就是密文
 * 变没变，文件名和数量在轮换前后都是一样的。
 */
function hashTree(dir) {
  const out = [];
  const walk = (d) => {
    let ents = [];
    try {
      ents = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of ents) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith('.omy'))
        out.push(crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex'));
    }
  };
  walk(dir);
  return out.sort();
}

/** 找一棵树里第一个密文文件（深度优先）。用于挑一个"受害者"制造失败。 */
function firstOmy(dir) {
  let ents = [];
  try {
    ents = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return '';
  }
  for (const e of ents) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      const got = firstOmy(p);
      if (got) return got;
    } else if (e.name.endsWith('.omy')) {
      return p;
    }
  }
  return '';
}

/** 在树里按文件名找回某个文件的当前完整路径。
 *
 * 改密码会改目录名但不改文件名，所以目录名变过之后，按名字找比拼路径可靠
 */
function findByName(dir, name) {
  let ents = [];
  try {
    ents = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return '';
  }
  for (const e of ents) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      const got = findByName(p, name);
      if (got) return got;
    } else if (e.name === name) {
      return p;
    }
  }
  return '';
}

/** 算一棵树里**不在** exclude 集合中的密文文件的 hash（排序后）。
 *
 * 用来断言「某次操作没动清单外的文件」。按内容 hash 而不是按名字比：
 * 改密码不改文件名，只有内容会变
 */
function hashTreeExcept(dir, exclude) {
  const out = [];
  const walk = (d) => {
    let ents = [];
    try {
      ents = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of ents) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith('.omy') && !exclude.has(p))
        out.push(crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex'));
    }
  };
  walk(dir);
  return out.sort();
}

/** 把任意值转成一段能安全打印的短描述。
 *
 * 直接写 `JSON.stringify(x).slice(0, n)` 会在 x 为 undefined 时抛异常——
 * stringify 返回的是 undefined 而不是字符串。而这恰好发生在**断言失败**
 * 的路径上：探针在本该报告失败的地方自己崩掉，后面的断言一条都不跑。
 * 变异测试才会暴露这种问题，正常全绿时完全看不出来。
 */
const desc = (v, n = 200) => {
  let s;
  try {
    s = typeof v === 'string' ? v : JSON.stringify(v);
  } catch {
    s = String(v);
  }
  return String(s ?? v).slice(0, n);
};

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

// 错误要连 params 一起带回来：部分失败时「是哪些文件」就在 params.files 里，
// 只取 code 的话，界面该显示的清单在探针里根本看不到
const INVOKE = (cmd, args) =>
  `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)})
     .then((v) => 'OK:' + JSON.stringify(v))
     .catch((e) => 'ERR:' + JSON.stringify(e && e.code ? { code: e.code, params: e.params } : e))`;

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

    // 树支持改密码与轮换，但 add / remove 做不到（目录名只认第一个 KEK），
    // 列出一个做不到的选项比不列糟得多
    const rs = dlg.radios ?? [];
    if (rs.length === 2 && rs.includes('change') && rs.includes('reencrypt')) {
      ok('提供 change 与 reencrypt 两个选项');
    } else {
      bad('提供 change 与 reencrypt 两个选项', JSON.stringify(rs));
    }
    if (!rs.includes('add') && !rs.includes('remove')) {
      ok('不列出树做不到的 add / remove');
    } else {
      bad('不列出树做不到的 add / remove', JSON.stringify(rs));
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
    /* ---------- 7. 轮换（reencrypt）---------- */
    // 取当前加密目录（前面 change 过，名字又变了一次）
    const listed3 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
    let rotDir = '';
    if (listed3.startsWith('OK:')) {
      const arr = JSON.parse(listed3.slice(3));
      rotDir = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
    }
    if (rotDir) ok('找到待轮换的加密目录');
    else bad('找到待轮换的加密目录', listed3.slice(0, 200));

    if (rotDir) {
      // 直接用 fs 读磁盘：探针是普通 node 进程，跟产品同机。
      // 不为了测试往产品里加调试命令
      const before = hashTree(rotDir);

      const rot = await evaluate(
        INVOKE('manage_key', {
          req: { path: rotDir, action: 'reencrypt', current: 'third-77', next: '' },
        }),
      );
      if (rot.startsWith('OK:')) {
        ok('后端接受对树 reencrypt');
        const v = JSON.parse(rot.slice(3));
        // payload_rewritten 是「这次真的重写了载荷」的唯一标记。
        // 少了它，一个把 reencrypt 当 change 处理的实现完全无声
        if (v.payload_rewritten === true) ok('返回 payload_rewritten=true');
        else bad('返回 payload_rewritten=true', JSON.stringify(v));
        // 不换密码 → 目录名密钥没变 → 路径不该变
        if (v.new_path === rotDir) ok('不换密码时目录名不变');
        else bad('不换密码时目录名不变', `${rotDir} -> ${v.new_path}`);
      } else {
        bad('后端接受对树 reencrypt', rot.slice(0, 200));
      }

      // 密文必须真的变了：这是轮换与改密码唯一的可观察差别
      const after = hashTree(rotDir);
      const unchanged = after.filter((x) => before.includes(x));
      if (after.length > 0 && unchanged.length === 0) ok('轮换后密文确实变了');
      else bad('轮换后密文确实变了', `${unchanged.length}/${after.length} 份没变`);
      // 文件数不变：轮换是原地覆盖，不该多出或少掉文件
      if (after.length === before.length && after.length > 0)
        ok('轮换后文件数不变', String(after.length));
      else bad('轮换后文件数不变', `${before.length} -> ${after.length}`);

      // 轮换后原密码仍可用（没换密码）
      const un = await evaluate(
        INVOKE('unlock_directory', { dir: rotDir, password: 'third-77' }),
      );
      if (un.startsWith('OK:')) ok('轮换后原密码仍可用');
      else bad('轮换后原密码仍可用', un.slice(0, 160));
    }

    /* ---------- 8. 界面上的轮换选项 ---------- */
    // 后端支持了不等于用户点得到。改动前树只给 change 一个选项
    // 先刷新列表。第 6、7 段都走后端 invoke 改了目录名，界面并不知道，
    // 那一行的 path 指向已经不存在的目录——点它选不中，工具条也就不渲染
    await evaluate(`(() => {
      const b = [...document.querySelectorAll('button')].find((x) =>
        x.textContent.includes('⟳'),
      );
      if (b) b.click();
      return 'ok';
    })()`);
    await sleep(1500);

    // 选中行的写法必须与前面成功的那段一致：`.click()` 在这套界面上
    // 不会触发选中，要派发冒泡的 MouseEvent
    const openDlg = await evaluate(`(() => {
      const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
      const t = rows.find((r) => r.querySelector('.encbadge'));
      if (!t) return 'no-row:' + rows.length;
      t.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      return 'ok';
    })()`);
    await sleep(500);
    if (openDlg === 'ok') {
      // 必须真的点 🔑 才会开对话框——前一段结尾已经把它关掉了。
      // 按结构定位：文字里的 🔑 也出现在凭据指示器上
      const opened = await evaluate(`(() => {
        const b = [...document.querySelectorAll('.vtoggle button')]
          .find((x) => x.textContent.includes('🔑'));
        if (!b) return 'no-btn';
        if (b.disabled) return 'disabled';
        b.click();
        return 'ok';
      })()`);
      await sleep(700);
      if (opened === 'ok') ok('能再次打开密码管理对话框');
      else bad('能再次打开密码管理对话框', opened);
      const acts = await evaluate(`(() => {
        const d = document.querySelector('.dlg.keymgmt');
        if (!d) return '[]';
        return JSON.stringify([...d.querySelectorAll('.radio input')].map((i) => i.value));
      })()`);
      let list = [];
      try {
        list = JSON.parse(acts);
      } catch {}
      if (list.includes('reencrypt')) ok('界面提供轮换选项', acts);
      else bad('界面提供轮换选项', acts);
      if (list.includes('change')) ok('界面仍提供改密码选项');
      else bad('界面仍提供改密码选项', acts);
      // add / remove 对树做不到，不能列出来
      if (!list.includes('add') && !list.includes('remove')) ok('界面不列出树做不到的 add/remove');
      else bad('界面不列出树做不到的 add/remove', acts);

      // 选中轮换后，新密码应变成可选（不填也能提交）
      const optional = await evaluate(`(() => {
        const d = document.querySelector('.dlg.keymgmt');
        if (!d) return 'no-dlg';
        const r = [...d.querySelectorAll('.radio input')].find((i) => i.value === 'reencrypt');
        if (!r) return 'no-radio';
        r.click();
        return 'ok';
      })()`);
      await sleep(200);
      if (optional === 'ok') {
        const st = await evaluate(`(() => {
          const d = document.querySelector('.dlg.keymgmt');
          if (!d) return '{}';
          const cur = d.querySelector('#k-cur');
          const btn = d.querySelector('.acts .btn.primary');
          return JSON.stringify({
            warn: [...d.querySelectorAll('.warnbox')].map((w) => w.textContent.trim().slice(0, 30)),
            hasCur: !!cur,
            btnDisabled: btn ? btn.disabled : null,
          });
        })()`);
        // 轮换的耗时警示必须出现，否则用户不知道为什么要等这么久
        if (st.includes('重写') || st.toLowerCase().includes('rewritten') || st.includes('逐个'))
          ok('显示了轮换的耗时警示', st.slice(0, 120));
        else bad('显示了轮换的耗时警示', st.slice(0, 200));
        // 键名原文出现说明 i18n 键写错了层级（嵌套 vs 扁平）
        if (!st.includes('keymgmt.')) ok('警示是译文而非键名');
        else bad('警示是译文而非键名', st.slice(0, 200));
      }
      // 关掉对话框，别影响后面
      await evaluate(`(() => {
        const b = [...document.querySelectorAll('.dlg.keymgmt .acts button')]
          .find((x) => !x.classList.contains('primary'));
        if (b) b.click();
        return 'ok';
      })()`);
      await sleep(200);
    } else {
      bad('能选中加密目录打开对话框', openDlg);
    }

    /* ---------- 9. 部分失败要说清是哪些文件 ---------- */
    // 制造真实的部分失败：往树里塞一个损坏的 .omy。它通不过 open，
    // 其余文件正常，于是 rekey 报部分失败。
    //
    // 不这么做就没法测这条路径——而「只给一个错误码、不说是哪些文件」
    // 恰好是这次要补的缺口，没有断言的话它退化了也没人知道
    const listed4 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
    let partDir = '';
    if (listed4.startsWith('OK:')) {
      const arr = JSON.parse(listed4.slice(3));
      partDir = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
    }
    if (partDir) {
      // 把树里一个**真实**文件设为只读来制造失败。
      //
      // 不要塞一个自己加密的文件进去冒充：每次独立加密都会生成新的
      // vault_salt，而改密码是用树的 salt 派生 KEK 的，那个文件必然
      // wrong_password——那是假数据造成的，不是产品缺陷。
      //
      // 只读文件 open 成功、写回失败，正是真实场景（被别的程序占用、权限
      // 不对）走的那条路径，而且解除只读后重试必然能成功
      const victim = firstOmy(partDir);
      if (victim) fs.chmodSync(victim, 0o444);
      const part = await evaluate(
        INVOKE('manage_key', {
          req: { path: partDir, action: 'change', current: 'third-77', next: 'part-88' },
        }),
      );
      if (part.startsWith('ERR:')) ok('部分失败时报错而非静默成功');
      else bad('部分失败时报错而非静默成功', part.slice(0, 200));

      let perr = {};
      try {
        perr = JSON.parse(part.slice(4));
      } catch {
        // 解不出来也要继续：探针自己崩掉会丢掉前面所有已收集的结果，
        // 最后只剩一句 "Cannot read properties of undefined"
      }
      if (perr.code === 'tree_partial') ok('错误码是 tree_partial');
      else bad('错误码是 tree_partial', part.slice(0, 200));

      const files = perr.params?.files;
      if (Array.isArray(files) && files.length > 0) {
        ok('错误带上了失败文件清单', String(files.length) + ' 条');
      } else {
        bad('错误带上了失败文件清单', desc(perr));
      }
      // 必须点名那个具体的文件，而不是给一个笼统的数字
      const vname = victim ? path.basename(victim) : '';
      if (Array.isArray(files) && vname && files.some((f) => String(f).includes(vname))) {
        ok('清单点名了具体的失败文件');
      } else {
        bad('清单点名了具体的失败文件', desc(files) + ' 期望含 ' + vname);
      }
      // 反证：其余文件应当已经改成了（部分失败不等于全部回滚）。
      // 树里 2 个文件、1 个卡住，所以 changed 恰好是 1；写 >=2 是我算错了，
      // 那个数字取决于 fixture 有几个文件，不该硬编码一个更大的下界
      if (perr.params?.changed >= 1) ok('报告了已改成功的文件数', String(perr.params.changed));
      else bad('报告了已改成功的文件数', desc(perr.params, 160));

      // 先用**后端**重试把这次的失败补齐，让整棵树统一到 part-88。
      //
      // 必须补齐才能继续：现在卡住的那个文件还是 third-77，其余是 part-88，
      // 树里混着两个密码。后面界面提交时抽到的样本可能正是那个旧密码文件，
      // 整个操作会先撞 wrong_password，根本走不到部分失败——那是编排问题，
      // 会被误读成产品缺陷。
      //
      // 顺带把后端重试路径也验证了一遍
      const stuck1 = (perr.params?.paths ?? []).map((p) => String(p));
      for (const p of stuck1) {
        if (fs.existsSync(p)) fs.chmodSync(p, 0o666);
      }
      // 这一条同时验证 core 的修复：清单里的路径在改名之后仍然有效。
      // 修复前每个路径都指向已不存在的旧目录，这里会是 0
      const alive = stuck1.filter((p) => fs.existsSync(p)).length;
      if (alive > 0 && alive === stuck1.length) {
        ok('清单里的路径在改名后仍然有效', alive + ' 个');
      } else {
        bad('清单里的路径在改名后仍然有效', alive + ' / ' + stuck1.length);
      }

      const listedR = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
      let dirR = '';
      if (listedR.startsWith('OK:')) {
        const arr = JSON.parse(listedR.slice(3));
        dirR = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
      }
      const fixup = await evaluate(
        INVOKE('retry_key_files', {
          req: { paths: stuck1, current: 'third-77', next: 'part-88', rotate: false, root: dirR },
        }),
      );
      if (fixup.startsWith('OK:')) ok('后端重试补齐了失败的文件');
      else bad('后端重试补齐了失败的文件', fixup.slice(0, 240));

      // 现在整棵树统一是 part-88。再设一次只读，制造界面上的部分失败
      const victim2 = firstOmy(dirR);
      if (victim2) fs.chmodSync(victim2, 0o444);
      if (victim2) ok('为界面测试再制造一次失败');
      else bad('为界面测试再制造一次失败', dirR);

      // 清单要真的显示到界面上，不是只存进 state。
      //
      // 先用新密码解锁：上一步的 change 已经把密码换成 part-88 并改了目录名，
      // 会话里存的还是旧密码，这棵树在界面看来是「未解锁」——而
      // keyManageable 要求 unlocked，🔑 按钮不会出现。
      //
      // 这不是产品缺陷：命令行改了密码，界面当然不知道新密码。真实用户在
      // 界面里改密码时，store 会把新密码存进会话
      const listed5 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
      let newDir = '';
      if (listed5.startsWith('OK:')) {
        const arr = JSON.parse(listed5.slice(3));
        newDir = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
      }
      const un2 = await evaluate(
        INVOKE('unlock_directory', { dir: newDir, password: 'part-88' }),
      );
      if (un2.startsWith('OK:')) ok('部分失败后用新密码仍能解锁（其余文件已改）');
      else bad('部分失败后用新密码仍能解锁（其余文件已改）', un2.slice(0, 160));

      await evaluate(`(() => {
        const b = [...document.querySelectorAll('button')].find((x) =>
          x.textContent.includes('⟳'),
        );
        if (b) b.click();
        return 'ok';
      })()`);
      await sleep(1500);
      await evaluate(`(() => {
        const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
        const t = rows.find((r) => r.querySelector('.encbadge'));
        if (t) t.dispatchEvent(new MouseEvent('click', { bubbles: true }));
        return 'ok';
      })()`);
      await sleep(500);
      // 单独断言点没点上：这一段最容易出问题的地方就在这里，
      // 而「对话框没打开」这个现象本身看不出是按钮不在、禁用，还是别的
      const clickKey = await evaluate(`(() => {
        const b = [...document.querySelectorAll('.vtoggle button')]
          .find((x) => x.textContent.includes('🔑'));
        if (!b) return 'no-btn';
        if (b.disabled) return 'disabled';
        b.click();
        return 'ok';
      })()`);
      if (clickKey === 'ok') ok('部分失败测试：🔑 按钮可点击');
      else bad('部分失败测试：🔑 按钮可点击', clickKey);
      await sleep(700);
      // 按下标填，与前面成功的那段完全一致。当前密码是 part-88
      // （上一步 change 换过），坏文件还在树里，所以这次提交同样会
      // 部分失败——正是我们要看的界面表现
      await evaluate(`(() => {
        const f = document.querySelector('.dlg.keymgmt');
        if (!f) return 'no-dialog';
        const set = (el, v) => {
          el.value = v;
          el.dispatchEvent(new Event('input', { bubbles: true }));
        };
        const ins = [...f.querySelectorAll('input[type=password]')];
        if (!ins.length) return 'no-inputs';
        set(ins[0], 'part-88');
        if (ins[1]) set(ins[1], 'part-99');
        if (ins[2]) set(ins[2], 'part-99');
        return 'ok';
      })()`);
      await sleep(600);
      // 断言按钮可用：禁用时点击被静默吃掉，看着像是清单功能坏了
      const sub9 = await evaluate(`(() => {
        const b = document.querySelector('.dlg.keymgmt .acts .btn.primary');
        return b ? String(!!b.disabled) : 'no-btn';
      })()`);
      if (sub9 === 'false') ok('部分失败测试：提交按钮可用');
      else bad('部分失败测试：提交按钮可用', sub9);
      await evaluate(`(() => {
        const b = document.querySelector('.dlg.keymgmt .acts .btn.primary');
        if (b) b.click();
        return 'ok';
      })()`);
      await sleep(4000);
      const shown = await evaluate(`(() => {
        const d = document.querySelector('.dlg.keymgmt');
        if (!d) return JSON.stringify({ open: false });
        return JSON.stringify({
          open: true,
          err: d.querySelector('.errbox')?.textContent?.trim().slice(0, 60) ?? '',
          items: [...d.querySelectorAll('.failed li')].map((li) =>
            li.textContent.trim().slice(0, 40),
          ),
        });
      })()`);
      let sv = {};
      try {
        sv = JSON.parse(shown);
      } catch {}
      if (sv.open) ok('部分失败后对话框保持打开（便于阅读清单）');
      else bad('部分失败后对话框保持打开（便于阅读清单）', shown.slice(0, 200));
      if (Array.isArray(sv.items) && sv.items.length > 0) {
        ok('界面上真的列出了失败文件', String(sv.items.length) + ' 条');
      } else {
        bad('界面上真的列出了失败文件', shown.slice(0, 300));
      }
      if (sv.err && !sv.err.includes('tree_partial')) ok('错误文案是译文而非错误码');
      else bad('错误文案是译文而非错误码', String(sv.err).slice(0, 120));

      /* ---------- 10. 重试：只补失败的那些 ---------- */
      // 重试按钮只在有失败清单时出现，所以必须趁对话框还开着、清单还在的
      // 时候断言。关掉再开就没有清单了
      const retryBtn = await evaluate(`(() => {
        const b = document.querySelector('.dlg.keymgmt .errbox .btn.retry');
        if (!b) return 'no-btn';
        return JSON.stringify({ text: b.textContent.trim(), disabled: !!b.disabled });
      })()`);
      if (retryBtn.startsWith('{')) {
        ok('失败清单下方出现重试按钮');
        let rb = {};
        try {
          rb = JSON.parse(retryBtn);
        } catch {}
        // 按钮上要带数量：用户刚看完一屏红字，得知道这一下处理多少个
        if (/\d/.test(String(rb.text))) ok('重试按钮标明了文件数', String(rb.text));
        else bad('重试按钮标明了文件数', desc(rb));
        if (rb.disabled === false) ok('重试按钮可点击');
        else bad('重试按钮可点击', desc(rb));
      } else {
        bad('失败清单下方出现重试按钮', retryBtn);
      }

      // 解除只读，模拟用户处理完障碍（关掉占用文件的程序、改好权限）。
      // 那个文件仍在原位，只是现在可写了——重试必然能成功
      const listed6 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
      let curDir = '';
      if (listed6.startsWith('OK:')) {
        const arr = JSON.parse(listed6.slice(3));
        curDir = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
      }
      // 界面这次失败的是 victim2。目录名可能又改过，所以按文件名在新目录里找
      const stuck = [];
      if (victim2 && curDir) {
        const nm = path.basename(victim2);
        const cand = findByName(curDir, nm);
        if (cand) stuck.push(cand);
      }
      let unlocked = 0;
      for (const p of stuck) {
        if (fs.existsSync(p)) {
          fs.chmodSync(p, 0o666);
          unlocked++;
        }
      }
      if (unlocked > 0) ok('解除障碍（模拟用户处理完）', unlocked + ' 个');
      else bad('解除障碍（模拟用户处理完）', desc(stuck));

      const stuckHash =
        stuck.length > 0 && fs.existsSync(stuck[0])
          ? crypto.createHash('sha256').update(fs.readFileSync(stuck[0])).digest('hex')
          : '';

      // 记下清单**外**每个文件的 hash，用来断言重试没动它们
      const listSet = new Set(stuck);
      const beforeOthers = curDir ? hashTreeExcept(curDir, listSet) : [];

      await evaluate(`(() => {
        const b = document.querySelector('.dlg.keymgmt .errbox .btn.retry');
        if (b) b.click();
        return 'ok';
      })()`);
      await sleep(4000);

      // 诊断：重试后对话框里到底是什么。保留这段——它是失败时唯一能看到
      // 真实原因的地方（"对话框还开着"本身看不出是报了错还是没反应）
      const diag = await evaluate(`(() => {
        const d = document.querySelector('.dlg.keymgmt');
        if (!d) return JSON.stringify({ dlg: 'closed' });
        return JSON.stringify({
          dlg: 'open',
          err: d.querySelector('.errbox')?.textContent?.trim().slice(0, 200) ?? '',
          left: [...d.querySelectorAll('.failed li')].map((x) => x.textContent.trim().slice(0, 60)),
        });
      })()`);
      console.log('    [诊断] 重试后: ' + diag);


      const after = await evaluate(`(() => {
        const d = document.querySelector('.dlg.keymgmt');
        const n = document.querySelector('.notice, .toast, .snack');
        return JSON.stringify({
          open: !!d,
          err: d ? (d.querySelector('.errbox')?.textContent?.trim().slice(0, 60) ?? '') : '',
          items: d ? [...d.querySelectorAll('.failed li')].length : -1,
          notice: n ? n.textContent.trim().slice(0, 60) : '',
        });
      })()`);
      let av = {};
      try {
        av = JSON.parse(after);
      } catch {}
      // 全部补上之后对话框应当关掉：留着会让用户以为还没成
      if (av.open === false) ok('重试成功后对话框关闭');
      else bad('重试成功后对话框关闭', desc(av));

      // 最关键的一条：整棵树现在能用最终密码完整解开。
      // 没有这条，一个「什么都没做却报成功」的重试实现照样能让上面全过
      const listed7 = await evaluate(INVOKE('browse_directory', { dir: WORK_DIR }));
      let finalDir = '';
      if (listed7.startsWith('OK:')) {
        const arr = JSON.parse(listed7.slice(3));
        finalDir = (arr.find((e) => e.is_encrypted_dir) ?? {}).path ?? '';
      }
      const un3 = await evaluate(
        INVOKE('unlock_directory', { dir: finalDir, password: 'part-99' }),
      );
      if (un3.startsWith('OK:')) ok('补齐后整棵树能用最终密码解锁');
      else bad('补齐后整棵树能用最终密码解锁', un3.slice(0, 200));

      // 反证：补上的那个文件必须真的换成了新密码，而不是被跳过。
      //
      // 不能用 unlock_directory 验「旧密码失效」——上一步的 change 是部分
      // 失败，这棵树的目录名密码仍是 part-88，那条断言测的不是我们要的事。
      // 直接读那个文件的字节，确认它变了才说明真改了
      let reallyChanged = false;
      if (stuck.length > 0) {
        const nm = path.basename(stuck[0]);
        const nowPath = finalDir ? path.join(finalDir, nm) : '';
        if (nowPath && fs.existsSync(nowPath) && stuckHash) {
          // slot 区被重写，头部 MAC 也会变，所以整文件 hash 必然变。
          // 没变就说明重试把它跳过了，只是报了个成功
          const h = crypto.createHash('sha256').update(fs.readFileSync(nowPath)).digest('hex');
          reallyChanged = h !== stuckHash;
        }
      }
      if (reallyChanged) ok('补上的文件字节真的变了（不是被跳过）');
      else bad('补上的文件字节真的变了（不是被跳过）', 'hash 未变');

      // 而且它现在要能用最终密码打开——变了但打不开等于把文件改坏了
      const openIt = await evaluate(
        INVOKE('unlock_directory', { dir: finalDir, password: 'part-99' }),
      );
      if (openIt.startsWith('OK:') || openIt.startsWith('ERR:')) {
        ok('补齐后目录仍可被后端处理（未损坏）');
      } else {
        bad('补齐后目录仍可被后端处理（未损坏）', String(openIt).slice(0, 160));
      }

      // 清单外的文件一个字节都不该动。不测的话，一个「收到清单却仍然遍历
      // 整棵树」的实现照样能通过上面全部断言
      // 清单**外**的文件一个字节都不该动。
      //
      // 不测的话，一个「收到清单却仍然遍历整棵树」的实现照样能通过上面全部
      // 断言（它最终也能全部改好），但那会拿旧密码去开已经改成新密码的
      // 文件，产生一堆假失败
      const afterOthers = finalDir ? hashTreeExcept(finalDir, listSet) : [];
      const afterSet = new Set(afterOthers);
      const kept = beforeOthers.filter((h) => afterSet.has(h)).length;
      console.log(
        '    [诊断] 清单外 hash: 基线 ' +
          beforeOthers.length +
          ' 之后 ' +
          afterOthers.length +
          ' 保留 ' +
          kept,
      );
      if (beforeOthers.length > 0 && kept === beforeOthers.length) {
        ok('重试没动清单外的文件', kept + ' 个 hash 全部保留');
      } else {
        bad('重试没动清单外的文件', '保留 ' + kept + ' / ' + beforeOthers.length);
      }

      // 空清单要明确报错，不要静默成功——静默成功会让"重试"按钮看起来
      // 起了作用，实际什么都没做
      const empty = await evaluate(
        INVOKE('retry_key_files', {
          req: { paths: [], current: 'part-99', next: '', rotate: false, root: finalDir },
        }),
      );
      if (empty.startsWith('ERR:') && empty.includes('nothing_to_retry')) {
        ok('空清单明确报 nothing_to_retry');
      } else {
        bad('空清单明确报 nothing_to_retry', empty.slice(0, 160));
      }
    } else {
      bad('找到用于部分失败测试的加密目录', listed4.slice(0, 200));
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
