/** 远程目录的进入、加载、分页、缓存统计与目录快照。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import * as api from '../api';
import * as i18n from '../i18n';
import { setNotice, state } from './state';
import { locateFile, locateMessage, tgMsgOfId } from './remote-view';
import { isVirtualPlace, promptVirtualUnlock } from './virtual';
import { alignToGrid, viewportFillCount } from './grid';
import { applyBufferedRemoteEntries, remoteDirCache, remoteFileCacheKeyFor } from './remote-files';
import { promptTgUnlock } from './places';

/** 进入一个远程位置的根目录。 */
/** 单调递增的「浏览代号」。
 *
 * 每次 reloadRemoteDir 入口 +1（含不触网的虚拟浏览）。openRemotePlace 切位置、
 * enterRemoteDir 进目录、setRemoteTab 换栏都会触发它，于是「A 加载中点 B」时
 * A 捕获的代号必然落后、回写被丢弃——与搜索 searchSeq 同一模式。 */
let browseSeq = 0;

export async function openRemotePlace(id, opts?: { skipReload?: boolean }) {
  // 上报"切到某个远程位置/账号"（记 redact 后的短标识，不记位置名）
  api.uiLog('open-place', id ? `#${id}` : '');
  // 无论接下来刷不刷文件栏，先 +1 作废上一个位置/目录所有在飞的浏览回写，
  // 否则上一屏晚到的 browse 结果会把这个位置的内容覆盖掉（浏览竞态）。
  browseSeq += 1;
  state.remotePlace = id;
  state.remoteDir = '';
  // 切账号先清掉上一个账号的列表：不清的话，切到一个**从没加载过**的账号时
  // 会先显示上一个账号的内容 + 顶部小刷新图标，而不是「占满区域的大加载圈」。
  // reloadRemoteDir 随后若命中缓存会立刻把内容摆回来，所以有缓存的账号仍是
  // 「先出缓存」，不受影响——三态就此区分开。
  state.remoteItems = [];
  state.remoteSelected = [];
  state.remoteMessageSelected = [];
  state.telegramForward = null;
  // skipReload：调用方紧接着要直接进某个对话的消息时间线（虚拟引用「定位到
  // 真实位置」），此时先拉根目录对话列表纯属浪费一次请求，还会让界面先在文件
  // 视图闪一下根列表再跳消息 tab。由调用方负责随后加载目标视图。
  if (opts?.skipReload) return;
  await reloadRemoteDir();
}

/** 离开远程位置，回到本地浏览。 */
export function leaveRemotePlace() {
  // 暂存区跟着清：它只服务「当前这一屏」，留着既占内存，
  // 也可能在下次进同一目录时贴上已经过时的识别结果
  remoteEntryBuffer.clear();
  state.remotePlace = '';
  state.remoteDir = '';
  state.remoteItems = [];
  state.hasMoreFiles = false;
  state.remoteRetrying = [];
  // 能力必须跟着清：留着会让下次进另一个位置时先按上一个位置的能力渲染一帧
  state.remoteDirCaps = null;
  state.remoteProtected = false;
  // 搜索态也要清：回到本地后还留着「服务端搜索」模式的话，界面会显示一个
  // 本地位置根本不支持的模式，而那批结果也已经不属于眼前这个位置了
  state.searchMode = 'local';
  state.searchResults = [];
  state.searchedQuery = '';
  // 消息视图也要复位：留着会让下次进另一个对话时先显示上一个对话的消息
  state.remoteViewMode = 'files';
  state.remoteMessages = [];
  state.hasMoreMessages = false;
  state.hasNewerMessages = false;
  state.loadingMore = false;
  state.loadingNewer = false;
  state.locatingMsg = null;
  state.highlightMsg = null;
  state.highlightFile = null;
  state.remoteSelected = [];
  state.remoteMessageSelected = [];
  state.telegramForward = null;
  state.remoteDirName = '';
}

/** 列出当前虚拟位置的当前文件夹：把文件夹 + 引用映射成 remoteItems 条目。
 *
 * 引用带 source_state（三态）：源可用正常、源不可用灰显、源加密未解锁提示解锁。
 * 引用条目 is_dir=false 但不是本地文件——它的 id 是 ref_id，双击走
 * activateVirtualEntry（定位到真实位置），不走普通识别。 */
