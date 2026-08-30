<script setup>
/** 输密码解锁当前目录。
 *
 * 双击一个锁着的加密文件时弹出。解锁的粒度是**目录**而不是单个文件：
 * 同一个库里的文件共用一个 vault salt，解开一个等于解开一批，
 * 逐个问密码只会让用户重复输入同一个密码。
 */

import { ref, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';

defineProps({
  busy: { type: Boolean, default: false },
  error: { type: String, default: '' },
});

const emit = defineEmits(['cancel', 'submit']);

const password = ref('');
const label = ref('main');
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

function submit() {
  if (!password.value) return;
  emit('submit', { password: password.value, label: label.value.trim() || 'main' });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow" @submit.prevent="submit">
      <h3>🔒 {{ i18n.t('unlock.title') }}</h3>
      <div class="hint">{{ i18n.t('unlock.quick_hint') }}</div>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <div class="field">
        <label class="flabel" for="u-pass">{{ i18n.t('unlock.password') }}</label>
        <input
          id="u-pass"
          ref="input"
          v-model="password"
          type="password"
          autocomplete="current-password"
        />
      </div>

      <div class="field">
        <label class="flabel" for="u-label">{{ i18n.t('unlock.label') }}</label>
        <input id="u-label" v-model="label" type="text" autocomplete="off" />
      </div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!password || busy">
          {{ busy ? i18n.t('unlock.deriving') : i18n.t('unlock.submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
