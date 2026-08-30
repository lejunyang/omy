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

      <div v-if="hasFolder" class="note">{{ i18n.t('encrypt.folder_note') }}</div>

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