export async function reloadVirtualDir() {
  const place = state.remotePlace;
  const folder = state.remoteDir;
  // 捕获本次浏览代号（reloadRemoteDir 已先 +1），回写前比对，防切走后覆盖。
  const seq = browseSeq;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  // 虚拟位置只有「文件」没有消息时间线。从「定位到真实位置的消息视图」再点回
  // 虚拟位置时若不复位，会停在上一个真实对话的消息视图——表现为点了虚拟位置
  // 「没反应」（其实是没切回文件网格）。tab 一并复位到默认媒体栏。
  state.remoteViewMode = 'files';
  state.remoteTab = 'media';
  state.remoteMessages = [];
  state.hasMoreMessages = false;
  state.hasNewerMessages = false;
  state.loadingMore = false;
  state.loadingNewer = false;
  state.locatingMsg = null;
  state.highlightMsg = null;
  state.highlightFile = null;
  state.remoteSelected = [];
  try {
    const rows = await api.virtualBrowse(place, folder);
    if (seq !== browseSeq || state.remotePlace !== place
        || state.remoteDir !== folder) return;
    // 映射成 PlaceBrowser 认识的条目形状。文件夹 is_dir=true 用文件夹图标；
    // 引用把源状态挂在条目上，视图据此显示灰显/锁标识。
    state.remoteItems = rows.map((e) => ({
      id: e.id,
      name: e.name,
      is_dir: e.is_dir,
      size: e.size ?? null,
      mtime: null,
      is_encrypted: false,
      unlocked: false,
      real_name: null,
      plaintext_size: null,
      probe_failed: false,
      // 后端对源可用的引用复用真实位置已落盘的清晰缩略图磁盘缓存（零网络），
      // 读不到就是 null，前端回退类型图标。
      thumb_token: e.thumb_token ?? null,
      probing: false,
      // 虚拟引用专属字段
      is_ref: !e.is_dir,
      source_state: e.source_state ?? null,
      source_place: e.source_place ?? null,
      source_dir: e.source_dir ?? null,
      source_file: e.source_file ?? null,
      // 添加引用时记住的源 Telegram 分栏（可能缺：旧引用），定位源文件时优先用它
      source_media_tab: e.source_media_tab ?? null,
      // 记录该条目当前所在文件夹 id，剪切粘贴到同目录时跳过它自身
      __folder: folder,
    }));
    state.hasMoreFiles = false;
    // 缓存角标：引用的缓存落在**真实位置**名下（委托读取时缓存 key 是源
    // place + 源 file_id，与在真实位置直接打开同一文件完全相同），不能用
    // 虚拟位置自己的 id 去查——那必然查不到、角标永远不亮。
    void refreshVirtualCacheStats(rows);
  } catch (e) {
    if (seq !== browseSeq) {
      // 旧请求，什么都不改
    } else if (api.errCode(e) === 'virtual_locked') {
      // 加密且未解锁：不显示空列表（那会被误以为收藏没了），弹解锁框。
      state.remoteItems = [];
      promptVirtualUnlock(place);
    } else {
      state.remoteItems = [];
      state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
    }
  } finally {
    if (seq === browseSeq) {
      state.busy = false;
      state.busyKey = '';
    }
  }
}

/** 打开/定位一个虚拟引用条目：跳到它指向的真实位置。
 *
 * mode：
 * - 'msg'：进源对话的**消息时间线**并高亮那条消息；
 * - 'file'：进源对话/目录的**文件网格**并把源文件滚出来高亮。
 * 组件按右键菜单传入；缺省 'file'（等同旧「定位到真实位置」）。
 *
 * 源不可用（Missing）提示先把那个远程加回来；源加密未解锁（Locked）提示先解锁。
 * 委托真实位置读取，缓存天然共享。 */
