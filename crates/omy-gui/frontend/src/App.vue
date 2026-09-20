<script setup>
/** 根组件。
 *
 * 负责编排四件事：文件管理器主体、加密对话框、解锁对话框、预览层。
 *
 * # 双击一个条目会发生什么
 *
 * - 目录 → 进去
 * - 未加密文件，能在应用内看的（图片/视频/音频/文本）→ 应用内预览
 * - 未加密文件，看不了的（PDF、压缩包、Office…）→ 交给系统默认程序
 * - 加密文件且已解锁 → 应用内预览
 * - 加密文件但锁着 → 弹密码框
 *
 * 未加密文件走的是 `omystream://localhost/plain/<token>`，与加密文件
 * **共用同一个预览组件**。这不是为了省代码，是为了让两者的播放行为
 * 不可能产生差异——分成两套的话，迟早出现「加密的能拖进度条、
 * 没加密的反而不能」这种荒唐事。
 *
 * 读未加密文件不产生任何新的明文：它本来就以明文躺在磁盘上。
 * 这与文档 §3 的 L2（临时解密文件）是两回事，那条针对的是把
 * 加密内容解出来写到磁盘。
 *
 * 这条分支写在这里而不是散在组件里，因为它是**交互策略**，
 * 改动频繁且需要一眼看全。
 */

import { ref, computed, onMounted, onBeforeUnmount } from 'vue';
import * as api from './api.js';
import * as i18n from './i18n.js';
import {
  state,
  navigate,
  reload,
  setNotice,
  loadPlaces,
  ensureRemoteListeners,
  restoreStartupDir,
  loadStorageAccess,
  grantStorageAccess,
  encryptSelected,
  restoreSelected,
  tryDeviceUnlock,
  tryUnlock,
  lock,
  switchLanguage,
  applyLanguage,
  afterPlaceAdded,
  reloadRemotePlaces,
  openPlaceBrowser,
  closePlaceBrowser,
  selectAll,
  enrich,
  encryptable,
  restorable,
  refreshDeviceOverview,
  connectRemote,
  tryUnlockRemote,
  openWithSystem,
  enterContainer,
  enterContainerDir,
  clearRetry,
  manageKey,
  generateRecovery,
  restoreWithRecovery,
  retryKeyFiles,
  ctxMenu,
  recoveryGeneratable,
  recoveryUsable,
  ctxItems,
  openContextMenu,
  closeContextMenu,
  doDelete,
  doRename,
  doCreateFolder,
  // 右键菜单的「密码管理」与「在文件管理器中显示」要用这两个。
  // 漏掉它们不会有构建错误，只在点菜单那一刻抛 ReferenceError，
  // 而 onCtxPick 的异常没人接——表现就是「点了完全没反应」。
  keyManageable,
  revealEntry,
  closeTransfers,
} from './store.js';
import MainScreen from './components/MainScreen.vue';
import EncryptDialog from './components/EncryptDialog.vue';
import RestoreDialog from './components/RestoreDialog.vue';
import UnlockDialog from './components/UnlockDialog.vue';
import ContextMenu from './components/ContextMenu.vue';
import KeyDialog from './components/KeyDialog.vue';
import RecoveryDialog from './components/RecoveryDialog.vue';
import NameDialog from './components/NameDialog.vue';
import PreviewOverlay from './components/PreviewOverlay.vue';
import DevicePanel from './components/DevicePanel.vue';
import RemoteScreen from './components/RemoteScreen.vue';
import PlaceBrowser from './components/PlaceBrowser.vue';
import TransferScreen from './components/TransferScreen.vue';
import SettingsDialog from './components/SettingsDialog.vue';
import RemotePlaceDialog from './components/RemotePlaceDialog.vue';
import TelegramLoginDialog from './components/TelegramLoginDialog.vue';
import { initAutoLock, configureAutoLock } from './autolock.js';

/** 设置对话框是否打开。 */
const showSettings = ref(false);
/** 添加远程位置对话框是否打开。 */
const showAddPlace = ref(false);
/** Telegram 扫码登录对话框是否打开。 */
const showTelegramLogin = ref(false);

/** 扫码登录结束。
 *
 * 登录态是否落盘要如实转达：存不住时用户下次打开又要扫码，不说清楚
 * 他会以为程序把他登出了。
 */
