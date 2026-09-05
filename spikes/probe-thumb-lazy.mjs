/** 验证缩略图按可见性加载，并预取即将进入视口的部分。
 *
 * 用法：node --experimental-websocket probe-thumb-lazy.mjs <port> <目录>
 *
 * # 为什么必须用「很多文件」来测
 *
 * 懒加载的全部意义在于「不要一次性加载全部」。文件少于一屏时，加载全部
 * 与懒加载的表现完全一样——那种情况下任何断言都是通过的，测不出东西。
 * 所以这里造 260 个文件——不仅要超过一屏，还要远超保活范围
 * （KEEP_MARGIN=2000px），否则卸载逻辑永远不触发，「加载量受控」
 * 那条会因为语料不够而失败，看起来像产品缺陷。
 *
 * # 断言的是 <img> 元素数量，而不是网络请求
 *
 * 数请求要挂 CDP 的 Network 域，且请求可能被缓存、去重，不可靠。
 * 而 img 元素数量直接对应「WebView 现在持有多少张解码后的图」——
 * 那正是我们要控制的东西（每张约 300 KB，与压缩后的几 KB 无关）。
 *
 * # 三条关键断言
 *
 * 1. 初始只加载可见部分 + 预取范围，不是全部；
 * 2. 滚动后新的位置被加载（说明懒加载不是「永久不加载」）；
 * 3. 滚回原处不产生重新加载（说明保活边距起作用，来回滚动不闪）。
 *
 * 第 3 条最容易被忽略：只用一个边距的实现会在刚离开视口时就卸载，
 * 用户往回滚一点就要重新请求、重新解密、重新解码。
 */

const port = process.argv[2] || '9380';
const WORK = process.argv[3];
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
        .split('\n')[0].slice(0, 160);
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

/** 网格的统计：卡片总数、已加载的 img 数、滚动位置。 */
const STATS = `(() => {
  const sc = document.querySelector('.list, .grid')?.parentElement;
  return {
    cards: document.querySelectorAll('.grid .card').length,
    imgs: document.querySelectorAll('.grid .card .thumb img').length,
    withThumb: document.querySelectorAll('.grid .card.enc').length,
    scrollTop: sc ? Math.round(sc.scrollTop) : -1,
    scrollH: sc ? sc.scrollHeight : -1,
    clientH: sc ? sc.clientHeight : -1,
  };
})()`;

