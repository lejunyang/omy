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
  enterRemoteDir,
  setView,
} from '../store.js';
import SideBar from './SideBar.vue';
import EntryCard from './EntryCard.vue';

/** 远程条目的图标。
 *
 * 三种异常状态要能一眼区分：探测失败（网络）、锁定（密码不对）、
 * 正常。用同一个图标的话，用户会把网络故障当成密码问题。
 */
function remoteIcon(it) {
  if (it.probe_failed) return '⚠️';
  if (it.is_dir) return '📁';
  if (it.is_encrypted && !it.unlocked) return '🔒';
  if (it.is_encrypted) return '🔓';
  return '📄';
}

/** 打开一个远程条目。
 *
 * 目录进去；文件暂不预览——远程预览要先把 RemoteSource 接到
 * omystream 协议上，那是下一步。现在点文件不做任何事，
 * 而不是报一个看不懂的错。
 */
async function onRemoteOpen(it) {
  if (it.is_dir) await enterRemoteDir(it.id);
}

/* ---- 移动端外壳 ----
 *
 * 与桌面共用这一个组件，只换外壳：侧栏改抽屉、底栏改导航。
 * 复制成两个组件的话，列表渲染、搜索、进度、统计会分叉，
 * 修一边漏一边。
 */

/** 抽屉（移动端的侧栏）是否展开。 */
const drawer = ref(false);
/** 移动端底部导航的当前页。 */
const tab = ref('files');

// 切回桌面时必须收起抽屉：抽屉在桌面布局里是个盖住半屏的浮层，
// 留着它会挡住文件列表，而桌面上没有关掉它的入口
watch(isMobile, (m) => {
  if (!m) drawer.value = false;
});