async function onTelegramDone({ sessionSaved, proxyUrl }) {
  showTelegramLogin.value = false;
  setNotice(sessionSaved ? i18n.t('tg.session_saved') : i18n.t('tg.session_not_saved'));
  // 登录完就把它接成一个远程位置并进去。
  //
  // 不停在「登录成功」那句提示上：用户扫码是为了看里面的文件，
  // 停在提示上等于让他自己去找入口，而那个入口刚刚才出现。
  try {
    state.placeBrowserOpen = true;
    const id = await api.telegramPlaceConnect(proxyUrl);
    await afterPlaceAdded(id);
  } catch (e) {
    // 按错误码给话，不要把 Error 对象丢进 te()——那会落到通用「内部错误」，
    // 把「没登录态 / 登录态失效 / 网络不通」三种抹平成一句，
    // 而它们要引导用户做的事完全不同
    setNotice(i18n.te(api.errCode(e), i18n.t('tg.connect_failed')));
  }
}

/** 远程位置添加成功：刷新列表并直接进去。
 *
 * 不留在原地——用户刚填完一串地址和密码，想看到的是里面有什么，
 * 而不是回到一个看不出有没有添加成功的界面。
 */
async function onPlaceAdded(id) {
  showAddPlace.value = false;
  // 从桌面侧栏直接添加时浏览器可能还没开，这里保证添加后落在云盘视图里
  state.placeBrowserOpen = true;
  await afterPlaceAdded(id);
}

/** 设置页里改了语言：立刻加载对应语言包。
 *
 * 不等关闭后再统一应用——用户选完语言却看不到界面变化，
 * 无法确认自己选对了。
 */
async function onSettingsLang(pref) {
  await applyLanguage(pref);
}

/** 设置页关闭：重新应用配置。
 *
 * 不这样的话，改了自动锁定超时要等重启才生效——用户会以为没保存住。
 */
async function onSettingsClose() {
  showSettings.value = false;
  await applySettings();
}

/** 安全页「立即锁定」：关掉设置弹窗，再走统一锁定收尾（关预览、清会话）。 */
async function onSettingsLock() {
  showSettings.value = false;
  await doLock();
  await applySettings();
}

/** 设备页「管理」：关掉设置弹窗、打开完整设备面板，并让本页改动即时生效。 */
function onSettingsDevices() {
  showSettings.value = false;
  showDevices.value = true;
  applySettings();
}

const showEncrypt = ref(false);
const showRestore = ref(false);
/** 密码管理的目标条目；null 表示对话框关着。
 *
 * 存条目本身而不是一个布尔量：对话框要显示改的是哪个文件，
 * 而「当前选中项」在对话框打开期间可能被别处改掉。
 */
const keyTarget = ref(null);
/** 密码管理的错误单独存，与解锁框的 error 分开。 */
const keyError = ref('');
const keyErrorFiles = ref([]);
const showUnlock = ref(false);
const showDevices = ref(false);
const unlockError = ref('');
const previewEntry = ref(null);

/** 远端预览目标。与本机预览分开存：两者的对象结构不同，
 * 混在一个 ref 里迟早会出现「用本机的字段去读远端对象」。 */
const remotePreview = ref(null);
/** 容器内文件的预览目标。
 *
 * 与明文预览分开存：URL 前缀不同（`/citem/` 要密钥，`/plain/` 不要），
 * 而且这个不能给「用外部应用打开」——那需要磁盘上的一个真实文件，
 * 容器内的条目没有。混在一起就会出现一个点了没反应的按钮。
 */
const citemPreview = ref(null);
/** 当前密码框是给远端用的还是本机用的。 */
const unlockForRemote = ref(false);
/** 未加密文件的预览目标。
 *
 * 与加密文件的 `previewEntry` 分开存：后者要走 `state.known` 查
 * 媒体元信息，而明文文件根本没有那份数据。混在一起会写出
 * 「用加密文件的字段去读明文对象」这类必然出错的代码。
 */
const plainPreview = ref(null);

/** 云盘（远程位置）文件的预览目标。
 *
 * `id` 是后端 remotePlaceOpen 颁发的播放 token（pf{n}），不是文件 id；
 * 预览组件据此拼 `/pfile/<token>`。关闭时必须 remotePlaceClose 释放句柄，
 * 否则后端来源表会一直挂着这个文件（密文缓存仍保留，那是刻意的）。
 */
const placePreview = ref(null);

/** 在云盘里双击/单击一个已解锁的加密文件：向后端换播放令牌再预览。
 *
 * 三态分开处理：拿到 token 才弹预览；`unlocked:false`（密码不在当前会话）
 * 与 `not_encrypted`（根本不是 omy）给不同提示，不能笼统报「打不开」。
 */
