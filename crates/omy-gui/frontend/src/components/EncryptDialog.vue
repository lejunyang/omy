<script setup lang="ts">
/** 加密对话框。
 *
 * 选项取自 `docs/research/appendix/ui-prototype.html` 的加密对话框，
 * 以及决策 D-14（分块大小可自定义）、D-19（不提供安全擦除）。
 */

import { ref, computed, onMounted } from 'vue';
import * as i18n from '../i18n';
import * as api from '../api';
import type { PasswordManagerCredential } from '../types';
import VideoDialog from './VideoDialog.vue';

const props = defineProps({
  /** 待加密的条目。 */
  targets: { type: Array as () => any[], required: true },
  busy: { type: Boolean, default: false },
  passwordManager: { type: Boolean, default: false },
  managerCredential: { type: Object as () => PasswordManagerCredential | null, default: null },
});

const emit = defineEmits(['cancel', 'submit', 'password-manager', 'clear-password-manager']);

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
/**
 * 槽位模式。默认 deniable——多一分保护的那个应当是默认值，
 * 想要便利的人会自己去选。
 */
const slotMode = ref('deniable');

/** 是否生成缩略图。
 *
 * 默认开：缩略图存在加密 TLV 里，只有拿到密码才能读，所以它不泄露内容，
 * 而没有缩略图的网格视图基本没法用。留出开关是因为它确实让文件大几 KB。
 */
const thumbnail = ref(true);

/** 视频取帧时间点（秒）。`null` 表示自动取时长 10% 处。
 *
 * 从「用户敲的字符串」改成了「秒数」：时间点现在由视频处理对话框选，
 * 那里能看着画面挑，解析与校验都在它内部完成，这里只收结果。
 */
const frameSeconds = ref(null);

/** 视频处理产出的中间文件路径。非空时加密它而不是原文件。 */
const convertedPath = ref(null);
/** 视频处理对话框是否打开。 */
const videoOpen = ref(false);

/** 视频扩展名。
 *
 * 只用来决定「要不要显示视频处理入口」，判错了不影响加密结果——真正的
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

/** 可以打开视频处理的那个条目。
 *
 * 只在**恰好选中一个视频文件**时给出。多选时不给：处理是逐文件的设置
 * （不同视频的封面帧、目标容器都不一样），一个对话框代表不了一批文件，
 * 硬套只会让用户以为设置应用到了全部。
 */
const videoTarget = computed(() => {
  if (props.targets.length !== 1) return null;
  const t = props.targets[0];
  if (!t || t.is_dir || !t.token) return null;
  const i = (t.name || '').lastIndexOf('.');
  if (i < 0) return null;
  return VIDEO_EXT.includes(t.name.slice(i + 1).toLowerCase()) ? t : null;
});

