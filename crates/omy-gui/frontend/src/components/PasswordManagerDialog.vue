<script setup lang="ts">
/** 第三方密码管理器选择器。
 *
 * 密码不会作为 prop 传进来；每一行的 id 只是 Rust 候选缓存的句柄。用户点选
 * 后仍由后端完成解锁或加密，WebView 从始至终看不到真正的同步密钥。
 */

import { ref } from 'vue';
import * as i18n from '../i18n';
import type { PasswordManagerCredential, PasswordManagerStatus } from '../types';

defineProps<{
  purpose: 'unlock' | 'encrypt';
  status: PasswordManagerStatus | null;
  entries: PasswordManagerCredential[];
  busy?: boolean;
  error?: string;
}>();

const emit = defineEmits<{
  cancel: [];
  connect: [];
  forget: [];
  refresh: [];
  select: [entry: PasswordManagerCredential];
  generate: [label: string];
}>();

const label = ref('');

function generate() {
  const value = label.value.trim();
  if (value) emit('generate', value);
}
</script>

<template>
  <div class="overlay dlg-overlay pm-overlay" @click.self="$emit('cancel')">
    <div class="dlg narrow pm-dialog">
      <h3>{{ i18n.t('password_manager.title') }}</h3>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <template v-if="!status">
        <div class="hint">{{ i18n.t('password_manager.checking') }}</div>
        <button type="button" class="btn wide" :disabled="busy" @click="$emit('refresh')">
          {{ i18n.t('actions.retry') }}
        </button>
      </template>

      <template v-else-if="!status.installed">
        <div class="hint">
          {{
            status.provider === 'android_credential_manager'
              ? i18n.t('password_manager.requires_android_14')
              : i18n.t('password_manager.not_installed')
          }}
        </div>
      </template>

      <template v-else-if="!status.running || !status.database_open">
        <div class="hint">
          {{
            status.running
              ? i18n.t('password_manager.database_locked')
              : i18n.t('password_manager.not_running')
          }}
        </div>
        <button type="button" class="btn wide" :disabled="busy" @click="$emit('refresh')">
          {{ i18n.t('actions.retry') }}
        </button>
      </template>

      <template v-else-if="!status.associated">
        <div class="hint">{{ i18n.t('password_manager.connect_hint') }}</div>
        <button
          type="button"
          class="btn primary wide"
          data-pm="connect"
          :disabled="busy"
          @click="$emit('connect')"
        >
          {{ i18n.t('password_manager.connect') }}
        </button>
      </template>

      <template v-else>
        <div class="hint">
          {{
            status.provider === 'android_credential_manager'
              ? i18n.t('password_manager.android_choose_hint')
              : i18n.t('password_manager.choose_hint')
          }}
        </div>
        <div v-if="entries.length" class="pm-list">
          <button
            v-for="entry in entries"
            :key="entry.id"
            type="button"
            class="pm-entry"
            data-pm="entry"
            :disabled="busy"
            @click="$emit('select', entry)"
          >
            <span class="pm-name">{{ entry.login || entry.name }}</span>
            <span class="pm-meta">{{ entry.group || entry.name }}</span>
          </button>
        </div>
        <div v-else class="hint">{{ i18n.t('password_manager.none') }}</div>

        <button type="button" class="btn wide" :disabled="busy" @click="$emit('refresh')">
          {{ i18n.t('password_manager.refresh') }}
        </button>
        <button
          v-if="status.provider === 'keepassxc'"
          type="button"
          class="btn small"
          :disabled="busy"
          @click="$emit('forget')"
        >
          {{ i18n.t('password_manager.forget') }}
        </button>

        <form v-if="purpose === 'encrypt'" class="pm-create" @submit.prevent="generate">
          <label class="flabel" for="pm-label">{{ i18n.t('password_manager.new_label') }}</label>
          <input
            id="pm-label"
            v-model="label"
            type="text"
            autocomplete="off"
            :placeholder="i18n.t('password_manager.new_label_placeholder')"
          />
          <button type="submit" class="btn wide" :disabled="busy || !label.trim()">
            {{ i18n.t('password_manager.generate') }}
          </button>
        </form>
      </template>

      <div class="acts">
        <button type="button" class="btn" @click="$emit('cancel')">
          {{ i18n.t('actions.cancel') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.pm-overlay {
  z-index: 80;
}

.pm-dialog {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.pm-list {
  display: flex;
  max-height: 260px;
  flex-direction: column;
  gap: 8px;
  overflow: auto;
}

.pm-entry {
  display: flex;
  width: 100%;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  border: 1px solid var(--line);
  border-radius: 8px;
  padding: 10px 12px;
  background: var(--panel);
  color: inherit;
  cursor: pointer;
  text-align: left;
}

.pm-entry:hover {
  border-color: var(--accent);
}

.pm-name {
  font-weight: 600;
}

.pm-meta {
  color: var(--muted);
  font-size: 12px;
}

.pm-create {
  display: flex;
  flex-direction: column;
  gap: 8px;
  border-top: 1px solid var(--line);
  padding-top: 12px;
}
</style>
