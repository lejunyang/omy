/** 远程位置的能力表、消息时间线、定位跳转与服务端搜索。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import { computed, nextTick } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import type { MessageRow, MessageWindow } from '../types';
import { state } from './state';
import { isTelegramDialog, refreshCacheStats, reloadRemoteDir, remoteDirKey } from './remote-browse';
import { alignToGrid, viewportFillCount } from './grid';
import { remoteDirCache } from './remote-files';

/* ---------------- 远程位置 ---------------- */

/** 本地位置的全能力。
 *
 * 这几份常量的字段必须与后端 `omy_remote::Capabilities` 逐一对上（含
 * `search`）：少写一个字段，读到的就是 `undefined`，而 `!caps.write` 为真会让
 * 只读位置反而全部可写。后端 `caps.rs` 的 `field_names_are_stable` 就是为拦这
 * 件事存在的——**后端加字段时这里三处都要跟着加**。
 */
const LOCAL_CAPS = {
  read: true,
  write: true,
  delete: true,
  rename: true,
  create_dir: true,
  random_write: true,
  range_read: true,
  // 本地搜索是对已列出条目的前端过滤，没有「下推到服务端」这回事
  search: false,
};

/** 状态不明时用的保守能力：能读，什么都不能写。 */
const SAFE_READONLY_CAPS = {
  read: true,
  write: false,
  delete: false,
  rename: false,
  create_dir: false,
  random_write: false,
  range_read: true,
  search: false,
};

/** 当前所在位置 + 目录的**有效**能力。界面一律读这一个。
 *
 * 不在远程位置时返回本地全能力——这样上层判断可以统一写 `caps.write`，
 * 不必到处分叉「是不是远程」。
 *
 * 在远程位置但还没查到目录能力时返回**保守只读**，而不是回落到位置级上界：
 * 回落等于把「最多能做什么」当成「现在能做什么」用，那正是这层收窄要消除的
 * 缺陷——用户会在一个只读目录里看到亮着的删除按钮。少几个按钮只是等一下就
 * 补上，点了报错却已经发出去了。
 */
export const currentCaps = computed(() => {
  if (!state.remotePlace) return LOCAL_CAPS;
  return state.remoteDirCaps || SAFE_READONLY_CAPS;
});

/** 切到消息视图并加载。
 *
 * 只在对话内可用：位置列表那一层没有「消息」这个概念。
 */
/** 消息视图缓存：`位置\x00目录` -> 消息行数组。
 *  二次进入某对话的消息栏时先出缓存、再后台刷新，别每次全量重拉。 */
const remoteMsgCache = new Map<string, MessageRow[]>();

/** 加载消息栏首屏（带缓存优先）。
 *
 * 先出缓存（若有），再拉服务端刷新。缓存/刷新都写回 remoteMsgCache。
 * 与文件栏的 reloadRemoteDir 同一种「缓存先出 + 后台刷新」节奏。 */
export async function loadMessagesFirstPage(): Promise<void> {
  if (!state.remotePlace || !state.remoteDir) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  const key = remoteDirKey(place, dir);
  state.placeError = '';
  // 缓存优先：有上次的就先摆出来，避免空屏
  const cached = remoteMsgCache.get(key);
  if (cached && cached.length) {
    state.remoteMessages = cached;
    state.loadingMessages = false;
  } else {
    state.remoteMessages = [];
    state.loadingMessages = true;
  }
  try {
    const rows = await api.remoteMessages(place, dir);
    // 回写前校验没切走对话/栏
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    if (state.remoteTab !== 'messages') return;
    state.remoteMessages = rows;
    remoteMsgCache.set(key, rows);
    // 取满一页就假定还有更早的。少于一页说明到头了——不靠「下一次返回空」
    state.hasMoreMessages = rows.length >= 25;
    // 首屏取的就是最新消息，上面不可能再有更新的；引用跳转残留的向新开关在这里
    // 必须关掉，否则到顶会触发一个必空的向新续翻。
    state.hasNewerMessages = false;
  } catch (e) {
    // 有缓存就保留，别让一次网络抖动把已看到的消息抹掉
    if (!cached || !cached.length) state.remoteMessages = [];
    state.placeError = i18n.te(api.errCode(e), i18n.te('remote_failed'));
  } finally {
    state.loadingMessages = false;
  }
}

