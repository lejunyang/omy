<script setup lang="ts">
/** `ask.ts` 的界面：确认 / 文本 / 密码三种对话框共用一个组件。
 *
 * 由 App 根组件常驻挂载，`askState.current` 有值时出现。返回键与 Esc
 * 都走全局的返回栈（registerMobileBack），按下即取消。
 */
import { ref, watch, nextTick, onBeforeUnmount, useTemplateRef } from 'vue';
import * as i18n from '../i18n';
import { askState, answer } from '../ask';
import { registerMobileBack } from '../mobile-platform';

const value = ref('');
const input = useTemplateRef<HTMLInputElement>('input');
const okBtn = useTemplateRef<HTMLButtonElement>('okBtn');
let unregisterBack: (() => void) | null = null;

watch(
  () => askState.current,
  async (cur) => {
    unregisterBack?.();
    unregisterBack = null;
    if (!cur) return;
    value.value = cur.initial || '';
    unregisterBack = registerMobileBack(() => {
      cancel();
      return true;
    });
    await nextTick();
    if (cur.kind === 'confirm') okBtn.value?.focus();
    else {
      input.value?.focus();
      input.value?.select();
    }
  },
  // immediate：组件晚于提问挂载时也要带上初始值与返回键处理
  { immediate: true },
);

onBeforeUnmount(() => unregisterBack?.());

function cancel() {
  answer(askState.current?.kind === 'confirm' ? false : null);
}

function submit() {
  const cur = askState.current;
  if (!cur) return;
  if (cur.kind === 'confirm') answer(true);
  else if (cur.kind === 'password') {
    if (!value.value) return;
    answer(value.value);
  } else {
    answer(value.value.trim());
  }
}
</script>

<template>
  <div v-if="askState.current" class="overlay dlg-overlay ask-overlay" @click.self="cancel">
    <form class="dlg narrow ask" role="dialog" :aria-label="askState.current.title" @submit.prevent="submit">
      <h3>
        <AppIcon v-if="askState.current.danger" name="circle-alert" class="ask-danger-ic" />
        {{ askState.current.title }}
      </h3>
      <p v-if="askState.current.message" class="hint ask-msg">{{ askState.current.message }}</p>
      <div v-if="askState.current.kind !== 'confirm'" class="field">
        <input
          ref="input"
          v-model="value"
          :type="askState.current.kind === 'password' ? 'password' : 'text'"
          :placeholder="askState.current.placeholder || ''"
          :autocomplete="askState.current.kind === 'password' ? 'current-password' : 'off'"
        />
      </div>
      <div class="acts">
        <button type="button" class="btn" @click="cancel">{{ i18n.t('actions.cancel') }}</button>
        <button
          ref="okBtn"
          type="submit"
          class="btn primary"
          :class="{ danger: askState.current.danger }"
          :disabled="askState.current.kind === 'password' && !value"
        >
          {{ askState.current.confirmLabel || i18n.t('actions.confirm') }}
        </button>
      </div>
    </form>
  </div>
</template>

<style scoped>
/* 比其它对话框高一层：它常常是从别的对话框或菜单里问出来的 */
.ask-overlay {
  z-index: 80;
}
.ask-msg {
  white-space: pre-line;
}
.ask-danger-ic {
  color: var(--danger);
}
</style>
