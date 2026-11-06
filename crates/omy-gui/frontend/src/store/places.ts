/** 位置级操作：当前位置高亮、位置浏览器开关、Telegram 位置管理，以及锁定后退出容器的守卫。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import { computed, watch } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import { askPassword } from '../ask';
import { setNotice, state } from './state';
import { reloadRemotePlaces } from './virtual';
import { leaveRemotePlace, openRemotePlace, reloadRemoteDir } from './remote-browse';

/** 当前位置：侧栏高亮的**唯一**真相。
 *
 * 返回 `{ kind: 'local'|'remote'|'transfers', key }`。
 *
 * # 为什么要这个派生值
 *
 * 原来侧栏两处各自判断——本地看 `state.cwd === p.path`、远程看
 * `state.remotePlace === p.id`。而 `state.cwd` 进远程后**不会清空**
 * （也不该清空：退出远程要回到原来那个目录），于是两个条件同时成立，
 * 侧栏同时高亮本地「文档」和远程「Lol」。
 *
 * 不能靠「进远程就清 cwd」来解决：那会让退出远程后落到一个空目录，
 * 把导航弄坏。所以改成从「当前在哪个视图」+「那个视图里选了什么」
 * 算出唯一位置，两边高亮都从它派生。
 *
 * 这样「不会同时亮两个」是**结构上**的保证，而不是靠两处条件恰好互斥
 * ——后者在下一次有人新增视图时会再次失效。
 */
export const activeLocation = computed(() => {
  if (state.transfersOpen) return { kind: 'transfers', key: '' };
  if (state.placeBrowserOpen) {
    return { kind: 'remote', key: state.remotePlace || '' };
  }
  return { kind: 'local', key: state.cwd || '' };
});

/** 打开云盘浏览器（停在位置列表）。 */
export async function openPlaceBrowser() {
  state.transfersOpen = false;
  state.placeBrowserOpen = true;
  await reloadRemotePlaces();
}/** 打开云盘浏览器并直接进入某个已保存位置（桌面侧栏入口）。
 *  必须同时置 placeBrowserOpen，否则只加载了目录数据、视图却还停在本地，
 *  表现为点侧栏云盘项「没反应」。 */
export async function openPlaceBrowserAt(id) {
  state.transfersOpen = false;
  state.placeBrowserOpen = true;
  await openRemotePlace(id);
}

/** 关闭云盘浏览器，回到本地文件，并退出当前位置。 */
/** 打开传输管理页。
 *
 * 它是顶层入口而不是某个位置的子页：任务本身跨对话、跨位置，
 * 挂在某个位置下面的话，用户切走就找不到了。 */
export function openTransfers() {
  state.transfersOpen = true;
  state.placeBrowserOpen = false;
}

/** 关闭传输管理页，回到文件浏览。 */
export function closeTransfers() {
  state.transfersOpen = false;
}

export function closePlaceBrowser() {
  state.placeBrowserOpen = false;
  leaveRemotePlace();
}

/** 退出所有整屏覆盖的视图，回到本地文件浏览。
 *
 * # 为什么要收口在一处
 *
 * 这些视图都是**整屏覆盖**的。少关一个，本地导航就会「已经发生了、
 * 却被盖住」——用户看到的是点了没反应。
 *
 * 原来调用方只关云盘视图（那时只有它），后来加了传输管理页却没跟着改，
 * 于是从传输页点本地位置界面不动。与其在每个调用方列举要关哪些，
 * 不如收在这里：**以后新增整屏视图只改这一处**。
 */
export function leaveOverlays() {
  state.transfersOpen = false;
  closePlaceBrowser();
}

/** 移除一个远程位置；若正在浏览它，先退回到位置列表。 */
export async function removeRemotePlace(id) {
  await api.remotePlaceRemove(id);
  await afterPlaceGone(id);
}

/** 位置消失后的统一收尾：正在浏览它就先退出，然后刷新列表。
 *
 * 三个入口（通用移除、Telegram 摘除、删除账号）共用这一份。分别写的话，
 * 少写一次 leaveRemotePlace 的那个入口会把用户留在一个已经不存在的位置里，
 * 而那时任何操作都只会报「位置不存在」。 */
async function afterPlaceGone(id) {
  if (state.remotePlace === id) leaveRemotePlace();
  await reloadRemotePlaces();
}

