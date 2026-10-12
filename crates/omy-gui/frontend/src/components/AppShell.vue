<script setup lang="ts">
/**
 * 主界面外壳：标题栏 + 侧栏 + 内容区。
 *
 * # 为什么要抽出来
 *
 * 本地文件与远程位置**共用同一个框架**：侧栏常驻、顶栏一致、移动端抽屉
 * 行为一致。原先 PlaceBrowser 是一个整屏组件，与 MainScreen 互斥替换，
 * 结果进远程位置后侧栏整个消失——用户没法在两个位置之间切换，
 * 只能先退回去。
 *
 * 抽成外壳而不是「在 PlaceBrowser 里也放一个 SideBar」：后者会有两份
 * 外壳，顶栏按钮、抽屉状态、移动端底栏都要写两遍，迟早出现
 * 「本地修了远程还是老样子」。
 *
 * # 内容区由插槽提供
 *
 * 外壳只管框架，`.main` 里放什么由调用方决定。这样远程位置也能有
 * 自己的面包屑与工具栏，而不必把两种页面的逻辑揉进一个组件。
 */
import { ref, watch, onBeforeUnmount, nextTick, useTemplateRef } from 'vue';

import { isMobile } from '../viewport';
import { registerMobileBack } from '../mobile-platform';
import { state } from '../store';
import * as i18n from '../i18n';
import SideBar from './SideBar.vue';

const props = defineProps({
  /** 顶栏搜索框是否可用。远程位置有自己的搜索栏（带服务端搜索开关），
   *  顶栏那个会与它语义重叠，所以进远程位置时隐藏。 */
  search: { type: Boolean, default: true },
  /** 移动端顶栏的大标题（当前位置名）。桌面端有常驻侧栏与面包屑，
   *  不需要它；手机上侧栏收进抽屉，没有标题就看不出自己在哪。 */
  title: { type: String, default: '' },
  /** 标题下的一行小字（条目数等）。 */
  subtitle: { type: String, default: '' },
  /** 底栏高亮哪一项：files / places / none。由页面告诉外壳，
   *  而不是外壳自己猜：传输页既不属于文件也不属于远程位置。 */
  tab: { type: String, default: 'files' },
});

const emit = defineEmits([
  'pick', 'devices', 'lang', 'new-remote', 'places',
  'settings', 'lock', 'quick-unlock', 'files',
]);

/** 移动端抽屉是否展开。280px 宽度放不下侧栏 + 网格。 */
const drawer = ref(false);

/** 移动端搜索框是否展开。
 *
 * 手机顶栏放不下「标题 + 搜索框 + 按钮」三样：常驻搜索框会把标题挤没，
 * 用户就看不出自己在哪个目录。所以收成一个图标，点开后搜索框盖住标题。
 * 有搜索词时保持展开——收起后词还在生效，列表却莫名变少，用户找不到原因。 */
const searchOpen = ref(false);
const searchInput = useTemplateRef<HTMLInputElement>('searchInput');

async function openSearch() {
  searchOpen.value = true;
  await nextTick();
  searchInput.value?.focus();
}

function closeSearch() {
  state.query = '';
  searchOpen.value = false;
}

// 切回桌面时必须收起抽屉：它在桌面布局里是个盖住半屏的浮层，
// 而桌面上没有关掉它的入口
watch(isMobile, (m) => {
  if (!m) {
    drawer.value = false;
    searchOpen.value = false;
  }
});

/** 移动端点了侧栏里的位置后自动收起抽屉，否则挡着刚打开的目录。 */
function onNavigate() {
  drawer.value = false;
}

function setView(v) {
  state.view = v;
}

/* ---- 抽屉左滑收起 ----
 *
 * 只做「滑动关闭」，不做「从屏幕左缘滑出打开」：Android 的系统返回手势
 * 就是从左右边缘向内滑，抢这个区域会让用户返回时误开抽屉（AGENTS.md
 * 「系统手势冲突」）。关闭方向是在抽屉内部往左拖，与系统手势不重叠。 */
const DRAWER_CLOSE_PX = 70;
const drawerDx = ref(0);
const drawerDragging = ref(false);
let dragStartX = 0;
let dragStartY = 0;
let dragAxis: '' | 'x' | 'y' = '';

function onDrawerDown(ev: PointerEvent) {
  if (!isMobile.value || !drawer.value || ev.pointerType === 'mouse') return;
  dragStartX = ev.clientX;
  dragStartY = ev.clientY;
  dragAxis = '';
}

function onDrawerMove(ev: PointerEvent) {
  if (!drawer.value || ev.pointerType === 'mouse' || !dragStartX) return;
  const dx = ev.clientX - dragStartX;
  const dy = ev.clientY - dragStartY;
  // 先判方向再跟手：竖向滑动是在滚侧栏列表，不能被当成关闭手势
  if (!dragAxis && (Math.abs(dx) > 8 || Math.abs(dy) > 8)) {
    dragAxis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
  }
  if (dragAxis !== 'x') return;
  drawerDragging.value = true;
  drawerDx.value = Math.min(0, dx);
}

function onDrawerUp() {
  if (drawerDragging.value && drawerDx.value < -DRAWER_CLOSE_PX) drawer.value = false;
  drawerDragging.value = false;
  drawerDx.value = 0;
  dragStartX = 0;
  dragAxis = '';
}

const unregisterBack = registerMobileBack(() => {
  if (!isMobile.value) return false;
  if (drawer.value) {
    drawer.value = false;
    return true;
  }
  // 返回键先收起搜索框，与「先关最上层的东西」一致
  if (searchOpen.value) {
    closeSearch();
    return true;
  }
  return false;
});

onBeforeUnmount(unregisterBack);
</script>

