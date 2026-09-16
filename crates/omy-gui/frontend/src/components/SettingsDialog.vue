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
import { state, setNotice } from '../store.js';

const emit = defineEmits(['close', 'lang', 'lock', 'devices']);

/** 当前分类。`cache` 是「远程位置」的二级页。 */
const pane = ref('general');
/** 移动端：null 表示停在主列表，否则是二级页的分类名。 */
const mobilePane = ref(null);

/** 整份配置。读失败时用空对象兜底，界面显示默认值而不是白屏。 */
const cfg = ref(null);
const paths = ref({ config: '', cache: '', data: '', portable: false });
const loading = ref(true);
const error = ref('');

/** 远程密文缓存的真实用量，来自后端（{used,limit,root}）。
 *  配置里的 cache_limit 是「将要保存」的值，这里是「磁盘上现在」的值。 */
const cacheUsage = ref({ used: 0, limit: 0, root: '' });
const clearing = ref(false);

/** 当前会话已装入的密码数量，安全页显示 + 判断「立即锁定」是否可点。 */
const loadedCount = ref(0);

/** 关于页的版本信息，来自后端编译期常量。 */
const aboutInfo = ref({ app_version: '', format_major: 1, format_minor: 0 });

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

/** 移动端进入二级页。 */
function goMobile(key) {
  mobilePane.value = key;
}

/** 进入「密文缓存」二级页（PC 与移动共用同一 pane）。 */
function openCachePane() {
  // isMobile 是 readonly(ref)，在 <script> 里必须取 .value，
  // 直接判断 ref 对象永远为真——那会让桌面端也走移动分支、PC 缓存页进不去
  if (isMobile.value) mobilePane.value = 'cache';
  else pane.value = 'cache';
  // 进页面前刷一次用量，避免看到上次的旧数字
  loadCacheUsage();
}

/** 移动端标题栏返回：缓存页的上级是远程位置，其余二级页回主列表。 */
function backMobile() {
  mobilePane.value = mobilePane.value === 'cache' ? 'remote' : null;
}

/** 移动端二级页标题（cache 不在 PANES 里，单独给名）。 */
const paneTitle = computed(() => {
  // 同上，isMobile 在 JS 中要取 .value
  const key = isMobile.value ? mobilePane.value : pane.value;
  if (key === 'cache') return i18n.t('settings.cache_title');
  return i18n.t(PANES.find((p) => p.key === key)?.label || 'settings.title');
});

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
        <button class="iconbtn close" data-si="close" :aria-label="i18n.t('common.close')" @click="onClose">
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

        <!-- 移动端主列表 -->
        <div v-if="isMobile && !mobilePane" class="mlist">
          <button
            v-for="p in PANES"
            :key="p.key"
            class="mrow"
            :data-sp="p.key"
            @click="goMobile(p.key)"
          >
            <span class="mri" aria-hidden="true">{{ p.icon }}</span>
            <span class="mrt">{{ i18n.t(p.label) }}</span>
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
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_used') }}</label>
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
.mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 50;
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
  height: 100vh;
  max-width: none;
  border-radius: 0;
  border: 0;
  padding-bottom: env(safe-area-inset-bottom, 0px);
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
  background: var(--accent);
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
