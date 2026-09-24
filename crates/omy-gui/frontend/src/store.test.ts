/**
 * Telegram 消息时间线的**状态层**测试（store.js），重点钉住引用跳转这次修的
 * 四个行为，防止它们被以后的改动悄悄改回去：
 *
 * 1. 已在消息时间线上点引用：**不重拉整屏**——不置 loadingMessages、目标在
 *    列表里时甚至不发网络请求，只走 locatingMsg（视图在引用旁转小圈）。
 * 2. 目标不在已加载范围：拉 around 窗口并**合并**进现有列表，而不是替换；
 *    合并后严格新→旧，跳转点之前的更新消息仍在（用户报的「往上滑很快到顶」
 *    就是被替换丢了）。
 * 3. around 窗口按**降序**（新→旧）契约处理，服务端给 has_older/has_newer。
 * 4. 到顶续翻 prepend 更新一页；到底续翻 append 更旧一页，两者互不污染。
 *
 * 后端通信全部 mock：这层测的是前端自己的编排（拉不拉、怎么合、开关怎么置），
 * 不是网络。消息行只取用到的字段（message），按 (对话,消息) 唯一即可。
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';

// 固定 mock 的 api：每个用例再按需改返回值。remoteMessagesAround 返回的窗口
// 必须与后端新契约一致——rows 降序、带 has_older/has_newer。
vi.mock('./api.js', () => ({
  remoteMessagesAround: vi.fn(),
  remoteMessagesAfter: vi.fn(),
  remoteMessages: vi.fn(),
  // 失败路径 store 会用 errCode 取错误码；测试里错误都是普通 Error，给 undefined
  // 让它走兜底文案即可（与真实取不到 code 时的行为一致）。
  errCode: () => undefined,
  uiLog: vi.fn(),
}));

import * as api from './api';
import { state, locateMessage, loadNewerMessages, loadMoreMessages } from './store';

/** 造一条只带消息号的消息行（store 合并/排序只依赖 message）。 */
const m = (message) => ({ message });
/** 造一个降序（新→旧）的连续窗口：from 最新 -> to 最旧。 */
const windowRows = (from, to) => {
  const rows = [];
  for (let id = from; id >= to; id--) rows.push(m(id));
  return rows;
};

/** 每个用例前把消息视图相关状态复位到「已进入某 Telegram 对话的消息栏」。 */
function resetMessagesView() {
  Object.assign(state, {
    remotePlace: 'p1',
    remoteDir: 'tg:-100',
    remoteTab: 'messages',
    remoteViewMode: 'messages',
    remoteMessages: [],
    highlightMsg: null,
    placeError: '',
    loadingMessages: false,
    locatingMsg: null,
    hasMoreMessages: false,
    hasNewerMessages: false,
    loadingMore: false,
    loadingNewer: false,
  });
  vi.clearAllMocks();
}

beforeEach(resetMessagesView);

