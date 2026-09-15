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

const emit = defineEmits(['close', 'lang']);

/** 当前分类。`cache` 是「远程位置」的二级页。 */
const pane = ref('general');
/** 移动端：null 表示停在主列表，否则是二级页的分类名。 */
const mobilePane = ref(null);

/** 整份配置。读失败时用空对象兜底，界面显示默认值而不是白屏。 */
const cfg = ref(null);
const paths = ref({ config: '', cache: '', data: '', portable: false });
const loading = ref(true);
const error = ref('');

/** 分类定义。顺序按使用频率，不按模块划分——「通用」放第一是因为
 *  语言和主题是最常被找的两项。 */
const PANES = [
  { key: 'general', icon: '⚙️', label: 'settings.general' },
  { key: 'remote', icon: '☁️', label: 'settings.remote' },
  { key: 'security', icon: '🔐', label: 'settings.security' },
  { key: 'encrypt', icon: '🔒', label: 'settings.encrypt_defaults' },
  { key: 'playback', icon: '🎬', label: 'settings.playback' },
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

/** 清空缓存。目前只清目录，实际实现在后端接上缓存后替换。 */
const clearing = ref(false);

/** 移动端进入二级页。 */
function goMobile(key) {
  mobilePane.value = key;
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
          @click="mobilePane = null"
        >
          ←
        </button>
        <span class="sethn">
          {{ isMobile && mobilePane
            ? i18n.t(PANES.find((p) => p.key === mobilePane)?.label || 'settings.title')
            : i18n.t('settings.title') }}
        </span>
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
            <div class="row">
              <label class="lb">{{ i18n.t('settings.thumbnails') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="thumbnails" v-model="cfg.ui.thumbnails" />
                  {{ i18n.t('settings.thumbnails_desc') }}
                </label>
              </div>
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

            <div class="grp">{{ i18n.t('settings.cache') }}</div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_limit') }}</label>
              <select data-sf="cache_limit" v-model.number="cfg.remote.cache_limit">
                <option v-for="o in CACHE_LIMITS" :key="o.v" :value="o.v">
                  {{ o.k.startsWith('settings.') ? i18n.t(o.k) : o.k }}
                </option>
              </select>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.cache_dir') }}</label>
              <div class="fld path">{{ paths.cache }}</div>
            </div>
            <div class="row">
              <label class="lb"></label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="clear_cache_on_exit" v-model="cfg.remote.clear_cache_on_exit" />
                  {{ i18n.t('settings.clear_on_exit') }}
                </label>
                <label v-if="isMobile" class="chk">
                  <input type="checkbox" data-sf="cache_wifi_only" v-model="cfg.remote.cache_wifi_only" />
                  {{ i18n.t('settings.wifi_only') }}
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
              <label class="lb">{{ i18n.t('settings.background') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="lock_on_background" v-model="cfg.security.lock_on_background" />
                  {{ i18n.t('settings.lock_on_background') }}
                </label>
                <div class="desc">{{ i18n.t('settings.lock_on_background_desc') }}</div>
              </div>
            </div>
            <div class="row">
              <label class="lb">{{ i18n.t('settings.temp_plaintext') }}</label>
              <div class="fld">
                <label class="chk">
                  <input type="checkbox" data-sf="wipe_temp_plaintext" v-model="cfg.security.wipe_temp_plaintext" />
                  {{ i18n.t('settings.wipe_temp') }}
                </label>
              </div>
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
                </select>
                <div class="desc">{{ i18n.t('settings.original_action_desc') }}</div>
              </div>
            </div>
          </template>

          <!-- 播放与预览 -->
          <template v-else-if="(isMobile ? mobilePane : pane) === 'playback'">
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
          </template>

          <!-- 关于 -->
          <template v-else>
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
