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
import { ref, watch, onBeforeUnmount } from 'vue';

import { isMobile } from '../viewport';
import { registerMobileBack } from '../mobile-platform';
import { state } from '../store';
import * as i18n from '../i18n';
import SideBar from './SideBar.vue';

defineProps({
  /** 顶栏搜索框是否可用。远程位置有自己的搜索栏（带服务端搜索开关），
   *  顶栏那个会与它语义重叠，所以进远程位置时隐藏。 */
  search: { type: Boolean, default: true },
});

const emit = defineEmits([
  'pick', 'devices', 'lang', 'new-remote', 'places',
  'settings', 'lock', 'quick-unlock', 'files',
]);

/** 移动端抽屉是否展开。280px 宽度放不下侧栏 + 网格。 */
const drawer = ref(false);

// 切回桌面时必须收起抽屉：它在桌面布局里是个盖住半屏的浮层，
// 而桌面上没有关掉它的入口
watch(isMobile, (m) => {
  if (!m) drawer.value = false;
});

/** 移动端点了侧栏里的位置后自动收起抽屉，否则挡着刚打开的目录。 */
function onNavigate() {
  drawer.value = false;
}

function setView(v) {
  state.view = v;
}

const unregisterBack = registerMobileBack(() => {
  if (!isMobile.value || !drawer.value) return false;
  drawer.value = false;
  return true;
});

onBeforeUnmount(unregisterBack);
</script>

<template>
  <div class="titlebar" :class="{ mob: isMobile }">
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

    <!-- 远程位置有自己的搜索栏（含服务端搜索开关），顶栏这个会语义重叠：
         两个框搜同一批东西，用户不知道该用哪个 -->
    <input
      v-if="search"
      v-model="state.query"
      class="search"
      type="search"
      :placeholder="i18n.t('view.search')"
      :aria-label="i18n.t('view.search')"
    />

    <span class="spacer"></span>

    <button
      class="pill"
      :class="{ ok: state.credentials > 0 }"
      :title="i18n.t('unlock.quick_hint')"
      @click="emit('quick-unlock')"
    >
      <span class="pill-ico" aria-hidden="true">{{ state.credentials > 0 ? '🔓' : '🔑' }}</span>
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
        @click="emit('settings')"
      >
        ⚙️
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
      🔒
    </button>
  </div>

  <div class="body" :class="{ mob: isMobile }">
    <!-- 抽屉遮罩：移动端没有 Esc 键，没有它抽屉只能靠汉堡键关 -->
    <div
      v-if="isMobile && drawer"
      class="scrim"
      @click="drawer = false"
    ></div>

    <SideBar
      :class="{ drawer: isMobile, open: drawer }"
      :mobile="isMobile"
      @pick="emit('pick'); onNavigate()"
      @devices="emit('devices'); onNavigate()"
      @navigate="onNavigate"
      @lang="emit('lang')"
      @new-remote="emit('new-remote'); onNavigate()"
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
      :class="{ on: !state.placeBrowserOpen }"
      @click="emit('files')"
    >
      <span aria-hidden="true">📂</span>{{ i18n.t('nav.tab_files') }}
    </button>
    <button
      class="pnavi"
      :class="{ on: state.placeBrowserOpen }"
      @click="emit('places')"
    >
      <span aria-hidden="true">☁️</span>{{ i18n.t('rplace.title') }}
    </button>
    <button class="pnavi" @click="emit('devices')">
      <span aria-hidden="true">📡</span>{{ i18n.t('nav.tab_devices') }}
      <span v-if="state.pairedCount" class="ndot"></span>
    </button>
    <button class="pnavi" @click="emit('settings')">
      <span aria-hidden="true">⚙️</span>{{ i18n.t('settings.title') }}
    </button>
  </nav>
</template>
