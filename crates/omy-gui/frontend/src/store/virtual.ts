/** 虚拟位置、远程多选、转发与「上传到 / 添加到」对话框的状态编排。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import * as api from '../api';
import * as i18n from '../i18n';
import { askText } from '../ask';
import { setNotice, state } from './state';
import { tgMsgOfId } from './remote-view';
import { leaveRemotePlace, openRemotePlace, reloadRemoteDir } from './remote-browse';
import { openTransfers } from './places';
import { submitTelegramAdvancedBatch } from './remote-files';

/** 刷新远程位置列表。 */
export async function reloadRemotePlaces() {
  // 两份列表互不依赖，并发取：串行时每次锁定 / 解锁 / 增删位置都多等一轮 IPC，
  // 侧栏的锁徽标也要晚一拍才更新
  const [remote, virtual] = await Promise.all([
    api.remotePlaceList().catch(() => []),
    api.virtualPlaces().catch(() => []),
  ]);
  state.remotePlaces = remote;
  state.virtualPlaces = virtual;
}

/** 重命名虚拟位置（侧栏「编辑」）。空名忽略。 */
export async function renameVirtualPlace(id, name) {
  const trimmed = (name || '').trim();
  if (!trimmed) return;
  try {
    await api.virtualRename(id, trimmed);
    await reloadRemotePlaces();
    setNotice(i18n.t('rplace.renamed'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e), 'errors.remote_failed');
  }
}

/** 删除虚拟位置（侧栏）。调用方负责先 confirm。正在浏览它就退出。 */
export async function deleteVirtualPlace(id) {
  await api.virtualDelete(id);
  if (state.remotePlace === id) leaveRemotePlace();
  state.virtualClipboard = null;
  await reloadRemotePlaces();
  setNotice(i18n.t('virtual.deleted'));
}

/** 侧栏「加密虚拟位置」：弹与 Telegram 同款的加密对话框。 */
export function promptVirtualEncrypt(id) {
  const p = state.virtualPlaces.find((x) => x.id === id);
  state.vEncryptFor = { id, name: p?.name || '' };
}
export function cancelVirtualEncrypt() {
  state.vEncryptFor = null;
  state.vEncryptBusy = false;
}
export async function confirmVirtualEncrypt({ password }) {
  const target = state.vEncryptFor;
  if (!target || !password) return false;
  state.vEncryptBusy = true;
  try {
    // 收集「同密码」的 vault 材料：当前浏览的本地目录 + 所有远程位置。
    // KEK 与 vault_salt 绑定，加密时把这些 salt/参数一并登记，解锁虚拟位置时
    // 才能用同一密码为每个 vault 重派生 KEK，实现互相自动解锁。
    // 远程 vault 拉取是网络操作，失败就跳过那批（本地的仍登记），不阻断加密。
    const vaults: any[] = [];
    try {
      if (state.cwd) {
        const local = await api.vaultParamsOf(state.cwd).catch(() => []);
        vaults.push(...(Array.isArray(local) ? local : []));
      }
      for (const place of state.remotePlaces || []) {
        try {
          // 远程位置根目录下列出的是对话；用每个位置根的加密文件头取 vault 参数。
          const rv = await api.remotePlaceVaults(place.id, '').catch(() => []);
          if (Array.isArray(rv)) vaults.push(...rv);
        } catch { /* 单个位置取不到不阻塞 */ }
      }
    } catch { /* 收集 vault 是增强项，失败仍允许加密（只是不自动解锁其它） */ }

    await api.virtualEncrypt({ placeId: target.id, password, vaults });
    await reloadRemotePlaces();
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    setNotice(i18n.t('rplace.encrypted'));
    state.vEncryptFor = null;
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), 'errors.tg_encrypt_failed');
    return false;
  } finally {
    state.vEncryptBusy = false;
  }
}

