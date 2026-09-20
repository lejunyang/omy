<script setup>
/** Telegram 扫码登录。
 *
 * 界面口径以 `docs/research/appendix/telegram-remote-prototype.html` 第 2 节为准。
 *
 * # 为什么扫码是主推、而且这里只做扫码
 *
 * 手机号路径有一个绕不过去的可用性问题：账号只要还有其他活跃 session，
 * Telegram 就优先把验证码发到**应用内**而不是短信，而强制走短信的参数官方已经
 * 移除。也就是说「已经在用 Telegram 的人」默认都收不到短信，得去另一个客户端
 * 取码——而扫码本来就在那个客户端上操作，不存在「码发到哪去了」这个问题。
 *
 * # 三条必须守住的交互规则
 *
 * 1. **过期换图要看得见。** 后端自动重新导出令牌换一张图，但静默替换会让盯着
 *    屏幕的人以为卡住了、或者怀疑刚才扫的那张还算不算数。所以换图时闪一下边框
 *    并显示「已自动换一张（第 N 次）」。
 * 2. **切数据中心是中间态，不是失败。** 显示进度并继续等，绝不弹「登录失败，
 *    请重试」——那会让一次本来会成功的登录被用户自己打断。
 * 3. **云密码是独立子步，不是固定的第 N 步。** 没开两步验证的账号永远走不到
 *    这一步，画成「第 3 步 / 共 4 步」等于让他一直看着一个到不了的步骤。
 */

import { ref, computed, onMounted, onBeforeUnmount, useTemplateRef } from 'vue';
import * as i18n from '../i18n.js';
import * as api from '../api.js';

const emit = defineEmits(['done', 'cancel']);

/** 当前阶段，取值与后端 `LoginPhase` 的 `phase` 一一对应。 */
const phase = ref('idle');
/** 二维码模块矩阵 `{size, modules}`。 */
const matrix = ref(null);
/** 倒计时剩余秒数。 */
const secsLeft = ref(0);
/** 已经换过几张图。0 表示还没换过。 */
const refreshes = ref(0);
/** 换图动画的触发开关。 */
const flash = ref(false);
/** 切换到的目标数据中心。 */
const dc = ref(0);
/** 云密码提示；为空时**整行不显示**，不要放一个空的「提示：」。 */
const hint = ref('');
/** 用户输入的云密码。 */
const password = ref('');
/** 失败的错误码与限流秒数。 */
const errCode = ref('');
const waitSecs = ref(0);
/** 失败的具体原因（服务端错误名）。空表示没有更多信息可说。 */
const errDetail = ref('');
/** 登录态有没有真的落盘。 */
const sessionSaved = ref(true);
/** 这台机器能不能保存登录态。扫码**之前**就要知道。 */
const canPersist = ref(true);
/** 代理地址。 */
const proxyUrl = ref('');
/** 已经点过开始了吗（决定显示前置说明还是登录过程）。 */
const started = ref(false);
/** 本机已经有可用的登录态吗。
 *
 * 有的话不该再摆一张二维码——用户的登录态就在盘上，让他重扫一次既多余又
 * 有风险（频繁扫码可能被 Telegram 判为异常）。
 *
 * 判的是「auth key 在不在」而不是「文件在不在」：一份没有 auth key 的存档
 * 看着一切正常，用起来却仍是未登录。
 */
const hasSession = ref(false);
/** 正在查有没有登录态。查之前不要先渲染扫码界面，否则会闪一下。 */
const checking = ref(true);

const pwInput = useTemplateRef('pwInput');

let unlisten = null;
let ticker = null;

/** 只有这些阶段需要倒计时。 */
const counting = computed(() => phase.value === 'qr');

/** 密码错了要停在本步重输，所以「正在要密码」与「刚报了密码错」都算同一步。 */
const askingPassword = computed(
  () => phase.value === 'need_password' || (phase.value === 'failed' && errCode.value === 'tg_wrong_password'),
);

/** 这个失败还值不值得重来。
 *
 * 限流与 api_id 被封都**不给**重试：前者重试会把等待越点越长，后者重试永远
 * 无用（唯一出路是填自己的 api_id）。给一个必然失败的按钮等于骗用户。
 */
const canRetry = computed(
  () =>
    phase.value === 'failed' &&
    !['tg_flood_wait', 'tg_api_id_flood', 'tg_wrong_password'].includes(errCode.value),
);