export async function activateVirtualEntry(entry, mode: 'file' | 'msg' = 'file') {
  if (!entry || entry.is_dir) return;
  if (entry.source_state === 'missing') {
    setNotice(i18n.t('virtual.source_missing'));
    return;
  }
  if (entry.source_state === 'locked') {
    setNotice(i18n.t('virtual.source_locked'));
    return;
  }
  const place = entry.source_place;
  const dir = entry.source_dir;
  const fileId = entry.source_file;
  if (!place || !dir || !fileId) {
    setNotice(i18n.t('virtual.source_missing'));
    return;
  }
  // Telegram 源才有「消息 / 文件」两个落点；其它 provider（WebDAV 等）只有文件。
  const isTg = typeof fileId === 'string' && fileId.startsWith('tg:');
  // 先切真实位置但**跳过根目录加载**：紧接着直接进目标对话/目录，先列根列表既
  // 浪费请求，又会让界面闪一下根目录。
  await openRemotePlace(place, { skipReload: true });
  state.remoteDir = dir;
  state.remoteDirName = '';
  api.uiLog('enter-dir');
  if (mode === 'msg' && isTg) {
    state.remoteTab = 'messages';
    state.remoteViewMode = 'messages';
    const msg = tgMsgOfId(fileId);
    if (msg != null) await locateMessage(msg);
    return;
  }
  // 文件落点：交给 locateFile 统一切分栏/锚定/高亮。Telegram 用记住的分栏
  // （source_media_tab），缺了让它按文件名兜底；非 Telegram 传 'file' 且无消息号，
  // locateFile 会退化成「进源目录刷新一次」（WebDAV 一次列全）。
  await locateFile({
    id: fileId,
    name: entry.name,
    tab: isTg ? (entry.source_media_tab ?? null) : 'file',
    isTelegram: isTg,
  });
}

/** 列出当前远程目录。
 *
 * 失败时清空列表并报错，而不是留着上一个目录的内容——那会让用户
 * 以为自己进到了一个内容相同的目录。
 */
