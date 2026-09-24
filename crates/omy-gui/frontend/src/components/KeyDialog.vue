<script setup lang="ts">
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
 * # 「当前有几个密码」分两种情况
 *
 * 可否认模式下查不出来：未使用的槽位填的是随机数据，与真实槽位不可区分。
 * 界面显示一个猜出来的数字比不显示更糟，所以明确告诉用户查不出来。直接
 * 后果是 `remove` 只能说「只保留当前密码」，不能说「删除某个密码」。
 *
 * 可管理模式下能查：文件里带一份加密的槽位目录，输入密码后就能列出每个
 * 槽位是什么，并精确删掉其中一个。这是加密时选的，事后改不了。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n';

const props = defineProps({
  /** 目标条目，需要 path 与显示名。 */
  entry: { type: Object, required: true },
  /**
   * 查询槽位的函数，由父组件注入。
   *
   * 不在这里直接 import api：对话框只管展示，让父组件决定怎么调后端。
   */
  querySlots: { type: Function, default: null },
  busy: { type: Boolean, default: false },
  error: { type: String, default: '' },
  /** 目标是树形加密的目录（整棵树共用一个密码）。 */
  isTree: { type: Boolean, default: false },
  /** 部分失败时，具体是哪些文件没改成。 */
  errorFiles: { type: Array, default: () => [] },
  /**
   * 可重试的文件数。0 表示没有可重试的。
   *
   * 与 errorFiles.length 分开传：展示清单是「给人看」，能不能重试取决于
   * 后端有没有给回完整路径。两者不一定相等，用长度代替会渲染出一个点了
   * 没反应的按钮
   */
  retryCount: { type: Number, default: 0 },
});

const emit = defineEmits(['cancel', 'retry', 'submit', 'remove-slot']);

/** `add` / `change` / `remove` / `reencrypt` */
const action = ref(props.isTree ? 'change' : 'add');

/** 这个目标允许哪些操作。
 *
 * 树和单个文件现在一样，四个操作都支持。
 *
 * 以前树不给 add / remove，是因为目录名由第一个 KEK 派生，一棵树同时
 * 只能有一个能浏览的密码——add 出来的第二个密码能打开每一个文件却解不开
 * 目录名，解密时报「文件损坏」。目录名改用两层结构（随机目录密钥 +
 * 每把钥匙包一份放在 .omy-keys 里）之后，任一把钥匙都能解开它。
 */
const availableActions = computed(() => ['add', 'change', 'remove', 'reencrypt']);
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

/** 槽位清单。null 表示还没查过。 */
const slots = ref(null);
/** 是不是可管理模式。null 表示还不知道（没查过或查失败）。 */
const managed = ref(null);
const slotsLoading = ref(false);

/**
 * 查询槽位清单。
 *
 * 要密码才能查——槽位目录是加密的。这不是不便，正是可管理模式的边界：
 * 对打不开这个文件的人，它与可否认模式一样什么都不透露。
 */
async function loadSlots() {
  if (!props.querySlots || !current.value || slotsLoading.value) return;
  slotsLoading.value = true;
  try {
    const r = await props.querySlots(props.entry.path, current.value);
    managed.value = r.managed;
    slots.value = r.slots;
  } catch {
    // 查不到就当作不知道，界面退回「不显示清单」。不弹错：用户可能只是
    // 密码还没打完，提交时自然会报「密码不正确」
    managed.value = null;
    slots.value = null;
  } finally {
    slotsLoading.value = false;
  }
}

/** 只显示占用的槽位。空槽对用户没有意义，列出来只是噪声。 */
const usedSlots = computed(() => (slots.value || []).filter((s) => s.kind !== 'empty'));

/** 还剩几把能打开文件的钥匙。用来拦住「删到一个不剩」。 */
const remaining = computed(() => usedSlots.value.length);