<template>
  <div class="titlebar" :class="{ mob: isMobile, searching: isMobile && (searchOpen || !!state.query) }">
    <button
      v-if="isMobile"
      class="iconbtn"
      :title="i18n.t('nav.menu')"
      :aria-label="i18n.t('nav.menu')"
      :aria-expanded="drawer"
      @click="drawer = !drawer"
    >
      <AppIcon name="menu" />
    </button>

    <span v-if="!isMobile" class="brand">{{ i18n.t('app.name') }}</span>

    <!-- 移动端标题：当前位置名 + 一行摘要。搜索展开时让位给搜索框 -->
    <div v-if="isMobile" class="mtitle" :aria-hidden="searchOpen || !!state.query">
      <span class="mtitle-main">{{ props.title || i18n.t('app.name') }}</span>
      <span v-if="props.subtitle" class="mtitle-sub">{{ props.subtitle }}</span>
    </div>

    <!-- 远程位置有自己的搜索栏（含服务端搜索开关），顶栏这个会语义重叠：
         两个框搜同一批东西，用户不知道该用哪个 -->
    <input
      v-if="search && (!isMobile || searchOpen || state.query)"
      ref="searchInput"
      v-model="state.query"
      class="search"
      type="search"
      :placeholder="i18n.t('view.search')"
      :aria-label="i18n.t('view.search')"
    />

    <span v-if="!isMobile" class="spacer"></span>

    <template v-if="isMobile && search">
      <button
        v-if="!searchOpen && !state.query"
        class="iconbtn"
        data-tb="search"
        :aria-label="i18n.t('view.search')"
        @click="openSearch"
      >
        <AppIcon name="search" />
      </button>
      <button
        v-else
        class="iconbtn"
        data-tb="search-close"
        :aria-label="i18n.t('actions.close')"
        @click="closeSearch"
      >
        <AppIcon name="close" />
      </button>
    </template>

    <button
      class="pill"
      :class="{ ok: state.credentials > 0 }"
      :title="i18n.t('unlock.quick_hint')"
      @click="emit('quick-unlock')"
    >
      <span class="pill-ico" aria-hidden="true"><AppIcon :name="state.credentials > 0 ? 'unlock' : 'key'" /></span>
      <span class="pill-txt">{{
        state.credentials > 0
          ? i18n.tn('status.credentials', state.credentials)
          : i18n.t('status.no_password')
      }}</span>
    </button>

    <template v-if="!isMobile">
      <button
        class="iconbtn"
        data-tb="view-grid"
        :aria-pressed="state.view === 'grid'"
        :title="i18n.t('view.grid')"
        :aria-label="i18n.t('view.grid')"
        @click="setView('grid')"
      >
        <AppIcon name="grid" />
      </button>
      <button
        class="iconbtn"
        data-tb="view-list"
        :aria-pressed="state.view === 'list'"
        :title="i18n.t('view.list')"
        :aria-label="i18n.t('view.list')"
        @click="setView('list')"
      >
        <AppIcon name="menu" />
      </button>
      <button
        class="iconbtn"
        :title="i18n.t('settings.title')"
        :aria-label="i18n.t('settings.title')"
        data-tb="settings"
        @click="emit('settings')"
      >
        <AppIcon name="settings" />
      </button>
    </template>
    <button
      v-if="state.credentials > 0"
      id="btn-lock"
      class="iconbtn"
      :title="i18n.t('status.lock_now')"
      :aria-label="i18n.t('status.lock_now')"
      @click="emit('lock')"
    >
      <AppIcon name="lock" />
    </button>
  </div>

  <div class="body" :class="{ mob: isMobile }">
    <!-- 抽屉遮罩：移动端没有 Esc 键，没有它抽屉只能靠汉堡键关 -->
    <Transition name="fade">
      <div
        v-if="isMobile && drawer"
        class="scrim"
        @click="drawer = false"
      ></div>
    </Transition>

    <SideBar
      :class="{ drawer: isMobile, open: drawer, dragging: drawerDragging }"
      :style="isMobile && drawerDragging ? { '--drawer-dx': drawerDx + 'px' } : undefined"
      :mobile="isMobile"
      @pick="emit('pick'); onNavigate()"
      @devices="emit('devices'); onNavigate()"
      @navigate="onNavigate"
      @lang="emit('lang')"
      @new-remote="emit('new-remote'); onNavigate()"
      @pointerdown="onDrawerDown"
      @pointermove="onDrawerMove"
      @pointerup="onDrawerUp"
      @pointercancel="onDrawerUp"
    />

    <slot />
  </div>

  <!-- 移动端底部导航。放在外壳里而不是 MainScreen 里：
       远程位置走的是另一个内容组件，底栏写在 MainScreen 中的话，
       一进远程位置这四个入口就全没了——实测 pnav 从 4 变 0，
       而设置里才有缓存管理，用户在远程位置里根本够不到。 -->
  <nav v-if="isMobile" class="pnav">
    <button
      class="pnavi"
      :class="{ on: props.tab === 'files' }"
      @click="emit('files')"
    >
      <AppIcon name="folder-open" />{{ i18n.t('nav.tab_files') }}
    </button>
    <button
      class="pnavi"
      :class="{ on: props.tab === 'places' }"
      @click="emit('places')"
    >
      <AppIcon name="cloud" />{{ i18n.t('rplace.title') }}
    </button>
    <button class="pnavi" @click="emit('devices')">
      <AppIcon name="radio" />{{ i18n.t('nav.tab_devices') }}
      <span v-if="state.pairedCount" class="ndot"></span>
    </button>
    <button class="pnavi" @click="emit('settings')">
      <AppIcon name="settings" />{{ i18n.t('settings.title') }}
    </button>
  </nav>
</template>
