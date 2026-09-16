/** 自动锁定：闲置一段时间后清空会话密钥。
 *
 * # 「闲置」不能只看输入设备
 *
 * 用户看一部两小时的加密电影时全程不碰鼠标键盘——那不是闲置。
 * 播放中必须算活跃，否则会把正在看的片子锁掉，而用户完全不知道
 * 为什么画面突然没了。
 *
 * # 切到后台怎么算
 *
 * 默认「切后台 = 开始闲置」，不单独立即锁：切出去回消息时，计时照常走，
 * 而播放中（见上）仍算活跃不会被锁。只有用户显式打开「切后台立即锁定」
 * （lock_on_background）时，才在页面 hidden 的瞬间锁——那是给担心任务
 * 切换器截图泄露内容的强安全场景，代价是回到前台总要重新解锁。
 *
 * # 为什么计时器放前端
 *
 * 判定依据（有没有在播放、用户有没有操作界面）全在 WebView 里。
 * 放后端的话要把每一次鼠标移动都发过去，纯属浪费；而锁定动作本身
 * 仍然是后端执行的——密钥从来不在前端。
 */

import { ref } from 'vue';
import * as api from './api.js';

/** 上次活跃时间戳。 */
let lastActive = Date.now();
/** 轮询句柄。 */
let timer = null;
/** 闲置多少毫秒后锁定；0 表示从不。 */
let idleMs = 0;
/** 切后台是否立即锁定（显式强安全选项，默认关：默认切后台只算闲置）。 */
let lockOnBackground = false;

/** 当前是否有媒体正在播放。
 *
 * 由播放组件在开始/结束时设置。不自己去查 DOM 里的 `<video>`：
 * 预览覆盖层关闭后元素会被移除，而移除的瞬间恰好可能被轮询读到，
 * 判断结果取决于时序。
 */
export const playing = ref(false);

/** 记一次用户活跃。 */
export function markActive() {
  lastActive = Date.now();
}

/** 真正执行锁定。
 *
 * 失败不重试也不报错：锁定失败的唯一可能是后端已经锁了，
 * 再弹一条错误只会让用户困惑。
 */
async function doLock() {
  try {
    await api.lock();
  } catch {
    /* 已经锁上了 */
  }
}

/** 轮询检查是否该锁定。
 *
 * 用轮询而不是 setTimeout：每次活跃都重设定时器的话，鼠标移动时
 * 会每秒重建几十个定时器。轮询一秒一次，开销可忽略。
 */
function tick() {
  if (!idleMs) return;
  // 播放中一律算活跃——这条必须在超时判断之前，否则看片看到一半会被锁
  if (playing.value) {
    lastActive = Date.now();
    return;
  }
  if (Date.now() - lastActive >= idleMs) {
    doLock();
    // 锁定后重置计时，避免锁定状态下每秒都触发一次
    lastActive = Date.now();
  }
}

/**
 * 页面切到后台。
 *
 * 只有显式开启「切后台立即锁定」才在这里立刻锁。默认开关关闭时什么都不
 * 做——切后台等同于开始闲置，由上面的 tick 按 auto_lock_secs 结算，播放中
 * 同样受豁免。后台定时器会被系统节流甚至挂起，所以默认路径下真正的锁定
 * 往往发生在回到前台、tick 补发发现已超时的那一刻：回前台先锁、再要密码，
 * 不会把内容直接亮出来。
 */
function onVisibility() {
  if (document.visibilityState === 'hidden' && lockOnBackground) {
    doLock();
  }
}

/** 按配置启动或重启自动锁定。
 *
 * 配置改变后要调一次，否则用户在设置里改了超时却要等到重启才生效。
 */
export function configureAutoLock(security) {
  idleMs = Math.max(0, Number(security?.auto_lock_secs || 0)) * 1000;
  // 只有显式 true 才立即锁；缺省/旧配置缺字段时按默认（切后台只算闲置）
  lockOnBackground = security?.lock_on_background === true;

  if (timer) {
    clearInterval(timer);
    timer = null;
  }
  // 即便 idleMs 为 0 也要挂 visibility 监听：切后台锁定是独立开关
  if (idleMs > 0) {
    lastActive = Date.now();
    timer = setInterval(tick, 1000);
  }
}

/** 装上全局监听。只在应用启动时调一次。 */
export function initAutoLock() {
  for (const ev of ['mousedown', 'keydown', 'wheel', 'touchstart']) {
    // passive：这些只是记时间戳，不阻止默认行为；
    // 不加的话滚动会被浏览器判定为可能阻塞而掉帧
    document.addEventListener(ev, markActive, { passive: true });
  }
  document.addEventListener('visibilitychange', onVisibility);
}
