<script setup>
/** 云盘视图：浏览远程位置（首期 WebDAV）里的加密文件并点播。
 *
 * # 与 RemoteScreen（局域网对端）的区别
 *
 * | | 局域网对端 RemoteScreen | 云盘 PlaceBrowser |
 * |---|---|---|
 * | 目录层级 | 没有，一份平铺清单 | 有，能逐层进出 |
 * | 定位 | 对端给的 handle | 位置 id + 完整路径 |
 * | 入口 | 设备面板「连接」 | 底部「远程」tab / 侧栏入口 |
 *
 * 所以它比 RemoteScreen 多一层「位置列表」：没进入某个位置时先列出
 * 已保存的位置，点进去才是目录浏览。
 *
 * # 只读能力投影
 *
 * 首期所有云盘位置都只做浏览/点播，不渲染任何上传、新建、删除、重命名
 * 按钮——后端写命令也还没接。`caps` 已经在状态栏如实显示「只读/可写」，
 * 写链路在只读浏览稳定后再开，避免放出点了却报错的按钮。
 */

import { computed } from 'vue';
import * as i18n from '../i18n.js';
import { isMobile } from '../viewport.js';
import {
  state,
  currentCaps,
  openRemotePlace,
  leaveRemotePlace,
  reloadRemoteDir,
  enterRemoteDir,
  remoteGoUp,
  removeRemotePlace,
} from '../store.js';

const emit = defineEmits(['open', 'add']);

/** 当前位置元信息。 */
const currentPlace = computed(() =>
  state.remotePlaces.find((p) => p.id === state.remotePlace),
);

/** 面包屑分段：每段带可跳转的绝对目录。 */
const crumbs = computed(() => {
  const out = [];
  let acc = '';
  for (const seg of state.remoteDir.split('/').filter(Boolean)) {
    acc += `/${seg}`;
    out.push({ name: seg, dir: acc });
  }
  return out;
});

const visible = computed(() => {
  const q = state.query.trim().toLowerCase();
  if (!q) return state.remoteItems;
  // 锁定项没有可搜的明文名，只用真实名/目录名匹配，避免「搜什么都有」
  return state.remoteItems.filter((f) => {
    const name = f.is_dir ? f.name : f.unlocked ? f.real_name || f.name : '';
    return (name || '').toLowerCase().includes(q);
  });
});

/** 进入某个已保存位置。 */
async function open(p) {
  await openRemotePlace(p.id);
}

/** 从位置面包屑退回位置列表（仍停留在云盘视图）。 */
function leaveToList() {
  leaveRemotePlace();
}

/** 顶部返回：在目录里逐级上级，在位置列表则由父组件切回本地。 */
async function onBack() {
  if (state.remotePlace) await remoteGoUp();
  else emit('close');
}

async function reload() {
  if (state.remotePlace) await reloadRemoteDir();
}

/** 跳转到面包屑指定目录。 */
async function jump(c) {
  await enterRemoteDir(c.dir);
}

async function remove(p) {
  // 只删注册信息，不碰云端任何文件，所以不需要二次确认
  await removeRemotePlace(p.id);
}

/** 移动端单击：目录进入、可播放文件打开。桌面靠双击。 */
function onEntryClick(f) {
  if (!isMobile.value) return;
  activate(f);
}

function onEntryDbl(f) {
  activate(f);
}

/** 打开一个条目的统一入口：目录下钻，已解锁的加密文件交给父组件点播。
 *  锁定项与非加密文件首期不可在线打开（见模板里的提示）。 */
function activate(f) {
  if (f.is_dir) {
    enterRemoteDir(f.id);
    return;
  }
  if (f.is_encrypted && f.unlocked && !f.probe_failed) {
    emit('open', f);
  }
}

/** 列表/网格里显示的名字。 */
function displayName(f) {
  if (f.is_dir) return f.name;
  if (f.is_encrypted && f.unlocked) return f.real_name || f.name;
  return f.name;
}

/** 按真实文件名后缀给一个粗图标；识别不了就用通用文件图标。 */
function icon(f) {
  if (f.is_dir) return '📁';
  if (f.probe_failed) return '⚠️';
  if (!f.is_encrypted) return '📄';
  if (!f.unlocked) return '🔒';
  const n = (f.real_name || '').toLowerCase();
  if (/\.(mp4|mkv|webm|mov|avi|m4v|flv|wmv|ts)$/.test(n)) return '🎬';
  if (/\.(mp3|flac|aac|m4a|ogg|opus|wav|wma)$/.test(n)) return '🎵';
  if (/\.(png|jpe?g|gif|webp|bmp|avif|svg|heic|heif)$/.test(n)) return '🖼️';
  if (/\.(txt|md|log|json|srt|vtt)$/.test(n)) return '📝';
  return '📄';
}

/** 条目能否被激活（决定可点击观感）。 */
function activatable(f) {
  return f.is_dir || (f.is_encrypted && f.unlocked && !f.probe_failed);
}

function kindLabel(p) {
  return p.kind === 'nextcloud'
    ? i18n.t('rplace.vendor_nextcloud')
    : 'WebDAV';
}

function capsLabel() {
  const c = currentCaps.value;
  return c.write ? i18n.t('rplace.caps_writable') : i18n.t('rplace.readonly_badge');
}
</script>

