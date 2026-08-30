<script setup>
/** 设备面板：设备库、已配对设备、局域网发现、配对、共享。
 *
 * # 为什么设备库要单独输一次密码
 *
 * 设备库密码与**文件密码是两回事**，这一点必须在界面上说清楚：
 *
 * - 文件密码：解开加密文件的内容
 * - 设备库密码：保护本机身份（静态私钥）与已配对设备列表
 *
 * 用户很容易以为「开共享 = 把文件密码交出去」。实际上共享方
 * 全程不需要文件密码——服务端只搬运密文，解密在访问端完成。
 * 面板上有一条常驻说明讲这件事。
 *
 * # 配对为什么要轮询
 *
 * 配对天然要等人：这台显示配对码，那台去输入。中间可能过一分钟。
 * 所以后端把它做成后台任务，这里轮询进展——用户能看到「正在等待」
 * 还是「正在握手」，也随时可以取消。
 */

import { ref, computed, onMounted, onBeforeUnmount } from 'vue';
import * as api from '../api.js';
import * as i18n from '../i18n.js';

const emit = defineEmits(['close']);

/* 设备库 */
const status = ref({ exists: false, opened: false, paired_count: 0 });
const storePassword = ref('');
const busy = ref(false);
const error = ref('');

/* 列表 */
const paired = ref([]);
const found = ref([]);
const scanning = ref(false);

/* 配对 */
const pair = ref({ phase: 'idle' });
const pairAddr = ref('');
const pairPin = ref('');
const showConnect = ref(false);

/* 共享 */
const share = ref({ running: false });

let timer = null;

const opened = computed(() => status.value.opened);

/** 打开或创建设备库。 */
async function openStore() {
  if (!storePassword.value) return;
  busy.value = true;
  error.value = '';
  try {
    status.value = await api.openDeviceStore(storePassword.value);
    storePassword.value = '';
    await refresh();
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  } finally {
    busy.value = false;
  }
}

/** 刷新列表与状态。 */
async function refresh() {
  try {
    status.value = await api.deviceStatus();
    if (status.value.opened) {
      paired.value = await api.pairedDevices();
      share.value = await api.shareStatus();
    }
    pair.value = await api.pairStatus();
  } catch {
    /* 面板刷新失败不该打断用户，下一轮会重试 */
  }
}

/** 搜索局域网。 */
async function scan() {
  scanning.value = true;
  error.value = '';
  try {
    found.value = await api.discoverDevices(4);
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  } finally {
    scanning.value = false;
  }
}

/** 显示配对码，等对方连入。 */
async function startListen() {
  error.value = '';
  try {
    pair.value = await api.pairListen(0, 0);
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  }
}

/** 主动连接对方。 */
async function connectPair() {
  if (!pairAddr.value || !pairPin.value) return;
  busy.value = true;
  error.value = '';
  try {
    pair.value = await api.pairWith(pairAddr.value.trim(), pairPin.value.trim(), 0);
    if (pair.value.phase === 'done') {
      showConnect.value = false;
      pairPin.value = '';
      await refresh();
    }
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  } finally {
    busy.value = false;
  }
}

async function cancelPair() {
  await api.pairCancel().catch(() => {});
  pair.value = { phase: 'idle' };
}

/** 吊销一台设备。 */
async function revoke(fp) {
  error.value = '';
  try {
    paired.value = await api.revokeDevice(fp);
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  }
}

/** 开始共享一个目录。 */
async function pickAndShare() {
  const dir = await api.pickFolder(i18n.t('device.pick_share_dir')).catch(() => null);
  if (!dir) return;
  busy.value = true;
  error.value = '';
  try {
    share.value = await api.startShare(dir, 0, true);
  } catch (e) {
    error.value = i18n.te(api.errCode(e));
  } finally {
    busy.value = false;
  }
}

async function stopSharing() {
  share.value = await api.stopShare().catch(() => share.value);
}