describe('locateMessage — 已在消息时间线上的内联跳转', () => {
  it('目标已在列表：不发请求、不置整屏 loading，只短暂置 locatingMsg 并高亮', async () => {
    // 屏上已有 #100..#90（新→旧），点的引用指向 #95——本屏内。
    state.remoteMessages = windowRows(100, 90);
    await locateMessage(95);

    expect(vi.mocked(api.remoteMessagesAround)).not.toHaveBeenCalled();
    expect(state.loadingMessages).toBe(false);
    // 内联期间置过定位号，结束后清掉（小圈由这个字段驱动）。
    expect(state.locatingMsg).toBeNull();
    expect(state.highlightMsg).toBe(95);
    expect(state.placeError).toBe('');
  });

  it('目标不在列表：拉 around 窗口并与现有列表合并，跳转前的更新消息必须保留', async () => {
    // 跳转前在看最新一段 #110..#100；引用指向历史中的 #50。
    state.remoteMessages = windowRows(110, 100);
    // 服务端返回以 50 为中心的降序窗口 #62..#38，两头都取满=都能续翻。
    vi.mocked(api.remoteMessagesAround).mockResolvedValue({
      rows: windowRows(62, 38),
      oldest: 38,
      newest: 62,
      found: true,
      has_older: true,
      has_newer: true,
    });

    await locateMessage(50);

    expect(vi.mocked(api.remoteMessagesAround)).toHaveBeenCalledTimes(1);
    expect(state.loadingMessages).toBe(false); // 内联：不能整屏重载
    expect(state.highlightMsg).toBe(50);

    const ids = state.remoteMessages.map((r) => r.message);
    // 关键回归：合并后必须严格降序，且 #50 上方（更小下标=更新方向）仍有
    // #100..#110——否则就是旧实现那种「替换成新窗口、往回滑立刻到顶」。
    for (let i = 1; i < ids.length; i++) {
      expect(ids[i - 1]).toBeGreaterThan(ids[i]);
    }
    expect(ids[0]).toBe(110);
    expect(ids).toContain(50);
    expect(ids).toContain(38);
    expect(ids).toHaveLength(36); // 11 条新 + 25 条旧，两段不相交
    // 双向续翻开关来自服务端。
    expect(state.hasMoreMessages).toBe(true);
    expect(state.hasNewerMessages).toBe(true);
  });

  it('两段有重叠：按消息号去重，不出现重复行', async () => {
    // 现有 #105..#90（16 条），窗口 #100..#76（25 条），重叠 #100..#90（11 条）。
    // 目标必须是窗口内、且不在现有段里的一条（#85），否则根本不会发起拉取。
    state.remoteMessages = windowRows(105, 90);
    vi.mocked(api.remoteMessagesAround).mockResolvedValue({
      rows: windowRows(100, 76),
      oldest: 76,
      newest: 100,
      found: true,
      has_older: false,
      has_newer: true,
    });

    await locateMessage(85);

    const ids = state.remoteMessages.map((r) => r.message);
    expect(new Set(ids).size).toBe(ids.length); // 无重复（数组长度要用 length）
    expect(ids[0]).toBe(105);
    expect(ids[ids.length - 1]).toBe(76);
    expect(ids).toHaveLength(105 - 76 + 1);
  });

  it('目标已删（found=false）：不高亮、给未找到提示，但保留拉回的上下文', async () => {
    state.remoteMessages = windowRows(110, 100);
    vi.mocked(api.remoteMessagesAround).mockResolvedValue({
      rows: windowRows(62, 38),
      oldest: 38,
      newest: 62,
      found: false, // #50 已删，窗口里没有它
      has_older: true,
      has_newer: true,
    });

    await locateMessage(50);

    expect(state.highlightMsg).toBeNull();
    expect(state.placeError).toContain('msgs.locate_not_found');
    // 不假装成功，但拉回的上下文仍在，用户能看到附近内容
    expect(state.remoteMessages.length).toBeGreaterThan(0);
  });

  it('请求飞行途中切走了对话：晚到的结果不得写回当前对话', async () => {
    state.remoteMessages = windowRows(110, 100);
    vi.mocked(api.remoteMessagesAround).mockImplementation(async () => {
      // 请求在飞时用户切到了别的对话
      state.remotePlace = 'p2';
      return { rows: windowRows(62, 38), found: true, has_older: true, has_newer: true };
    });

    await locateMessage(50);

    // 原列表原样保留，没有被 p1 的结果覆盖
    expect(state.remoteMessages.map((r) => r.message)).toEqual(windowRows(110, 100).map((r) => r.message));
    expect(state.highlightMsg).toBeNull();
  });

  it('网络失败：内联时保留现有列表，不用空/错误态盖掉正在看的时间线', async () => {
    state.remoteMessages = windowRows(110, 100);
    vi.mocked(api.remoteMessagesAround).mockRejectedValue(new Error('flood'));

    await locateMessage(50);

    expect(state.remoteMessages).toHaveLength(11);
    expect(state.highlightMsg).toBeNull();
    expect(state.placeError).not.toBe('');
  });
});

