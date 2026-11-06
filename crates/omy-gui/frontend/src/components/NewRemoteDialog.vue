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
  <!-- 用全局的 .overlay.dlg-overlay / .dlg：与其它对话框同一套外观与动效，
       手机上同样是底部弹层。原来自带一套 .mask / .dlg / .btn 样式，手机上是
       居中弹窗，与别的对话框风格不一致 -->
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <div class="dlg nr-dlg" data-new-remote="dialog">
      <h3>{{ i18n.t('rplace.new') }}</h3>
      <div class="methods">
        <button class="mcard" data-new-remote="webdav" @click="$emit('pick', 'webdav')">
          <AppIcon class="mi" name="cloud" />
          <b>{{ i18n.t('rplace.new_webdav') }}</b>
          <span class="d">{{ i18n.t('rplace.new_webdav_desc') }}</span>
        </button>
        <button class="mcard" data-new-remote="telegram" @click="$emit('pick', 'telegram')">
          <AppIcon class="mi" name="send" />
          <b>{{ i18n.t('rplace.new_telegram') }}</b>
          <span class="d">{{ i18n.t('rplace.new_telegram_desc') }}</span>
        </button>
        <button class="mcard" data-new-remote="virtual" @click="$emit('pick', 'virtual')">
          <AppIcon class="mi" name="archive" />
          <b>{{ i18n.t('rplace.new_virtual') }}</b>
          <span class="d">{{ i18n.t('rplace.new_virtual_desc') }}</span>
        </button>
      </div>
      <div class="acts">
        <button class="btn" data-new-remote="cancel" @click="$emit('cancel')">
          {{ i18n.t('common.cancel') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.nr-dlg {
  width: min(620px, 100%);
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
@media (max-width: 768px) {
  .methods { grid-template-columns: 1fr; }
  .btn, .mcard { min-height: 44px; }
}
</style>
