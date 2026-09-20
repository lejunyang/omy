<script setup>
/** 主界面：文件管理器。
 *
 * 布局取自 `docs/research/appendix/ui-prototype.html`：
 * 顶栏（品牌 / 搜索 / 状态 / 操作）+ 侧栏（位置 / 设备）+
 * 面包屑 + 内容区 + 状态栏。
 */

import { computed, ref, watch } from 'vue';
import * as i18n from '../i18n.js';
import { isMobile } from '../viewport.js';
import {
  state,
  visibleEntries,
  currentCount,
  encryptedCount,
  lockedCount,
  encryptable,
  restorable,
  keyManageable,
  crumbs,
  gotoCrumb,
  goUp,
  reload,
  toggleSelect,
  selectAll,
  clearSelection,
  clearNotice,
  setView,
} from '../store.js';
import AppShell from './AppShell.vue';
import EntryCard from './EntryCard.vue';

/* ---- 移动端 ----
 *
 * 与桌面共用这一个组件，只换外壳（外壳本身在 AppShell 里）。
 * 复制成两个组件的话，列表渲染、搜索、进度、统计会分叉，修一边漏一边。
 */

/** 移动端「选择模式」：已有选中项时，单击是加选而不是打开。 */
const selectionActive = computed(() => state.selected.length > 0);

/** 进度百分比，取整。
 *
 * total 为 0（空文件）时直接算 100，避免除零得到 NaN——
 * NaN 会让 width 样式失效，进度条看起来永远是空的。
 */
const progPercent = computed(() => {
  const p = state.progress;
  if (!p) return 0;
  if (!p.total) return 100;
  return Math.min(100, Math.round((p.done / p.total) * 100));
});

const emit = defineEmits([
  'open',
  'menu',
  'encrypt',
  'restore',
  'manage-key',
  'lock',
  'quick-unlock',
  'pick',
  'lang',
  'devices',
  'settings',
  'places',
  'add-place',
  'telegram',
]);

// 总大小按**当前看到的**条目算。在容器里时 `state.entries` 是外层
// 磁盘目录，拿它算出来的数字与眼前的列表无关
const totalSize = computed(() =>
  visibleEntries.value.reduce((s, e) => s + (e.size || 0), 0),
);

/** 把已识别的元信息挂到条目上，供卡片显示缩略图与分级。
 *
 * 容器内的条目没有磁盘路径，`state.known` 里查不到也不该查——
 * 那张表是按磁盘路径索引的，容器内相对路径可能与某个磁盘路径
 * 撞上，硬查会把别的文件的缩略图贴过来。
 */
const entriesWithMeta = computed(() =>
  visibleEntries.value.map((e) => {
    if (e.in_container) return e;
    const meta = state.known[e.path];
    return meta ? { ...e, meta } : e;
  }),
);

function onSelect(entry, ev) {
  toggleSelect(entry.path, ev.ctrlKey || ev.metaKey || ev.shiftKey);
}

/** 列表行的点击。移动端单击即打开，与网格卡片保持一致——
 *  两种视图的打开方式若不同，用户切一次视图就得重新学一遍。 */
function onRowClick(entry, ev) {
  if (isMobile.value && !selectionActive.value) {
    emit('open', entry);
    return;
  }
  toggleSelect(entry.path, isMobile.value || ev.ctrlKey || ev.metaKey || ev.shiftKey);
}

/** 列表行右键。与卡片同一套语义：先选中，再弹菜单。
 *
 * 不复用 onRowClick：那个函数处理 ctrl/shift 组合选择，右键不该有那些
 * 行为——在已选中的一批上右键必须保留整批选择。
 */
function onRowMenu(e, ev) {
  // 未选中就单选它。已在多选里的不动：用户选了 5 个再右键，
  // 意图显然是对这 5 个一起操作
  //
  // toggleSelect 的第二参为 false 时，若该项已是唯一选中项会**取消**
  // 选择，所以这里必须先判断未选中
  if (!state.selected.includes(e.path)) toggleSelect(e.path, false);
  emit('menu', { entry: e, x: ev.clientX, y: ev.clientY });
}
</script>