function removeSlot(s) {
  if (props.busy) return;
  emit('remove-slot', {
    path: props.entry.path,
    current: current.value,
    slotIndex: s.index,
    kind: s.kind,
  });
}

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
        <span v-if="isTree" class="ktag">{{ i18n.t('keymgmt.tree_tag') }}</span>
      </div>

      <!-- 这句必须常驻，不能藏在展开区里：用户对「有几个密码」的第一反应
           就是去界面上找那个数字，找不到会以为是 bug。
           树形不显示：整棵树只有一个密码，说「查不出有几个」反而制造困惑 -->
      <div v-if="isTree" class="hint">{{ i18n.t('keymgmt.tree_single_password') }}</div>
      <!-- 可管理模式不能再说「查不出来」：下面就列着清单，两句话自相矛盾 -->
      <div v-else-if="managed === true" class="hint">
        {{ i18n.t('keymgmt.slots_note_managed') }}
      </div>
      <div v-else class="hint">{{ i18n.t('keymgmt.slots_hidden') }}</div>

      <!-- 边车告知只对树显示：单个文件没有它。
           常驻而不是折叠起来——删掉 .omy-keys 是不可逆的，而它看起来
           就像个可以清理的隐藏文件，用户没有理由知道它要紧 -->
      <div v-if="isTree" class="warnbox" role="alert">
        {{ i18n.t('keymgmt.tree_sidecar_notice') }}
      </div>

      <div v-if="error" class="errbox" role="alert">
        {{ error }}
        <!-- 逐条列出没改成的文件。只说「部分文件失败」用户无从下手：
             不知道该处理什么，也判断不了损失有多大 -->
        <ul v-if="errorFiles.length" class="failed">
          <li v-for="f in errorFiles" :key="String(f)">{{ f }}</li>
        </ul>
        <!-- 重试按钮挨着清单放：它作用于上面这些文件，隔远了看不出关系。
             不放进底部按钮区——那里是取消/提交，混进去会被当成另一种提交。
             按钮上带数量：用户刚看完一屏红字，得知道这一下处理多少个 -->
        <button
          v-if="retryCount > 0"
          type="button"
          class="btn retry"
          :disabled="busy"
          @click="$emit('retry')"
        >
          {{ i18n.t('keymgmt.retry_failed', { n: retryCount }) }}
        </button>
      </div>

      <div class="hr"></div>

      <div class="field">
        <div class="flabel">{{ i18n.t('keymgmt.action') }}</div>
        <label v-for="a in availableActions" :key="a" class="radio">
          <input v-model="action" type="radio" :value="a" @change="onActionChange" />
          <span>
            {{ i18n.t(`keymgmt.${a}`) }}
            <div class="d">{{ i18n.t(`keymgmt.${a}_desc`) }}</div>
          </span>
        </label>
      </div>

<!-- 目录名现在由独立的目录密钥加密，与密码无关，所以换密码不会
           改名。这句从警告变成了安心提示——用户对「改密码会不会让文件夹
           不见」是有顾虑的，明确说不会比什么都不说好 -->
      <div v-if="isTree" class="note if">
        {{ i18n.t('keymgmt.tree_warning') }}
      </div>

      <!-- 轮换整棵树要按文件逐个重写，比单文件慢得多，得先说清楚 -->
      <div v-if="isTree && rewrites" class="warnbox" role="alert">
        {{ i18n.t('keymgmt.tree_reencrypt_warning') }}
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
          @change="querySlots && loadSlots()"
          @blur="querySlots && loadSlots()"
        />
        <div class="fhint">{{ i18n.t('keymgmt.current_hint') }}</div>
      </div>

      <!-- 槽位清单只在可管理模式下出现。可否认模式不显示空清单——
           那会让用户以为「一个密码都没有」，而文件明明能打开 -->
      <div v-if="managed === true && usedSlots.length" class="field slots">
        <div class="flabel">{{ i18n.t('keymgmt.slots_title') }}</div>
        <div v-for="s in usedSlots" :key="s.index" class="slotrow">
          <span class="sk">{{ i18n.t(`keymgmt.slot_kind_${s.kind}`) }}</span>
          <span v-if="s.current" class="sbadge">{{ i18n.t('keymgmt.slot_current') }}</span>
          <!-- 当前密码那一行不给删除按钮：删掉正在用的这把，对话框
               下一步就没法继续了 -->
          <button
            v-if="!s.current"
            type="button"
            class="btn slim danger"
            :disabled="busy || remaining <= 1"
            @click="removeSlot(s)"
          >
            {{ i18n.t('keymgmt.slot_remove') }}
          </button>
        </div>
      </div>
      <div v-else-if="managed === false && slots !== null" class="note if">
        {{ i18n.t('keymgmt.slots_deniable') }}
      </div>

      <template v-if="showsNext">
        <div class="field">
          <label class="flabel" for="k-new">
            {{ requiresNext ? i18n.t('keymgmt.next') : i18n.t('keymgmt.next_optional') }}
          </label>
          <input id="k-new" v-model="next" type="password" autocomplete="new-password" />
          <div v-if="same" class="ferr">{{ i18n.t('keymgmt.same') }}</div>
          <div v-if="!requiresNext" class="fhint">
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
