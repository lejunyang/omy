<script setup lang="ts">
/** 加密一个 Telegram 远程位置——输密码保护它的登录态。
 *
 * 与普通 omy 文件加密同款交互：输密码 + 再输一次确认 + 选 KDF 强度。
 * 加密的钥匙就是用户在这里现场输的密码；下次冷启动要输它才能进这个位置。
 * 已经解锁过同一个 omy 密码的话，那个密码也能直接开（后端把会话已解锁的
 * KEK 也建了槽）——但加密动作本身一定要在这里输一个密码。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n';

defineProps({
  /** 位置显示名，放标题里让用户确认加密的是哪个账号。 */
  name: { type: String, default: '' },
  busy: { type: Boolean, default: false },
});

const emit = defineEmits(['cancel', 'submit']);

const password = ref('');
const password2 = ref('');
/** interactive / moderate / sensitive，默认 moderate（与文件加密一致的稳妥档）。 */
const strength = ref('moderate');
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

const mismatch = computed(
  () => password2.value.length > 0 && password.value !== password2.value,
);
const canSubmit = computed(() => password.value.length > 0 && !mismatch.value);

function submit() {
  if (!canSubmit.value) return;
  emit('submit', { password: password.value, kdf_profile: strength.value });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow" @submit.prevent="submit">
      <h3>🔒 {{ i18n.t('rplace.encrypt_title', { name }) }}</h3>
      <div class="hint">{{ i18n.t('rplace.encrypt_dialog_hint') }}</div>

      <div class="field">
        <label class="flabel" for="te-pass">{{ i18n.t('encrypt.password') }}</label>
        <input
          id="te-pass"
          ref="input"
          v-model="password"
          type="password"
          autocomplete="new-password"
        />
      </div>

      <div class="field">
        <label class="flabel" for="te-pass2">{{ i18n.t('encrypt.password_again') }}</label>
        <input id="te-pass2" v-model="password2" type="password" autocomplete="new-password" />
        <div v-if="mismatch" class="ferr">{{ i18n.t('encrypt.password_mismatch') }}</div>
      </div>

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.strength') }}</div>
        <label v-for="s in ['interactive', 'moderate', 'sensitive']" :key="s" class="radio">
          <input v-model="strength" type="radio" :value="s" />
          <span>
            {{ i18n.t(`encrypt.strength_${s}`) }}
            <div class="d">{{ i18n.t(`encrypt.strength_${s}_desc`) }}</div>
          </span>
        </label>
      </div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!canSubmit || busy">
          {{ busy ? i18n.t('busy.encrypting') : i18n.t('rplace.encrypt_submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
