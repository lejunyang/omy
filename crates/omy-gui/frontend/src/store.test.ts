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
  // 任务 #8（虚拟引用/浏览竞态）用到的命令，全部 mock 掉，绝不出网。
  virtualBrowse: vi.fn(),
  remoteBrowse: vi.fn(),
  remoteBrowseTab: vi.fn(),
  remoteCacheFileStats: vi.fn(),
  remoteMetaGet: vi.fn(),
  remoteMetaPut: vi.fn(),
  remoteEffectiveCaps: vi.fn(),
  remoteDirProtected: vi.fn(),
  // 任务 #10 虚拟位置管理命令
  virtualRemoveRef: vi.fn(),
  virtualMoveRef: vi.fn(),
  virtualCopyRefs: vi.fn(),
  virtualRemoveFolder: vi.fn(),
  virtualRenameFolder: vi.fn(),
  virtualAddFolder: vi.fn(),
  virtualRename: vi.fn(),
  virtualDelete: vi.fn(),
  virtualEncrypt: vi.fn(),
  virtualUnlock: vi.fn(),
  virtualLock: vi.fn(),
  virtualPlaces: vi.fn(),
  remotePlaceList: vi.fn(),
  // 加密虚拟位置时收集同密码 vault 的探测命令（mock，绝不出网）
  vaultParamsOf: vi.fn(),
  remotePlaceVaults: vi.fn(),
  credentialCount: vi.fn(),
  // 失败路径 store 会用 errCode 取错误码；测试里错误都是普通 Error，给 undefined
  // 让它走兜底文案即可（与真实取不到 code 时的行为一致）。
  errCode: () => undefined,
  uiLog: vi.fn(),
}));

import * as api from './api';
import {
  state,
  locateMessage,
  loadNewerMessages,
  loadMoreMessages,
  reloadVirtualDir,
  activateVirtualEntry,
  openRemotePlace,
  reloadRemoteDir,
  remoteFileCacheKeyFor,
  toggleRemoteSelected,
  remoteSelectionActive,
  clearRemoteSelection,
  selectedRemoteEntries,
  locateFile,
  fileTabForName,
  tgMsgOfId,
  removeVirtualRefs,
  setVirtualClipboard,
  canPasteVirtual,
  pasteVirtualHere,
  promptVirtualEncrypt,
  confirmVirtualEncrypt,
} from './store';

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

// ---------------------------------------------------------------------------
// 任务 #8：虚拟引用（缩略图/缓存角标/直接跳消息 tab）与远程浏览竞态
// ---------------------------------------------------------------------------

/** 一个可控的 Promise：测试里先发起加载、在请求飞行途中切位置，再放行旧请求。 */
function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

/** 造一条虚拟引用的后端行（virtual_browse 返回的形状）。 */
const vref = (over: Record<string, unknown> = {}) => ({
  id: 'vr1',
  name: 'a.jpg',
  is_dir: false,
  size: 1234,
  source_state: 'available',
  source_place: 'p1',
  source_dir: 'tg:-100',
  source_file: 'tg:-100:38',
  thumb_token: null,
  ...over,
});

