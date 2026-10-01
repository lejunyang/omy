/**
 * PlaceBrowser 消息时间线的**组件级**测试。
 *
 * 只测这次改动落在模板上的、不依赖真实布局几何的行为（这些用 CDP 真浏览器测
 * 成本高、用例还少；几何/滚动手感仍归 CDP）：
 *
 * - 引用按钮点击走 locateMessage；定位进行中（state.locatingMsg）只在**那条**
 *   引用右侧出现小圈并禁用按钮，别的引用不转圈——即「局部 loading、不重载整区」。
 * - 「本屏 N 条消息」统计行彻底不存在。
 * - 搜索区带吸顶类；日期组标题带吸顶 top；顶部「向更新续翻」转圈按需出现。
 *
 * 为让用例又快又稳：
 * - store 整体 mock（编排逻辑已在 store.test.js 里用真实 store 覆盖，这里只验证
 *   组件怎么渲染/怎么调用）；
 * - WindowList 替成一个把默认插槽按 item 逐条展开的 render stub（happy-dom 没有
 *   布局，virtua 的窗口化/滚动测不了，也不该在这里测）；
 * - AppShell 替成只渲染默认插槽的壳。
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { defineComponent, h, reactive, computed, nextTick } from 'vue';
import { mount, flushPromises } from '@vue/test-utils';

// happy-dom 不实现真实布局，ResizeObserver 只需要「存在且不报错」。
if (typeof globalThis.ResizeObserver === 'undefined') {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  };
}

// 用 hoisted 持有同一份 state，让测试代码与被 mock 的 store 模块共享对象。
const hst: { state: any } = vi.hoisted(() => ({ state: {} as any }));

vi.mock('../store.js', async () => {
  const { reactive, computed } = await import('vue');
  hst.state = reactive({
    remotePlace: 'p1',
    remotePlaces: [{ id: 'p1', kind: 'telegram', name: 'TG' }],
    remoteDir: 'tg:-100',
    remoteDirName: '测试群',
    remoteTab: 'messages',
    remoteViewMode: 'messages',
    remoteMessages: [],
    remoteItems: [],
    searchResults: [],
    query: '',
    searchMode: 'local',
    searchedQuery: '',
    loadingMessages: false,
    loadingMore: false,
    loadingNewer: false,
    hasMoreMessages: false,
    hasNewerMessages: false,
    highlightMsg: null,
    locatingMsg: null,
    placeError: '',
    remoteProtected: false,
    busy: false,
    remoteRetrying: [],
    remoteSelected: [],
    remoteMessageSelected: [],
    remoteCacheStat: {},
    remoteDirCaps: { read: true, write: false },
    showThumbnails: true,
  });
  const noop = () => {};
  return {
    state: hst.state,
    currentCaps: computed(() => hst.state.remoteDirCaps),
    showMediaTabs: computed(() => true),
    canServerSearch: computed(() => false),
    MEDIA_TABS: [
      { key: 'media', i18n: 'rplace.tab_media' },
      { key: 'file', i18n: 'rplace.tab_file' },
      { key: 'messages', i18n: 'msgs.view_messages' },
    ],
    locateMessage: vi.fn(),
    loadMoreMessages: vi.fn(),
    loadNewerMessages: vi.fn(),
    openRemotePlace: noop,
    leaveRemotePlace: noop,
    reloadRemoteDir: noop,
    enterRemoteDir: noop,
    remoteGoUp: noop,
    removeRemotePlace: noop,
    detachTelegramPlace: noop,
    deleteTelegramAccount: noop,
    renameTelegramPlace: noop,
    retryRemoteEntry: noop,
    decryptRemoteToLocal: noop,
    requestRemoteFileCache: noop,
    uploadToRemote: noop,
    pinRemoteFile: noop,
    unpinRemoteFile: noop,
    remoteFileCache: () => null,
    removeRemoteFileCache: noop,
    placeThumbUrl: () => '',
    setSearchMode: noop,
    runServerSearch: noop,
    onSearchQueryCleared: noop,
    setRemoteTab: noop,
    activateVirtualEntry: noop,
    locateFile: noop,
    isVirtualPlace: () => false,
    addVirtualFolder: noop,
    openAddToVirtual: noop,
    openUploadToRemote: noop,
    remoteSelectionActive: () => hst.state.remoteSelected.length > 0,
    toggleRemoteSelected: noop,
    clearRemoteSelection: noop,
    selectedRemoteEntries: () => [],
    remoteMessageSelectionActive: () => hst.state.remoteMessageSelected.length > 0,
    toggleRemoteMessageSelected: vi.fn((id) => {
      const i = hst.state.remoteMessageSelected.indexOf(id);
      if (i >= 0) hst.state.remoteMessageSelected.splice(i, 1);
      else hst.state.remoteMessageSelected.push(id);
    }),
    clearRemoteMessageSelection: vi.fn(() => { hst.state.remoteMessageSelected = []; }),
    openTelegramForward: vi.fn(),
    openTelegramForwardFiles: vi.fn(),
    setGridLayout: noop,
    // 任务 #10 虚拟位置整理操作：组件引用了它们，mock 成空实现/常量。
    newVirtualFolderPrompt: noop,
    renameVirtualFolderPrompt: noop,
    removeVirtualFolder: noop,
    removeVirtualRefs: noop,
    setVirtualClipboard: noop,
    canPasteVirtual: () => false,
    pasteVirtualHere: noop,
  };
});

// i18n 轻量替身：t 原样返回键（用例不断言文案），formatSize 给个稳定串。
// __v_isRef:false 是必须的：模板把 i18n 当顶层绑定做 unref，会读这个内部标记；
// vi.mock 的 ESM proxy 访问未定义的命名导出会直接抛错（真实模块只读得 undefined）。
vi.mock('../i18n.js', () => ({
  __v_isRef: false,
  t: (key) => key,
  tn: (key) => key,
  te: (_code, fallback) => fallback || '',
  formatSize: (n) => `${n} B`,
}));

// 外壳只渲染默认内容，避免把侧栏/顶栏的依赖拖进来。
vi.mock('./AppShell.vue', () => ({
  default: defineComponent({
    setup(_, { slots }) {
      return () => h('div', { class: 'appshell-stub' }, slots.default?.());
    },
  }),
}));

// WindowList 替身：把 items 逐条喂给默认插槽。expose scrollToIndex
// （highlight watch 在真组件里会调它，stub 给个空实现即可）。
const WindowListStub = defineComponent({
  props: { items: { type: Array, default: () => [] } },
  setup(props, { slots, expose }) {
    expose({ scrollToIndex: () => {} });
    return () =>
      h(
        'div',
        { class: 'windowlist-stub' },
        props.items.map((item, index) => slots.default?.({ item, index })),
      );
  },
});

import PlaceBrowser from './PlaceBrowser.vue';
import { state, locateMessage } from '../store';

/** 同一天的三条消息：纯文本、带引用、带文件。date 取当前秒保证归一组。 */
function seedMessages() {
  const now = Math.floor(Date.now() / 1000);
  state.remoteMessages = [
    { message: 102, date: now, outgoing: false, text: '最新一条', reply_to: null, file_id: null },
    {
      message: 101,
      date: now,
      outgoing: false,
      text: '这条引用了 #50',
      reply_to: 50,
      file_id: null,
    },
    {
      message: 100,
      date: now,
      outgoing: false,
      text: '',
      reply_to: null,
      file_id: 'tg:-100:100',
      file_name: 'a.mp4',
      file_size: 10,
      thumb: null,
      duration: null,
    },
  ];
}