<template>
  <AppShell
    @pick="$emit('pick')"
    @devices="$emit('devices')"
    @lang="$emit('lang')"
    @add-place="$emit('add-place')"
    @telegram="$emit('telegram')"
    @places="$emit('places')"
    @files="() => {}"
    @settings="$emit('settings')"
    @lock="$emit('lock')"
    @quick-unlock="$emit('quick-unlock')"
  >
    <div class="main">
      <div class="crumb">
        <button
          class="crumbbtn"
          :disabled="!state.cwd && !state.container"
          :title="i18n.t('nav.up')"
          :aria-label="i18n.t('nav.up')"
          @click="goUp"
        >
          ↑
        </button>
        <button
          class="crumbbtn"
          :disabled="!state.cwd && !state.container"
          :title="i18n.t('nav.reload')"
          :aria-label="i18n.t('nav.reload')"
          @click="reload"
        >
          ⟳
        </button>

        <nav class="crumbpath">
          <!-- key 要带 kind：容器内的相对路径可能与磁盘段字符串相同，
               只用 path 做 key 会让 Vue 复用错的节点 -->
          <template v-for="(c, i) in crumbs" :key="c.kind + ':' + c.path">
            <span v-if="i > 0" class="sep">›</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1, box: c.kind === 'container' }"
              @click="gotoCrumb(c)"
            >
              <span v-if="c.kind === 'container'" aria-hidden="true">📦 </span>{{ c.name }}
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
          <button
            v-if="restorable.length"
            class="btn small"
            @click="$emit('restore')"
          >
            📤 {{ i18n.t('file.restore') }} ({{ restorable.length }})
          </button>
          <button
            v-if="keyManageable"
            class="btn small"
            :title="i18n.t('keymgmt.title')"
            @click="$emit('manage-key', keyManageable)"
          >
            🔑 {{ i18n.t('keymgmt.title') }}
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

          <!-- 有进度才显示进度条：派生密钥等阶段拿不到百分比，
               强行显示一个不动的空槽比不显示更让人以为卡住了 -->
          <!-- data-* 上带一份未经格式化的原始值：自动化与无障碍工具读它，
               不必去解析给人看的文案（文案随语言变，解析必然脆） -->
          <div
            v-if="state.progress"
            class="prog"
            :data-stage="state.progress.index"
            :data-stages="state.progress.total_files"
            :data-pct="progPercent"
          >
            <div class="prog-track">
              <div class="prog-fill" :style="{ width: progPercent + '%' }"></div>
            </div>
            <div class="prog-text">
              <span class="prog-name">{{ state.progress.name }}</span>
              <span class="prog-pct">{{ progPercent }}%</span>
            </div>
            <div v-if="state.progress.total_files > 1" class="prog-sub">
              {{ i18n.t('busy.file_of', {
                i: state.progress.index,
                n: state.progress.total_files,
              }) }}
            </div>
          </div>
        </div>

        <!-- 「没有匹配」与「这里是空的」要分清：前者是搜索词的结果，
             后者是位置本身为空。用 currentCount 而不是 state.entries，
             否则在容器里会拿外层磁盘目录的数量来做判断 -->
        <div v-else-if="!entriesWithMeta.length" class="empty">
          <div class="icon" aria-hidden="true">📭</div>
          <div class="title">
            {{ currentCount ? i18n.t('view.no_match') : i18n.t('view.empty_title') }}
          </div>
          <div v-if="!currentCount" class="sub">{{ i18n.t('view.empty_hint') }}</div>
        </div>

        <div v-else-if="state.view === 'grid'" class="grid">
          <EntryCard
            v-for="e in entriesWithMeta"
            :key="e.path"
            :entry="e"
            :selected="state.selected.includes(e.path)"
            :selection-active="selectionActive"
            @open="$emit('open', e)"
            @select="onSelect(e, $event)"
            @menu="$emit('menu', $event)"
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
            @click="onRowClick(e, $event)"
            @contextmenu.prevent="onRowMenu(e, $event)"
            @keydown.enter.prevent="$emit('open', e)"
          >
            <span class="ic">{{
              e.is_dir
                ? (e.is_encrypted_dir ? '🔐' : '📁')
                : e.is_encrypted
                  ? (e.unlocked ? '🔓' : '🔒')
                  : '📄'
            }}</span>
            <span class="nm">
              {{ e.is_encrypted && !e.unlocked ? i18n.t('file.locked_name') : e.real_name || e.name }}
            </span>
            <span class="sz">{{ e.size == null ? '' : i18n.formatSize(e.size) }}</span>
            <span class="tg">
              {{
                e.is_dir
                  ? (e.is_encrypted_dir ? i18n.t('kind.folder_encrypted') : i18n.t('kind.folder'))
                  : e.is_encrypted
                    ? i18n.t('kind.encrypted')
                    : (e.ext || '').toUpperCase()
              }}
            </span>
          </div>
        </div>
      </div>

      <div class="statusbar" :class="{ mob: isMobile }">
        <!-- 在容器里时说清这一点：列出来的名字是即时解密出来的，
             不是磁盘上的文件。少了这句，用户会以为这些文件就摆在硬盘上 -->
        <span v-if="state.container" class="cbadge">
          📦 {{ i18n.t('container.subtitle') }}
        </span>
        <span>{{ i18n.tn('status.files', currentCount) }}</span>
        <span v-if="encryptedCount">{{ i18n.tn('status.encrypted', encryptedCount) }}</span>
        <span v-if="lockedCount">🔒 {{ i18n.tn('status.locked_count', lockedCount) }}</span>
        <span v-if="state.selected.length">
          {{ i18n.tn('status.selected', state.selected.length) }}
        </span>
        <span class="spacer"></span>
        <span v-if="totalSize">{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
      </div>
    </div>
  </AppShell>

  <!-- 移动端底部导航：文件 / 远程 / 设备 / 设置。
       语言与主题原先各占一格，现在收进设置页的「通用」第一组——
       它们原本一点就切换，若收进去还要点两层才够到就是退步，
       所以在设置里放在最上面。 -->
  <!-- 提示条：成功提示会自动消失，错误留到用户主动关掉。
       错误若也自动消失就等于没报错——用户可能正低头看别处 -->
  <div v-if="state.error || state.notice" class="toast" :class="{ err: !!state.error }">
    <span>{{ state.error || state.notice }}</span>
    <button class="iconbtn" @click="state.error = ''; clearNotice()">✕</button>
  </div>
</template>