async function onOpenPlace(f) {
  // 打开播放必须知道密文总大小（Range 边界、片尾判断都靠它）。
  // 目录不会走到这里；文件大小缺失说明 PROPFIND 信息不全，直接提示而非
  // 给后端传 null 造成参数反序列化失败。
  const size = Number(f.size);
  if (!Number.isFinite(size) || size <= 0) {
    state.error = i18n.t('rplace.open_failed');
    return;
  }
  try {
    // 名字要一起传：普通文件的 MIME 靠它推，而 Telegram 的 id
    // （tg:<对话>:<消息>）里没有扩展名
    const r = await api.remotePlaceOpen(
      state.remotePlace, f.id, size, f.real_name || f.name);
    if (r.token) {
      placePreview.value = {
        id: r.token,
        name: r.name || f.real_name || f.name,
        kind: r.kind || 'other',
        mime: r.mime || '',
      };
      return;
    }
    // 走到这里说明没拿到 token。普通文件现在也会有 token，
    // 所以 not_encrypted 已不再是「打不开」的理由——真到这一步
    // 多半是后端登记失败
    state.error = r.not_encrypted
      ? i18n.t('rplace.open_failed')
      : i18n.t('rplace.err_locked');
  } catch (e) {
    state.error = i18n.te(api.errCode(e), 'rplace.open_failed');
  }
}

/** 关闭云盘预览：先释放后端来源句柄，再清前端目标。 */
async function onClosePlacePreview() {
  const token = placePreview.value?.id;
  placePreview.value = null;
  if (token) {
    try {
      await api.remotePlaceClose(token);
    } catch {
      // 释放失败不影响关闭：锁定时后端会统一清句柄
    }
  }
}

/** 退出云盘视图回本地：先关掉可能开着的云盘预览（含释放句柄）。 */
async function onClosePlaceBrowser() {
  if (placePreview.value) await onClosePlacePreview();
  closePlaceBrowser();
}

/** 侧栏/底部「远程」入口：打开云盘浏览器（位置列表或当前位置）。 */
async function onPlacesEntry() {
  await openPlaceBrowser();
}

/** 预览目标的完整信息（含媒体元数据）。 */
const previewFile = computed(() => {
  const e = previewEntry.value;
  if (!e || !e.entry_id) return null;
  const meta = state.known[e.path];
  if (!meta || !meta.unlocked) return null;
  return meta;
});

/** 双击一个条目。 */
async function onOpen(entry) {
  // 容器内的条目：目录在容器里往下走，文件走容器专用的预览 URL。
  // 这一支必须在最前面——下面几支都以「有磁盘路径」为前提
  if (entry.in_container) {
    if (entry.is_dir) {
      enterContainerDir(entry.path);
      return;
    }
    openContainerItem(entry);
    return;
  }
  if (entry.is_dir) {
    await navigate(entry.path);
    return;
  }
  if (!entry.is_encrypted) {
    await openPlain(entry);
    return;
  }
  if (entry.unlocked && entry.entry_id) {
    // 容器是一整个文件夹，载荷为多个文件拼接。当成单个文件预览
    // 只会得到一堆首尾相接的字节，所以先分流出去——像进普通文件夹
    // 那样进入它，而不是弹一个另一套交互的面板
    if (entry.is_container) {
      await enterContainer(entry);
      return;
    }
    await openPreview(entry);
    return;
  }
  // 锁着：先看看会话里是不是已经有能开它的密钥
  const probe = await api.probeOne(entry.path).catch(() => null);
  if (probe?.unlocked) {
    // 会话里有密钥但列表还没刷新，刷一下再开
    await tryRefreshThenPreview(entry);
    return;
  }
  unlockError.value = '';
  refreshDeviceKey();
  showUnlock.value = true;
}

/** 打开一个未加密文件。
 *
 * 能在应用内看的就内嵌预览，其余交给系统默认程序——这是普通文件
 * 管理器的行为，用户对它有稳定预期。
 */
async function openPlain(entry) {
  state.selected = [entry.path];
  if (!entry.token) return;

  // preview 由后端算好（`mime.rs`），前端不再按后缀猜。
  // 判定规则会随浏览器支持情况变化，散在两处早晚不一致
  if (entry.preview && entry.preview !== 'other') {
    plainPreview.value = {
      id: entry.token,
      name: entry.name,
      kind: entry.preview,
      mime: entry.mime,
    };
    return;
  }
  await openWithSystem(entry);
}

