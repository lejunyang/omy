<script setup lang="ts">
/** 远程位置列表：真实位置与虚拟远程并列，末尾一行「新建」。
 *
 * 从 PlaceBrowser 拆出——那个组件同时管位置列表和目录浏览，两千四百多行。
 * 进入位置会改动目录浏览的整套状态，所以仍归父组件：这里只发 `open`。
 * 改名、摘掉、删除账号只动位置注册信息，在这里就地完成。 */
import { computed, ref } from 'vue';
import * as i18n from '../i18n';
import { askConfirm, askText } from '../ask';
import { isMobile } from '../viewport';
import {
  state,
  removeRemotePlace,
  detachTelegramPlace,
  deleteTelegramAccount,
  renameTelegramPlace,
} from '../store';
import ContextMenu from './ContextMenu.vue';

const emit = defineEmits<{
  (e: 'open', place: any): void;
  (e: 'new-remote'): void;
}>();

/** 虚拟远程列表。状态初值就是空数组，这里再兜一次 undefined：
 *  旧快照或测试替身里没有这个字段时，位置列表不能整屏渲染失败。 */
const virtualList = computed(() => state.virtualPlaces || []);

/** 这个位置有没有「本机登录态」这回事。
 *
 * 只有 Telegram 才分得出「摘掉位置」与「删掉登录态」；WebDAV 的凭据跟着
 * 位置配置走，摘掉就没了，给它一个「删除账号」纯属多余且会让人误解。 */
function hasLocalSession(p) {
  return p.kind === 'telegram';
}

async function remove(p) {
  // 原本这里不做确认（只删注册信息、不碰云端文件）。多账号之后它与
  // 「删除账号」并排出现，点错的代价不对称，所以也确认一次，并顺便说清
  // 登录态和永久缓存都还在——否则用户以为摘掉位置就干净了
  if (hasLocalSession(p)) {
    if (!(await askConfirm({ title: i18n.t('rplace.detach_confirm', { name: p.name }) }))) return;
    await detachTelegramPlace(p.id);
    return;
  }
  await removeRemotePlace(p.id);
}

/** 删除账号：连本机登录态一起清掉，下次要重新扫码或导入 tdata。 */
async function deleteAccount(p) {
  const ok = await askConfirm({
    title: i18n.t('rplace.delete_account_confirm', { name: p.name }),
    danger: true,
  });
  if (!ok) return;
  await deleteTelegramAccount(p.id);
}

/** 改本机显示名。两个账号昵称相同时，这是唯一能分辨它们的办法。 */
async function rename(p) {
  const next = await askText({ title: i18n.t('rplace.rename_prompt', { name: p.name }), initial: p.name });
  if (next === null) return;
  await renameTelegramPlace(p.id, next);
}

/* ---- 移动端位置列表：整行单击进入，其余操作收进底部弹层 ----
 *
 * 桌面行尾并排四个按钮（进入/改名/移除/删除账号），在手机上每个只剩
 * 二十来像素宽，而且「移除」与「删除账号」紧挨着，误触代价很大。手机上
 * 只留一个「更多」按钮，弹层里逐项写明后果（与侧栏右键菜单同一套文案）。 */
const placeMenu = ref<{ place: any; x: number; y: number } | null>(null);

const placeMenuItems = computed(() => {
  const p = placeMenu.value?.place;
  if (!p) return [];
  const tg = hasLocalSession(p);
  const items: any[] = [{ key: 'open', label: i18n.t('rplace.enter'), icon: 'folder-open' }];
  if (tg) items.push({ key: 'rename', label: i18n.t('rplace.rename'), icon: 'pencil' });
  items.push({ key: 'sep' });
  items.push({
    key: 'detach',
    label: tg ? i18n.t('rplace.detach') : i18n.t('rplace.remove'),
    icon: 'minus',
    note: tg ? i18n.t('rplace.detach_note') : undefined,
  });
  if (tg) {
    items.push({
      key: 'delacct',
      label: i18n.t('rplace.delete_account'),
      icon: 'trash',
      danger: true,
      note: i18n.t('rplace.delete_account_note'),
    });
  }
  return items;
});