/** 二维码格子的行内样式：边长由后端给的矩阵决定，不写死 21。
 *
 * 写死会在高版本二维码（内容变长时会升版本）下画错——而画错的图看起来
 * **像**二维码，只是扫不出来，那个现象完全指不到这里。
 */
const gridStyle = computed(() => {
  const n = matrix.value?.size || 21;
  return {
    gridTemplateColumns: `repeat(${n}, 1fr)`,
    gridTemplateRows: `repeat(${n}, 1fr)`,
  };
});

function resetTransient() {
  errCode.value = '';
  waitSecs.value = 0;
  errDetail.value = '';
}

/** 处理一条后端推来的进度。 */
function onPhase(p) {
  if (!p || typeof p.phase !== 'string') return;
  switch (p.phase) {
    case 'connecting':
      phase.value = 'connecting';
      resetTransient();
      break;
    case 'qr': {
      resetTransient();
      phase.value = 'qr';
      matrix.value = p.matrix;
      secsLeft.value = p.expires_in_secs;
      // refresh_index 为 0 表示首张，不能显示成「已换第 1 次」——
      // 用户还什么都没等
      const isRefresh = p.refresh_index > 0 && p.refresh_index !== refreshes.value;
      refreshes.value = p.refresh_index;
      if (isRefresh) {
        // 重置动画：不先关掉再开，连续换图时动画不会重放
        flash.value = false;
        requestAnimationFrame(() => {
          flash.value = true;
        });
      }
      break;
    }
    case 'migrating':
      resetTransient();
      phase.value = 'migrating';
      dc.value = p.dc;
      break;
    case 'need_password':
      resetTransient();
      phase.value = 'need_password';
      hint.value = p.hint || '';
      // 自动聚焦，省一次点击
      requestAnimationFrame(() => pwInput.value?.focus());
      break;
    case 'done':
      phase.value = 'done';
      sessionSaved.value = p.session_saved;
      // 代理要一并回传：登录走了代理而后续浏览不走，表现是「登录成功了
      // 但点进去什么都加载不出来」，而这两件事看起来毫无关联
      emit('done', { sessionSaved: p.session_saved, proxyUrl: proxyUrl.value });
      break;
    case 'failed':
      phase.value = 'failed';
      errCode.value = p.code;
      waitSecs.value = p.wait_secs || 0;
      errDetail.value = p.detail || '';
      // 密码错了停在本步：清空输入框但保留提示与焦点，不要把用户踢回扫码
      if (p.code === 'tg_wrong_password') {
        password.value = '';
        requestAnimationFrame(() => pwInput.value?.focus());
      }
      break;
    default:
      break;
  }
}

// ---- tdata 导入 ----
//
// 与扫码**并列**的一条路，不是它的子步骤：用户要么扫码要么复用桌面端，
// 画成子步骤会让人以为得先扫码再导入。
/** 当前在不在 tdata 面板。 */
const tdataMode = ref(false);
/** 桌面客户端在不在跑（跑着就占着 tdata）。 */
const tdRunning = ref(false);
/** 自动探测到的路径；为空是常态，便携版必然探不到。 */
const tdPath = ref('');
/** 自动探测有没有找到——决定提示语是「找到了」还是「请手动指定」。 */
const tdAuto = ref(false);
/** 这个路径像不像 tdata（即时反馈，避免填完密码才报错）。 */
const tdPathOk = ref(false);
/** 本地密码。**事先不可预知**要不要，所以默认不显示这一栏。 */
const tdPass = ref('');
/** 要不要向用户要本地密码——由后端的实际解析结果决定，不猜。 */
const tdNeedPass = ref(false);
const tdBusy = ref(false);

async function openTdata() {
  tdataMode.value = true;
  errCode.value = '';
  try {
    const p = await api.telegramTdataProbe();
    tdRunning.value = !!p.client_running;
    const first = (p.candidates || [])[0] || '';
    tdAuto.value = !!first;
    if (first) {
      tdPath.value = first;
      tdPathOk.value = true;
    }
  } catch {
    // 探测失败不该挡住用户：手动填路径照样能走
    tdRunning.value = false;
    tdAuto.value = false;
  }
}

