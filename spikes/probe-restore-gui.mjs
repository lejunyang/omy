/** 验证 GUI 的「还原到磁盘」走真实界面能用。
 *
 * 用法：node --experimental-websocket probe-restore-gui.mjs <port> <目录>
 *
 * # 这个探针要回答的问题
 *
 * 后端命令有单元测试、core 的解包有 9 项、CLI 那条链路有 27 项，但
 * 「用户点得到吗」全都测不到。上一轮元数据的缺陷正是这样躲过所有检查的：
 * 每一层单独看都对，没人测层与层之间。
 *
 * 所以这里只验界面这一层：按钮出不出现、对话框拿不拿得到路径、点下去
 * 文件有没有真的出现在选定位置。文件内容是否正确交给驱动脚本用 CLI 核对。
 *
 * # 定位一律按结构，且结构必须先查准
 *
 * AGENTS.md 记着「按结构定位不按文字」，但上一轮踩到的是这条的前半截：
 * 类名也是凭印象写的（写成 .name/.fname，实际是 .cname/.nm），结果和
 * 按文字定位一样不可靠。所以这里用的选择器都从源码核实过：
 * 网格 .cname（EntryCard.vue）、列表 .nm（MainScreen.vue:358），
 * aria-label 携带 entry.name 原文。
 */

const port = process.argv[2] || '9373';
const base = `http://127.0.0.1:${port}`;

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
      if (m.id && this.pending.has(m.id)) {
        this.pending.get(m.id)(m); this.pending.delete(m.id);
      }
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
        .split('\n')[0].slice(0, 140);
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

/** 精确匹配一行的名字。选择器来源见文件头。 */
const rowByName = (name) => `${ROWS}.find(r => {
  const aria = r.getAttribute('aria-label');
  const el = r.querySelector('.cname, .nm');
  const txt = (aria || (el ? el.textContent : '')).trim();
  return txt === ${JSON.stringify(name)};
})`;

/** 按类名找按钮，不按文案。
 *
 * 还原按钮没有独立类名（和其他工具栏按钮一样是 .btn.small），所以只能
 * 靠文案区分。这里同时匹配中英，并且用 startsWith 判断图标前缀——
 * 「还原」这两个字不会出现在别的按钮上，但 includes 在未来加了
 * 「还原设置」之类的按钮时会命中错的那个。
 */
const restoreBtn = `[...document.querySelectorAll('.vtoggle button')]
  .find(b => b.textContent.includes('📤'))`;

