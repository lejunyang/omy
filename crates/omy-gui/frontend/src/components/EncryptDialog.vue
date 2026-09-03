<script setup>
/** 加密对话框。
 *
 * 选项取自 `docs/research/appendix/ui-prototype.html` 的加密对话框，
 * 以及决策 D-14（分块大小可自定义）、D-19（不提供安全擦除）。
 */

import { ref, computed } from 'vue';
import * as i18n from '../i18n.js';

const props = defineProps({
  /** 待加密的条目。 */
  targets: { type: Array, required: true },
  busy: { type: Boolean, default: false },
});

const emit = defineEmits(['cancel', 'submit']);

const password = ref('');
const password2 = ref('');
/** `encrypt` / `keep_ext` / `plain` */
const filenameMode = ref('encrypt');
const compress = ref(true);
/** 2 的幂次，索引进 CHUNKS */
const chunkIndex = ref(2);
const strength = ref('moderate');
const original = ref('keep');
/** 文件夹模式：`container` 打包成单文件，`tree` 逐个加密。
 *
 * 默认 container 而不是「记住上次选择」：两种模式泄露的元数据量不同，
 * 而「上次选了什么」是用户看不见的状态。加密一个敏感文件夹时，不该因为
 * 上次选过 tree 就默默沿用——container 是更安全的那个，作默认值合适。
 */
const folderMode = ref('container');

/** 可选的分块大小。
 *
 * 下限 64 KiB：再小的话每块的 nonce 与 tag 开销占比过高。
 * 上限 16 MiB：更大会让 seek 精度变差，且解密时内存峰值明显。
 */
const CHUNKS = [65536, 131072, 262144, 524288, 1048576, 2097152, 4194304, 8388608, 16777216];

const chunkSize = computed(() => CHUNKS[chunkIndex.value] ?? 262144);
const chunkLabel = computed(() => i18n.formatSize(chunkSize.value));

const hasFolder = computed(() => props.targets.some((t) => t.is_dir));

const mismatch = computed(
  () => password2.value.length > 0 && password.value !== password2.value,
);

const canSubmit = computed(
  () => password.value.length > 0 && !mismatch.value && !props.busy,
);

const totalSize = computed(() =>
  props.targets.reduce((s, t) => s + (t.size || 0), 0),
);

function submit() {
  if (!canSubmit.value) return;
  emit('submit', {
    password: password.value,
    encrypt_filename: filenameMode.value !== 'plain',
    preserve_extension: filenameMode.value === 'keep_ext',
    compress: compress.value,
    chunk_size: chunkSize.value,
    kdf_profile: strength.value,
    original: original.value,
    folder_mode: folderMode.value,
  });
}
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('cancel')">
    <form class="dlg" @submit.prevent="submit">
      <h3>{{ i18n.tn('encrypt.title', targets.length) }}</h3>

      <div class="filelist">
        <div v-for="t in targets.slice(0, 6)" :key="t.path" class="frow">
          <span class="fn">{{ t.is_dir ? '📁' : '📄' }} {{ t.name }}</span>
          <span class="fs">{{ t.size == null ? '' : i18n.formatSize(t.size) }}</span>
        </div>
        <div v-if="targets.length > 6" class="frow more">
          + {{ i18n.tn('status.files', targets.length - 6) }}
        </div>
        <div class="frow total">
          <span class="fn">{{ i18n.t('status.total_size', { size: i18n.formatSize(totalSize) }) }}</span>
        </div>
      </div>

      <div v-if="hasFolder" class="field">
        <div class="flabel">{{ i18n.t('encrypt.folder_mode') }}</div>
        <label class="radio">
          <input v-model="folderMode" type="radio" value="container" />
          <span>
            {{ i18n.t('encrypt.folder_mode_container') }}
            <div class="d">{{ i18n.t('encrypt.folder_mode_container_desc') }}</div>
          </span>
        </label>
        <label class="radio">
          <input v-model="folderMode" type="radio" value="tree" />
          <span>
            {{ i18n.t('encrypt.folder_mode_tree') }}
            <div class="d">{{ i18n.t('encrypt.folder_mode_tree_desc') }}</div>
          </span>
        </label>
        <!-- 泄露量必须在勾选那一刻就摆在眼前，而不是藏在帮助文档里。
             措辞刻意具体：「泄露元数据」用户无法据此判断风险，
             「别人能数出你有多少文件」才能（威胁模型 N6）。 -->
        <div v-if="folderMode === 'tree'" class="warnbox">
          {{ i18n.t('encrypt.folder_mode_tree_leak') }}
        </div>
      </div>

      <div class="hr"></div>

      <div class="field">
        <label class="flabel" for="e-pass">{{ i18n.t('encrypt.password') }}</label>
        <input id="e-pass" v-model="password" type="password" autocomplete="new-password" />
      </div>

      <div class="field">
        <label class="flabel" for="e-pass2">{{ i18n.t('encrypt.password_again') }}</label>
        <input id="e-pass2" v-model="password2" type="password" autocomplete="new-password" />
        <div v-if="mismatch" class="ferr">{{ i18n.t('encrypt.password_mismatch') }}</div>
      </div>

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.filename') }}</div>
        <label class="radio">
          <input v-model="filenameMode" type="radio" value="encrypt" />
          <span>{{ i18n.t('encrypt.filename_encrypt') }}</span>
        </label>
        <label class="radio">
          <input v-model="filenameMode" type="radio" value="keep_ext" />
          <span>
            {{ i18n.t('encrypt.filename_keep_ext') }}
            <div class="d">{{ i18n.t('encrypt.filename_keep_ext_desc') }}</div>
          </span>
        </label>
        <label class="radio">
          <input v-model="filenameMode" type="radio" value="plain" />
          <span>
            {{ i18n.t('encrypt.filename_plain') }}
            <div class="d">{{ i18n.t('encrypt.filename_plain_desc') }}</div>
          </span>
        </label>
      </div>

      <div class="field">
        <label class="radio">
          <input v-model="compress" type="checkbox" />
          <span>
            {{ i18n.t('encrypt.compress') }}
            <div class="d">{{ i18n.t('encrypt.compress_desc') }}</div>
          </span>
        </label>
      </div>

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.chunk_size') }} · {{ chunkLabel }}</div>
        <input
          v-model.number="chunkIndex"
          class="slider"
          type="range"
          min="0"
          :max="CHUNKS.length - 1"
          step="1"
        />
        <div class="sliderv">
          <span>64 KB</span>
          <span>{{ i18n.t('encrypt.chunk_hint') }}</span>
          <span>16 MB</span>
        </div>
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

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.original') }}</div>
        <label class="radio">
          <input v-model="original" type="radio" value="keep" />
          <span>{{ i18n.t('encrypt.original_keep') }}</span>
        </label>
        <label class="radio">
          <input v-model="original" type="radio" value="trash" />
          <span>
            {{ i18n.t('encrypt.original_trash') }}
            <div class="d">{{ i18n.t('encrypt.original_trash_desc') }}</div>
          </span>
        </label>
        <label class="radio">
          <input v-model="original" type="radio" value="delete" />
          <span>
            {{ i18n.t('encrypt.original_delete') }}
            <div class="d">{{ i18n.t('encrypt.original_delete_desc') }}</div>
          </span>
        </label>
      </div>

      <div class="note if">{{ i18n.t('encrypt.no_erase_note') }}</div>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
        <button type="submit" class="btn primary" :disabled="!canSubmit">
          {{ busy ? i18n.t('busy.encrypting') : i18n.t('encrypt.submit') }}
        </button>
      </div>
    </form>
  </div>
</template>
