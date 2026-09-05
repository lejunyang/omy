<script setup>
/** 远端视图：浏览一台已连接设备共享的加密文件。
 *
 * # 为什么不复用文件管理器的那套渲染
 *
 * 看着像，但底下的模型完全不同：
 *
 * | | 本机 | 远端 |
 * |---|---|---|
 * | 有目录层级吗 | 有，能进出 | 没有，共享是一份平铺清单 |
 * | 能加密吗 | 能 | **不能**，共享是只读的 |
 * | 用什么定位 | 路径 | handle（对方的路径我们根本看不到）|
 *
 * 硬塞进 `EntryCard` 的话，每个分支都要写「如果是远端就……」，
 * 而「远端不能加密」这类约束会散落在十几个地方——漏一处就是
 * 用户点了加密然后收到一个没人看得懂的错误。
 *
 * 这里另起一套，反而让两边各自的规则都留在明面上。
 */

import { computed } from 'vue';
import * as i18n from '../i18n.js';
import { isMobile } from '../viewport.js';
import { state, remoteThumbUrl, disconnectRemote, reloadRemote } from '../store.js';

const emit = defineEmits(['open', 'unlock']);

/** 打开一个远端条目（移动端单击触发）。
 *
 * 触屏上没有双击这个手势：dblclick 在移动 WebView 里要么不触发，
 * 要么被系统当成双击缩放吃掉。只绑 dblclick 的结果是手机上远端文件
 * 一个都打不开，而界面看着完全正常——这种缺陷截图和 CSS 审查都发现不了。
 *
 * 与主界面（EntryCard、MainScreen 列表行）保持同一套语义：
 * 两个界面的打开方式若不同，用户切过来就得重新学一遍。
 */
function onEntryClick(f) {
  if (!isMobile.value || !f.unlocked) return;
  emit('open', f);
}

const visible = computed(() => {
  const q = state.query.trim().toLowerCase();
  if (!q) return state.remoteEntries;
  return state.remoteEntries.filter((f) => {
    // 锁定的没有可搜的名字。用 handle 去匹配没有意义，
    // 而且会让「搜什么都能搜到点东西」这种误导性行为出现
    if (!f.unlocked) return false;
    return (f.name || '').toLowerCase().includes(q);
  });
});

const lockedCount = computed(() => state.remoteEntries.filter((f) => !f.unlocked).length);
const openedCount = computed(() => state.remoteEntries.filter((f) => f.unlocked).length);

function icon(f) {
  if (!f.unlocked) return '🔒';
  switch (f.kind) {
    case 'video':
      return '🎬';
    case 'audio':
      return '🎵';
    case 'image':
      return '🖼️';
    case 'text':
      return '📝';
    default:
      return '📄';
  }
}
</script>

<template>
  <div class="main">
    <div class="crumb">
      <button
        class="crumbbtn"
        :title="i18n.t('remote.disconnect')"
        :aria-label="i18n.t('remote.disconnect')"
        @click="disconnectRemote"
      >
        ←
      </button>
      <button
        class="crumbbtn"
        :title="i18n.t('nav.reload')"
        :aria-label="i18n.t('nav.reload')"
        @click="reloadRemote"
      >
        ⟳
      </button>

      <nav class="crumbpath">
        <span class="remotetag">🌐</span>
        <button class="crumbseg cur">{{ state.peer?.name || i18n.t('remote.unknown') }}</button>
        <span class="sep">·</span>
        <span class="addr">{{ state.peer?.addr }}</span>
      </nav>

      <div class="vtoggle">
        <!-- 共享是只读的，所以这里没有加密按钮。
             不是「禁用」而是根本不出现——放个灰按钮反而让人反复去点 -->
        <button v-if="lockedCount" class="btn small primary" @click="$emit('unlock')">
          🔑 {{ i18n.t('remote.try_password') }}
        </button>
      </div>
    </div>

    <div class="content">
      <div v-if="state.busy" class="empty">
        <div class="icon" aria-hidden="true">⏳</div>
        <div class="title">{{ i18n.t(state.busyKey || 'busy.loading') }}</div>
      </div>

      <div v-else-if="!visible.length" class="empty">
        <div class="icon" aria-hidden="true">📭</div>
        <div class="title">
          {{ state.remoteEntries.length ? i18n.t('view.no_match') : i18n.t('remote.empty') }}
        </div>
        <div v-if="!state.remoteEntries.length" class="sub">{{ i18n.t('remote.empty_hint') }}</div>
      </div>

      <div v-else-if="state.view === 'grid'" class="grid">
        <div
          v-for="f in visible"
          :key="f.id"
          class="card"
          :class="{ locked: !f.unlocked }"
          tabindex="0"
          @dblclick="f.unlocked && $emit('open', f)"
          @click="onEntryClick(f)"
          @keydown.enter.prevent="f.unlocked && $emit('open', f)"
        >
          <div class="thumb">
            <img
              v-if="f.unlocked && f.has_thumb"
              :src="remoteThumbUrl(f.id)"
              alt=""
              loading="lazy"
            />
            <span v-else aria-hidden="true">{{ icon(f) }}</span>
            <span v-if="f.tier && f.tier !== 'p1'" class="tier">{{ f.tier.toUpperCase() }}</span>
          </div>
          <div class="cname">
            {{ f.unlocked ? f.name : i18n.t('file.locked_name') }}
          </div>
          <div class="cmeta">
            <!-- 锁定项只显示密文大小：明文大小本身也是信息 -->
            {{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}
          </div>
        </div>
      </div>

      <div v-else class="list">
        <div
          v-for="f in visible"
          :key="f.id"
          class="lrow"
          tabindex="0"
          @dblclick="f.unlocked && $emit('open', f)"
          @click="onEntryClick(f)"
          @keydown.enter.prevent="f.unlocked && $emit('open', f)"
        >
          <span class="ic">{{ icon(f) }}</span>
          <span class="nm">{{ f.unlocked ? f.name : i18n.t('file.locked_name') }}</span>
          <span class="sz">{{ i18n.formatSize(f.unlocked ? f.plaintext_size : f.size) }}</span>
          <span class="tg">{{ f.unlocked ? (f.kind || '') : i18n.t('kind.encrypted') }}</span>
        </div>
      </div>
    </div>

    <div class="statusbar">
      <span>🌐 {{ i18n.t('remote.connected_to', { name: state.peer?.name || '' }) }}</span>
      <span>{{ i18n.tn('status.files', state.remoteEntries.length) }}</span>
      <span v-if="openedCount">🔓 {{ i18n.tn('status.credentials', openedCount) }}</span>
      <span v-if="lockedCount">🔒 {{ i18n.tn('status.locked_count', lockedCount) }}</span>
      <span class="spacer"></span>
      <span class="readonly">{{ i18n.t('remote.readonly') }}</span>
    </div>
  </div>
</template>