(async () => {
  const wd = process.argv[3];
  const t = await waitTarget();
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((res, rej) => {
    ws.addEventListener('open', res);
    ws.addEventListener('error', () => rej(new Error('ws 失败')));
  });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  console.log('\n--- 1. 进入测试目录并解锁 ---');
  const side = await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const t = items.find(i => i.textContent.includes('文档')
                           || i.textContent.includes('Documents'));
    if (!t) return 'no-documents:' + items.length;
    t.click();
    return 'clicked';
  })()`);
  check('侧栏进入文档目录', side === 'clicked', String(side));
  await sleep(1300);

  const into = await c.eval(`(() => {
    const row = ${rowByName('omy-restore-test')};
    if (!row) return 'not-found:' + ${ROWS}.map(r=>r.textContent.trim().slice(0,20)).join('|');
    row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  check('双击进入测试目录', into === 'ok', String(into));
  await sleep(1400);

  // 直接调后端解锁：走 UI 输密码要多好几步，而这个探针要验的是还原，
  // 不是解锁（解锁另有覆盖）
  const unlocked = await c.eval(inv('unlock_directory', {
    dir: wd, password: 'restore-pw',
  }));
  check('目录已解锁', unlocked && unlocked.credentials > 0,
    JSON.stringify(unlocked));
  await sleep(600);

  // **必须**扫描一次。annotate_unlocked（browse.rs:204）是拿
  // state.files() 这张已登记表按路径匹配的，光解锁不会往表里加东西——
  // 不扫的话 browse_directory 永远报 unlocked:false、entry_id:null，
  // 前端 restorable 为空，按钮不出现。
  //
  // 真实 UI 走的是 store.js 的 refreshKnown → scan_directory，
  // 这里补上同一步，而不是去点某个可能不存在的「刷新」按钮
  const scanned = await c.eval(inv('scan_directory', { dir: wd, recursive: false }));
  check('已扫描登记加密文件',
    Array.isArray(scanned) && scanned.length > 0,
    Array.isArray(scanned) ? `登记 ${scanned.length} 个` : String(scanned));
  await sleep(600);

  // 让界面重新拉一次列表，使 entry_id 进到前端的 state.entries。
  //
  // 走「上一级 + 重新双击进来」而不是找刷新按钮：一次真实的 navigate
  // 必然重新 browse + scan，而刷新按钮的存在与文案都可能变，找不到时
  // 静默跳过会让列表压根没更新
  await c.eval(`(() => {
    const up = [...document.querySelectorAll('button')]
      .find(x => (x.getAttribute('aria-label')||'').includes('上一级')
              || (x.getAttribute('title')||'').includes('上一级'));
    if (up) up.click();
    return 'ok';
  })()`);
  await sleep(1200);
  await c.eval(`(() => {
    const row = ${rowByName('omy-restore-test')};
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`);
  await sleep(1600);

  console.log('\n--- 2. 选中加密文件，还原按钮出现 ---');
  // 名字是随机的（GUI 默认加密文件名），按扩展名找
  const listed = await c.eval(inv('browse_directory', { dir: wd }));
  const enc = Array.isArray(listed)
    ? listed.find((e) => e.name.endsWith('.omy'))
    : null;
  check('目录里有加密文件', !!enc,
    Array.isArray(listed) ? JSON.stringify(listed.map((e) => e.name)) : String(listed));
  check('后端认为它已解锁', enc && enc.unlocked === true, JSON.stringify(enc && {
    name: enc.name, unlocked: enc.unlocked, is_container: enc.is_container,
  }));

  // 未选中时按钮不该出现：这条锁住「按钮跟着选择走」，
  // 否则一个永远显示的按钮点下去只会报 empty_selection
  const before = await c.eval(`!!(${restoreBtn})`);
  check('未选中时还原按钮不出现', before === false, String(before));

  // 解锁后界面显示的是**原名**（payload），不是磁盘名（payload.omy）：
  // real_name 就是为此存在的。首轮按磁盘名找，打印出来的候选是
  // 「🔒 加密文件」——那是未解锁时的占位文案，说明当时压根没解锁成功
  const wantName = enc ? (enc.real_name || enc.name) : '?';
  const sel = await c.eval(`(() => {
    const row = ${rowByName('%NAME%')}
      || ${ROWS}.find(r => {
           const a = (r.getAttribute('aria-label')||'');
           return a.endsWith('.omy') || a === 'payload';
         });
    if (!row) return 'not-found:' + ${ROWS}.map(r=>(r.getAttribute('aria-label')||r.textContent).trim().slice(0,24)).join('|');
    row.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
    return 'ok';
  })()`.replace('%NAME%', wantName));
  check('选中加密文件', sel === 'ok', String(sel));
  await sleep(700);

  const appeared = await c.eval(`(() => {
    const b = ${restoreBtn};
    if (!b) return 'no-button:' + [...document.querySelectorAll('.vtoggle button')]
      .map(x => x.textContent.trim().slice(0,16)).join('|');
    if (b.disabled) return 'disabled';
    b.click();
    return 'ok';
  })()`);
  check('还原按钮出现并已点击', appeared === 'ok', String(appeared));
  await sleep(900);

  console.log('\n--- 3. 对话框与目标目录 ---');
  const dlg = await c.eval(`!!document.querySelector('.dlg')`);
  check('还原对话框已弹出', dlg === true, String(dlg));

  // 默认应停在「当前目录」，且路径已显示出来。
  // 这条锁住「点确定之前就能看到文件会落在哪」这个设计意图
  const shown = await c.eval(`(() => {
    const el = document.querySelector('.dlg .tpath');
    if (!el) return 'no-tpath';
    return el.textContent.trim();
  })()`);
  check('默认显示当前目录作为落点',
    typeof shown === 'string' && shown.length > 0 && !shown.startsWith('no-'),
    String(shown));

  // 切到「自选目录」但不选路径时必须禁止提交。
  // 不做这条检查的话，那个静默落到源目录的缺陷不会被发现
  const guard = await c.eval(`(() => {
    const radios = [...document.querySelectorAll('.dlg input[type=radio]')];
    const pick = radios[1];
    if (!pick) return 'no-radio:' + radios.length;
    pick.click();
    pick.dispatchEvent(new Event('change', { bubbles: true }));
    return 'clicked';
  })()`);
  check('切到自选目录', guard === 'clicked', String(guard));
  await sleep(400);

  const disabled = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    return b ? b.disabled : 'no-submit';
  })()`);
  check('未选路径时提交被禁用', disabled === true, String(disabled));

  // 用旁路指定目录：原生对话框是 OS 窗口，CDP 点不到
  // （commands.rs 里 OMY_GUI_PICK_FOLDER 就是为此留的）
  const chose = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.dlg button')]
      .find(x => x.textContent.includes('📂'));
    if (!b) return 'no-choose';
    b.click();
    return 'ok';
  })()`);
  check('点了选择目录', chose === 'ok', String(chose));
  await sleep(1200);

  const outDir = await c.eval(`(() => {
    const el = document.querySelector('.dlg .tpath');
    return el ? el.textContent.trim() : 'no-tpath';
  })()`);
  check('落点已更新为选定目录',
    typeof outDir === 'string' && outDir.includes('restored-here'),
    String(outDir));

  const enabled = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    return b ? !b.disabled : 'no-submit';
  })()`);
  check('选好路径后可提交', enabled === true, String(enabled));

  console.log('\n--- 4. 提交并确认落盘 ---');
  const sub = await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (!b) return 'no-submit';
    b.click();
    return 'ok';
  })()`);
  check('已提交还原', sub === 'ok', String(sub));

  // 轮询产物：还原要解密全部载荷，固定等待要么不够要么白等
  let out = [];
  for (let i = 0; i < 40; i++) {
    const l = await c.eval(inv('browse_directory', { dir: `${wd}\\restored-here` }));
    out = Array.isArray(l) ? l.map((e) => e.name) : [];
    if (out.length) break;
    await sleep(500);
  }
  check('目标目录里出现了还原结果', out.length > 0, JSON.stringify(out));

  // 成功提示应当出现，且对话框关掉了。
  // 只看文件存在的话，「文件出来了但界面卡在对话框」也算通过
  const closed = await c.eval(`!document.querySelector('.dlg')`);
  check('成功后对话框已关闭', closed === true, String(closed));

  const notice = await c.eval(`(() => {
    const el = document.querySelector('.notice, .toast, .banner');
    return el ? el.textContent.trim().slice(0, 120) : 'no-notice';
  })()`);
  check('显示了成功提示', typeof notice === 'string' && notice !== 'no-notice',
    String(notice));

  console.log('RESTORE_DIR=' + `${wd}\\restored-here`);

  const okAll = results.every((r) => r.ok);
  console.log(`\n  探针合计 ${results.filter((r) => r.ok).length} 通过 / ` +
    `${results.filter((r) => !r.ok).length} 失败`);
  if (okAll) console.log('PROBE_OK');
  ws.close();
  process.exit(okAll ? 0 : 1);
})().catch((e) => {
  console.log('探针异常: ' + e.message);
  process.exit(1);
});