export async function reloadRemoteDir() {
  if (!state.remotePlace) return;
  // 单调递增的「浏览代号」：每次进入/切换位置、目录、分栏都 +1。任何异步浏览
  // （含不触网的虚拟浏览）回写前都要比对自己捕获的序号——用户在 A 加载中点 B，
  // A 的结果晚回来时代号已落后，整份丢弃，绝不允许把 B 的内容覆盖成 A。
  browseSeq += 1;
  // 虚拟位置：不发网络，从本地引用表组装条目（含源三态）。文件夹/引用都映射成
  // remoteItems 里的条目，PlaceBrowser 照常渲染；引用的 is_dir=false、双击走
  // 「定位到真实位置」而不是就地识别。
  if (isVirtualPlace()) {
    await reloadVirtualDir();
    return;
  }
  // 捕获进入时的位置/目录与本次浏览代号：读磁盘 meta、列目录都是异步的，
  // 回来前用户可能切走/切栏/切到虚拟位置，回写前要校验还在同一屏且是最新一次
  // （代号由 reloadRemoteDir 入口统一 +1，比逐字段比对多挡「位置目录恰好相同但
  // 中间切出去又切回」的情况）。
  const placeAtStart = state.remotePlace;
  const dirAtStart = state.remoteDir;
  const browseToken = browseSeq;
  const stale = () => browseToken !== browseSeq
      || state.remotePlace !== placeAtStart
      || state.remoteDir !== dirAtStart;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  state.remoteRetrying = [];
  // 先作废上一个目录的能力再去查：留着旧值会让新目录短暂显示错误的能力，
  // 而用户完全可能在那一瞬间点下去
  state.remoteDirCaps = null;
  state.remoteProtected = false;
  // 换目录就清掉上一个对话的消息，否则切到消息视图会先闪一屏别的对话的内容
  state.remoteMessages = [];
  state.remoteViewMode = 'files';
  state.hasMoreMessages = false;
  state.hasNewerMessages = false;
  state.loadingMore = false;
  state.loadingNewer = false;
  state.locatingMsg = null;
  state.highlightMsg = null;
  state.highlightFile = null;
  state.remoteSelected = [];
  refreshRemoteDirCaps(state.remotePlace, state.remoteDir);
  // 本次重载的暂存区从空开始：留着上一次的会把已被服务端改动过的
  // 旧识别结果贴到新骨架上（比如文件被替换后 id 相同但内容已变）
  remoteEntryBuffer.delete(remoteDirKey(state.remotePlace, state.remoteDir));

  // 这个目录之前进过，就**先把上次的结果摆出来**再去刷新。
  //
  // 实测后端一次 browse 稳定约 0.4 秒（连接是复用的，那 0.4 秒是真的
  // 在拉消息）。不先摆出来的话，每次进对话都是 0.4 秒空白 + 骨架重画
  // ——用户说的「每次进来都需要重新加载」就是这个。
  //
  // 摆出来之后仍然会刷新（Telegram 里随时可能有新文件），只是用户不必
  // 盯着空屏等。
  // 是否在 Telegram 对话内：对话内按媒体分栏取（remote_browse_tab），
  // 根目录（列对话）与网盘走原来的 remote_browse。用 kind + remoteDir 判，
  // 不硬编码 id 形状。
  const inDialog = !!state.remoteDir && isTelegramDialog();
  const tab = inDialog ? state.remoteTab : '';
  // 缓存键带上分栏：同一对话的媒体栏和文件栏是两批内容，共用一个键会串。
  const ckey = remoteDirKey(state.remotePlace, state.remoteDir, tab);
  let cachedRows = remoteDirCache.get(ckey);
  if (cachedRows && cachedRows.length) {
    state.remoteItems = tagConversations(cachedRows);
    void refreshCacheStats(state.remotePlace, cachedRows);
  } else {
    // 内存未命中（多半是重启后首次）：尝试读磁盘 meta 缓存，先把上次这一屏
    // 摆出来，避免空屏等网络。磁盘里存的行不带 thumb_token（见落盘处的说明），
    // 缩略图/头像由后台事件重新补上。
    const diskRows = await loadDirSnapshotFromDisk(ckey);
    if (diskRows && diskRows.length && !stale()) {
      cachedRows = diskRows;
      remoteDirCache.set(ckey, diskRows);
      state.remoteItems = tagConversations(diskRows);
      void refreshCacheStats(state.remotePlace, diskRows);
    }
  }

  // 对话内分栏：首屏请求量按视口估并对齐到整行（见 viewportFillCount）。
  // 在请求前算一次并复用，别在拿到结果后再算——那时窗口可能已被拖宽，
  // 用新值去判 hasMore 会和「实际请求的条数」对不上。
  // 读磁盘快照（一次异步 IPC）期间用户已经切走/切栏：这次网络请求的结果回来也会
  // 被序号守卫丢弃，索性不发。busy 不在这里清——它属于正在进行的新一次浏览，
  // 由那次浏览自己的 finally 复位。
  if (stale()) return;
  const firstFill = inDialog ? viewportFillCount() : 0;
  try {
    const rows = inDialog
      ? await api.remoteBrowseTab(
          state.remotePlace, state.remoteDir, tab, 0, firstFill,
        )
      : await api.remoteBrowse(state.remotePlace, state.remoteDir);
    // 从缓存里把**已升级的清晰缩略图 token**带到刷新回来的新行上。
    // 新行带的是 stripped 占位 token（后端每次 browse 都先给占位、清晰图走
    // 后台事件）。若直接用新行覆盖，二次进对话就会先退回糊占位、再等后台
    // 重新拉一遍清晰图——用户报的「切出切入清晰图退回占位」。缓存里那份 token
    // 是上次后台升级后写透进来的（见 onRemoteEntry），沿用它即可直接出清晰图。
    if (cachedRows && cachedRows.length) {
      const prev = new Map(cachedRows.map((x) => [x.id, x.thumb_token]));
      for (const r of rows) {
        const pt = prev.get(r.id);
        // 缓存里那份是「上次已知的最新 token」——媒体栏首屏给的是 stripped 占位、
        // 后台升级事件再把它换成清晰图并写透进缓存，所以缓存里多半已是清晰图。
        // 新行带的又是 stripped 占位，二者都非空。**优先沿用缓存的**，避免退回
        // 糊占位；本次刷新的后台升级仍会照常跑一遍、确认或再更新它。
        if (pt) r.thumb_token = pt;
      }
    }
    if (stale()) return;
    state.remoteItems = tagConversations(rows);
    remoteDirCache.set(ckey, rows);
    // 写透磁盘 meta（重启后可先出）：去掉 thumb_token——它是会话内 PlaceThumbs
    // 的句柄，重启后失效，存了会让重启后短暂显示裂图。缩略图/头像自有各自的
    // 磁盘缓存(rthumbs)与后台回填，列表 meta 只负责「名字/大小/结构」这一层。
    void saveDirSnapshotToDisk(ckey, rows);
    void refreshCacheStats(state.remotePlace, rows);
    // 取满一页就假定还有更多。只对「对话内层」成立：根目录列的是对话
    // （不分页），WebDAV 的 PROPFIND 也是一次列全。对话内按本次请求的
    // limit 判（不是固定 pageSize），因为分栏首屏 limit 是按视口算的。
    state.hasMoreFiles = inDialog
      ? rows.length >= firstFill
      : (!!state.remoteDir && rows.length >= state.pageSize);
    // 骨架刚落地，把 await 期间早到的识别结果补贴上去。
    // 少了这一句，识别快于 browse 返回时整屏都会卡在「识别中」
    applyBufferedRemoteEntries();
  } catch (e) {
    // 作废的请求（用户已切走）失败了不该清当前屏，更不该弹属于旧位置的错误。
    if (stale()) return;
    // 有缓存就保留：刷新失败不该把用户已经看到的内容抹掉。
    // 清空的话一次网络抖动就让整屏变空，而那些文件其实都还在
    if (!cachedRows || !cachedRows.length) state.remoteItems = [];
    state.hasMoreFiles = false;
    // 位置是加密态且未解锁：不是错误，弹解锁密码框让用户输密码进入。
    // 这是「加密位置锁定后如何进入」的入口——否则加密位置一锁就再进不去。
    if (api.errCode(e) === 'tg_locked') {
      promptTgUnlock(placeAtStart);
      state.placeError = i18n.t('rplace.locked_hint');
    } else {
      // 用云盘视图自己的错误位，不弹底部全局条——那是给本地操作留的
      state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
    }
  } finally {
    // 只有仍是最新一次浏览时才清 busy（理由同 reloadVirtualDir）。
    if (!stale()) {
      state.busy = false;
      state.busyKey = '';
    }
  }
}