/** 进入一个加密但未解锁的虚拟位置前弹解锁框。 */
export function promptVirtualUnlock(id) {
  const p = state.virtualPlaces.find((x) => x.id === id);
  state.vUnlockFor = { id, name: p?.name || '' };
  state.vUnlockError = '';
}
export function cancelVirtualUnlock() {
  state.vUnlockFor = null;
  state.vUnlockBusy = false;
  state.vUnlockError = '';
}
export async function confirmVirtualUnlock(password) {
  const target = state.vUnlockFor;
  if (!target) return false;
  state.vUnlockBusy = true;
  try {
    await api.virtualUnlock(target.id, password);
    state.vUnlockFor = null;
    await reloadRemotePlaces();
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    // 若解锁的正是当前位置，重新载入它的内容
    if (state.remotePlace === target.id) await reloadRemoteDir();
    return true;
  } catch (e) {
    if (api.errCode(e) === 'virtual_wrong_password') {
      state.vUnlockError = i18n.t('virtual.wrong_password');
    } else {
      state.vUnlockError = i18n.te(api.errCode(e), 'errors.remote_failed');
    }
    return false;
  } finally {
    state.vUnlockBusy = false;
  }
}

/** 锁定当前/指定虚拟位置：退出浏览并清本会话明文。 */
export async function lockVirtualPlace(id) {
  await api.virtualLock(id);
  if (state.remotePlace === id) leaveRemotePlace();
  await reloadRemotePlaces();
}

/* ---------------- 虚拟位置内的文件/文件夹操作（剪贴板） ---------------- */

/** 在当前虚拟位置新建文件夹（右键空白/工具入口），刷新当前目录。 */
export async function newVirtualFolderPrompt() {
  if (!isVirtualPlace()) return;
  const name = await askText({ title: i18n.t('virtual.folder_name_prompt') });
  if (name === null) return;
  await addVirtualFolder(name);
}

/** 重命名虚拟位置里的一个文件夹（右键）。 */
export async function renameVirtualFolderPrompt(folderId) {
  if (!isVirtualPlace() || !folderId) return;
  const cur = state.remoteItems.find((x) => x.id === folderId)?.name || '';
  const trimmed = await askText({ title: i18n.t('virtual.folder_rename_prompt'), initial: cur });
  if (!trimmed) return;
  await api.virtualRenameFolder(state.remotePlace, folderId, trimmed);
  await reloadRemoteDir();
}

/** 删除虚拟位置里的一个文件夹（右键，先由调用方 confirm）。返回删除的引用条数。 */
export async function removeVirtualFolder(folderId) {
  if (!isVirtualPlace() || !folderId) return 0;
  const n = await api.virtualRemoveFolder(state.remotePlace, folderId);
  await reloadRemoteDir();
  return n || 0;
}

/** 删除一条/多条引用（右键或键盘 Delete）。返回删除条数。 */
export async function removeVirtualRefs(items) {
  const list = Array.isArray(items) ? items : [items];
  let n = 0;
  for (const f of list) {
    if (!f?.is_ref) continue;
    try {
      if (await api.virtualRemoveRef(state.remotePlace, f.id)) n += 1;
    } catch (e) {
      state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
    }
  }
  if (n) await reloadRemoteDir();
  return n;
}

/* ---------------- 远程浏览多选（桌面 Ctrl/⌘ + 点，移动端长按） ---------------- */

/** 当前是否处于多选态。 */
export function remoteSelectionActive() {
  return state.remoteSelected.length > 0;
}

/** 切换某条目的选中态。addTo=true 时只加不取消（长按进入的第一条）。 */
export function toggleRemoteSelected(id: string, addTo = false) {
  const i = state.remoteSelected.indexOf(id);
  if (i >= 0) {
    if (!addTo) state.remoteSelected.splice(i, 1);
  } else {
    state.remoteSelected.push(id);
  }
}

export function clearRemoteSelection() {
  state.remoteSelected = [];
}

/** 当前消息时间线是否处于多选态。 */
export function remoteMessageSelectionActive() {
  return state.remoteMessageSelected.length > 0;
}

