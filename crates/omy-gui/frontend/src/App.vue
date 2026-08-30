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
} from './store.js';
import MainScreen from './components/MainScreen.vue';
import EncryptDialog from './components/EncryptDialog.vue';
import UnlockDialog from './components/UnlockDialog.vue';
import PreviewOverlay from './components/PreviewOverlay.vue';
import ContainerPanel from './components/ContainerPanel.vue';
import DevicePanel from './components/DevicePanel.vue';
import RemoteScreen from './components/RemoteScreen.vue';

const showEncrypt = ref(false);
const showUnlock = ref(false);
const showDevices = ref(false);
const unlockError = ref('');
const previewEntry = ref(null);

/** 正在浏览的目录容器：`{ name, items }`。 */
const container = ref(null);
/** 远端预览目标。与本机预览分开存：两者的对象结构不同，
 * 混在一个 ref 里迟早会出现「用本机的字段去读远端对象」。 */
const remotePreview = ref(null);
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
    // 只会得到一堆首尾相接的字节，所以先分流出去
    if (entry.is_container) {
      await openContainer(entry);
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

/** 打开一个目录容器，列出里面的条目。 */
async function openContainer(entry) {
  const items = await api.listContainer(entry.entry_id).catch(() => null);
  if (!items) {
    state.error = i18n.te('container_failed');
    return;
  }
  container.value = { name: entry.real_name || entry.name, items };
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
  // 容器面板同样要关：里面列的是文件名，锁定后不该继续可见
  container.value = null;
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

  <ContainerPanel
    v-if="container"
    :name="container.name"
    :items="container.items"
    @close="container = null"
  />

  <PreviewOverlay
    v-if="plainPreview"
    :key="'p' + plainPreview.id"
    :file="plainPreview"
    plain
    @close="plainPreview = null"
    @external="onPlainExternal"
  />
</template>