describe('虚拟位置浏览 reloadVirtualDir', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: 'v1',
      remoteDir: '',
      remoteTab: 'media',
      remoteViewMode: 'messages', // 故意留在消息视图，验证进虚拟位置会复位成文件
      remoteMessages: [{ message: 9 }],
      virtualPlaces: [{ id: 'v1' }, { id: 'v2' }],
      remotePlaces: [],
      remoteItems: [],
      remoteCacheStat: {},
      busy: false,
      busyKey: '',
      placeError: '',
    });
    vi.clearAllMocks();
    vi.mocked(api.remoteCacheFileStats).mockResolvedValue([]);
  });

  it('后端给的 thumb_token 透传到卡片；并复位掉残留的消息视图/tab', async () => {
    vi.mocked(api.virtualBrowse).mockResolvedValue([
      vref({ thumb_token: 'piabc' }),
    ]);
    await reloadVirtualDir();

    expect(state.remoteItems).toHaveLength(1);
    expect(state.remoteItems[0].thumb_token).toBe('piabc');
    expect(state.remoteItems[0].is_ref).toBe(true);
    // 从「定位到真实位置的消息视图」返回虚拟位置，必须回到文件网格，否则界面
    // 还停在上一个真实对话的消息时间线，看起来就是「点虚拟位置没反应」。
    expect(state.remoteViewMode).toBe('files');
    expect(state.remoteTab).toBe('media');
    expect(state.remoteMessages).toEqual([]);
    expect(state.hasMoreFiles).toBe(false);
  });

  it('为引用按**源真实位置**批量查缓存角标（不是用虚拟位置 id）', async () => {
    vi.mocked(api.virtualBrowse).mockResolvedValue([
      vref({ id: 'vr1', source_place: 'p1', source_file: 'tg:-100:38' }),
      vref({ id: 'vr2', source_place: 'p9', source_file: 'tg:-200:55' }),
    ]);
    vi.mocked(api.remoteCacheFileStats)
      // 两个源位置各查一次，分别返回「整文件已缓存」「部分缓存」。
      .mockResolvedValueOnce([{ cached_blocks: 4, total_blocks: 4, cached_bytes: 10, fully_cached: true, pinned: false }])
      .mockResolvedValueOnce([{ cached_blocks: 1, total_blocks: 4, cached_bytes: 2, fully_cached: false, pinned: false }]);

    await reloadVirtualDir();

    expect(vi.mocked(api.remoteCacheFileStats)).toHaveBeenCalledTimes(2);
    // 第一条查询必须带源位置 p1 和源 file_id，而不是虚拟位置 v1 / 引用 id vr1。
    const p1Call = vi.mocked(api.remoteCacheFileStats).mock.calls
      .find((c) => c[0][0].place_id === 'p1');
    expect(p1Call?.[0][0].path).toBe('tg:-100:38');
    // remoteFileCache（卡片角标）按条目解析到源 key 后能命中。
    const f1 = state.remoteItems.find((x) => x.id === 'vr1');
    expect(f1).toBeTruthy();
    expect(remoteFileCacheKeyFor(f1)).toBe('p1\u0001tg:-100:38');
  });

  it('missing/locked 的引用不发缓存查询，也不尝试缩略图', async () => {
    vi.mocked(api.virtualBrowse).mockResolvedValue([
      vref({ id: 'vr1', source_state: 'missing', source_place: null }),
    ]);
    await reloadVirtualDir();
    expect(vi.mocked(api.remoteCacheFileStats)).not.toHaveBeenCalled();
    expect(state.remoteItems[0].thumb_token).toBeNull();
  });
});

describe('activateVirtualEntry — 定位到真实位置直接进消息 tab', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: 'v1',
      remoteDir: '',
      remoteTab: 'media',
      remoteViewMode: 'files',
      remoteMessages: [],
      remoteDirName: '虚拟',
      highlightMsg: null,
      virtualPlaces: [{ id: 'v1' }],
      remoteItems: [],
    });
    vi.clearAllMocks();
  });

  const entry = {
    is_dir: false,
    source_state: 'available',
    source_place: 'p1',
    source_dir: 'tg:-100',
    source_file: 'tg:-100:38',
  };

  it('不加载根/文件栏：只切消息时间线并定位高亮', async () => {
    vi.mocked(api.remoteMessagesAround).mockResolvedValue({
      rows: [{ message: 38 }], found: true, has_older: false, has_newer: false,
    });
    await activateVirtualEntry(entry, 'msg');

    // 关键：不能先刷一遍文件网格（那会先闪「文件」tab 再跳「消息」tab）。
    expect(vi.mocked(api.remoteBrowse)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteBrowseTab)).not.toHaveBeenCalled();
    expect(vi.mocked(api.virtualBrowse)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteMessagesAround)).toHaveBeenCalledTimes(1);
    expect(state.remotePlace).toBe('p1');
    expect(state.remoteDir).toBe('tg:-100');
    expect(state.remoteTab).toBe('messages');
    expect(state.remoteViewMode).toBe('messages');
    expect(state.highlightMsg).toBe(38);
  });

  it('源 missing：只提示，不切位置不发请求', async () => {
    await activateVirtualEntry({ ...entry, source_state: 'missing', source_place: null });
    expect(state.remotePlace).toBe('v1');
    expect(vi.mocked(api.remoteMessagesAround)).not.toHaveBeenCalled();
  });
});

