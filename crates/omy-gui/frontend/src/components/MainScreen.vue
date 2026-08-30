<script setup>
/** 主界面：文件管理器。
 *
 * 布局取自 `docs/research/appendix/ui-prototype.html`：
 * 顶栏（品牌 / 搜索 / 状态 / 操作）+ 侧栏（位置 / 设备）+
 * 面包屑 + 内容区 + 状态栏。
 */

import { computed } from 'vue';
import * as i18n from '../i18n.js';
import {
  state,
  visibleEntries,
  encryptedCount,
  lockedCount,
  encryptable,
  crumbs,
  navigate,
  goUp,
  reload,
  toggleSelect,
  selectAll,
  clearSelection,
} from '../store.js';
import { toggleTheme } from '../theme.js';
import SideBar from './SideBar.vue';
import EntryCard from './EntryCard.vue';

defineEmits(['open', 'encrypt', 'lock', 'quick-unlock', 'pick', 'lang']);

const totalSize = computed(() =>
  state.entries.reduce((s, e) => s + (e.size || 0), 0),
);

/** 把已识别的元信息挂到条目上，供卡片显示缩略图与分级。 */
const entriesWithMeta = computed(() =>
  visibleEntries.value.map((e) => {
    const meta = state.known[e.path];
    return meta ? { ...e, meta } : e;
  }),
);

function onSelect(entry, ev) {
  toggleSelect(entry.path, ev.ctrlKey || ev.metaKey || ev.shiftKey);
}
</script>

<template>
  <div class="titlebar">
    <span class="brand">{{ i18n.t('app.name') }}</span>

    <input
      v-model="state.query"
      class="search"
      type="search"
      :placeholder="i18n.t('view.search')"
      :aria-label="i18n.t('view.search')"
    />

    <span class="spacer"></span>

    <!-- 统一密码入口：不是门槛，是「顺手试一下」。
         没有密码时应用照常可用，这个框只影响加密文件能不能看见 -->
    <button
      class="pill"
      :class="{ ok: state.credentials > 0 }"
      :title="i18n.t('unlock.quick_hint')"
      @click="$emit('quick-unlock')"
    >
      {{ state.credentials > 0 ? '🔓' : '🔑' }}
      {{
        state.credentials > 0
          ? i18n.tn('status.credentials', state.credentials)
          : i18n.t('status.no_password')
      }}
    </button>

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
      @click="$emit('lang')"
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
      v-if="state.credentials > 0"
      class="iconbtn"
      :title="i18n.t('status.lock_now')"
      :aria-label="i18n.t('status.lock_now')"
      @click="$emit('lock')"
    >
      🔒
    </button>
  </div>

  <div class="body">
    <SideBar @pick="$emit('pick')" />

    <div class="main">
      <div class="crumb">
        <button
          class="crumbbtn"
          :disabled="!state.cwd"
          :title="i18n.t('nav.up')"
          :aria-label="i18n.t('nav.up')"
          @click="goUp"
        >
          ↑
        </button>
        <button
          class="crumbbtn"
          :disabled="!state.cwd"
          :title="i18n.t('nav.reload')"
          :aria-label="i18n.t('nav.reload')"
          @click="reload"
        >
          ⟳
        </button>

        <nav class="crumbpath">
          <template v-for="(c, i) in crumbs" :key="c.path">
            <span v-if="i > 0" class="sep">›</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1 }"
              @click="navigate(c.path)"
            >
              {{ c.name }}
            </button>
          </template>
        </nav>

        <div class="vtoggle">
          <button
            v-if="encryptable.length"
            class="btn small primary"
            @click="$emit('encrypt')"
          >
            🔒 {{ i18n.t('file.encrypt') }} ({{ encryptable.length }})
          </button>
          <button v-if="state.selected.length" class="btn small" @click="clearSelection">
            {{ i18n.t('view.clear_selection') }}
          </button>
        </div>
      </div>

      <div class="content">
        <!-- 还没选位置 -->
        <div v-if="!state.cwd" class="empty">
          <div class="icon" aria-hidden="true">📂</div>
          <div class="title">{{ i18n.t('view.start_title') }}</div>
          <div class="sub">{{ i18n.t('view.start_hint') }}</div>
        </div>

        <div v-else-if="state.busy" class="empty">
          <div class="icon" aria-hidden="true">⏳</div>
          <div class="title">{{ i18n.t(state.busyKey || 'busy.loading') }}</div>
        </div>

        <div v-else-if="!entriesWithMeta.length" class="empty">
          <div class="icon" aria-hidden="true">📭</div>
          <div class="title">
            {{ state.entries.length ? i18n.t('view.no_match') : i18n.t('view.empty_title') }}
          </div>
          <div v-if="!state.entries.length" class="sub">{{ i18n.t('view.empty_hint') }}</div>
        </div>

        <div v-else-if="state.view === 'grid'" class="grid">
          <EntryCard
            v-for="e in entriesWithMeta"
            :key="e.path"
            :entry="e"
            :selected="state.selected.includes(e.path)"
            @open="$emit('open', e)"
            @select="onSelect(e, $event)"
          />
        </div>

        <div v-else class="list">
          <div
            v-for="e in entriesWithMeta"
            :key="e.path"
            class="lrow"
            :class="{ sel: state.selected.includes(e.path) }"
            tabindex="0"
            @dblclick="$emit('open', e)"
            @click="onSelect(e, $event)"
            @keydown.enter.prevent="$emit('open', e)"
          >
            <span class="ic">{{ e.is_dir ? '📁' : e.is_encrypted ? (e.unlocked ? '🔓' : '🔒') : '📄' }}</span>
            <span class="nm">
              {{ e.is_encrypted && !e.unlocked ? i18n.t('file.locked_name') : e.real_name || e.name }}
            </span>
            <span class="sz">{{ e.size == null ? '' : i18n.formatSize(e.size) }}</span>
            <span class="tg">
              {{
                e.is_dir
                  ? i18n.t('kind.folder')
                  : e.is_encrypted
                    ? i18n.t('kind.encrypted')
                    : (e.ext || '').toUpperCase()
              }}
            </span>
          </div>
        </div>
      </div>

      <div class="statusbar">
        <span>{{ i18n.tn('status.files', state.entries.length) }}</span>
        <span v-if="encryptedCount">{{ i18n.tn('status.encrypted', encryptedCount) }}</span>
        <span v-if="lockedCount">🔒 {{ i18n.tn('status.locked_count', lockedCount) }}</span>
        <span v-if="state.selected.length">
          {{ i18n.tn('status.selected', state.selected.length) }}
        </span>
        <span class="spacer"></span>
        <span v-if="totalSize">{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
      </div>
    </div>
  </div>

  <!-- 提示条：成功与错误都在这里，不打断操作 -->
  <div v-if="state.error || state.notice" class="toast" :class="{ err: !!state.error }">
    <span>{{ state.error || state.notice }}</span>
    <button class="iconbtn" @click="state.error = ''; state.notice = ''">✕</button>
  </div>
</template>