describe('locateMessage — 从文件栏/外部进入（非内联）', () => {
  it('时间线还没数据：置整屏 loading，成功后用窗口整体替换', async () => {
    state.remoteViewMode = 'files';
    state.remoteMessages = [];
    // 记录 loadingMessages 在执行过程中确实被置过 true（finally 会清回 false）。
    let sawLoading = false;
    vi.mocked(api.remoteMessagesAround).mockImplementation(async () => {
      sawLoading = state.loadingMessages;
      return { rows: windowRows(62, 38), found: true, has_older: true, has_newer: true };
    });

    await locateMessage(50);

    expect(sawLoading).toBe(true);
    expect(state.remoteViewMode).toBe('messages');
    expect(state.remoteTab).toBe('messages');
    expect(state.remoteMessages.map((r) => r.message)).toEqual(
      windowRows(62, 38).map((r) => r.message),
    );
    expect(state.highlightMsg).toBe(50);
  });

  it('非内联且失败：清空列表并报错', async () => {
    state.remoteViewMode = 'files';
    state.remoteMessages = [];
    vi.mocked(api.remoteMessagesAround).mockRejectedValue(new Error('net'));

    await locateMessage(50);

    expect(state.remoteMessages).toEqual([]);
    expect(state.placeError).not.toBe('');
  });
});

describe('双向续翻', () => {
  it('到顶 loadNewerMessages：把更新一页 prepend 到头部，开关按是否取满一页置', async () => {
    // 当前在历史窗口 #62..#38，上方还有更新消息。
    state.remoteMessages = windowRows(62, 38);
    state.hasNewerMessages = true;
    // after 游标取当前最新 #62，返回它之后的一页 #87..#63（新→旧，取满 25）。
    vi.mocked(api.remoteMessagesAfter).mockResolvedValue(windowRows(87, 63));

    await loadNewerMessages();

    expect(vi.mocked(api.remoteMessagesAfter)).toHaveBeenCalledWith('p1', 'tg:-100', 62);
    const ids = state.remoteMessages.map((r) => r.message);
    expect(ids).toHaveLength(50);
    expect(ids[0]).toBe(87);
    expect(ids[ids.length - 1]).toBe(38);
    for (let i = 1; i < ids.length; i++) expect(ids[i - 1]).toBeGreaterThan(ids[i]);
    expect(state.hasNewerMessages).toBe(true); // 取满 25，可能还有
  });

  it('向新取不满一页：到对话最新，关闭 hasNewerMessages', async () => {
    state.remoteMessages = windowRows(70, 60);
    state.hasNewerMessages = true;
    vi.mocked(api.remoteMessagesAfter).mockResolvedValue(windowRows(75, 71)); // 只有 5 条

    await loadNewerMessages();

    const ids = state.remoteMessages.map((r) => r.message);
    expect(ids[0]).toBe(75);
    expect(state.hasNewerMessages).toBe(false);
  });

  it('hasNewerMessages 为 false 时不发请求', async () => {
    state.remoteMessages = windowRows(62, 38);
    state.hasNewerMessages = false;
    await loadNewerMessages();
    expect(vi.mocked(api.remoteMessagesAfter)).not.toHaveBeenCalled();
  });

  it('加载中重入被忽略（不并发拉两页）', async () => {
    state.remoteMessages = windowRows(62, 38);
    state.hasNewerMessages = true;
    state.loadingNewer = true;
    await loadNewerMessages();
    expect(vi.mocked(api.remoteMessagesAfter)).not.toHaveBeenCalled();
  });

  it('到底 loadMoreMessages：把更旧一页 append 到末尾（原行为不被向新逻辑带偏）', async () => {
    state.remoteMessages = windowRows(62, 38);
    state.hasMoreMessages = true;
    // remoteMessages(place, dir, before) 取当前最旧 #38 之前的一页。
    vi.mocked(api.remoteMessages).mockResolvedValue(windowRows(37, 13));

    await loadMoreMessages();

    expect(vi.mocked(api.remoteMessages)).toHaveBeenCalledWith('p1', 'tg:-100', 38);
    const ids = state.remoteMessages.map((r) => r.message);
    expect(ids).toHaveLength(50);
    expect(ids[0]).toBe(62);
    expect(ids[ids.length - 1]).toBe(13);
    // 向旧取满 25 条，仍可继续
    expect(state.hasMoreMessages).toBe(true);
    // 向新开关不应被向旧续翻改动
    expect(state.hasNewerMessages).toBe(false);
  });
});
