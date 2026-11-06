/** 远程目录内存缓存、后台事件监听、远程文件的解密、缓存、上传与常驻。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import * as api from '../api';
import * as i18n from '../i18n';
import { setNotice, state } from './state';
import { isTelegramDialog, reloadRemoteDir, remoteDirKey, remoteEntryBuffer } from './remote-browse';
import { openTransfers } from './places';

/** 已加载过的目录列表：`位置\x01目录` -> 条目数组。
 *
 * # 为什么要缓存
 *
 * 实测后端 remote_browse 连续五次是 6332 / 408 / 408 / 428 / 439 ms
 * ——**连接是复用的**（第一次含握手），但没有任何列表缓存，每次进对话
 * 都真的去 RPC 一趟。界面上表现为每次都有约 0.4 秒空白 + 骨架重画，
 * 也就是用户说的「每次进来都需要重新加载」。
 *
 * 缓存之后再进立刻见内容，同时后台静默刷新、有变化才替换。
 *
 * **刻意不设过期时间**：静默刷新总会发生，加个 TTL 只会让「多久算新鲜」
 * 变成一个要调的参数，而调错的表现是用户看到过期列表。
 */
export const remoteDirCache = new Map();

// ⚠️ 这个声明必须留在 reloadRemoteDir **之前**。
//
// 它原来写在文件后半，而 reloadRemoteDir 在前面就读它——`const` 不像
// `function` 会提升，模块顶层的 const 在求值到那一行之前处于时间死区，
// 提前访问直接抛 ReferenceError。
//
// 而那个错误**不会显示出来**：reloadRemoteDir 是 async，抛出变成
// rejected promise 被静默吞掉，表现只是「缓存好像没生效」，甚至因为
// 走了退化路径而比不缓存更慢（实测二次进入 1.33s，比首次 0.47s 还慢）。

/** 把暂存区里属于当前这一屏的识别结果贴到列表上。
 *
 *  `reloadRemoteDir` 写完骨架后立刻调用，专治「事件比骨架先到」。 */
export function applyBufferedRemoteEntries() {
  const buf = remoteEntryBuffer.get(remoteDirKey(state.remotePlace, state.remoteDir));
  if (!buf || buf.size === 0) return;
  for (let i = 0; i < state.remoteItems.length; i += 1) {
    const hit = buf.get(state.remoteItems[i].id);
    // 后端事件行不带 is_conversation（那是前端在根目录建列表时打的行级标记）。
    // 替换时保留它，否则头像事件一到就把 💬 判据抹掉、无头像的对话又闪回 📁。
    if (hit) state.remoteItems[i] = state.remoteItems[i].is_conversation
      ? { ...hit, is_conversation: true } : hit;
  }
}

/**
 * 全局只注册一次「边扫边出」监听：remote_browse 返回骨架后，后台每识别完
 * 一个文件推一条 remote-entry，这里只在事件仍属于当前位置+当前目录时，
 * 就地替换同 id 骨架；切走目录后晚到的事件直接丢弃，绝不串屏。
 */
let remoteListenerStarted = false;
export async function ensureRemoteListeners() {
  if (remoteListenerStarted) return;
  remoteListenerStarted = true;
  await api.onRemoteEntry((p) => {
    if (!p || !p.entry) return;
    if (p.place_id !== state.remotePlace || p.dir !== state.remoteDir) return;
    // 先存再贴：骨架可能还没到（见 remoteEntryBuffer 的说明）。
    // 只为当前位置+目录暂存，切走后晚到的事件连存都不存，
    // 否则反复进出目录会让这张表一直涨。
    const key = remoteDirKey(p.place_id, p.dir);
    let buf = remoteEntryBuffer.get(key);
    if (!buf) {
      buf = new Map();
      remoteEntryBuffer.set(key, buf);
    }
    buf.set(p.entry.id, p.entry);
    const idx = state.remoteItems.findIndex((x) => x.id === p.entry.id);
    // 保留行级 is_conversation（后端事件不带它），避免头像事件把 💬 判据抹掉。
    if (idx >= 0) {
      state.remoteItems[idx] = state.remoteItems[idx].is_conversation
        ? { ...p.entry, is_conversation: true } : p.entry;
    }
    // **写透到内容缓存**：识别结果与清晰缩略图 token 都通过这个事件回来。
    // 只更新 state.remoteItems 而不更新缓存的话，二次进对话摆出的是缓存里
    // 那批**带 stripped 占位 token 的旧行**，于是清晰图退回糊占位、又得重新
    // 后台拉一遍——这正是用户报的「切出切入清晰缩略图退回占位」。
    // 缓存键要带当前分栏（媒体栏和文件栏是两批）。
    const tab = isTelegramDialog() ? state.remoteTab : '';
    const ck = remoteDirKey(p.place_id, p.dir, tab);
    const cachedArr = remoteDirCache.get(ck);
    if (cachedArr) {
      const ci = cachedArr.findIndex((x) => x.id === p.entry.id);
      if (ci >= 0) cachedArr[ci] = p.entry;
    }
  });
}

