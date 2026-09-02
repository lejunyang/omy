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
  loadPlaces,
  encryptSelected,
  tryUnlock,
  lock,
  switchLanguage,
  selectAll,
  enrich,
  encryptable,
  refreshDeviceOverview,
  connectRemote,
  tryUnlockRemote,
  openWithSystem,
  enterContainer,
  enterContainerDir,
  manageKey,
} from './store.js';
import MainScreen from './components/MainScreen.vue';
import EncryptDialog from './components/EncryptDialog.vue';
import UnlockDialog from './components/UnlockDialog.vue';
import KeyDialog from './components/KeyDialog.vue';
import PreviewOverlay from './components/PreviewOverlay.vue';
import DevicePanel from './components/DevicePanel.vue';
import RemoteScreen from './components/RemoteScreen.vue';

const showEncrypt = ref(false);
/** 密码管理的目标条目；null 表示对话框关着。
 *
 * 存条目本身而不是一个布尔量：对话框要显示改的是哪个文件，
 * 而「当前选中项」在对话框打开期间可能被别处改掉。
 */
const keyTarget = ref(null);
/** 密码管理的错误单独存，与解锁框的 error 分开。 */
const keyError = ref('');
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
    // 错误已经在对话框里显示，不要再占用底部提示条重复一遍
    state.error = '';
  }
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
  const dir = await api.pickFolder(i18n.t('nav.pick_folder')).catch(() => null);
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

onMounted(async () => {
  document.addEventListener('keydown', onKey);
  await loadPlaces();
  await refreshDeviceOverview();
});
onBeforeUnmount(() => document.removeEventListener('keydown', onKey));
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
    @encrypt="showEncrypt = true"
    @manage-key="((keyError = ''), (keyTarget = $event))"
    @lock="doLock"
    @quick-unlock="((unlockError = ''), (showUnlock = true))"
    @pick="onPick"
    @lang="switchLanguage"
    @devices="showDevices = true"
  />

  <DevicePanel
    v-if="showDevices"
    @close="onDevicePanelClose"
    @connect="onConnectRemote"
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
    :busy="state.busy"
    :error="keyError"
    @cancel="keyTarget = null"
    @submit="onKeySubmit"
  />

  <UnlockDialog
    v-if="showUnlock"
    :busy="state.busy"
    :error="unlockError"
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
</template>
