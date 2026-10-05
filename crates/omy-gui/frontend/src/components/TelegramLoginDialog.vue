<script setup lang="ts">
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
import { isAndroid } from '../mobile-platform';
import * as i18n from '../i18n';
import * as api from '../api';

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
/** 路径输入框的提示文本。
 *
 * 给的是 Telegram 桌面版的**默认安装位置**，不是某台机器上的实际路径
 * ——写死后者既泄露了那台机器的目录布局，对别人也没有参考价值。
 *
 * 只是提示，不预填：真要预填得先确认目录存在，那是 checkTdPath 的事。
 */
const tdPlaceholder = computed(() =>
  navigator.userAgent.includes('Windows')
    ? '%APPDATA%\\Telegram Desktop\\tdata'
    : '~/.local/share/TelegramDesktop/tdata',
);

/** 是否还没通过风险告知那一步。
 *
 * 只在**首次登录**时拦一次——已经有 Telegram 位置的用户再加一个账号时
 * 不重复拦，那只是摩擦。
 *
 * 判据用「本机有没有 Telegram 位置」而不是 localStorage 里的一个标记：
 * 后者在换机器或清数据之后就不再提示了，而那时恰恰是一次真正的首次登录。
 */
const needRisk = ref(false);
/** 风险告知的勾选。 */
const riskAck = ref(false);

/** api_id 状态：{builtin, id, configured}。 */
const apiStatus = ref(null);
/** api_id 面板展开着吗。 */
const apiOpen = ref(false);
const apiId = ref('');
const apiHash = ref('');
const apiSaved = ref(false);
const apiErr = ref('');

async function loadApiStatus() {
  try {
    apiStatus.value = await api.telegramApiIdStatus();
  } catch {
    apiStatus.value = null;
  }
}

async function saveApiId() {
  apiErr.value = '';
  apiSaved.value = false;
  const n = Number.parseInt(apiId.value.trim(), 10);
  if (!Number.isFinite(n)) {
    apiErr.value = i18n.te('tg_bad_api_id');
    return;
  }
  try {
    await api.telegramApiIdSave(n, apiHash.value.trim());
    apiSaved.value = true;
    apiHash.value = '';
    await loadApiStatus();
  } catch (e) {
    apiErr.value = i18n.te(api.errCode(e) || 'tg_bad_api_id');
  }
}

async function resetApiId() {
  apiErr.value = '';
  apiSaved.value = false;
  try {
    await api.telegramApiIdReset();
    apiId.value = '';
    apiHash.value = '';
    await loadApiStatus();
  } catch (e) {
    apiErr.value = i18n.te(api.errCode(e));
  }
}

/** 连通性自检：null=没查过，否则 {status, elapsed_ms, via_proxy}。
 *
 * 字段是 snake_case——Rust 那边没加 serde rename，而本仓库其它命令
 * （RemoteEntry 的 is_dir / real_name 等）也都是 snake。按 camel 读
 * 会得到 undefined，而界面只会显示成「直连可用（undefined ms）」，
 * 状态本身却是对的，很难一眼看出。 */
const conn = ref(null);
/** 正在自检连通性。
 *
 * 与下面那个 `checking`（正在查有没有登录态）是两件事，所以不复用名字
 * ——同名会让人以为是同一个状态。 */
const connChecking = ref(false);

/** 跑一次连通性自检。
 *
 * 放在登录**之前**：实测直连 MTProto 数据中心超时、经 socks5 才通，
 * 而用户分不清「连不上」是代理、网络还是 api_id 的问题。先自检就把
 * 归因定下来，不用等他登录失败再盲猜。
 */
async function checkConn() {
  if (connChecking.value) return;
  connChecking.value = true;
  conn.value = null;
  try {
    conn.value = await api.telegramCheckConnection();
  } catch {
    // 自检本身失败也当成连不上——它不该比登录更脆弱
    conn.value = { status: 'no_route', elapsed_ms: 0, via_proxy: false };
  } finally {
    connChecking.value = false;
  }
}

/** 当前全局策略解析出的实际代理，只读展示；配置只能在设置页修改。 */
const activeProxy = ref('');
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
      emit('done', {
        sessionSaved: p.session_saved,
        placeId: null,
      });
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

// ---- 登录方式选择（扫码 / 手机号 / tdata）----
//
// 原型 §12.3：三条路是并列入口，不是把手机号/tdata 藏成切换按钮。
// method 为空时显示三选一；选定后进对应流程。
/** 'qr' | 'phone' | 'tdata' | ''（还没选）。 */
const method = ref('');