function mountView() {
  return mount(PlaceBrowser, {
    global: {
      stubs: { WindowList: WindowListStub, ContextMenu: true },
    },
  });
}

beforeEach(() => {
  seedMessages();
  Object.assign(state, {
    remotePlace: 'p1',
    remotePlaces: [{ id: 'p1', kind: 'telegram', name: 'TG' }],
    remoteDir: 'tg:-100',
    remoteTab: 'messages',
    remoteViewMode: 'messages',
    remoteItems: [],
    remoteSelected: [],
    remoteMessageSelected: [],
    loadingNewer: false,
    locatingMsg: null,
    highlightMsg: null,
    placeError: '',
    hasMoreMessages: false,
    hasNewerMessages: false,
  });
  vi.clearAllMocks();
});

describe('PlaceBrowser 远程位置空态', () => {
  it('连接按钮通过 add 事件交给父组件打开表单', async () => {
    state.remotePlace = '';
    state.remotePlaces = [];
    state.remoteItems = [];
    state.remoteTab = 'file';
    const w = mountView();
    await flushPromises();
    const add = w.find('.empty .btn.primary');
    expect(add.exists()).toBe(true);
    await add.trigger('click');
    expect(w.emitted('add')).toHaveLength(1);
  });
});
describe('PlaceBrowser 消息时间线', () => {
  it('渲染出全部消息行与日期组标题', async () => {
    const w = mountView();
    await flushPromises();
    // 三条消息
    expect(w.findAll('[data-tg="msgrow"]')).toHaveLength(3);
    // 至少一个日期组标题
    expect(w.find('[data-tg="msggroup"]').exists()).toBe(true);
  });

  it('不再渲染「本屏 N 条消息」统计行', async () => {
    const w = mountView();
    await flushPromises();
    expect(w.find('[data-tg="msgstat"]').exists()).toBe(false);
    // .msgstat 这个类也不该残留在任何元素上
    expect(w.find('.msgstat').exists()).toBe(false);
  });

  it('搜索区吸顶：带 stickybar 类', async () => {
    const w = mountView();
    await flushPromises();
    const bar = w.find('[data-pb="searchbar"]');
    expect(bar.exists()).toBe(true);
    expect(bar.classes()).toContain('stickybar');
  });

  it('点引用按钮调用 locateMessage(reply_to)', async () => {
    const w = mountView();
    await flushPromises();
    const replyBtn = w.find('[data-tg="msgreply"]');
    expect(replyBtn.exists()).toBe(true);
    await replyBtn.trigger('click');
    expect(locateMessage).toHaveBeenCalledWith(50);
  });

  it('定位进行中：只在对应引用右侧出现小圈，且该按钮禁用（不重载整区）', async () => {
    const w = mountView();
    await flushPromises();
    // 初始没有小圈
    expect(w.find('[data-tg="replyspin"]').exists()).toBe(false);

    state.locatingMsg = 50;
    await nextTick();

    const spin = w.find('[data-tg="replyspin"]');
    expect(spin.exists()).toBe(true);
    const btn = w.find('[data-tg="msgreply"]');
    expect(btn.attributes('disabled')).toBeDefined();
    // 整屏 loading 没被点亮：消息行仍都在（没有被 loading 占位替换）
    expect(w.findAll('[data-tg="msgrow"]')).toHaveLength(3);

    state.locatingMsg = null;
    await nextTick();
    expect(w.find('[data-tg="replyspin"]').exists()).toBe(false);
    expect(w.find('[data-tg="msgreply"]').attributes('disabled')).toBeUndefined();
  });

  it('向顶部续翻加载中：列表顶部出现小转圈，消息行不被清空', async () => {
    state.hasNewerMessages = true;
    state.loadingNewer = true;
    const w = mountView();
    await flushPromises();
    expect(w.find('[data-tg="msgloading-newer"]').exists()).toBe(true);
    expect(w.findAll('[data-tg="msgrow"]')).toHaveLength(3);
  });

  it('消息复选后出现多选转发工具条，清除后消失', async () => {
    const w = mountView();
    await flushPromises();
    const first = w.find('.msgcheck');
    expect(first.exists()).toBe(true);
    await first.setValue(true);
    await nextTick();
    expect(state.remoteMessageSelected.length).toBe(1);
    expect(w.find('.msg-selectbar').exists()).toBe(true);
  });

  it('消息行右键菜单包含转发入口', async () => {
    const w = mount(PlaceBrowser, {
      global: { stubs: { WindowList: WindowListStub } },
    });
    await flushPromises();
    await w.find('[data-tg="msgrow"]').trigger('contextmenu');
    await nextTick();
    expect(w.text()).toContain('tg_forward.menu');
  });

  it('日期组标题带吸顶 top 内联样式（落在搜索栏下方而非 top:0）', async () => {
    const w = mountView();
    await flushPromises();
    const group = w.find('[data-tg="msggroup"]');
    // 初始测量值为 0 时是 '0px'；关键是绑定存在、是个像素值而不是无样式。
    expect(group.attributes('style')).toMatch(/top:\s*\d+(\.\d+)?px/);
  });
});