/** 预览容器内的一个文件。
 *
 * 与容器外的文件走**同一个** PreviewOverlay：加密文件、远端文件、
 * 磁盘明文、容器内文件四种来源共用一个组件，差别只有 URL 前缀。
 * 这样「容器里的视频能不能拖进度条」不可能与容器外不一致。
 *
 * 应用内看不了的类型（PDF、压缩包…）会落到预览层的兜底分支，显示
 * 「此格式无法在应用内预览」。这里**不能**退回「用系统程序打开」——
 * 那要求磁盘上有个真实文件，而容器内的条目只是载荷里的一段区间；
 * 真要支持得先解出一个临时文件，那正是本项目要避免的明文落盘。
 */
function openContainerItem(entry) {
  if (!entry.token) {
    state.error = i18n.te('container_failed');
    return;
  }
  state.selected = [entry.path];
  citemPreview.value = {
    id: entry.token,
    name: entry.name,
    kind: entry.preview,
    mime: entry.mime,
  };
}

async function openPreview(entry) {
  previewEntry.value = entry;
  // 媒体元信息按需补：预览时才需要 tier / 时长 / 尺寸
  if (entry.entry_id) {
    const updated = await enrich(entry.entry_id);
    if (updated) state.known[entry.path] = updated;
  }
}

async function tryRefreshThenPreview(entry) {
  await reload();
  const fresh = state.entries.find((e) => e.path === entry.path);
  if (fresh?.unlocked) await openPreview(fresh);
}

async function onRestoreSubmit(opts) {
  const r = await restoreSelected(opts);
  // 只在完全成功时关闭。部分失败（典型是 target_exists）时留着对话框，
  // 用户勾一下「覆盖」或换个目录就能重试——关掉的话他得从选文件重来
  if (r && !r.failed.length) showRestore.value = false;
}

async function onEncryptSubmit(opts) {
  const r = await encryptSelected(opts);
  if (r) showEncrypt.value = false;
}

/** 查询槽位清单。交给对话框自己处理失败——用户可能只是密码还没打完。 */
function onQuerySlots(path, password) {
  return api.listSlots(path, password);
}

/**
 * 精确删除一个槽位。
 *
 * 与 onKeySubmit 分开：它删完不关对话框——用户往往要连删几个，
 * 每删一个就关掉再重开一遍太难用。
 */
async function onRemoveSlot({ path, current, slotIndex, kind }) {
  const label = i18n.t(`keymgmt.slot_kind_${kind}`);
  if (!window.confirm(i18n.t('keymgmt.slot_remove_confirm', { index: slotIndex, kind: label }))) {
    return;
  }
  keyError.value = '';
  const ok = await manageKey({
    path,
    action: 'remove',
    current,
    next: '',
    slot_index: slotIndex,
  });
  if (ok) {
    setNotice(i18n.t('keymgmt.ok_remove_slot'));
  } else {
    keyError.value = state.error;
    // 错误已经显示在对话框里，不要再占用底部提示条重复一遍
    state.error = '';
  }
}

async function onKeySubmit(req) {
  const r = await manageKey(req);
  if (r) {
    keyTarget.value = null;
    keyError.value = '';
  } else {
    keyError.value = state.error;
    // 部分失败时后端会带上是哪些文件。挂在对话框上而不是底部提示条：
    // 提示条 4 秒后自动消失，用户还没读完就没了
    keyErrorFiles.value = state.errorDetails;
    // 错误已经在对话框里显示，不要再占用底部提示条重复一遍
    state.error = '';
  }
}

/** 关闭密钥对话框。顺手清掉重试上下文——里面存着密码，没有再留的理由。 */
function onKeyCancel() {
  keyTarget.value = null;
  keyErrorFiles.value = [];
  clearRetry();
}

/** 只重试上次失败的那些文件。 */
async function onKeyRetry() {
  const r = await retryKeyFiles();
  // 全成了才关对话框。仍有失败就留着，让用户看清还剩哪些——
  // 关掉的话那份清单就没了，用户只能重新走一遍整个操作才知道
  if (r) {
    keyTarget.value = null;
    keyError.value = '';
    keyErrorFiles.value = [];
    return;
  }
  // 失败时两个都要更新。只更新清单的话，对话框里显示的还是**上一次**的
  // 错误文案，而清单已经换成这次的——界面上出现「有错误、但没有失败
  // 文件」这种自相矛盾的状态，用户完全判断不出这次重试到底怎么了
  keyError.value = state.error;
  keyErrorFiles.value = state.errorDetails;
  // 错误已经在对话框里显示，不要再占用底部提示条重复一遍
  state.error = '';
}

/* ---------------- 恢复码 ---------------- */