/** 秒 → `00:00:12.40`，用于按钮旁的状态文字。 */
function fmtTime(sec) {
  const s = Math.max(0, Number(sec) || 0);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}:${r.toFixed(2).padStart(5, '0')}`;
}

/** 视频处理对话框的结果。 */
function onVideoApply(r) {
  frameSeconds.value = r.frame;
  convertedPath.value = r.convertedPath;
  videoOpen.value = false;
}

/** 可选的分块大小。
 *
 * 下限 64 KiB：再小的话每块的 nonce 与 tag 开销占比过高。
 * 上限 16 MiB：更大会让 seek 精度变差，且解密时内存峰值明显。
 */
const CHUNKS = [65536, 131072, 262144, 524288, 1048576, 2097152, 4194304, 8388608, 16777216];

const chunkSize = computed(() => CHUNKS[chunkIndex.value] ?? 262144);
const chunkLabel = computed(() => i18n.formatSize(chunkSize.value));

/** 把配置里的分块代码（`64K`/`256K`/`1M`/`4M`）换算成滑块索引，认不出返回 -1。 */
function chunkCodeToIndex(code) {
  if (typeof code !== 'string' || !code) return -1;
  const m = code.trim().match(/^(\d+)\s*([KM])?$/i);
  if (!m) return -1;
  const unit = (m[2] || '').toUpperCase();
  const mult = unit === 'M' ? 1048576 : unit === 'K' ? 1024 : 1;
  return CHUNKS.indexOf(Number(m[1]) * mult);
}

/**
 * 打开时用「设置 → 加密默认值」初始化各选项。
 *
 * 没有这步，设置页里改的默认值只在 CLI 生效，GUI 对话框永远是硬编码那一套，
 * 用户会以为设置没保存。只认对话框确实提供的取值（强度没有 mobile 档），
 * 配置里出现认不出的值就保留对话框自带默认，避免单选组一个都选不中。
 * 读配置失败也静默沿用内置默认，不让设置缺失挡住加密。
 */
onMounted(async () => {
  let cfg;
  try {
    cfg = await api.configGet();
  } catch {
    return;
  }
  const d = cfg?.defaults || {};
  if (['interactive', 'moderate', 'sensitive'].includes(d.kdf_profile)) {
    strength.value = d.kdf_profile;
  }
  const ci = chunkCodeToIndex(d.chunk_size);
  if (ci >= 0) chunkIndex.value = ci;
  if (typeof cfg?.compress?.enabled === 'boolean') compress.value = cfg.compress.enabled;
  if (['keep', 'trash', 'delete'].includes(d.original_action)) {
    original.value = d.original_action;
  }
  // 配置序列化为 keep-ext（连字符），对话框 radio 值用 keep_ext（下划线）
  if (d.name_mode === 'encrypt' || d.name_mode === 'plain') filenameMode.value = d.name_mode;
  else if (d.name_mode === 'keep-ext') filenameMode.value = 'keep_ext';
});

const hasFolder = computed(() => props.targets.some((t) => t.is_dir));

const mismatch = computed(
  () => !props.managerCredential && password2.value.length > 0 && password.value !== password2.value,
);

const canSubmit = computed(
  () =>
    (!!props.managerCredential || (password.value.length > 0 && !mismatch.value)) &&
    !props.busy,
);

const totalSize = computed(() =>
  props.targets.reduce((s, t) => s + (t.size || 0), 0),
);

function submit() {
  if (!canSubmit.value) return;
  emit('submit', {
    password: props.managerCredential ? '' : password.value,
    password_manager_credential_id: props.managerCredential?.id ?? null,
    encrypt_filename: filenameMode.value !== 'plain',
    preserve_extension: filenameMode.value === 'keep_ext',
    compress: compress.value,
    chunk_size: chunkSize.value,
    kdf_profile: strength.value,
    original: original.value,
    folder_mode: folderMode.value,
    slot_mode: slotMode.value,
    thumbnail: thumbnail.value ? 'auto' : 'none',
    thumbnail_frame: thumbnail.value ? frameSeconds.value : null,
    /** 视频处理后的中间文件。非空时加密它，加密完由调用方清理。 */
    converted_path: convertedPath.value,
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

      <div v-if="passwordManager" class="field">
        <div class="flabel">{{ i18n.t('password_manager.key_source') }}</div>
        <button type="button" class="btn wide" data-sf="pm_encrypt" @click="$emit('password-manager')">
          {{
            managerCredential
              ? i18n.t('password_manager.selected', { name: managerCredential.login || managerCredential.name })
              : i18n.t('password_manager.choose_or_create')
          }}
        </button>
        <button
          v-if="managerCredential"
          type="button"
          class="btn small"
          @click="$emit('clear-password-manager')"
        >
          {{ i18n.t('password_manager.use_manual') }}
        </button>
        <div class="fhint">{{ i18n.t('password_manager.sync_hint') }}</div>
      </div>

      <div v-if="!managerCredential" class="field">
        <label class="flabel" for="e-pass">{{ i18n.t('encrypt.password') }}</label>
        <input id="e-pass" v-model="password" type="password" autocomplete="new-password" />
      </div>

      <div v-if="!managerCredential" class="field">
        <label class="flabel" for="e-pass2">{{ i18n.t('encrypt.password_again') }}</label>
        <input id="e-pass2" v-model="password2" type="password" autocomplete="new-password" />
        <div v-if="mismatch" class="ferr">{{ i18n.t('encrypt.password_mismatch') }}</div>
      </div>

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.slot_mode') }}</div>
        <label class="radio">
          <input v-model="slotMode" type="radio" value="deniable" />
          <span>
            {{ i18n.t('encrypt.slot_mode_deniable') }}
            <div class="d">{{ i18n.t('encrypt.slot_mode_deniable_desc') }}</div>
          </span>
        </label>
        <label class="radio">
          <input v-model="slotMode" type="radio" value="managed" />
          <span>
            {{ i18n.t('encrypt.slot_mode_managed') }}
            <div class="d">{{ i18n.t('encrypt.slot_mode_managed_desc') }}</div>
          </span>
        </label>
        <!-- 这个选择加密后改不了：模式写在文件头里，换模式等于重新加密。
             不说清楚的话，用户会以为和其它选项一样能随时回来改 -->
        <div class="fhint">{{ i18n.t('encrypt.slot_mode_permanent') }}</div>
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
          <input v-model="compress" type="checkbox" data-sf="enc_compress" />
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
        <!-- 视频处理入口只在恰好选中一个视频文件时出现。
             多选或文件夹时不给：封面帧与目标容器是逐文件的设置，
             一个对话框代表不了一批文件，硬套会让用户以为设置应用到了全部。
             关掉缩略图后仍然显示——转换格式与缩略图无关。 -->
        <div v-if="videoTarget" class="subfield">
          <button class="btn" type="button" @click="videoOpen = true">
            {{ i18n.t('encrypt.video_process') }}
          </button>
          <div class="fhint">
            {{
              convertedPath
                ? i18n.t('encrypt.video_converted', { name: i18n.t('video.tab_proc') })
                : i18n.t('encrypt.video_process_desc')
            }}
          </div>
          <div class="fhint">
            {{
              frameSeconds == null
                ? i18n.t('encrypt.video_frame_auto')
                : i18n.t('encrypt.video_frame_set', { time: fmtTime(frameSeconds) })
            }}
          </div>
        </div>
        <!-- 选了多个、或选的是文件夹时，只说明默认行为，不给入口 -->
        <div v-else-if="thumbnail && hasVideo" class="fhint subfield">
          {{ i18n.t('encrypt.thumbnail_frame_hint') }}
        </div>
      </div>

      <div class="field">
        <div class="flabel">{{ i18n.t('encrypt.chunk_size') }} · {{ chunkLabel }}</div>
        <input
          v-model.number="chunkIndex"
          class="slider"
          data-sf="enc_chunk"
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

    <!-- 视频处理叠在加密对话框之上。放在 form 外面：嵌套在 form 里时，
         它内部任何 button 没写 type="button" 都会触发表单提交 -->
    <VideoDialog
      v-if="videoOpen && videoTarget"
      :entry="videoTarget"
      :frame="frameSeconds"
      @cancel="videoOpen = false"
      @apply="onVideoApply"
    />
  </div>
</template>
