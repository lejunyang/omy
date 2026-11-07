/** 本地文件浏览：URL 拼接、排序、选中、右键菜单、增删改、容器视图、导航与面包屑。
 *
 * 从原先四千多行的 store.ts 拆出；对外仍统一经由 `store.ts` 导出，
 * 调用方与测试的 import / vi.mock 路径不变。
 */

import { reactive, computed } from 'vue';
import * as api from '../api';
import * as i18n from '../i18n';
import { theme } from '../theme';
import { setNotice, state, trashSupported } from './state';
import { refreshKnown } from './session';

/** 拼出远端文件的内容 URL。 */
export function remoteFileUrl(id) {
  return `${state.streamBase}/rfile/${encodeURIComponent(id)}`;
}

/** 拼出远端文件的缩略图 URL。 */
export function remoteThumbUrl(id) {
  return `${state.streamBase}/rthumb/${encodeURIComponent(id)}`;
}

/** 拼出远程位置（WebDAV 等）已打开文件的内容 URL。
 *
 * token 由 remotePlaceOpen 颁发，对应后端一个带密文块缓存的来源；
 * 多次 Range 请求复用它，seek 才不会重复下载。
 */
export function placeFileUrl(token) {
  return `${state.streamBase}/pfile/${encodeURIComponent(token)}`;
}

/** 拼出远程位置文件的缩略图 URL。 */
export function placeThumbUrl(token) {
  return `${state.streamBase}/pthumb/${encodeURIComponent(token)}`;
}

/** 连接一台设备并进入远端视图。 */
export async function connectRemote(fingerprint, addr) {
  state.busy = true;
  state.busyKey = 'busy.connecting';
  state.error = '';
  try {
    state.peer = await api.remoteConnect(fingerprint, addr || null);
    state.remoteEntries = await api.remoteList();
    state.remoteMode = true;
    state.selected = [];
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.connect_failed'));
    state.peer = null;
    state.remoteMode = false;
    return false;
  } finally {
    state.busy = false;
  }
}

/** 断开远端，回到本机视图。 */
export async function disconnectRemote() {
  await api.remoteDisconnect().catch(() => {});
  state.peer = null;
  state.remoteEntries = [];
  state.remoteMode = false;
}

/** 重新拉远端列表。 */
export async function reloadRemote() {
  if (!state.remoteMode) return;
  state.busy = true;
  state.busyKey = 'busy.loading';
  try {
    state.remoteEntries = await api.remoteList();
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    state.busy = false;
  }
}

/** 在远端视图里试一个密码。
 *
 * 远端没有「目录」可探测 vault，所以不能走 `unlockDirectory`。
 * 这里直接用远端文件头里的 salt 派生——头部在列表里已经有了。
 */
