/** 解锁（密码 / 设备密钥 / 密码管理器）、锁定收尾与界面语言。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import * as api from '../api';
import * as i18n from '../i18n';
import { setNotice, state } from './state';
import { refreshDeviceOverview, reload } from './local';
import { reloadRemotePlaces } from './virtual';
import { leaveRemotePlace, reloadRemoteDir } from './remote-browse';

/** 用一个密码试解锁当前目录。 */
/**
 * 用设备密钥解锁当前目录。
 *
 * 与 tryUnlock 并列而不是替代：会话里的凭据是累加的，按指纹不会挤掉
 * 之前手动输入的密码。
 */
export async function tryDeviceUnlock() {
  if (!state.cwd) return false;
  state.busy = true;
  // 文案是「正在等待确认」而不是「正在派生」：这一步在等用户按指纹，
  // 说「派生中」会让人以为该干等
  state.busyKey = 'busy.waiting_hello';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.deviceKeyUnlock(state.cwd);
    state.credentials = r.credentials;
    await reload();
    // 与 tryUnlock 同一个判据：加密目录的 is_encrypted 故意是 false
    // （那个字段指「内容是密文」，而目录只有名字是密文），只按它过滤
    // 会把「只含树形加密目录」的文件夹误判成解锁失败
    const opened = state.entries.filter(
      (e) => (e.is_encrypted || e.is_encrypted_dir) && e.unlocked,
    ).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      // 设备密钥能解封说明硬件那边没问题，打不开文件就是另一回事了：
      // 这把钥匙不属于这个位置的文件
      state.error = i18n.te('device_key_not_enrolled');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(e instanceof Error ? e.message : String(e ?? ''));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 用密码管理器候选直接解锁当前目录。
 *
 * 前端只传候选 UUID；真正的密码留在 Rust 缓存里，由后端直接走与手工密码
 * 相同的 unlock_directory 链路。这样 Vue 状态和浏览器调试工具都看不到秘密。
 */
export async function tryPasswordManagerUnlock(credentialId: string) {
  if (!state.cwd) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.passwordManagerUnlock(state.cwd, credentialId);
    state.credentials = r.credentials;
    await reload();
    void reloadRemotePlaces();
    const opened = state.entries.filter(
      (e) => (e.is_encrypted || e.is_encrypted_dir) && e.unlocked,
    ).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
      return true;
    }
    state.error = i18n.te('wrong_password');
    return false;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

/** 在**远程位置**里试密码。
 *
 * 与局域网那条 `unlockRemote` 同构：远端没有「目录」可探测 vault，
 * 所以先从当前远程目录的文件头里收集 vault 参数，再派生。
 *
 * 成功后要重新识别当前目录——解锁改变的是**识别结果**（真实文件名、
 * 缩略图、可否播放），不重新识别的话界面上一切照旧，
 * 用户会以为密码没生效。
 */
async function tryUnlockRemotePlace(password) {
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const vaults = await api.remotePlaceVaults(state.remotePlace, state.remoteDir || '');
    if (!vaults.length) {
      // 这个目录里没有加密文件，谈不上解锁。与「密码不对」分开说：
      // 前者是「这儿没东西要解」，后者是「你输错了」
      state.error = i18n.te('no_vault_found');
      return false;
    }
    const r = await api.unlock('main', password, vaults);
    state.credentials = r.credentials;
    await reloadRemoteDir();
    const opened = state.remoteItems.filter((f) => f.is_encrypted && f.unlocked).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
      // 派生几乎总会「成功」，真正的判据是有没有东西被解开
      state.error = i18n.te('wrong_password');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
    state.busyKey = '';
  }
}