/** 加载更早的消息。
 *
 * 用当前最老那条的消息号作 offset。Telegram 的消息号在对话内单调递增，
 * 所以「更早」就是号更小的那些。
 *
 * # 两件必须做对的事
 *
 * 1. **全部追加，不能只追加带文件的。** §7.8 要求消息视图包含纯文本
 *    消息，界面上那行「N 条消息，其中 M 条带文件」正是用来暴露判据有没有
 *    退化成文件视图的；只追加带文件的话翻几页后两个数字就相等了。
 * 2. **按消息号去重。** offset_id 理论上是严格早于，但网络重试或消息被删
 *    都可能让边界错位。重复一条在界面上表现为同一个文件出现两次，
 *    用户会以为自己传了两遍。
 */

/** 批量拉这批条目的缓存状态，填进 `state.remoteCacheStat`。
 *
 * 用批量命令：逐个查的话一屏 N 个文件就 N 次 IPC 往返，而这些查询全在
 * 本地（读缓存索引、不碰网络），往返本身反而成了主要成本。 */
export async function refreshCacheStats(place, items) {
  const files = (items || []).filter((x) => !x.is_dir);
  if (!place || !files.length) return;
  try {
    const stats = await api.remoteCacheFileStats(
      files.map((f) => ({
        // size 必须兜底：缺了它后端 build_remote_source 可能拿不到正确的
        // 分块信息，返回一个 pinned=false 的空 stat（单文件路径就因为
        // 有 `|| 0` 而没踩到）
        place_id: place, path: f.id, size: f.size || 0,
        name: f.real_name || f.name, server_name: f.name,
      })),
    );
    if (!Array.isArray(stats)) return;
    // 写进**已有的** remoteCacheStat，不另起一份。
    //
    // 它原本只在右键菜单里被单个填充，而 pin / unpin / 移除缓存三处都
    // 已经就地更新它。另存一份的代价实测过：pin 之后菜单知道状态变了、
    // 卡片标识不知道，要退出再进才刷新——因为两者看的不是同一份数据。
    const next = { ...state.remoteCacheStat };
    files.forEach((f, i) => {
      if (stats[i]) next[remoteFileCacheKeyFor(f, place)] = stats[i];
    });
    state.remoteCacheStat = next;
  } catch {
    // 查不到就不画标识。这是附加信息，失败不该影响浏览本身
  }
}