/** 路径变了就即时校验，让「选错目录」当场可见。 */
async function checkTdPath() {
  const p = tdPath.value.trim();
  if (!p) { tdPathOk.value = false; return; }
  try {
    tdPathOk.value = await api.telegramTdataCheck(p);
  } catch {
    tdPathOk.value = false;
  }
}

async function importTdata() {
  const p = tdPath.value.trim();
  if (!p || tdBusy.value) return;
  tdBusy.value = true;
  errCode.value = '';
  // 密码用完立刻从本地清掉：它在 WebView 里已无用途，留着只是多一处泄露面
  const pw = tdPass.value || null;
  try {
    await api.telegramTdataImport(p, pw, proxyUrl.value.trim());
    tdPass.value = '';
    emit('done', { sessionSaved: true, proxyUrl: proxyUrl.value });
  } catch (e) {
    const code = api.errCode(e) || 'tg_tdata_failed';
    errCode.value = code;
    // 「要密码」不是失败，是一条正常分支——展开输入框而不是把它当错误收场
    if (code === 'tg_tdata_need_passcode') tdNeedPass.value = true;
    // 密码错了也要保持输入框可见，否则用户没地方重输
    if (code === 'tg_tdata_wrong_passcode') { tdNeedPass.value = true; tdPass.value = ''; }
  } finally {
    tdBusy.value = false;
  }
}

async function start() {
  started.value = true;
  phase.value = 'connecting';
  resetTransient();
  refreshes.value = 0;
  try {
    await api.telegramLoginStart(proxyUrl.value.trim());
  } catch (e) {
    phase.value = 'failed';
    errCode.value = api.errCode(e) || 'tg_login_failed';
  }
}

async function submitPassword() {
  if (!password.value) return;
  const pw = password.value;
  // 立刻清掉本地副本：它在 WebView 里已经没有用途，留着只是多一处泄露面
  password.value = '';
  phase.value = 'connecting';
  try {
    await api.telegramSubmitPassword(pw);
  } catch (e) {
    phase.value = 'failed';
    errCode.value = api.errCode(e) || 'tg_login_failed';
  }
}

/** 退出 Telegram 账号：清掉本机保存的登录态。 */
async function signOut() {
  try {
    // 这里退的是还没被位置接管的那份登录态（扫码先于位置存在）
    await api.telegramForgetSession(api.TG_PENDING_ACCOUNT);
    hasSession.value = false;
  } catch (e) {
    errCode.value = api.errCode(e) || 'tg_forget_failed';
    phase.value = 'failed';
  }
}

/** 已有登录态时，用户仍可以主动重新扫码（换账号）。 */
function scanAnyway() {
  hasSession.value = false;
}

async function cancel() {
  try {
    await api.telegramLoginCancel();
  } catch {
    // 取消失败不值得打断用户关窗口这个动作
  }
  emit('cancel');
}

onMounted(async () => {
  unlisten = await api.onTelegramLogin(onPhase);
  try {
    canPersist.value = await api.telegramCanPersist();
  } catch {
    // 问不到就按能存处理：这只影响一句提示，不影响能不能登录
  }
  try {
    hasSession.value = await api.telegramHasSession(api.TG_PENDING_ACCOUNT);
  } catch {
    // 问不到就当没有：多扫一次码总好过让用户以为已登录、然后处处失败
    hasSession.value = false;
  } finally {
    checking.value = false;
  }
  // 倒计时只做显示。真正何时换图由后端决定——两边各算一份必然漂移，
  // 而漂移的表现是「界面说还有 8 秒，图却已经换了」
  ticker = setInterval(() => {
    if (counting.value && secsLeft.value > 0) secsLeft.value -= 1;
  }, 1000);
});

onBeforeUnmount(() => {
  if (unlisten) unlisten();
  if (ticker) clearInterval(ticker);
  // 组件销毁必须取消后台登录：那条连接挂着会占一个连接配额，而 Telegram 对
  // 同一账号的并发连接数有限制，反复开关几次登录页就会开始收到 429
  api.telegramLoginCancel().catch(() => {});
});
</script>

