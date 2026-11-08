/** 高级搜索纯逻辑。这里的规则决定了发给后端的查询与批量范围——后端会据此对
 *  全部命中执行永久保留、转发等有副作用的动作，所以每条断言都对应一种「发错了
 *  就会处理错一批文件」的缺陷。 */
import { describe, expect, it } from 'vitest';
import {
  buildQuery,
  hasSelection,
  isPicked,
  newGroup,
  newRow,
  parseExts,
  selectionCount,
  togglePick,
  toSelection,
  type SelectionState,
} from './tg-advanced-search';

describe('parseExts', () => {
  it('去点、转小写、中英文分隔符都认、去重', () => {
    // 不这样会怎样：用户写 ".MP4，mkv" 时后端收到 [".MP4，mkv"]，一个都匹配不上
    expect(parseExts(' .MP4，mkv; avi  mp4\t.Pdf')).toEqual(['mp4', 'mkv', 'avi', 'pdf']);
  });
  it('只有分隔符时是空列表', () => {
    expect(parseExts(' , ；. ')).toEqual([]);
  });
});

describe('buildQuery', () => {
  it('四种条件按各自字段转换，取反原样带上', () => {
    const media = { ...newRow('media'), types: ['photo', 'video'] as const };
    const ext = { ...newRow('ext'), exts: 'MKV, .mp4', negate: true };
    const size = { ...newRow('size'), min: '1.5', max: '', unit: 'MB' as const };
    const text = { ...newRow('text'), text: '  周末 ' };
    const { query, errors } = buildQuery(newGroup('any', [
      { ...media, types: [...media.types] }, ext, size, text,
    ]));
    expect(errors).toEqual([]);
    expect(query).toEqual({
      match: 'any',
      conditions: [
        { kind: 'media', types: ['photo', 'video'], negate: false },
        // 不这样会怎样：取反丢失，「不是 mkv」变成「是 mkv」，批量动作作用于恰好相反的一批
        { kind: 'ext', exts: ['mkv', 'mp4'], negate: true },
        // 1.5 MB 必须换算成字节；空的上限是 null（不限），不能变成 0（只剩空文件）
        { kind: 'size', min: 1572864, max: null, negate: false },
        { kind: 'text', text: '周末', negate: false },
      ],
    });
  });

  it('有任何一行不合法就不出查询，并把错误挂到那一行', () => {
    const ok = { ...newRow('text'), text: 'a' };
    const badMedia = newRow('media');
    const badSize = { ...newRow('size'), min: '5', max: '2' };
    const { query, errors } = buildQuery(newGroup('all', [ok, badMedia, badSize]));
    // 不这样会怎样：跳过坏行只发好行，查询被静默放宽，命中比用户以为的多
    expect(query).toBeNull();
    expect(errors).toEqual([
      { rowId: badMedia.id, key: 'adv_search.err_media' },
      { rowId: badSize.id, key: 'adv_search.err_size_order' },
    ]);
  });

  it('大小输入框给的是 number 时也能换算', () => {
    // 不这样会怎样：v-model 绑 type=number 时值是 number，按字符串 .trim() 直接抛错，
    // 点「搜索」毫无反应（真实界面里实测踩到）
    const row = { ...newRow('size'), min: 5, max: '', unit: 'KB' as const };
    expect(buildQuery(newGroup('all', [row])).query?.conditions).toEqual([
      { kind: 'size', min: 5120, max: null, negate: false },
    ]);
  });

  it('大小两端都空、或填了负数都算错', () => {
    const empty = newRow('size');
    const neg = { ...newRow('size'), min: '-1' };
    const { errors } = buildQuery(newGroup('all', [empty, neg]));
    expect(errors.map((e) => e.key)).toEqual(['adv_search.err_size_empty', 'adv_search.err_size_number']);
  });

  it('没有任何条件不能搜', () => {
    // 不这样会怎样：空条件 = 匹配全部，「全选 → 转发」会把整个对话转出去
    const { query, errors } = buildQuery(newGroup('all', []));
    expect(query).toBeNull();
    expect(errors).toEqual([{ rowId: null, key: 'adv_search.err_no_condition' }]);
  });
});