/** 点一台发现到的设备：已配对就填地址去连，未配对则提示先配对。 */
function useDevice(d) {
  if (d.addr) {
    pairAddr.value = d.addr;
    showConnect.value = true;
  }
}

onMounted(async () => {
  await refresh();
  // 配对进行中要看到进展，所以轮询；其余时候也刷一下共享状态
  timer = setInterval(refresh, 1500);
});
onBeforeUnmount(() => clearInterval(timer));
</script>

<template>
  <div class="overlay dlg-overlay" @click.self="$emit('close')">
    <div class="dlg wide">
      <h3>
        📡 {{ i18n.t('device.title') }}
        <span class="spacer"></span>
        <button class="iconbtn" :aria-label="i18n.t('actions.close')" @click="$emit('close')">
          ✕
        </button>
      </h3>

      <div v-if="error" class="errbox" role="alert">{{ error }}</div>

      <!-- 设备库未打开：先问密码。这里要讲清楚它和文件密码的区别 -->
      <template v-if="!opened">
        <div class="note if">{{ i18n.t('device.store_explain') }}</div>
        <form @submit.prevent="openStore">
          <div class="field">
            <label class="flabel" for="sp">
              {{ status.exists ? i18n.t('device.store_password') : i18n.t('device.store_new_password') }}
            </label>
            <input id="sp" v-model="storePassword" type="password" autocomplete="current-password" />
          </div>
          <div class="acts">
            <button type="submit" class="btn primary" :disabled="!storePassword || busy">
              {{ busy ? i18n.t('device.opening') : (status.exists ? i18n.t('device.open_store') : i18n.t('device.create_store')) }}
            </button>
          </div>
        </form>
      </template>

      <template v-else>
        <!-- 本机身份。指纹要显眼：配对时两端要人工核对 -->
        <div class="selfbox">
          <div class="row">
            <span class="lbl">{{ i18n.t('device.this_device') }}</span>
            <strong>{{ status.device_name }}</strong>
          </div>
          <div class="row">
            <span class="lbl">{{ i18n.t('device.fingerprint') }}</span>
            <code class="fp">{{ status.fingerprint }}</code>
          </div>
          <div class="hint">{{ i18n.t('device.fingerprint_hint') }}</div>
        </div>

        <!-- 配对进行中 -->
        <div v-if="pair.phase === 'waiting'" class="pairbox">
          <div class="pin">{{ pair.pin }}</div>
          <div class="hint">{{ i18n.t('device.pin_hint', { port: pair.port }) }}</div>
          <button class="btn small" @click="cancelPair">{{ i18n.t('actions.cancel') }}</button>
        </div>
        <div v-else-if="pair.phase === 'handshaking'" class="pairbox">
          <div class="hint">{{ i18n.t('device.handshaking') }}</div>
        </div>
        <div v-else-if="pair.phase === 'done'" class="pairbox ok">
          <div>{{ i18n.t('device.paired_with', { name: pair.name }) }}</div>
          <div class="cmp">
            <span>{{ i18n.t('device.their_fp') }} <code>{{ pair.fingerprint }}</code></span>
            <span>{{ i18n.t('device.your_fp') }} <code>{{ pair.own_fingerprint }}</code></span>
          </div>
          <div class="hint">{{ i18n.t('device.verify_hint') }}</div>
          <button class="btn small" @click="cancelPair">{{ i18n.t('actions.ok') }}</button>
        </div>
        <div v-else-if="pair.phase === 'failed'" class="pairbox err">
          <div>{{ i18n.te(pair.code) }}</div>
          <button class="btn small" @click="cancelPair">{{ i18n.t('actions.ok') }}</button>
        </div>

        <div class="hr"></div>

        <!-- 已配对设备 -->
        <div class="sect">
          <div class="sect-h">
            <span>{{ i18n.t('device.paired_list') }}</span>
            <button class="btn small" @click="startListen">➕ {{ i18n.t('device.pair_new') }}</button>
          </div>
          <div v-if="!paired.length" class="muted">{{ i18n.t('device.no_paired') }}</div>
          <div v-for="d in paired" :key="d.fingerprint" class="drow">
            <span class="ic">{{ d.expired ? '⌛' : '💻' }}</span>
            <span class="nm">
              {{ d.name }}
              <code class="fp small">{{ d.fingerprint }}</code>
            </span>
            <span v-if="d.expired" class="tag warn">{{ i18n.t('device.expired') }}</span>
            <button class="btn small" @click="revoke(d.fingerprint)">
              {{ i18n.t('device.revoke') }}
            </button>
          </div>
        </div>

        <div class="hr"></div>

        <!-- 局域网发现 -->
        <div class="sect">
          <div class="sect-h">
            <span>{{ i18n.t('device.nearby') }}</span>
            <button class="btn small" :disabled="scanning" @click="scan">
              {{ scanning ? i18n.t('device.scanning') : '🔍 ' + i18n.t('device.scan') }}
            </button>
          </div>
          <div v-if="!found.length && !scanning" class="muted">{{ i18n.t('device.none_found') }}</div>
          <div
            v-for="d in found"
            :key="d.fingerprint"
            class="drow clickable"
            @click="useDevice(d)"
          >
            <span class="ic">{{ d.paired ? '💻' : '❓' }}</span>
            <span class="nm">
              {{ d.name }}
              <code class="fp small">{{ d.addr || d.fingerprint }}</code>
            </span>
            <span v-if="!d.compatible" class="tag warn">{{ i18n.t('device.incompatible') }}</span>
            <span v-else-if="d.expired" class="tag warn">{{ i18n.t('device.expired') }}</span>
            <span v-else-if="d.paired" class="tag ok">{{ i18n.t('device.is_paired') }}</span>
            <span v-else class="tag">{{ i18n.t('device.not_paired') }}</span>
          </div>
          <div class="hint">{{ i18n.t('device.name_untrusted') }}</div>
        </div>

        <!-- 主动连接配对 -->
        <div v-if="showConnect" class="sect">
          <div class="hr"></div>
          <form @submit.prevent="connectPair">
            <div class="field">
              <label class="flabel" for="pa">{{ i18n.t('device.peer_addr') }}</label>
              <input id="pa" v-model="pairAddr" type="text" placeholder="192.168.1.5:5000" />
            </div>
            <div class="field">
              <label class="flabel" for="pp">{{ i18n.t('device.enter_pin') }}</label>
              <input id="pp" v-model="pairPin" type="text" inputmode="numeric" maxlength="6" />
            </div>
            <div class="acts">
              <button type="button" class="btn" @click="showConnect = false">
                {{ i18n.t('actions.cancel') }}
              </button>
              <button type="submit" class="btn primary" :disabled="busy || !pairAddr || !pairPin">
                {{ i18n.t('device.do_pair') }}
              </button>
            </div>
          </form>
        </div>

        <div class="hr"></div>

        <!-- 共享 -->
        <div class="sect">
          <div class="sect-h">
            <span>{{ i18n.t('device.sharing') }}</span>
          </div>
          <div class="note if">{{ i18n.t('device.share_no_password') }}</div>
          <template v-if="share.running">
            <div class="drow">
              <span class="ic">📡</span>
              <span class="nm">
                {{ share.dir }}
                <code class="fp small">
                  {{ i18n.tn('device.share_files', share.files) }} · {{ share.addr }}
                </code>
              </span>
              <button class="btn small" @click="stopSharing">{{ i18n.t('device.stop_share') }}</button>
            </div>
            <div v-if="!paired.length" class="note">{{ i18n.t('device.share_no_peer') }}</div>
          </template>
          <button v-else class="btn" :disabled="busy" @click="pickAndShare">
            📂 {{ i18n.t('device.pick_share_dir') }}
          </button>
        </div>
      </template>
    </div>
  </div>
</template>
