/** 与 Rust 后端的全部通信。
 *
 * # 为什么改用官方包
 *
 * 之前手写了一个 `invoke`，直接读 `window.__TAURI_INTERNALS__`。
 * 那是没有构建步骤时的无奈之举——引包需要 npm。现在有了打包步骤，
 * 用官方 `@tauri-apps/api` 更稳：Tauri 升级时注入形态若有变化，
 * 由包自己适配，不需要我们跟着改兼容分支。
 *
 * # 错误约定
 *
 * 后端只返回错误**码**不返回文案（见 `commands.rs`），
 * 由前端按当前语言翻译。所以这里原样抛出，翻译在调用处用
 * `i18n.te(e.code)` 完成。
 */

import { invoke } from '@tauri-apps/api/core';

/** 从后端错误里取出错误码。
 *
 * Tauri 把 `Err(E)` 序列化后传过来，形态取决于 E 的 Serialize 实现。
 * 这里统一成字符串码，取不到时返回 undefined 让调用方走兜底文案。
 */
export function errCode(e) {
  if (typeof e === 'string') return e;
  if (e && typeof e === 'object' && typeof e.code === 'string') return e.code;
  return undefined;
}

/* ---------------- 文件浏览（不需要密码） ---------------- */

/** 列出一个目录的内容。目录 + 普通文件 + 加密文件，一次一层。 */
export const browseDirectory = (dir) => invoke('browse_directory', { dir });

/** 侧栏的起点：常用目录与磁盘根。 */
export const listPlaces = () => invoke('list_places');

/** 上一级目录。已在根时返回 null。 */
export const parentOf = (path) => invoke('parent_of', { path });

/** 探测单个文件是不是加密文件、当前会话能否打开。 */
export const probeOne = (path) => invoke('probe_one', { path });

/* ---------------- 加密 ---------------- */

/** 加密一批路径。成功后密码会自动进入会话。 */
export const encryptPaths = (req) => invoke('encrypt_paths', { req });

/* ---------------- 会话 ---------------- */

/** 探测目录里有哪些 vault（每个有独立的 salt）。 */
export const vaultParamsOf = (dir) => invoke('vault_params_of', { dir });

/** 用一组 vault 参数派生密钥并解锁。 */
export const unlock = (label, password, vaults) =>
  invoke('unlock', { label, password, vaults });

/** 对一个目录直接试密码：探测 vault + 派生，一步到位。 */
export const unlockDirectory = (dir, password) =>
  invoke('unlock_directory', { dir, password });

/** 锁定。后端会清空文件列表与会话密钥。 */
export const lock = () => invoke('lock');

/** 当前是否已解锁。 */
export const isUnlocked = () => invoke('is_unlocked');

/** 已装载的凭据数量。 */
export const credentialCount = () => invoke('credential_count');

/* ---------------- 文件 ---------------- */

/** 扫描目录，返回文件列表（含锁定项）。 */
export const scanDirectory = (dir, recursive = true) =>
  invoke('scan_directory', { dir, recursive });

/** 取当前列表。 */
export const listFiles = () => invoke('list_files');

/** 补齐单个文件的媒体元信息。 */
export const enrichFile = (id) => invoke('enrich_file', { id });

/** 已扫描过的根目录。 */
export const listRoots = () => invoke('list_roots');

/* ---------------- 环境 ---------------- */

/** 自定义协议的 URL 前缀。各平台形式不同，必须问后端。 */
export const streamBase = () => invoke('stream_base');

/** 系统语言。 */
export const getLanguage = () => invoke('get_language');

/** 告知后端当前语言（影响后端产生的文案，如原生对话框标题）。 */
export const setLanguage = (lang) => invoke('set_language', { lang });

/* ---------------- 原生对话框 ---------------- */

/** 弹出目录选择器。用户取消时返回 null。 */
export const pickFolder = (title) => invoke('pick_folder', { title });

/** 弹出文件选择器。用户取消时返回 null。 */
export const pickFiles = (title) => invoke('pick_files', { title });

/* ---------------- 设备库 ---------------- */

/** 设备库状态。未打开时也能问——界面据此决定显示「设置」还是「输入」密码。 */
export const deviceStatus = () => invoke('device_status');

/** 打开或创建设备库。首次调用会创建本机身份。 */
export const openDeviceStore = (password) => invoke('open_device_store', { password });

/** 关闭设备库，抹掉内存里的身份。 */
export const closeDeviceStore = () => invoke('close_device_store');

/** 已配对设备列表。 */
export const pairedDevices = () => invoke('paired_devices');

/** 改本机设备名。这个名字会广播到局域网。 */
export const renameDevice = (name) => invoke('rename_device', { name });

/** 吊销一台设备。对方**下次连接**才会被拒绝。 */
export const revokeDevice = (fingerprint) => invoke('revoke_device', { fingerprint });

/* ---------------- 局域网 ---------------- */

/** 搜索局域网设备。 */
export const discoverDevices = (timeoutSecs = 4) =>
  invoke('discover_devices', { timeoutSecs });

/** 开始等待对方连入，立刻返回配对码。 */
export const pairListen = (port = 0, expiresDays = 0) =>
  invoke('pair_listen', { port, expiresDays });

/** 主动连接对方完成配对。 */
export const pairWith = (addr, pin, expiresDays = 0) =>
  invoke('pair_with', { addr, pin, expiresDays });

/** 查询配对进展。配对要等人操作，所以轮询这个。 */
export const pairStatus = () => invoke('pair_status');

/** 取消配对，配对码立即作废。 */
export const pairCancel = () => invoke('pair_cancel');

/* ---------------- 共享 ---------------- */

/** 开始共享一个目录。**不需要文件密码**：服务端只搬运密文。 */
export const startShare = (dir, port = 0, advertise = true) =>
  invoke('start_share', { dir, port, advertise });

/** 停止共享。会注销 mDNS 广播。 */
export const stopShare = () => invoke('stop_share');

/** 共享服务状态。 */
export const shareStatus = () => invoke('share_status');

/* ---------------- 连接远端 ---------------- */

/** 连接一台已配对设备。addr 留空时自动在局域网里找。 */
export const remoteConnect = (fingerprint, addr = null) =>
  invoke('remote_connect', { fingerprint, addr });

/** 断开远端连接。 */
export const remoteDisconnect = () => invoke('remote_disconnect');

/** 拉取远端文件列表。每次都重新拉——对方可能改了共享内容。 */
export const remoteList = () => invoke('remote_list');

/** 远端连接状态。 */
export const remoteStatus = () => invoke('remote_status');

/** 输入新密码后重新解析远端列表。不重新拉，省一次往返。 */
export const remoteRelock = () => invoke('remote_relock');

/** 远端文件里出现过的 vault 参数（已去重）。 */
export const remoteVaults = () => invoke('remote_vaults');