(async () => {
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

  console.log('\n--- 1. 解锁并扫描（目录里有很多带缩略图的文件）---');
  // 必须先进到目标目录再解锁，而且解锁要走界面按钮。
  //
  // 直接 invoke('unlock_directory') 是行不通的：store 的
  // refreshKnown() 在 state.credentials === 0 时直接把 state.known
  // 清成 {} 就返回，而那个计数只有走界面的解锁流程才会加。绕过界面
  // 解锁的结果是后端明明解开了、卡片也渲染成已解锁分支，但 known
  // 恒为 null，于是**一张 img 都不渲染**——看起来完全像懒加载没生效。
  // 已经踩过一次，这里记下来。
  await c.eval(`(() => {
    const items = [...document.querySelectorAll('.side .sitem')];
    const t = items.find(i => i.textContent.includes('文档')
                           || i.textContent.includes('Documents'));
    if (t) t.click();
    return true;
  })()`);
  await sleep(1200);
  await c.eval(`(() => {
    const rows = [...document.querySelectorAll('.grid .card, .list .lrow')];
    const row = rows.find(r => {
      const aria = r.getAttribute('aria-label');
      const el = r.querySelector('.cname, .nm');
      return (aria || (el ? el.textContent : '')).trim() === 'omy-lazy-test';
    });
    if (row) row.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
    return true;
  })()`);
  await sleep(1800);

  // 点工具栏的统一密码入口（.pill），让 store 自己去调后端并累加
  // credentials。按结构定位而不是按文字：文案随语言变，而且
  // 「解锁」二字也出现在别的地方
  const opened = await c.eval(`(() => {
    const b = document.querySelector('.pill');
    if (b) { b.click(); return 'pill'; }
    return 'no-pill';
  })()`);
  await sleep(900);
  // 填密码与点提交分两次 evaluate：合在一起时对话框可能还没渲染完
  await c.eval(`(() => {
    const ins = [...document.querySelectorAll('.dlg input[type=password]')];
    for (const i of ins) {
      i.value = 'lazy-pw';
      i.dispatchEvent(new Event('input', { bubbles: true }));
    }
    return ins.length;
  })()`);
  await sleep(400);
  await c.eval(`(() => {
    const b = document.querySelector('.dlg .acts .btn.primary');
    if (b) { b.click(); return true; }
    return false;
  })()`);
  await sleep(2500);
  const creds = await c.eval(`(() => {
    const cards = document.querySelectorAll('.grid .card').length;
    const locked = document.querySelectorAll('.grid .card.locked').length;
    return { cards, locked };
  })()`);
  check('解锁成功（界面上没有锁定卡片）',
    creds && creds.cards > 0 && creds.locked === 0,
    `入口按钮=${opened}，` + JSON.stringify(creds));

  const files = await c.eval(inv('scan_directory', { dir: WORK, recursive: false }));
  const arr = Array.isArray(files) ? files : [];
  const thumbCount = arr.filter((f) => f.has_thumbnail).length;
  check('后端报告足够多的缩略图', thumbCount >= 200,
    `共 ${arr.length} 个文件，${thumbCount} 个带缩略图`);

  console.log('\n--- 2. 初始只加载可见范围 ---');
  let s0 = null;
  for (let i = 0; i < 20; i++) {
    s0 = await c.eval(STATS);
    if (s0 && s0.cards >= 200 && s0.imgs > 0) break;
    await sleep(500);
  }
  console.log('  [诊断] ' + JSON.stringify(s0));
  check('网格渲染出了全部卡片', s0 && s0.cards >= 200, JSON.stringify(s0 && s0.cards));
  check('确实有内容需要滚动', s0 && s0.scrollH > s0.clientH,
    `scrollH=${s0 && s0.scrollH} clientH=${s0 && s0.clientH}`);
  check('已加载若干缩略图（不是零）', s0 && s0.imgs > 0, `imgs=${s0 && s0.imgs}`);
  // 核心断言：加载数必须明显少于总数。
  // 若实现退回「全部渲染」，imgs 会等于 cards，这条立刻失败。
  check('未一次性加载全部缩略图',
    s0 && s0.imgs < s0.cards,
    `已加载 ${s0 && s0.imgs} / 共 ${s0 && s0.cards} 张`);

  // 预取必须真的生效——这条是「只比总数」测不出来的。
  //
  // 算出可见区域能放多少张，再算上预取距离应覆盖多少张，两者比对。
  // 若 observer 没把滚动容器设为 root，rootMargin 会被祖先容器的裁剪
  // 抵消，加载数恰好等于**可见**数量，预取那一半悄悄失效——
  // 而「imgs < cards」照样通过。实测踩过：20/60 看着像懒加载正常，
  // 实际应该是 54 张。
  const geo = await c.eval(`(() => {
    const cards = [...document.querySelectorAll('.grid .card')];
    if (!cards.length) return null;
    const sc = document.querySelector('.list, .grid').parentElement;
    const r0 = cards[0].getBoundingClientRect();
    // 同一行的卡片 top 相同，据此数出每行几个
    const perRow = cards.filter(
      x => Math.abs(x.getBoundingClientRect().top - r0.top) < 4).length;
    const rowH = r0.height + 12;
    const visibleRows = Math.ceil(sc.clientHeight / rowH);
    return { perRow, rowH: Math.round(rowH), visibleRows,
             visible: perRow * visibleRows };
  })()`);
  console.log('  [诊断] 几何 ' + JSON.stringify(geo));
  // 预取 600px 约等于额外几行；只要明显超出可见量就说明起作用了。
  // 不写死具体张数：窗口尺寸会变，写死会让断言变脆
  check('预取生效（加载量明显超出可见范围）',
    geo && s0 && s0.imgs > geo.visible,
    `已加载 ${s0 && s0.imgs} 张，可见约 ${geo && geo.visible} 张` +
    `（每行 ${geo && geo.perRow} × 可见 ${geo && geo.visibleRows} 行）`);

  console.log('\n--- 3. 滚到底部，新位置被加载 ---');
  await c.eval(`(() => {
    const sc = document.querySelector('.list, .grid')?.parentElement;
    if (sc) sc.scrollTop = sc.scrollHeight;
    return true;
  })()`);
  await sleep(1800);
  const s1 = await c.eval(STATS);
  console.log('  [诊断] ' + JSON.stringify(s1));
  check('滚动后仍有已加载的缩略图', s1 && s1.imgs > 0, `imgs=${s1 && s1.imgs}`);
  check('滚动位置确实变了', s1 && s1.scrollTop > 0, `scrollTop=${s1 && s1.scrollTop}`);
  // 底部的卡片原先不在预取范围内，滚过去后必须被加载。
  // 若 observer 没接上，滚到底部也不会有任何新图
  const lastLoaded = await c.eval(`(() => {
    const cards = [...document.querySelectorAll('.grid .card')];
    const last = cards.slice(-6);
    return {
      tail: last.length,
      tailImgs: last.filter(x => x.querySelector('.thumb img')).length,
    };
  })()`);
  check('底部卡片滚过去后被加载',
    lastLoaded && lastLoaded.tailImgs > 0, JSON.stringify(lastLoaded));

  console.log('\n--- 4. 滚回顶部不重新加载（保活边距生效）---');
  // 记下顶部若干卡片当前的加载状态，滚走再滚回，状态应保持
  await c.eval(`(() => {
    const sc = document.querySelector('.list, .grid')?.parentElement;
    if (sc) sc.scrollTop = 0;
    return true;
  })()`);
  await sleep(1500);
  const s2 = await c.eval(STATS);
  console.log('  [诊断] ' + JSON.stringify(s2));
  const topState = await c.eval(`(() => {
    const cards = [...document.querySelectorAll('.grid .card')].slice(0, 6);
    return {
      head: cards.length,
      headImgs: cards.filter(x => x.querySelector('.thumb img')).length,
      // complete=false 意味着正在重新请求；全 true 说明没有重新加载
      allComplete: cards.every(x => {
        const im = x.querySelector('.thumb img');
        return !im || im.complete;
      }),
    };
  })()`);
  check('滚回顶部后顶部缩略图仍在',
    topState && topState.headImgs > 0, JSON.stringify(topState));
  check('滚回后没有处于加载中的图（未重新请求）',
    topState && topState.allComplete === true, JSON.stringify(topState));

  console.log('\n--- 5. 加载量始终受控 ---');
  // 走完一轮滚动后，已加载数仍应远小于总数。
  // 若卸载逻辑没生效，滚过一遍后 imgs 会累积到接近 cards
  check('滚动一轮后加载量仍受控',
    s2 && s2.imgs < s2.cards,
    `已加载 ${s2 && s2.imgs} / 共 ${s2 && s2.cards} 张`);

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
