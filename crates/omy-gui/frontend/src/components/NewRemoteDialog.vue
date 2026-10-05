<script setup lang="ts">
/** 统一的新建远程位置入口。
 *
 * 三种位置的后续流程差异很大，所以这里只负责选择类型；选择后关闭本层，
 * 由根组件打开原有 WebDAV / Telegram 流程，或创建虚拟远程。
 */
import * as i18n from '../i18n';

const emit = defineEmits<{
  (e: 'pick', kind: 'webdav' | 'telegram' | 'virtual'): void;
  (e: 'cancel'): void;
}>();
</script>

<template>
  <div class="mask" @click.self="$emit('cancel')">
    <div class="dlg" data-new-remote="dialog">
      <div class="dh">{{ i18n.t('rplace.new') }}</div>
      <div class="methods">
        <button class="mcard" data-new-remote="webdav" @click="$emit('pick', 'webdav')">
          <span class="mi" aria-hidden="true">☁️</span>
          <b>{{ i18n.t('rplace.new_webdav') }}</b>
          <span class="d">{{ i18n.t('rplace.new_webdav_desc') }}</span>
        </button>
        <button class="mcard" data-new-remote="telegram" @click="$emit('pick', 'telegram')">
          <span class="mi" aria-hidden="true">✈️</span>
          <b>{{ i18n.t('rplace.new_telegram') }}</b>
          <span class="d">{{ i18n.t('rplace.new_telegram_desc') }}</span>
        </button>
        <button class="mcard" data-new-remote="virtual" @click="$emit('pick', 'virtual')">
          <span class="mi" aria-hidden="true">🗂️</span>
          <b>{{ i18n.t('rplace.new_virtual') }}</b>
          <span class="d">{{ i18n.t('rplace.new_virtual_desc') }}</span>
        </button>
      </div>
      <div class="act">
        <button class="btn" data-new-remote="cancel" @click="$emit('cancel')">
          {{ i18n.t('common.cancel') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mask {
  position: fixed;
  inset: 0;
  z-index: 60;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 16px;
  background: rgba(0, 0, 0, 0.5);
}
.dlg {
  width: min(620px, 100%);
  max-height: 90vh;
  overflow: auto;
  padding: 18px;
  padding-bottom: calc(18px + env(safe-area-inset-bottom, 0px));
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-l);
}
.dh {
  margin-bottom: 14px;
  font-size: 15px;
  font-weight: 600;
}
.methods {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 10px;
  margin: 6px 0 12px;
}
.mcard {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  padding: 14px 12px;
  text-align: start;
  color: var(--fg);
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-m);
  cursor: pointer;
}
.mcard:hover,
.mcard:focus-visible {
  border-color: var(--accent);
}
.mcard .mi { font-size: 22px; }
.mcard b { font-size: 13px; }
.mcard .d {
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--fg2);
}
.act {
  display: flex;
  justify-content: flex-end;
  margin-top: 16px;
}
.btn {
  min-height: 36px;
  padding: 7px 14px;
  font-size: 13px;
  color: var(--fg);
  background: var(--bg3);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  cursor: pointer;
}
@media (max-width: 768px) {
  .methods { grid-template-columns: 1fr; }
  .btn, .mcard { min-height: 44px; }
}
</style>
