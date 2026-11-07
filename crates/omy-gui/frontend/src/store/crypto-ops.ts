/** 对选中文件的加密、恢复、改密码与恢复码。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import * as api from '../api';
import * as i18n from '../i18n';
import { setNotice, state } from './state';
import { encryptable, reload, restorable } from './local';

/* ---------------- 加密与解锁 ---------------- */

/** 加密选中的路径。 */
export async function encryptSelected(opts) {
  const paths = encryptable.value.map((e) => e.path);
  if (!paths.length) {
    state.error = i18n.te('empty_selection');
    return null;
  }

  state.busy = true;
  state.busyKey = 'busy.encrypting';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅要在发起加密**之前**建立：小文件可能在 await 返回前就发完事件，
  // 晚一步订阅就会一个都收不到。
  let unlisten = null;
  try {
    unlisten = await api.onEncryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    // 订阅失败只是没有进度条，不该让加密本身失败
    unlisten = null;
  }

  try {
    const summary = await api.encryptPaths({ paths, ...opts });
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await reload();

    if (summary.failed.length) {
      // 部分失败要说清楚哪些失败了，不能只报个数字。
      // 用户需要知道是哪个文件出了问题才能处理
      const first = summary.failed[0];
      state.error = i18n.t('notice.encrypt_partial', {
        ok: summary.items.length,
        failed: summary.failed.length,
        reason: i18n.te(first[1], first[1]),
      });
    } else {
      setNotice(i18n.tn('notice.encrypted', summary.items.length));
    }
    return summary;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.encrypt_failed'));
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    // 必须取消订阅：每次加密都新建一个监听器，不取消的话
    // 加密 N 次之后同一个事件会被处理 N 遍
    if (unlisten) unlisten();
  }
}

/** 把选中的加密文件还原到磁盘。
 *
 * `opts` 形如 `{ target_dir, overwrite }`，来自还原对话框。
 *
 * 成功后**不** reload：还原不改变加密文件本身，列表内容没有变化。
 * 白刷一次列表会让大目录闪一下，看起来像出了什么事。
 */
export async function restoreSelected(opts) {
  // 字段是 entry_id 不是 id：DirEntry 里的 id 专指「已登记的 FileEntry
  // 的 id」，与磁盘条目本身区分开
  const ids = restorable.value.map((e) => e.entry_id).filter(Boolean);
  if (!ids.length) {
    state.error = i18n.te('empty_selection');
    return null;
  }

  state.busy = true;
  state.busyKey = 'busy.restoring';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅必须在发起之前建立，理由同加密：小文件可能在 await 返回前
  // 就把事件发完了，晚一步订阅一个都收不到
  let unlisten = null;
  try {
    unlisten = await api.onDecryptProgress((p) => {
      state.progress = p;
    });
  } catch {
    unlisten = null;
  }

  try {
    const summary = await api.decryptPaths({ ids, ...opts });

    if (summary.failed.length) {
      // 部分失败要说清是哪个、为什么。只报数字的话用户无从下手，
      // 尤其 target_exists 是他自己能解决的（勾覆盖或换目录）
      const first = summary.failed[0];
      state.error = i18n.t('notice.restore_partial', {
        ok: summary.items.length,
        failed: summary.failed.length,
        reason: i18n.te(first[1], first[1]),
      });
    } else {
      setNotice(restoreNotice(summary.items));
    }
    return summary;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.restore_failed'));
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    if (unlisten) unlisten();
  }
}

/** 还原成功后的提示文案。
 *
 * 单独提出来是因为它要合并三种信息：还原了什么、放在哪、元数据有没有
 * 缺项。挤在一行里会很长，所以元数据只在**确实有缺项**时才提。
 *
 * 元数据那句不能省：解出来的文件时间戳不对是用户会注意到的事，
 * 事先说明「哪些项没还原、为什么」比让他事后怀疑文件坏了要好。
 */
