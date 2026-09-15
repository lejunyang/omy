<script setup>
/** 恢复码对话框：生成一份新的，或用已有的重设密码。
 *
 * # 为什么不并进 KeyDialog
 *
 * 那个对话框回答的是「我怎么让某个密码不再有效」，四个操作共享同一套
 * 输入（当前密码 + 新密码）。恢复码是另外两个心智：
 *
 * - **生成**：一次性展示 26 个词，用户要抄下来，此后再也看不到
 * - **使用**：密码已经忘了，输入那 26 个词换一个新密码
 *
 * 前者根本没有「新密码」这个概念，后者根本没有「当前密码」。硬塞进去
 * 会让那个对话框变成一堆互斥分支，而用户来的时候本来就很清楚自己要
 * 哪一个。
 *
 * # 生成后为什么要强制勾选「我已抄下」
 *
 * 后端不保存恢复码明文，关掉这个框就真的没了。摩擦在这里是必要的——
 * 用户点「完成」时必须已经意识到「这串东西不会再出现」。
 *
 * # 词为什么不做成可复制的一整行
 *
 * 有「复制」按钮的话，多数人会粘到某个笔记应用里——那是同步到云端、
 * 进入搜索索引、留在剪贴板历史里的一份明文万能钥匙。所以只做分行编号
 * 展示，引导抄在纸上。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';

const props = defineProps({
  /** 目标条目，需要 path 与显示名。 */
  entry: { type: Object, required: true },
  /** generate 或 estore，由调用方按用户点的菜单项决定。 */
  mode: { type: String, default: 'generate' },
  busy: { type: Boolean, default: false },
  error: { type: String, default: '' },
  /**
   * 生成成功后的 26 个词。
   *
   * 由父组件持有而不是本组件自己存：对话框关闭即卸载，词随之消失，
   * 不会因为组件被 keep-alive 之类的机制意外留存。
   */
  words: { type: Array, default: () => [] },
  /** 本次是否可能顶掉了原本挂在该槽位上的其它密码。 */
  mayHaveEvicted: { type: Boolean, default: false },
});

const emit = defineEmits(['cancel', 'generate', 'restore', 'done']);

const current = ref('');
const code = ref('');
const next = ref('');
const next2 = ref('');
const acknowledged = ref(false);
const input = useTemplateRef('input');

onMounted(() => input.value?.focus());

/** 已经生成出来了——此时界面切换到「抄写」态。 */
const generated = computed(() => props.words.length > 0);

/** 未生成时的标题键。
 *
 * 在 script 里算好而不是在模板里拼字符串：模板里拼键名拼错了只会渲染出
 * 一个键名，不会报错，而这类问题在截图上看着就像「文案没翻译」。
 */
const titleKey = computed(() =>
  props.mode === 'generate' ? 'recovery.generate_title' : 'recovery.restore_title',
);

/** 把 26 个词按每行 4 个分组并编号。
 *
 * 排成一行没法抄——用户会数不清抄到哪个了。编号还让「第 7 个词有问题」
 * 这类提示能被真正用上。
 */
const rows = computed(() => {
  const out = [];
  for (let i = 0; i < props.words.length; i += 4) {
    out.push({ start: i + 1, words: props.words.slice(i, i + 4) });
  }
  return out;
});

const mismatch = computed(
  () => next2.value.length > 0 && next.value !== next2.value,
);

const canGenerate = computed(() => !props.busy && current.value.length > 0);
const canRestore = computed(
  () =>
    !props.busy &&
    code.value.trim().length > 0 &&
    next.value.length > 0 &&
    !mismatch.value,
);

function onGenerate() {
  if (!canGenerate.value) return;
  emit('generate', { path: props.entry.path, current: current.value });
}

