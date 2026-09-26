<script setup lang="ts">
/** 输密码解锁当前目录。
 *
 * 双击一个锁着的加密文件时弹出。解锁的粒度是**目录**而不是单个文件：
 * 同一个库里的文件共用一个 vault salt，解开一个等于解开一批，
 * 逐个问密码只会让用户重复输入同一个密码。
 *
 * # 会话已有密码时，这里是「再加一个」而不是「换一个」
 *
 * 后端走累加语义，新密码不会顶掉已装入的。但用户没理由知道这一点——
 * 大多数应用的密码框都是「重来一次」。所以已有密码时必须把这件事
 * 说出来，否则想同时看两批文件的人根本不会想到可以再输一次。
 */

import { ref, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n';

defineProps({
  busy: { type: Boolean, default: false },
  error: { type: String, default: '' },
  /** 会话里已装入的密码数，决定提示说「解锁」还是「再加一个」。 */
  loaded: { type: Number, default: 0 },
  /**
   * 这个位置能不能用 Windows Hello 免密解锁。
   *
   * 只有「这台电脑支持 且 这个库已启用」时才是 true。两个条件缺一个就
   * 不显示按钮——摆一个点了就报错的按钮比没有更糟。
   */
  deviceKey: { type: Boolean, default: false },
});

const emit = defineEmits(['cancel', 'submit', 'device-unlock']);

const password = ref('');
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

function submit() {
  if (!password.value) return;
  emit('submit', { password: password.value });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form id="unlock-form" class="dlg narrow" @submit.prevent="submit">
      <h3>🔒 {{ i18n.t('unlock.title') }}</h3>
      <!-- 已有密码时换一句话：不说清楚的话，没人会想到可以再输一个 -->
      <div class="hint">
        {{
          loaded > 0
            ? i18n.tn('unlock.add_more_hint', loaded)
            : i18n.t('unlock.quick_hint')
        }}
      </div>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <!-- Hello 按钮放在密码框之前：它才是这个位置的常用路径，
           放后面用户会先去点密码框。只在真能用时才出现 -->
      <div v-if="deviceKey" class="field">
        <button
          type="button"
          class="btn wide"
          :disabled="busy"
          @click="$emit('device-unlock')"
        >
          {{ i18n.t('devicekey.unlock_with_hello') }}
        </button>
      </div>

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
