<script setup>
/** 「添加到虚拟远程」的树形目标选择对话框（方案甲）。
 *
 * 列出所有虚拟远程位置；点开一个展开它的文件夹树（含根）；选中一个目标节点后
 * 确认，把待添加的引用加到那个虚拟位置的那个文件夹。移动端全屏（.dlg 自带响应）。
 *
 * 没有任何虚拟位置时给一个「先新建一个」的入口——否则用户点了「添加到虚拟远程」
 * 却面对空对话框，不知道下一步。
 */

import { ref, onMounted } from 'vue';
import * as i18n from '../i18n.js';
import * as api from '../api.js';
import { state, confirmAddToVirtual, cancelAddToVirtual, createVirtualPlaceOnly } from '../store.js';

/** 每个虚拟位置的展开态与文件夹列表：{ id, name, expanded, folders:[{id,name,depth}] }。 */
const places = ref([]);
/** 选中的目标：{ placeId, folder }。 */
const target = ref(null);
const busy = ref(false);

async function load() {
  const list = state.virtualPlaces || [];
  places.value = list.map((v) => ({ id: v.id, name: v.name, expanded: false, folders: null }));
}
onMounted(load);

/** 展开/收起一个虚拟位置，首次展开时拉它的文件夹树。 */
async function toggle(p) {
  p.expanded = !p.expanded;
  if (p.expanded && p.folders === null) {
    try {
      const fs = await api.virtualFolders(p.id);
      // 根节点 name 为空，用位置名兜底
      p.folders = fs.map((f) => ({ ...f, name: f.id === '' ? p.name : f.name }));
    } catch {
      p.folders = [];
    }
  }
}

function pick(placeId, folder) {
  target.value = { placeId, folder };
}
function isPicked(placeId, folder) {
  return target.value && target.value.placeId === placeId && target.value.folder === folder;
}

async function submit() {
  if (!target.value || busy.value) return;
  busy.value = true;
  try {
    await confirmAddToVirtual(target.value.placeId, target.value.folder);
  } finally {
    busy.value = false;
  }
}

/** 空态：先新建一个虚拟位置再选。 */
async function newVirtual() {
  const name = window.prompt(i18n.t('virtual.name_prompt'));
  if (name === null) return;
  await createVirtualPlaceOnly(name.trim());
  await load();
}

const count = () => (state.addToVirtual?.items?.length || 0);
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="cancelAddToVirtual">
    <form class="dlg" @submit.prevent="submit">
      <h3>{{ i18n.t('virtual.pick_title', { n: count() }) }}</h3>
      <div class="hint">{{ i18n.t('virtual.pick_hint') }}</div>

      <div v-if="!places.length" class="vp-empty">
        <div>{{ i18n.t('virtual.pick_empty') }}</div>
        <button type="button" class="btn" @click="newVirtual">{{ i18n.t('virtual.add') }}</button>
      </div>

      <div v-else class="vp-tree" data-vp="tree">
        <template v-for="p in places" :key="p.id">
          <!-- 虚拟位置一行：点它展开/收起文件夹树 -->
          <button type="button" class="vp-node vp-place" @click="toggle(p)">
            <span aria-hidden="true">{{ p.expanded ? '▼' : '▶' }} 🗂️</span>
            <span class="stext">{{ p.name }}</span>
          </button>
          <!-- 展开后：它的文件夹（含根），点一个选为目标 -->
          <template v-if="p.expanded && p.folders">
            <button
              v-for="f in p.folders"
              :key="p.id + '/' + f.id"
              type="button"
              class="vp-node vp-folder"
              :class="{ picked: isPicked(p.id, f.id) }"
              :style="{ paddingInlineStart: (16 + f.depth * 16) + 'px' }"
              :data-vp-folder="p.id + '/' + f.id"
              @click="pick(p.id, f.id)"
            >
              <span aria-hidden="true">📁</span>
              <span class="stext">{{ f.name }}</span>
              <span v-if="isPicked(p.id, f.id)" class="vp-check">✓</span>
            </button>
          </template>
        </template>
      </div>

      <div class="acts">
        <button type="button" class="btn" @click="cancelAddToVirtual">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!target || busy">
          {{ busy ? i18n.t('busy.working') : i18n.t('virtual.pick_confirm') }}
        </button>
      </div>
    </form>
  </div>
</template>

<style scoped>
.vp-tree {
  max-height: 46vh;
  overflow: auto;
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  margin: 8px 0;
}
.vp-node {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 8px 12px;
  background: none;
  border: 0;
  border-bottom: 1px solid var(--border);
  text-align: start;
  cursor: pointer;
  color: var(--fg);
}
.vp-node:hover { background: var(--bg2); }
.vp-place { font-weight: 600; }
.vp-folder.picked {
  background: var(--accent-solid);
  color: #fff;
}
.vp-check { margin-inline-start: auto; }
.vp-empty {
  display: flex;
  flex-direction: column;
  gap: 10px;
  align-items: center;
  padding: 24px;
  color: var(--fg2);
}
.stext { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