/** 从 Telegram 文件/消息条目 id（`tg:<chat>:<msg>`）解析消息号；非法形状给 null。
 *  与视图层 msgIdOf 同一判据，但状态层也要用（虚拟引用「定位到源消息」）。 */
export function tgMsgOfId(id: string | null | undefined): number | null {
  if (typeof id !== 'string') return null;
  const parts = id.split(':');
  if (parts.length !== 3 || parts[0] !== 'tg') return null;
  const n = Number(parts[2]);
  return Number.isInteger(n) ? n : null;
}

/** 没有可信的 media_tab 时（旧引用、外部传入只有文件名），按文件后缀粗分到
 *  与服务端一致的文件分栏。判据刻意保守：只把很确定的类型归到 audio/gif，
 *  图片/视频归 media，其余一律 file——分错一栏比落进 file 更糟（会看不到目标）。 */
export function fileTabForName(name: string | null | undefined): string {
  const ext = String(name || '').toLowerCase().split('.').pop() || '';
  if (['mp3', 'flac', 'm4a', 'ogg', 'wav', 'aac'].includes(ext)) return 'audio';
  // 普通动图多以 .gif 发送（Telegram 的 GIF 实际是静音 mp4，但文件名仍常是 .gif）
  if (['gif', 'webp'].includes(ext)) return 'gif';
  if (['jpg', 'jpeg', 'png', 'heic', 'heif', 'mp4', 'mov', 'mkv', 'avi', 'webm'].includes(ext)) {
    return 'media';
  }
  return 'file';
}

/** 把一个文件定位到文件网格里并高亮。
 *
 * 与 locateMessage 对称的「文件版」：切到该文件所在的分栏（照片视频/文档/链接/
 * 音乐/GIF），确保它在已加载列表里（不在就以其消息号为游标拉一页并合并），然后
 * 置 highlightFile 由视图层滚动 + 脉冲高亮。
 *
 * target：
 * - id   目标条目 id（Telegram 为 tg:<chat>:<msg>）；
 * - tab  已知分栏优先（来自后端 media_tab / 虚拟引用快照），为 null 时按名字兜底；
 * - name 文件名（兜底分栏与找不到时提示用）。
 *
 * 非 Telegram 目录（WebDAV 等）没有分栏、一次列全：只刷新当前目录，再尝试高亮；
 * 列表里确实没有（比如不在当前目录）则给未找到提示，不假装成功。 */