export async function tryUnlock(password) {
  // 在远程位置里走远程那条：它没有本地目录，unlockDirectory 用不了。
  //
  // 不分流会怎样：下面那句 `if (!state.cwd)` 直接 return false，
  // 于是用户在远程位置里输密码**什么都不会发生，连错误都没有**，
  // 看到的是「密码输了没反应，文件一直锁着」。
  if (state.remotePlace) return tryUnlockRemotePlace(password);
  if (!state.cwd) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const r = await api.unlockDirectory(state.cwd, password);
    state.credentials = r.credentials;
    await reload();
    // 同密码的远程/虚拟位置可能因此变成可免密进入，刷一次侧栏锁徽标。
    // 不 await：不挡本地列表的即时反馈。
    void reloadRemotePlaces();
    // 判据要同时算上加密文件与加密目录。
    //
    // 加密目录的 is_encrypted 故意是 false——那个字段的含义是「内容是
    // 密文」，而目录没有内容，只有名字是密文。所以只按 is_encrypted 过滤
    // 会漏掉它们：一个只含树形加密目录的文件夹，解锁成功后 opened 仍是 0，
    // 被判成「密码不对」，用户输了正确密码却看到红字报错，而目录名其实
    // 已经解出来了。
    const opened = state.entries.filter(
      (e) => (e.is_encrypted || e.is_encrypted_dir) && e.unlocked,
    ).length;
    if (opened > 0) {
      // 重复输入同一个密码要与「真的加了一个」区分开。
      //
      // 不区分会怎样：用户忘了这个密码已经装过，再输一次看到「解锁了 N 个
      // 文件」，但状态栏的密码数纹丝不动——看起来像计数坏了。如实说
      // 「已经在用了」才对得上他看到的现象
      if (r.added === false) {
        setNotice(i18n.t('unlock.already_loaded'));
      } else {
        setNotice(i18n.tn('notice.unlocked', opened));
      }
    } else {
      // 派生成功但一个都没解开 = 密码不对。
      // 这个区分很重要：KEK 派生几乎总是"成功"的，
      // 真正的判据是有没有东西被解开
      state.error = i18n.te('wrong_password');
    }
    return opened > 0;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 刷新已识别加密文件的索引。 */
export async function refreshKnown() {
  if (state.credentials === 0) {
    state.known = {};
    return;
  }
  try {
    // scan_directory 会把结果存进后端状态，预览时按 id 取
    const files = await api.scanDirectory(state.cwd, false);
    const map = {};
    for (const f of files) map[f.path] = f;
    state.known = map;

    // 把解锁信息补回列表，让预览能找到对应条目。
    //
    // 后端 `annotate_unlocked` 做的是同一件事。两处都要改是重复，
    // 但这条路径（扫描后回填）与那条（列目录时标注）触发时机不同，
    // 目前无法合并——**新增字段时两边都得加**，漏了就会出现
    // 「后端算对了、界面上却没生效」。
    for (const e of state.entries) {
      const f = map[e.path];
      if (f && f.unlocked) {
        e.unlocked = true;
        e.real_name = f.name;
        e.entry_id = f.id;
        e.is_container = f.is_container;
      }
    }
  } catch {
    // 扫描失败不影响浏览——列表还在，只是加密文件显示成锁定
  }
}

/** 锁定：抹掉所有凭据。
 *
 * 后端的 `lock` 会**同时关闭设备库**，所以这里也要把设备概览清掉——
 * 否则侧栏还挂着「已配对 3 台」的角标，而设备库其实已经锁上了。
 */
export async function lock() {
  // 后端失败也要清界面：锁定的语义是「从现在起什么都看不到」，
  // 后端报错时界面继续亮着解密内容是最糟的失败方式（fail-open）。
  try {
    await api.lock();
  } finally {
    await applyLockedState();
  }
}

/** 正在进行的界面清理。后端的锁定事件与主动锁定可能同时触发，合并成一次。 */
let lockTeardown: Promise<void> | null = null;

/** 把界面切到「已锁定」：清凭据计数、解密出来的名字、容器与远程内容，再重载列表。
 *
 * 由两条路径调用：前端主动 `lock()`，以及后端的 `session://locked` 事件
 * （自动锁定等不经过前端的锁定）。两者常常同时到达，所以并发调用共享同一次清理。 */
export function applyLockedState(): Promise<void> {
  if (!lockTeardown) {
    lockTeardown = clearLockedState().finally(() => {
      lockTeardown = null;
    });
  }
  return lockTeardown;
}

async function clearLockedState() {
  state.credentials = 0;
  state.known = {};
  // 先同步抹掉列表里已解密的字段，再等重载：重载要走 IPC，失败时 navigate
  // 会保留旧列表——那份旧列表里正是解密出来的真名。
  for (const e of state.entries) {
    if (e.is_encrypted || e.is_encrypted_dir) {
      e.unlocked = false;
      e.real_name = null;
    }
  }
  state.selected = [];
  state.pairedCount = 0;
  // 容器视图里列的是解密出来的文件名，锁定后不该继续可见。
  // 下面还有一道 watch 兜着，两道都要：这一行是主路径，
  // watch 保证「即使某条路径忘了清，界面也不会留着内容」
  state.container = null;
  // 后端的 lock 已经断开了远端连接，这里同步界面状态。
  // 顺序不能反：先清状态再调后端的话，中间那一刻界面显示的是
  // 「已断开」而连接其实还在
  state.peer = null;
  state.remoteEntries = [];
  state.remoteMode = false;
  // 云盘/Telegram/虚拟位置同样已被后端锁定：退出云盘浏览（清掉解密出来的
  // 文件/消息列表与当前位置），并重载侧栏位置，让锁徽标统一回到闭合锁。
  // 不做这步的话，本地锁了但右侧还停在已解密的远程内容、侧栏仍显示开口锁。
  if (state.remotePlace) leaveRemotePlace();
  await reloadRemotePlaces();
  // 重新载入让加密文件回到锁定显示。
  // 不能只改本地字段——那样万一漏改一处就是信息泄露
  await reload();
  await refreshDeviceOverview();
}

/** 切换语言。 */
export async function switchLanguage() {
  const next = i18n.nextLang();
  await i18n.load(next);
  await api.setLanguage(next);
  localStorage.setItem('omy.lang', next);
}

/** 切到指定语言。设置页用这个，顶栏的循环切换用 switchLanguage。
 *
 * `auto` 表示跟随系统：此处解析成具体语言再加载，因为 i18n.load 要一个
 * 确定的语言包。配置文件里仍然存 'auto'——存解析结果的话，用户换了系统
 * 语言之后应用不会跟着变，而他明明选的是「跟随系统」。
 */
export async function applyLanguage(pref) {
  const target = pref === 'auto' ? detectSystemLang() : pref;
  if (!i18n.LANGS.includes(target)) return;
  await i18n.load(target);
  await api.setLanguage(target);
  localStorage.setItem('omy.lang', pref);
}

/** 从浏览器环境猜系统语言，猜不出时回落到英文。
 *
 * 与后端 `state::detect_language` 是同一套意图——**改一处要想到另一处**。
 * 两边不一致的表现是「启动瞬间是一种语言，加载完配置后跳成另一种」。
 */
function detectSystemLang() {
  const raw = (navigator.language || 'en').toLowerCase();
  return raw.startsWith('zh') ? 'zh-CN' : 'en';
}
