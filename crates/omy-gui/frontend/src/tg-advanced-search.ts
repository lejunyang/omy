/** Telegram 高级搜索弹窗的纯逻辑：表单 → 查询、选择计数。
 *
 * 从组件里拆出来，是因为这些规则决定了「发给后端的是什么」——条件校验漏一条，
 * 后端就会收到一个空扩展名列表之类的查询，按「什么都不匹配」或「什么都匹配」
 * 执行，而批量动作是对**全部命中**做永久保留、转发这类有副作用的事。这类规则要
 * 能脱离界面单测。 */
import { localDayEnd, localDayStart, parseYmd } from './calendar';
import type {
  TgAdvancedQuery,
  TgMediaKind,
  TgSearchCondition,
  TgSearchNode,
  TgSelection,
} from './types';

/** 弹窗里可选的媒体种类，顺序即界面上的顺序。 */
export const MEDIA_KINDS: TgMediaKind[] = ['photo', 'video', 'gif', 'audio', 'voice', 'file', 'link'];

export type SizeUnit = 'KB' | 'MB' | 'GB';
const UNIT_BYTES: Record<SizeUnit, number> = { KB: 1024, MB: 1024 ** 2, GB: 1024 ** 3 };

/** 表单里一行条件的编辑态。各种类的字段都在同一个对象上，切换种类时不丢已填的值。 */
export interface ConditionRow {
  id: number;
  kind: TgSearchCondition['kind'];
  negate: boolean;
  types: TgMediaKind[];
  exts: string;
  /** 绑在 type=number 的输入框上：Vue 的 v-model 会把能解析的值转成 number，
   *  清空时又是空串，所以两种都可能出现。 */
  min: string | number;
  max: string | number;
  unit: SizeUnit;
  text: string;
  /** 日期区间，'YYYY-MM-DD' 或空串（不限）。 */
  from: string;
  to: string;
}

/** 表单里的条件组。根也是一个 GroupRow，只是界面上不给它取反。 */
export interface GroupRow {
  id: number;
  kind: 'group';
  match: 'all' | 'any';
  negate: boolean;
  children: EditNode[];
}

export type EditNode = ConditionRow | GroupRow;

/** 子组最多嵌套几层（根不算）。再深的逻辑在界面上已经读不清，而且两层已经能
 *  表达「(A 且 B) 或 (C 且 不(D 或 E))」这种程度的组合。 */
export const MAX_GROUP_DEPTH = 2;

let nextRowId = 1;

export function newRow(kind: ConditionRow['kind'] = 'media'): ConditionRow {
  return {
    id: nextRowId++, kind, negate: false, types: [], exts: '', min: '', max: '', unit: 'MB', text: '', from: '', to: '',
  };
}

export function newGroup(match: GroupRow['match'] = 'all', children: EditNode[] = [newRow('media')]): GroupRow {
  return { id: nextRowId++, kind: 'group', match, negate: false, children };
}

export function isGroup(n: EditNode): n is GroupRow {
  return n.kind === 'group';
}

/** 把「mp4, .MKV；avi」这类自由输入规整成 ['mp4', 'mkv', 'avi']：去点、转小写、
 *  中英文逗号分号与空白都算分隔、去重。 */
export function parseExts(input: string): string[] {
  const out: string[] = [];
  for (const raw of input.split(/[\s,，;；]+/)) {
    const e = raw.trim().replace(/^\.+/, '').toLowerCase();
    if (e && !out.includes(e)) out.push(e);
  }
  return out;
}

/** 空串 = 不限（null）；非数字或负数返回 NaN，交给校验报错。 */
function toBytes(value: string | number, unit: SizeUnit): number | null {
  const v = String(value ?? '').trim();
  if (!v) return null;
  const n = Number(v);
  if (!Number.isFinite(n) || n < 0) return Number.NaN;
  return Math.round(n * UNIT_BYTES[unit]);
}

/** 校验错误的 i18n 键，附带出错行的 id，界面据此把提示画在那一行下面。 */
export interface RowError {
  rowId: number | null;
  key: string;
}