export async function locateFile(target: {
  id: string;
  tab?: string | null;
  name?: string | null;
  // 显式告知源是不是 Telegram：刚 openRemotePlace(skipReload) 完，state.remotePlaces
  // 未必刷新过，靠 isTelegramDialog() 推断可能误判成非 Telegram。
  isTelegram?: boolean;
}): Promise<void> {
  const { id } = target;
  if (!state.remotePlace || !state.remoteDir || !id) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  const inDialog = target.isTelegram ?? isTelegramDialog();
  // 目标分栏：后端/快照给的最准；缺了才按文件名猜。
  const tab = target.tab || fileTabForName(target.name);

  // 切文件视图。进来时可能还停在消息栏，或刚 skipReload（remoteItems 为空）：
  //  - Telegram 对话：切到目标分栏并刷该栏（同栏也刷，因为 skipReload 没拉过）；
  //  - 非 Telegram（WebDAV 等）：没有分栏，刷一次当前目录（一次列全）。
  state.remoteViewMode = 'files';
  if (inDialog) {
    // 记下「是否换了栏」：换栏必须 reload（reloadRemoteDir 会清掉旧栏的条目再拉
    // 新栏首屏，否则旧栏卡片会残留在网格里）。不能在赋值后再比 state.remoteTab——
    // 那时永远相等，条件会退化成「只在列表空时刷」，从消息栏跨栏定位就会带着一屏
    // 别的栏的卡片。
    let tabChanged = false;
    if (state.remoteTab !== tab) {
      state.remoteTab = tab;
      tabChanged = true;
      api.uiLog('switch-tab', tab);
    }
    if (tabChanged || state.remoteItems.length === 0) {
      await reloadRemoteDir();
    }
  } else {
    // 非 Telegram 没有分栏概念；刚 skipReload 进来（列表空）时刷一次当前目录，
    // WebDAV 一次列全。列表已有内容就不重拉。
    if (state.remoteItems.length === 0) {
      await reloadRemoteDir();
    }
  }
  // 切位置/目录/分栏会让上面的 await 之后身份变化；回写前校验仍在目标。
  const stillTarget = () => state.remotePlace === place && state.remoteDir === dir;

  // 确保目标在 remoteItems 里。
  let found = state.remoteItems.some((x) => x.id === id);
  if (!found && inDialog) {
    const n = tgMsgOfId(id);
    // 以目标消息号为游标拉一页（before=n+1 让目标落在这一页里）。只尝试一次：
    // 找不到多半是目标不属于这个分栏或已删，不做循环续翻（避免无限请求）。
    if (n != null) {
      const lim = alignToGrid(viewportFillCount());
      try {
        const rows = await api.remoteBrowseTab(place, dir, tab, n + 1, lim);
        if (stillTarget() && state.remoteTab === tab) {
          const seen = new Set(state.remoteItems.map((x) => x.id));
          const fresh = (rows || []).filter((x) => !seen.has(x.id));
          if (fresh.length) {
            state.remoteItems = [...state.remoteItems, ...fresh];
            remoteDirCache.set(remoteDirKey(place, dir, tab), state.remoteItems);
            void refreshCacheStats(place, fresh);
          }
          found = state.remoteItems.some((x) => x.id === id);
          // 这一页只是定位锚点，不改变「是否还有更多」的大方向判据。
        }
      } catch (e) {
        state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
      }
    }
  }
  // 非 Telegram：reloadRemoteDir 已一次列全，仍没有就真的不在当前目录。

  if (!stillTarget()) return;
  if (!found) {
    state.placeError = i18n.t('msgs.locate_file_not_found');
    state.highlightFile = null;
  state.remoteSelected = [];
    return;
  }
  // 先清再置，保证即便与上一次目标相同也能再次触发视图的高亮 watch。
  state.highlightFile = null;
  state.remoteSelected = [];
  await nextTick();
  state.highlightFile = id;
}

/** 定位到某条消息（引用跳转 / 媒体「定位源消息」）。
 *
 * 两种入口，两种加载粒度：
 * - 已经在消息时间线上（点消息里的「回复 #N」）：**不重拉整屏**，只把 locatingMsg
 *   置成目标号（视图在引用按钮右侧转一个小圈）。目标已在当前列表里就直接高亮
 *   滚动；不在再用 remoteMessagesAround 拉它前后一段，并**合并**进现有列表而不是
 *   替换——这样跳转前后的消息都还在，往回滚不会「很快到顶」。
 * - 从文件栏 / 外部（媒体条目「定位源消息」、虚拟引用）进来：时间线还没加载，
 *   才置 loadingMessages 整屏拉一段。
 *
 * 对齐 Telegram 官方：跳转是「以目标消息为锚点开一个窗口、双向都能继续翻」，
 * 而不是把整个内容区清空重载成一个方向不明的新列表。
 *
 * around 是目标消息号（来自媒体条目 id 的 (对话,消息)，或消息 reply_to）。 */
