<script setup>
/** 设置：PC 是左列分类的弹窗，移动端是列表 + 二级页。
 *
 * # 为什么需要这个组件
 *
 * 语言与主题原先是顶栏的两个图标，移动端底栏也各占一格。底栏要腾出
 * 位置给「远程」，这两格没了地方；而远程位置又带来缓存、扫描并发等
 * 一批配置。散在各处迟早找不到，收进一个入口是必然的。
 *
 * # 桌面与移动共用这一个组件
 *
 * 不复制出一个 MobileSettings：分类、表单项、保存逻辑完全一样，
 * 分成两份迟早出现「桌面改了移动端还是老样子」。差异只在外壳——
 * 桌面是两栏弹窗，移动端是单栏列表加二级页。
 *
 * # 改动即时生效，但落盘是整份写回
 *
 * 语言和主题改完要立刻能看到效果（否则用户不知道选对没有），
 * 所以这两项在 change 时就应用；其余项在关闭时统一保存。
 */

import { ref, computed, onMounted } from 'vue';
import * as i18n from '../i18n.js';
import { isMobile } from '../viewport.js';
import { theme, setTheme } from '../theme.js';
import * as api from '../api.js';
import { state, setNotice, reloadRemotePlaces } from '../store.js';

const emit = defineEmits(['close', 'lang', 'lock', 'devices']);

/** 当前分类。`cache` 是「远程位置」的二级页。 */
const pane = ref('general');
/** 移动端：null 表示停在主列表，否则是当前二级页的分类名。 */
const mobilePane = ref(null);
/** 移动端返回栈：记录进入当前二级页之前的层级，返回时逐级弹出。
 *  原来 backMobile 硬编码「cache 的上级是 remote」，但 cache 其实是从
 *  pinned 页进的——于是从缓存返回会跳到「远程位置」而不是回上一级
 *  （用户报的返回栈错乱）。用栈就不必猜每个页的父级。 */
const mobileStack = ref([]);

/** 整份配置。读失败时用空对象兜底，界面显示默认值而不是白屏。 */
const cfg = ref(null);
const paths = ref({ config: '', cache: '', data: '', log: '', portable: false });
const loading = ref(true);
const error = ref('');

/** 远程密文缓存的真实用量，来自后端（{used,limit,root}）。
 *  配置里的 cache_limit 是「将要保存」的值，这里是「磁盘上现在」的值。 */
const cacheUsage = ref({ used: 0, limit: 0, root: '', pinned_used: 0, pinned_files: 0 });
const clearing = ref(false);

/** 当前会话已装入的密码数量，安全页显示 + 判断「立即锁定」是否可点。 */
const loadedCount = ref(0);

/** 关于页的版本信息，来自后端编译期常量。 */
const aboutInfo = ref({ app_version: '', format_major: 1, format_minor: 0 });

/** 已连接的远程位置数量，设置主页「已连接的位置」摘要用。
 *  单独拉一次，不依赖此刻是否正开着云盘浏览器（那里才会 reload 列表）。 */
const remotePlaceCount = ref(0);

/** 跳到完整的设备与共享面板：配对/撤销需要先解锁设备库，
 *  不在设置弹窗里复制那套流程与计数（库没解锁时计数会失真成 0）。
 *  跳走前先保存，否则在这一页改的本机名称/自启会丢。 */
async function openDevices() {
  await save();
  emit('devices');
}

async function loadCacheUsage() {
  try {
    cacheUsage.value = await api.remoteCacheUsage();
  } catch {
    // 用量读不出来不应挡住设置页：保留 0，清理按钮会因 used=0 禁用
  }
}

/** 分类定义。顺序按使用频率，不按模块划分——「通用」放第一是因为
 *  语言和主题是最常被找的两项。 */
const PANES = [
  { key: 'general', icon: '⚙️', label: 'settings.general' },
  { key: 'remote', icon: '☁️', label: 'settings.remote' },
  { key: 'security', icon: '🔐', label: 'settings.security' },
  { key: 'encrypt', icon: '🔒', label: 'settings.encrypt_defaults' },
  { key: 'playback', icon: '🎬', label: 'settings.playback' },
  { key: 'devices', icon: '📡', label: 'settings.devices' },
  { key: 'about', icon: 'ℹ️', label: 'settings.about' },
];

/**
 * 移动端主列表不渲染通用分类按钮：「通用」整组直接平铺到设置主页
 * （语言/主题原本在底栏一点即切，收进设置后还要点两层才够到就是退步）。
 * 其余分类在模板里按「远程位置 / 安全 / 其他」分组、并带当前值摘要，
 * 顺序与分组见模板；PC 端左栏仍用完整的 PANES（含通用页）。
 */

/** 缓存上限的可选值（字节）。0 表示不限制。 */
const CACHE_LIMITS = [
  { v: 512 * 1024 * 1024, k: '512 MB' },
  { v: 1024 * 1024 * 1024, k: '1 GB' },
  { v: 2 * 1024 * 1024 * 1024, k: '2 GB' },
  { v: 5 * 1024 * 1024 * 1024, k: '5 GB' },
  { v: 10 * 1024 * 1024 * 1024, k: '10 GB' },
  { v: 0, k: 'settings.cache_unlimited' },
];

/** 自动锁定的可选值（秒）。0 表示从不。 */
const LOCK_OPTIONS = [0, 300, 600, 1800, 3600];