<template>
  <div class="main">
    <div class="crumb">
      <button
        class="crumbbtn"
        :title="i18n.t('common.back')"
        :aria-label="i18n.t('common.back')"
        @click="onBack"
      >
        ←
      </button>
      <button
        v-if="state.remotePlace"
        class="crumbbtn"
        :title="i18n.t('nav.reload')"
        :aria-label="i18n.t('nav.reload')"
        @click="reload"
      >
        ⟳
      </button>

      <nav class="crumbpath">
        <span class="remotetag" aria-hidden="true">☁️</span>
        <template v-if="!state.remotePlace">
          <button class="crumbseg cur">{{ i18n.t('rplace.title') }}</button>
        </template>
        <template v-else>
          <button class="crumbseg" @click="leaveToList">{{ currentPlace?.name }}</button>
          <template v-for="(c, i) in crumbs" :key="c.dir">
            <span class="sep">/</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1 }"
              @click="jump(c)"
            >
              {{ c.name }}
            </button>
          </template>
        </template>
      </nav>

      <div class="vtoggle">
        <button class="btn small" data-pb="add" @click="$emit('add')">
          ＋ {{ i18n.t('rplace.add_short') }}
        </button>
      </div>
    </div>

    <div class="content">
      <!-- 一、位置列表 -->
      <div v-if="!state.remotePlace" class="list">
        <div v-if="!state.remotePlaces.length" class="empty">
          <div class="icon" aria-hidden="true">☁️</div>
          <div class="title">{{ i18n.t('rplace.empty') }}</div>
          <div class="sub">{{ i18n.t('rplace.empty_hint') }}</div>
          <button class="btn primary" style="margin-top: 12px" @click="$emit('add')">
            {{ i18n.t('rplace.add') }}
          </button>
        </div>

        <div
          v-for="p in state.remotePlaces"
          :key="p.id"
          class="lrow placerow"
          tabindex="0"
          @dblclick="open(p)"
          @keydown.enter.prevent="open(p)"
        >
          <span class="ic" aria-hidden="true">☁️</span>
          <span class="nm">{{ p.name }}</span>
          <span class="tg">{{ kindLabel(p) }}</span>
          <span class="rowactions">
            <button class="btn small" data-pb-enter="p.id" @click.stop="open(p)">
              {{ i18n.t('rplace.enter') }}
            </button>
            <button
              class="iconbtn"
              :aria-label="i18n.t('rplace.remove')"
              :title="i18n.t('rplace.remove')"
              @click.stop="remove(p)"
            >
              ✕
            </button>
          </span>
        </div>
      </div>

      <!-- 二、某个位置的目录浏览 -->
      <template v-else>
        <div v-if="state.busy" class="empty">
          <div class="icon" aria-hidden="true">⏳</div>
          <div class="title">{{ i18n.t(state.busyKey || 'busy.loading') }}</div>
        </div>

        <div v-else-if="state.placeError" class="empty">
          <div class="icon" aria-hidden="true">⚠️</div>
          <div class="title">{{ state.placeError }}</div>
          <button class="btn" style="margin-top: 12px" @click="reload">
            {{ i18n.t('rplace.retry') }}
          </button>
        </div>

        <div v-else-if="!visible.length" class="empty">
          <div class="icon" aria-hidden="true">📭</div>
          <div class="title">
            {{ state.remoteItems.length ? i18n.t('view.no_match') : i18n.t('rplace.empty_dir') }}
          </div>
        </div>

        <div v-else-if="state.view === 'grid'" class="grid">
          <div
            v-for="f in visible"
            :key="f.id"
            class="card"
            :class="{ locked: f.is_encrypted && !f.unlocked, off: !activatable(f) }"
            tabindex="0"
            @dblclick="onEntryDbl(f)"
            @click="onEntryClick(f)"
            @keydown.enter.prevent="onEntryDbl(f)"
          >
            <div class="thumb">
              <span aria-hidden="true">{{ icon(f) }}</span>
            </div>
            <div class="cname">{{ displayName(f) }}</div>
            <div class="cmeta">
              <template v-if="f.is_dir">📁</template>
              <template v-else>{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</template>
            </div>
          </div>
        </div>

        <div v-else class="list">
          <div
            v-for="f in visible"
            :key="f.id"
            class="lrow"
            :class="{ off: !activatable(f) }"
            tabindex="0"
            @dblclick="onEntryDbl(f)"
            @click="onEntryClick(f)"
            @keydown.enter.prevent="onEntryDbl(f)"
          >
            <span class="ic">{{ icon(f) }}</span>
            <span class="nm">{{ displayName(f) }}</span>
            <span class="sz">
              <template v-if="!f.is_dir">{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</template>
            </span>
            <span class="tg">
              <template v-if="f.is_dir">{{ i18n.t('rplace.folder') }}</template>
              <template v-else-if="f.probe_failed">{{ i18n.t('rplace.probe_failed') }}</template>
              <template v-else-if="!f.is_encrypted">{{ i18n.t('rplace.plain_file') }}</template>
              <template v-else-if="!f.unlocked">{{ i18n.t('kind.encrypted') }}</template>
            </span>
          </div>
        </div>
      </template>
    </div>

    <div class="statusbar">
      <span v-if="state.remotePlace">
        ☁️ {{ currentPlace?.name }}
      </span>
      <span v-else>☁️ {{ i18n.t('rplace.title') }}</span>
      <span v-if="state.remotePlace">{{ i18n.tn('status.files', visible.length) }}</span>
      <span class="spacer"></span>
      <span class="readonly">{{ capsLabel() }}</span>
    </div>
  </div>
</template>

<style scoped>
/* 位置行：让操作按钮靠右排成一组 */
.placerow {
  cursor: pointer;
}
.rowactions {
  margin-left: auto;
  display: flex;
  align-items: center;
  gap: 6px;
}
.rowactions .btn {
  padding: 3px 10px;
  font-size: 12px;
  min-height: 30px;
}
/* 不可激活的条目（锁定项、非加密文件、识别失败）降低对比并去掉指点光标 */
.lrow.off,
.card.off {
  opacity: 0.72;
}
.card.off {
  cursor: default;
}
</style>