export async function tryUnlockRemote(password) {
  if (!state.remoteMode) return false;
  state.busy = true;
  state.busyKey = 'busy.deriving';
  state.error = '';
  state.notice = '';
  try {
    const vaults = await api.remoteVaults();
    if (!vaults.length) {
      state.error = i18n.te('no_vault_found');
      return false;
    }
    const r = await api.unlock('main', password, vaults);
    state.credentials = r.credentials;
    state.remoteEntries = await api.remoteRelock();
    const opened = state.remoteEntries.filter((f) => f.unlocked).length;
    if (opened > 0) {
      setNotice(i18n.tn('notice.unlocked', opened));
    } else {
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

/** 刷新设备相关的概览状态（侧栏角标与共享指示）。
 *
 * 只取计数不取明细：侧栏不需要设备名，而少取一层就少一处泄露面。
 */
export async function refreshDeviceOverview() {
  try {
    const st = await api.deviceStatus();
    state.pairedCount = st.opened ? st.paired_count : 0;
    const sh = await api.shareStatus();
    state.shareRunning = sh.running;
  } catch {
    // 设备库不可用不该影响文件浏览
    state.pairedCount = 0;
    state.shareRunning = false;
  }
}

/** 拼出某个文件的内容 URL。 */
export function fileUrl(id) {
  return `${state.streamBase}/file/${encodeURIComponent(id)}`;
}

/** 拼出未加密文件的内容 URL。
 *
 * 走的是同一条 `omystream://` 协议，只是换个前缀。为什么不直接用
 * 文件路径：那需要开启 Tauri 的 asset 协议，等于把整个文件系统
 * 暴露给 WebView 里的 JS。
 */
export function plainUrl(token) {
  return `${state.streamBase}/plain/${encodeURIComponent(token)}`;
}

/** 拼出容器内单个文件的内容 URL。
 *
 * 同样只换前缀。token 由后端 `list_container` 登记后下发，前端**拿不到**
 * 「按偏移读容器任意位置」的能力——那个约束在后端。
 */
export function containerItemUrl(token) {
  return `${state.streamBase}/citem/${encodeURIComponent(token)}`;
}

/** 用系统程序打开。显式“用其他应用打开”时强制重新选择应用。 */
export async function openWithSystem(entry, chooseApplication = false) {
  if (!entry?.token) return false;
  try {
    const result = await api.openExternal(entry.token, chooseApplication, theme.value);
    // 用户取消原生选择器不是失败，不显示错误提示。
    return result?.cancelled !== true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('open_failed'));
    return false;
  }
}

/** 在系统文件管理器里定位本地文件或文件夹。 */
export async function revealEntry(entry) {
  if (!entry?.token) return false;
  try {
    await api.revealInFolder(entry.token);
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('open_failed'));
    return false;
  }
}

/** 拼出某个文件的缩略图 URL。 */
export function thumbUrl(id) {
  return `${state.streamBase}/thumb/${encodeURIComponent(id)}`;
}

/** 过滤并排序后的条目。
 *
 * 排序在后端已按自然序做过（目录在前）。这里只做搜索过滤，
 * 不重排——否则会和后端的顺序打架。
 */
export type SortKey = 'name' | 'size' | 'modified' | 'type';

const SORT_STORE_KEY = 'omy.sort';

/** 读回上次的排序。读写都包在 try 里：隐私模式下 localStorage 可能直接抛错。 */
export function loadSort() {
  try {
    const raw = JSON.parse(localStorage.getItem(SORT_STORE_KEY) || 'null');
    if (raw && ['name', 'size', 'modified', 'type'].includes(raw.key)) {
      state.sortKey = raw.key;
      state.sortDir = raw.dir === 'desc' ? 'desc' : 'asc';
    }
  } catch {
    /* 读不到就用默认的按名称升序 */
  }
}

/** 设置排序；再点同一列切换升降序。 */
export function setSort(key: SortKey) {
  if (state.sortKey === key) {
    state.sortDir = state.sortDir === 'asc' ? 'desc' : 'asc';
  } else {
    state.sortKey = key;
    // 大小与时间默认从大 / 新到小 / 旧：找「最大的文件」「最近改过的」才是常见目的
    state.sortDir = key === 'size' || key === 'modified' ? 'desc' : 'asc';
  }
  try {
    localStorage.setItem(SORT_STORE_KEY, JSON.stringify({ key: state.sortKey, dir: state.sortDir }));
  } catch {
    /* 存不下只是下次不记得，不影响这次 */
  }
}

/** 排序用的名字：已解锁的用真名，锁着的用磁盘名（不会泄露什么，只是让顺序稳定）。 */
function sortName(e) {
  return (e.real_name || e.name || '').toString();
}

function sortType(e) {
  // 锁着的加密文件类型未知，排在所有类型之后
  if (e.is_encrypted && !e.unlocked) return '￿';
  const n = e.is_encrypted ? e.real_name || '' : e.name || '';
  const i = n.lastIndexOf('.');
  return i > 0 ? n.slice(i + 1).toLowerCase() : '';
}

/** 目录永远在前；同类之间按所选字段排，相同时按名称兜底，保证顺序稳定。 */
function sortEntries(list) {
  const dir = state.sortDir === 'desc' ? -1 : 1;
  // 与界面语言一致的排序规则：中文按拼音，数字按数值（第2章在第10章之前）
  const coll = i18n.collator();
  const byName = (a, b) => coll.compare(sortName(a), sortName(b));
  const cmp = (a, b) => {
    if (!!a.is_dir !== !!b.is_dir) return a.is_dir ? -1 : 1;
    let r = 0;
    if (state.sortKey === 'size') r = (a.size ?? -1) - (b.size ?? -1);
    else if (state.sortKey === 'modified') r = (a.modified ?? -1) - (b.modified ?? -1);
    else if (state.sortKey === 'type') r = sortType(a).localeCompare(sortType(b));
    else r = byName(a, b);
    return r * dir || byName(a, b);
  };
  return [...list].sort(cmp);
}

export const visibleEntries = computed(() => {
  // 服务端搜索模式：列的是候选集，而不是当前目录。
  //
  // 仍然要过一遍本地精筛——这就是原型里的「两段式搜索」：服务端按文字缩小
  // 范围（快、省流量），本地再按**解出来的真实文件名**匹配。服务端搜的是
  // 消息文字，omy 加密文件的真实文件名它永远没有，所以少了本地这一段，
  // 搜索对加密文件就完全失效。
  const source = state.searchMode === 'server' && state.remotePlace
    ? state.searchResults
    : state.container
      ? containerEntries.value
      : state.entries;
  const q = state.query.trim().toLowerCase();
  // 只有本地列表按用户选的字段排；远程服务端搜索结果保持服务端给的相关度顺序
  const sorted = state.searchMode === 'server' && state.remotePlace ? source : sortEntries(source);
  if (!q) return sorted;
  return sorted.filter((e) => {
    // 锁定的加密文件与加密目录都没有可搜的名字，搜索时直接排除：
    // 用磁盘名去匹配等于拿 base32 密文当明文搜，还会泄露信息
    if ((e.is_encrypted || e.is_encrypted_dir) && !e.unlocked) return false;
    const name = e.real_name || e.name;
    return name.toLowerCase().includes(q);
  });
});

/** 当前视图里的条目总数（状态栏用）。
 *
 * 不能直接用 `state.entries.length`：在容器里那是**外层磁盘目录**的
 * 数量，与眼前列的东西无关。
 */
export const currentCount = computed(() =>
  state.container ? containerEntries.value.length : state.entries.length,
);

/** 当前目录里加密内容的数量。容器内恒为 0：里面的东西已经解出来了。
 *
 * 加密**目录**也要算：一个只含树形加密目录的文件夹否则会显示
 * 「0 个加密文件」，而用户眼前明明有一个带锁的加密文件夹。
 */
export const encryptedCount = computed(() =>
  state.container
    ? 0
    : state.entries.filter((e) => e.is_encrypted || e.is_encrypted_dir).length,
);

/** 其中还锁着的数量。加密目录同样计入，理由见 `encryptedCount`。 */
export const lockedCount = computed(() =>
  state.container
    ? 0
    : state.entries.filter(
        (e) => (e.is_encrypted || e.is_encrypted_dir) && !e.unlocked,
      ).length,
);

/** 选中项里可加密的（排除已经是加密文件的）。
 *
 * 容器内恒为空：那些条目没有磁盘路径，加密命令收的是路径。
 * 在这里返回空数组，「加密」按钮自然就不出现——比让按钮出现
 * 然后点了报错要好。
 */
export const encryptable = computed(() =>
  state.container
    ? []
    : state.selected
        .map((p) => state.entries.find((e) => e.path === p))
        .filter((e) => e && !e.is_encrypted),
);

/** 选中项里可以还原到磁盘的（已加密且已解锁的）。
 *
 * 与 `encryptable` 互补：一个挑还没加密的，一个挑已经加密的。
 *
 * 为什么要求 `unlocked`：还原需要密钥，而密钥来自会话。锁着的文件
 * 即使选中了也解不开，让按钮出现然后点了报 `locked` 不如不出现。
 *
 * 容器内恒为空：里面的条目没有独立的文件 id，还原命令收的是 id。
 * 真要取出容器里的单个文件，是另一件事（预览面板的「导出」），
 * 不该混进这个批量入口。
 */
export const restorable = computed(() =>
  state.container
    ? []
    : state.selected
        .map((p) => state.entries.find((e) => e.path === p))
        .filter((e) => e && e.is_encrypted && e.unlocked),
);

/** 选中项里可以管理密码的那**一个**。
 *
 * 只在恰好选中一个已解锁的加密文件时返回它，否则返回 null。
 *
 * # 为什么不支持批量
 *
 * 每个文件的 vault salt 可能不同，同一个密码在不同文件上派生出的 KEK
 * 就不同；而「这个文件配了几个密码」查不出来，批量操作时无法逐个确认
 * 结果。真要批量改，一半成功一半失败时用户手里就是一堆状态不明的文件。
 *
 * # 为什么要求已解锁
 *
 * 改密码必须先能打开这个文件（要取出 FEK 重新包裹）。锁着的文件先走
 * 正常的解锁流程，解开后这个入口自然出现——而不是在密码管理对话框里
 * 再套一层解锁。
 *
 * 容器内的条目恒为空：它们没有独立的磁盘文件，密码在外层容器上。
 *
 * # 树形加密的目录也算
 *
 * 加密目录的 `is_encrypted` 恒为 false（那个字段的语义是「内容是密文」，
 * 目录的内容是子项），要看 `is_encrypted_dir`。只判断前者的话，选中一个
 * 加密目录时 🔑 按钮压根不出现，用户得不到任何提示。
 */
export const keyManageable = computed(() => {
  if (state.container) return null;
  if (state.selected.length !== 1) return null;
  const e = state.entries.find((x) => x.path === state.selected[0]);
  if (!e || !e.unlocked) return null;
  return e.is_encrypted || e.is_encrypted_dir ? e : null;
});

/** 当前可管理密码的目标是不是树形加密的目录。
 *
 * 决定对话框只给 change：一棵树同时只能有一个密码（目录名由第一个 KEK
 * 派生），add 出来的第二个密码能开文件却解不开目录名，解密会报「文件
 * 损坏」。
 */
export const keyTargetIsTree = computed(() => !!keyManageable.value?.is_encrypted_dir);

/** 能不能给它生成恢复码：已解锁的单个加密文件或加密文件夹。
 *
 * 目录曾经被排除，理由是「树里每个文件各自挂槽，共用一份还是各一份
 * 未定」。现在定了：整棵树共用一份，由 rekey_tree 统一改写。
 * 「整棵树共用一份还是各一份」还没定，后端会如实拒绝。既然做不到，
 * 菜单项就该是灰的——让用户点进去再看到报错，不如一开始就说清楚。
 */
export const recoveryGeneratable = computed(() => {
  const e = keyManageable.value;
  return e;
});

/** 能不能对它使用恢复码：单个加密文件或加密文件夹，锁着或开着都行。
 *
 * 这里刻意**不要求** unlocked，而 keyManageable 要求。原因是这两个
 * 功能面对的处境正好相反：管理密码的前提是你还记得密码，而用恢复码
 * 的前提是你已经忘了。若照抄 keyManageable 的判据，菜单项只在文件
 * 已解锁时才亮——那时用户根本不需要它，真正需要的时候反而是灰的。
 */
export const recoveryUsable = computed(() => {
  if (state.container) return null;
  if (state.selected.length !== 1) return null;
  const e = state.entries.find((x) => x.path === state.selected[0]);
  if (!e || (!e.is_encrypted && !e.is_encrypted_dir)) return null;
  return e;
});

/* ---------------- 右键菜单 ---------------- */

/** 右键菜单的状态。`entry` 为 null 表示菜单没开。 */
export const ctxMenu = reactive({ entry: null, x: 0, y: 0 });

export function openContextMenu({ entry, x, y }) {
  ctxMenu.entry = entry;
  ctxMenu.x = x;
  ctxMenu.y = y;
}

export function closeContextMenu() {
  ctxMenu.entry = null;
}

/** 当前右键菜单该显示哪些项。
 *
 * 用 disabled 而不是直接隐藏：菜单项的位置固定，用户才能形成肌肉记忆。
 * 每次右键都换一套项目、位置浮动，会让人反复找「删除在哪一行」。
 * 不可用的原因写进 hint，鼠标悬停能看到——否则用户只知道点不动，
 * 不知道为什么。
 *
 * 容器内的条目全部不可操作：它们没有独立的磁盘文件，删一个「条目」
 * 得重写整个容器，这不是右键菜单该做的事。
 */
export const ctxItems = computed(() => {
  const e = ctxMenu.entry;
  if (!e) return [];
  const t = i18n.t;
  const inContainer = !!state.container;
  const many = state.selected.length > 1;

  // 容器内只留一项说明，不给任何会失败的操作
  if (inContainer) {
    return [
      {
        key: 'noop-container',
        icon: 'ℹ️',
        label: t('ctx.in_container'),
        disabled: true,
        hint: t('ctx.in_container_hint'),
      },
    ];
  }

  const items = [];

  // 打开：目录进去，文件预览
  items.push({
    key: 'open',
    icon: e.is_dir ? '📂' : '👁️',
    label: e.is_dir ? t('ctx.open_folder') : t('ctx.preview'),
    disabled: many,
    hint: many ? t('ctx.single_only') : '',
  });
  // 普通磁盘文件可显式重新选择应用；加密文件和目录没有可交给系统的明文文件。
  if (!e.is_dir && !e.is_encrypted) {
    items.push({
      key: 'open-with',
      icon: '↗',
      label: t('file.open_external'),
      disabled: many || !e.token,
      hint: many ? t('ctx.single_only') : '',
    });
  }

  // “上传至”只接受真实磁盘文件；目录与容器内虚拟条目没有可直接上传的对象。
  const uploadEntries = state.selected
    .map((path) => state.entries.find((entry) => entry.path === path))
    .filter((entry) => entry && !entry.is_dir);
  items.push({
    key: 'upload-to',
    icon: '⬆️',
    label: t('upload_to.menu'),
    disabled: uploadEntries.length !== state.selected.length || uploadEntries.length === 0,
    hint: uploadEntries.length ? '' : t('upload_to.files_only'),
  });

  items.push({ key: 'sep' });

  // 「永远用不上」的项不出现，「暂时用不了」的项禁用并说明原因。
  //
  // 菜单项位置固定是为了肌肉记忆（见 PROGRESS「交互上的两条防误删」），但
  // 普通文件的菜单里摆四个永远是灰的密码 / 恢复码项，手机底部弹层因此长到
  // 13 项、删除被挤出屏幕。按选中项的类型裁掉不可能适用的那一组：同一类
  // 条目的菜单仍然一模一样，肌肉记忆不受影响。
  const selEntries = state.selected
    .map((path) => state.entries.find((x) => x.path === path))
    .filter(Boolean);
  const pool = selEntries.length ? selEntries : [e];
  const anyEncrypted = pool.some((x) => x.is_encrypted || x.is_encrypted_dir);
  const anyPlain = pool.some((x) => !x.is_encrypted && !x.is_encrypted_dir);

  const canEncrypt = encryptable.value.length > 0;
  if (anyPlain) {
    items.push({
      key: 'encrypt',
      icon: '🔒',
      label: t('file.encrypt'),
      disabled: !canEncrypt,
      hint: canEncrypt ? '' : t('ctx.already_encrypted'),
    });
  }

  if (anyEncrypted) {
    // 还原：已解锁的加密文件
    const canRestore = restorable.value.length > 0;
    items.push({
      key: 'restore',
      icon: '📤',
      label: t('file.restore'),
      disabled: !canRestore,
      hint: canRestore ? '' : t('ctx.restore_needs_unlocked'),
    });

    // 密码管理：必须是**已解锁的单个**加密文件
    const canKey = !!keyManageable.value;
    items.push({
      key: 'manage-key',
      icon: '🔑',
      label: t('keymgmt.title'),
      disabled: !canKey,
      hint: canKey ? '' : t('ctx.key_needs_unlocked_file'),
    });

    // 恢复码：生成要已解锁（得先证明你现在能打开），使用则不要求——
    // 会用到它正是因为密码已经忘了
    const canGenReco = !!recoveryGeneratable.value;
    items.push({
      key: 'recovery-generate',
      icon: '🔐',
      label: t('recovery.menu_generate'),
      disabled: !canGenReco,
      hint: canGenReco ? '' : t('ctx.recovery_needs_unlocked_file'),
    });

    const canUseReco = !!recoveryUsable.value;
    items.push({
      key: 'recovery-restore',
      icon: '🔓',
      label: t('recovery.menu_restore'),
      disabled: !canUseReco,
      hint: canUseReco ? '' : t('ctx.recovery_needs_file'),
    });
  }

  items.push({ key: 'sep' });

  // 重命名：密文目录不行——名字本身就是密文，改掉就再也解不开了
  const isEncDir = !!e.is_encrypted_dir;
  items.push({
    key: 'rename',
    icon: '✏️',
    label: t('ctx.rename'),
    disabled: many || isEncDir,
    hint: isEncDir
      ? t('ctx.rename_encrypted_dir')
      : many
        ? t('ctx.single_only')
        : '',
  });

  items.push({
    key: 'newfolder',
    icon: '📁',
    label: t('ctx.new_folder'),
  });

  items.push({ key: 'sep' });

  // Android 没有系统回收站，后端会报 trash_not_supported。禁用并说明原因，
  // 而不是留一个点了就报错的项——那会让紧挨着的「永久删除」看起来是唯一出路
  items.push({
    key: 'trash',
    icon: '🗑️',
    label: t('ctx.trash'),
    disabled: !trashSupported,
    hint: trashSupported ? '' : t('ctx.trash_unsupported'),
  });
  items.push({ key: 'delete', icon: '⛔', label: t('ctx.delete'), danger: true });

  items.push({ key: 'sep' });

  // 「在文件管理器中显示」只接受后端扫描时登记的受控 token。
  // 文件与目录都应有 token；若旧数据或异常条目缺失，直接禁用而不是点后静默无反应。
  const cannotReveal = many || !e.token;
  items.push({
    key: 'reveal',
    icon: '📍',
    label: t('ctx.reveal'),
    disabled: cannotReveal,
    hint: many ? t('ctx.single_only') : '',
  });

  return items;
});

/* ---------------- 常规文件操作 ---------------- */

/** 删除选中项。`toTrash` 为 false 是永久删除。
 *
 * 作用于**整个选中集**而不只是右键点的那一项：右键前已经先选中了，
 * 用户看到几个高亮就期望删掉几个。
 */
export async function doDelete(toTrash) {
  const paths = state.selected.slice();
  if (!paths.length) return null;
  state.busy = true;
  try {
    const r = await api.deletePaths(paths, toTrash);
    state.selected = [];
    await reload();
    if (r.failed.length) {
      // 部分失败要说清哪些没删掉。只报「删除失败」的话，用户不知道
      // 20 个里有 19 个已经删了
      state.error = i18n.t('ctx.delete_partial', {
        ok: String(r.deleted.length),
        bad: String(r.failed.length),
      });
    } else {
      setNotice(
        i18n.t(toTrash ? 'ctx.trashed_n' : 'ctx.deleted_n', {
          n: String(r.deleted.length),
        }),
      );
    }
    return r;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return null;
  } finally {
    state.busy = false;
  }
}

/** 重命名一个条目。 */
export async function doRename(path, name) {
  state.busy = true;
  try {
    await api.renamePath(path, name);
    state.selected = [];
    await reload();
    setNotice(i18n.t('ctx.renamed'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 在当前目录下新建文件夹。 */
export async function doCreateFolder(name) {
  if (!state.cwd) return false;
  state.busy = true;
  try {
    await api.createFolder(state.cwd, name);
    await reload();
    setNotice(i18n.t('ctx.folder_created'));
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e));
    return false;
  } finally {
    state.busy = false;
  }
}

/* ---------------- 容器（加密文件夹）浏览 ---------------- */

/** 进入一个加密文件夹，像打开普通文件夹那样。
 *
 * 拿到的是**扁平**的全路径列表，之后由 `visibleEntries` 按当前层级过滤。
 * 一次取全而不是每层问一次：索引本来就是整份解出来的，分层请求只会
 * 让每次进目录都重解一遍容器头部。
 */
export async function enterContainer(entry) {
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.error = '';
  try {
    const items = await api.listContainer(entry.entry_id);
    state.container = {
      entryId: entry.entry_id,
      name: entry.real_name || entry.name,
      items,
      cwd: '',
    };
    state.selected = [];
    return true;
  } catch (e) {
    state.error = i18n.te(api.errCode(e), i18n.te('container_failed'));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 进入一个**远程**目录容器。
 *
 * 与本地 `enterContainer` 同构，区别只在拿条目的方式：本地读磁盘，
 * 这里要先 `remotePlaceOpen` 拿句柄（容器明文不在磁盘上）。
 *
 * 复用同一个 `state.container`，于是面包屑、返回、列表渲染全都不用分叉——
 * 另起一套的话「本地修了远程还是老样子」是迟早的事。
 */
export async function enterRemoteContainer(f) {
  if (!state.remotePlace) return false;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.placeError = '';
  try {
    const opened = await api.remotePlaceOpen(
      state.remotePlace,
      (f as { id: string; size?: number }).id,
      (f as { id: string; size?: number }).size || 0,
      undefined,
      (f as { name?: string }).name || null,
    );
    if (!opened || !opened.token) {
      state.placeError = i18n.te('container_failed');
      return false;
    }
    const items = await api.remoteListContainer(opened.token);
    state.container = {
      entryId: f.id,
      name: f.real_name || f.name,
      items,
      cwd: '',
      // 记下来源：容器内条目的预览要走 /pcitem/ 而不是 /citem/，
      // 少了这一位前端会拼出本地那条 URL，表现是点开里面的文件一片空白
      remote: true,
      fileToken: opened.token,
    };
    state.selected = [];
    return true;
  } catch (e) {
    state.placeError = i18n.te(api.errCode(e), i18n.te('container_failed'));
    return false;
  } finally {
    state.busy = false;
  }
}

/** 在容器内进入一个子目录。 */
export function enterContainerDir(path) {
  if (!state.container) return;
  state.container.cwd = path;
  state.selected = [];
}

/** 离开容器，回到它所在的磁盘目录。 */
export function leaveContainer() {
  state.container = null;
  state.selected = [];
}

/** 把容器内条目转成主列表认识的形状。
 *
 * # 为什么要适配而不是让 EntryCard 认识第二种结构
 *
 * 卡片和列表行只需要「名字、大小、是不是目录、图标线索」这几件事。
 * 与其在模板里到处加 `v-if="isContainerItem"`，不如在这里补齐字段——
 * 那种分支写法正是「改了一个分支忘了另一个」的温床。
 *
 * `path` 用容器内的相对路径：主列表用它做 `:key` 和选中标识，只要在
 * 当前视图里唯一就够，不需要是磁盘路径。
 */
function adaptContainerItem(it) {
  return {
    path: it.path,
    name: it.name,
    is_dir: it.is_dir,
    size: it.size,
    // 容器内的条目已经在解密后的视野里了，不是「一个加密文件」——
    // 标成 is_encrypted 会让卡片显示锁图案并要求再输一次密码
    is_encrypted: false,
    unlocked: true,
    real_name: null,
    entry_id: null,
    ext: it.name.includes('.') ? it.name.split('.').pop().toLowerCase() : null,
    // token 走 /citem/ 而不是 /plain/，两者不能混：前者要密钥，
    // 后者是磁盘明文。`in_container` 就是给预览层区分用的
    token: it.token,
    preview: it.kind,
    mime: it.mime,
    is_container: false,
    in_container: true,
  };
}

/** 容器内当前层级的直接子项。
 *
 * 容器索引是扁平的全路径列表，这里按 `cwd` 过滤出直接子项——
 * 否则进根目录会把所有层级的文件一股脑铺平列出来。
 */
const containerEntries = computed(() => {
  const c = state.container;
  if (!c) return [];
  const prefix = c.cwd ? `${c.cwd}/` : '';
  const out = [];
  for (const it of c.items) {
    if (!it.path.startsWith(prefix)) continue;
    const rest = it.path.slice(prefix.length);
    if (!rest || rest.includes('/')) continue; // 只要直接子项
    out.push(adaptContainerItem(it));
  }
  // 目录在前、各自自然序——与后端 `list_dir` 对磁盘目录的约定一致。
  // 两处都要排是因为容器索引的顺序是打包时的字节序，不是显示序
  return out.sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    return a.name.localeCompare(b.name, undefined, { numeric: true });
  });
});

/* ---------------- 导航 ---------------- */

/** 打开一个目录。 */
/** 切换网格/列表视图，并把选择持久化为「默认视图」。
 *
 * 界面上的切换若只改内存，重启就回到网格——设置页那个「默认视图」选项
 * 也就形同虚设。这里在切换后读改写一次配置；保存失败不阻断本次切换。
 */
export async function setView(v) {
  if (v !== 'grid' && v !== 'list') return;
  state.view = v;
  try {
    const c = await api.configGet();
    if (c.ui) {
      c.ui.view = v;
      await api.configSet(c);
    }
  } catch {
    // 偏好存不下时本次切换仍生效，只是不跨重启
  }
}

/** 记住最后浏览的本地目录，去抖落盘。
 *
 * 连续进入多级目录（双击进入、上级、面包屑）会在短时间内触发多次 navigate，
 * 每次都读改写配置既浪费也可能乱序，去抖后只记最后停留的目录。
 */
let lastDirTimer: number | null = null;
function schedulePersistLastDir(dir) {
  if (lastDirTimer) window.clearTimeout(lastDirTimer);
  lastDirTimer = window.setTimeout(() => {
    lastDirTimer = null;
    api.configGet().then((c) => {
      if (c.ui && c.ui.last_dir !== dir) {
        c.ui.last_dir = dir;
        return api.configSet(c);
      }
      return undefined;
    }).catch(() => { /* 记不住不影响本次浏览 */ });
  }, 800);
}

/** 启动时按「启动时打开」配置恢复起始目录。
 *
 * last：回到上次最后浏览的目录；home：进主目录；其余（询问/指定）不导航。
 * 目录已失效或未授权时 navigate 自身会给出可关闭的错误条，不阻断启动。
 */
export async function restoreStartupDir() {
  try {
    const c = await api.configGet();
    const mode = c.ui?.startup || 'last';
    if (mode === 'last' && c.ui?.last_dir) {
      await navigate(c.ui.last_dir);
    } else if (mode === 'home') {
      const home = state.places.find((p) => p && p.name === 'home');
      if (home && home.path) await navigate(home.path);
    }
  } catch {
    // 配置读不出就停在空白起始页，不影响启动
  }
}

/** 本地浏览的代号：每次 navigate +1。与远程浏览的 browseSeq 同一个道理。 */
let navSeq = 0;

export async function navigate(dir) {
  // 快速连点（双击进目录后立刻点面包屑）时两次读目录并发进行，先发的可能后回。
  // 不比对代号的话，晚到的旧结果会把新目录覆盖掉：界面停在用户已经离开的目录，
  // 而且先结束的那次会提前关掉加载态
  const seq = ++navSeq;
  const stale = () => seq !== navSeq;
  state.busy = true;
  state.busyKey = 'busy.loading';
  state.error = '';
  try {
    const entries = await api.browseDirectory(dir);
    if (stale()) return;
    state.entries = entries;
    state.cwd = dir;
    state.selected = [];
    // 进磁盘目录就意味着不在容器里了。不清的话面包屑会同时显示
    // 磁盘路径和容器层级，点哪个都对不上
    state.container = null;
    await refreshKnown();
    schedulePersistLastDir(dir);
  } catch (e) {
    if (stale()) return;
    state.error = i18n.te(api.errCode(e), i18n.t('errors.load_failed'));
  } finally {
    if (!stale()) state.busy = false;
  }
}

export const canGoUp = computed(() => {
  if (state.container) return true;
  const cwd = String(state.cwd || '');
  if (!cwd) return false;
  // Android/Unix 的根目录只有斜杠；Windows 盘符根可能是 C:、C:\ 或 C:/。
  // 返回桥必须同步告诉原生层是否已处理，不能临时 await parent_of 再决定。
  if (/^[\\/]+$/.test(cwd) || /^[A-Za-z]:[\\/]*$/.test(cwd)) return false;
  return true;
});

/** 回到上一级。
 *
 * 在容器里时先在容器内部往上走，走到容器根再退出容器——
 * 与普通文件夹的层级感受一致。
 */
export async function goUp() {
  if (state.container) {
    const cwd = state.container.cwd;
    if (!cwd) {
      leaveContainer();
      return;
    }
    const i = cwd.lastIndexOf('/');
    enterContainerDir(i < 0 ? '' : cwd.slice(0, i));
    return;
  }
  if (!state.cwd) return;
  const parent = await api.parentOf(state.cwd).catch(() => null);
  if (parent) await navigate(parent);
}

/** 刷新当前位置。
 *
 * 在容器里就重新取一次容器索引（同时刷新 token），而不是退回磁盘目录。
 * 刷新不该改变用户所在的位置。
 */
export async function reload() {
  if (state.container) {
    const { entryId, name, cwd } = state.container;
    const items = await api.listContainer(entryId).catch(() => null);
    if (items) {
      state.container = { entryId, name, items, cwd };
    } else {
      // 取不到通常意味着已经锁定或文件不在了，退回磁盘目录比
      // 停在一个再也刷不出内容的位置要好
      leaveContainer();
      if (state.cwd) await navigate(state.cwd);
    }
    return;
  }
  if (state.cwd) await navigate(state.cwd);
}

/** 载入侧栏起点。 */
export async function loadPlaces() {
  state.places = await api.listPlaces().catch(() => []);
}

/** 查询存储访问权限状态。
 *
 * 查询失败时保持原值不动：把它当成「没权限」会让桌面端也弹出授权提示，
 * 而那里根本没有可申请的东西。
 */
export async function loadStorageAccess() {
  const s = await api.storageAccess().catch(() => null);
  if (s) state.storage = s;
}

/** 申请存储访问权限，成功后重新载入侧栏。
 *
 * 安卓上这会跳到系统设置页，用户回来后 Promise 才 resolve。
 * 必须重新 loadPlaces：拿到权限后可列的位置完全不同，不刷新的话
 * 侧栏还是那两个沙箱目录，用户以为授权没生效。
 */
export async function grantStorageAccess() {
  const s = await api.requestStorageAccess().catch((e) => {
    state.error = i18n.te(e?.code, e?.message);
    return null;
  });
  if (!s) return false;
  state.storage = s;
  if (s.granted) await loadPlaces();
  return s.granted;
}

/** 面包屑的各段。
 *
 * 段的形状是 `{ name, path, kind }`：`kind` 为 `dir` 表示磁盘目录，
 * `container` 表示容器本身，`inner` 表示容器内的子目录。点击时按 kind
 * 分派——三者的「跳转」是三件不同的事，靠路径字符串猜会出错
 * （容器内路径和相对磁盘路径长得一样）。
 *
 * 在容器里时，磁盘路径那几段仍然保留在前面：用户需要知道这个加密
 * 文件夹是从哪儿打开的，而且点它能回去。
 */
/** 侧栏位置的显示名：常用目录按语义翻译，可移除卷用系统给的名字。
 *  侧栏与面包屑共用——两处各写一份的话，同一个目录在两处叫法不同。 */
export function placeLabel(place: { name: string; real_name?: string | null }): string {
  if (place.real_name) return place.real_name;
  const known = [
    'home', 'desktop', 'documents', 'downloads', 'pictures', 'videos', 'cache', 'camera',
    'documents_shared', 'movies', 'music', 'internal_storage', 'removable',
  ];
  return known.includes(place.name) ? i18n.t(`places.${place.name}`) : place.name;
}

/** 去掉末尾分隔符（根目录 `/`、`C:\` 保持原样）。 */
function trimSep(p: string): string {
  const t = p.replace(/[\\/]+$/, '');
  return t || p;
}

/** cwd 落在哪个侧栏位置之下（取最长的那个前缀）。 */
function placeRootOf(cwd: string) {
  let best = null;
  for (const pl of state.places || []) {
    if (!pl?.path) continue;
    const root = trimSep(pl.path);
    // 根目录本身（`/`、`C:\`）不合并：那一段本来就短，合并只会丢掉盘符
    if (root.length <= 3) continue;
    const hit = cwd === root || cwd.startsWith(root + '/') || cwd.startsWith(root + '\\');
    if (hit && (!best || root.length > trimSep(best.path).length)) best = pl;
  }
  return best;
}

export const crumbs = computed(() => {
  const out = [];
  if (state.cwd) {
    // Windows 用反斜杠，Unix 用正斜杠。分割后重新拼接成可点击的路径
    const sep = state.cwd.includes('\\') ? '\\' : '/';
    // 落在某个侧栏位置之下时，那一段前缀合成一个带名字的段：Android 上原来
    // 显示成「storage › emulated › 0 › Download」，手机上一行放不下，当前目录
    // 被挤到屏幕外；桌面上主目录之下也少了一串用户名
    const root = placeRootOf(state.cwd);
    let acc = '';
    let rest = state.cwd;
    if (root) {
      acc = trimSep(root.path);
      out.push({ name: placeLabel(root), path: root.path, kind: 'dir' });
      rest = state.cwd.slice(acc.length);
    }
    const parts = rest.split(/[\\/]/).filter(Boolean);
    for (const [i, p] of parts.entries()) {
      if (i === 0 && !root) {
        // Windows 的 "C:" 要补上分隔符才是合法路径；
        // Unix 的第一段前面要加根斜杠
        acc = sep === '\\' ? `${p}${sep}` : `${sep}${p}`;
      } else {
        acc = `${acc}${acc.endsWith(sep) ? '' : sep}${p}`;
      }
      out.push({ name: p, path: acc, kind: 'dir' });
    }
  }
  const c = state.container;
  if (c) {
    out.push({ name: c.name, path: '', kind: 'container' });
    if (c.cwd) {
      const parts = c.cwd.split('/').filter(Boolean);
      for (const [i, p] of parts.entries()) {
        out.push({
          name: p,
          path: parts.slice(0, i + 1).join('/'),
          kind: 'inner',
        });
      }
    }
  }
  return out;
});

/** 点击面包屑的某一段。 */
export async function gotoCrumb(c) {
  if (c.kind === 'dir') {
    await navigate(c.path);
    return;
  }
  if (c.kind === 'container') {
    enterContainerDir('');
    return;
  }
  enterContainerDir(c.path);
}

/* ---------------- 选择 ---------------- */

/** 区间选择的起点：最近一次普通点击或 Ctrl 点击的条目。 */
let selectAnchor: string | null = null;

/** 切换单项选中。 */
export function toggleSelect(path, additive) {
  selectAnchor = path;
  if (!additive) {
    state.selected = state.selected.includes(path) && state.selected.length === 1 ? [] : [path];
    return;
  }
  const i = state.selected.indexOf(path);
  if (i >= 0) state.selected.splice(i, 1);
  else state.selected.push(path);
}

/** Shift 点击：选中从起点到这一项之间的全部（按当前显示顺序）。
 *
 * 原来 Shift 与 Ctrl 一样只是加选一项，想选一段连续文件只能逐个点。
 * 没有起点时退化为单选。 */
export function selectRange(path) {
  const list = visibleEntries.value.map((e) => e.path);
  const a = selectAnchor ? list.indexOf(selectAnchor) : -1;
  const b = list.indexOf(path);
  if (a < 0 || b < 0) {
    toggleSelect(path, false);
    return;
  }
  const [lo, hi] = a < b ? [a, b] : [b, a];
  state.selected = list.slice(lo, hi + 1);
}

/** 键盘移动「当前项」：选中相邻的条目（Shift 时扩展区间）。返回新的当前项路径。 */
export function moveSelection(delta: number, extend: boolean) {
  const list = visibleEntries.value.map((e) => e.path);
  if (!list.length) return null;
  const cur = state.selected.length ? state.selected[state.selected.length - 1] : null;
  const i = cur ? list.indexOf(cur) : -1;
  const j = Math.min(list.length - 1, Math.max(0, i < 0 ? 0 : i + delta));
  const target = list[j];
  if (extend) {
    if (!selectAnchor) selectAnchor = cur || target;
    const a = list.indexOf(selectAnchor);
    const [lo, hi] = a < j ? [a, j] : [j, a];
    state.selected = list.slice(lo, hi + 1);
    // 把「当前项」放到末尾，下一次移动从它出发
    state.selected = state.selected.filter((p) => p !== target).concat(target);
  } else {
    selectAnchor = target;
    state.selected = [target];
  }
  return target;
}

/** 全选当前可见项。 */
export function selectAll() {
  state.selected = visibleEntries.value.map((e) => e.path);
}

/** 清空选择。 */
export function clearSelection() {
  state.selected = [];
}