onMounted(async () => {
  try {
    const [c, p] = await Promise.all([api.configGet(), api.configPaths()]);
    cfg.value = c;
    paths.value = p;
    await dkRefresh();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.load_failed');
  } finally {
    loading.value = false;
  }
  // 用量与配置独立加载：配置失败不该连累用量显示
  loadCacheUsage();
  // 读不出来按 0 处理：按钮会因此禁用，不会误导用户去锁一个空会话
  try {
    loadedCount.value = Number(await api.credentialCount()) || 0;
  } catch {
    loadedCount.value = 0;
  }
  // 版本信息读不出来就保留占位默认值，不挡住整个设置页
  try {
    aboutInfo.value = await api.appAbout();
  } catch {
    /* 用内置占位 */
  }
  // 位置数读不出来按 0 显示，不挡住设置页
  try {
    const places = await api.remotePlaceList();
    remotePlaceCount.value = Array.isArray(places) ? places.length : 0;
  } catch {
    remotePlaceCount.value = 0;
  }
});

/** 保存整份配置。
 *
 * 失败必须让用户知道：静默失败会让他以为改好了，下次启动才发现没生效，
 * 那时已经无从判断是哪一步出的问题。
 */
async function save() {
  if (!cfg.value) return;
  try {
    await api.configSet(cfg.value);
    // 缓存上限/目录改了要让运行中的后端立刻换缓存实例，否则要等重启；
    // 失败不阻断关闭——配置已落盘，重启后仍会生效
    try {
      await api.remoteCacheApply(
        cfg.value.remote?.cache_limit,
        cfg.value.remote?.cache_dir ?? null,
      );
      await loadCacheUsage();
    } catch {
      /* 重建失败不影响配置已保存 */
    }
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.save_failed');
    return false;
  }
  return true;
}

async function onClose() {
  const ok = await save();
  if (ok) emit('close');
}

/** 安全页「立即锁定」：先把设置落盘，再交给 App 走统一的锁定收尾
 *  （关各类预览、清会话），避免在这里只调 lock 而留下打开的解密预览。 */
async function lockNow() {
  await save();
  emit('lock');
}

/** 语言要立刻生效：选完还得关掉设置页才看到变化的话，
 *  用户无法确认自己选对了。 */
function onLanguage(v) {
  cfg.value.ui.language = v;
  emit('lang', v);
}

/** 主题同理，立即应用。 */
function onTheme(v) {
  cfg.value.ui.theme = v;
  setTheme(v);
}

/** 字节数转可读文本。 */
function fmtBytes(n) {
  if (!n) return i18n.t('settings.cache_unlimited');
  return i18n.formatSize(n);
}

function lockLabel(secs) {
  if (secs === 0) return i18n.t('settings.lock_never');
  if (secs < 3600) return i18n.t('settings.lock_minutes', { n: Math.round(secs / 60) });
  return i18n.t('settings.lock_hours', { n: Math.round(secs / 3600) });
}

/** 立即清空远程密文缓存，返回后刷新用量。清的只是密文块，不碰云端文件。 */
async function clearCache() {
  if (clearing.value) return;
  clearing.value = true;
  try {
    // 后端 clear 直接返回清空后的已用字节数（u6），不是对象
    const used = await api.remoteCacheClear();
    cacheUsage.value = { ...cacheUsage.value, used: Number(used) || 0 };
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.cache_clear_failed');
  } finally {
    clearing.value = false;
  }
}

/** 永久保留清单。进入该页时拉一次，取消后就地去掉那一行。 */
const pinned = ref([]);
const pinnedLoading = ref(false);
const unpinning = ref('');

/** 把位置 id 换成用户看得懂的名字。
 *
 * **查不到是正常情况**：`PinnedFile.place` 记的是打 pin 那一刻的位置 id，
 * 用户之后把位置「从列表移除」，这条永久记录仍然在、文件也仍然占着磁盘。
 * 那时必须显示成「已移除的位置」并保持可取消——否则就成了最糟的组合：
 * 空间占着、用户想清、界面上却点不动。 */
function placeName(id) {
  const p = (state.remotePlaces || []).find((x) => x.id === id);
  return p ? p.name : i18n.t('settings.pinned_gone_place');
}

/** 永久清单里一行的标识：位置 + 键。单用 key 不够——
 *  两个位置上完全可能存在内容相同、因而版本哈希也相同的文件。 */
function pinnedRowId(f) {
  return `${f.place}\u0000${f.key}`;
}

async function openPinnedPane() {
  if (isMobile.value) {
    mobileStack.value.push(mobilePane.value);
    mobilePane.value = 'pinned';
  } else {
    pane.value = 'pinned';
  }
  pinnedLoading.value = true;
  try {
    // 先刷新位置列表再列清单：名字映射要对着当前真相。
    // 不刷的话，设置页在位置列表从未加载过时打开（比如启动后直接进设置），
    // placeName 会把**每一行**都回落成「已移除的位置」——
    // 那比不显示名字更糟，用户会以为自己的位置全丢了
    await reloadRemotePlaces();
    pinned.value = await api.remoteCacheListPinned();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.pinned_list_failed');
  } finally {
    pinnedLoading.value = false;
  }
}

/** 取消一个文件的永久保留。释放的字节数由后端返回，用来更新那两个数字。 */
async function unpinOne(f) {
  const rid = pinnedRowId(f);
  if (unpinning.value) return;
  unpinning.value = rid;
  try {
    const freed = await api.remoteCacheUnpinByKey(f.place, f.key, f.total_blocks);
    pinned.value = pinned.value.filter((x) => pinnedRowId(x) !== rid);
    // 永久层的用量与文件数要跟着变，否则用户清完回上一页
    // 还看到原来那个数字，会以为没清掉
    const u = cacheUsage.value;
    cacheUsage.value = {
      ...u,
      pinned_used: Math.max(0, (u.pinned_used || 0) - (Number(freed) || 0)),
      pinned_files: Math.max(0, (u.pinned_files || 0) - 1),
    };
    setNotice(i18n.t('settings.pinned_unpinned',
      { size: i18n.formatSize(Number(freed) || 0) }));
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.pinned_unpin_failed');
  } finally {
    unpinning.value = '';
  }
}