function restoreNotice(items) {
  const where = items[0]?.output || '';
  const base = i18n.tn('notice.restored', items.length, { path: where });

  // 汇总所有条目的不支持项。同一种原因在多个文件上出现算一次——
  // 用户要知道的是「哪类元数据没还原」，不是每个文件重复一遍
  const kinds = new Set();
  for (const it of items) {
    for (const [code] of it.metadata?.unsupported || []) kinds.add(code);
  }
  if (!kinds.size) return base;

  const names = [...kinds].map((c) => i18n.t(`restore.meta_${c}`)).join('、');
  return `${base}（${i18n.t('restore.meta_partial', { items: names })}）`;
}

/** 给一个已加密文件增删改密码，或重新加密。
 *
 * add / change / remove 只改文件头，再大的文件也是毫秒级，所以不订阅
 * 进度——显示一个瞬间闪过的进度条只会让人以为出了什么事。
 *
 * reencrypt 相反：它要把载荷读一遍再写一遍，耗时与文件大小成正比，而
 * 对话框里还写着「期间请不要关闭程序」。不给进度条的话，用户面对一个
 * 静止的界面又被告知不能关，无从判断是在跑还是已经卡死。
 *
 * 成功后必须 reload：改完密码，会话里装的凭据变了，列表里这个文件
 * 是解锁还是锁定要重新算。不刷新的话界面还显示旧状态，用户点开会
 * 发现和刚才的操作对不上。
 */
/** 放弃重试并擦掉留在内存里的密码。对话框关闭时调用。 */
export function clearRetry() {
  state.retry = null;
  state.errorDetails = [];
}

/**
 * 只重试上次失败的那些文件。
 *
 * 与重跑整个操作的区别：重跑会拿旧密码去开已经改成新密码的文件，产生一堆
 * 假失败，用户分不清哪些是真问题；轮换时还要把已处理的文件再读写一遍。
 */
export async function retryKeyFiles() {
  const ctx = state.retry;
  if (!ctx || ctx.paths.length === 0) return null;

  state.busy = true;
  state.busyKey = ctx.rotate ? 'busy.reencrypting' : 'busy.deriving';
  state.error = '';
  state.notice = '';
  state.progress = null;

  let unlisten = null;
  if (ctx.rotate) {
    try {
      unlisten = await api.onEncryptProgress((p) => {
        state.progress = p;
      });
    } catch {
      unlisten = null;
    }
  }

  try {
    state.errorDetails = [];
    const r = await api.retryKeyFiles(ctx);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    // 全都补上了，重试上下文就该丢掉——留着会让"重试"按钮继续显示，
    // 用户再点一次会拿旧密码去开已经改好的文件，得到 wrong_password
    state.retry = null;
    await reload();
    setNotice(i18n.t('keymgmt.done_retry', { n: r.files_changed }));
    return r;
  } catch (e) {
    const p =
      e && typeof e === 'object'
        ? (e as { params?: { files?: unknown[]; paths?: unknown[] } }).params ?? null
      : null;
    const files = p && Array.isArray(p.files) ? p.files : [];
    const paths = p && Array.isArray(p.paths) ? p.paths : [];
    if (files.length > 0) await reload();
    state.error = i18n.te(api.errCode(e));
    state.errorDetails = files;
    // 仍有失败就缩小清单：只留这次还没成的，别让用户重复处理已经好了的
    if (paths.length > 0) state.retry = { ...ctx, paths };
    return null;
  } finally {
    if (unlisten) unlisten();
    state.busy = false;
    state.busyKey = '';
    state.progress = null;
  }
}

/** 把后端错误翻成给人看的话。
 *
 * 单独抽出来是因为恢复码有一条特殊的错误：bad_recovery_code 带着
 * core 给的定位信息（「第 7 个词『acadmic』不在词表中，是不是
 * 『academic』？」）。i18n.te 只按码取文案、不做插值，直接用会让
 * 界面上出现一个原样的「{{detail}}」，而这恰恰是最该说清的一条。
 */
function recoveryError(e) {
  const code = api.errCode(e);
  const params = e && typeof e === 'object' ? e.params : null;
  const msg = i18n.te(code);
  const detail = params && typeof params.detail === 'string' ? params.detail : '';
  return detail ? msg.replace('{{detail}}', detail) : msg;
}