/** 批量拉一批**虚拟引用**条目的缓存状态。
 *
 * 引用的缓存归属在各自的**源真实位置**：同一屏引用可能来自多个位置，按
 * source_place 分组、每组一次 remoteCacheFileStats（path 用源 file_id），
 * 结果写进 remoteFileCacheKeyFor 会命中的源 key。纯本地查询（读缓存索引、
 * 不触网），与 refreshCacheStats 同样失败静默。 */
async function refreshVirtualCacheStats(entries) {
  const refs = (entries || []).filter(
    (e) => e && !e.is_dir && e.source_state === 'available'
      && e.source_place && e.source_file);
  if (!refs.length) return;
  const groups = new Map();
  for (const e of refs) {
    const g = groups.get(e.source_place) || [];
    g.push(e);
    groups.set(e.source_place, g);
  }
  const next = { ...state.remoteCacheStat };
  for (const [place, list] of groups) {
    try {
      const stats = await api.remoteCacheFileStats(
        list.map((e) => ({
          place_id: place, path: e.source_file, size: e.size || 0,
          name: e.real_name || e.name, server_name: e.name,
        })),
      );
      if (!Array.isArray(stats)) continue;
      list.forEach((e, i) => {
        if (stats[i]) next[remoteFileCacheKeyFor(e)] = stats[i];
      });
    } catch {
      // 单组查不到不影响其它源位置的标识
    }
  }
  state.remoteCacheStat = next;
}

/** 取后端的分页大小。启动时问一次就够——它是编译期常量。 */
export async function loadPageSize() {
  try {
    const n = await api.remotePageSize();
    if (Number.isFinite(n) && n > 0) state.pageSize = n;
  } catch {
    // 取不到就用默认值；判据只是「取满一页就可能还有」，
    // 差一点不会导致漏文件，最多多问一次
  }
}

/** 加载更多文件。
 *
 * 与消息视图那条同源：用当前最后一条的消息号作 offset。
 *
 * # 为什么必须有
 *
 * 固定拉一页的话，活跃群里**第 N+1 个文件之后永远看不到，而且界面上
 * 没有任何迹象**——用户会以为那些文件不在这个群里。
 */
export async function loadMoreFiles() {
  if (state.loadingMoreFiles || !state.hasMoreFiles) return;
  if (!state.remotePlace || !state.remoteDir) return;
  const last = state.remoteItems.at(-1);
  if (!last) return;
  // Telegram 条目 id 形如 tg:<对话>:<消息号>，取最后一段。
  // 这里能按 ':' 切是因为**这条路径只对 Telegram 开放**（后端对其它
  // provider 直接回 remote_no_messages），不是在猜通用 id 的形状
  const n = Number.parseInt(String(last.id).split(':').pop(), 10);
  if (!Number.isFinite(n)) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  const inDialog = isTelegramDialog();
  const tab = inDialog ? state.remoteTab : '';
  // 续翻同样对齐到整行：否则每一页都可能末行缺几块、网格右侧出现空位。
  const lim = inDialog ? alignToGrid(viewportFillCount()) : viewportFillCount();
  state.loadingMoreFiles = true;
  try {
    // 对话内按当前分栏续拉（同一个类型 filter），否则走无过滤的 more
    const rows = inDialog
      ? await api.remoteBrowseTab(place, dir, tab, n, lim)
      : await api.remoteBrowseMore(place, dir, n);
    // 回写前校验还在同一个目录**且没换栏**：用户可能在请求飞行途中切走
    // 或切了栏，晚到的这批不能贴到别的栏上
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    if (inDialog && state.remoteTab !== tab) return;
    const seen = new Set(state.remoteItems.map((x) => x.id));
    const fresh = rows.filter((x) => !seen.has(x.id));
    state.remoteItems = [...state.remoteItems, ...fresh];
    // 回写内容缓存（带 tab 的键），下次进来先摆这批
    remoteDirCache.set(remoteDirKey(place, dir, tab), state.remoteItems);
    const need = inDialog ? lim : state.pageSize;
    state.hasMoreFiles = rows.length >= need && fresh.length > 0;
    void refreshCacheStats(place, fresh);
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.loadingMoreFiles = false;
  }
}