// ---- 手机号登录 ----
/** 手机号流程的子阶段：'input'（填号）| 'code'（输码）| 'password'（2FA）。 */
const phoneStage = ref('input');
/** 用户填的手机号（含国家码）。 */
const phoneNumber = ref('');
/** 用户填的验证码。 */
const phoneCode = ref('');
/** 用户填的 2FA 云密码。 */
const phonePassword = ref('');
/** 验证码是不是纯数字（决定 inputmode 与校验）。 */
const codeNumeric = ref(true);
/** 验证码固定位数；null 表示不限制（单词/短句）。 */
const codeLength = ref(null);
/** 「验证码发到哪里」的 i18n key。 */
const codeViaKey = ref('');
/** 这个送达方式要不要去其他客户端取码（App 为真，要显眼提示）。 */
const codeNeedsOther = ref(false);
/** 手机号登录是否正忙（发码/验码中，用于禁用按钮防重复提交）。 */
const phoneBusy = ref(false);
/** 手机号登录的错误码（停在本步显示，如验证码错）。 */
const phoneErr = ref('');
/** 手机号登录的限流秒数。 */
const phoneWait = ref(0);

let phoneUnlisten = null;

/** 处理一条手机号登录进度。 */
function onPhonePhase(p) {
  if (!p || typeof p.phase !== 'string') return;
  phoneBusy.value = false;
  switch (p.phase) {
    case 'connecting':
      phoneErr.value = '';
      break;
    case 'awaiting_phone':
      // 后端已连上、在等手机号——界面本来就停在 input，不用动
      phoneStage.value = 'input';
      break;
    case 'code_sent':
      phoneErr.value = '';
      phoneWait.value = 0;
      codeNumeric.value = !!p.numeric;
      codeLength.value = p.length || null;
      codeViaKey.value = p.via_key || 'tg.code.sentUnknown';
      codeNeedsOther.value = !!p.needs_other_client;
      phoneStage.value = 'code';
      phoneCode.value = '';
      break;
    case 'need_password':
      phoneErr.value = '';
      phoneStage.value = 'password';
      hint.value = p.hint || '';
      phonePassword.value = '';
      break;
    case 'done':
      phase.value = 'done';
      sessionSaved.value = p.session_saved;
      emit('done', {
        sessionSaved: p.session_saved,
        placeId: null,
      });
      break;
    case 'failed':
      // 停在本步显示错误（验证码错要重输、密码错要重输），不退回选择
      phoneErr.value = p.code;
      phoneWait.value = p.wait_secs || 0;
      break;
    default:
      break;
  }
}

/** 选了「手机号登录」：连上后端、开始等手机号。 */
async function startPhone() {
  method.value = 'phone';
  phoneStage.value = 'input';
  phoneErr.value = '';
  phoneNumber.value = '';
  started.value = true;
  phase.value = 'phone';
  if (phoneUnlisten) phoneUnlisten();
  phoneUnlisten = await api.onTelegramPhoneLogin(onPhonePhase);
  try {
    await api.telegramPhoneStart();
  } catch (e) {
    phoneErr.value = api.errCode(e) || 'tg_login_failed';
  }
}

async function submitPhone() {
  if (phoneBusy.value || !phoneNumber.value.trim()) return;
  phoneBusy.value = true;
  phoneErr.value = '';
  try {
    await api.telegramPhoneSubmitPhone(phoneNumber.value.trim());
  } catch (e) {
    phoneErr.value = api.errCode(e) || 'tg_login_failed';
    phoneBusy.value = false;
  }
}

async function submitCode() {
  if (phoneBusy.value || !phoneCode.value.trim()) return;
  phoneBusy.value = true;
  phoneErr.value = '';
  try {
    await api.telegramPhoneSubmitCode(phoneCode.value.trim());
  } catch (e) {
    phoneErr.value = api.errCode(e) || 'tg_login_failed';
    phoneBusy.value = false;
  }
}

async function resendCode() {
  if (phoneBusy.value) return;
  phoneBusy.value = true;
  phoneErr.value = '';
  try {
    await api.telegramPhoneResend();
  } catch (e) {
    phoneErr.value = api.errCode(e) || 'tg_login_failed';
    phoneBusy.value = false;
  }
}

async function submitPhonePassword() {
  if (phoneBusy.value || !phonePassword.value) return;
  phoneBusy.value = true;
  phoneErr.value = '';
  try {
    await api.telegramPhoneSubmitPassword(phonePassword.value);
  } catch (e) {
    phoneErr.value = api.errCode(e) || 'tg_login_failed';
    phoneBusy.value = false;
  }
}