export async function locateMessage(around: number): Promise<void> {
  if (!state.remotePlace || !state.remoteDir || !around) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  // 已经在消息时间线（含已有数据）：内联跳转，不闪整屏加载。
  const inline = state.remoteViewMode === 'messages' && state.remoteMessages.length > 0;
  state.remoteTab = 'messages';
  state.remoteViewMode = 'messages';
  state.placeError = '';
  if (!inline) state.loadingMessages = true;
  state.locatingMsg = around;
  api.uiLog('locate-msg');
  try {
    // 目标已在当前列表：不发请求，直接交给视图层高亮滚动（小圈也立刻消失）。
    const present = state.remoteMessages.some((m) => m.message === around);
    let win: MessageWindow | null = null;
    if (!present) {
      win = await api.remoteMessagesAround(place, dir, around);
      if (state.remotePlace !== place || state.remoteDir !== dir) return;
    }
    if (win) {
      const incoming = win.rows || [];
      if (inline) {
        // 合并而非替换：跳转点之前（更新）与之后（更旧）的已加载消息都保留，
        // 去重后统一按新→旧排。这样往顶部回滚能看到跳转前的新消息，不会
        // 「滑一点就到顶」。
        const seen = new Set();
        const merged = [];
        for (const m of [...incoming, ...state.remoteMessages]) {
          if (seen.has(m.message)) continue;
          seen.add(m.message);
          merged.push(m);
        }
        merged.sort((a, b) => b.message - a.message);
        state.remoteMessages = merged;
      } else {
        state.remoteMessages = incoming;
      }
      remoteMsgCache.set(remoteDirKey(place, dir), state.remoteMessages);
      // 双向续翻开关以服务端返回的 has_older/has_newer 为准；老形状兜底：
      // 有数据就先允许向旧翻。
      state.hasMoreMessages =
        typeof win.has_older === 'boolean'
          ? win.has_older
          : incoming.length > 0;
      state.hasNewerMessages =
        typeof win.has_newer === 'boolean'
          ? win.has_newer
          : false;
      if (!win.found) {
        // 目标已删或超出可取范围：不假装定位成功（仍保留拉回的上下文）。
        state.highlightMsg = null;
  state.highlightFile = null;
  state.remoteSelected = [];
        state.placeError = i18n.t('msgs.locate_not_found');
        return;
      }
    }
    // 命中（本来就在列表，或新窗口里找到）：置高亮号，由 PlaceBrowser 的
    // watch 滚到它并做脉冲高亮。
    state.highlightMsg = around;
  } catch (e) {
    // 内联失败时保留现有列表，不用错误态把用户正在看的时间线盖掉
    if (!inline) state.remoteMessages = [];
    state.placeError = i18n.te(api.errCode(e), i18n.te('remote_failed'));
  } finally {
    state.loadingMessages = false;
    state.locatingMsg = null;
  }
}

/** 往**更新**方向续翻一页（引用跳到历史中间后，列表顶部继续拉回更新的消息）。
 *
 * 与 loadMoreMessages（向更旧、追加到末尾）对称：取当前最新一条之后的一页，
 * 合并去重到列表**头部**。视图层会在请求前后做滚动位置锚定，视觉上原地不动。
 */
export async function loadNewerMessages(): Promise<void> {
  if (state.loadingNewer || !state.hasNewerMessages) return;
  if (!state.remotePlace || !state.remoteDir) return;
  const newest = state.remoteMessages[0];
  if (!newest) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  state.loadingNewer = true;
  try {
    const rows = await api.remoteMessagesAfter(place, dir, newest.message);
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    const seen = new Set(state.remoteMessages.map((m) => m.message));
    const fresh = (rows || []).filter((m) => !seen.has(m.message));
    if (fresh.length) {
      // 新页是新→旧、且都比当前段新；整体放在头部即可，组按新→旧排不用再排。
      state.remoteMessages = [...fresh, ...state.remoteMessages];
      remoteMsgCache.set(remoteDirKey(place, dir), state.remoteMessages);
    }
    // 取不满一页说明已经到对话最新。
    state.hasNewerMessages = (rows || []).length >= 25 && fresh.length > 0;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.loadingNewer = false;
  }
}

/** 兼容保留：旧调用点（若有）切文件/消息视图。现在消息是第六个分栏，
 *  统一走 setRemoteTab；这里仅转发，避免遗漏的调用点报错。 */
export async function setRemoteViewMode(mode) {
  if (mode === 'messages') {
    await setRemoteTab('messages');
  } else {
    state.remoteViewMode = 'files';
    state.placeError = '';
    if (state.remoteTab === 'messages') await setRemoteTab('media');
  }
}

/** 切换对话内的分栏：媒体/文件/链接/音频/GIF/消息。
 *
 * 消息栏走 getHistory 时间线（loadMessagesFirstPage）；其余五栏走服务端
 * 类型 filter（reloadRemoteDir）。两条加载路径不同，切栏时正确分发。 */
