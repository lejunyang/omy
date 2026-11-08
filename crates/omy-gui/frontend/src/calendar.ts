/** 日期选择用的纯日期运算。
 *
 * 一律用 'YYYY-MM-DD' 字符串表示「本地日历上的某一天」，而不是 Date：Date 带时区，
 * 跨夏令时或在 UTC 偏移为负的时区里做「加一天」「取月初」，很容易差出一天，而且
 * 只在特定时区的机器上才出错。字符串只在需要时间戳时，才按本地时区转换一次。 */

export interface Ymd {
  y: number;
  m: number; // 1–12
  d: number;
}

const pad = (n: number, w = 2) => String(n).padStart(w, '0');

export function formatYmd({ y, m, d }: Ymd): string {
  return `${pad(y, 4)}-${pad(m)}-${pad(d)}`;
}

export function daysInMonth(y: number, m: number): number {
  return new Date(y, m, 0).getDate();
}

/** 严格解析：格式不对或日子不存在（2月30日）都返回 null，不做「自动进位」。 */
export function parseYmd(s: string): Ymd | null {
  const r = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s?.trim?.() ?? '');
  if (!r) return null;
  const y = Number(r[1]);
  const m = Number(r[2]);
  const d = Number(r[3]);
  if (m < 1 || m > 12 || d < 1 || d > daysInMonth(y, m)) return null;
  return { y, m, d };
}

/** 按日历加减天数。借 Date 的本地正午做进位：正午离两端都有 12 小时，夏令时
 *  前后的 ±1 小时不会把日期推到隔壁那天。 */
export function addDays(s: string, n: number): string {
  const p = parseYmd(s);
  if (!p) return s;
  const dt = new Date(p.y, p.m - 1, p.d + n, 12);
  return formatYmd({ y: dt.getFullYear(), m: dt.getMonth() + 1, d: dt.getDate() });
}

/** 月份加减。日子超出目标月份时夹到月底（1月31日 + 1 个月 = 2月28/29日）。 */
export function addMonths(s: string, n: number): string {
  const p = parseYmd(s);
  if (!p) return s;
  const idx = p.y * 12 + (p.m - 1) + n;
  const y = Math.floor(idx / 12);
  const m = (idx % 12) + 1;
  return formatYmd({ y, m, d: Math.min(p.d, daysInMonth(y, m)) });
}

export function todayYmd(now = new Date()): string {
  return formatYmd({ y: now.getFullYear(), m: now.getMonth() + 1, d: now.getDate() });
}

/** 一周从周几开始（0 = 周日）。中文习惯周一，英文界面沿用周日。 */
export function weekStartFor(lang: string): number {
  return lang.startsWith('zh') ? 1 : 0;
}

/** 某月的日历网格：固定 6 行 × 7 列，含前后月份补位。固定行数是为了切换月份时
 *  面板高度不跳，不然下方的按钮会在手指下面移动。 */
export function monthGrid(y: number, m: number, weekStart: number): string[] {
  const first = new Date(y, m - 1, 1, 12).getDay();
  const lead = (first - weekStart + 7) % 7;
  const start = addDays(formatYmd({ y, m, d: 1 }), -lead);
  return Array.from({ length: 42 }, (_, i) => addDays(start, i));
}

/** 某天在本地时区的 00:00:00，单位秒。 */
export function localDayStart(s: string): number | null {
  const p = parseYmd(s);
  if (!p) return null;
  return Math.floor(new Date(p.y, p.m - 1, p.d).getTime() / 1000);
}

/** 某天在本地时区的 23:59:59，单位秒。取「次日零点 − 1」，而不是 +86399：
 *  夏令时切换那天不是 86400 秒。 */
export function localDayEnd(s: string): number | null {
  const p = parseYmd(s);
  if (!p) return null;
  return Math.floor(new Date(p.y, p.m - 1, p.d + 1).getTime() / 1000) - 1;
}
