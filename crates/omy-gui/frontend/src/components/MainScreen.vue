<script setup lang="ts">
/** 主界面：文件管理器。
 *
 * 布局取自 `docs/research/appendix/ui-prototype.html`：
 * 顶栏（品牌 / 搜索 / 状态 / 操作）+ 侧栏（位置 / 设备）+
 * 面包屑 + 内容区 + 状态栏。
 */

import { ref, computed, watch, nextTick, useTemplateRef } from 'vue';
import * as i18n from '../i18n';
import { isMobile } from '../viewport';
import { fileClickAction } from '../file-interaction';
import { fileKind, KIND_ICON } from '../file-icons';
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
  setSort,
  type SortKey,
} from '../store';
import AppShell from './AppShell.vue';
import EntryCard from './EntryCard.vue';
import ContextMenu from './ContextMenu.vue';

/* ---- 移动端 ----
 *
 * 与桌面共用这一个组件，只换外壳（外壳本身在 AppShell 里）。
 * 复制成两个组件的话，列表渲染、搜索、进度、统计会分叉，修一边漏一边。
 */

/** 移动端顶栏标题：当前目录（或容器内目录）的名字。
 *  取面包屑最后一段，与桌面面包屑的「当前」段是同一个来源。 */
const mobileTitle = computed(() => {
  const last = crumbs.value[crumbs.value.length - 1];
  return last ? last.name : '';
});

/** 移动端顶栏副标题：条目数与加密数，替代桌面的状态栏。 */
const mobileSubtitle = computed(() => {
  if (!state.cwd) return '';
  const parts = [i18n.tn('status.files', currentCount.value)];
  if (lockedCount.value) parts.push(i18n.tn('status.locked_count', lockedCount.value));
  return parts.join(' · ');
});


/** 面包屑容器。路径一长，当前目录那一段就被挤出可视区（真机上实测只露出
 *  一个字），每次换目录都滚到最右端，保证用户看得到自己在哪。 */
const crumbEl = useTemplateRef<HTMLElement>('crumbEl');
watch(
  () => crumbs.value.map((c) => c.path).join('|'),
  async () => {
    await nextTick();
    const el = crumbEl.value;
    if (el) el.scrollLeft = el.scrollWidth;
  },
  { immediate: true },
);

/** 排序菜单：打开时记着按钮位置（桌面菜单贴着按钮弹出）。 */
const sortMenu = ref<{ x: number; y: number } | null>(null);

const SORT_KEYS = ['name', 'size', 'modified', 'type'] as const;

const sortItems = computed(() =>
  SORT_KEYS.map((k) => ({
    key: k,
    // 当前字段用 ✓ 标出，并写明方向
    icon: state.sortKey === k ? 'check' : 'minus',
    label: i18n.t(`sort.${k}`) + (state.sortKey === k
      ? ` · ${i18n.t(state.sortDir === 'asc' ? 'sort.asc' : 'sort.desc')}`
      : ''),
  })),
);

function openSortMenu(ev: MouseEvent) {
  const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
  sortMenu.value = { x: r.left, y: r.bottom + 4 };
}

function onSortPick(key: string) {
  sortMenu.value = null;
  if ((SORT_KEYS as readonly string[]).includes(key)) setSort(key as SortKey);
}

/** 列头上当前排序列的样式类（带方向箭头）。 */
function sortCls(key: SortKey) {
  return state.sortKey === key ? ['sorted', state.sortDir] : [];
}

/** 悬浮按钮的「新建」菜单是否打开。 */
const fabMenu = ref(false);

const fabItems = computed(() => [
  { key: 'pick', icon: 'folder-open', label: i18n.t('nav.pick_folder') },
  {
    key: 'new-folder',
    icon: 'plus',
    label: i18n.t('ctx.new_folder'),
    // 还没打开任何目录时无处可建
    disabled: !state.cwd || !!state.container,
  },
  { key: 'new-remote', icon: 'cloud', label: i18n.t('rplace.new') },
]);