/** 移动端点了侧栏里的位置后要自动收起抽屉，否则挡着刚打开的目录。 */
function onDrawerNavigate() {
  drawer.value = false;
}

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
  <div class="titlebar" :class="{ mob: isMobile }">
    <!-- 移动端：汉堡键代替常驻侧栏。280px 宽度放不下侧栏 + 网格 -->
    <button
      v-if="isMobile"
      class="iconbtn"
      :title="i18n.t('nav.menu')"
      :aria-label="i18n.t('nav.menu')"
      :aria-expanded="drawer"
      @click="drawer = !drawer"
    >
      ☰
    </button>

    <span v-if="!isMobile" class="brand">{{ i18n.t('app.name') }}</span>

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

    <!-- 移动端顶栏只留密码与上锁：屏幕宽度有限，
         视图/语言/主题这些低频项收进抽屉，避免图标挤成一排点不准 -->
    <template v-if="!isMobile">
      <button
        class="iconbtn"
        data-tb="view-grid"
        :aria-pressed="state.view === 'grid'"
        :title="i18n.t('view.grid')"
        :aria-label="i18n.t('view.grid')"
        @click="setView('grid')"
      >
        ⊞
      </button>
      <button
        class="iconbtn"
        data-tb="view-list"
        :aria-pressed="state.view === 'list'"
        :title="i18n.t('view.list')"
        :aria-label="i18n.t('view.list')"
        @click="setView('list')"
      >
        ☰
      </button>
      <button
        class="iconbtn"
        :title="i18n.t('settings.title')"
        :aria-label="i18n.t('settings.title')"
        data-tb="settings"
        @click="$emit('settings')"
      >
        ⚙️
      </button>
    </template>
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

  <div class="body" :class="{ mob: isMobile }">
    <!-- 抽屉遮罩：点空白处收起。移动端没有 Esc 键，
         没有这层遮罩，抽屉一旦打开就只能靠汉堡键关 -->
    <div
      v-if="isMobile && drawer"
      class="scrim"
      @click="drawer = false"
    ></div>

    <SideBar
      :class="{ drawer: isMobile, open: drawer }"
      :mobile="isMobile"
      @pick="$emit('pick'); onDrawerNavigate()"
      @devices="$emit('devices'); onDrawerNavigate()"
      @navigate="onDrawerNavigate"
      @lang="$emit('lang')"
      @add-place="$emit('add-place')"
      @telegram="$emit('telegram'); onDrawerNavigate()"
    />

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
        <!-- 远程位置：与本地同构（有目录层级、能进出），所以复用这个外壳，
             只在条目渲染上分叉。另起一套组件的话，搜索、空态、统计这些
             都得写第二遍，迟早出现「本地修了远程还是老样子」。 -->
        <template v-if="state.remotePlace">
          <div v-if="state.busy" class="empty">
            <div class="icon" aria-hidden="true">⏳</div>
            <div class="title">{{ i18n.t(state.busyKey || 'busy.loading') }}</div>
          </div>
          <div v-else-if="!state.remoteItems.length" class="empty">
            <div class="icon" aria-hidden="true">📭</div>
            <div class="title">{{ i18n.t('rplace.empty_dir') }}</div>
          </div>
          <div v-else class="grid">
            <div
              v-for="it in state.remoteItems"
              :key="it.id"
              class="card"
              :class="{ locked: it.is_encrypted && !it.unlocked, failed: it.probe_failed }"
              data-rit="1"
              tabindex="0"
              @dblclick="onRemoteOpen(it)"
              @click="isMobile && onRemoteOpen(it)"
              @keydown.enter.prevent="onRemoteOpen(it)"
            >
              <div class="thumb">
                <span aria-hidden="true">{{ remoteIcon(it) }}</span>
              </div>
              <div class="cname">{{ it.unlocked && it.real_name ? it.real_name : it.name }}</div>
              <div class="cmeta">
                <!-- 三种状态分开：网络失败混进「密码不对」的话，
                     用户会对着网络故障反复试密码 -->
                <template v-if="it.probe_failed">{{ i18n.t('rplace.probe_failed') }}</template>
                <template v-else-if="it.is_encrypted && !it.unlocked">
                  {{ i18n.t('rplace.locked_hint') }}
                </template>
                <template v-else-if="it.is_dir">{{ i18n.t('kind.folder') }}</template>
                <template v-else>{{ i18n.formatSize(it.plaintext_size ?? it.size) }}</template>
              </div>
            </div>
          </div>
        </template>

        <!-- 还没选位置 -->
        <div v-else-if="!state.cwd" class="empty">
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
  </div>

  <!-- 移动端底部导航：文件 / 远程 / 设备 / 设置。
       语言与主题原先各占一格，现在收进设置页的「通用」第一组——
       它们原本一点就切换，若收进去还要点两层才够到就是退步，
       所以在设置里放在最上面。 -->
  <nav v-if="isMobile" class="pnav">
    <button
      class="pnavi"
      :class="{ on: tab === 'files' }"
      @click="tab = 'files'"
    >
      <span aria-hidden="true">📂</span>{{ i18n.t('nav.tab_files') }}
    </button>
    <button
      class="pnavi"
      :class="{ on: tab === 'remote' }"
      @click="tab = 'remote'; $emit('places')"
    >
      <span aria-hidden="true">☁️</span>{{ i18n.t('rplace.title') }}
    </button>
    <button
      class="pnavi"
      :class="{ on: tab === 'devices' }"
      @click="tab = 'devices'; $emit('devices')"
    >
      <span aria-hidden="true">📡</span>{{ i18n.t('nav.tab_devices') }}
      <span v-if="state.pairedCount" class="ndot"></span>
    </button>
    <button class="pnavi" @click="$emit('settings')">
      <span aria-hidden="true">⚙️</span>{{ i18n.t('settings.title') }}
    </button>
  </nav>

  <!-- 提示条：成功提示会自动消失，错误留到用户主动关掉。
       错误若也自动消失就等于没报错——用户可能正低头看别处 -->
  <div v-if="state.error || state.notice" class="toast" :class="{ err: !!state.error }">
    <span>{{ state.error || state.notice }}</span>
    <button class="iconbtn" @click="state.error = ''; clearNotice()">✕</button>
  </div>
</template>
