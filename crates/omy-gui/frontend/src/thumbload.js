/** 缩略图按可见性加载，并预取即将进入视口的部分。
 *
 * # 为什么不能简单地全部渲染
 *
 * 扫描时已经把 `has_thumbnail` 一次性填好了（那是**后端**的事，很便宜：
 * 按 header_len 精确读取约 0.09 ms/文件）。但让 WebView 同时持有几千张
 * 已解码的图是另一回事：一张 320×240 的缩略图解码后占 RGBA
 * 320×240×4 ≈ 300 KB，一万张就是 3 GB。压缩后每张只有几 KB，
 * 所以「文件很小」完全不代表「内存占用很小」。
 *
 * 而且每个 `/thumb/` 请求都要在后端解一次 header 并解密 TLV。一次性发出
 * 几千个请求会把协议线程占满，表现是**整个界面卡住**，而不只是图片慢。
 *
 * # 为什么不用 `loading="lazy"`
 *
 * 原来 `<img>` 上写的是 `loading="lazy"`，但它有两个不够用的地方：
 *
 * 1. **只推迟，不释放**。滚过一万个文件后，那一万张图仍然全在内存里。
 * 2. **边距不可控**。各 WebView 自己决定提前多少加载，行为不一致；
 *    我们要的是明确的预取距离。
 *
 * 所以自己用 IntersectionObserver 管，`loading="lazy"` 保留作为兜底。
 *
 * # 两个边距，而不是一个
 *
 * 只用一个边距的话，图片会在刚离开视口时就被卸载，用户往回滚一点就要
 * 重新请求、重新解密、重新解码——表现是**来回滚动时图片不停闪**。
 *
 * 所以分开：进入 `LOAD_MARGIN` 就开始加载（预取），直到离开更远的
 * `KEEP_MARGIN` 才卸载（保活）。中间这段缓冲让正常幅度的来回滚动
 * 不产生任何重新加载。
 *
 * # 为什么用共享的 observer
 *
 * 每张卡各建一个 IntersectionObserver 的话，几千个 observer 本身就是
 * 性能问题——那等于用一个性能方案换来另一个。这里全局只有两个，
 * 所有卡片注册到同一对上。
 */

import { ref, watch, onBeforeUnmount } from 'vue';

/** 提前多少像素开始加载（预取距离）。
 *
 * 600 ≈ 网格里三四行的高度：以常规滚动速度，图片能在进入视口前
 * 加载完，用户看不到占位符。调小会看到空白格子，调大则失去意义
 * （接近一次性全加载）。
 */
export const LOAD_MARGIN = 600;

/** 离开视口多远才卸载（保活距离）。
 *
 * 取 LOAD_MARGIN 的三倍以上：留出足够缓冲，让「往下翻一屏再翻回来」
 * 这种最常见的操作完全不触发重新加载。
 */
export const KEEP_MARGIN = 2000;

/** 元素 -> 回调。用 WeakMap：卡片销毁后条目自动消失，不会漏。 */
const registry = new WeakMap();

/** root 元素 -> { load, keep } 一对 observer。
 *
 * 按 root 分组缓存，而不是全局各一个：root 不同的 observer 不能共用。
 * 用 Map 而非 WeakMap 是因为 `null`（视口）也要能作键。
 */
const observerPairs = new Map();

/** 找出元素最近的可滚动祖先。
 *
 * # 为什么必须显式指定 root
 *
 * IntersectionObserver 不指定 root 时以**视口**为准，而 `rootMargin`
 * 只扩大视口那个矩形——**祖先滚动容器的裁剪不受它影响**。本应用的
 * 文件列表是嵌套在 `.main` 里的滚动 div，于是滚出容器的卡片无论
 * rootMargin 设多大都算「不相交」，预取完全失效。
 *
 * 症状是加载数恰好等于**可见**数量（实测 20/60，而按 600px 预取距离
 * 应该是 54 张左右）。这个偏差很容易被当成正常——毕竟懒加载「看起来
 * 是在工作」，只有把预期数量算出来比对才会发现预取那一半没生效。
 * 变异测试也印证了：把 LOAD_MARGIN 改成 0 时行为毫无变化。
 */