function openPlaceMenu(p, ev: MouseEvent) {
  placeMenu.value = { place: p, x: ev.clientX, y: ev.clientY };
}

async function onPlaceMenuPick(key: string) {
  const p = placeMenu.value?.place;
  placeMenu.value = null;
  if (!p) return;
  if (key === 'open') emit('open', p);
  else if (key === 'rename') await rename(p);
  else if (key === 'detach') await remove(p);
  else if (key === 'delacct') await deleteAccount(p);
}

/** 位置行的单击：移动端直接进入（触屏没有双击），桌面不响应——
 *  桌面靠双击或行尾的「进入」按钮，单击进入会让人在想改名时误入。 */
function onPlaceRowClick(p) {
  if (isMobile.value) emit('open', p);
}

function kindLabel(p) {
  if (p.kind === 'nextcloud') return i18n.t('rplace.vendor_nextcloud');
  if (p.kind === 'telegram') return i18n.t('rplace.vendor_telegram');
  if (p.kind === 'webdav') return 'WebDAV';
  return p.kind || '';
}
</script>

<template>
  <div class="list anim-in">
    <div v-if="!state.remotePlaces.length && !virtualList.length" class="empty">
      <div class="icon" aria-hidden="true"><AppIcon name="cloud" /></div>
      <div class="title">{{ i18n.t('rplace.empty') }}</div>
      <div class="sub">{{ i18n.t('rplace.empty_hint') }}</div>
      <button class="btn primary" style="margin-top: 12px" @click="emit('new-remote')">
        {{ i18n.t('rplace.new') }}
      </button>
    </div>

    <div
      v-for="(p, pi) in state.remotePlaces"
      :key="p.id"
      class="lrow placerow"
      :class="{ mob: isMobile }"
      :style="{ '--i': pi }"
      tabindex="0"
      @click="onPlaceRowClick(p)"
      @dblclick="emit('open', p)"
      @keydown.enter.prevent="emit('open', p)"
    >
      <span class="placeic" :class="'k-' + p.kind"><AppIcon :name="p.kind === 'telegram' ? 'send' : 'cloud'" /></span>
      <span class="placetext">
        <span class="nm">{{ p.name }}</span>
        <span v-if="isMobile" class="placesub">{{ kindLabel(p) }}<template v-if="p.encrypted"> · <AppIcon :name="p.unlocked ? 'unlock' : 'lock'" :size="12" /></template></span>
      </span>
      <span v-if="!isMobile" class="tg">{{ kindLabel(p) }}</span>
      <!-- 移动端：只留一个「更多」，操作进底部弹层（理由见 placeMenu） -->
      <span v-if="isMobile" class="rowactions">
        <button
          class="iconbtn"
          :data-pb-more="p.id"
          :aria-label="i18n.t('rplace.menu_open')"
          @click.stop="openPlaceMenu(p, $event)"
        >
          <AppIcon name="more" />
        </button>
      </span>
      <span v-else class="rowactions">
        <button class="btn small" :data-pb-enter="p.id" @click.stop="emit('open', p)">
          {{ i18n.t('rplace.enter') }}
        </button>
        <button
          v-if="hasLocalSession(p)"
          class="iconbtn"
          :data-pb-rename="p.id"
          :aria-label="i18n.t('rplace.rename')"
          :title="i18n.t('rplace.rename')"
          @click.stop="rename(p)"
        >
          <AppIcon name="pencil" />
        </button>
        <button
          class="iconbtn"
          :data-pb-detach="p.id"
          :aria-label="hasLocalSession(p) ? i18n.t('rplace.detach') : i18n.t('rplace.remove')"
          :title="hasLocalSession(p) ? i18n.t('rplace.detach') : i18n.t('rplace.remove')"
          @click.stop="remove(p)"
        >
          <AppIcon name="close" />
        </button>
        <button
          v-if="hasLocalSession(p)"
          class="iconbtn danger"
          :data-pb-delacct="p.id"
          :aria-label="i18n.t('rplace.delete_account')"
          :title="i18n.t('rplace.delete_account')"
          @click.stop="deleteAccount(p)"
        >
          <AppIcon name="trash" />
        </button>
      </span>
    </div>

    <!-- 虚拟远程（本地收藏夹式的引用集合）与真实位置一起列出。
         原来这一页只列真实位置：手机上「远程位置」这个 tab 里看不到虚拟收藏，
         只能从抽屉进；两者都没有时还会误报「还没有远程位置」 -->
    <div
      v-for="(v, vi) in virtualList"
      :key="v.id"
      class="lrow placerow"
      :class="{ mob: isMobile }"
      :style="{ '--i': state.remotePlaces.length + vi }"
      :data-pb-virtual="v.id"
      tabindex="0"
      @click="onPlaceRowClick(v)"
      @dblclick="emit('open', v)"
      @keydown.enter.prevent="emit('open', v)"
    >
      <span class="placeic k-virtual"><AppIcon name="archive" /></span>
      <span class="placetext">
        <span class="nm">{{ v.name }}</span>
        <span v-if="isMobile" class="placesub">{{ i18n.t('rplace.kind_virtual') }}<template v-if="v.encrypted"> · <AppIcon :name="v.unlocked ? 'unlock' : 'lock'" :size="12" /></template></span>
      </span>
      <span v-if="!isMobile" class="tg">{{ i18n.t('rplace.kind_virtual') }}</span>
      <span v-if="!isMobile" class="rowactions">
        <button class="btn small" @click.stop="emit('open', v)">{{ i18n.t('rplace.enter') }}</button>
      </span>
    </div>

    <!-- 列表不空时也要有新建入口：原来只有空状态里有按钮，手机上加第二个位置
         得先打开抽屉才找得到 -->
    <div
      v-if="state.remotePlaces.length || virtualList.length"
      class="lrow placerow addrow"
      :class="{ mob: isMobile }"
      data-pb="new-remote"
      tabindex="0"
      role="button"
      @click="emit('new-remote')"
      @keydown.enter.prevent="emit('new-remote')"
    >
      <span class="placeic k-add"><AppIcon name="plus" /></span>
      <span class="placetext"><span class="nm">{{ i18n.t('rplace.new') }}</span></span>
    </div>
  </div>

  <Transition name="menu">
    <ContextMenu
      v-if="placeMenu"
      :items="placeMenuItems"
      :x="placeMenu.x"
      :y="placeMenu.y"
      :title="placeMenu.place.name"
      @pick="onPlaceMenuPick"
      @close="placeMenu = null"
    />
  </Transition>