/** 切换一条消息的选中态；addTo=true 时只加不取消。 */
export function toggleRemoteMessageSelected(message: number, addTo = false) {
  const i = state.remoteMessageSelected.indexOf(message);
  if (i >= 0) {
    if (!addTo) state.remoteMessageSelected.splice(i, 1);
  } else {
    state.remoteMessageSelected.push(message);
  }
}

export function clearRemoteMessageSelection() {
  state.remoteMessageSelected = [];
}

/** 打开 Telegram 原生转发目标选择器。 */
export function openTelegramForward(messageIds: number[]) {
  const place = state.remotePlaces.find((p) => p.id === state.remotePlace);
  const ids = [...new Set((messageIds || []).filter(Number.isInteger))].sort((a, b) => a - b);
  if (state.remoteProtected) {
    state.placeError = i18n.te('tg_forward_protected');
    return false;
  }
  if (place?.kind !== 'telegram' || !state.remoteDir || !ids.length) return false;
  if (ids.length > 100) {
    state.placeError = i18n.t('tg_forward.too_many');
    return false;
  }
  state.telegramForward = {
    placeId: state.remotePlace,
    sourceDir: state.remoteDir,
    messageIds: ids,
  };
  return true;
}

/** 从文件网格的一条或多条 Telegram 文件打开转发。 */
export function openTelegramForwardFiles(items) {
  const list = Array.isArray(items) ? items : [items];
  return openTelegramForward(list.map((f) => tgMsgOfId(f?.id)).filter((n) => n != null));
}

export function cancelTelegramForward() {
  state.telegramForward = null;
}

/** 确认转发；成功才清选择，失败保留原选择便于换目标重试。 */
export async function confirmTelegramForward(targetDir: string) {
  const pending = state.telegramForward;
  if (!pending || !targetDir) return 0;
  if (pending.advancedBatch) {
    const ok = await submitTelegramAdvancedBatch(pending.advancedBatch, { kind: 'forward', dir: targetDir });
    if (ok) state.telegramForward = null;
    else state.placeError = state.error;
    return ok ? 1 : 0;
  }
  try {
    const count = await api.telegramForwardMessages(
      pending.placeId,
      pending.sourceDir,
      targetDir,
      pending.messageIds,
    );
    state.telegramForward = null;
    state.remoteSelected = [];
    state.remoteMessageSelected = [];
    state.placeError = '';
    setNotice(i18n.tn('tg_forward.done', count, { count }));
    return count;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.t('tg_forward.failed'));
    return 0;
  }
}

/** 取当前选中的完整条目（按当前 remoteItems，顺序稳定）。 */
export function selectedRemoteEntries() {
  return state.remoteItems.filter((f) => state.remoteSelected.includes(f.id));
}

/** 多选批量删除虚拟引用。返回删除条数。真实文件不允许走这里（组件已拦）。 */
export async function removeSelectedVirtualRefs() {
  const refs = selectedRemoteEntries().filter((f) => f.is_ref && !f.is_dir);
  if (!refs.length) return 0;
  const n = await removeVirtualRefs(refs);
  state.remoteSelected = [];
  return n;
}

/** 多选批量剪切/复制虚拟引用。真实文件不进虚拟剪贴板（组件只在虚拟位置开放）。 */
export function clipSelectedVirtual(mode: 'cut' | 'copy') {
  const refs = selectedRemoteEntries().filter((f) => f.is_ref && !f.is_dir);
  if (!refs.length) return;
  setVirtualClipboard(mode, refs);
}

/** 把条目放进虚拟剪贴板（剪切/复制）。只接受虚拟引用；其它来源忽略。 */
export function setVirtualClipboard(mode, items) {
  if (!isVirtualPlace()) return;
  const refs = (Array.isArray(items) ? items : [items]).filter((f) => f?.is_ref && !f.is_dir);
  if (!refs.length) return;
  state.virtualClipboard = { mode, placeId: state.remotePlace, items: refs };
}

/** 当前能否在当前虚拟位置粘贴（剪贴板有内容且目标也是虚拟位置）。 */
export function canPasteVirtual() {
  return !!state.virtualClipboard && isVirtualPlace();
}