/**
 * 恢复码对话框状态。null 表示没开。
 *
 * 结构 { entry, mode, words, mayHaveEvicted }。words 放在这里而不是
 * 组件内部，是为了让「关闭对话框」这一个动作就能确保词从内存消失——
 * 组件卸载时状态跟着没，不依赖组件自己记得清理。
 */
const recoveryDlg = ref(null);
const recoveryError = ref('');

/** 生成恢复码。 */
async function onRecoveryGenerate(req) {
  recoveryError.value = '';
  const r = await generateRecovery(req);
  if (!r) {
    recoveryError.value = state.error;
    state.error = '';
    return;
  }
  // 切到「抄写」态，同一个对话框继续用：换一个框会让用户以为操作
  // 已经结束、词只是附带信息，而这恰恰是最需要他停下来的一步
  recoveryDlg.value = {
    ...recoveryDlg.value,
    words: r.words,
    mayHaveEvicted: r.may_have_evicted,
  };
}

/** 用恢复码重设密码。 */
async function onRecoveryRestore(req) {
  recoveryError.value = '';
  const r = await restoreWithRecovery(req);
  if (!r) {
    recoveryError.value = state.error;
    state.error = '';
    return;
  }
  recoveryDlg.value = null;
  setNotice(i18n.t('recovery.restored_toast'));
  await reload();
}

/** 关闭恢复码对话框。
 *
 * 生成态下关闭要额外提示一句：词到此为止，不会再出现。这是唯一
 * 一处「关掉窗口就丢东西」的地方，沉默关闭太容易让人以为还能找回。
 */
function onRecoveryClose() {
  const hadWords = recoveryDlg.value?.words?.length > 0;
  recoveryDlg.value = null;
  recoveryError.value = '';
  if (hadWords) setNotice(i18n.t('recovery.generated_toast'));
}

/* ---------------- 右键菜单 ---------------- */

/** 命名对话框：null 表示没开，否则 { mode, initial, path }。 */
const nameDlg = ref(null);

function onEntryMenu(payload) {
  openContextMenu(payload);
}

/** 菜单项被点。
 *
 * 先关菜单再执行：删除会触发 reload，列表重排后菜单会悬在一个已经不存在
 * 的条目上方。
 */
async function onCtxPick(key) {
  const entry = ctxMenu.entry;
  closeContextMenu();
  if (!entry) return;

  switch (key) {
    case 'open':
      await onOpen(entry);
      break;
    case 'encrypt':
      showEncrypt.value = true;
      break;
    case 'restore':
      showRestore.value = true;
      break;
    case 'manage-key':
      keyError.value = '';
      keyTarget.value = keyManageable.value;
      break;
    case 'recovery-generate':
      recoveryError.value = '';
      recoveryDlg.value = {
        entry: recoveryGeneratable.value,
        mode: 'generate',
        words: [],
        mayHaveEvicted: false,
      };
      break;
    case 'recovery-restore':
      recoveryError.value = '';
      recoveryDlg.value = {
        entry: recoveryUsable.value,
        mode: 'restore',
        words: [],
        mayHaveEvicted: false,
      };
      break;
    case 'rename':
      // 用真名做初始值：加密文件在磁盘上叫一串十六进制，
      // 拿那个当初始值等于让用户从头输
      nameDlg.value = {
        mode: 'rename',
        initial: entry.real_name || entry.name,
        path: entry.path,
      };
      break;
    case 'newfolder':
      nameDlg.value = { mode: 'newfolder', initial: '', path: '' };
      break;
    case 'trash':
      await doDelete(true);
      break;
    case 'delete':
      await doDelete(false);
      break;
    case 'reveal':
      await revealEntry(entry);
      break;
    default:
      break;
  }
}

async function onNameSubmit(name) {
  const d = nameDlg.value;
  if (!d) return;
  const ok =
    d.mode === 'rename' ? await doRename(d.path, name) : await doCreateFolder(name);
  // 失败时保留对话框：名字冲突或非法时用户要改的正是这个输入框，
  // 关掉他得从右键菜单重新走一遍
  if (ok) nameDlg.value = null;
}

async function onUnlockSubmit({ password }) {
  const ok = unlockForRemote.value
    ? await tryUnlockRemote(password)
    : await tryUnlock(password);
  if (ok) {
    showUnlock.value = false;
    unlockForRemote.value = false;
    unlockError.value = '';
  } else {
    unlockError.value = state.error;
    // 错误已在对话框里显示，不用再占用底部提示条
    state.error = '';
  }
}

