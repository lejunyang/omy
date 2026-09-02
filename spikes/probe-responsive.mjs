/** 验证响应式：同一个 WebView 里改视口尺寸，两套 UI 必须各自成立。
 *
 * 用法：node --experimental-websocket probe-responsive.mjs <port> <目录>
 *
 * # 为什么必须真跑而不是看截图
 *
 * 移动端最致命的缺陷是**看着完全正常但点不开**：桌面靠 dblclick 打开
 * 条目，触屏上没有这个手势。截图、CSS 审查、甚至 @media 是否命中，
 * 全都发现不了它——界面渲染得一模一样。所以这里必须真的派发一次
 * 单击，看列表是否真的进了下一级目录。
 *
 * # 判据
 *
 * 桌面态：侧栏可见、无底部导航、单击不打开（只选中）。
 * 移动态：侧栏收起为抽屉、有底部导航、网格 2 列、**单击真的打开**。
 * 切回桌面：抽屉必须自动收起，否则它会盖住桌面布局的半个屏幕。
 */

const port = process.argv[2] || '9358';
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
      return { __err: m.result.exceptionDetails.exception?.description || m.result.exceptionDetails.text };
    }
    return m.result?.result?.value;
  }
}

const results = [];
function check(name, ok, detail = '') {
  results.push({ name, ok });
  console.log(`  ${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? '  ' + detail : ''}`);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 采集当前布局特征。一次取全，减少来回。 */
const SNAPSHOT = `(() => {
  const vis = (el) => {
    if (!el) return false;
    const r = el.getBoundingClientRect();
    const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.display !== 'none' && s.visibility !== 'hidden';
  };
  const side = document.querySelector('.side');
  const grid = document.querySelector('.grid');
  let cols = 0;
  if (grid) cols = getComputedStyle(grid).gridTemplateColumns.split(' ').filter(Boolean).length;
  const sideRect = side ? side.getBoundingClientRect() : null;
  return {
    w: window.innerWidth,
    sideExists: !!side,
    sideIsDrawer: side ? side.classList.contains('drawer') : false,
    sideOpen: side ? side.classList.contains('open') : false,
    // 抽屉未展开时被 translateX 移出屏幕，用 left 判断比 display 准
    sideLeft: sideRect ? Math.round(sideRect.left) : null,
    hasBottomNav: vis(document.querySelector('.pnav')),
    navItems: document.querySelectorAll('.pnavi').length,
    hasHamburger: !!document.querySelector('.titlebar.mob .iconbtn'),
    scrim: vis(document.querySelector('.scrim')),
    gridCols: cols,
    cards: document.querySelectorAll('.grid .card').length,
    crumb: [...document.querySelectorAll('.crumbseg')].map(b => b.textContent.trim()).join('/'),
  };
})()`;

async function setViewport(c, width, height, mobile) {
  await c.send('Emulation.setDeviceMetricsOverride', {
    width, height, deviceScaleFactor: 1, mobile,
  });
  // 等 matchMedia 事件与 Vue 重渲染跑完
  await sleep(700);
}

async function main() {
  const page = await waitTarget();
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.addEventListener('open', res); ws.addEventListener('error', rej); });
  const c = new Cdp(ws);
  await c.send('Runtime.enable');
  await c.send('Page.enable');
  await sleep(2200);

  console.log('=== 0. 进入测试目录 ===');
  const side = await c.eval(`(() => {
    const t = [...document.querySelectorAll('.side .sitem')]
      .find(i => i.textContent.includes('文档') || i.textContent.includes('Documents'));
    if (!t) return 'no-documents';
    t.click();
    return 'clicked';
  })()`);
  check('侧栏能点到「文档」', side === 'clicked', String(side));
  await sleep(1200);

  const enter = await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.grid .card')];
    const t = rows.find(r => r.textContent.includes('omy-responsive-test'));
    if (!t) return 'not-found:' + rows.length;
    t.dispatchEvent(new MouseEvent('dblclick', { bubbles: true }));
    return 'dispatched';
  })()`);
  check('进入测试目录', enter === 'dispatched', String(enter));
  await sleep(1200);

  // ---------- 桌面态 ----------
  console.log('');
  console.log('=== 1. 桌面视口 1280×860 ===');
  await setViewport(c, 1280, 860, false);
  const d = await c.eval(SNAPSHOT);
  check('侧栏常驻可见（不是抽屉）', d.sideExists && !d.sideIsDrawer, `left=${d.sideLeft}`);
  check('没有底部导航', !d.hasBottomNav);
  check('没有汉堡键', !d.hasHamburger);
  check('网格按宽度自适应多列', d.gridCols >= 4, `cols=${d.gridCols}`);
  check('列出了测试文件', d.cards >= 3, `cards=${d.cards}`);

  // 桌面单击只选中，不打开——这是与移动端的关键分野
  const beforeCrumb = d.crumb;
  await c.eval(`(() => {
    const t = [...document.querySelectorAll('.grid .card')]
      .find(r => r.textContent.includes('sub-dir'));
    if (t) t.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    return 1;
  })()`);
  await sleep(800);
  const afterClick = await c.eval(SNAPSHOT);
  check('桌面单击不打开目录（只选中）', afterClick.crumb === beforeCrumb,
    `${beforeCrumb} -> ${afterClick.crumb}`);

  // 上一步把 sub-dir 选中了，必须清掉再进移动态。
  // 移动端对已选中项按「加选」处理（正确行为：否则长按选中后
  // 就没法再选第二项），留着它会让下面的「单击打开」测不到东西。
  // 用界面上真实的「清除选择」按钮，顺带验证它有效
  const cleared = await c.eval(`(() => {
    const b = [...document.querySelectorAll('.vtoggle .btn')]
      .find(x => !x.classList.contains('primary'));
    if (!b) return 'no-clear-button';
    b.click();
    return 'clicked';
  })()`);
  await sleep(400);
  const afterClear = await c.eval(`document.querySelectorAll('.grid .card.sel').length`);
  check('「清除选择」按钮能清空选中', cleared === 'clicked' && afterClear === 0,
    `${cleared} sel=${afterClear}`);

  // ---------- 移动态 ----------
  console.log('');
  console.log('=== 2. 移动视口 390×844 ===');
  await setViewport(c, 390, 844, true);
  const m = await c.eval(SNAPSHOT);
  check('宽度确实是移动尺寸', m.w <= 768, `w=${m.w}`);
  check('侧栏变为抽屉且默认收起', m.sideIsDrawer && !m.sideOpen && m.sideLeft < 0,
    `drawer=${m.sideIsDrawer} open=${m.sideOpen} left=${m.sideLeft}`);
  check('出现底部导航', m.hasBottomNav, `items=${m.navItems}`);
  check('出现汉堡键', m.hasHamburger);
  check('网格为 2 列（与原型一致）', m.gridCols === 2, `cols=${m.gridCols}`);

  // 抽屉开合
  console.log('');
  console.log('=== 3. 抽屉开合 ===');
  await c.eval(`document.querySelector('.titlebar.mob .iconbtn').click()`);
  await sleep(600);
  const open = await c.eval(SNAPSHOT);
  check('点汉堡键抽屉展开', open.sideOpen && open.sideLeft >= 0, `left=${open.sideLeft}`);
  check('展开时出现遮罩', open.scrim);

  await c.eval(`document.querySelector('.scrim').click()`);
  await sleep(600);
  const closed = await c.eval(SNAPSHOT);
  check('点遮罩抽屉收起', !closed.sideOpen && closed.sideLeft < 0, `left=${closed.sideLeft}`);

  // ---------- 移动端单击打开：最关键的一项 ----------
  console.log('');
  console.log('=== 4. 移动端单击就能打开（触屏没有双击） ===');
  const beforeMob = closed.crumb;
  const tap = await c.eval(`(() => {
    const t = [...document.querySelectorAll('.grid .card')]
      .find(r => r.textContent.includes('sub-dir'));
    if (!t) return 'not-found';
    // 用真实的 pointer 序列，而不是直接 click：
    // 长按判定挂在 pointerdown 上，只发 click 测不到它会不会误触发
    t.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerType: 'touch', clientX: 50, clientY: 50 }));
    t.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerType: 'touch', clientX: 50, clientY: 50 }));
    t.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    return 'tapped';
  })()`);
  check('派发了单击', tap === 'tapped', String(tap));
  await sleep(1100);
  const afterTap = await c.eval(SNAPSHOT);
  check('单击真的进入了子目录', afterTap.crumb !== beforeMob && afterTap.crumb.includes('sub-dir'),
    `${beforeMob} -> ${afterTap.crumb}`);

  // ---------- 长按多选 ----------
  console.log('');
  console.log('=== 5. 长按进入多选（移动端没有 Ctrl 键） ===');
  await c.eval(`(() => {
    const up = document.querySelectorAll('.crumbbtn')[0];
    if (up) up.click();
    return 1;
  })()`);
  await sleep(1000);
  const longPress = await c.eval(`(async () => {
    const t = [...document.querySelectorAll('.grid .card')]
      .find(r => r.textContent.includes('a.txt'));
    if (!t) return 'not-found';
    t.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, pointerType: 'touch', clientX: 50, clientY: 50 }));
    await new Promise(r => setTimeout(r, 700));
    t.dispatchEvent(new PointerEvent('pointerup', { bubbles: true, pointerType: 'touch', clientX: 50, clientY: 50 }));
    t.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    await new Promise(r => setTimeout(r, 300));
    return document.querySelectorAll('.grid .card.sel').length;
  })()`);
  check('长按选中了一项', longPress === 1, `selected=${JSON.stringify(longPress)}`);

  const stillThere = await c.eval(`document.querySelectorAll('.grid .card').length > 0
    && !!document.querySelector('.crumbseg')`);
  check('长按后没有误打开文件', stillThere === true);

  // ---------- 切回桌面 ----------
  console.log('');
  console.log('=== 6. 切回桌面：抽屉必须自动收起 ===');
  await setViewport(c, 390, 844, true);
  await c.eval(`document.querySelector('.titlebar.mob .iconbtn').click()`);
  await sleep(500);
  const openAgain = await c.eval(SNAPSHOT);
  check('先把抽屉打开', openAgain.sideOpen);

  await setViewport(c, 1280, 860, false);
  const back = await c.eval(SNAPSHOT);
  check('回到桌面后抽屉已收起', !back.sideOpen && !back.sideIsDrawer,
    `open=${back.sideOpen} drawer=${back.sideIsDrawer}`);
  check('回到桌面后底部导航消失', !back.hasBottomNav);
  check('回到桌面后侧栏在正常位置', back.sideLeft === 0, `left=${back.sideLeft}`);

  await c.send('Emulation.clearDeviceMetricsOverride');

  const pass = results.filter((r) => r.ok).length;
  console.log('');
  console.log(`=== 合计 ${pass}/${results.length} 通过 ===`);
  ws.close();
  process.exit(pass === results.length ? 0 : 1);
}

main().catch((e) => { console.error('探针异常:', e.message); process.exit(2); });
