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
  loadStorageAccess,
  grantStorageAccess,
  encryptSelected,
  restoreSelected,
  tryUnlock,
  lock,
  switchLanguage,
  applyLanguage,
  afterPlaceAdded,
  reloadRemotePlaces,
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
import SettingsDialog from './components/SettingsDialog.vue';
import RemotePlaceDialog from './components/RemotePlaceDialog.vue';
import { initAutoLock, configureAutoLock } from './autolock.js';

/** 设置对话框是否打开。 */
const showSettings = ref(false);
/** 添加远程位置对话框是否打开。 */
const showAddPlace = ref(false);

/** 远程位置添加成功：刷新列表并直接进去。
 *
 * 不留在原地——用户刚填完一串地址和密码，想看到的是里面有什么，
 * 而不是回到一个看不出有没有添加成功的界面。
 */
async function onPlaceAdded(id) {
  showAddPlace.value = false;
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
  if (dir) await navigate(dir);
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

/** 在远端视图里点「试密码」。 */
function onRemoteUnlock() {
  unlockForRemote.value = true;
  unlockError.value = '';
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
  await refreshDeviceOverview();

  // 自动锁定按配置启动。放在最后：它依赖配置读取，而前面几步
  // 都是界面首屏需要的，不该为它推迟
  initAutoLock();
  await applySettings();
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
    @places="showAddPlace = state.remotePlaces.length === 0"
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
    @cancel="onKeyCancel"
    @retry="onKeyRetry"
    @submit="onKeySubmit"
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
    @cancel="onUnlockCancel"
    @submit="onUnlockSubmit"
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

  <!-- 设置。语言改动要立刻生效，所以它自己 emit 一个 lang 事件，
       而不是等关闭后统一应用——用户选完语言却看不到变化，
       无法确认自己选对了 -->
  <SettingsDialog
    v-if="showSettings"
    @close="onSettingsClose"
    @lang="onSettingsLang"
  />

  <RemotePlaceDialog
    v-if="showAddPlace"
    @cancel="showAddPlace = false"
    @added="onPlaceAdded"
  />
</template>
