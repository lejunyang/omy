<script setup>
/** 主界面：顶栏 + 侧栏 + 文件区 + 状态栏。
 *
 * # 搜索框不再需要「只重绘内容区」的技巧
 *
 * 重构前每敲一个字都要整页 `innerHTML = ...`，这会让输入框
 * 被替换成新节点从而失焦。当时的解法是手动只重绘 `#content`，
 * 再重新绑定其中的事件——一处很容易出错的特例。
 *
 * Vue 的 diff 会原地复用输入框节点，焦点自然保持，
 * 这段特例逻辑连同它的注释一起消失了。
 */

import * as i18n from '../i18n.js';
import { state, visibleFiles, totalSize, pickFolder, switchLanguage } from '../store.js';
import { toggleTheme } from '../theme.js';
import FileCard from './FileCard.vue';
import FileList from './FileList.vue';

// lock 通过事件上抛而不是直接调 store：锁定时必须先关掉预览层，
// 那是 App 的职责。直接调 store.lock 会让预览停留在已清空的列表上
defineEmits(['open', 'lock']);

/** 侧栏只显示目录名，完整路径放 title。 */
function baseName(p) {
  return p.split(/[\\/]/).filter(Boolean).pop() || p;
}
</script>

<template>
  <div class="topbar">
    <span class="brand">{{ i18n.t('app.name') }}</span>
    <input
      v-model="state.query"
      class="search"
      type="search"
      :placeholder="i18n.t('view.search')"
      :aria-label="i18n.t('view.search')"
    />
    <span class="spacer"></span>
    <span class="lockstate" data-on="1">🔓 {{ i18n.t('status.unlocked') }}</span>
    <button
      class="iconbtn"
      :aria-pressed="state.view === 'grid'"
      :title="i18n.t('view.grid')"
      :aria-label="i18n.t('view.grid')"
      @click="state.view = 'grid'"
    >
      ⊞
    </button>
    <button
      class="iconbtn"
      :aria-pressed="state.view === 'list'"
      :title="i18n.t('view.list')"
      :aria-label="i18n.t('view.list')"
      @click="state.view = 'list'"
    >
      ☰
    </button>
    <button
      class="iconbtn"
      :title="i18n.t('lang.toggle')"
      :aria-label="i18n.t('lang.toggle')"
      @click="switchLanguage"
    >
      🌐
    </button>
    <button
      class="iconbtn"
      :title="i18n.t('theme.toggle')"
      :aria-label="i18n.t('theme.toggle')"
      @click="toggleTheme"
    >
      ◐
    </button>
    <button
      class="iconbtn"
      :title="i18n.t('status.lock_now')"
      :aria-label="i18n.t('status.lock_now')"
      @click="$emit('lock')"
    >
      🔒
    </button>
  </div>

  <div class="body">
    <aside class="sidebar">
      <div class="side-title">{{ i18n.t('app.tagline') }}</div>
      <button v-for="r in state.roots" :key="r" class="side-item" :title="r">
        📂 {{ baseName(r) }}
      </button>
      <button class="side-item" @click="pickFolder">➕ {{ i18n.t('unlock.browse') }}</button>
    </aside>

    <div class="main">
      <div class="toolbar">
        <span>{{ i18n.tn('status.files', visibleFiles.length) }}</span>
      </div>
      <div class="content">
        <div v-if="!visibleFiles.length" class="empty">
          <div class="icon" aria-hidden="true">📂</div>
          <div class="title">
            {{ state.files.length ? i18n.t('view.no_match') : i18n.t('view.empty_title') }}
          </div>
          <div v-if="!state.files.length">{{ i18n.t('view.empty_hint') }}</div>
        </div>

        <div v-else-if="state.view === 'grid'" class="grid">
          <FileCard
            v-for="f in visibleFiles"
            :key="f.id"
            :file="f"
            @dblclick="f.unlocked && $emit('open', f.id)"
            @keydown.enter.prevent="f.unlocked && $emit('open', f.id)"
            @keydown.space.prevent="f.unlocked && $emit('open', f.id)"
          />
        </div>

        <FileList v-else :files="visibleFiles" @open="$emit('open', $event)" />
      </div>
    </div>
  </div>

  <div class="statusbar">
    <span>{{ i18n.tn('status.files', state.files.length) }}</span>
    <span>{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
    <span class="spacer"></span>
    <span>{{ i18n.tn('status.credentials', state.credentials) }}</span>
  </div>
</template>