async function openTdata() {
  tdataMode.value = true;
  method.value = 'tdata';
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

/** 打开目录选择器挑 tdata。
 *
 * 手输一个深路径既容易打错、也没法确认自己选对了。复用既有的
 * pickFolder（tauri dialog 插件本来就在用）。 */
async function browseTdata() {
  try {
    const dir = await api.pickFolder(i18n.t('tg.tdata_path'));
    if (dir) {
      tdPath.value = dir;
      tdAuto.value = false;
      await checkTdPath();
    }
  } catch {
    // 用户取消或没有选择器，维持原样
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

/** tdata 导入建好的位置 id。
 *
 * 成功屏上的「关闭」也要带着它——否则用户不点自动跳转、而是手动关掉时，
 * 外面又会走一次 connect，等于回到那个 bug。 */
const tdPlaceId = ref(null);

async function importTdata() {
  const p = tdPath.value.trim();
  if (!p || tdBusy.value) return;
  tdBusy.value = true;
  errCode.value = '';
  // 密码用完立刻从本地清掉：它在 WebView 里已无用途，留着只是多一处泄露面
  const pw = tdPass.value || null;
  try {
    // 这条命令**内部已经建好位置**并返回它的 id。必须接住往上传：
    // 丢掉的话外面会照扫码那条路去调 telegramPlaceConnect，而那个命令
    // 只认 PENDING_ACCOUNT 下的 session（扫码的占位账号），tdata 的
    // session 不在那儿——于是要么报「尚未登录」而位置其实已经建好了，
    // 要么拿上次扫码残留的 session 又建一个**别的账号**的位置
    // 后端返回 { id, duplicate }：命中已有账号时 id 是那个已有位置的
    const r = await api.telegramTdataImport(p, pw);
    tdPass.value = '';
    tdPlaceId.value = r.id;
    emit('done', {
      sessionSaved: true,
      placeId: r.id,
      duplicate: r.duplicate === true,
    });
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
    await api.telegramLoginStart();
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
  // 首次登录才拦一次风险告知。判据是本机有没有 Telegram 位置——
  // 用 localStorage 标记的话，换机器或清数据之后就不再提示了，
  // 而那时恰恰是一次真正的首次登录
  try {
    // 用 remotePlaceList（已注册的远程位置），不是 listPlaces——
    // 后者返回的是本地侧栏起点（主目录、磁盘根），里面永远没有
    // kind==='telegram'，于是每次都会拦，而不是只拦首次
    const places = await api.remotePlaceList();
    needRisk.value = !places.some((p) => p.kind === 'telegram');
  } catch {
    // 查不到就当作首次：多问一次好过漏掉告知
    needRisk.value = true;
  }
  void loadApiStatus();

  try {
    // 登录页只展示后端按全局策略解析出的实际地址；修改入口只在设置页。
    const p = await api.telegramSuggestProxy();
    activeProxy.value = p || '';
  } catch {
    // 读取失败不阻断登录；真正建连时后端会返回可翻译的配置错误。
  }
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
  if (phoneUnlisten) phoneUnlisten();
  api.telegramPhoneCancel().catch(() => {});
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
          <button class="btn pri" data-tg="close" @click="emit('done', { sessionSaved: true, placeId: tdPlaceId })">
            {{ i18n.t('common.close') }}
          </button>
        </div>
      </template>

      <!-- 首次登录先过风险告知。这一条有事实基础而非形式主义：
           官方明确写着用非官方客户端登录会让账号被置于观察状态，
           而用户一旦登录就无法撤销。只拦首次，不重复打扰 -->
      <template v-else-if="needRisk">
        <div class="sgh" data-tg="risk-title">{{ i18n.t('tg.risk_title') }}</div>
        <ul class="risklist" data-tg="risk-list">
          <li>{{ i18n.t('tg.risk_1') }}</li>
          <li>{{ i18n.t('tg.risk_2') }}</li>
        </ul>
        <label class="chk">
          <input v-model="riskAck" type="checkbox" data-tg="risk-ack" />
          {{ i18n.t('tg.risk_ack') }}
        </label>
        <div class="act">
          <button class="btn" data-tg="cancel" @click="emit('cancel')">
            {{ i18n.t('common.cancel') }}
          </button>
          <button
            class="btn pri"
            data-tg="risk-next"
            :disabled="!riskAck"
            @click="needRisk = false"
          >
            {{ i18n.t('tg.risk_next') }}
          </button>
        </div>
      </template>

      <!-- 开始之前：说明 + 代理 + 「存不存得住」的前置告知 -->
      <template v-else-if="!started">
        <!-- 选方式：扫码 / 手机号 / tdata 三条并列（原型 §12.3）。
             不是把手机号/tdata 藏成切换按钮——那样它们不是平等入口。 -->
        <div v-if="!method" class="methods" data-tg="methods">
          <button class="mcard" data-tg="m-qr" @click="method = 'qr'">
            <AppIcon class="mi" name="qr" />
            <b>{{ i18n.t('tg.method_qr') }}</b>
            <span class="d">{{ i18n.t('tg.method_qr_desc') }}</span>
          </button>
          <button class="mcard" data-tg="m-phone" @click="startPhone">
            <AppIcon class="mi" name="phone" />
            <b>{{ i18n.t('tg.method_phone') }}</b>
            <span class="d">{{ i18n.t('tg.method_phone_desc') }}</span>
          </button>
          <button v-if="!isAndroid" class="mcard" data-tg="m-tdata" @click="openTdata">
            <AppIcon class="mi" name="monitor" />
            <b>{{ i18n.t('tg.method_tdata') }}</b>
            <span class="d">{{ i18n.t('tg.method_tdata_desc') }}</span>
          </button>
        </div>

        <!-- 只在选了扫码那一支时显示这句说明 -->
        <p v-if="method === 'qr'" class="lead">{{ i18n.t('tg.qr_why') }}</p>

        <div v-if="method" class="f">
          <span class="fl">{{ i18n.t('tg.proxy') }}</span>
          <div class="d" data-tg="proxy">{{ activeProxy || i18n.t('tg.proxy_direct') }}</div>
          <span class="d">{{ i18n.t('tg.proxy_global_desc') }}</span>
        </div>

        <!-- 内置 api_id 说明 + 改用自己的那一对。
             这是一条逃生口：内置的是 Telegram Desktop 的 2040，它一旦触发
             API_ID_PUBLISHED_FLOOD，所有用户同时连不上而没有自救办法 -->
        <div v-if="method" class="apibox" data-tg="apibox">
          <span class="d" data-tg="api-status">
            {{ apiStatus && !apiStatus.builtin
              ? i18n.t('tg.api_mine', { id: apiStatus.id })
              : i18n.t('tg.api_builtin') }}
          </span>
          <button class="btn" data-tg="api-toggle" @click="apiOpen = !apiOpen">
            {{ i18n.t(apiStatus && !apiStatus.builtin ? 'tg.api_reset' : 'tg.api_custom') }}
          </button>
        </div>
        <div v-if="method && (apiOpen)" class="apifields" data-tg="apifields">
          <label class="f">
            <span class="fl">{{ i18n.t('tg.api_id_label') }}</span>
            <input v-model="apiId" data-tg="api-id" type="text" spellcheck="false" />
          </label>
          <label class="f">
            <span class="fl">{{ i18n.t('tg.api_hash_label') }}</span>
            <input v-model="apiHash" data-tg="api-hash" type="password"
                   autocomplete="off" spellcheck="false" />
          </label>
          <span class="d">{{ i18n.t('tg.api_hint') }}</span>
          <span v-if="apiErr" class="d warn" data-tg="api-err">{{ apiErr }}</span>
          <span v-if="apiSaved" class="d ok" data-tg="api-saved">
            {{ i18n.t('tg.api_saved') }}
          </span>
          <div class="act">
            <button class="btn" data-tg="api-reset" @click="resetApiId">
              {{ i18n.t('tg.api_reset') }}
            </button>
            <button class="btn pri" data-tg="api-save" @click="saveApiId">
              {{ i18n.t('tg.api_save') }}
            </button>
          </div>
        </div>

        <!-- 连通性自检。放在登录之前，把「连不上」的归因先定下来 -->
        <div v-if="method" class="conn" data-tg="conn" :data-st="connChecking ? 'checking' : (conn ? conn.status : 'idle')">
          <AppIcon
            :name="connChecking ? 'loader' : conn ? (conn.status === 'ok' ? 'circle-check' : 'circle-alert') : 'circle'"
            :class="{ spinning: connChecking }"
          />
          <div class="cb" data-tg="conn-text">
            <template v-if="connChecking">{{ i18n.t('tg.conn_checking') }}</template>
            <template v-else-if="!conn">{{ i18n.t('tg.conn_idle') }}</template>
            <template v-else-if="conn.status === 'ok'">
              {{ i18n.t(conn.via_proxy ? 'tg.conn_ok_proxy' : 'tg.conn_ok_direct',
                        { ms: conn.elapsed_ms }) }}
            </template>
            <template v-else-if="conn.status === 'bad_proxy'">
              {{ i18n.t('tg.conn_bad_proxy') }}
            </template>
            <template v-else>
              {{ i18n.t(conn.via_proxy ? 'tg.conn_no_route_proxy'
                                      : 'tg.conn_no_route_direct') }}
            </template>
          </div>
          <button class="btn" data-tg="conn-retry" :disabled="connChecking" @click="checkConn">
            {{ i18n.t('tg.conn_check') }}
          </button>
        </div>

        <!-- 这条必须在扫码**之前**说。等他扫完再说「存不住」，他下次打开
             发现又要扫码会以为程序把他登出了 -->
        <div v-if="method && (!canPersist)" class="warnbox" data-tg="no-persist">
          {{ i18n.t('tg.no_persist') }}
        </div>

        <!-- tdata 面板。与扫码并列的一条路，不是子步骤 -->
        <template v-if="method === 'tdata'">
          <div class="sgh" data-tg="td-title">{{ i18n.t('tg.tdata_title') }}</div>
          <p class="lead">{{ i18n.t('tg.tdata_desc') }}</p>

          <!-- 前提检查清单。
               原来这两条前提是散开的：客户端在跑是个 warnbox，路径没找到
               是输入框下面一行小字——长得不一样、位置也不一样，用户看不出
               「我还差几件事才能继续」。做成逐项打勾就一眼可见。 -->
          <div class="reqs" data-tg="td-reqs">
            <div class="req" :data-ok="tdRunning ? 0 : 1" data-req="running">
              <span class="ri" aria-hidden="true"><AppIcon :name="tdRunning ? 'close' : 'check'" :size="15" /></span>
              <span class="rt">
                <b>{{ i18n.t(tdRunning ? 'tg.req_running_bad' : 'tg.req_running_ok') }}</b>
                <span v-if="tdRunning" class="d">{{ i18n.t('tg.tdata_running') }}</span>
              </span>
            </div>
            <div class="req" :data-ok="tdPathOk ? 1 : 0" data-req="path">
              <span class="ri" aria-hidden="true"><AppIcon :name="tdPathOk ? 'check' : 'close'" :size="15" /></span>
              <span class="rt">
                <b>{{ i18n.t(tdPathOk ? 'tg.req_path_ok' : 'tg.req_path_bad') }}</b>
                <span v-if="!tdPathOk" class="d">{{ i18n.t('tg.tdata_not_found') }}</span>
              </span>
            </div>
          </div>

          <!-- 前提②⑤：找不到就**就地**给输入框。显示「未检测到」等于把
               「需要你补个信息」说成「不支持」，便携版用户会直接走掉 -->
          <label class="f">
            <span class="fl">{{ i18n.t('tg.tdata_path') }}</span>
            <!-- 输入框与「浏览…」同一行。
                 .f 是 block 的，直接并列会把按钮挤到输入框正下方、
                 左对齐孤零零一个（实测 gapY=0），看着像误放的 -->
            <div class="pathrow">
              <input
                v-model="tdPath"
                data-tg="td-path"
                type="text"
                :placeholder="tdPlaceholder"
                spellcheck="false"
                @input="checkTdPath"
              />
              <button class="btn" data-tg="td-browse" @click="browseTdata">
                {{ i18n.t('tg.browse') }}
              </button>
            </div>
            <span v-if="tdAuto && tdPathOk" class="d" data-tg="td-auto">
              {{ i18n.t('tg.tdata_autofound') }}
            </span>
            <!-- 「没自动找到」那句不在这里重复：上面的前提清单已经说了，
                 而且它会随状态打勾。同一件事说两遍会让人以为是两件事、
                 反而去找"还有哪里没配对" -->
            <span v-else-if="!tdPathOk && tdPath" class="d warn" data-tg="td-bad">
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
            <button class="btn" data-tg="td-back" @click="tdataMode = false; method = ''">
              {{ i18n.t('tg.back') }}
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

        <!-- 扫码那一支的动作区。手机号与 tdata 各有自己的流程/动作区。 -->
        <div v-else-if="method === 'qr'" class="act">
          <button class="btn" data-tg="qr-back" @click="method = ''">
            {{ i18n.t('tg.back') }}
          </button>
          <button class="btn pri" data-tg="start" @click="start">
            {{ i18n.t('tg.start') }}
          </button>
        </div>
        <!-- 还没选方式：只有一个取消。方法卡片自己就是「下一步」 -->
        <div v-else-if="!method" class="act">
          <button class="btn" data-tg="cancel" @click="cancel">
            {{ i18n.t('common.cancel') }}
          </button>
        </div>
      </template>

      <template v-else>
        <!-- 连接中 -->
        <div v-if="phase === 'connecting'" class="prog" data-tg="connecting">
          {{ i18n.t('tg.connecting') }}
        </div>

        <!-- 手机号登录：三个子屏由 phoneStage 切 -->
        <template v-if="phase === 'phone'">
          <!-- ① 输入手机号 -->
          <div v-if="phoneStage === 'input'" class="phonebox" data-tg="ph-input">
            <div class="strong">{{ i18n.t('tg.phone_title') }}</div>
            <div class="d">{{ i18n.t('tg.phone_desc') }}</div>
            <input
              v-model="phoneNumber"
              data-tg="ph-number"
              type="tel"
              inputmode="tel"
              autocomplete="off"
              spellcheck="false"
              :placeholder="i18n.t('tg.phone_placeholder')"
              @keydown.enter="submitPhone"
            />
            <div v-if="phoneErr" class="errbox" data-tg="ph-err">
              {{ i18n.te(phoneErr, i18n.t('tg.login_failed')) }}
            </div>
            <div class="act">
              <button class="btn" data-tg="ph-back"
                      @click="started = false; method = ''; phase = 'idle'">
                {{ i18n.t('tg.back') }}
              </button>
              <button
                class="btn pri"
                data-tg="ph-send"
                :disabled="phoneBusy || !phoneNumber.trim()"
                @click="submitPhone"
              >
                {{ phoneBusy ? i18n.t('tg.phone_sending') : i18n.t('tg.phone_send') }}
              </button>
            </div>
          </div>

          <!-- ② 输入验证码 -->
          <div v-else-if="phoneStage === 'code'" class="phonebox" data-tg="ph-code">
            <div class="strong">{{ i18n.t('tg.code_title') }}</div>
            <!-- 验证码发到哪里。App 那条要显眼——用户默认预期是等短信，
                 而它其实发到了其他已登录客户端，不提示他会白等 -->
            <div
              class="codevia"
              :class="{ strong: codeNeedsOther }"
              data-tg="ph-via"
            >
              {{ i18n.t(codeViaKey) }}
            </div>
            <input
              v-model="phoneCode"
              data-tg="ph-code-input"
              :type="codeNumeric ? 'text' : 'text'"
              :inputmode="codeNumeric ? 'numeric' : 'text'"
              :maxlength="codeLength || undefined"
              autocomplete="one-time-code"
              spellcheck="false"
              :placeholder="codeNumeric
                ? i18n.t('tg.code_placeholder_num')
                : i18n.t('tg.code_placeholder_text')"
              @keydown.enter="submitCode"
            />
            <div v-if="phoneErr" class="errbox" data-tg="ph-err">
              {{ i18n.te(phoneErr, i18n.t('tg.login_failed')) }}
              <span v-if="phoneWait > 0" class="d">
                {{ i18n.t('tg.flood_wait', { secs: phoneWait }) }}
              </span>
            </div>
            <div class="act">
              <button class="btn" data-tg="ph-resend"
                      :disabled="phoneBusy" @click="resendCode">
                {{ i18n.t('tg.code_resend') }}
              </button>
              <button
                class="btn pri"
                data-tg="ph-verify"
                :disabled="phoneBusy || !phoneCode.trim()"
                @click="submitCode"
              >
                {{ phoneBusy ? i18n.t('tg.code_verifying') : i18n.t('tg.code_verify') }}
              </button>
            </div>
          </div>

          <!-- ③ 2FA 云密码 -->
          <div v-else-if="phoneStage === 'password'" class="phonebox" data-tg="ph-pw">
            <div class="strong">{{ i18n.t('tg.need_password') }}</div>
            <div class="d">{{ i18n.t('tg.need_password_desc') }}</div>
            <input
              v-model="phonePassword"
              data-tg="ph-pw-input"
              type="password"
              autocomplete="off"
              :placeholder="i18n.t('tg.cloud_password')"
              @keydown.enter="submitPhonePassword"
            />
            <div v-if="hint" class="d" data-tg="ph-hint">
              {{ i18n.t('tg.password_hint', { hint }) }}
            </div>
            <div v-if="phoneErr" class="errbox" data-tg="ph-err">
              {{ i18n.te(phoneErr, i18n.t('tg.login_failed')) }}
            </div>
            <div class="act">
              <button class="btn" data-tg="cancel" @click="cancel">
                {{ i18n.t('common.cancel') }}
              </button>
              <button
                class="btn pri"
                data-tg="ph-pw-submit"
                :disabled="phoneBusy || !phonePassword"
                @click="submitPhonePassword"
              >
                {{ i18n.t('tg.submit_password') }}
              </button>
            </div>
          </div>
        </template>

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

        <div
          v-if="phase !== 'done' && phase !== 'phone' && !askingPassword"
          class="act"
        >
          <button class="btn" data-tg="cancel" @click="cancel">
            {{ i18n.t('common.cancel') }}
          </button>
          <!-- 限流与 api_id 被封时没有重试按钮：点了必然失败 -->
          <button v-if="canRetry" class="btn pri" data-tg="retry" @click="start">
            {{ i18n.t('tg.retry') }}
          </button>
        </div>
        <div v-if="phase === 'done'" class="act">
          <button class="btn pri" data-tg="close" @click="emit('done', { sessionSaved, placeId: tdPlaceId })">
            {{ i18n.t('common.close') }}
          </button>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.methods {
  display: grid;
  grid-template-columns: 1fr 1fr 1fr;
  gap: 10px;
  margin: 6px 0 12px;
}
.mcard {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 4px;
  padding: 14px 12px;
  text-align: start;
  background: var(--bg2);
  border: 1px solid var(--border);
  border-radius: var(--r-m);
  cursor: pointer;
}
.mcard:hover {
  border-color: var(--accent);
}
.mcard .mi {
  font-size: 22px;
}
.mcard b {
  font-size: 13px;
}
.mcard .d {
  font-size: 11.5px;
  color: var(--fg2);
}
/* 移动端窄屏：三张卡片竖排，横排会挤成一条读不出来 */
@media (max-width: 768px) {
  .methods {
    grid-template-columns: 1fr;
  }
}
.phonebox {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.phonebox input {
  padding: 8px 10px;
  font-size: 14px;
}
.codevia {
  font-size: 12.5px;
  color: var(--fg2);
}
/* App 送达（发到其他客户端）要显眼：用户默认以为等短信 */
.codevia.strong {
  color: var(--fg);
  font-weight: 500;
}
.risklist {
  margin: 8px 0 14px;
  padding-inline-start: 18px;
  font-size: 12.5px;
  line-height: 1.7;
  color: var(--fg2);
}
.chk {
  display: flex;
  gap: 8px;
  align-items: center;
  font-size: 12.5px;
  cursor: pointer;
}
.apibox {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 8px 0 2px;
}
.apibox .d {
  flex: 1;
}
.apibox .btn,
.apifields .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}
.apifields {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px;
  margin-bottom: 8px;
  border: 1px solid var(--border);
  border-radius: var(--r-s);
  background: var(--bg2);
}
.apifields .ok {
  color: var(--ok);
}

/* 前提检查清单：逐项打勾，一眼看出还差什么。
   散在各处的提示做不到这件事——用户得自己把它们拼起来 */
.reqs {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 10px 0 12px;
}
.req {
  display: flex;
  gap: 8px;
  align-items: flex-start;
  padding: 7px 9px;
  border-radius: var(--r-s);
  border: 1px solid var(--border);
  background: var(--bg2);
  font-size: 12px;
}
.req[data-ok='1'] {
  border-color: color-mix(in srgb, var(--ok) 40%, transparent);
  background: color-mix(in srgb, var(--ok) 8%, transparent);
}
.req[data-ok='0'] {
  border-color: color-mix(in srgb, var(--warn) 40%, transparent);
  background: color-mix(in srgb, var(--warn) 8%, transparent);
}
.req .ri {
  flex: 0 0 auto;
  line-height: 1.5;
}
.req[data-ok='1'] .ri {
  color: var(--ok);
}
.req[data-ok='0'] .ri {
  color: var(--warn);
}
.req .rt {
  display: flex;
  flex-direction: column;
  gap: 2px;
}

/* 连通性自检行：一行状态 + 两个小按钮。
   它出现在登录按钮之前，所以不能太重——是给用户定心的，不是报警。 */
.conn {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 10px 0 4px;
  padding: 8px 10px;
  border-radius: var(--r-s);
  border: 1px solid var(--border);
  background: var(--bg2);
  font-size: 12px;
}
.conn .cb {
  flex: 1;
  color: var(--fg2);
  line-height: 1.5;
}
/* 通了给一点绿，连不上给一点琥珀。用 color-mix 让两种主题都自适应，
   写死颜色会在浅色主题下糊成一团（这一轮已经踩过一次） */
.conn[data-st='ok'] {
  border-color: color-mix(in srgb, var(--ok) 45%, transparent);
  background: color-mix(in srgb, var(--ok) 10%, transparent);
}
.conn[data-st='ok'] .cb {
  color: color-mix(in srgb, var(--ok) 80%, var(--fg));
}
.conn[data-st='no_route'],
.conn[data-st='bad_proxy'] {
  border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
}
.conn[data-st='no_route'] .cb,
.conn[data-st='bad_proxy'] .cb {
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.conn .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

/* scheme 修正提示：填了 http:// 才出现，一行说明 + 一个按钮 */
.pxfix {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  font-size: 12px;
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.pxfix .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

/* 连通性自检行：一行状态 + 两个小按钮。
   它出现在登录按钮之前，所以不能太重——是给用户定心的，不是报警。 */
.conn {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 10px 0 4px;
  padding: 8px 10px;
  border-radius: var(--r-s);
  border: 1px solid var(--border);
  background: var(--bg2);
  font-size: 12px;
}
.conn .cb {
  flex: 1;
  color: var(--fg2);
  line-height: 1.5;
}
/* 通了给一点绿，连不上给一点琥珀。用 color-mix 让两种主题都自适应，
   写死颜色会在浅色主题下糊成一团（这一轮已经踩过一次） */
.conn[data-st='ok'] {
  border-color: color-mix(in srgb, var(--ok) 45%, transparent);
  background: color-mix(in srgb, var(--ok) 10%, transparent);
}
.conn[data-st='ok'] .cb {
  color: color-mix(in srgb, var(--ok) 80%, var(--fg));
}
.conn[data-st='no_route'],
.conn[data-st='bad_proxy'] {
  border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
}
.conn[data-st='no_route'] .cb,
.conn[data-st='bad_proxy'] .cb {
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.conn .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

/* scheme 修正提示：填了 http:// 才出现，一行说明 + 一个按钮 */
.pxfix {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  font-size: 12px;
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.pxfix .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

/* 连通性自检行：一行状态 + 两个小按钮。
   它出现在登录按钮之前，所以不能太重——是给用户定心的，不是报警。 */
.conn {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 10px 0 4px;
  padding: 8px 10px;
  border-radius: var(--r-s);
  border: 1px solid var(--border);
  background: var(--bg2);
  font-size: 12px;
}
.conn .cb {
  flex: 1;
  color: var(--fg2);
  line-height: 1.5;
}
/* 通了给一点绿，连不上给一点琥珀。用 color-mix 让两种主题都自适应，
   写死颜色会在浅色主题下糊成一团（这一轮已经踩过一次） */
.conn[data-st='ok'] {
  border-color: color-mix(in srgb, var(--ok) 45%, transparent);
  background: color-mix(in srgb, var(--ok) 10%, transparent);
}
.conn[data-st='ok'] .cb {
  color: color-mix(in srgb, var(--ok) 80%, var(--fg));
}
.conn[data-st='no_route'],
.conn[data-st='bad_proxy'] {
  border-color: color-mix(in srgb, var(--warn) 45%, transparent);
  background: color-mix(in srgb, var(--warn) 10%, transparent);
}
.conn[data-st='no_route'] .cb,
.conn[data-st='bad_proxy'] .cb {
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.conn .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

/* scheme 修正提示：填了 http:// 才出现，一行说明 + 一个按钮 */
.pxfix {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 6px;
  font-size: 12px;
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
}
.pxfix .btn {
  padding: 2px 9px;
  min-height: 28px;
  font-size: 12px;
  white-space: nowrap;
}

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
  /* 必须给背景。原来只设了文字色和边框，于是琥珀色字直接压在对话框
     底色上——浅色主题下算出来对比度只有 2.42（深色主题 7.6 正常），
     这就是「只在浅色主题下看不清」的来源。
     做法与 styles/app.css 里的 .warnbox 一致：淡的同色底 + 同色边框。 */
  background: color-mix(in srgb, var(--warn) 12%, transparent);
  color: color-mix(in srgb, var(--warn) 80%, var(--fg));
  line-height: 1.55;
  border: 1px solid color-mix(in srgb, var(--warn) 45%, transparent);
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
/* 主按钮与禁用态的配色统一由 styles/app.css 提供（.pri 是
   .primary 的别名）。这里**不要**再抄一份：抄漏哪个状态，
   那个状态就退回默认值，而最容易漏的正是 hover */
.pathrow {
  display: flex;
  gap: calc(var(--sp) * 2);
  align-items: center;
}
.pathrow input {
  flex: 1;
  min-width: 0;
}
.pathrow .btn {
  flex: none;
  white-space: nowrap;
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