describe('远程浏览竞态：A 加载中点 B，A 的结果不得覆盖 B', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: '',
      remoteDir: '',
      remoteTab: 'media',
      remoteViewMode: 'files',
      remoteMessages: [],
      virtualPlaces: [{ id: 'v1' }, { id: 'v2' }],
      remotePlaces: [],
      remoteItems: [],
      busy: false,
      busyKey: '',
      placeError: '',
      remoteDirCaps: null,
      remoteProtected: false,
      hasMoreFiles: false,
    });
    vi.clearAllMocks();
    vi.mocked(api.remoteMetaGet).mockResolvedValue(null); // 无磁盘快照
    vi.mocked(api.remoteEffectiveCaps).mockResolvedValue(null);
  });

  it('两个虚拟位置：旧位置晚到的 virtual_browse 结果被整份丢弃', async () => {
    const a = deferred<unknown[]>();
    vi.mocked(api.virtualBrowse)
      .mockImplementationOnce(() => a.promise as Promise<never>)
      .mockResolvedValueOnce([vref({ id: 'B', source_place: 'p2' })]);
    vi.mocked(api.remoteCacheFileStats).mockResolvedValue([]);

    const p1 = openRemotePlace('v1'); // 不 await：A 挂起
    await Promise.resolve(); await Promise.resolve();
    const p2 = openRemotePlace('v2'); // 用户在 A 加载中点了 B
    await p2;
    expect(state.remoteItems.map((x) => x.id)).toEqual(['B']);

    a.resolve([vref({ id: 'A', source_place: 'p1' })]); // A 这时候才回来
    await p1;

    expect(state.remotePlace).toBe('v2');
    expect(state.remoteItems.map((x) => x.id)).toEqual(['B']); // 没被 A 覆盖
    expect(state.busy).toBe(false);
  });

  it('两个真实位置：旧位置晚到的网络 browse 结果被序号守卫丢弃', async () => {
    const a = deferred<unknown[]>();
    vi.mocked(api.remoteBrowse)
      .mockImplementationOnce(() => a.promise as Promise<never>)
      .mockResolvedValueOnce([{ id: 'B', is_dir: false }]);

    const p1 = openRemotePlace('p1');
    await Promise.resolve(); await Promise.resolve();
    const p2 = openRemotePlace('p2');
    await p2;
    expect(state.remoteItems.map((x) => x.id)).toEqual(['B']);

    a.resolve([{ id: 'A', is_dir: false }]);
    await p1;

    expect(state.remotePlace).toBe('p2');
    expect(state.remoteItems.map((x) => x.id)).toEqual(['B']);
    expect(state.busy).toBe(false);
  });
});


// ---------------------------------------------------------------------------
// 任务 #9：消息 / 虚拟引用「定位到源文件」
// ---------------------------------------------------------------------------

describe('fileTabForName — 没有后端 media_tab 时的分栏兜底', () => {
  it('图片/视频归 media；音乐归 audio；gif/webp 归 gif；文档归 file', () => {
    expect(fileTabForName('a.jpg')).toBe('media');
    expect(fileTabForName('a.MP4')).toBe('media');
    expect(fileTabForName('song.mp3')).toBe('audio');
    expect(fileTabForName('loop.gif')).toBe('gif');
    expect(fileTabForName('report.pdf')).toBe('file');
    expect(fileTabForName('x.omy')).toBe('file');
    expect(fileTabForName(null)).toBe('file');
  });
});

describe('tgMsgOfId', () => {
  it('tg:<chat>:<msg> 取消息号；其它形状给 null', () => {
    expect(tgMsgOfId('tg:-100:38')).toBe(38);
    expect(tgMsgOfId('tg:-100')).toBeNull();
    expect(tgMsgOfId('/webdav/path')).toBeNull();
    expect(tgMsgOfId(null)).toBeNull();
  });
});

