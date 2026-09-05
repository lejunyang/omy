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

/** 是否生成缩略图。
 *
 * 默认开：缩略图存在加密 TLV 里，只有拿到密码才能读，所以它不泄露内容，
 * 而没有缩略图的网格视图基本没法用。留出开关是因为它确实让文件大几 KB。
 */
const thumbnail = ref(true);
/** 视频取帧时间点，形如 `12`、`12.5`、`1:23`、`00:01:23`。
 *
 * 空串表示自动取时长 10% 处。存成字符串而不是数字：用户习惯写 `1:23`，
 * 而 number 类型的 input 收不下冒号。
 */
const frameText = ref('');

/** 视频扩展名。
 *
 * 只用来决定「要不要显示取帧输入框」，判错了不影响加密结果——真正的
 * 图/视频分派在后端按容器探测（扩展名可以是骗人的）。
 */
const VIDEO_EXT = [
  'mp4', 'mov', 'mkv', 'webm', 'avi', 'm4v', 'wmv', 'flv', 'mpg', 'mpeg', 'ts', 'm2ts', '3gp',
];

/** 选中的条目里有没有视频。
 *
 * 文件夹也算：里面很可能有视频，而这里看不到内容。
 */
const hasVideo = computed(() =>
  props.targets.some((x) => {
    if (x.is_dir) return true;
    const i = (x.name || '').lastIndexOf('.');
    if (i < 0) return false;
    return VIDEO_EXT.includes(x.name.slice(i + 1).toLowerCase());
  }),
);

/** 把 `1:23.5` / `83.5` 解析成秒。非法或空串返回 null。
 *
 * 不用 parseFloat 直接吞：`parseFloat('1:23')` 得 1，会把「1 分 23 秒」
 * 静默变成第 1 秒——用户拿到的缩略图不是他选的那一帧，却没有任何提示。
 */
function parseFrame(s) {
  const v = (s || '').trim();
  if (!v) return null;
  const parts = v.split(':');
  if (parts.length > 3) return null;
  let sec = 0;
  for (const p of parts) {
    // 每段必须是纯数字（末段可带小数），否则视为非法
    if (!/^\d*\.?\d*$/.test(p) || p === '' || p === '.') return null;
    sec = sec * 60 + parseFloat(p);
  }
  if (!Number.isFinite(sec) || sec < 0) return null;
  return sec;
}

/** 取帧时间点填了但解析不出来。 */
const frameInvalid = computed(
  () => frameText.value.trim().length > 0 && parseFrame(frameText.value) === null,
);

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
  () => password.value.length > 0 && !mismatch.value && !frameInvalid.value && !props.busy,
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
    thumbnail: thumbnail.value ? 'auto' : 'none',
    thumbnail_frame: thumbnail.value ? parseFrame(frameText.value) : null,
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
        <label class="radio">
          <input v-model="thumbnail" type="checkbox" />
          <span>
            {{ i18n.t('encrypt.thumbnail') }}
            <div class="d">{{ i18n.t('encrypt.thumbnail_desc') }}</div>
          </span>
        </label>
        <!-- 取帧时间点只在选中项里可能有视频时出现：对一堆文档显示
             「视频取帧时间点」只会让人困惑。关掉缩略图后也不显示，
             那时它没有任何作用。 -->
        <div v-if="thumbnail && hasVideo" class="subfield">
          <label class="flabel" for="e-frame">{{ i18n.t('encrypt.thumbnail_frame') }}</label>
          <input
            id="e-frame"
            v-model="frameText"
            type="text"
            inputmode="text"
            placeholder="00:00:03"
          />
          <div v-if="frameInvalid" class="ferr">{{ i18n.t('encrypt.thumbnail_frame_bad') }}</div>
          <div v-else class="fhint">{{ i18n.t('encrypt.thumbnail_frame_hint') }}</div>
        </div>
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