function onRestore() {
  if (!canRestore.value) return;
  emit('restore', {
    path: props.entry.path,
    code: code.value,
    next: next.value,
  });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg narrow recovery" @submit.prevent="generated ? $emit('done') : (mode === 'generate' ? onGenerate() : onRestore())">
      <h3>
        {{ mode === 'generate' ? '🔐' : '🔓' }}
        {{ i18n.t(generated ? 'recovery.generated_title' : titleKey) }}
      </h3>

      <div class="krow">
        <span class="klabel">{{ i18n.t('keymgmt.target') }}</span>
        <span class="kname">{{ entry.real_name || entry.name }}</span>
      </div>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <!-- ============ 已生成：展示并要求抄写 ============ -->
      <template v-if="generated">
        <div class="hint">{{ i18n.t('recovery.write_it_down') }}</div>

        <div class="words">
          <div v-for="r in rows" :key="r.start" class="wrow">
            <span class="wnum">{{ r.start }}</span>
            <span v-for="w in r.words" :key="w" class="word">{{ w }}</span>
          </div>
        </div>

        <!-- 三条警告必须在词的下面：放上面会被一长串词挤出视野 -->
        <div class="warnbox" role="alert">{{ i18n.t('recovery.warn_once') }}</div>
        <div class="warnbox" role="alert">{{ i18n.t('recovery.warn_weakest') }}</div>
        <div class="warnbox" role="alert">{{ i18n.t('recovery.warn_no_scan') }}</div>
        <div v-if="mayHaveEvicted" class="warnbox" role="alert">
          {{ i18n.t('recovery.warn_evicted') }}
        </div>

        <!-- 必须勾选才能关：后端不保存明文，关掉就真的没了 -->
        <label class="ack">
          <input v-model="acknowledged" type="checkbox" />
          <span>{{ i18n.t('recovery.acknowledge') }}</span>
        </label>

        <div class="acts">
          <button
            type="submit"
            class="btn primary"
            :disabled="!acknowledged"
          >
            {{ i18n.t('recovery.done') }}
          </button>
        </div>
      </template>

      <!-- ============ 生成前：只要当前密码 ============ -->
      <template v-else-if="mode === 'generate'">
        <div class="hint">{{ i18n.t('recovery.generate_hint') }}</div>
        <div class="field">
          <label class="flabel" for="r-cur">{{ i18n.t('keymgmt.current') }}</label>
          <input
            id="r-cur"
            ref="input"
            v-model="current"
            type="password"
            autocomplete="current-password"
          />
          <div class="fhint">{{ i18n.t('recovery.generate_why_password') }}</div>
        </div>
        <div class="acts">
          <button type="button" class="btn" @click="$emit('cancel')">
            {{ i18n.t('actions.cancel') }}
          </button>
          <button type="submit" class="btn primary" :disabled="!canGenerate">
            {{ busy ? i18n.t('keymgmt.working') : i18n.t('recovery.generate_submit') }}
          </button>
        </div>
      </template>

      <!-- ============ 使用恢复码 ============ -->
      <template v-else>
        <div class="hint">{{ i18n.t('recovery.restore_hint') }}</div>
        <div class="field">
          <label class="flabel" for="r-code">{{ i18n.t('recovery.code_label') }}</label>
          <!-- 用 textarea 而不是 input：26 个词一行放不下，而用户是
               照着纸抄进来的，需要能看见自己抄到哪了 -->
          <textarea
            id="r-code"
            ref="input"
            v-model="code"
            rows="4"
            spellcheck="false"
            autocomplete="off"
          ></textarea>
          <div class="fhint">{{ i18n.t('recovery.code_hint') }}</div>
        </div>
        <div class="field">
          <label class="flabel" for="r-new">{{ i18n.t('keymgmt.next') }}</label>
          <input id="r-new" v-model="next" type="password" autocomplete="new-password" />
        </div>
        <div class="field">
          <label class="flabel" for="r-new2">{{ i18n.t('keymgmt.next_again') }}</label>
          <input id="r-new2" v-model="next2" type="password" autocomplete="new-password" />
          <div v-if="mismatch" class="ferr">{{ i18n.t('keymgmt.mismatch') }}</div>
        </div>
        <div class="note if">{{ i18n.t('recovery.restore_note') }}</div>
        <div class="acts">
          <button type="button" class="btn" @click="$emit('cancel')">
            {{ i18n.t('actions.cancel') }}
          </button>
          <button type="submit" class="btn primary" :disabled="!canRestore">
            {{ busy ? i18n.t('keymgmt.working') : i18n.t('recovery.restore_submit') }}
          </button>
        </div>
      </template>
    </form>
  </div>
</template>