describe('locateFile — 在文件网格里定位一个文件', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: 'p1',
      remoteDir: 'tg:-100',
      remoteTab: 'media',
      remoteViewMode: 'files',
      remoteMessages: [],
      remoteItems: [],
      virtualPlaces: [],
      remotePlaces: [{ id: 'p1', kind: 'telegram' }],
      highlightFile: null,
      placeError: '',
      hasMoreFiles: true,
      remoteCacheStat: {},
    });
    vi.clearAllMocks();
    // mockReset 连实现一起清：clearAllMocks 只清调用记录，上一个用例残留的
    // mockResolvedValueOnce 队列会被本用例误取，造成「明明没找到却高亮」。
    vi.mocked(api.remoteBrowseTab).mockReset();
    vi.mocked(api.remoteCacheFileStats).mockResolvedValue([]);
    vi.mocked(api.remoteMetaGet).mockResolvedValue(null);
    vi.mocked(api.remoteMetaPut).mockResolvedValue(undefined);
    vi.mocked(api.remoteEffectiveCaps).mockResolvedValue(null);
    vi.mocked(api.remoteDirProtected).mockResolvedValue(false);
  });

  it('目标已在当前分栏列表：不发请求，切文件视图并高亮', async () => {
    state.remoteItems = [{ id: 'tg:-100:38', name: 'a.jpg', is_dir: false, media_tab: 'media' }];
    await locateFile({ id: 'tg:-100:38', tab: 'media', name: 'a.jpg' });
    expect(vi.mocked(api.remoteBrowseTab)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteBrowse)).not.toHaveBeenCalled();
    expect(state.remoteViewMode).toBe('files');
    expect(state.remoteTab).toBe('media');
    expect(state.highlightFile).toBe('tg:-100:38');
  });

  it('目标在别的分栏：先切到目标 tab（触发一次该分栏浏览）再高亮', async () => {
    // 当前媒体栏列表没有目标；切到 file 栏后返回包含目标的一页。
    state.remoteItems = [{ id: 'tg:-100:1', name: 'x.jpg', is_dir: false }];
    vi.mocked(api.remoteBrowseTab).mockResolvedValue([
      { id: 'tg:-100:50', name: 'doc.pdf', is_dir: false, media_tab: 'file' },
    ]);
    await locateFile({ id: 'tg:-100:50', tab: 'file', name: 'doc.pdf' });
    expect(vi.mocked(api.remoteBrowseTab)).toHaveBeenCalledTimes(1);
    expect(state.remoteTab).toBe('file');
    expect(state.highlightFile).toBe('tg:-100:50');
  });

  it('目标不在已加载范围：以消息号为游标拉一页合并进来，再高亮', async () => {
    // 列表预置一条非目标内容（同 tab、列表非空 → 不触发首屏补刷），只剩锚点一次调用。
    state.remoteItems = [{ id: 'tg:-100:200', name: 'new.jpg', is_dir: false }];
    vi.mocked(api.remoteBrowseTab).mockResolvedValue([
      { id: 'tg:-100:38', name: 'old.jpg', is_dir: false, media_tab: 'media' },
    ]);
    await locateFile({ id: 'tg:-100:38', tab: 'media', name: 'old.jpg' });
    // before 游标 = 38+1 = 39，让目标落在这一页里；且只有锚点这一次调用。
    expect(vi.mocked(api.remoteBrowseTab)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(api.remoteBrowseTab).mock.calls[0][3]).toBe(39);
    const ids = state.remoteItems.map((x) => x.id);
    expect(ids).toContain('tg:-100:200');
    expect(ids).toContain('tg:-100:38');
    expect(state.highlightFile).toBe('tg:-100:38');
  });

  it('拉了一页仍没有：不高亮，给未找到提示，且锚点页只拉一次（不循环续翻）', async () => {
    // 预置非目标内容，列表非空，不触发首屏补刷。
    state.remoteItems = [{ id: 'tg:-100:200', name: 'new.jpg', is_dir: false }];
    vi.mocked(api.remoteBrowseTab).mockResolvedValue([
      { id: 'tg:-100:99', name: 'other.jpg', is_dir: false },
    ]);
    await locateFile({ id: 'tg:-100:38', tab: 'media', name: 'x.jpg' });
    expect(vi.mocked(api.remoteBrowseTab)).toHaveBeenCalledTimes(1);
    expect(state.highlightFile).toBeNull();
    expect(state.placeError).toContain('locate_file_not_found');
  });
});