async function onPick() {
  // 不能静默吞错误：移动端根本没有文件夹选择器，后端会返回
  // unsupported。吞掉的话点了完全没反应，用户只会以为按钮坏了
  let dir = null;
  try {
    dir = await api.pickFolder(i18n.t('nav.pick_folder'));
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return;
  }
  if (dir) {
    // 同上：用户挑了个本地文件夹，就该落到本地视图，
    // 不能把它载入到一个被云盘视图盖住的地方
    closePlaceBrowser();
    await navigate(dir);
  }
}

async function doLock() {
  previewEntry.value = null;
  remotePreview.value = null;
  // 明文预览也要关：后端 lock 会清空 token 表，
  // 留着的话画面会突然变成加载失败，很莫名其妙
  plainPreview.value = null;
  // 容器内文件的预览同样要关，理由更强：那是解密出来的内容。
  // 容器视图本身由 store 的 lock() 与一道 watch 一起收掉
  citemPreview.value = null;
  // 云盘播放预览同样要关：后端 lock 已清空远程来源句柄，留着只会 403。
  // 云盘目录浏览本身保留——锁定后仍可看目录，只是文件显示为锁定、点不开
  placePreview.value = null;
  showDevices.value = false;
  // 断开远端由**后端**的 lock 负责，这里不再重复调用。
  // 早先版本在这里调 disconnectRemote()，实测发现绕过这段前端代码
  // 的调用会留下活连接——保证放在后端才成立，放在这里只是碰巧生效。
  await lock();
}

/** 从设备面板发起连接。 */
async function onConnectRemote({ fingerprint, addr }) {
  showDevices.value = false;
  await connectRemote(fingerprint, addr);
}

/** 双击一个远端文件。锁着的不给开——预览需要密钥。 */
function onOpenRemote(f) {
  if (f.unlocked) remotePreview.value = f;
}

/** 在云盘位置里双击了一个锁着的 omy 文件：弹解锁框。
 *
 * 复用本地那一套 UnlockDialog，不另做一个。走 tryUnlock 而不是
 * tryUnlockRemote——后者是给局域网对端视图用的，云盘位置由
 * tryUnlock 内部分流到 tryUnlockRemotePlace。
 */
function onRemotePlaceUnlock() {
  unlockForRemote.value = false;
  unlockError.value = '';
  refreshDeviceKey();
  showUnlock.value = true;
}

/** 在远端视图里点「试密码」。 */
function onRemoteUnlock() {
  unlockForRemote.value = true;
  unlockError.value = '';
  refreshDeviceKey();
  showUnlock.value = true;
}

/** 关掉设备面板时刷新概览：面板里可能配了新设备或开了共享。 */
async function onDevicePanelClose() {
  showDevices.value = false;
  await refreshDeviceOverview();
}

/** 预览层里点「用外部应用打开」。 */
async function onPlainExternal() {
  const cur = state.entries.find((e) => e.token === plainPreview.value?.id);
  plainPreview.value = null;
  if (cur) await openWithSystem(cur);
}

/**
 * 当前位置能不能用 Windows Hello 免密解锁。
 *
 * 两个条件同时成立才为 true：这台电脑支持、这个库已启用。缺一个就不显示
 * 按钮——摆一个点了就报错的按钮比没有更糟。
 */
const deviceKeyReady = ref(false);

/**
 * 刷新免密解锁的可用状态。
 *
 * 在弹出解锁对话框之前查，而不是进目录就查：后者会在每次切目录时多一次
 * 无用的 IO，而这个状态只在要解锁的那一刻才有意义。
 */
async function refreshDeviceKey() {
  deviceKeyReady.value = false;
  if (!state.cwd) return;
  try {
    const s = await api.deviceKeyStatus(state.cwd);
    deviceKeyReady.value = s.available && s.enrolled;
  } catch {
    // 查不到就当不可用。不弹错：用户只是想解锁，
    // 「免密解锁状态查询失败」这种话对他没有意义
    deviceKeyReady.value = false;
  }
}

async function onDeviceUnlock() {
  const ok = await tryDeviceUnlock();
  if (ok) {
    showUnlock.value = false;
    unlockError.value = '';
  } else {
    unlockError.value = state.error;
    // 错误已在对话框里显示，不用再占用底部提示条
    state.error = '';
  }
}

function onUnlockCancel() {
  showUnlock.value = false;
  unlockForRemote.value = false;
}