/** 把一个已解锁的远程 `.omy` 流式解密到用户选择的本地目录。
 *  只读位置也保留（这是只读位置的主要用途）。复用全局解密进度条；
 *  默认不覆盖，目标已存在时后端返回 target_exists。 */
export async function decryptRemoteToLocal(f) {
  if (!state.remotePlace || !f || f.is_dir || !f.is_encrypted || !f.unlocked) return null;
  let dest;
  try {
    dest = await api.pickFolder(i18n.t('rplace.decrypt_pick_title'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.decrypt_failed'));
    return null;
  }
  if (!dest) return null; // 用户在目录选择器里取消

  state.busy = true;
  state.busyKey = 'busy.remote_decrypt';
  state.error = '';
  state.notice = '';
  state.progress = null;
  let unlisten = null;
  try {
    unlisten = await api.onDecryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    unlisten = null;
  }
  try {
    const r = await api.remoteDecryptToLocal(
      state.remotePlace, f.id, f.size || 0, f.name || null, dest);
    setNotice(i18n.t('rplace.decrypted_local', { name: r.name, dir: dest }));
    return r;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.decrypt_failed'));
    return null;
  } finally {
    state.busy = false;
    state.busyKey = '';
    state.progress = null;
    if (unlisten) unlisten();
  }
}

/** 远程单文件缓存状态在 state.remoteCacheStat 里的键。
 *
 * 虚拟引用的密文缓存是**委托真实位置**读取时落下的，key 与在真实位置直接打开
 * 同一文件完全相同（源 place + 源 file_id）；所以这里对引用条目解析到源，而不是
 * 用虚拟位置自己的 id（那样必然查不到、角标永远不亮）。普通条目维持
 * 「当前位置 + 条目 id」。 */
export function remoteFileCacheKeyFor(f, fallbackPlace?: string | null) {
  const place = (f?.is_ref ? f.source_place : null) ?? fallbackPlace ?? state.remotePlace;
  const path = f?.is_ref ? f.source_file : f?.id;
  // 控制字符分隔，正常路径里不会出现，避免 place/path 粘连
  return `${place}\u{1}${path}`;
}

/**
 * 打开条目菜单时按需查询该文件的密文块缓存覆盖情况（不阻塞菜单弹出）。
 * 列目录不逐文件查：一部几 GB 的电影有数千块，逐文件 stat 会拖慢整屏扫描。
 */
export async function requestRemoteFileCache(f) {
  // 普通文件也要查：远程位置里它们是主体内容，同样走块缓存、
  // 同样值得永久保留（离线看视频）。原先这里排除非加密文件，
  // 于是普通文件的右键菜单里永远没有「转为永久」，而后端明明支持。
  if (!state.remotePlace || !f || f.is_dir) return;
  try {
    const stat = await api.remoteCacheFileStat(
      (f.is_ref ? f.source_place : state.remotePlace) || state.remotePlace,
      f.is_ref ? f.source_file : f.id, f.size || 0, f.name || null);
    const key = remoteFileCacheKeyFor(f);
    state.remoteCacheStat = { ...state.remoteCacheStat, [key]: stat };
  } catch {
    // 查不到（非加密 / 头部读取失败）就当无缓存：菜单本就可以没有这一项，不打扰用户
  }
}

/** 读取已查询过的单文件缓存状态（供菜单响应式判断是否显示「从缓存中移除」）。 */
export function remoteFileCache(f) {
  if (!f || !state.remotePlace) return null;
  return state.remoteCacheStat[remoteFileCacheKeyFor(f)] || null;
}

/** 选本地文件上传到当前远程目录。
 *
 * 上传用的是**磁盘上的文件名**（后端保证）。用户开了文件名加密时磁盘上
 * 就是那串密文名——用解密后的真名上传等于把加密掉的文件名主动交给服务端。
 *
 * 部分成功是常态（网络抖动、个别文件超限），所以逐个报而不是一句
 * 「失败了」：不区分的话用户不知道哪些传上去了，再传一次就产生重复文件。
 */
export async function uploadToRemote() {
  if (!state.remotePlace || !state.remoteDir) return false;
  let paths;
  try {
    paths = await api.pickFiles(i18n.t('rplace.upload_pick_title'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.upload_failed'));
    return false;
  }
  if (!paths || !paths.length) return false; // 用户取消

  state.busy = true;
  state.busyKey = 'busy.uploading';
  state.error = '';
  state.notice = '';
  try {
    const res = await api.remoteUpload(state.remotePlace, state.remoteDir, paths);
    const ok = res.filter((r) => r.ok).length;
    const bad = res.filter((r) => !r.ok);
    // 传完必须重新列目录，否则用户看不到刚传上去的东西、以为失败了
    await reloadRemoteDir();
    if (bad.length && ok) {
      // 部分成功要说清哪些没成——只说「部分失败」的话，
      // 用户只能整批重传，于是产生重复文件
      state.error = i18n.t('rplace.upload_partial', {
        ok,
        names: bad.map((b) => b.name).join('、'),
      });
    } else if (bad.length) {
      state.error = i18n.t('rplace.upload_failed');
    } else {
      setNotice(i18n.tn('rplace.upload_done', ok));
    }
    return ok > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.upload_failed'));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

export async function startTelegramCacheAll(filters) {
  if (!state.telegramCacheAll) return false;
  const context = state.telegramCacheAll;
  try {
    await api.telegramCacheAll({
      place_id: context.placeId,
      dir: context.dir,
      dir_name: context.dirName,
      media_types: filters.mediaTypes,
      from: filters.from,
      to: filters.to,
      keyword: filters.keyword,
    });
    state.telegramCacheAll = null;
    openTransfers();
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('cache_all.failed'));
    return false;
  }
}

/** 把一个远程文件保存为**完整原文件**永久缓存。
 *
 * 下载过程写临时文件，完整校验后才原子提交；服务端文件名用于磁盘落盘，
 * 解密后的真实名只用于传输列表显示。
 */
export async function pinRemoteFile(f) {
  if (!state.remotePlace || !f || f.is_dir) return false;
  state.busy = true;
  state.busyKey = 'busy.pinning';
  state.error = '';
  try {
    // 带上列表里显示的那个名字。不传的话后端只能从 id 猜，而 Telegram
    // 的 id 是 tg:<对话>:<消息号>，猜出来是个数字——传输列表里就会
    // 出现一行「6」，用户不知道那是哪个文件（实测过）
    // 注意：remoteCachePin 返回的是**字节数**（u64），不是 FileCacheStat。
    // 早先把它当 stat 存进 remoteCacheStat，于是 st.pinned 是 undefined，
    // 卡片标识判成 false（菜单却"对"，因为菜单打开时另查了一次真 stat
    // 覆盖掉那个数字——同一份数据两条路径写成不同形状）。
    await api.remoteCachePin(
      state.remotePlace,
      f.id,
      f.size || 0,
      f.real_name || f.name || null,
      f.name || null,
    );
    // 重新查一次真 stat 填进去。用 requestRemoteFileCache（单文件、
    // 与右键菜单同一条路径），不用批量版：批量版在单文件场景实测不生效
    // （最可能是 f 缺 size 兜底），而单数路径一直可靠
    await requestRemoteFileCache(f);
    setNotice(i18n.t('rplace.pinned'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.pin_failed'));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 取消永久缓存：删除完整原文件，已有临时分块仍由 LRU 管理。 */
export async function unpinRemoteFile(f) {
  if (!state.remotePlace || !f || f.is_dir) return false;
  try {
    // remoteCacheUnpin 返回字节数、不是 stat。用单数路径重查真 stat
    await api.remoteCacheUnpin(
      state.remotePlace, f.id, f.size || 0, f.name || null);
    await requestRemoteFileCache(f);
    setNotice(i18n.t('rplace.unpinned'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.unpin_failed'));
    return false;
  }
}

/** 删除单个远程文件的本地密文块。只读位置也允许：只清本机缓存，绝不写云端。 */
export async function removeRemoteFileCache(f) {
  // 同 requestRemoteFileCache：普通文件也有缓存块要清
  if (!state.remotePlace || !f || f.is_dir) return false;
  try {
    const r = await api.remoteCacheRemoveFile(state.remotePlace, f.id, f.size || 0);
    const key = remoteFileCacheKeyFor(f);
    const prev = state.remoteCacheStat[key];
    const zero = {
      cached_blocks: 0,
      total_blocks: prev ? prev.total_blocks : 0,
      cached_bytes: 0,
      fully_cached: false,
      // 「从缓存中移除」只清临时层，不动永久标记，所以这一位要沿用原值
      // 而不是一律置 false——置 false 会让一个仍然永久保留的文件在界面上
      // 显示成普通缓存，用户以为自己的永久标记丢了
      pinned: prev ? prev.pinned === true : false,
    };
    state.remoteCacheStat = { ...state.remoteCacheStat, [key]: zero };
    setNotice(i18n.t('rplace.removed_cache', { size: i18n.formatSize(r.freed_bytes || 0) }));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.remove_cache_failed'));
    return false;
  }
}