describe('虚拟引用 activateVirtualEntry — Telegram 双落点', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: 'v1',
      remoteDir: '',
      remoteTab: 'media',
      remoteViewMode: 'files',
      remoteMessages: [],
      remoteItems: [],
      virtualPlaces: [{ id: 'v1' }],
      remotePlaces: [{ id: 'p1', kind: 'telegram' }],
      highlightFile: null,
      highlightMsg: null,
      placeError: '',
      remoteCacheStat: {},
    });
    vi.clearAllMocks();
    vi.mocked(api.remoteCacheFileStats).mockResolvedValue([]);
    vi.mocked(api.remoteMetaGet).mockResolvedValue(null);
    vi.mocked(api.remoteMetaPut).mockResolvedValue(undefined);
    vi.mocked(api.remoteEffectiveCaps).mockResolvedValue(null);
    vi.mocked(api.remoteDirProtected).mockResolvedValue(false);
  });

  const ref = (over = {}) => ({
    is_dir: false,
    source_state: 'available',
    source_place: 'p1',
    source_dir: 'tg:-100',
    source_file: 'tg:-100:38',
    name: 'a.jpg',
    source_media_tab: 'media',
    ...over,
  });

  it("mode='msg'：直接进消息时间线，不刷文件网格", async () => {
    vi.mocked(api.remoteMessagesAround).mockResolvedValue({
      rows: [{ message: 38 }], found: true, has_older: false, has_newer: false,
    });
    await activateVirtualEntry(ref(), 'msg');
    expect(vi.mocked(api.remoteBrowseTab)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteBrowse)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteMessagesAround)).toHaveBeenCalledTimes(1);
    expect(state.remoteTab).toBe('messages');
    expect(state.highlightMsg).toBe(38);
  });

  it("mode='file'：切源位置后在记住的分栏网格里定位文件，不进消息视图", async () => {
    vi.mocked(api.remoteBrowseTab).mockResolvedValue([
      { id: 'tg:-100:38', name: 'a.jpg', is_dir: false, media_tab: 'media' },
    ]);
    await activateVirtualEntry(ref(), 'file');
    expect(state.remotePlace).toBe('p1');
    expect(state.remoteDir).toBe('tg:-100');
    expect(state.remoteTab).toBe('media');
    expect(state.remoteViewMode).toBe('files');
    expect(vi.mocked(api.remoteMessagesAround)).not.toHaveBeenCalled();
    expect(state.highlightFile).toBe('tg:-100:38');
  });

  it('旧引用没有记住分栏：按文件名兜底（a.mp3 -> audio）', async () => {
    vi.mocked(api.remoteBrowseTab).mockResolvedValue([
      { id: 'tg:-100:38', name: 'a.mp3', is_dir: false, media_tab: 'audio' },
    ]);
    await activateVirtualEntry(ref({ source_media_tab: null, name: 'a.mp3' }), 'file');
    expect(vi.mocked(api.remoteBrowseTab).mock.calls[0][2]).toBe('audio');
  });

  it('非 Telegram（WebDAV）引用：只有文件落点，进源目录（不要求 tg 消息号）', async () => {
    // 共用 beforeEach 默认位置是 telegram；这里显式把它换成 webdav，
    // reloadRemoteDir 才会走无分栏的 remoteBrowse（一次列全）。
    state.remotePlaces = [{ id: 'p1', kind: 'webdav' }];
    const wd = ref({ source_dir: '/docs', source_file: '/docs/a.pdf', source_media_tab: null, name: 'a.pdf' });
    // WebDAV 根/目录列表一次列全，remoteBrowse 给回该文件。
    vi.mocked(api.remoteBrowse).mockResolvedValue([
      { id: '/docs/a.pdf', name: 'a.pdf', is_dir: false },
    ]);
    await activateVirtualEntry(wd, 'file');
    expect(state.remotePlace).toBe('p1');
    expect(state.remoteDir).toBe('/docs');
    expect(state.remoteViewMode).toBe('files');
    expect(vi.mocked(api.remoteBrowseTab)).not.toHaveBeenCalled();
    expect(vi.mocked(api.remoteMessagesAround)).not.toHaveBeenCalled();
    expect(state.highlightFile).toBe('/docs/a.pdf');
  });
});


// ---------------------------------------------------------------------------
// 任务 #10：虚拟位置剪贴板（剪切/复制/粘贴）与删除
// ---------------------------------------------------------------------------

