<script setup>
/** 解锁界面。 */

import { ref, watch, nextTick, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';
import { state, pickFolder, unlock } from '../store.js';

const label = ref('main');
const password = ref('');
const passInput = useTemplateRef('passInput');

// 选完文件夹后焦点直接落到密码框，少一次点击
watch(
  () => state.vaults,
  async (v) => {
    if (!v) return;
    await nextTick();
    passInput.value?.focus();
  },
);

function submit() {
  unlock(label.value.trim(), password.value);
}
</script>

<template>
  <div class="unlock-screen">
    <form class="unlock-box" @submit.prevent="submit">
      <div class="lock-icon" aria-hidden="true">🔒</div>
      <h1>{{ i18n.t('app.name') }}</h1>
      <div class="hint">{{ i18n.t('unlock.pick_folder_hint') }}</div>

      <div v-if="state.error" class="errbox" role="alert">{{ state.error }}</div>

      <div class="folder-row">
        <div class="path" :title="state.folder || undefined">
          {{ state.folder || i18n.t('unlock.pick_folder') }}
        </div>
        <button type="button" class="btn" @click="pickFolder">
          {{ i18n.t('unlock.browse') }}
        </button>
      </div>

      <div class="field">
        <label for="u-label">{{ i18n.t('unlock.label') }}</label>
        <input id="u-label" v-model="label" type="text" autocomplete="off" />
      </div>

      <div class="field">
        <label for="u-pass">{{ i18n.t('unlock.password') }}</label>
        <input
          id="u-pass"
          ref="passInput"
          v-model="password"
          type="password"
          autocomplete="current-password"
        />
      </div>

      <button type="submit" class="btn primary" :disabled="!state.vaults || state.busy">
        {{ state.busy ? i18n.t('unlock.deriving') : i18n.t('unlock.submit') }}
      </button>
    </form>
  </div>
</template>
