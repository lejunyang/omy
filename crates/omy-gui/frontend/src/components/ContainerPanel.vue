<script setup>
/**
 * 容器内容浏览面板。
 *
 * 一个加密的文件夹（目录容器）双击后进这里，看到里面有哪些文件。
 *
 * # 为什么不复用主列表
 *
 * 容器里的条目没有磁盘路径——它们是载荷里的一段区间。主列表的每个
 * 操作（选中、加密、系统打开、在文件管理器中显示）都以路径为前提，
 * 硬塞进去会让一半按钮点了没反应或者报错。
 *
 * 单独一个只读面板，把「这里能做什么」讲清楚，比复用一个处处是
 * 例外的列表更诚实。
 */
import { computed, ref, watch } from 'vue';
import * as i18n from '../i18n.js';
import { state } from '../store.js';

const props = defineProps({
  /** 容器文件的展示名。 */
  name: { type: String, required: true },
  /** 条目列表，来自后端 `list_container`。 */
  items: { type: Array, required: true },
});

const emit = defineEmits(['close']);

// 锁定后自己关掉，不依赖父组件记得清状态。
//
// 面板里列的是**解密出来的文件名**——锁定的语义是「这些都不该再
// 看得见」。父组件的 doLock 确实清了这个状态，但那是一处容易在
// 重构中被漏掉的赋值；这里再守一道，让「锁定即不可见」不依赖
// 某一行代码没被删掉。
watch(
  () => state.credentials,
  (n) => {
    if (n === 0) emit('close');
  },
);

/** 当前所在的子目录（相对容器根），空串表示根。 */
const cwd = ref('');

/** 面包屑：把 cwd 拆成可点击的层级。 */
const crumbs = computed(() => {
  if (!cwd.value) return [];
  const parts = cwd.value.split('/');
  return parts.map((p, i) => ({ name: p, path: parts.slice(0, i + 1).join('/') }));
});

/**
 * 当前目录下的直接子项。
 *
 * 容器索引是**扁平**的全路径列表，这里按当前目录过滤出直接子项——
 * 否则进入根目录会把所有层级的文件一股脑列出来。
 */
const visible = computed(() => {
  const prefix = cwd.value ? cwd.value + '/' : '';
  const out = [];
  for (const it of props.items) {
    if (!it.path.startsWith(prefix)) continue;
    const rest = it.path.slice(prefix.length);
    if (!rest || rest.includes('/')) continue; // 只要直接子项
    out.push(it);
  }
  // 目录在前，各自按名称排序——与主列表一致的约定
  return out.sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true });
  });
});

function enter(item) {
  if (item.is_dir) cwd.value = item.path;
}

function icon(item) {
  if (item.is_dir) return '📁';
  return { image: '🖼️', video: '🎬', audio: '🎵', text: '📝' }[item.kind] || '📄';
}

</script>

<template>
  <div class="mask" @click.self="$emit('close')">
    <div class="panel">
      <header>
        <span class="ic">📦</span>
        <div class="titles">
          <div class="name">{{ name }}</div>
          <div class="sub">{{ i18n.t('container.subtitle') }}</div>
        </div>
        <button class="x" @click="$emit('close')">✕</button>
      </header>

      <nav class="crumbs">
        <button :class="{ on: !cwd }" @click="cwd = ''">
          {{ i18n.t('container.root') }}
        </button>
        <template v-for="c in crumbs" :key="c.path">
          <span class="sep">›</span>
          <button :class="{ on: c.path === cwd }" @click="cwd = c.path">
            {{ c.name }}
          </button>
        </template>
      </nav>

      <div class="list">
        <div v-if="!visible.length" class="empty">
          {{ i18n.t('container.empty') }}
        </div>
        <div
          v-for="it in visible"
          :key="it.path"
          class="row"
          :class="{ dir: it.is_dir }"
          @dblclick="enter(it)"
        >
          <span class="ic">{{ icon(it) }}</span>
          <span class="nm">{{ it.name }}</span>
          <span class="sz">{{ it.size == null ? '' : i18n.formatSize(it.size) }}</span>
        </div>
      </div>

      <footer>
        <!-- 说清楚现在能做什么、不能做什么。用户看到文件列表却双击
             没反应时，需要知道这是设计如此而不是坏了 -->
        {{ i18n.t('container.hint') }}
      </footer>
    </div>
  </div>
</template>

<style scoped>
.mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.55);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 60;
}
.panel {
  width: min(640px, 92vw);
  max-height: 82vh;
  display: flex;
  flex-direction: column;
  background: var(--panel, #fff);
  border-radius: 14px;
  box-shadow: 0 18px 48px rgba(0, 0, 0, 0.28);
  overflow: hidden;
}
header {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 16px 18px;
  border-bottom: 1px solid var(--line, #e6e6ea);
}
header .ic {
  font-size: 26px;
}
.titles {
  flex: 1;
  min-width: 0;
}
.name {
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sub {
  font-size: 12px;
  color: var(--dim, #8a8a92);
  margin-top: 2px;
}
.x {
  border: 0;
  background: transparent;
  font-size: 17px;
  cursor: pointer;
  color: var(--dim, #8a8a92);
  padding: 4px 8px;
  border-radius: 8px;
}
.x:hover {
  background: var(--hover, #f0f0f3);
}
.crumbs {
  display: flex;
  align-items: center;
  gap: 4px;
  flex-wrap: wrap;
  padding: 10px 18px;
  border-bottom: 1px solid var(--line, #e6e6ea);
  font-size: 13px;
}
.crumbs button {
  border: 0;
  background: transparent;
  cursor: pointer;
  padding: 3px 8px;
  border-radius: 6px;
  color: var(--fg, #24242a);
}
.crumbs button:hover {
  background: var(--hover, #f0f0f3);
}
.crumbs button.on {
  font-weight: 600;
}
.sep {
  color: var(--dim, #8a8a92);
}
.list {
  flex: 1;
  overflow: auto;
  padding: 6px 0;
}
.row {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 18px;
  font-size: 14px;
  user-select: none;
}
.row.dir {
  cursor: pointer;
}
.row.dir:hover {
  background: var(--hover, #f0f0f3);
}
.row .nm {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.row .sz {
  color: var(--dim, #8a8a92);
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}
.empty {
  padding: 28px;
  text-align: center;
  color: var(--dim, #8a8a92);
  font-size: 13px;
}
footer {
  padding: 11px 18px;
  border-top: 1px solid var(--line, #e6e6ea);
  font-size: 12px;
  color: var(--dim, #8a8a92);
  line-height: 1.5;
}
</style>