<template>
  <div class="mask" @click.self="cancel">
    <div class="dlg" data-tg="login">
      <div class="dh">{{ i18n.t('tg.login_title') }}</div>

      <!-- 正在查有没有登录态。不先占位的话会先闪一下扫码界面 -->
      <div v-if="checking" class="prog" data-tg="checking">
        {{ i18n.t('tg.checking') }}
      </div>

      <!-- 已经登录过：不要再摆二维码。用户的登录态就在盘上，
           让他重扫既多余又有风险（频繁扫码可能被判为异常） -->
      <template v-else-if="hasSession && !started">
        <div class="okbox" data-tg="already">
          <div class="strong">{{ i18n.t('tg.already_signed_in') }}</div>
          <div class="d">{{ i18n.t('tg.already_desc') }}</div>
        </div>
        <div class="act">
          <button class="btn" data-tg="signout" @click="signOut">
            {{ i18n.t('tg.sign_out') }}
          </button>
          <button class="btn" data-tg="rescan" @click="scanAnyway">
            {{ i18n.t('tg.scan_again') }}
          </button>
          <button class="btn pri" data-tg="close" @click="emit('done', { sessionSaved: true, proxyUrl })">
            {{ i18n.t('common.close') }}
          </button>
        </div>
      </template>

      <!-- 开始之前：说明 + 代理 + 「存不存得住」的前置告知 -->
      <template v-else-if="!started">
        <p class="lead">{{ i18n.t('tg.qr_why') }}</p>

        <label class="f">
          <span class="fl">{{ i18n.t('tg.proxy') }}</span>
          <input
            v-model="proxyUrl"
            data-tg="proxy"
            type="text"
            placeholder="socks5://127.0.0.1:7897"
            spellcheck="false"
            @keydown.enter="start"
          />
          <span class="d">{{ i18n.t('tg.proxy_desc') }}</span>
        </label>

        <!-- 这条必须在扫码**之前**说。等他扫完再说「存不住」，他下次打开
             发现又要扫码会以为程序把他登出了 -->
        <div v-if="!canPersist" class="warnbox" data-tg="no-persist">
          {{ i18n.t('tg.no_persist') }}
        </div>

        <!-- tdata 面板。与扫码并列的一条路，不是子步骤 -->
        <template v-if="tdataMode">
          <div class="sgh" data-tg="td-title">{{ i18n.t('tg.tdata_title') }}</div>
          <p class="lead">{{ i18n.t('tg.tdata_desc') }}</p>

          <!-- 前提①：客户端在跑就占着 tdata。指名道姓说清「关窗口不够，
               要从托盘退出」，而不是抛一个文件锁错误让人去查磁盘权限 -->
          <div v-if="tdRunning" class="warnbox" data-tg="td-running">
            {{ i18n.t('tg.tdata_running') }}
          </div>

          <!-- 前提②⑤：找不到就**就地**给输入框。显示「未检测到」等于把
               「需要你补个信息」说成「不支持」，便携版用户会直接走掉 -->
          <label class="f">
            <span class="fl">{{ i18n.t('tg.tdata_path') }}</span>
            <input
              v-model="tdPath"
              data-tg="td-path"
              type="text"
              placeholder="D:\TelegramDesktop\tdata"
              spellcheck="false"
              @input="checkTdPath"
            />
            <span v-if="tdAuto && tdPathOk" class="d" data-tg="td-auto">
              {{ i18n.t('tg.tdata_autofound') }}
            </span>
            <span v-else-if="!tdPath" class="d" data-tg="td-manual">
              {{ i18n.t('tg.tdata_not_found') }}
            </span>
            <span v-else-if="!tdPathOk" class="d warn" data-tg="td-bad">
              {{ i18n.t('tg.tdata_bad_dir') }}
            </span>
            <span class="d">{{ i18n.t('tg.tdata_path_hint') }}</span>
          </label>

          <!-- 前提④：本地密码只在后端说需要时才出现。
               有没有设事先无法预知，所以不能画成固定的一步；
               文案必须与云密码区分，否则用户会把云密码填进来反复被拒 -->
          <label v-if="tdNeedPass" class="f" data-tg="td-passwrap">
            <span class="fl">{{ i18n.t('tg.tdata_passcode') }}</span>
            <input
              v-model="tdPass"
              data-tg="td-pass"
              type="password"
              autocomplete="off"
              @keydown.enter="importTdata"
            />
            <span class="d">{{ i18n.t('tg.tdata_passcode_hint') }}</span>
          </label>

          <div class="warnbox" data-tg="td-shared">{{ i18n.t('tg.tdata_shared') }}</div>

          <div v-if="errCode" class="errbox" data-tg="td-err">
            {{ i18n.te(errCode) }}
          </div>

          <div class="act">
            <button class="btn" data-tg="td-back" @click="tdataMode = false">
              {{ i18n.t('tg.tdata_use_qr') }}
            </button>
            <button
              class="btn pri"
              data-tg="td-import"
              :disabled="!tdPathOk || tdBusy"
              @click="importTdata"
            >
              {{ tdBusy ? i18n.t('tg.tdata_importing') : i18n.t('tg.tdata_import') }}
            </button>
          </div>
        </template>

        <div v-else class="act">
          <button class="btn" data-tg="cancel" @click="cancel">
            {{ i18n.t('common.cancel') }}
          </button>
          <button class="btn" data-tg="td-open" @click="openTdata">
            {{ i18n.t('tg.tdata_switch') }}
          </button>
          <button class="btn pri" data-tg="start" @click="start">
            {{ i18n.t('tg.start') }}
          </button>
        </div>
      </template>

      <template v-else>
        <!-- 连接中 -->
        <div v-if="phase === 'connecting'" class="prog" data-tg="connecting">
          {{ i18n.t('tg.connecting') }}
        </div>

        <!-- 二维码 -->
        <div v-if="phase === 'qr' && matrix" class="qrwrap" data-tg="qr">
          <div
            class="qr"
            :class="{ refreshed: flash }"
            :style="gridStyle"
            role="img"
            :aria-label="i18n.t('tg.qr_alt')"
            :data-refreshes="refreshes"
            @animationend="flash = false"
          >
            <i v-for="(on, i) in matrix.modules" :key="i" :class="{ b: on }"></i>
          </div>
          <div class="qrside">
            <b class="strong">{{ i18n.t('tg.qr_how') }}</b>
            <div class="ttl" data-tg="ttl">
              {{ i18n.t('tg.qr_ttl', { secs: secsLeft }) }}
            </div>
            <div class="d">{{ i18n.t('tg.qr_is_credential') }}</div>
            <!-- 换图要让用户看见。静默替换等于没告诉他发生过什么 -->
            <div v-if="refreshes > 0" class="refreshnote" data-tg="refreshed">
              {{ i18n.t('tg.qr_refreshed', { n: refreshes }) }}
            </div>
          </div>
        </div>

        <!-- 切数据中心：中间态，显示进度，**不给重试** -->
        <div v-if="phase === 'migrating'" class="prog" data-tg="migrating">
          {{ i18n.t('tg.migrating', { dc }) }}
          <span class="d">{{ i18n.t('tg.migrating_desc') }}</span>
        </div>

        <!-- 云密码：独立子步，只有开了 2FA 才出现 -->
        <div v-if="askingPassword" class="pwbox" data-tg="password">
          <div class="strong">{{ i18n.t('tg.need_password') }}</div>
          <div class="d">{{ i18n.t('tg.need_password_desc') }}</div>
          <input
            ref="pwInput"
            v-model="password"
            data-tg="pw-input"
            type="password"
            autocomplete="off"
            :placeholder="i18n.t('tg.cloud_password')"
            @keydown.enter="submitPassword"
          />
          <!-- 没有提示时整行不显示，不要放一个空的「提示：」 -->
          <div v-if="hint" class="d" data-tg="hint">
            {{ i18n.t('tg.password_hint', { hint }) }}
          </div>
          <div class="act">
            <button class="btn" data-tg="cancel" @click="cancel">
              {{ i18n.t('common.cancel') }}
            </button>
            <button
              class="btn pri"
              data-tg="pw-submit"
              :disabled="!password"
              @click="submitPassword"
            >
              {{ i18n.t('tg.submit_password') }}
            </button>
          </div>
        </div>

        <!-- 成功 -->
        <div v-if="phase === 'done'" class="okbox" data-tg="done">
          <div class="strong">{{ i18n.t('tg.logged_in') }}</div>
          <div class="d">
            {{ sessionSaved ? i18n.t('tg.session_saved') : i18n.t('tg.session_not_saved') }}
          </div>
        </div>

        <!-- 失败。密码错误不在这里显示——它归 askingPassword 那一支，
             因为那一步要停在原地重输而不是退回来 -->
        <div
          v-if="phase === 'failed' && errCode !== 'tg_wrong_password'"
          class="errbox"
          data-tg="failed"
        >
          <div>{{ i18n.te(errCode, i18n.t('tg.login_failed')) }}</div>
          <div v-if="waitSecs > 0" class="d" data-tg="wait">
            {{ i18n.t('tg.flood_wait', { secs: waitSecs }) }}
          </div>
          <!-- 服务端给的原因。没有它的话界面只剩一句「登录失败」，
               用户报障时也说不出任何可供排查的信息 -->
          <div v-if="errDetail" class="d" data-tg="detail">
            {{ i18n.t('tg.detail', { detail: errDetail }) }}
          </div>
        </div>

        <div v-if="phase !== 'done' && !askingPassword" class="act">
          <button class="btn" data-tg="cancel" @click="cancel">
            {{ i18n.t('common.cancel') }}
          </button>
          <!-- 限流与 api_id 被封时没有重试按钮：点了必然失败 -->
          <button v-if="canRetry" class="btn pri" data-tg="retry" @click="start">
            {{ i18n.t('tg.retry') }}
          </button>
        </div>
        <div v-if="phase === 'done'" class="act">
          <button class="btn pri" data-tg="close" @click="emit('done', { sessionSaved, proxyUrl })">
            {{ i18n.t('common.close') }}
          </button>
        </div>
      </template>
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
  width: min(480px, 100%);
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
.lead {
  font-size: 12.5px;
  color: var(--fg2);
  line-height: 1.6;
  margin: 0 0 14px;
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
input[type='password'] {
  width: 100%;
  background: var(--bg3);
  color: var(--fg);
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  padding: 7px 9px;
  font-size: 13px;
  min-height: 36px;
}
.d {
  display: block;
  font-size: 11.5px;
  color: var(--fg2);
  margin-top: 4px;
  line-height: 1.5;
}
.strong {
  font-size: 13px;
  color: var(--fg);
}

/* 二维码：用格子画，不引外部资源也不生成图片。
   列数与行数由后端给的矩阵边长决定，见脚本里的 gridStyle。 */
.qrwrap {
  display: flex;
  gap: 16px;
  align-items: flex-start;
  flex-wrap: wrap;
}
.qr {
  width: 188px;
  height: 188px;
  /* 二维码必须是黑白的，不跟随主题：深色主题下反相的码有些客户端扫不出来 */
  background: #fff;
  padding: 10px;
  border-radius: var(--r-s);
  flex: none;
  display: grid;
}
.qr i {
  background: transparent;
}
.qr i.b {
  background: #000;
}
.qrside {
  flex: 1;
  min-width: 190px;
  font-size: 12.5px;
  color: var(--fg2);
  line-height: 1.6;
}
.ttl {
  font-size: 11.5px;
  color: var(--warn);
  margin-top: 7px;
}
/* 换图的一瞬要能被看见：静默替换等于没告诉用户发生过什么 */
.qr.refreshed {
  animation: qrflash 0.9s ease-out;
}
@keyframes qrflash {
  0% {
    box-shadow: 0 0 0 0 var(--accent);
  }
  100% {
    box-shadow: 0 0 0 10px transparent;
  }
}
.refreshnote {
  margin-top: 9px;
  font-size: 11.5px;
  color: var(--fg);
  border-inline-start: 2px solid var(--accent);
  padding-inline-start: 8px;
  line-height: 1.5;
}

.prog {
  font-size: 13px;
  color: var(--fg2);
  padding: 10px 0;
}
.pwbox,
.okbox {
  padding: 4px 0;
}
.pwbox input {
  margin-top: 9px;
}
.warnbox {
  font-size: 12px;
  color: var(--warn);
  line-height: 1.55;
  border: 1px solid var(--warn);
  border-radius: var(--r-s);
  padding: 9px 11px;
  margin-bottom: 4px;
}
.errbox {
  color: var(--danger);
  font-size: 12.5px;
  line-height: 1.55;
  margin-top: 10px;
}
.errbox .d {
  color: var(--fg2);
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

/* 窄屏把按钮抬到触控下限。
   这里必须写在组件自己的 scoped 样式里：scoped 会附加 [data-v-*]
   属性选择器，特异性高于 app.css 里的全局 `.btn`，
   全局那条压不住它——表现是「规则写了但按钮还是 36px」。 */
@media (max-width: 768px) {
  .btn,
  .qrbtn {
    min-height: 44px;
  }
}
</style>