/** 把剪贴板内容粘贴进当前虚拟位置的**当前文件夹**。
 *  - 同位置剪切：逐条 move（保留 ref_id），完成后清空剪贴板；
 *  - 跨位置剪切：copy_refs 到目标后把源位置里的原引用逐条删除（剪切必须从源消失）；
 *  - 复制：调 copy_refs 复制（生成新 ref_id），剪贴板保留（可多处粘贴）。
 *  返回新增条数。 */
export async function pasteVirtualHere() {
  const cb = state.virtualClipboard;
  if (!cb || !isVirtualPlace()) return 0;
  const destPlace = state.remotePlace;
  const destFolder = state.remoteDir || '';
  const isCut = cb.mode === 'cut';
  let added = 0;
  try {
    if (isCut && cb.placeId === destPlace) {
      for (const f of cb.items) {
        // 同位置移动；目标就是当前目录时跳过（条目已在此）
        if (f.__folder === destFolder) continue;
        await api.virtualMoveRef(destPlace, f.id, destFolder);
        added += 1;
      }
      state.virtualClipboard = null; // 剪切是一次性的
    } else {
      added = await api.virtualCopyRefs({
        sourcePlaceId: cb.placeId,
        destPlaceId: destPlace,
        destFolder,
        refIds: cb.items.map((f) => f.id),
      });
      if (isCut) {
        // 跨位置剪切：复制成功后必须删除源位置里的原引用，否则只是「复制」、
        // 源位置还留着（用户报的 bug）。逐条删；删失败不回滚已复制件，避免丢数据。
        let removed = 0;
        for (const f of cb.items) {
          if (await api.virtualRemoveRef(cb.placeId, f.id).catch(() => false)) removed += 1;
        }
        if (removed !== cb.items.length) state.placeError = i18n.t('virtual.cut_partial');
        state.virtualClipboard = null; // 剪切是一次性的
      }
      // 复制（非 cut）保留剪贴板，可继续粘贴到别处。
    }
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  }
  if (added) {
    await reloadRemoteDir();
    setNotice(i18n.t('virtual.pasted', { n: added }));
  }
  return added;
}

/** 当前 remotePlace 是不是一个虚拟位置（id 以 v 开头且在 virtualPlaces 里）。
 *  虚拟位置复用 remotePlace/remoteDir/remoteItems 那套状态与 PlaceBrowser 视图，
 *  只在加载路径上分叉（走 virtualBrowse 而不是网络 list）。 */
export function isVirtualPlace(id?: string | null) {
  const pid = id ?? state.remotePlace;
  return !!pid && state.virtualPlaces.some((v) => v.id === pid);
}

/** 新建一个虚拟远程位置，只建不进入（返回新 id）。
 *  供「添加到虚拟远程」对话框空态用：那里正在往里加引用，建完不能把用户从当前
 *  真实位置卷走——否则加了一半人就跳到别处去了。 */
export async function createVirtualPlaceOnly(name) {
  const id = await api.virtualCreate(name || i18n.t('virtual.default_name'));
  await reloadRemotePlaces();
  return id;
}

/** 新建一个虚拟远程位置并进入它（侧栏「新建虚拟远程」入口用：用户就是想进去建东西）。 */
export async function createVirtualPlace(name) {
  const id = await createVirtualPlaceOnly(name);
  await openRemotePlace(id);
  return id;
}

export function openUploadToLocal(entries) {
  const items = (Array.isArray(entries) ? entries : [entries])
    .filter((entry) => entry && !entry.is_dir && entry.path)
    .map((entry) => ({ path: entry.path, name: entry.name, size: entry.size ?? 0 }));
  if (!items.length) return;
  state.uploadTo = { source: 'local', items };
}

/** 远程文件复制到另一远程位置。目标名使用远端存储名，不使用解密后的 real_name：
 * 后者可能是用户刻意加密隐藏的信息，把它交给目标服务等于主动泄露。 */
export function openUploadToRemote(entry) {
  if (!entry || entry.is_dir || !state.remotePlace || !entry.id || !entry.size) return;
  state.uploadTo = {
    source: 'remote',
    item: {
      placeId: state.remotePlace,
      path: entry.id,
      name: entry.name,
      size: entry.size,
    },
  };
}