/** 生成恢复码。
 *
 * 成功时**不** reload：文件内容没变，只是多了一个槽位，列表上看不出
 * 任何差别。刷一次只会让界面闪一下，还会清掉用户的选中项。
 *
 * 也不在这里发提示条——词还要留在对话框里给用户抄，弹一句「已生成」
 * 会让人以为事情已经办完。提示留到关闭对话框时再发。
 */
export async function generateRecovery(req) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.generateRecovery(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    return r;
  } catch (e) {
    state.error = recoveryError(e);
    return null;
  } finally {
    state.busy = false;
  }
}

/** 用恢复码重设密码。
 *
 * 这个要 reload：新密码装进了会话，原本锁着的文件可能因此显形。
 */
export async function restoreWithRecovery(req) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.restoreWithRecovery(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    await reload();
    return r;
  } catch (e) {
    state.error = recoveryError(e);
    return null;
  } finally {
    state.busy = false;
  }
}

export async function manageKey(req) {
  const rewrites = req.action === 'reencrypt';
  state.busy = true;
  // 阶段不同，说法也要不同：轮换时说"正在派生密钥"，用户会以为卡在
  // 一个本该几百毫秒的步骤上
  state.busyKey = rewrites ? 'busy.reencrypting' : 'busy.deriving';
  state.error = '';
  state.notice = '';
  state.progress = null;

  // 订阅要在发起之前建立：小文件可能在 await 返回前就发完事件，
  // 晚一步订阅就会一个都收不到
  let unlisten = null;
  if (rewrites) {
    try {
      unlisten = await api.onEncryptProgress((p) => {
        state.progress = p;
      });
    } catch {
      // 订阅失败只是没有进度条，不该让操作本身失败
      unlisten = null;
    }
  }

  try {
    // 上一次的失败清单必须清掉，否则这次成功了，界面上还挂着上次的红字
    state.errorDetails = [];
    const r = await api.manageKey(req);
    state.credentials = await api.credentialCount().catch(() => state.credentials);
    // 树形改密码后目录名会变（它由密码派生）。正在这棵树里面浏览时，
    // 当前目录本身也失效了，要在刷新前换过去，否则会去列一个不存在的路径
    const moved = r.is_tree && r.new_path && r.new_path !== req.path;
    if (moved && state.cwd === req.path) state.cwd = r.new_path;
    await reload();
    // 选中项必须在 reload **之后**设置：reload -> navigate 里有
    // `state.selected = []`，放在前面会被清掉。表现是改完密码一项都没选中，
    // 而列表里的名字又变了，用户会以为操作对象弄错了
    if (moved) state.selected = [r.new_path];
    setNotice(
      r.is_tree
        ? i18n.t('keymgmt.done_tree', { n: r.files_changed })
        : i18n.t(`keymgmt.done_${r.action}`),
    );
    return r;
  } catch (e) {
    // 部分失败要说清是哪些文件。i18n.te() 只按码取文案、不做插值，
    // 后端带过来的清单会被丢掉——只剩一句「部分文件改写失败」，用户既
    // 不知道该处理什么，也无法判断损失多大
    const p =
      e && typeof e === 'object'
        ? (e as { params?: { files?: unknown[]; paths?: unknown[] } }).params ?? null
      : null;
    const files = p && Array.isArray(p.files) ? p.files : [];
    const paths = p && Array.isArray(p.paths) ? p.paths : [];
    // 攒好重试要用的东西。密码用 req.current 而不是 req.next：失败的文件
    // 没被改写，还是旧密码
    state.retry =
      paths.length > 0
        ? { paths, current: req.current, next: req.next || '', rotate: rewrites, root: state.cwd }
        : null;
    // 刷新必须在设置错误**之前**：reload -> navigate 里有
    // `state.error = ''`，反过来会把刚设好的错误和清单一起冲掉，界面上
    // 对话框开着却什么都不显示，看起来像「什么都没发生」。
    // 与「选中项被 reload 清掉」是同一类顺序缺陷
    if (files.length > 0) await reload();
    state.error = i18n.te(api.errCode(e));
    state.errorDetails = files;
    return null;
  } finally {
    state.busy = false;
    state.progress = null;
    // 必须取消订阅：每次操作都新建一个监听器，不取消的话
    // 操作 N 次之后同一个事件会被处理 N 遍
    if (unlisten) unlisten();
  }
}