function onKey(e) {
  // Ctrl/Cmd+L 锁定（文档 §8）
  if ((e.ctrlKey || e.metaKey) && e.key === 'l') {
    e.preventDefault();
    if (state.credentials > 0) doLock();
    return;
  }
  // Ctrl/Cmd+A 全选
  if ((e.ctrlKey || e.metaKey) && e.key === 'a' && !previewEntry.value) {
    const tag = document.activeElement?.tagName;
    if (tag === 'INPUT' || tag === 'TEXTAREA') return;
    e.preventDefault();
    selectAll();
  }
}

/** 回到前台时重查存储权限。
 *
 * 安卓上用户可能在设置里把权限关掉再切回来，也可能是从我们跳出去的
 * 设置页返回。两种情况应用都收不到通知，只能在 resume 时重查。
 *
 * 只在状态**变化**时刷侧栏：每次切前台都无条件重列会让侧栏闪一下，
 * 而绝大多数切换其实什么都没变。
 */
async function onVisible() {
  if (document.visibilityState !== 'visible') return;
  if (state.storage.mode === 'not-applicable') return;
  const before = state.storage.granted;
  await loadStorageAccess();
  if (state.storage.granted !== before) await loadPlaces();
}

onMounted(async () => {
  document.addEventListener('keydown', onKey);
  document.addEventListener('visibilitychange', onVisible);
  // 必须先查权限：list_places 的结果取决于授权状态，顺序颠倒会让
  // 首屏侧栏停在「未授权」的那份列表上，直到下一次刷新才对
  await loadStorageAccess();
  await loadPlaces();
  await reloadRemotePlaces();
  // 远程「边扫边出」事件监听注册一次即可，不必阻塞首屏
  void ensureRemoteListeners();
  await refreshDeviceOverview();

  // 自动锁定按配置启动。放在最后：它依赖配置读取，而前面几步
  // 都是界面首屏需要的，不该为它推迟
  initAutoLock();
  await applySettings();
  // 起始目录放最后：它依赖 places 已加载（home 模式）且只在启动时跑一次，
  // 设置保存触发的 applySettings 不应把用户拽回起始目录
  await restoreStartupDir();
});

/** 读配置并应用到运行时。
 *
 * 设置页保存后也要调一次，否则改了超时要等到重启才生效——
 * 用户会以为设置没保存住。
 */
async function applySettings() {
  try {
    const c = await api.configGet();
    configureAutoLock(c.security);
    // 默认视图也在这里应用：它存在配置里，但界面启动时用的是
    // store 的初值，不读一次就永远是网格
    if (c.ui?.view === 'grid' || c.ui?.view === 'list') state.view = c.ui.view;
    // 缩略图开关：关闭后列表不再渲染缩略图 <img>，也就不发 /thumb 请求
    state.showThumbnails = c.ui?.thumbnails !== false;
  } catch {
    // 配置读不出来就用内置默认值：这不该阻止应用启动
  }
}
onBeforeUnmount(() => {
  document.removeEventListener('keydown', onKey);
  document.removeEventListener('visibilitychange', onVisible);
});
</script>