/** 把一行条件转成查询节点；不合法时记错误并返回 null。 */
function buildCondition(r: ConditionRow, errors: RowError[]): TgSearchCondition | null {
  const err = (key: string) => {
    errors.push({ rowId: r.id, key });
    return null;
  };
  switch (r.kind) {
    case 'media':
      return r.types.length ? { kind: 'media', types: [...r.types], negate: r.negate } : err('adv_search.err_media');
    case 'ext': {
      const exts = parseExts(r.exts);
      return exts.length ? { kind: 'ext', exts, negate: r.negate } : err('adv_search.err_ext');
    }
    case 'size': {
      const min = toBytes(r.min, r.unit);
      const max = toBytes(r.max, r.unit);
      if (Number.isNaN(min) || Number.isNaN(max)) return err('adv_search.err_size_number');
      if (min == null && max == null) return err('adv_search.err_size_empty');
      if (min != null && max != null && min > max) return err('adv_search.err_size_order');
      return { kind: 'size', min, max, negate: r.negate };
    }
    case 'date': {
      const from = r.from.trim();
      const to = r.to.trim();
      if ((from && !parseYmd(from)) || (to && !parseYmd(to))) return err('adv_search.err_date_invalid');
      if (!from && !to) return err('adv_search.err_date_empty');
      if (from && to && from > to) return err('adv_search.err_date_order');
      return { kind: 'date', from: from ? localDayStart(from) : null, to: to ? localDayEnd(to) : null, negate: r.negate };
    }
    default: {
      const text = r.text.trim();
      return text ? { kind: 'text', text, negate: r.negate } : err('adv_search.err_text');
    }
  }
}

function buildNodes(children: EditNode[], errors: RowError[]): TgSearchNode[] {
  const out: TgSearchNode[] = [];
  for (const c of children) {
    if (isGroup(c)) {
      // 空组的真值没有直观含义（全部满足 = 恒真，任一满足 = 恒假），
      // 无论按哪种算都会让结果和用户以为的不一样，所以直接报错
      if (!c.children.length) errors.push({ rowId: c.id, key: 'adv_search.err_empty_group' });
      const conditions = buildNodes(c.children, errors);
      out.push({ kind: 'group', match: c.match, negate: c.negate, conditions });
    } else {
      const n = buildCondition(c, errors);
      if (n) out.push(n);
    }
  }
  return out;
}

/** 把表单（条件树的根）转成后端查询。有任何错误时 query 为 null——宁可不让搜，
 *  也不发一个被静默放宽或收窄的查询。 */
export function buildQuery(root: GroupRow): { query: TgAdvancedQuery | null; errors: RowError[] } {
  const errors: RowError[] = [];
  if (!root.children.length) errors.push({ rowId: null, key: 'adv_search.err_no_condition' });
  const conditions = buildNodes(root.children, errors);
  return { query: errors.length ? null : { match: root.match, conditions }, errors };
}

/** 弹窗内的选择状态。all 模式下记「取消勾选」的，ids 模式下记「勾选」的——
 *  这样全选不需要先把全部命中加载出来。 */
export interface SelectionState {
  all: boolean;
  ids: Set<string>;
}

export function isPicked(sel: SelectionState, id: string): boolean {
  return sel.all ? !sel.ids.has(id) : sel.ids.has(id);
}

export function togglePick(sel: SelectionState, id: string): void {
  if (sel.ids.has(id)) sel.ids.delete(id);
  else sel.ids.add(id);
}

/** 已选条数的显示值。all 模式且还没扫完时，真实总数未知，只能给出下界，
 *  界面写成「N+」——不能把已加载数当总数，否则用户会以为只处理这么多。 */
export function selectionCount(
  sel: SelectionState,
  loaded: number,
  done: boolean,
): { n: number; atLeast: boolean } {
  if (!sel.all) return { n: sel.ids.size, atLeast: false };
  return { n: Math.max(0, loaded - sel.ids.size), atLeast: !done };
}

/** 转成发给后端的选择范围。 */
export function toSelection(sel: SelectionState): TgSelection {
  return sel.all ? { mode: 'all', except: [...sel.ids] } : { mode: 'ids', ids: [...sel.ids] };
}

/** 是否选中了至少一条。all 模式下只要扫描还没结束就可能有，按「有」处理。 */
export function hasSelection(sel: SelectionState, loaded: number, done: boolean): boolean {
  const c = selectionCount(sel, loaded, done);
  return c.n > 0 || c.atLeast;
}
