<script setup>
/** 添加一个远程位置（WebDAV）。
 *
 * # 为什么默认不勾「允许写入」
 *
 * 只读挂载是防误操作的第一道闸。用户添加一个 NAS 多半是为了看里面的
 * 加密文件，不是为了往里写；而一旦挂成可写，右键菜单里的删除、重命名
 * 就全亮了，误点的后果是远端文件真的没了。
 *
 * 想写的人会主动勾上，不想写的人不会因为没勾而损失什么——这个方向的
 * 不对称决定了默认值该是哪个。
 *
 * # 凭据填完就走后端
 *
 * 提交后前端不保留密码。它在 WebView 里没有用途，留着只是多一处泄露面。
 */

import { ref, computed, onMounted, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';
import * as api from '../api.js';

const emit = defineEmits(['added', 'cancel']);

const name = ref('');
const url = ref('');
const username = ref('');
const password = ref('');
const vendor = ref('generic');
const writable = ref(false);
const busy = ref(false);
const error = ref('');

const nameInput = useTemplateRef('nameInput');
onMounted(() => nameInput.value?.focus());

/** URL 必须是 http(s)。
 *
 * 前端也校验一遍不是为了防非法输入（后端会拦），而是让用户在按下按钮
 * 之前就知道这个地址不行——等提交后再弹错误，他得先看懂错误码。
 */
const urlProblem = computed(() => {
  const v = url.value.trim();
  if (!v) return 'empty';
  if (!/^https?:\/\//i.test(v)) return 'scheme';
  return '';
});

const canSubmit = computed(
  () => !busy.value && name.value.trim() && !urlProblem.value,
);

async function submit() {
  if (!canSubmit.value) return;
  busy.value = true;
  error.value = '';
  try {
    const id = await api.remotePlaceAdd({
      name: name.value.trim(),
      url: url.value.trim(),
      username: username.value,
      password: password.value,
      vendor: vendor.value,
      writable: writable.value,
    });
    // 提交成功后立刻清掉密码：它已经交给后端了，前端再留着没有用途
    password.value = '';
    emit('added', id);
  } catch (e) {
    error.value = i18n.te(api.errCode(e), 'rplace.add_failed');
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <div class="mask" @click.self="$emit('cancel')">
    <div class="dlg" data-rp="add">
      <div class="dh">{{ i18n.t('rplace.add') }}</div>

      <label class="f">
        <span class="fl">{{ i18n.t('rplace.name') }}</span>
        <input ref="nameInput" v-model="name" data-rf="name" type="text" @keydown.enter="submit" />
      </label>

      <label class="f">
        <span class="fl">{{ i18n.t('rplace.url') }}</span>
        <input
          v-model="url"
          data-rf="url"
          type="text"
          placeholder="https://example.com/remote.php/dav/files/me/"
          @keydown.enter="submit"
        />
        <span v-if="urlProblem === 'scheme'" class="warn">{{ i18n.t('rplace.url_scheme') }}</span>
      </label>

      <label class="f">
        <span class="fl">{{ i18n.t('rplace.username') }}</span>
        <input v-model="username" data-rf="username" type="text" autocomplete="off" />
      </label>

      <label class="f">
        <span class="fl">{{ i18n.t('rplace.password') }}</span>
        <input v-model="password" data-rf="password" type="password" autocomplete="off" @keydown.enter="submit" />
      </label>

      <label class="f">
        <span class="fl">{{ i18n.t('rplace.vendor') }}</span>
        <select v-model="vendor" data-rf="vendor">
          <option value="generic">{{ i18n.t('rplace.vendor_generic') }}</option>
          <option value="nextcloud">{{ i18n.t('rplace.vendor_nextcloud') }}</option>
        </select>
      </label>

      <label class="chk">
        <input v-model="writable" data-rf="writable" type="checkbox" />
        <span>
          {{ i18n.t('rplace.writable') }}
          <span class="d">{{ i18n.t('rplace.writable_desc') }}</span>
        </span>
      </label>

      <div v-if="error" class="err">{{ error }}</div>

      <div class="act">
        <button class="btn" data-rf="cancel" @click="$emit('cancel')">{{ i18n.t('common.cancel') }}</button>
        <button class="btn pri" data-rf="submit" :disabled="!canSubmit" @click="submit">
          {{ busy ? i18n.t('rplace.connecting') : i18n.t('common.save') }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 60;
  padding: 16px;
}
.dlg {
  width: min(440px, 100%);
  max-height: 90vh;
  overflow: auto;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-l);
  padding: 18px;
  /* 底部安全区：移动端手势导航条会盖住按钮 */
  padding-bottom: calc(18px + env(safe-area-inset-bottom, 0px));
}
.dh {
  font-size: 15px;
  font-weight: 600;
  margin-bottom: 14px;
}
.f {
  display: block;
  margin-bottom: 11px;
}
.fl {
  display: block;
  font-size: 12.5px;
  color: var(--fg2);
  margin-bottom: 5px;
}
input[type='text'],
input[type='password'],
select {
  width: 100%;
  background: var(--bg3);
  color: var(--fg);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 7px 9px;
  font-size: 13px;
  /* 触控目标不小于 44px 的一半高度，配合 padding 达到可点面积 */
  min-height: 36px;
}
.warn {
  display: block;
  color: var(--warn);
  font-size: 11.5px;
  margin-top: 4px;
}
.chk {
  display: flex;
  gap: 9px;
  align-items: flex-start;
  font-size: 13px;
  cursor: pointer;
  margin-top: 6px;
  min-height: 44px;
}
.chk input {
  margin-top: 3px;
  flex: none;
}
/* 说明文字与上方标签左对齐，不再缩进——缩进后会看起来像属于别的项 */
.d {
  display: block;
  font-size: 11.5px;
  color: var(--fg2);
  margin-top: 3px;
  line-height: 1.45;
}
.err {
  color: var(--danger);
  font-size: 12.5px;
  margin-top: 10px;
}
.act {
  display: flex;
  justify-content: flex-end;
  gap: 9px;
  margin-top: 16px;
}
.btn {
  background: var(--bg3);
  color: var(--fg);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 7px 14px;
  cursor: pointer;
  font-size: 13px;
  min-height: 36px;
}
.btn.pri {
  background: var(--accent);
  border-color: var(--accent);
  color: #fff;
}
.btn:disabled {
  opacity: 0.5;
  cursor: default;
}

/* 窄屏把按钮抬到触控下限。写在组件自己的 scoped 样式里：
   scoped 会附加 [data-v-*]，特异性高于 app.css 的全局 `.btn`，
   全局那条压不住——表现是规则写了但按钮仍是 36px。 */
@media (max-width: 768px) {
  .btn {
    min-height: 44px;
  }
}
</style>