</template>

<style scoped>
/* 位置行：让操作按钮靠右排成一组 */
.placerow {
  cursor: pointer;
}
/* 位置图标放进一个色块：桌面上与普通列表行对齐（20px 方块内），
   手机上放大成 44px 圆角块（见 mobile.css） */
.placeic {
  width: 24px;
  flex: none;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--fg2);
}
/* 新建行：虚线与弱化色，读起来是「入口」而不是又一个已有位置 */
.addrow .nm {
  color: var(--fg2);
}
.addrow .placeic {
  color: var(--accent);
}
.placetext {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.placesub {
  font-size: 12px;
  color: var(--fg2);
  display: inline-flex;
  align-items: center;
  gap: 4px;
}
/* 删除账号不可撤销，要和旁边的「移除」在视觉上区分开——
   两个相邻的灰图标，点错的代价却差很远 */
.iconbtn.danger:hover {
  background: #fee2e2;
  border-color: #fca5a5;
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

/* 窄屏抬到触控下限。同一行里的图标钮已经是 44，只有「进入」是 30，
   既按不准也显得高度不齐。这条要写在这里而不是全局 app.css：
   `.rowactions .btn` 特异性更高，全局那条压不住它。 */
@media (--mobile) {
  .rowactions .btn {
    min-height: 44px;
  }
}
</style>
