<script setup>
/** 根组件。
 *
 * 负责编排四件事：文件管理器主体、加密对话框、解锁对话框、预览层。
 *
 * # 双击一个条目会发生什么
 *
 * - 目录 → 进去
 * - 普通文件 → 目前只能选中（应用内不预览未加密文件，那是系统
 *   文件管理器的活；将来可以加「用外部应用打开」）
 * - 加密文件且已解锁 → 预览
 * - 加密文件但锁着 → 弹密码框
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
} from './store.js';
import MainScreen from './components/MainScreen.vue';
import EncryptDialog from './components/EncryptDialog.vue';
import UnlockDialog from './components/UnlockDialog.vue';
import PreviewOverlay from './components/PreviewOverlay.vue';
import DevicePanel from './components/DevicePanel.vue';
import RemoteScreen from './components/RemoteScreen.vue';

const showEncrypt = ref(false);
const showUnlock = ref(false);
const showDevices = ref(false);
const unlockError = ref('');
const previewEntry = ref(null);
/** 远端预览目标。与本机预览分开存：两者的对象结构不同，
 * 混在一个 ref 里迟早会出现「用本机的字段去读远端对象」。 */
const remotePreview = ref(null);
/** 当前密码框是给远端用的还是本机用的。 */
const unlockForRemote = ref(false);

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
  if (entry.is_dir) {
    await navigate(entry.path);
    return;
  }
  if (!entry.is_encrypted) {
    // 未加密文件应用内不预览。静默什么都不做会让人以为卡了，
    // 所以至少选中它，给一个可见的反馈
    state.selected = [entry.path];
    return;
  }
  if (entry.unlocked && entry.entry_id) {
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

async function onUnlockSubmit({ password, label }) {
  const ok = unlockForRemote.value
    ? await tryUnlockRemote(password, label)
    : await tryUnlock(password, label);
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
</template>