export async function setRemoteTab(tab) {
  if (state.remoteTab === tab) return;
  state.remoteTab = tab;
  state.remoteSelected = [];
  state.remoteMessageSelected = [];
  state.telegramForward = null;
  api.uiLog('switch-tab', tab);
  if (tab === 'messages') {
    // remoteViewMode 仍用 'messages' 驱动模板渲染消息时间线
    state.remoteViewMode = 'messages';
    await loadMessagesFirstPage();
  } else {
    state.remoteViewMode = 'files';
    await reloadRemoteDir();
  }
}

/** 对话内文件视图的媒体分栏定义。key 与后端 MediaTab::from_key 对应。 */
export const MEDIA_TABS = [
  { key: 'media', i18n: 'rplace.tab_media' },
  { key: 'file', i18n: 'rplace.tab_file' },
  { key: 'link', i18n: 'rplace.tab_link' },
  { key: 'audio', i18n: 'rplace.tab_audio' },
  { key: 'gif', i18n: 'rplace.tab_gif' },
  // 消息作为第六个分栏（末位）：原来是「文件/消息」两 tab 那层单独切换，
  // 合并进这一排后交互更一致。消息栏走 getHistory（时间线），其余五栏走
  // 服务端类型 filter。
  { key: 'messages', i18n: 'msgs.view_messages' },
];

/** 分栏 tab 该不该显示：Telegram 对话内、文件视图下才出现。 */
export const showMediaTabs = computed(
  () => !!state.remotePlace && !!state.remoteDir
    && (state.remotePlaces.find((p) => p.id === state.remotePlace)?.kind === 'telegram'),
);

/** 消息视图里能不能用。
 *
 * 只有 Telegram 有「消息」这个概念，而且要已经进到某个对话里。
 * WebDAV 上不显示这个切换——不是置灰，是整个不出现（与搜索那个分段控件同理）。
 */
export const canShowMessages = computed(
  () => !!state.remotePlace && !!state.remoteDir
    && (state.remotePlaces.find((p) => p.id === state.remotePlace)?.kind === 'telegram'),
);

/** 远程视图里该显示哪些条目。
 *
 * 远程网格以前直接渲染 `state.remoteItems`，于是顶栏那个搜索框**对远程位置
 * 完全无效**——输入什么都不会过滤。本地视图有 `visibleEntries` 做过滤，
 * 远程没有对应物，这个缺口从界面上看不出来（搜索框照常能输入）。
 *
 * 两种语义都落在这里：
 * - 本地过滤：在已加载的这一屏里按**解出来的真实文件名**匹配，搜索词不出本机；
 * - 服务端搜索：列服务端给的候选集，再用同一套本地规则精筛一遍。
 */
export const remoteVisible = computed(() => {
  const server = state.searchMode === 'server';
  const source = server ? state.searchResults : state.remoteItems;
  const q = state.query.trim().toLowerCase();
  if (!q) return source;
  return source.filter((e) => {
    // 锁定的加密文件没有可搜的名字：用磁盘上的密文名去匹配等于拿乱码当明文搜，
    // 既搜不到也会泄露信息。解锁对应密码后它会自动出现在结果里
    if (e.is_encrypted && !e.unlocked) return false;
    const name = e.real_name || e.name;
    return name.toLowerCase().includes(q);
  });
});

/** 被本地精筛挡掉的加密文件数量。
 *
 * 界面要如实说明「还有 N 个加密文件因为没解锁而没参与搜索」——不说的话
 * 用户会以为那些文件不存在，而其实只是差一个密码。
 */
export const searchLockedOut = computed(() => {
  const q = state.query.trim();
  if (!q) return 0;
  const source = state.searchMode === 'server' ? state.searchResults : state.remoteItems;
  return source.filter((e) => e.is_encrypted && !e.unlocked).length;
});

/** 当前位置支不支持服务端搜索。
 *
 * 分段控件据此决定**出不出现**（而不是置不置灰）：本地目录和网盘根本没有
 * 「搜索整个对话」这个概念，摆一个灰按钮只会让人以为是暂时不可用。
 */