describe('虚拟位置剪贴板', () => {
  beforeEach(() => {
    Object.assign(state, {
      remotePlace: 'v1',
      remoteDir: '',
      virtualPlaces: [{ id: 'v1' }, { id: 'v2' }],
      remoteItems: [],
      virtualClipboard: null,
      placeError: '',
      remoteCacheStat: {},
    });
    vi.clearAllMocks();
    vi.mocked(api.remoteCacheFileStats).mockResolvedValue([]);
  });

  const refItem = (id: string, over: Record<string, unknown> = {}) => ({
    id, is_dir: false, is_ref: true, name: id + '.jpg',
    source_state: 'available', source_place: 'p1',
    source_dir: 'tg:-100', source_file: 'tg:-100:' + id,
    __folder: '', ...over,
  });

  it('复制：写入剪贴板，且 canPasteVirtual 只在虚拟位置为真', async () => {
    setVirtualClipboard('copy', [refItem('vr1')]);
    expect(state.virtualClipboard?.mode).toBe('copy');
    expect(canPasteVirtual()).toBe(true);
  });

  it('只接受虚拟引用：真实位置条目 / 文件夹不放不进剪贴板', () => {
    setVirtualClipboard('cut', [{ id: 'tg:x:1', is_dir: false, is_ref: false }]);
    expect(state.virtualClipboard).toBeNull();
    setVirtualClipboard('cut', [{ id: 'f1', is_dir: true, is_ref: false }]);
    expect(state.virtualClipboard).toBeNull();
  });

  it('同位置剪切粘贴：逐条 move，且剪切是一次性（贴完清空剪贴板）', async () => {
    state.remoteItems = [refItem('vr1', { __folder: 'sub' })];
    setVirtualClipboard('cut', [refItem('vr1', { __folder: 'sub' })]);
    vi.mocked(api.virtualMoveRef).mockResolvedValue('vr1');
    vi.mocked(api.virtualBrowse).mockResolvedValue([]);

    // 目标是当前根目录（''），源在 sub，move 被调用
    const n = await pasteVirtualHere();
    expect(n).toBe(1);
    expect(vi.mocked(api.virtualMoveRef)).toHaveBeenCalledWith('v1', 'vr1', '');
    expect(state.virtualClipboard).toBeNull();
  });

  it('跨位置复制：走 copy_refs（生成新引用），剪贴板保留可继续贴', async () => {
    setVirtualClipboard('copy', [refItem('vr1'), refItem('vr2')]);
    state.remotePlace = 'v2';
    vi.mocked(api.virtualCopyRefs).mockResolvedValue(2);
    vi.mocked(api.virtualBrowse).mockResolvedValue([]);

    const n = await pasteVirtualHere();
    expect(n).toBe(2);
    const req = vi.mocked(api.virtualCopyRefs).mock.calls[0][0];
    expect(req.sourcePlaceId).toBe('v1');
    expect(req.destPlaceId).toBe('v2');
    expect(req.refIds).toEqual(['vr1', 'vr2']);
    // 复制不删源、剪贴板保留
    expect(vi.mocked(api.virtualRemoveRef)).not.toHaveBeenCalled();
    expect(state.virtualClipboard?.mode).toBe('copy');
  });

  it('跨位置剪切：copy_refs 之后必须删除源引用，源位置不再保留', async () => {
    setVirtualClipboard('cut', [refItem('vr1')]);
    state.remotePlace = 'v2';
    vi.mocked(api.virtualCopyRefs).mockResolvedValue(1);
    vi.mocked(api.virtualRemoveRef).mockResolvedValue(true);
    vi.mocked(api.virtualBrowse).mockResolvedValue([]);

    const n = await pasteVirtualHere();
    expect(n).toBe(1);
    expect(vi.mocked(api.virtualCopyRefs)).toHaveBeenCalledTimes(1);
    // 关键回归：剪切要从源位置删除原引用（旧实现漏了这步，表现得像复制）。
    expect(vi.mocked(api.virtualRemoveRef)).toHaveBeenCalledWith('v1', 'vr1');
    expect(state.virtualClipboard).toBeNull();
  });

  it('多选：Ctrl 点选集合只收虚拟引用，批量操作取当前 remoteItems', () => {
    state.remoteItems = [
      refItem('vr1'), refItem('vr2'),
      { id: 'f1', is_dir: true, is_ref: false },
      { id: 'tg:1:2', is_dir: false, is_ref: false },
    ];
    toggleRemoteSelected('vr1', true);
    toggleRemoteSelected('vr2', true);
    expect(remoteSelectionActive()).toBe(true);
    expect(selectedRemoteEntries().map((f) => f.id)).toEqual(['vr1', 'vr2']);
    // 再点已选的取消
    toggleRemoteSelected('vr2', false);
    expect(selectedRemoteEntries().map((f) => f.id)).toEqual(['vr1']);
    clearRemoteSelection();
    expect(remoteSelectionActive()).toBe(false);
  });

  it('删除引用：只删 is_ref 的条目，删除后重载；不触碰后端移动接口', async () => {
    vi.mocked(api.virtualRemoveRef).mockResolvedValue(true);
    vi.mocked(api.virtualBrowse).mockResolvedValue([]);
    const n = await removeVirtualRefs([refItem('vr1'), { id: 'f1', is_dir: true }]);
    expect(n).toBe(1);
    expect(vi.mocked(api.virtualRemoveRef)).toHaveBeenCalledTimes(1);
    expect(vi.mocked(api.virtualRemoveRef)).toHaveBeenCalledWith('v1', 'vr1');
  });
});


