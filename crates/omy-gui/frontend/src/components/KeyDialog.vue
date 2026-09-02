<script setup>
/** 密码管理对话框：给一个已加密文件增删改密码。
 *
 * # 为什么三个操作放在一个对话框里
 *
 * 它们在格式层面是同一个操作——重建 slot 区。分成三个入口会让人以为
 * 它们代价不同（比如「改密码要重新加密吧」），而实际上都是毫秒级。
 *
 * # 为什么不显示「当前有几个密码」
 *
 * 查不出来。未使用的槽位填的是随机数据，与真实槽位不可区分（可否认性）。
 * 界面显示一个猜出来的数字比不显示更糟，所以这里明确告诉用户查不出来。
 *
 * 直接后果是 `remove` 只能说「只保留当前密码」，不能说「删除某个密码」——
 * 我们既不知道有哪些密码，也无法只删其中一个。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';

const props = defineProps({
  /** 目标条目，需要 path 与显示名。 */
  entry: { type: Object, required: true },
  busy: { type: Boolean, default: false },
  error: { type: String, default: '' },
});

const emit = defineEmits(['cancel', 'submit']);

/** `add` / `change` / `remove` */
const action = ref('add');
const current = ref('');
const next = ref('');
const next2 = ref('');
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

const needsNext = computed(() => action.value !== 'remove');

const mismatch = computed(
  () => needsNext.value && next2.value.length > 0 && next.value !== next2.value,
);

/** 新密码与当前密码相同：这不是笔误就是误解，拦下来并说明原因。 */
const same = computed(
  () => needsNext.value && next.value.length > 0 && next.value === current.value,
);

const canSubmit = computed(() => {
  if (props.busy || !current.value) return false;
  if (!needsNext.value) return true;
  return next.value.length > 0 && !mismatch.value && !same.value;
});

/** 换操作时清掉新密码输入框。
 *
 * 不清的话，从 add 切到 remove 再切回来，输入框里还留着上次打的东西，
 * 用户以为自己没填、直接提交，结果用了一个他已经不记得的密码。
 */
function onActionChange() {
  next.value = '';
  next2.value = '';
}

function submit() {
  if (!canSubmit.value) return;
  emit('submit', {
    path: props.entry.path,
    action: action.value,
    current: current.value,
    // remove 不能带新密码，后端会拒绝——这里也不发，
    // 保证前后端对「这个操作不需要新密码」的理解一致
    next: needsNext.value ? next.value : '',
  });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow keymgmt" @submit.prevent="submit">
      <h3>🔑 {{ i18n.t('keymgmt.title') }}</h3>

      <div class="krow">
        <span class="klabel">{{ i18n.t('keymgmt.target') }}</span>
        <span class="kname">{{ entry.real_name || entry.name }}</span>
      </div>

      <!-- 这句必须常驻，不能藏在展开区里：用户对「有几个密码」的第一反应
           就是去界面上找那个数字，找不到会以为是 bug -->
      <div class="hint">{{ i18n.t('keymgmt.slots_hidden') }}</div>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <div class="hr"></div>

      <div class="field">
        <div class="flabel">{{ i18n.t('keymgmt.action') }}</div>
        <label v-for="a in ['add', 'change', 'remove']" :key="a" class="radio">
          <input v-model="action" type="radio" :value="a" @change="onActionChange" />
          <span>
            {{ i18n.t(`keymgmt.${a}`) }}
            <div class="d">{{ i18n.t(`keymgmt.${a}_desc`) }}</div>
          </span>
        </label>
      </div>

      <!-- 移除会作废其它密码，且旧副本仍可用旧密码打开。后者是最容易
           产生错觉的地方：以为「删掉密码」等于「那个人再也看不到了」 -->
      <div v-if="action === 'remove'" class="warnbox" role="alert">
        {{ i18n.t('keymgmt.stale_copy_warning') }}
      </div>

      <div class="field">
        <label class="flabel" for="k-cur">{{ i18n.t('keymgmt.current') }}</label>
        <input
          id="k-cur"
          ref="input"
          v-model="current"
          type="password"
          autocomplete="current-password"
        />
        <div class="d">{{ i18n.t('keymgmt.current_hint') }}</div>
      </div>

      <template v-if="needsNext">
        <div class="field">
          <label class="flabel" for="k-new">{{ i18n.t('keymgmt.next') }}</label>
          <input id="k-new" v-model="next" type="password" autocomplete="new-password" />
          <div v-if="same" class="ferr">{{ i18n.t('keymgmt.same') }}</div>
        </div>

        <div class="field">
          <label class="flabel" for="k-new2">{{ i18n.t('keymgmt.next_again') }}</label>
          <input id="k-new2" v-model="next2" type="password" autocomplete="new-password" />
          <div v-if="mismatch" class="ferr">{{ i18n.t('keymgmt.mismatch') }}</div>
        </div>
      </template>

      <div class="note if">{{ i18n.t('keymgmt.payload_note') }}</div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button
          type="submit"
          class="btn primary"
          :class="{ danger: action === 'remove' }"
          :disabled="!canSubmit"
        >
          {{ busy ? i18n.t('keymgmt.working') : i18n.t('keymgmt.submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