/** 实际生效的缓存目录：后端返回的 root 比配置推断更准（自定义可能建不起来）。 */
const cacheRoot = computed(() => cacheUsage.value.root || paths.value.cache || '');

/** 用量百分比；不限上限时不画填充比例（没有 100% 可言）。 */
const usedPct = computed(() => {
  const { used, limit } = cacheUsage.value;
  if (!limit || limit <= 0) return 0;
  return Math.min(100, Math.round((used / limit) * 100));
});

const usedText = computed(() => {
  const { used, limit } = cacheUsage.value;
  if (limit && limit > 0) {
    return `${i18n.formatSize(used)} / ${i18n.formatSize(limit)}`;
  }
  return `${i18n.formatSize(used)} · ${i18n.t('settings.cache_unlimited')}`;
});

/** 永久缓存的显示文本：**绝对值 + 文件计数，没有分母**。
 *
 * 14 号文档 §8.4.1 明确不要给它画进度条——一旦画了条，用户就会去找
 * 「那上限是多少」，而答案是没有上限。
 */
const pinnedText = computed(() => {
  const { pinned_used: used, pinned_files: files } = cacheUsage.value;
  return `${i18n.formatSize(used || 0)}（${i18n.tn('settings.pinned_files', files || 0)}）`;
});

/** 移动端进入二级页：把当前层级压栈，便于逐级返回。 */
function goMobile(key) {
  mobileStack.value.push(mobilePane.value);
  mobilePane.value = key;
}

/** 进入「密文缓存」二级页（PC 与移动共用同一 pane）。 */
function openCachePane() {
  // isMobile 是 readonly(ref)，在 <script> 里必须取 .value，
  // 直接判断 ref 对象永远为真——那会让桌面端也走移动分支、PC 缓存页进不去
  if (isMobile.value) {
    mobileStack.value.push(mobilePane.value);
    mobilePane.value = 'cache';
  } else {
    pane.value = 'cache';
  }
  // 进页面前刷一次用量，避免看到上次的旧数字
  loadCacheUsage();
}

/** 移动端标题栏返回：弹出返回栈，回到进入当前页之前的那一层。
 *  栈空则回主列表（null）。不再硬编码某页的父级。 */
function backMobile() {
  mobilePane.value = mobileStack.value.length ? mobileStack.value.pop() : null;
}

/** 移动端二级页标题（cache 不在 PANES 里，单独给名）。 */
const paneTitle = computed(() => {
  // 同上，isMobile 在 JS 中要取 .value
  const key = isMobile.value ? mobilePane.value : pane.value;
  if (key === 'cache') return i18n.t('settings.cache_title');
  return i18n.t(PANES.find((p) => p.key === key)?.label || 'settings.title');
});

// ── 免密解锁（Windows Hello + TPM）─────────────────────────────

/** 这台电脑支持吗、当前位置启用了吗。 */
const dk = ref({ available: false, enrolled: false });
const dkPassword = ref('');
const dkBusy = ref(false);
const dkNotice = ref('');

async function dkRefresh() {
  try {
    dk.value = await api.deviceKeyStatus(state.cwd);
  } catch {
    // 查不到就当不可用，整行不显示。这里不弹错：用户是来看设置的，
    // 「免密解锁状态查询失败」对他没有意义
    dk.value = { available: false, enrolled: false };
  }
}

async function dkEnroll() {
  dkBusy.value = true;
  dkNotice.value = '';
  error.value = '';
  try {
    await api.deviceKeyEnroll(state.cwd, dkPassword.value);
    // 成功后立刻清掉密码：它没有必要在内存里多留一秒
    dkPassword.value = '';
    dkNotice.value = i18n.t('devicekey.enroll_done');
    await dkRefresh();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'devicekey.enroll_title');
  } finally {
    dkBusy.value = false;
  }
}

async function dkForget() {
  dkBusy.value = true;
  dkNotice.value = '';
  error.value = '';
  try {
    await api.deviceKeyForget(state.cwd);
    dkNotice.value = i18n.t('devicekey.forget_done');
    await dkRefresh();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'devicekey.forget');
  } finally {
    dkBusy.value = false;
  }
}

/** 缓存入口摘要：已用 / 上限 · LRU 淘汰。 */
const cacheSummary = computed(() => `${usedText.value} · ${i18n.t('settings.cache_lru')}`);

/** 在系统文件管理器里打开缓存目录（仅桌面端有此按钮）。 */
async function openCacheDir() {
  try {
    await api.remoteCacheOpenDir();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.cache_open_failed');
  }
}

/** 在系统文件管理器里打开日志目录（仅桌面端）。 */
async function openLogDir() {
  try {
    await api.openLogDir();
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'settings.log_open_failed');
  }
}
</script>