// 加密虚拟位置时收集同密码 vault 材料：确认本地当前目录 + 每个远程位置的
// vault 都被传进 virtualEncrypt（解锁虚拟位置后才能跨 vault 自动解锁）。
describe('confirmVirtualEncrypt 收集 vault', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    state.cwd = 'E:/';
    state.virtualPlaces = [{ id: 'v1', name: 'test' }];
    state.remotePlaces = [{ id: 'p1' }, { id: 'p2' }];
    vi.mocked(api.virtualEncrypt).mockResolvedValue(true);
    vi.mocked(api.remotePlaceList).mockResolvedValue([]);
    vi.mocked(api.virtualPlaces).mockResolvedValue([]);
    vi.mocked(api.credentialCount).mockResolvedValue(1);
  });

  it('合并本地目录和所有远程位置的 vault 后传给 virtualEncrypt', async () => {
    const local = { salt: 'aa'.repeat(16), m_kib: 1, t: 1, p: 1 };
    const r1 = { salt: 'bb'.repeat(16), m_kib: 2, t: 2, p: 2 };
    const r2 = { salt: 'cc'.repeat(16), m_kib: 3, t: 3, p: 3 };
    vi.mocked(api.vaultParamsOf).mockResolvedValue([local]);
    vi.mocked(api.remotePlaceVaults)
      .mockResolvedValueOnce([r1])
      .mockResolvedValueOnce([r2]);

    promptVirtualEncrypt('v1');
    const ok = await confirmVirtualEncrypt({ password: '132' });
    expect(ok).toBe(true);

    // 为每个远程位置都探测过根目录 vault（漏掉某个位置，那个位置的同密码
    // 文件解锁虚拟位置后就不会自动解锁）
    expect(api.remotePlaceVaults).toHaveBeenCalledTimes(2);
    expect(vi.mocked(api.remotePlaceVaults)).toHaveBeenNthCalledWith(1, 'p1', '');
    expect(vi.mocked(api.remotePlaceVaults)).toHaveBeenNthCalledWith(2, 'p2', '');

    const req = vi.mocked(api.virtualEncrypt).mock.calls[0][0];
    expect(req).toMatchObject({ placeId: 'v1', password: '132' });
    const salts = req.vaults.map((v) => v.salt).sort();
    expect(salts).toEqual(['aa'.repeat(16), 'bb'.repeat(16), 'cc'.repeat(16)]);
  });

  it('本地/远程 vault 探测失败不阻断加密，仍带上已取到的部分', async () => {
    vi.mocked(api.vaultParamsOf).mockRejectedValue(new Error('no file'));
    vi.mocked(api.remotePlaceVaults)
      .mockResolvedValueOnce([{ salt: 'bb'.repeat(16), m_kib: 2, t: 2, p: 2 }])
      .mockRejectedValueOnce(new Error('net'));

    promptVirtualEncrypt('v1');
    const ok = await confirmVirtualEncrypt({ password: '132' });
    expect(ok).toBe(true);
    const req = vi.mocked(api.virtualEncrypt).mock.calls[0][0];
    expect(req.vaults.map((v) => v.salt)).toEqual(['bb'.repeat(16)]);
  });

  it('无密码或无目标时不发起加密', async () => {
    state.vEncryptFor = null;
    expect(await confirmVirtualEncrypt({ password: '132' })).toBe(false);
    expect(api.virtualEncrypt).not.toHaveBeenCalled();
  });
});