/** Telegram：只把位置从列表摘掉，保留本机登录态（之后可直接加回来）。 */
export async function detachTelegramPlace(id) {
  await api.telegramPlaceDetach(id);
  await afterPlaceGone(id);
  setNotice(i18n.t('rplace.detached'));
}

/** Telegram：删除账号，连本机登录态一起清掉。**不可撤销。** */
export async function deleteTelegramAccount(id) {
  await api.telegramPlaceDeleteAccount(id);
  await afterPlaceGone(id);
  setNotice(i18n.t('rplace.account_deleted'));
}

/** Telegram：改这个位置在本机的显示名。
 *
 * 空名字直接不改：留一个没有名字的位置，侧栏上就只剩一个云图标，
 * 多账号时完全分不清谁是谁。 */
export async function renameTelegramPlace(id, name) {
  const trimmed = (name || '').trim();
  if (!trimmed) return;
  await api.telegramPlaceRename(id, trimmed);
  await reloadRemotePlaces();
  setNotice(i18n.t('rplace.renamed'));
}

/** Telegram：显式加密这个位置——**弹密码对话框**让用户现场输密码（与普通文件
 *  加密同款），不再用已解锁 KEK 静默加密。真正的加密在 confirmTgEncrypt 里做。 */
export function encryptTelegramPlace(id) {
  const p = state.remotePlaces.find((x) => x.id === id);
  state.tgEncryptFor = { id, name: p?.name || '' };
}

/** 关掉加密对话框（取消）。 */
export function cancelTgEncrypt() {
  state.tgEncryptFor = null;
  state.tgEncryptBusy = false;
}

/** 用对话框里输的密码真正加密。成功后关框、刷新侧栏加密标识。 */
export async function confirmTgEncrypt({ password, kdf_profile }) {
  const target = state.tgEncryptFor;
  if (!target || !password) return false;
  state.tgEncryptBusy = true;
  try {
    const changed = await api.telegramPlaceEncrypt(target.id, password, kdf_profile || 'moderate');
    setNotice(i18n.t(changed ? 'rplace.encrypted' : 'rplace.already_encrypted'));
    state.tgEncryptFor = null;
    return true;
  } catch (e) {
    setNotice(i18n.te(api.errCode(e), 'errors.tg_encrypt_failed'));
    return false;
  } finally {
    state.tgEncryptBusy = false;
  }
}

/** Telegram：取消加密这个位置，转回默认保护。需当前有能打开它的密码。 */
export async function decryptTelegramPlace(id) {
  try {
    const changed = await api.telegramPlaceDecrypt(id);
    setNotice(i18n.t(changed ? 'rplace.decrypted' : 'rplace.already_plain'));
    return true;
  } catch (e) {
    // 会话里没有能开这个位置的 KEK（它用独立密码加密、当前没解锁过那个密码）：
    // 不是失败，弹框收现场密码走 decrypt_pw。这也是把误加密位置解回来的途径。
    if (api.errCode(e) === 'tg_decrypt_no_key' || api.errCode(e) === 'tg_decrypt_failed') {
      // 密码框必须遮蔽：原来用 window.prompt，输入的密码明文摆在屏幕上
      const pw = await askPassword({ title: i18n.t('rplace.decrypt_pw_prompt') });
      if (pw === null) return false; // 取消
      try {
        const changed = await api.telegramPlaceDecryptPw(id, pw);
        setNotice(i18n.t(changed ? 'rplace.decrypted' : 'rplace.already_plain'));
        return true;
      } catch (e2) {
        // 失败走错误条：setNotice 是成功样式且 4 秒自动消失，用户可能根本没看到
        state.error = i18n.te(api.errCode(e2), 'errors.tg_decrypt_failed');
        return false;
      }
    }
    state.error = i18n.te(api.errCode(e), 'errors.tg_decrypt_failed');
    return false;
  }
}

/** 弹出「解锁这个加密位置」对话框。id 是锁定的 Telegram 位置。 */
export function promptTgUnlock(id) {
  const p = state.remotePlaces.find((x) => x.id === id);
  state.tgUnlockFor = { id, name: p?.name || '' };
  state.tgUnlockError = '';
}

/** 关掉解锁对话框（取消）。 */
export function cancelTgUnlock() {
  state.tgUnlockFor = null;
  state.tgUnlockBusy = false;
  state.tgUnlockError = '';
}