function scrollParent(el) {
  let p = el.parentElement;
  while (p && p !== document.body) {
    const s = getComputedStyle(p);
    if (/(auto|scroll|overlay)/.test(s.overflowY) && p.scrollHeight > p.clientHeight) {
      return p;
    }
    p = p.parentElement;
  }
  // 找不到就退回视口。宁可预取失效，也不能不加载
  return null;
}

/** 取得（或惰性建立）某个 root 上的两个共享 observer。
 *
 * 不给每张卡各建一个：几千个 observer 本身就是性能问题，
 * 那等于用一个性能方案换来另一个。
 */
function observersFor(root) {
  const cached = observerPairs.get(root);
  if (cached) return cached;
  const pair = {
    load: new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          const h = registry.get(e.target);
          if (h) h.load();
        }
      },
      { root, rootMargin: `${LOAD_MARGIN}px` },
    ),
    keep: new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          // 只处理「离开保活范围」，进入由 load 那个负责
          if (e.isIntersecting) continue;
          const h = registry.get(e.target);
          if (h) h.unload();
        }
      },
      { root, rootMargin: `${KEEP_MARGIN}px` },
    ),
  };
  observerPairs.set(root, pair);
  return pair;
}

/** 供测试使用：重置共享状态。 */
export function _resetObservers() {
  for (const pair of observerPairs.values()) {
    pair.load.disconnect();
    pair.keep.disconnect();
  }
  observerPairs.clear();
}

/** 让一个元素的缩略图随可见性加载/卸载。
 *
 * 返回 `{ thumbEl, shouldLoad }`：把 `thumbEl` 绑到要观察的元素上，
 * 用 `shouldLoad` 控制 `<img>` 是否渲染。
 */
export function useThumbLoad() {
  const thumbEl = ref(null);
  const shouldLoad = ref(false);
  // 当前已注册的元素及其 observer 对，用于在换元素时精确注销
  let observed = null;
  let observedPair = null;

  function detach() {
    if (!observed) return;
    if (observedPair) {
      observedPair.load.unobserve(observed);
      observedPair.keep.unobserve(observed);
    }
    registry.delete(observed);
    observed = null;
    observedPair = null;
  }

  /* 用 watch 而不是 onMounted。
   *
   * EntryCard 的模板是一条 v-if / v-else-if 链（目录 / 锁定 / 已解锁 /
   * 普通文件），**解锁后条目会从「锁定」分支切到「已解锁」分支**。切分支
   * 时 Vue 销毁旧元素、创建新元素，而 onMounted 只跑一次：它拿到的是
   * 锁定分支的元素（那个分支上根本没有这个 ref，于是拿到 null），
   * 新元素永远不会被 observe，shouldLoad 就一直是 false。
   *
   * 症状是**解锁后一张缩略图都不显示**，而后端字段、IntersectionObserver
   * 本身、meta 挂载全都正常——三处都查不出问题，因为问题在「注册时机」。
   * 实测正是如此：卡片渲染的是已解锁分支、known.size 正常显示，
   * 但 imgs 恒为 0。
   *
   * watch 带 flush:'post' 在 DOM 更新后运行，元素每次变化都会重新注册。
   */
  watch(
    thumbEl,
    (el) => {
      detach();
      if (!el) return;
      // 没有 IntersectionObserver（老 WebView、测试环境）时退化为直接加载。
      // 宁可多占内存，也不能让缩略图完全不显示
      if (typeof IntersectionObserver === 'undefined') {
        shouldLoad.value = true;
        return;
      }
      const pair = observersFor(scrollParent(el));
      registry.set(el, {
        load: () => {
          shouldLoad.value = true;
        },
        unload: () => {
          shouldLoad.value = false;
        },
      });
      observed = el;
      observedPair = pair;
      pair.load.observe(el);
      pair.keep.observe(el);
    },
    { immediate: true, flush: 'post' },
  );

  onBeforeUnmount(detach);

  return { thumbEl, shouldLoad };
}
