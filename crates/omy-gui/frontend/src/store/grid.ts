/** 文件网格几何：列数与行高由视图层实测写回，用于分页对齐。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */


/** 文件网格的几何，由 PlaceBrowser 的 recomputeGridColumns 实测后写回。
 *
 * store 自己读不到「虚拟行」的真实高度（行是 WindowList 里的 flex 容器），
 * 所以列数与行高都由视图层供给。默认值只在视图还没挂载的极早时刻兜底。 */
export interface GridLayout {
  /** 每行卡片数。 */
  cols: number;
  /** 一行卡片的高度（px，含行间距）。 */
  rowH: number;
}
const CARD_MIN_W = 122;
const GRID_GAP = 10;
const CARD_ROW_H = 156;
const DEFAULT_GRID_LAYOUT: GridLayout = { cols: 4, rowH: CARD_ROW_H };

/** 当前文件网格布局。视图层用 setGridLayout 更新；读时给默认值兜底。 */
let currentGridLayout: GridLayout = { ...DEFAULT_GRID_LAYOUT };
export function setGridLayout(layout: GridLayout): void {
  currentGridLayout = { cols: Math.max(1, Math.round(layout.cols) || 1), rowH: layout.rowH || CARD_ROW_H };
}

/** 把任意条数向上取整到「整行卡片数」的倍数。
 *
 *  请求 N 条但 N 不是列数整数倍时，最后一行永远填不满、右侧留空——这正是
 *  「网格多出几个空位」的原因。对齐到整行后首屏与每一次续翻都铺满整行。 */
export function alignToGrid(n: number, cols: number = currentGridLayout.cols): number {
  const c = Math.max(1, Math.round(cols) || 1);
  return Math.ceil(Math.max(0, n) / c) * c;
}

/**
 * 首屏该拉多少条：填满可见网格 + 一屏缓冲，并**对齐到整行**。
 *
 * 为什么必须对齐列数：网格是「每行固定 cols 个」，请求条数若不是 cols 的整数
 * 倍，最后一行就缺几块、留下空位。向上取整到整行既铺满当前可见区域，也让下方
 * 多预备一行，滚到时不露白。
 *
 * 几何常量与 PlaceBrowser 的 .grid 覆盖保持一致（见同文件样式注释）：
 * 卡片最小宽 122px、列/行间距 10px、远程卡片行高约 156px。
 */
/** content 左右 padding 余量（与 .content 的 padding 对应，视图层另有实测列数，
 *  这里只在视图还没供给布局时的兜底估算用）。 */
const GRID_H_PAD = 28;
const FILL_MIN = 15;
const FILL_MAX = 100; // 后端单次也封顶 100

export function viewportFillCount(): number {
  const { cols, rowH } = currentGridLayout;
  try {
    // 优先用视图实测的列数；没有时再按容器宽兜底估一次
    let useCols = cols;
    const host = document.querySelector('.content');
    if (host) {
      const w = (host.clientWidth || 0) - GRID_H_PAD;
      if (w >= 40) {
        useCols = Math.max(1, Math.floor((w + GRID_GAP) / (CARD_MIN_W + GRID_GAP)));
      }
      const h = host.clientHeight || 0;
      if (h >= 40) {
        const visibleRows = Math.max(1, Math.ceil(h / rowH));
        // 总条数 = 列数 × 行数；行数取「可见 + 一屏缓冲」（*2），
        // 再向上取整到整行，保证铺满当前网格且下方多预备一屏。
        // 注意是 cols*rows，不是只算行数——漏乘 cols 会让宽屏列多时
        // 首屏只拉几行，网格大片留空。
        const n = alignToGrid(useCols * visibleRows * 2, useCols);
        return Math.min(FILL_MAX, Math.max(FILL_MIN, n));
      }
    }
    // 无可用几何：按「至少 3 整行」兜底，再夹到上下限
    return Math.min(FILL_MAX, Math.max(FILL_MIN, alignToGrid(useCols * 3, useCols)));
  } catch {
    return Math.min(FILL_MAX, Math.max(FILL_MIN, alignToGrid(cols * 3, cols)));
  }
}