export async function loadMoreMessages(): Promise<void> {
  if (state.loadingMore || !state.hasMoreMessages) return;
  if (!state.remotePlace || !state.remoteDir) return;
  const oldest = state.remoteMessages.at(-1);
  if (!oldest) return;
  const place = state.remotePlace;
  const dir = state.remoteDir;
  state.loadingMore = true;
  try {
    const rows = await api.remoteMessages(place, dir, oldest.message);
    // 回写前校验还在同一个对话：用户可能在请求飞行途中切走了
    if (state.remotePlace !== place || state.remoteDir !== dir) return;
    const seen = new Set(state.remoteMessages.map((m) => m.message));
    const fresh = rows.filter((m) => !seen.has(m.message));
    state.remoteMessages = [...state.remoteMessages, ...fresh];
    state.hasMoreMessages = rows.length >= 25 && fresh.length > 0;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.loadingMore = false;
  }
}

/** 查「受保护内容」标记，查到后写回 `state.remoteProtected`。
 *
 * 与 `refreshRemoteDirCaps` 一样不 await、一样在回写前校验位置与目录
 * 仍是当前这一个——用户可能在请求飞行途中已经切走，那时把旧对话的结果
 * 写上去，界面显示的就是上一个对话的状态。
 *
 * 取不到时按「没开保护」处理：这只是一行提示，不该因为它报错或挡住目录。
 */
function refreshProtected(placeId, dir) {
  if (!placeId || !dir) {
    state.remoteProtected = false;
    return;
  }
  api
    .remoteDirProtected(placeId, dir)
    .then((v) => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteProtected = !!v;
    })
    .catch(() => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteProtected = false;
    });
}

/** 查当前远程目录的有效能力，查到后写回 `state.remoteDirCaps`。
 *
 * 不 await：能力查询不该拖慢列目录（对某些 provider 它是一次网络请求）。
 * 列表先出来，按钮随后补上——反过来会让整屏卡在一个只影响菜单的请求上。
 *
 * 回写前校验位置与目录仍是当前这一个：切目录后晚到的结果必须丢弃，
 * 否则会把上一个目录的能力贴到新目录上。
 */
function refreshRemoteDirCaps(placeId, dir) {
  // 「受保护内容」跟着对话走，与能力同一时机刷新。不在这里刷的话，
  // 从一个受保护的群退回对话列表时那行告知会留着，变成「在没开保护
  // 的地方显示着上一个对话的告知」
  refreshProtected(placeId, dir);
  api
    .remoteEffectiveCaps(placeId, dir)
    .then((caps) => {
      if (state.remotePlace !== placeId || state.remoteDir !== dir) return;
      state.remoteDirCaps = caps;
    })
    .catch(() => {
      // 查不到就保持 null，`currentCaps` 会按保守只读处理。
      // 不在这里回落到位置级上界——那等于把上界当实际能力用
    });
}

/** 就地替换一个远程条目（单条目探测与边扫边出共用）。
 *  只更新当前列表里已存在的 id：切目录后晚到的结果找不到骨架，直接丢弃，
 *  绝不 push 进新目录，避免上一屏的条目串到下一屏。 */
export function patchRemoteItem(entry) {
  const idx = state.remoteItems.findIndex((x) => x.id === entry.id);
  if (idx >= 0) state.remoteItems[idx] = entry;
  state.remoteRetrying = state.remoteRetrying.filter((id) => id !== entry.id);
}

/** 单独重试一个「未能读取（网络）」条目，不重载整个目录。
 *  命令本身失败（如位置已失效）时恢复可再点的失败态，不吞错也不卡死。 */
export async function retryRemoteEntry(f) {
  if (!state.remotePlace || state.remoteRetrying.includes(f.id)) return;
  state.remoteRetrying = [...state.remoteRetrying, f.id];
  try {
    // 把已知的名字一并传过去：后端从 id 猜名字的那条回落路径写死了
    // WebDAV 的路径形状，对 Telegram 的 id 会猜成整个 id
    const entry = await api.remoteProbeEntry(
      state.remotePlace, f.id, f.size || 0, f.name);
    patchRemoteItem(entry);
  } catch (_) {
    state.remoteRetrying = state.remoteRetrying.filter((id) => id !== f.id);
  }
}

