<script setup lang="ts">
/** 还原对话框：把加密文件解出来放到磁盘上。
 *
 * # 为什么目标位置是单选而不是一个路径输入框
 *
 * 「当前目录」是绝大多数情况下想要的（在哪看到的就解在哪），让用户
 * 每次都去点一次文件夹选择器是多余的。但默认值又不能是「上次选的
 * 目录」——那会让同一个按钮在不同时刻把文件写到不同地方，而写盘是
 * 有副作用的操作，不该有隐藏状态。
 *
 * 所以做成两个显式选项，默认落在「当前目录」，并且把最终路径直接显示
 * 出来：用户点确定之前就能看到文件会出现在哪。
 */

import { ref, computed } from 'vue';
import * as i18n from '../i18n';
import * as api from '../api';

const props = defineProps({
  /** 待还原的条目（已解锁的加密文件）。 */
  targets: { type: Array as () => any[], required: true },
  /** 当前所在目录，作为「还原到当前目录」的落点显示。 */
  currentDir: { type: String, default: '' },
  busy: { type: Boolean, default: false },
});

const emit = defineEmits(['cancel', 'submit']);

/** `here`（源文件所在目录）/ `pick`（自选目录）。 */
const mode = ref('here');
const picked = ref('');
const overwrite = ref(false);
/** 选目录时的报错，单独存：它和整体提交失败不是一回事。 */
const pickError = ref('');

const totalSize = computed(() =>
  props.targets.reduce((s, t) => s + (t.size || 0), 0),
);

/** 有没有文件夹容器。有的话要提示会建一层目录。 */
const hasContainer = computed(() => props.targets.some((t) => t.is_container));

/** 最终落点，直接显示给用户。
 *
 * 「当前目录」显示 currentDir 而不是空白：用户需要在点确定之前
 * 就知道文件会出现在哪。
 */
const targetLabel = computed(() =>
  mode.value === 'pick' ? picked.value : props.currentDir,
);

const canSubmit = computed(
  () =>
    props.targets.length > 0 &&
    !props.busy &&
    // 选了「自选目录」却还没选，不能提交：否则会静默落到源目录，
    // 而用户以为自己已经指定了别的地方
    (mode.value !== 'pick' || picked.value.length > 0),
);

async function choose() {
  pickError.value = '';
  try {
    const dir = await api.pickFolder(i18n.t('restore.pick_title'));
    // 用户取消返回 null，此时不要清掉已选的值——
    // 「点开又取消」不应该撤销上一次的选择
    if (dir) {
      picked.value = dir;
      mode.value = 'pick';
    }
  } catch (e) {
    pickError.value = i18n.te(api.errCode(e), i18n.t('restore.pick_failed'));
  }
}

function submit() {
  if (!canSubmit.value) return;
  emit('submit', {
    // here 传 null 让后端用「源文件所在目录」，而不是前端把 currentDir
    // 拼进去：容器里浏览时 currentDir 是容器内的虚拟路径，不是磁盘路径
    target_dir: mode.value === 'pick' ? picked.value : null,
    overwrite: overwrite.value,
  });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg" @submit.prevent="submit">
      <h3>{{ i18n.tn('restore.title', targets.length) }}</h3>

      <div class="filelist">
        <div v-for="t in targets.slice(0, 6)" :key="t.path" class="frow">
          <span class="fn"><AppIcon :name="t.is_container ? 'package' : 'file'" /> {{ t.name }}</span>
          <span class="fs">{{ t.size == null ? '' : i18n.formatSize(t.size) }}</span>
        </div>
        <div v-if="targets.length > 6" class="frow more">
          + {{ i18n.tn('status.files', targets.length - 6) }}
        </div>
        <div class="frow total">
          <span class="fn">{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
        </div>
      </div>

      <div v-if="hasContainer" class="note">{{ i18n.t('restore.container_note') }}</div>

      <div class="hr"></div>

      <div class="field">
        <div class="flabel">{{ i18n.t('restore.target') }}</div>
        <label class="radio">
          <input v-model="mode" type="radio" value="here" />
          <span>
            {{ i18n.t('restore.target_here') }}
            <div class="d">{{ i18n.t('restore.target_here_desc') }}</div>
          </span>
        </label>
        <label class="radio">
          <input v-model="mode" type="radio" value="pick" />
          <span>
            {{ i18n.t('restore.target_pick') }}
            <div class="d">{{ i18n.t('restore.target_pick_desc') }}</div>
          </span>
        </label>
        <button type="button" class="btn small pickbtn" @click="choose">
          <AppIcon name="folder-open" /> {{ i18n.t('restore.choose') }}
        </button>
        <div v-if="pickError" class="ferr">{{ pickError }}</div>
        <div v-if="targetLabel" class="tpath">{{ targetLabel }}</div>
      </div>

      <div class="field">
        <label class="radio">
          <input v-model="overwrite" type="checkbox" />
          <span>
            {{ i18n.t('restore.overwrite') }}
            <div class="d">{{ i18n.t('restore.overwrite_desc') }}</div>
          </span>
        </label>
      </div>

      <div class="note if">{{ i18n.t('restore.plaintext_note') }}</div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!canSubmit">
          {{ busy ? i18n.t('busy.restoring') : i18n.t('restore.submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