<template>
  <RemoteScreen
    v-if="state.remoteMode"
    @open="onOpenRemote"
    @unlock="onRemoteUnlock"
  />

  <!-- 云盘（WebDAV 等远程位置）：位置列表 + 目录浏览 + 点播。
       与局域网对端 RemoteScreen 互斥，二者都不在时才是本地文件。 -->
  <TransferScreen
    v-else-if="state.transfersOpen"
    @pick="onPick"
    @devices="showDevices = true"
    @lang="switchLanguage"
    @telegram="showTelegramLogin = true"
    @settings="showSettings = true"
    @lock="doLock"
    @quick-unlock="((unlockError = ''), (showUnlock = true))"
    @files="closeTransfers()"
    @places="closeTransfers()"
  />

  <PlaceBrowser
    v-else-if="state.placeBrowserOpen"
    @need-unlock="onRemotePlaceUnlock"
    @open="onOpenPlace"
    @close="onClosePlaceBrowser"
    @pick="onPick"
    @devices="showDevices = true"
    @lang="switchLanguage"
    @telegram="showTelegramLogin = true"
    @settings="showSettings = true"
    @lock="doLock"
    @quick-unlock="((unlockError = ''), (showUnlock = true))"
  />

  <MainScreen
    v-else
    @open="onOpen"
    @menu="onEntryMenu"
    @encrypt="showEncrypt = true"
    @restore="showRestore = true"
    @manage-key="((keyError = ''), (keyTarget = $event))"
    @lock="doLock"
    @quick-unlock="((unlockError = ''), (showUnlock = true))"
    @pick="onPick"
    @lang="switchLanguage"
    @devices="showDevices = true"
    @settings="showSettings = true"
    @add-place="showAddPlace = true"
    @telegram="showTelegramLogin = true"
    @places="onPlacesEntry"
  />

  <DevicePanel
    v-if="showDevices"
    @close="onDevicePanelClose"
    @connect="onConnectRemote"
  />

  <RestoreDialog
    v-if="showRestore"
    :targets="restorable"
    :current-dir="state.cwd"
    :busy="state.busy"
    @cancel="showRestore = false"
    @submit="onRestoreSubmit"
  />

  <EncryptDialog
    v-if="showEncrypt"
    :targets="encryptable"
    :busy="state.busy"
    @cancel="showEncrypt = false"
    @submit="onEncryptSubmit"
  />

  <KeyDialog
    v-if="keyTarget"
    :entry="keyTarget"
    :is-tree="!!keyTarget.is_encrypted_dir"
    :error-files="keyErrorFiles"
    :retry-count="state.retry ? state.retry.paths.length : 0"
    :busy="state.busy"
    :error="keyError"
    :query-slots="onQuerySlots"
    @cancel="onKeyCancel"
    @retry="onKeyRetry"
    @submit="onKeySubmit"
    @remove-slot="onRemoveSlot"
  />

  <RecoveryDialog
    v-if="recoveryDlg && recoveryDlg.entry"
    :entry="recoveryDlg.entry"
    :mode="recoveryDlg.mode"
    :words="recoveryDlg.words"
    :may-have-evicted="recoveryDlg.mayHaveEvicted"
    :busy="state.busy"
    :error="recoveryError"
    @cancel="onRecoveryClose"
    @done="onRecoveryClose"
    @generate="onRecoveryGenerate"
    @restore="onRecoveryRestore"
  />
  <ContextMenu
    v-if="ctxMenu.entry"
    :items="ctxItems"
    :x="ctxMenu.x"
    :y="ctxMenu.y"
    @pick="onCtxPick"
    @close="closeContextMenu"
  />

  <NameDialog
    v-if="nameDlg"
    :mode="nameDlg.mode"
    :initial="nameDlg.initial"
    :busy="state.busy"
    @cancel="nameDlg = null"
    @submit="onNameSubmit"
  />

  <UnlockDialog
    v-if="showUnlock"
    :busy="state.busy"
    :error="unlockError"
    :loaded="state.credentials"
    :device-key="deviceKeyReady"
    @cancel="onUnlockCancel"
    @submit="onUnlockSubmit"
    @device-unlock="onDeviceUnlock"
  />

  <PreviewOverlay
    v-if="previewFile"
    :key="previewFile.id"
    :file="previewFile"
    @close="previewEntry = null"
  />

  <PreviewOverlay
    v-if="remotePreview"
    :key="'r' + remotePreview.id"
    :file="remotePreview"
    remote
    @close="remotePreview = null"
  />

  <PreviewOverlay
    v-if="plainPreview"
    :key="'p' + plainPreview.id"
    :file="plainPreview"
    plain
    @close="plainPreview = null"
    @external="onPlainExternal"
  />

  <!-- 容器内文件：同一个预览组件，只是换 URL 前缀。
       不传 plain，所以兜底分支不会出现「用外部应用打开」——
       容器里的条目没有磁盘文件可交给系统程序 -->
  <PreviewOverlay
    v-if="citemPreview"
    :key="'c' + citemPreview.id"
    :file="citemPreview"
    in-container
    @close="citemPreview = null"
  />

  <!-- 云盘文件：同一个预览组件，URL 前缀换成 /pfile/。
       关闭时要释放后端来源句柄，所以走专门的 close 处理。 -->
  <PreviewOverlay
    v-if="placePreview"
    :key="'f' + placePreview.id"
    :file="placePreview"
    place
    @close="onClosePlacePreview"
  />

  <!-- 设置。语言改动要立刻生效，所以它自己 emit 一个 lang 事件，
       而不是等关闭后统一应用——用户选完语言却看不到变化，
       无法确认自己选对了 -->
  <SettingsDialog
    v-if="showSettings"
    @close="onSettingsClose"
    @lang="onSettingsLang"
    @lock="onSettingsLock"
    @devices="onSettingsDevices"
  />

  <RemotePlaceDialog
    v-if="showAddPlace"
    @cancel="showAddPlace = false"
    @added="onPlaceAdded"
  />

  <TelegramLoginDialog
    v-if="showTelegramLogin"
    @cancel="showTelegramLogin = false"
    @done="onTelegramDone"
  />
</template>