<template>
  <div class="mask" @click.self="onClose">
    <div class="setdlg" :class="{ mob: isMobile }">
      <!-- 标题栏：移动端在二级页时显示返回箭头 -->
      <div class="seth">
        <button
          v-if="isMobile && mobilePane"
          class="iconbtn"
          data-si="back"
          :aria-label="i18n.t('common.back')"
          @click="backMobile"
        >
          ←
        </button>
        <span class="sethn">{{ paneTitle }}</span>
        <button
          v-if="!isMobile"
          class="iconbtn close"
          data-si="close"
          :aria-label="i18n.t('common.close')"
          @click="onClose"
        >
          ✕
        </button>
      </div>

      <div v-if="loading" class="pane">{{ i18n.t('busy.loading') }}</div>
      <div v-else-if="!cfg" class="pane err">{{ error }}</div>

      <div v-else class="setbody" :class="{ mob: isMobile }">
        <!-- 桌面：左列分类 -->
        <nav v-if="!isMobile" class="setnav">
          <button
            v-for="p in PANES"
            :key="p.key"
            class="snav"
            :class="{ on: pane === p.key || (pane === 'cache' && p.key === 'remote') }"
            :data-sp="p.key"
            @click="pane = p.key"
          >
            <span aria-hidden="true">{{ p.icon }}</span>
            <span>{{ i18n.t(p.label) }}</span>
          </button>
        </nav>

        <!-- 移动端主列表：通用高频项直接平铺可改，其余为二级页入口 -->
        <div v-if="isMobile && !mobilePane" class="mlist">
          <!-- 语言/主题原本在底栏一点即切，收进设置后必须在主页一层就够得到 -->
          <div class="mgh">{{ i18n.t('settings.general') }}</div>
          <div class="mqrow">
            <label class="mqlb" for="m-set-language">{{ i18n.t('settings.language') }}</label>
            <select
              id="m-set-language"
              data-sf="m_language"
              :value="cfg.ui.language"
              @change="onLanguage($event.target.value)"
            >
              <option value="auto">{{ i18n.t('settings.follow_system') }}</option>
              <option value="zh-CN">简体中文</option>
              <option value="en">English</option>
            </select>
          </div>
          <div class="mqrow">
            <label class="mqlb" for="m-set-theme">{{ i18n.t('settings.theme') }}</label>
            <select
              id="m-set-theme"
              data-sf="m_theme"
              :value="cfg.ui.theme"
              @change="onTheme($event.target.value)"
            >
              <option value="auto">{{ i18n.t('settings.follow_system') }}</option>
              <option value="dark">{{ i18n.t('settings.theme_dark') }}</option>
              <option value="light">{{ i18n.t('settings.theme_light') }}</option>
            </select>
          </div>
          <div class="mqrow">
            <label class="mqlb" for="m-set-view">{{ i18n.t('settings.default_view') }}</label>
            <select id="m-set-view" data-sf="m_view" v-model="cfg.ui.view">
              <option value="grid">{{ i18n.t('view.grid') }}</option>
              <option value="list">{{ i18n.t('view.list') }}</option>
            </select>
          </div>
          <div class="mqrow">
            <label class="mqlb" for="m-set-startup">{{ i18n.t('settings.startup') }}</label>
            <select id="m-set-startup" data-sf="m_startup" v-model="cfg.ui.startup">
              <option value="last">{{ i18n.t('settings.startup_last') }}</option>
              <option value="home">{{ i18n.t('settings.startup_home') }}</option>
            </select>
          </div>

          <div class="mgh">{{ i18n.t('settings.remote') }}</div>
          <button class="mrow" type="button" data-sp="remote" @click="goMobile('remote')">
            <span class="mri" aria-hidden="true">☁️</span>
            <span class="mrt">{{ i18n.t('settings.connected_places') }}</span>
            <span class="mrv">{{ remotePlaceCount }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>
          <button class="mrow" type="button" data-sf="cache_entry" @click="openCachePane">
            <span class="mri" aria-hidden="true">🗄️</span>
            <span class="mrt">{{ i18n.t('settings.cache_title') }}</span>
            <span class="mrv">{{ usedText }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>

          <div class="mgh">{{ i18n.t('settings.group_security') }}</div>
          <button class="mrow" type="button" data-sp="security" @click="goMobile('security')">
            <span class="mri" aria-hidden="true">🔐</span>
            <span class="mrt">{{ i18n.t('settings.security') }}</span>
            <span class="mrv" v-if="cfg">{{ lockLabel(cfg.security.auto_lock_secs) }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>
          <button class="mrow" type="button" data-sp="encrypt" @click="goMobile('encrypt')">
            <span class="mri" aria-hidden="true">🔒</span>
            <span class="mrt">{{ i18n.t('settings.encrypt_defaults') }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>
          <button class="mrow" type="button" data-sp="devices" @click="goMobile('devices')">
            <span class="mri" aria-hidden="true">📡</span>
            <span class="mrt">{{ i18n.t('settings.devices') }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>

          <div class="mgh">{{ i18n.t('settings.group_other') }}</div>
          <button class="mrow" type="button" data-sp="playback" @click="goMobile('playback')">
            <span class="mri" aria-hidden="true">🎬</span>
            <span class="mrt">{{ i18n.t('settings.playback') }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>
          <button class="mrow" type="button" data-sp="about" @click="goMobile('about')">
            <span class="mri" aria-hidden="true">ℹ️</span>
            <span class="mrt">{{ i18n.t('settings.about') }}</span>
            <span class="mrv">{{ aboutInfo.app_version }}</span>
            <span class="mra" aria-hidden="true">›</span>
          </button>
        </div>

        <!-- 内容区 -->
        <section
          v-if="!isMobile || mobilePane"
          class="pane"
          :data-pane="isMobile ? mobilePane : pane"
        >
          <!-- 通用 -->
          <template v-if="(isMobile ? mobilePane : pane) === 'general'">
            <div class="row">
              <label class="lb">{{ i18n.t('settings.language') }}</label>
              <select data-sf="language" :value="cfg.ui.language" @change="onLanguage($event.target.value)">
                <option value="auto">{{ i18n.t('settings.follow_system') }}</option>
                <option value="zh-CN">简体中文</option>
                <option value="en">English</option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.theme') }}</label>
              <select data-sf="theme" :value="cfg.ui.theme" @change="onTheme($event.target.value)">
                <option value="auto">{{ i18n.t('settings.follow_system') }}</option>
                <option value="dark">{{ i18n.t('settings.theme_dark') }}</option>
                <option value="light">{{ i18n.t('settings.theme_light') }}</option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.startup') }}</label>
              <select data-sf="startup" v-model="cfg.ui.startup">
                <option value="last">{{ i18n.t('settings.startup_last') }}</option>
                <option value="home">{{ i18n.t('settings.startup_home') }}</option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.default_view') }}</label>
              <select data-sf="view" v-model="cfg.ui.view">
                <option value="grid">{{ i18n.t('view.grid') }}</option>
                <option value="list">{{ i18n.t('view.list') }}</option>
              </select>
            </div>
          </template>

          <!-- 远程位置 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'remote'">
            <div class="row">
              <label class="lb">{{ i18n.t('settings.scan_scope') }}</label>
              <div class="fld">
                <select data-sf="scan_omy_only" v-model="cfg.remote.scan_omy_only">
                  <option :value="true">{{ i18n.t('settings.scan_omy_only') }}</option>
                  <option :value="false">{{ i18n.t('settings.scan_all_files') }}</option>
                </select>
                <div class="desc">{{ i18n.t('settings.scan_scope_desc') }}</div>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.concurrency') }}</label>
              <div class="fld">
                <select data-sf="scan_concurrency" v-model.number="cfg.remote.scan_concurrency">
                  <option :value="4">4</option>
                  <option :value="8">8</option>
                  <option :value="16">16</option>
                </select>
                <div class="desc">{{ i18n.t('settings.concurrency_desc') }}</div>
              </div>
            </div>

            <!-- 缓存内容较多，独立成二级页；这里只给一行摘要（对齐原型） -->
            <button class="subentry" type="button" data-sf="cache_entry" @click="openCachePane">
              <span class="se-ico" aria-hidden="true">🗄️</span>
              <span class="se-body">
                <span class="se-title">{{ i18n.t('settings.cache_title') }}</span>
                <span class="se-desc">{{ cacheSummary }}</span>
              </span>
              <span class="se-arrow" aria-hidden="true">›</span>
            </button>
          </template>

          <!-- 永久保留清单：缓存页的下一级。
               没有这个入口，用户要取消某个文件的永久保留，只能回到它原来
               所在的位置、在目录里翻出来再右键——而 Telegram 上那条消息
               可能早就找不到了，结果永久层只进不出。 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'pinned'">
            <div class="panehead">
              <button
                class="iconbtn"
                type="button"
                data-si="pinned-back"
                :aria-label="i18n.t('common.back')"
                @click="isMobile ? backMobile() : (pane = 'cache')"
              >
                ←
              </button>
              <span class="panetitle">{{ i18n.t('settings.pinned_title') }}</span>
            </div>

            <div v-if="pinnedLoading" class="row">
              <div class="fld desc">{{ i18n.t('settings.pinned_loading') }}</div>
            </div>
            <div v-else-if="!pinned.length" class="row">
              <div class="fld">
                <div>{{ i18n.t('settings.pinned_empty') }}</div>
                <div class="desc">{{ i18n.t('settings.pinned_empty_hint') }}</div>
              </div>
            </div>
            <div
              v-for="f in pinned"
              v-else
              :key="pinnedRowId(f)"
              class="row pinnedrow"
              data-sf="pinned_row"
            >
              <div class="fld pinnedinfo">
                <div class="pinnedplace">{{ placeName(f.place) }}</div>
                <!-- key 含版本哈希、不是路径，所以只当标识显示，
                     宽度收住、不让它把行撑破 -->
                <div class="desc pinnedkey" :title="f.key">{{ f.key }}</div>
              </div>
              <div class="pinnedsize">{{ i18n.formatSize(f.used_bytes) }}</div>
              <button
                type="button"
                class="btn small"
                :data-sf-unpin="f.key"
                :disabled="unpinning === pinnedRowId(f)"
                @click="unpinOne(f)"
              >
                {{ i18n.t('settings.pinned_unpin') }}
              </button>
            </div>
          </template>

          <!-- 密文缓存：「远程位置」的二级页，左列仍高亮远程位置 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'cache'">
            <div v-if="!isMobile" class="panehead">
              <button
                class="iconbtn"
                type="button"
                data-si="cache-back"
                :aria-label="i18n.t('common.back')"
                @click="pane = 'remote'"
              >
                ←
              </button>
              <span class="panetitle">{{ i18n.t('settings.cache_title') }}</span>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_limit') }}</label>
              <select data-sf="cache_limit" v-model.number="cfg.remote.cache_limit">
                <option v-for="o in CACHE_LIMITS" :key="o.v" :value="o.v">
                  {{ o.k.startsWith('settings.') ? i18n.t(o.k) : o.k }}
                </option>
              </select>
            </div>
            <!-- 临时层：有分母，画进度条 -->
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_temp') }}</label>
              <div class="fld">
                <div class="cachebar" :class="{ zero: usedPct === 0 }">
                  <div class="cachebar-fill" :style="{ width: usedPct + '%' }"></div>
                </div>
                <div class="desc">{{ usedText }}</div>
                <button
                  class="btn small"
                  data-sf="cache_clear"
                  :disabled="clearing || !cacheUsage.used"
                  @click="clearCache"
                >
                  {{ clearing ? i18n.t('settings.clearing') : i18n.t('settings.cache_clear') }}
                </button>
              </div>
            </div>

            <!-- 永久层：**没有分母，所以不画进度条**（14 号文档 §8.4.1）。
                 画了条用户就会去找「上限是多少」，而答案是没有上限。
                 「不占用上面的上限」要直接写出来——那是看到两个数字时的
                 第一个疑问。 -->
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_pinned') }}</label>
              <div class="fld">
                <div class="pinnedval" data-sf="cache_pinned">{{ pinnedText }}</div>
                <div class="desc">{{ i18n.t('settings.cache_pinned_hint') }}</div>
              </div>
            </div>
            <div class="row">
              <label class="lb"></label>
              <div class="fld">
                <button
                  type="button"
                  class="btn small"
                  data-sf="pinned_manage"
                  @click="openPinnedPane"
                >
                  {{ i18n.t('settings.pinned_manage') }}
                </button>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_dir') }}</label>
              <div class="fld pathrow">
                <div class="fld path">{{ cacheRoot }}</div>
                <button v-if="!isMobile" type="button" class="btn small" data-sf="cache_open" @click="openCacheDir">
                  {{ i18n.t('settings.cache_open') }}
                </button>
              </div>
            </div>
            <div class="row">
              <label class="lb"></label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="clear_cache_on_exit" v-model="cfg.remote.clear_cache_on_exit" />
                  {{ i18n.t('settings.clear_on_exit') }}
                </label>
                <label v-if="isMobile" class="chk">
                  <input type="checkbox" data-sf="cache_wifi_only" v-model="cfg.remote.cache_wifi_only" disabled :title="i18n.t('settings.wifi_only_soon')" />
                  <span>
                    {{ i18n.t('settings.wifi_only') }}
                    <div class="desc">{{ i18n.t('settings.wifi_only_soon') }}</div>
                  </span>
                </label>
              </div>
            </div>
            <div class="hint">
              <span aria-hidden="true">ⓘ</span>
              <span>{{ i18n.t('settings.cache_ciphertext_note') }}</span>
            </div>
          </template>

          <!-- 安全 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'security'">
            <div class="row">
              <label class="lb">{{ i18n.t('settings.auto_lock') }}</label>
              <div class="fld">
                <select data-sf="auto_lock_secs" v-model.number="cfg.security.auto_lock_secs">
                  <option v-for="s in LOCK_OPTIONS" :key="s" :value="s">{{ lockLabel(s) }}</option>
                </select>
                <div class="desc">{{ i18n.t('settings.auto_lock_desc') }}</div>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.session') }}</label>
              <div class="fld">
                <div>{{ i18n.t('settings.loaded_passwords', { n: loadedCount }) }}</div>
                <button
                  type="button"
                  class="btn small"
                  data-sf="lock_now"
                  :disabled="loadedCount === 0"
                  @click="lockNow"
                >
                  {{ i18n.t('settings.lock_now_btn') }}
                </button>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.background') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="lock_on_background" v-model="cfg.security.lock_on_background" />
                  {{ i18n.t('settings.lock_on_background') }}
                </label>
                <div class="desc">{{ i18n.t('settings.lock_on_background_desc') }}</div>
              </div>
            </div>
            <!-- 免密解锁。只在这台电脑支持时出现——不支持时摆一个
                 灰按钮只会让人反复去点 -->
            <div class="row" v-if="dk.available">
              <label class="lb">{{ i18n.t('devicekey.enroll_title') }}</label>
              <div class="fld">
                <div v-if="dk.enrolled">
                  <div>{{ i18n.t('devicekey.enrolled') }}</div>
                  <button
                    type="button"
                    class="btn small"
                    data-sf="dk_forget"
                    :disabled="dkBusy"
                    @click="dkForget"
                  >
                    {{ i18n.t('devicekey.forget') }}
                  </button>
                </div>
                <div v-else>
                  <div class="desc">{{ i18n.t('devicekey.enroll_hint') }}</div>
                  <input
                    type="password"
                    data-sf="dk_password"
                    v-model="dkPassword"
                    autocomplete="current-password"
                    :placeholder="i18n.t('devicekey.enroll_password')"
                  />
                  <button
                    type="button"
                    class="btn small"
                    data-sf="dk_enroll"
                    :disabled="!dkPassword || dkBusy"
                    @click="dkEnroll"
                  >
                    {{ i18n.t('devicekey.enroll_submit') }}
                  </button>
                </div>
                <!-- 这两句必须都在。说错的后果不对称：第一句理解错会让
                     用户把密码设弱，第二句会让他丢数据 -->
                <div class="desc">{{ i18n.t('devicekey.warn_not_safer') }}</div>
                <div class="desc">{{ i18n.t('devicekey.warn_can_be_lost') }}</div>
                <div v-if="dkNotice" class="desc">{{ dkNotice }}</div>
              </div>
            </div>
            <div class="hint">
              <span aria-hidden="true">ⓘ</span>
              <span>{{ i18n.t('settings.recovery_hint') }}</span>
            </div>
          </template>

          <!-- 加密默认值 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'encrypt'">
            <div class="desc top">{{ i18n.t('settings.encrypt_defaults_note') }}</div>
            <div class="row">
              <label class="lb">{{ i18n.t('encrypt.strength') }}</label>
              <select data-sf="kdf_profile" v-model="cfg.defaults.kdf_profile">
                <option value="interactive">{{ i18n.t('encrypt.strength_interactive') }}</option>
                <option value="moderate">{{ i18n.t('encrypt.strength_moderate') }}</option>
                <option value="sensitive">{{ i18n.t('encrypt.strength_sensitive') }}</option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('encrypt.chunk_size') }}</label>
              <select data-sf="chunk_size" v-model="cfg.defaults.chunk_size">
                <option value="64K">64 KB</option>
                <option value="256K">256 KB</option>
                <option value="1M">1 MB</option>
                <option value="4M">4 MB</option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('encrypt.filename') }}</label>
              <div class="fld">
                <select data-sf="name_mode" v-model="cfg.defaults.name_mode">
                  <option value="encrypt">{{ i18n.t('encrypt.filename_encrypt') }}</option>
                  <option value="keep-ext">{{ i18n.t('encrypt.filename_keep_ext') }}</option>
                  <option value="plain">{{ i18n.t('encrypt.filename_plain') }}</option>
                </select>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('encrypt.compress') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="compress" v-model="cfg.compress.enabled" />
                  {{ i18n.t('settings.enabled_by_default') }}
                </label>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.original_action') }}</label>
              <div class="fld">
                <select data-sf="original_action" v-model="cfg.defaults.original_action">
                  <option value="keep">{{ i18n.t('settings.original_keep') }}</option>
                  <option value="trash">{{ i18n.t('settings.original_trash') }}</option>
                  <option value="delete">{{ i18n.t('encrypt.original_delete') }}</option>
                </select>
                <div class="desc">{{ i18n.t('settings.original_action_desc') }}</div>
              </div>
            </div>
          </template>

          <!-- 播放与预览 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'playback'">
            <div class="row">
              <label class="lb">{{ i18n.t('settings.thumbnails') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="thumbnails" v-model="cfg.ui.thumbnails" />
                  {{ i18n.t('settings.thumbnails_desc') }}
                </label>
                <div class="desc">{{ i18n.t('settings.thumbnails_hint') }}</div>
              </div>
            </div>
          </template>

          <!-- 设备与共享 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'devices'">
            <div class="row">
              <label class="lb">{{ i18n.t('settings.device_name') }}</label>
              <input
                type="text"
                data-sf="device_name"
                :value="cfg.serve.device_name || ''"
                @input="cfg.serve.device_name = $event.target.value"
              />
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.share_autostart') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="autostart" v-model="cfg.serve.autostart" />
                  {{ i18n.t('settings.share_autostart_desc') }}
                </label>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.paired_devices') }}</label>
              <div class="fld">
                <button class="btn small" type="button" data-sf="manage_devices" @click="openDevices">
                  {{ i18n.t('settings.manage_devices') }}
                </button>
                <div class="desc">{{ i18n.t('settings.manage_devices_desc') }}</div>
              </div>
            </div>
          </template>

          <!-- 关于 -->
          <template v-else>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.about_version') }}</label>
              <div class="fld">omy {{ aboutInfo.app_version }}</div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.about_build') }}</label>
              <div class="fld">
                <code>{{ aboutInfo.build_git || 'unknown' }}</code>
                <span class="desc">{{ aboutInfo.build_time || '' }}</span>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.about_format') }}</label>
              <div class="fld">
                <code>OMYFILE</code> v{{ aboutInfo.format_major }}.{{ aboutInfo.format_minor }}
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.about_license') }}</label>
              <div class="fld">{{ i18n.t('settings.about_license_value') }}</div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.config_file') }}</label>
              <div class="fld path">{{ paths.config }}</div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.log_dir') }}</label>
              <div class="fld">
                <div class="path">{{ paths.log || i18n.t('settings.log_none') }}</div>
                <button
                  v-if="paths.log"
                  class="btn"
                  data-sf="open-log"
                  @click="openLogDir"
                >
                  {{ i18n.t('settings.log_open') }}
                </button>
                <div class="desc">{{ i18n.t('settings.log_desc') }}</div>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.portable') }}</label>
              <div class="fld">
                {{ paths.portable ? i18n.t('settings.portable_yes') : i18n.t('settings.portable_no') }}
                <div class="desc">{{ i18n.t('settings.portable_desc') }}</div>
              </div>
            </div>
          </template>

          <div v-if="error" class="err">{{ error }}</div>
        </section>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 永久缓存的数值：与进度条同高，视觉上两行对齐，
   但**刻意不用进度条**——它没有分母 */
.pinnedval {
  font-size: 13px;
  padding-block: 2px;
}

.mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 50;
}
/* 移动端：设置不是盖住一切的模态，而是「底栏一个 tab 的内容区」。
   底栏（.pnav，z-index 30、高 52px + 安全区）必须一直露出并可点，所以
   mask 底部留出底栏高度、且去掉暗背景（不是弹窗、不该压暗背后）。 */
@media (max-width: 768px) {
  .mask {
    background: none;
    inset-block-end: calc(52px + env(safe-area-inset-bottom, 0px));
    z-index: 28;
  }
}
.setdlg {
  width: min(840px, 94vw);
  height: min(560px, 88vh);
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: var(--r-l);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
/* 移动端铺满：小屏上留边框只会让本就不多的空间更挤，
   而且底部要留安全区，否则手势导航条会盖住内容 */
.setdlg.mob {
  width: 100vw;
  /* 占满 mask（mask 已在底部让出了底栏高度），底栏由 .pnav 自己处理安全区，
     这里不再重复留 padding，否则底部会多出一条空白 */
  height: 100%;
  max-width: none;
  border-radius: 0;
  border: 0;
}
.seth {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 12px 14px;
  border-bottom: 1px solid var(--border);
  background: var(--bg2);
  flex: none;
}
.sethn {
  font-size: 15px;
  font-weight: 600;
  flex: 1;
}
.close {
  margin-inline-start: auto;
}
.setbody {
  flex: 1;
  display: grid;
  grid-template-columns: 172px 1fr;
  min-height: 0;
}
.setbody.mob {
  grid-template-columns: 1fr;
}
.setnav {
  background: var(--bg2);
  border-inline-end: 1px solid var(--border);
  padding: 8px 6px;
  overflow: auto;
}
.snav {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  background: none;
  border: 0;
  color: var(--fg);
  padding: 8px;
  border-radius: var(--r-s);
  cursor: pointer;
  text-align: start;
  font-size: 13px;
  /* 触控目标不小于 44px：移动端虽然走列表，但桌面端触屏设备也要能点中 */
  min-height: 36px;
}
.snav:hover {
  background: var(--bg3);
}
.snav.on {
  /* 实心底 + 白字一律用 --accent-solid，不要用 --accent：后者为了在
     暗背景上当边框/图标时显眼而调亮，白字压上去实测只有 3.10。
     这是同一个错误的第三处（前两处是 .btn.primary 与 .btn.pri）。 */
  background: var(--accent-solid);
  color: #fff;
}
.pane {
  padding: 16px 18px;
  overflow: auto;
}
.row {
  display: flex;
  align-items: flex-start;
  gap: 12px;
  padding: 11px 0;
  border-bottom: 1px solid var(--border);
  flex-wrap: wrap;
}
.row:last-of-type {
  border-bottom: 0;
}
.lb {
  width: 130px;
  flex: none;
  font-size: 13px;
  padding-top: 6px;
}
.fld {
  flex: 1;
  min-width: 180px;
}
/* 说明文字与控件左缘对齐，不额外缩进——缩进后会比控件右移一截，
   看起来像是属于别的项 */
.desc {
  font-size: 11.5px;
  color: var(--fg2);
  margin-top: 5px;
  line-height: 1.45;
}
.desc.top {
  margin: 0 0 10px;
}
.path {
  font-size: 11.5px;
  color: var(--fg2);
  word-break: break-all;
  padding-top: 6px;
}
/* 缓存用量进度条：轨道用中性色，填充用主强调色；零占用时只留一条空轨 */
.cachebar {
  height: 8px;
  border-radius: 4px;
  background: var(--bg3);
  border: 1px solid var(--border);
  overflow: hidden;
  margin: 4px 0 6px;
}
.cachebar-fill {
  height: 100%;
  background: var(--accent);
  border-radius: 4px;
  transition: width 0.2s ease;
}
.cachebar.zero .cachebar-fill {
  background: transparent;
}
.fld .btn.small {
  margin-top: 8px;
}
.grp {
  font-size: 11px;
  color: var(--fg2);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  font-weight: 600;
  margin: 18px 0 6px;
}
.hint {
  display: flex;
  gap: 8px;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 10px 12px;
  font-size: 12px;
  color: var(--fg2);
  margin-top: 14px;
  line-height: 1.5;
}
/* 二级页入口：整行可点，右侧箭头提示还有下一层 */
.subentry {
  display: flex;
  align-items: center;
  gap: 10px;
  width: 100%;
  text-align: start;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 12px 14px;
  margin-top: 18px;
  cursor: pointer;
  color: inherit;
}
.subentry:hover {
  border-color: var(--accent);
}
.se-ico {
  font-size: 18px;
  line-height: 1;
}
.se-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
  min-width: 0;
}
.se-title {
  font-size: 13px;
  font-weight: 600;
}
.se-desc {
  font-size: 12px;
  color: var(--fg2);
}
.se-arrow {
  color: var(--fg2);
  font-size: 18px;
}
/* 缓存二级页页头（桌面端；移动端用弹窗标题栏的返回） */
.panehead {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 12px;
}
.panetitle {
  font-size: 15px;
  font-weight: 700;
}
.pathrow {
  display: flex;
  align-items: center;
  gap: 8px;
}
.pathrow .path {
  flex: 1;
  min-width: 0;
}
.chk {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 13px;
  cursor: pointer;
  min-height: 28px;
}
.err {
  color: var(--danger);
  font-size: 12.5px;
  margin-top: 12px;
}
/* 移动端主列表 */
.mlist {
  overflow: auto;
}
/* 移动设置主页的分组标题（通用 / 其余分类） */
.mgh {
  padding: 14px 12px 6px;
  font-size: 12px;
  color: var(--fg2);
  font-weight: 600;
}
/* 通用组里一行一项：标签在左、选择器在右，整行触控目标不小于 48px */
.mqrow {
  display: flex;
  align-items: center;
  gap: 11px;
  min-height: 48px;
  padding: 7px 12px;
  border-bottom: 1px solid var(--border);
}
.mqlb {
  flex: 1;
  font-size: 13.5px;
  color: var(--fg);
}
.mqrow select {
  flex: none;
  max-width: 56%;
}
.mrow {
  display: flex;
  align-items: center;
  gap: 11px;
  width: 100%;
  padding: 12px;
  /* 触控目标 ≥48px */
  min-height: 48px;
  border: 0;
  border-bottom: 1px solid var(--border);
  background: none;
  color: var(--fg);
  cursor: pointer;
  text-align: start;
}
.mri {
  width: 22px;
  text-align: center;
  flex: none;
  font-size: 15px;
}
.mrt {
  flex: 1;
  font-size: 13.5px;
}
/* 行右侧的当前值摘要（已连接位置数 / 缓存用量 / 锁定时间 / 版本） */
.mrv {
  flex: none;
  max-width: 46%;
  font-size: 12.5px;
  color: var(--fg2);
  text-align: end;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.mra {
  color: var(--fg2);
  font-size: 15px;
  flex: none;
}
select,
input[type='text'] {
  background: var(--bg3);
  color: var(--fg);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 6px 9px;
  font-size: 13px;
  min-height: 34px;
  max-width: 100%;
}
</style>