export const canServerSearch = computed(
  () => !!state.remotePlace && !!currentCaps.value.search,
);

/** 发起一次服务端搜索。
 *
 * 只有用户显式切到「搜索整个对话」时才会走到这里——这是把搜索词发出去的
 * 唯一路径，保持它唯一才好确认「发出去」和「告诉用户发出去了」是同步的。
 */
/** 搜索请求的序号。
 *
 * 用于丢弃**已经作废的那一次**的结果。实测过一条竞态：切到「搜索整个
 * 对话」会自动发起一次搜索，用户在请求飞行途中清空了输入框，清空逻辑
 * 已经把提示条撤掉，可请求随后回来又把搜索词写了回去——警告条于是挂着
 * 一个用户已经删掉的词。
 *
 * 与 `refreshRemoteDirCaps` 回写前校验位置与目录是同一种做法。 */
let searchSeq = 0;

export async function runServerSearch() {
  const q = state.query.trim();
  searchSeq += 1;
  const seq = searchSeq;
  if (!state.remotePlace || !q) {
    state.searchResults = [];
    state.searchedQuery = '';
    return;
  }
  state.searching = true;
  state.placeError = '';
  try {
    const rows = await api.remoteSearch(state.remotePlace, state.remoteDir, q);
    // 结果回来时若已经不是最新那一次，整份丢弃：用户可能已经清空输入框
    // 或又搜了别的词，写回去就是把界面拉回一个他已经离开的状态
    if (seq !== searchSeq) return;
    state.searchResults = rows;
    // 记下真正搜出去的那个词，而不是读 state.query——用户可能在请求飞行途中
    // 又改了输入框，那样提示条会显示一个其实没发出去的词
    state.searchedQuery = q;
  } catch (e) {
    // 作废的请求失败了不该清当前状态，更不该弹一条属于旧请求的错误
    if (seq !== searchSeq) return;
    state.searchResults = [];
    state.searchedQuery = '';
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  } finally {
    if (seq === searchSeq) state.searching = false;
  }
}

/** 输入框清空时，把「已经搜出去的那个词」一并作废。
 *
 * 不这样会怎样：实测过——输入 omy、服务端搜一次、再清空输入框，
 * 那条「搜索词「omy」已发送到 Telegram 服务器」的警告仍挂着。
 * 这条提示的作用是告诉用户**哪个词离开了本机**，停在一个他已经删掉的
 * 词上，会让人以为清空动作又发了一次请求。隐私提示给错状态比不给更糟。
 *
 * 不在渲染条件里加 `state.query`：那样刚敲第一个字符警告就没了，
 * 而那一刻上一次搜索的结果还在屏幕上摆着。 */
export function onSearchQueryCleared(ev) {
  // 读事件里的 DOM 值，不读 state.query：同一个元素上 v-model 与 @input
  // 谁先跑不由我们决定，读响应式变量可能拿到上一轮的旧值。实测正是如此
  // ——直接清空不生效，先敲一个字符再清空却生效，差别只在这个时序上。
  // ev.target.value 在 input 触发时一定已是新值。
  const v = ev?.target?.value ?? state.query;
  if (String(v).trim()) return;
  // 让飞行中的那次搜索作废，否则它回来会把刚撤掉的提示条又挂回去
  searchSeq += 1;
  state.searchResults = [];
  state.searchedQuery = '';
}

/** 切换搜索模式。 */
export async function setSearchMode(mode) {
  state.searchMode = mode;
  if (mode === 'server') {
    // 只有已经有搜索词才真去服务端搜。空词时若无条件 runServerSearch，
    // 服务端返回空集，而 server 模式下列表 source 用的是 searchResults，
    // 于是「切一下模式」就把整屏内容清空了——用户切模式并不等于想搜空。
    // 空词时只切模式标记、保留当前列表，等用户真输入词再搜。
    if (state.query.trim()) {
      await runServerSearch();
    }
  } else {
    // 切回本地时清掉服务端结果：留着会让用户以为本地过滤也能搜到那些
    state.searchResults = [];
    state.searchedQuery = '';
  }
}