export function cancelUploadTo() {
  state.uploadTo = null;
}

export async function confirmUploadTo(targetPlaceId, targetDir, mode) {
  const pending = state.uploadTo;
  if (!pending || !targetPlaceId) return false;
  if (pending.advancedBatch) {
    const ok = await submitTelegramAdvancedBatch(pending.advancedBatch, {
      kind: 'remote', place_id: targetPlaceId, dir: targetDir || '',
      mode: mode === 'memory' ? 'memory' : 'permanent_cache',
    });
    if (ok) state.uploadTo = null;
    return ok;
  }
  try {
    if (pending.source === 'local') {
      const result = await api.remoteUpload(
        targetPlaceId,
        targetDir || '',
        pending.items.map((item) => item.path),
      );
      const failed = result.filter((row) => !row.ok);
      if (failed.length) {
        state.error = i18n.t('rplace.upload_partial', {
          ok: result.length - failed.length,
          names: failed.map((row) => row.name).join('、'),
        });
        return false;
      }
      setNotice(i18n.tn('rplace.upload_done', result.length));
    } else {
      const item = pending.item;
      await api.remoteCopy({
        source: {
          place_id: item.placeId,
          path: item.path,
          size: item.size,
          name: item.name,
        },
        target_place_id: targetPlaceId,
        target_dir: targetDir || '',
        target_name: item.name,
        mode: mode === 'memory' ? 'memory' : 'permanent_cache',
      });
      state.uploadTo = null;
      openTransfers();
      return true;
    }
    state.uploadTo = null;
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('rplace.upload_failed'));
    return false;
  }
}

/** 打开「添加到虚拟远程」树形选择对话框。entries 是真实位置里选中的一批条目。 */
export function openAddToVirtual(entries) {
  const items = (Array.isArray(entries) ? entries : [entries])
    .filter((e) => e && !e.is_dir)
    .map((e) => ({
      fileId: e.id,
      name: e.real_name || e.name,
      size: e.plaintext_size ?? e.size ?? null,
      mediaTab: e.media_tab ?? null,
    }));
  if (!items.length) return;
  state.addToVirtual = { placeId: state.remotePlace, dirId: state.remoteDir, items };
}

/** 关闭「添加到虚拟远程」对话框。 */
export function cancelAddToVirtual() {
  state.addToVirtual = null;
}

/** 确认把待添加条目加到某个虚拟位置的某个文件夹。 */
export async function confirmAddToVirtual(virtualPlaceId, folder) {
  const req = state.addToVirtual;
  if (!req || !virtualPlaceId) return;
  if (req.advancedBatch) {
    const ok = await submitTelegramAdvancedBatch(req.advancedBatch, {
      kind: 'virtual', place_id: virtualPlaceId, folder: folder || '',
    });
    if (ok) state.addToVirtual = null;
    return;
  }
  let ok = 0;
  for (const it of req.items) {
    try {
      await api.virtualAddRef({
        placeId: virtualPlaceId,
        folder: folder || '',
        sourcePlaceId: req.placeId,
        dirId: req.dirId,
        fileId: it.fileId,
        snapshotName: it.name,
        snapshotSize: it.size,
        // 记住源文件所在分栏，供日后「定位到源文件」直接切到正确的网格
        sourceMediaTab: it.mediaTab ?? null,
      });
      ok += 1;
    } catch (e) {
      state.error = i18n.te(api.errCode(e), 'errors.remote_failed');
    }
  }
  state.addToVirtual = null;
  if (ok > 0) setNotice(i18n.t('virtual.added', { n: ok }));
}

/** 在当前虚拟位置的当前文件夹下新建子文件夹，然后刷新。 */
export async function addVirtualFolder(name?: string | null) {
  if (!isVirtualPlace()) return;
  try {
    await api.virtualAddFolder(state.remotePlace, state.remoteDir, name);
    await reloadRemoteDir();
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), 'errors.remote_failed');
  }
}