describe('条件组与日期', () => {
  it('子组嵌套原样转成树，取反落在组上', () => {
    const a = { ...newRow('media'), types: ['video' as const] };
    const b = { ...newRow('ext'), exts: 'mkv' };
    const c = { ...newRow('text'), text: '花絮' };
    const inner = newGroup('any', [b, c]);
    inner.negate = true;
    const { query, errors } = buildQuery(newGroup('all', [a, inner]));
    expect(errors).toEqual([]);
    // 不这样会怎样：把子组摊平成同一层，「视频 且 不(mkv 或 花絮)」变成
    // 「视频 且 mkv 且 花絮」，批量动作作用于完全不同的一批消息
    expect(query).toEqual({
      match: 'all',
      conditions: [
        { kind: 'media', types: ['video'], negate: false },
        {
          kind: 'group', match: 'any', negate: true,
          conditions: [
            { kind: 'ext', exts: ['mkv'], negate: false },
            { kind: 'text', text: '花絮', negate: false },
          ],
        },
      ],
    });
  });

  it('空的子组报错并挂到那个组上，子组里的坏行也拦得住', () => {
    const empty = newGroup('all', []);
    const bad = newRow('media');
    const g = newGroup('any', [bad]);
    const { query, errors } = buildQuery(newGroup('all', [{ ...newRow('text'), text: 'x' }, empty, g]));
    // 不这样会怎样：空的「任一满足」组恒为假，整个「全部满足」查询静默变成零命中
    expect(query).toBeNull();
    expect(errors).toEqual([
      { rowId: empty.id, key: 'adv_search.err_empty_group' },
      { rowId: bad.id, key: 'adv_search.err_media' },
    ]);
  });

  it('日期按本地时区换成当天首末秒，区间两端都含', () => {
    const row = { ...newRow('date'), from: '2026-03-01', to: '2026-03-02' };
    const cond = buildQuery(newGroup('all', [row])).query?.conditions[0];
    // 不这样会怎样：结束日取当天 00:00，选「到 3 月 2 日」却漏掉 2 日当天的全部消息
    expect(cond).toEqual({
      kind: 'date',
      from: new Date(2026, 2, 1).getTime() / 1000,
      to: new Date(2026, 2, 3).getTime() / 1000 - 1,
      negate: false,
    });
  });

  it('日期只填一端是开区间；两端都空或颠倒都算错', () => {
    const open = { ...newRow('date'), to: '2026-01-31' };
    expect(buildQuery(newGroup('all', [open])).query?.conditions[0]).toMatchObject({ from: null });
    const empty = newRow('date');
    const rev = { ...newRow('date'), from: '2026-02-02', to: '2026-02-01' };
    const bad = { ...newRow('date'), from: '2026-02-30' };
    expect(buildQuery(newGroup('all', [empty, rev, bad])).errors.map((e) => e.key)).toEqual([
      'adv_search.err_date_empty', 'adv_search.err_date_order', 'adv_search.err_date_invalid',
    ]);
  });
});

describe('选择状态', () => {
  const sel = (): SelectionState => ({ all: false, ids: new Set() });

  it('逐条勾选时只发勾上的 id', () => {
    const s = sel();
    togglePick(s, 'a');
    togglePick(s, 'b');
    togglePick(s, 'a');
    expect(isPicked(s, 'b')).toBe(true);
    expect(isPicked(s, 'a')).toBe(false);
    expect(toSelection(s)).toEqual({ mode: 'ids', ids: ['b'] });
  });

  it('全选后取消的条目进 except，没取消的都算选中', () => {
    const s = { ...sel(), all: true };
    togglePick(s, 'x');
    // 不这样会怎样：全选模式下取消勾选反而变成「只选它」
    expect(isPicked(s, 'x')).toBe(false);
    expect(isPicked(s, 'y')).toBe(true);
    expect(toSelection(s)).toEqual({ mode: 'all', except: ['x'] });
  });

  it('全选且未扫完时数量是下界，扫完才是确数', () => {
    const s = { ...sel(), all: true };
    togglePick(s, 'x');
    // 不这样会怎样：界面显示「已选 59 项」，用户以为只处理 59 个，实际会处理整段历史的全部命中
    expect(selectionCount(s, 60, false)).toEqual({ n: 59, atLeast: true });
    expect(selectionCount(s, 60, true)).toEqual({ n: 59, atLeast: false });
  });

  it('全选但一条都还没扫到时，仍允许执行（后端会继续遍历）', () => {
    expect(hasSelection({ all: true, ids: new Set() }, 0, false)).toBe(true);
    // 扫完了且确实没有命中，就不能执行一个空动作
    expect(hasSelection({ all: true, ids: new Set() }, 0, true)).toBe(false);
    expect(hasSelection(sel(), 10, false)).toBe(false);
  });
});