/** 用对话框里输的密码解锁并进入。密码对→进入该位置；错→就地红字提示可重试。 */
export async function confirmTgUnlock(password) {
  const target = state.tgUnlockFor;
  if (!target || !password) return false;
  state.tgUnlockBusy = true;
  state.tgUnlockError = '';
  try {
    const ok = await api.telegramPlaceUnlock(target.id, password);
    if (!ok) {
      state.tgUnlockError = i18n.t('rplace.unlock_wrong');
      return false;
    }
    // 解出登录态后进入该位置：走 openRemotePlace 让它连上并列内容
    state.tgUnlockFor = null;
    await reloadRemotePlaces();
    // 解锁 Telegram 会把同密码的 KEK 装进会话池（并可能为其它 vault 重派生），
    // 必须刷新右上角「N 个密码已解锁」，否则停留在解锁前的「未输入密码」。
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await openRemotePlace(target.id);
    setNotice(i18n.t('rplace.unlocked'));
    return true;
  } catch (e) {
    // 密码错后端返回 tg_unlock_wrong；其余错误给通用提示
    state.tgUnlockError = api.errCode(e) === 'tg_unlock_wrong'
      ? i18n.t('rplace.unlock_wrong')
      : i18n.te(api.errCode(e), 'errors.remote_failed');
    return false;
  } finally {
    state.tgUnlockBusy = false;
  }
}

/** 进入远程子目录。 */
export async function enterRemoteDir(id: string, name?: string) {
  state.remoteDir = id;
  state.remoteSelected = [];
  state.remoteMessageSelected = [];
  state.telegramForward = null;
  // 换对话/回根都回到默认媒体栏：留着上个对话选的栏，会让用户以为
  // 新对话「只有链接」之类（其实是停在链接栏且新对话没链接）
  state.remoteTab = 'media';
  // 记下显示名。拿不到就留空，由面包屑那边决定怎么兜底——
  // 不要在这里回落到 id，那样面包屑就无从区分「有名字」和「没名字」了
  state.remoteDirName = name || '';
  // 上报用户动作（不带对话名/id 明文，只记"进目录"还是"回根"）
  api.uiLog(id ? 'enter-dir' : 'enter-root');
  await reloadRemoteDir();
}

/** 远程位置里返回上一级。
 *
 * 已在根时离开该位置回到本地，而不是什么都不做——否则用户会觉得
 * 返回键卡住了。
 */
export async function remoteGoUp() {
  const cur = state.remoteDir.replace(/\/+$/, '');
  if (!cur) {
    leaveRemotePlace();
    return;
  }
  // 上一级怎么算，取决于 provider 的 id 形状——不能一律按 '/' 切。
  //
  // 这是本文件里第四处「写死 WebDAV 路径形状」的地方（前三处：kindLabel
  // 硬写 WebDAV、remote_probe_entry 按 '/' 猜名字、crumbs 按 '/' 切段）。
  // Telegram 的 dir 是 `tg:<对话>`，里面没有斜杠，lastIndexOf 返回 -1，
  // 于是直接置空回到对话列表——**结果碰巧是对的**（对话只有一层），
  // 但那靠的是「没有斜杠」这个副作用，不是「这个 provider 有几层」。
  //
  // 而且它绕过了 enterRemoteDir，于是不会清 remoteDirName /
  // remoteViewMode / remoteMessages：从对话返回后面包屑还挂着上一个
  // 对话的名字。所以这里统一走 enterRemoteDir，让复位只有一处。
  const i = cur.lastIndexOf('/');
  const parent = i > 0 ? cur.slice(0, i) : '';
  // 回到根（对话列表）时名字也要空，否则面包屑会留着上一层的名字
  await enterRemoteDir(parent, '');
}

/** 添加远程位置后刷新列表并进入它。 */
export async function afterPlaceAdded(id) {
  await reloadRemotePlaces();
  await openRemotePlace(id);
}

// 凭据归零就退出容器视图，不依赖某处记得清状态。
//
// 容器视图里列的是**解密出来的文件名**——锁定的语义是「这些都不该再
// 看得见」。`lock()` 确实清了，但那是一处容易在重构中被漏掉的赋值；
// 这里再守一道，让「锁定即不可见」不依赖某一行代码没被删掉。
// 这与原先 ContainerPanel 自己监听凭据数是同一个道理，面板没了，
// 这道保证要跟着搬过来而不是丢掉。
watch(
  () => state.credentials,
  (n) => {
    if (n === 0) state.container = null;
  },
);

/** 补齐某个已解锁文件的媒体元信息。 */
export async function enrich(entryId) {
  try {
    return await api.enrichFile(entryId);
  } catch {
    return null;
  }
}