function onFabPick(key: string) {
  fabMenu.value = false;
  if (key === 'pick') emit('pick');
  else if (key === 'new-folder') emit('new-folder');
  else if (key === 'new-remote') emit('new-remote');
}

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
  'new-folder',
  'delete-selected',
  'selection-more',
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
  'new-remote',
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

/** 「当前在哪」的唯一标识，供列表进场动画判断是否换了位置。
 *  容器内要带上容器 id 与内部路径：只用 cwd 的话，在容器里逐层进出时
 *  cwd 不变，动画不会重播，层级切换就没有视觉反馈。 */
const locationKey = computed(() =>
  state.container
    ? `${state.cwd}#${state.container.entryId}/${state.container.cwd}`
    : String(state.cwd),
);

function onSelect(entry, ev) {
  toggleSelect(entry.path, ev.ctrlKey || ev.metaKey || ev.shiftKey);
}

/** 列表行的点击。移动端单击即打开，与网格卡片保持一致——
 *  两种视图的打开方式若不同，用户切一次视图就得重新学一遍。 */
function onRowClick(entry, ev) {
  const action = fileClickAction(isMobile.value, rowLongFired);
  rowLongFired = false;
  // 列表与网格必须保持同一语义：移动端单击打开，长按才选择。
  if (action === 'ignore') return;
  if (action === 'open') {
    emit('open', entry);
    return;
  }
  toggleSelect(entry.path, ev.ctrlKey || ev.metaKey || ev.shiftKey);
}

/** 列表行移动端长按：进入多选（加选第一项）。网格卡片由 EntryCard 内部处理，
 *  列表行是这里自己渲染的，必须补同款长按，否则手机上列表视图无法多选。 */
const ROW_LONGPRESS_MS = 500;
const ROW_MOVE_TOLERANCE = 10;
let rowTimer: ReturnType<typeof setTimeout> | null = null;
let rowSX = 0;
let rowSY = 0;
let rowLongFired = false;

