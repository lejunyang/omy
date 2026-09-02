<script setup>
/** 密码管理对话框：给一个已加密文件增删改密码，或重新加密。
 *
 * # 为什么前三个操作放在一起
 *
 * add / change / remove 在格式层面是同一个操作——重建 slot 区。分成三个
 * 入口会让人以为它们代价不同（比如「改密码要重新加密吧」），而实际上都是
 * 毫秒级。
 *
 * # 为什么 reencrypt 也在这个对话框里，而不是单独入口
 *
 * 用户是**带着同一个问题**来的：「我怎么让某个密码不再有效」。答案分两种
 * （只改这份文件的密码集合 / 连文件密钥一起换掉），差别正是这个对话框要
 * 解释的。放到另一个入口，用户根本不会知道存在第二种，会误以为 remove
 * 就已经彻底作废了旧密码。
 *
 * 它与前三个的代价差一个数量级，所以要显式警示耗时，并用红色确认按钮。
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

/** `add` / `change` / `remove` / `reencrypt` */
const action = ref('add');
const current = ref('');
const next = ref('');
const next2 = ref('');
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

/** 要不要显示新密码输入框。 */
const showsNext = computed(() => action.value !== 'remove');

/** 新密码是不是必填。
 *
 * reencrypt 例外：它即使密码不变也有意义（换掉文件密钥，让旧副本的密码
 * 对这份文件失效）。强制填新密码会让这个正当需求无法表达。
 */
const requiresNext = computed(() => action.value === 'add' || action.value === 'change');

/** 这个操作会重写载荷吗——决定底部说明与耗时警示。 */
const rewrites = computed(() => action.value === 'reencrypt');

const mismatch = computed(
  () => showsNext.value && next2.value.length > 0 && next.value !== next2.value,
);

/** 新密码与当前密码相同：这不是笔误就是误解，拦下来并说明原因。
 *
 * 对 reencrypt 不算错——密码不变也是有效用法，所以不拦。
 */
const same = computed(
  () =>
    requiresNext.value && next.value.length > 0 && next.value === current.value,
);

const canSubmit = computed(() => {
  if (props.busy || !current.value) return false;
  if (requiresNext.value && next.value.length === 0) return false;
  // 填了就要校验，哪怕是可选的：两次不一致说明打错了，
  // 提交下去会得到一个自己也打不开的文件
  return !mismatch.value && !same.value;
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
    next: showsNext.value ? next.value : '',
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
        <label
          v-for="a in ['add', 'change', 'remove', 'reencrypt']"
          :key="a"
          class="radio"
        >
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

      <!-- 轮换要讲两件事：慢（与文件大小成正比），以及它同样收不回已经
           流出去的副本。只说前者会让人以为轮换=彻底作废旧密码 -->
      <div v-if="rewrites" class="warnbox" role="alert">
        {{ i18n.t('keymgmt.reencrypt_warning') }}
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

      <template v-if="showsNext">
        <div class="field">
          <label class="flabel" for="k-new">
            {{ requiresNext ? i18n.t('keymgmt.next') : i18n.t('keymgmt.next_optional') }}
          </label>
          <input id="k-new" v-model="next" type="password" autocomplete="new-password" />
          <div v-if="same" class="ferr">{{ i18n.t('keymgmt.same') }}</div>
          <div v-if="!requiresNext" class="d">
            {{ i18n.t('keymgmt.next_optional_hint') }}
          </div>
        </div>

        <div class="field">
          <label class="flabel" for="k-new2">{{ i18n.t('keymgmt.next_again') }}</label>
          <input id="k-new2" v-model="next2" type="password" autocomplete="new-password" />
          <div v-if="mismatch" class="ferr">{{ i18n.t('keymgmt.mismatch') }}</div>
        </div>
      </template>

      <!-- 这句必须跟着操作变：对 reencrypt 说「不重写载荷」是把实际
           情况说反了 -->
      <div class="note if">
        {{ rewrites ? i18n.t('keymgmt.rewrite_note') : i18n.t('keymgmt.payload_note') }}
      </div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button
          type="submit"
          class="btn primary"
          :class="{ danger: action === 'remove' || rewrites }"
          :disabled="!canSubmit"
        >
          {{ busy ? i18n.t('keymgmt.working') : i18n.t('keymgmt.submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
