<script setup lang="ts">
/** 解锁一个已加密的 Telegram 远程位置——输密码进入。
 *
 * 与加密对话框配套：加密时现场设密码，锁定后（冷启动/未解锁）在这里现场输密码
 * 解锁。只要一个密码框（不是设新密码，是验证已有密码），密码错就地提示可重试。
 * 若这个密码此前已解锁过某个 omy 库/文件，同一个密码也能直接开（后端一并试会话
 * 已解锁的 KEK）。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n';

defineProps({
  /** 位置显示名，放标题里让用户确认解锁的是哪个账号。 */
  name: { type: String, default: '' },
  busy: { type: Boolean, default: false },
  /** 上一次输错时的错误文案；非空即在密码框下红字提示。 */
  error: { type: String, default: '' },
});

const emit = defineEmits(['cancel', 'submit']);

const password = ref('');
const input = useTemplateRef('input');
onMounted(() => input.value?.focus());

const canSubmit = computed(() => password.value.length > 0);
function submit() {
  if (!canSubmit.value) return;
  emit('submit', password.value);
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow" @submit.prevent="submit">
      <h3><AppIcon name="unlock" /> {{ i18n.t('rplace.unlock_title', { name }) }}</h3>
      <div class="hint">{{ i18n.t('rplace.unlock_dialog_hint') }}</div>

      <div class="field">
        <label class="flabel" for="tu-pass">{{ i18n.t('encrypt.password') }}</label>
        <input
          id="tu-pass"
          ref="input"
          v-model="password"
          type="password"
          autocomplete="current-password"
        />
        <div v-if="error" class="ferr">{{ error }}</div>
      </div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!canSubmit || busy">
          {{ busy ? i18n.t('busy.working') : i18n.t('rplace.unlock_submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