function onRowPointerDown(e, ev) {
  if (!isMobile.value || ev.pointerType === 'mouse') return;
  rowSX = ev.clientX;
  rowSY = ev.clientY;
  rowLongFired = false;
  if (rowTimer) clearTimeout(rowTimer);
  rowTimer = setTimeout(() => {
    rowLongFired = true;
    // 长按进入多选：未选则加选；已选则保持，交给随后的右键/菜单。
    if (!state.selected.includes(e.path)) toggleSelect(e.path, true);
  }, ROW_LONGPRESS_MS);
}
function onRowPointerMove(ev) {
  if (!rowTimer) return;
  if (Math.abs(ev.clientX - rowSX) > ROW_MOVE_TOLERANCE
      || Math.abs(ev.clientY - rowSY) > ROW_MOVE_TOLERANCE) {
    clearTimeout(rowTimer);
    rowTimer = null;
  }
}
function onRowPointerUp() {
  if (rowTimer) { clearTimeout(rowTimer); rowTimer = null; }
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
    :title="mobileTitle"
    :subtitle="mobileSubtitle"
    tab="files"
    @pick="$emit('pick')"
    @devices="$emit('devices')"
    @lang="$emit('lang')"
    @new-remote="$emit('new-remote')"
    @places="$emit('places')"
    @files="() => {}"
    @settings="$emit('settings')"
    @lock="$emit('lock')"
    @quick-unlock="$emit('quick-unlock')"
  >
    <div class="main" :class="{ mob: isMobile }">
      <div class="crumb" :class="{ mob: isMobile }">
        <button
          class="crumbbtn"
          :disabled="!state.cwd && !state.container"
          :title="i18n.t('nav.up')"
          :aria-label="i18n.t('nav.up')"
          @click="goUp"
        >
          <AppIcon name="arrowLeft" />
        </button>
        <!-- 刷新按钮只在桌面显示：手机这一行要留给面包屑与视图切换，
             而本地目录每次进入都会重新读取，手机上很少需要原地刷新 -->
        <button
          v-if="!isMobile"
          class="crumbbtn"
          :disabled="!state.cwd && !state.container"
          :title="i18n.t('nav.reload')"
          :aria-label="i18n.t('nav.reload')"
          @click="reload"
        >
          <AppIcon name="refresh" />
        </button>

        <nav ref="crumbEl" class="crumbpath">
          <!-- key 要带 kind：容器内的相对路径可能与磁盘段字符串相同，
               只用 path 做 key 会让 Vue 复用错的节点 -->
          <template v-for="(c, i) in crumbs" :key="c.kind + ':' + c.path">
            <span v-if="i > 0" class="sep">›</span>
            <button
              class="crumbseg"
              :class="{ cur: i === crumbs.length - 1, box: c.kind === 'container' }"
              @click="gotoCrumb(c)"
            >
              <span v-if="c.kind === 'container'" aria-hidden="true"><AppIcon name="package" /> </span>{{ c.name }}
            </button>
          </template>
        </nav>

        <!-- 排序：网格与列表都要能排（列表视图另有可点的列头） -->
        <button
          class="crumbbtn"
          data-tb="sort"
          :title="i18n.t('sort.title')"
          :aria-label="i18n.t('sort.title')"
          @click="openSortMenu"
        >
          <AppIcon name="sort" />
        </button>

        <!-- 移动端：视图切换收成一个按钮放在面包屑尾部。桌面上它在顶栏，
             手机顶栏留给标题与搜索 -->
        <button
          v-if="isMobile"
          class="crumbbtn"
          data-tb="view-toggle"
          :title="state.view === 'grid' ? i18n.t('view.list') : i18n.t('view.grid')"
          :aria-label="state.view === 'grid' ? i18n.t('view.list') : i18n.t('view.grid')"
          @click="setView(state.view === 'grid' ? 'list' : 'grid')"
        >
          <AppIcon :name="state.view === 'grid' ? 'list' : 'grid'" />
        </button>

        <!-- 桌面工具条。移动端改为底部浮动的选择操作条（见下方 .selbar），
             挤在面包屑行里的话按钮只剩几个像素宽 -->
        <div v-if="!isMobile" class="vtoggle">
          <button
            v-if="encryptable.length"
            class="btn small primary"
            @click="$emit('encrypt')"
          >
            <AppIcon name="lock" /> {{ i18n.t('file.encrypt') }} ({{ encryptable.length }})
          </button>
          <button
            v-if="restorable.length"
            class="btn small"
            @click="$emit('restore')"
          >
            <AppIcon name="external" /> {{ i18n.t('file.restore') }} ({{ restorable.length }})
          </button>
          <button
            v-if="keyManageable"
            class="btn small"
            :title="i18n.t('keymgmt.title')"
            @click="$emit('manage-key', keyManageable)"
          >
            <AppIcon name="key" /> {{ i18n.t('keymgmt.title') }}
          </button>
          <button v-if="state.selected.length" class="btn small" @click="clearSelection">
            {{ i18n.t('view.clear_selection') }}
          </button>
        </div>
      </div>

      <div class="content">
        <!-- 还没选位置 -->
        <div v-if="!state.cwd" class="empty">
          <div class="icon" aria-hidden="true"><AppIcon name="folder-open" /></div>
          <div class="title">{{ i18n.t('view.start_title') }}</div>
          <div class="sub">{{ i18n.t('view.start_hint') }}</div>
        </div>

        <div v-else-if="state.busy" class="empty busy" :class="{ instant: !!state.progress }">
          <div class="icon" aria-hidden="true"><AppIcon name="loader" /></div>
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
          <div class="icon" aria-hidden="true"><AppIcon name="inbox" /></div>
          <div class="title">
            {{ currentCount ? i18n.t('view.no_match') : i18n.t('view.empty_title') }}
          </div>
          <div v-if="!currentCount" class="sub">{{ i18n.t('view.empty_hint') }}</div>
        </div>

        <!-- key 绑定当前位置：换目录时整块重建，错峰进场动画（.anim-in）才会
             重新播放；同目录里的刷新、选择变化不换 key，不会每点一下就闪一遍 -->
        <div v-else-if="state.view === 'grid'" :key="'g:' + locationKey" class="grid anim-in">
          <EntryCard
            v-for="(e, i) in entriesWithMeta"
            :key="e.path"
            :entry="e"
            :selected="state.selected.includes(e.path)"
            :style="{ '--i': i }"
            @open="$emit('open', e)"
            @select="onSelect(e, $event)"
            @menu="$emit('menu', $event)"
          />
        </div>

        <div v-else :key="'l:' + locationKey" class="list anim-in">
          <!-- 列头：点一下按该列排序，再点切换升降序。手机上列放不下，只靠排序按钮 -->
          <div v-if="!isMobile" class="lhead" role="row">
            <span class="ic"></span>
            <button class="nm" :class="sortCls('name')" @click="setSort('name')">{{ i18n.t('sort.name') }}</button>
            <button class="sz" :class="sortCls('size')" @click="setSort('size')">{{ i18n.t('sort.size') }}</button>
            <button class="tm" :class="sortCls('modified')" @click="setSort('modified')">{{ i18n.t('sort.modified') }}</button>
            <button class="tg" :class="sortCls('type')" @click="setSort('type')">{{ i18n.t('sort.type') }}</button>
          </div>
          <div
            v-for="(e, i) in entriesWithMeta"
            :key="e.path"
            class="lrow"
            :class="{ sel: state.selected.includes(e.path) }"
            :style="{ '--i': i }"
            tabindex="0"
            @dblclick="$emit('open', e)"
            @click="onRowClick(e, $event)"
            @contextmenu.prevent="onRowMenu(e, $event)"
            @pointerdown="onRowPointerDown(e, $event)"
            @pointermove="onRowPointerMove($event)"
            @pointerup="onRowPointerUp"
            @pointercancel="onRowPointerUp"
            @keydown.enter.prevent="$emit('open', e)"
          >
            <span class="ic" :class="'k-' + fileKind(e)"><AppIcon :name="KIND_ICON[fileKind(e)]" /></span>
            <span class="nm">
              {{ e.is_encrypted && !e.unlocked ? i18n.t('file.locked_name') : e.real_name || e.name }}
            </span>
            <span class="sz">{{ e.size == null ? '' : i18n.formatSize(e.size) }}</span>
            <span v-if="!isMobile" class="tm">{{ i18n.formatDateTime(e.modified) }}</span>
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

      <!-- 状态栏只在桌面显示。手机上文件数挪到顶栏副标题，选择数由底部
           操作条显示——三处都放会把本就不高的屏幕再吃掉一行 -->
      <div v-if="!isMobile" class="statusbar">
        <!-- 在容器里时说清这一点：列出来的名字是即时解密出来的，
             不是磁盘上的文件。少了这句，用户会以为这些文件就摆在硬盘上 -->
        <span v-if="state.container" class="cbadge">
          <AppIcon name="package" /> {{ i18n.t('container.subtitle') }}
        </span>
        <span>{{ i18n.tn('status.files', currentCount) }}</span>
        <span v-if="encryptedCount">{{ i18n.tn('status.encrypted', encryptedCount) }}</span>
        <span v-if="lockedCount"><AppIcon name="lock" /> {{ i18n.tn('status.locked_count', lockedCount) }}</span>
        <span v-if="state.selected.length">
          {{ i18n.tn('status.selected', state.selected.length) }}
        </span>
        <span class="spacer"></span>
        <span v-if="totalSize">{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
      </div>
    </div>

    <!-- 移动端选择操作条：长按进入多选后从底部升起，替代桌面面包屑行里的
         工具按钮。浮在底栏之上而不是替换底栏：用户在多选中途仍可能切 tab，
         隐藏导航会让人以为被困在了多选模式里 -->
    <Transition name="selbar">
      <div v-if="isMobile && state.selected.length" class="selbar" role="toolbar">
        <button class="iconbtn" :aria-label="i18n.t('view.clear_selection')" @click="clearSelection">
          <AppIcon name="close" />
        </button>
        <span class="selbar-count">{{ i18n.tn('status.selected', state.selected.length) }}</span>
        <span class="spacer"></span>
        <button
          v-if="state.selected.length < entriesWithMeta.length"
          class="selbar-act"
          @click="selectAll"
        >
          <AppIcon name="check" /><span>{{ i18n.t('view.select_all') }}</span>
        </button>
        <button v-if="encryptable.length" class="selbar-act pri" @click="$emit('encrypt')">
          <AppIcon name="lock" /><span>{{ i18n.t('file.encrypt') }}</span>
        </button>
        <button v-if="restorable.length" class="selbar-act" @click="$emit('restore')">
          <AppIcon name="external" /><span>{{ i18n.t('file.restore') }}</span>
        </button>
        <!-- 密码管理收进「更多」：操作条宽度有限，一行放不下五个按钮 -->
        <!-- 删除与「更多」：原来操作条只有全选 / 加密，删、改名、上传都要再长按
             一次去找菜单 -->
        <button class="selbar-act" data-selbar="delete" @click="$emit('delete-selected')">
          <AppIcon name="trash" /><span>{{ i18n.t('view.delete_short') }}</span>
        </button>
        <button class="selbar-act" data-selbar="more" @click="$emit('selection-more')">
          <AppIcon name="more" /><span>{{ i18n.t('view.more') }}</span>
        </button>
      </div>
    </Transition>

    <!-- 移动端「打开文件夹」悬浮按钮。桌面上它在侧栏里；手机侧栏收进了抽屉，
         这是首次使用时唯一的起点，藏两层太深。多选时让位给操作条 -->
    <Transition name="fab">
      <button
        v-if="isMobile && !state.selected.length"
        class="fab"
        data-tb="fab-pick"
        :aria-label="i18n.t('nav.new_title')"
        :title="i18n.t('nav.new_title')"
        @click="fabMenu = true"
      >
        <AppIcon name="plus" :size="24" :stroke-width="2.2" />
      </button>
    </Transition>

    <Transition name="menu">
      <ContextMenu
        v-if="sortMenu"
        :items="sortItems"
        :x="sortMenu.x"
        :y="sortMenu.y"
        :title="i18n.t('sort.title')"
        @pick="onSortPick"
        @close="sortMenu = null"
      />
    </Transition>

    <!-- 悬浮按钮的「新建」菜单。原来它只做「打开文件夹」，可「+」读起来是
         「新建」——想建文件夹、加远程位置的人点进去却弹出目录选择器 -->
    <Transition name="menu">
      <ContextMenu
        v-if="fabMenu"
        :items="fabItems"
        :x="0"
        :y="0"
        :title="i18n.t('nav.new_title')"
        @pick="onFabPick"
        @close="fabMenu = false"
      />
    </Transition>
  </AppShell>

  <!-- 移动端底部导航：文件 / 远程 / 设备 / 设置。
       语言与主题原先各占一格，现在收进设置页的「通用」第一组——
       它们原本一点就切换，若收进去还要点两层才够到就是退步，
       所以在设置里放在最上面。 -->
  <!-- 提示条：成功提示会自动消失，错误留到用户主动关掉。
       错误若也自动消失就等于没报错——用户可能正低头看别处 -->
  <Transition name="toast">
    <div v-if="state.error || state.notice" class="toast" :class="{ err: !!state.error }" role="status">
      <AppIcon :name="state.error ? 'circle-alert' : 'circle-check'" />
      <span>{{ state.error || state.notice }}</span>
      <button class="iconbtn" :aria-label="i18n.t('actions.close')" @click="state.error = ''; clearNotice()">
        <AppIcon name="close" />
      </button>
    </div>
  </Transition>
</template>