/** 识别结果暂存区：键是「位置 + 目录」，值是 id 到已识别条目的映射。
 *
 *  存在的理由是一个实测到的竞态，不是防御性编程：后台识别任务在
 *  `remote_browse` **内部**被 spawn，识别结果走事件、骨架走返回值，
 *  两条路径互相独立。头部改走密文块缓存之后，单文件识别只要 2~5ms，
 *  于是事件会**早于** `remote_browse` 返回就到达（实测 browse 在 399ms
 *  返回，6 条事件落在 398~399ms）。那一刻 `state.remoteItems` 还是上一屏，
 *  findIndex 找不到 id，事件被丢掉；紧接着骨架整份覆盖上来，
 *  界面就永远停在「识别中」。
 *
 *  注意这是缓存优化把一个潜伏的竞态变成了必现故障——在那之前识别要 1.5 秒，
 *  稳稳晚于 browse 返回，所以从没暴露过。 */
export const remoteEntryBuffer = new Map();

/** 暂存区的键。用 \u0000 分隔是因为它不可能出现在位置 id 或对话 id 里；
 *  用 ':' 会和 Telegram 的 `tg:<对话>:<消息>` 撞上。 */
/** 把一屏列表快照存进磁盘 meta 缓存（kind='list'）。
 *  去掉 thumb_token（会话句柄，重启失效）；只留名字/大小/结构等可明文项。
 *  真实文件名(.omy 解密名)不在 browse 返回里、不会进这里，隐私安全。 */
async function saveDirSnapshotToDisk(key, rows) {
  try {
    const slim = rows.map((r) => {
      const { thumb_token, ...rest } = r;
      return rest;
    });
    const json = JSON.stringify(slim);
    const bytes = new TextEncoder().encode(json);
    await api.remoteMetaPut('list', key, bytes);
  } catch {
    // 缓存是附加项，写不进只降级为「重启后走网络」，不影响浏览
  }
}

/** 从磁盘 meta 缓存读回一屏列表快照；未命中/损坏返回 null。 */
async function loadDirSnapshotFromDisk(key) {
  try {
    const bytes = await api.remoteMetaGet('list', key);
    if (!bytes || !bytes.length) return null;
    const json = new TextDecoder().decode(new Uint8Array(bytes));
    const rows = JSON.parse(json);
    return Array.isArray(rows) ? rows : null;
  } catch {
    return null;
  }
}

export function remoteDirKey(placeId: string, dir: string, tab?: string): string {
  // tab 只在**内容缓存**里带上（同一对话媒体栏/文件栏是两批内容）。
  // 识别事件的暂存区仍用不带 tab 的 2 参数键：识别结果按文件 id 贴，
  // 与它属于哪一栏无关，两者共用同一个函数但传参不同。
  return tab
    ? `${placeId}\u0000${dir}\u0000${tab}`
    : `${placeId}\u0000${dir}`;
}

/** 当前是否在某个 Telegram 对话内（而非根对话列表 / 网盘）。
 *  分栏只在这种情况下有意义。 */
export function isTelegramDialog() {
  if (!state.remoteDir) return false;
  const p = state.remotePlaces.find((x) => x.id === state.remotePlace);
  return p?.kind === 'telegram';
}

/** 给「Telegram 根对话列表」的每一行打上 is_conversation 标记。
 *
 * 图标该显示 💬（对话）还是 📁（文件夹）必须由**行自身**决定，不能靠渲染时
 * 的全局 state.remoteDir —— 双击进对话时 remoteDir 会先变、列表后换，中间那
 * 一帧旧对话行会被误判成文件夹（见 PlaceBrowser.icon 的说明）。所以在建列表
 * 时（此刻还确实在根目录）就把标记写死到行上。只标 Telegram 根目录的目录行；
 * 网盘/对话内的行不加，保持原样。 */
function tagConversations(rows) {
  if (!rows || !rows.length) return rows;
  const p = state.remotePlaces.find((x) => x.id === state.remotePlace);
  const atTelegramRoot = !state.remoteDir && p?.kind === 'telegram';
  if (!atTelegramRoot) return rows;
  for (const r of rows) {
    if (r && r.is_dir) r.is_conversation = true;
  }
  return rows;
}
