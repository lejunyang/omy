/** 多语言。
 *
 * # 相比重构前的变化
 *
 * 逻辑几乎原样保留（查表、插值、复数、locale 格式化），
 * 只把「当前语言」和「文案表」改成 Vue 的响应式引用。
 *
 * 收益是切语言不再需要手动调 `render()`：所有用到 `t()` 的组件
 * 会自动重算。重构前那一版必须记得在 `switchLanguage` 末尾补一次
 * 全量重绘，漏了就是半个界面还是旧语言。
 *
 * 首期只有简中和英文（决策 D-29），所以仍然没有引入 vue-i18n——
 * 那是 40 KB 依赖加一套编译期提取流程，而我们只需要查表和插值。
 * 语言超过四种、或出现复杂复数规则时再换。
 */

import { ref, computed } from 'vue';

/** 支持的语言。顺序决定「切换语言」按钮的轮转次序。 */
export const LANGS = ['zh-CN', 'en'];

const bundle = ref<Record<string, unknown>>({});
const errors = ref<Record<string, string>>({});
const current = ref('en');

/** 当前语言标签（响应式）。 */
export const lang = computed(() => current.value);

/** 载入某个语言的文案。 */
export async function load(lang) {
  const [common, errs] = await Promise.all([
    fetch(`locales/${lang}.json`).then((r) => r.json()),
    fetch(`locales/${lang}.errors.json`).then((r) => r.json()),
  ]);
  bundle.value = common;
  errors.value = errs;
  current.value = lang;
  document.documentElement.lang = lang;
}

/** 轮转到下一种语言。 */
export function nextLang() {
  const i = LANGS.indexOf(current.value);
  return LANGS[(i + 1) % LANGS.length];
}

/** 按点分路径取文案，缺失时返回键名本身。
 *
 * 返回键名而不是空串是有意的：界面上出现 `view.empty_title`
 * 这样的字符串一眼就能看出是漏翻译，而空白会被当成布局问题查半天。
 */
/** 插值参数：键 -> 任意可转字符串的值。 */
export type I18nParams = Record<string, string | number | boolean | null | undefined>;

export function t(key: string, params?: I18nParams): string {
  let node: unknown = bundle.value;
  for (const part of key.split('.')) {
    if (node == null || typeof node !== 'object') return key;
    node = (node as Record<string, unknown>)[part];
  }
  if (typeof node !== 'string') return key;
  return interpolate(node, params);
}

/** 带复数的文案。 */
export function tn(key: string, count: number, params?: I18nParams): string {
  // 中文没有单复数，但仍走同一套键：文案文件里两个键写同样的内容。
  // 这样切语言时代码不用分支判断。
  const suffix = count === 1 ? '_one' : '_other';
  const merged = { count, ...(params || {}) };
  const withSuffix = t(`${key}${suffix}`, merged);
  // 没有复数形式就退回基础键
  return withSuffix === `${key}${suffix}` ? t(key, merged) : withSuffix;
}

/** 翻译后端返回的错误码。 */
export function te(code?: string, fallback?: string): string {
  if (typeof code === 'string' && errors.value[code]) return errors.value[code];
  return fallback || errors.value.internal || code || '';
}

/** `{{name}}` 插值。 */
function interpolate(s: string, params?: I18nParams): string {
  if (!params) return s;
  return s.replace(/\{\{(\w+)\}\}/g, (m, k) =>
    Object.hasOwn(params, k) ? String(params[k]) : m,
  );
}

/** 文件大小的本地化格式。
 *
 * 单位用 KB/MB/GB（1024 进制）。虽然严格说该叫 KiB，
 * 但文件管理器普遍这么显示，跟随用户既有认知。
 */
export function formatSize(bytes?: number | null): string {
  if (bytes == null) return '';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let v = Number(bytes);
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  // 数字部分随 locale 变化：中文用 1,234.5，德语用 1.234,5
  const digits = i === 0 ? 0 : v < 10 ? 1 : 0;
  const num = v.toLocaleString(current.value, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
  return `${num} ${units[i]}`;
}

/** 时长格式化，毫秒 → `1:23:45` 或 `12:34`。 */
export function formatDuration(ms?: number | null): string {
  if (ms == null) return '';
  const total = Math.floor(Number(ms) / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

/** locale-aware 的文件名比较器（文档 §7.4）。
 *
 * **不能用字节序**：中文按 Unicode 码点排出来的顺序毫无意义，
 * 用户期望的是拼音序。`Intl.Collator` 在 zh-CN 下正好按拼音排。
 *
 * `numeric: true` 让 `第2章` 排在 `第10章` 前面——
 * 字典序会把 `10` 排在 `2` 前，这在文件列表里非常刺眼。
 */
export function collator() {
  return new Intl.Collator(current.value, { numeric: true, sensitivity: 'base' });
}
