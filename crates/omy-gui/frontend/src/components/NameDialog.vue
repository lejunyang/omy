<script setup lang="ts">
/** 输入一个名字：重命名与新建文件夹共用。
 *
 * 两者的界面只差标题和初始值，逻辑（校验、回车提交、Esc 取消）完全一样。
 * 拆成两个组件的话，「非法字符提示」这类改动早晚只改一处。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n';

/** 校验问题 → 文案键。
 *
 * 写成显式映射而不是 `'ctx.name_bad_' + problem`：拼出来的键无法被
 * `check-i18n-keys.py` 核对，漏翻一个的表现是界面上直接显示键名。
 */
const PROBLEM_KEYS = {
  empty: 'ctx.name_bad_empty',
  invalid: 'ctx.name_bad_invalid',
  slash: 'ctx.name_bad_slash',
  chars: 'ctx.name_bad_chars',
  trailing: 'ctx.name_bad_trailing',
  toolong: 'ctx.name_bad_toolong',
};

const props = defineProps({
  /** 'rename' | 'newfolder'，决定标题与按钮文案。 */
  mode: { type: String, required: true },
  /** 重命名时的原名，新建时为空。 */
  initial: { type: String, default: '' },
  busy: { type: Boolean, default: false },
});

const emit = defineEmits(['submit', 'cancel']);

const name = ref(props.initial);
const input = useTemplateRef('input');

/** 前端也校验一遍，理由不是「防止非法输入」（后端会拦），而是让用户在
 * 按下按钮之前就知道这个名字不行。等到提交后弹一条错误，他得先看懂
 * 错误码再回来改。
 *
 * 与后端 `fileops::is_valid_filename` 是同一套规则——**改一处必须改两处**。
 * 两边不一致的表现是「按钮亮着但点了报错」，或更糟：「按钮灰着但这个
 * 名字其实是合法的」，后者用户完全无从判断。
 */
const problem = computed(() => {
  const v = name.value.trim();
  if (!v) return 'empty';
  if (v === '.' || v === '..') return 'invalid';
  if (v.includes('/') || v.includes('\\')) return 'slash';
  if (/[:*?"<>|]/.test(v)) return 'chars';
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f]/.test(v)) return 'invalid';
  if (v.endsWith('.') || v.endsWith(' ')) return 'trailing';
  if (v.length > 255) return 'toolong';
  return '';
});

/** 名字没变时也不允许提交：那是一次什么都不做的改名，
 *  但会白跑一次 IO 并弹出「已重命名」，让人以为发生了什么 */
const canSubmit = computed(
  () => !problem.value && !props.busy && name.value.trim() !== props.initial,
);

function submit() {
  if (!canSubmit.value) return;
  emit('submit', name.value.trim());
}

onMounted(() => {
  const el = input.value;
  if (!el) return;
  el.focus();
  // 重命名时只选中主干、不选扩展名：改名基本都是改主干，
  // 全选会让用户先手动跳过 `.txt` 再改
  const dot = props.initial.lastIndexOf('.');
  if (props.mode === 'rename' && dot > 0) el.setSelectionRange(0, dot);
  else el.select();
});
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow" @submit.prevent="submit">
      <h3>
        <span aria-hidden="true">{{ mode === 'rename' ? '✏️' : '📁' }}</span>
        {{ i18n.t(mode === 'rename' ? 'ctx.rename' : 'ctx.new_folder') }}
      </h3>

      <div class="field">
        <label class="flabel" for="name-input">{{ i18n.t('ctx.name_label') }}</label>
        <input
          id="name-input"
          ref="input"
          v-model="name"
          type="text"
          autocomplete="off"
          @keydown.esc.prevent="$emit('cancel')"
        />
      </div>

      <!-- 名字为空时不报错：刚清空输入框就弹红字太急躁，
           此时提交按钮已经是灰的，用户不会误以为能提交 -->
      <div v-if="problem && problem !== 'empty'" class="errbox" role="alert">
        {{ i18n.t(PROBLEM_KEYS[problem] || 'ctx.name_bad_invalid') }}
      </div>

      <div class="acts">
        <button type="button" class="btn" :disabled="busy" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!canSubmit">
          {{ i18n.t(mode === 'rename' ? 'ctx.do_rename' : 'ctx.do_create') }}
        </button>
      </div>
    </form>
  </div>
</template>
