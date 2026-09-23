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
import { listen } from '@tauri-apps/api/event';

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

/* ---------------- 存储权限（安卓） ---------------- */

/** 查询存储访问权限状态。
 *
 * 返回 `{ granted, mode }`。非安卓平台恒为
 * `{ granted: true, mode: 'not-applicable' }`——桌面端进程本来就能
 * 读写文件系统，前端不必为此分叉。
 */
export const storageAccess = () => invoke('storage_access');

/** 申请存储访问权限。
 *
 * 安卓 API 30+ 会跳到系统设置页，这个 Promise 要等到用户从设置页
 * 返回才 resolve，可能是几十秒。返回的状态是重新实查的结果。
 */
export const requestStorageAccess = () => invoke('request_storage_access');

/** 探测单个文件是不是加密文件、当前会话能否打开。 */
export const probeOne = (path) => invoke('probe_one', { path });

/* ---------------- 视频处理（加密之前） ---------------- */

/** 查这台机器上 FFmpeg 的实际能力。
 *
 * 返回 `{ video_encoders, audio_encoders, muxers, has_ffmpeg, has_ffprobe, version }`。
 *
 * **不要把结果写死或缓存到下次启动**：内置的那份 FFmpeg 没有任何视频
 * 编码器，但用户随时可以换成完整版（`OMY_FFMPEG` 优先级最高）。
 * `refresh` 为真时绕过后端缓存重新探测，供「我换了 FFmpeg」的场景。
 */
export const videoCapabilities = (refresh = false) =>
  invoke('video_capabilities', { refresh });

/** 探测一个未加密的本地视频，取容器 / 编码 / 分辨率 / 时长 / 播放分级。
 *
 * 传 token 而不是路径：路径会让任何注入的脚本都能读任意文件。
 */
export const videoInfo = (token) => invoke('video_info', { token });

/** 转封装或压缩一个视频，产出磁盘上的中间文件。
 *
 * `req` 形如 `{ token, container, faststart, encode }`，`encode` 为 null
 * 时只转封装（`-c copy`，秒级完成、零画质损失）。
 *
 * 产物是 `.omytmp-` 前缀的临时文件，**调用方有责任**在用完或取消后调
 * `discardConverted` 删掉它——那可能是几个 GB。
 */
export const convertVideo = (req) => invoke('convert_video', { req });

/** 删除转换产生的中间文件。
 *
 * 后端只删 `.omytmp-` 前缀的文件，所以这个接口不能被用来删任意文件。
 */
export const discardConverted = (token) => invoke('discard_converted', { token });

/* ---------------- 加密 ---------------- */

/** 加密一批路径。成功后密码会自动进入会话。 */
export const encryptPaths = (req) => invoke('encrypt_paths', { req });

/** 事件名必须与后端 `ENCRYPT_PROGRESS_EVENT` 完全一致。
 *
 * 写错一个字母不会有任何报错，只会表现成「进度条永远不动」，
 * 所以两边都用常量，并在此注明出处。
 */
export const ENCRYPT_PROGRESS_EVENT = 'encrypt://progress';

/** 订阅加密进度，返回取消订阅的函数。
 *
 * 载荷形如 `{ index, total_files, name, done, total }`。
 * `done` / `total` 都是**明文**字节：压缩后密文大小与明文脱节，
 * 按密文报会让进度条走得莫名其妙。
 */
export const onEncryptProgress = (handler) =>
  listen(ENCRYPT_PROGRESS_EVENT, (e) => handler(e.payload));

/* ---------------- 还原（解密到磁盘） ---------------- */

/** 把选中的加密文件还原到磁盘。
 *
 * `req` 形如 `{ ids, target_dir, overwrite }`：
 * - `ids` 是已登记文件的 id，不是路径——后端要靠 id 查会话里的凭据
 * - `target_dir` 传 null 表示还原到加密文件所在目录
 * - `overwrite` 默认 false，同名已存在时报 `target_exists`
 */
export const decryptPaths = (req) => invoke('decrypt_paths', { req });

/** 事件名必须与后端 `DECRYPT_PROGRESS_EVENT` 完全一致。理由同上。
 *
 * 与加密用不同的名字：两者可能先后发生，共用一个会让进度条
 * 分不清当前该显示「加密中」还是「还原中」。
 */
export const DECRYPT_PROGRESS_EVENT = 'decrypt://progress';

/** 订阅还原进度，返回取消订阅的函数。载荷字段与加密进度一致。 */
export const onDecryptProgress = (handler) =>
  listen(DECRYPT_PROGRESS_EVENT, (e) => handler(e.payload));

/* ---------------- 密码管理 ---------------- */

/** 给一个已加密文件增删改密码。
 *
 * `action` 取 `add` / `change` / `remove`。注意三者的语义都是「重新声明
 * 这个文件的密码集合」而不是「操作某一个 slot」——格式上无法探测哪个
 * slot 是空的，详见后端 `keymgmt.rs`。`remove` 不传 `next`（传了会报错）。
 *
 * 只改文件头，载荷一个字节都不动，所以再大的文件也是毫秒级。
 */
export const manageKey = (req) => invoke('manage_key', { req });

/**
 * 读取一个文件的槽位清单。
 *
 * 要密码：槽位目录是加密的。可否认模式下返回 managed=false 与空清单，
 * 那不是「读取失败」，是设计如此——界面据此显示说明而非清单。
 */
export const listSlots = (path, password) =>
  invoke('list_slots', { req: { path, password } });

/**
 * 这个位置能不能用 Windows Hello 免密解锁。
 *
 * 不会弹 Hello——用户只是打开界面，为此弹指纹很突兀。代价是它只回答
 * 「密文在不在」，不保证还解得开（清除 TPM 之后仍报 true）。
 */
export const deviceKeyStatus = (dir) => invoke('device_key_status', { dir });

/** 用设备密钥解锁。会弹 Hello。 */
export const deviceKeyUnlock = (dir) => invoke('device_key_unlock', { dir });

/** 给这个位置启用免密解锁。需要先验一次密码，之后会弹 Hello。 */
export const deviceKeyEnroll = (path, password) =>
  invoke('device_key_enroll', { path, password });

/** 关闭免密解锁。只清这台机器上保管的密钥，文件不动。 */
export const deviceKeyForget = (dir) => invoke('device_key_forget', { dir });

/**
 * 只重试上次失败的那些文件。
 *
 * `paths` 取自 `tree_partial` 错误里的 `paths`（完整路径，不是展示用的短名）。
 * `current` 要填**原来的**密码：失败的文件没被改写，还是旧密码——填新密码
 * 会得到 wrong_password。
 *
 * 不重跑整个操作，是因为重跑会拿旧密码去开已经改好的文件，产生一堆假失败。
 */
export const retryKeyFiles = (req) => invoke('retry_key_files', { req });

/**
 * 生成恢复码并挂到文件上。
 *
 * 返回的 26 个词**只用于当场显示**：不得写进 localStorage、不得留在任何
 * 超出对话框生命周期的状态里。我们后端不保存它的明文，用户错过就得重新
 * 生成一份（旧的随之作废）。
 */
export const generateRecovery = (req) => invoke('generate_recovery', { req });

/**
 * 用恢复码打开文件并设置新密码。
 *
 * 失败时错误码区分得很细：`bad_recovery_code` 的 params.detail 里带着
 * 「第 7 个词『acadmic』不在词表中，是不是『academic』？」这样的原文，
 * 而 `recovery_mismatch` 表示码本身没抄错、只是不属于这个文件。
 * 这两种处境的处置方式相反，界面不能压成同一句话。
 */
export const restoreWithRecovery = (req) => invoke('restore_with_recovery', { req });

/* ---------------- 会话 ---------------- */

/** 探测目录里有哪些 vault（每个有独立的 salt）。 */
export const vaultParamsOf = (dir) => invoke('vault_params_of', { dir });

/** 用一组 vault 参数派生密钥并解锁。 */
export const unlock = (label, password, vaults) =>
  invoke('unlock', { label, password, vaults });

/** 取远程位置当前目录里所有加密文件的 vault 参数。
 *
 * 远程没有本地目录可探测，所以不能走 `unlockDirectory`——
 * 那条路径要求 `state.cwd`，在远程位置里它是空的。
 */
export const remotePlaceVaults = (placeId, dir) =>
  invoke('remote_place_vaults', { placeId, dir });

/** 对一个目录直接试密码：探测 vault + 派生，一步到位。 */
export const unlockDirectory = (dir, password) =>
  invoke('unlock_directory', { dir, password });

/** 锁定。后端会清空文件列表与会话密钥。 */
export const lock = () => invoke('lock');

/** 当前是否已解锁。 */
export const isUnlocked = () => invoke('is_unlocked');

/** 已装载的凭据数量。 */
export const credentialCount = () => invoke('credential_count');

/* ---------------- 常规文件操作 ---------------- */

/** 删除文件或目录。`toTrash` 为 true 进系统回收站。 */
export const deletePaths = (paths, toTrash = true) =>
  invoke('delete_paths', { req: { paths, to_trash: toTrash } });

/** 重命名。返回新的完整路径。 */
export const renamePath = (path, name) =>
  invoke('rename_path', { req: { path, name } });

/** 在 parent 下新建文件夹。返回新目录的完整路径。 */
export const createFolder = (parent, name) =>
  invoke('create_folder', { req: { parent, name } });

/* ---------------- 文件 ---------------- */

/** 扫描目录，返回文件列表（含锁定项）。 */
export const scanDirectory = (dir, recursive = true) =>
  invoke('scan_directory', { dir, recursive });

/** 取当前列表。 */
export const listFiles = () => invoke('list_files');

/** 补齐单个文件的媒体元信息。 */
/** 列出目录容器里的条目。文件必须已解锁。 */
export const listContainer = (id) => invoke("list_container", { id });

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

/** 用系统默认程序打开一个未加密文件。
 *
 * 只接受后端登记过的 token，前端拿不到「打开任意路径」的能力——
 * 这也是没有引入 opener 插件的原因。
 */
export const openExternal = (token) => invoke('open_external', { token });

/** 在系统文件管理器里定位一个文件。加密文件也适用。 */
export const revealInFolder = (token) => invoke('reveal_in_folder', { token });

/* ---------------- 设置 ---------------- */

/** 读取整份配置。
 *
 * 一次取全部而不是逐项：设置页要显示十几项，逐项调用就是十几次 IPC。
 */
export const configGet = () => invoke('config_get');

/** 写回整份配置。
 *
 * 同样整份写：分项写回会出现「改了两项、第一项成功第二项失败」的
 * 半截状态，而配置文件本身是原子写的，整份回写反而更安全。
 */
export const configSet = (config) => invoke('config_set', { config });

/** 查询配置文件与缓存/数据目录的位置。
 *
 * 便携模式下用户需要知道拷走哪个目录能带走全部状态。
 */
export const configPaths = () => invoke('config_paths');

/** 上报一条用户操作到日志（仅传写死的动作标识，不传用户内容）。
 *  发了就算，失败也不影响操作——日志是辅助设施。 */
export const uiLog = (action, detail) => {
  try { invoke('ui_log', { action, detail: detail || null }); } catch { /* 忽略 */ }
};

/** 在系统文件管理器里打开日志目录（仅桌面端）。 */
export const openLogDir = () => invoke('open_log_dir');

/**
 * 应用版本与 OMYFILE 格式版本，供设置页「关于」显示。
 * 版本来自后端编译期常量，避免在前端写死后发版漏改。
 */
export const appAbout = () => invoke('app_about');

/* ---------------- 远程位置（WebDAV） ---------------- */

/** 添加一个 WebDAV 位置，返回其 id。
 *
 * 凭据只进后端，不留在前端——它们在 WebView 里没有任何用途，
 * 留着只是多一处泄露面。
 */
export const remotePlaceAdd = (p) =>
  invoke('remote_place_add', {
    name: p.name,
    url: p.url,
    username: p.username || '',
    password: p.password || '',
    vendor: p.vendor || 'generic',
    writable: !!p.writable,
  });

/** 列出已注册的远程位置。返回项含 `caps` 能力位图。 */
export const remotePlaceList = () => invoke('remote_place_list');

/** 移除一个远程位置。 */
export const remotePlaceRemove = (id) => invoke('remote_place_remove', { id });

/** 浏览远程目录，返回已识别加密状态的条目。
 *
 * 条目上的 `probe_failed` 与 `unlocked === false` 是**两回事**：
 * 前者是没读到（网络），后者是密码不对。界面必须分开显示，
 * 否则用户会对着网络故障反复试密码。
 */
export const remoteBrowse = (placeId, dir) =>
  invoke('remote_browse', { placeId, dir });

/** 查询某个远程目录下的**有效**能力（按目录，不是位置级的那个上界）。
 *
 * 位置级能力（`remotePlaceList` 返回项里的 `caps`）对「对话即目录」的位置只是
 * 上界：同一位置里有的目录可写、有的只读。照上界渲染会点亮必然失败的菜单项，
 * 所以进目录后要用这个命令收窄。失败时**按只读处理**，不要回落到上界。
 */
export const remoteEffectiveCaps = (placeId, dir) =>
  invoke('remote_effective_caps', { placeId, dir });

/** 重新探测远程目录里的单个文件（「未能读取」条目就地重试，不重载整屏）。 */
export const remoteProbeEntry = (placeId, id, size, name) =>
  invoke('remote_probe_entry', { placeId, id, size, name: name ?? null });

/**
 * 订阅「边扫边出」：remote_browse 先返回一屏骨架，后台每识别完一个文件
 * 就推一条 payload `{ place_id, dir, entry }`，前端就地替换同 id 骨架。
 * 返回 unlisten。
 */
export const onRemoteEntry = (handler) =>
  listen('remote-entry', (e) => handler(e.payload));

/** 把已解锁的远程 `.omy` 流式解密到本地目录（只读位置也保留的主要用途）。
 *  嵌套文件引用走 snake_case（与 EncryptRequest 等一致），顶层参数走 camelCase。 */
export const remoteDecryptToLocal = (placeId, path, size, destDir) =>
  invoke('remote_decrypt_to_local', {
    req: { place_id: placeId, path, size },
    destDir,
  });

/** 打开一个远程 `.omy`，换回播放令牌。
 *
 * 后端在此时读完整头部、用当前会话密钥试解，并构造带密文块缓存的来源。
 * 返回 `unlocked:false` 表示是 omy 但密码不对（区别于网络错误），
 * `not_encrypted:true` 表示根本不是 omy。
 */
/** 在服务端搜索。
 *
 * **这会把搜索词发送到服务端。** 与 remoteBrowse 分成两个函数而不是加一个
 * 参数，是为了让这件事在调用点上就看得见——合成一个再用 flag 区分的话，
 * 界面很容易在某条路径上忘了给提示，而用户不会知道自己搜的词出去了。
 *
 * 返回的是**候选集**：服务端搜的是消息文字与说明，omy 加密文件的真实文件名
 * 它永远没有。精筛要在本地按解出来的名字再做一轮。
 */
export const remoteSearch = (placeId, dir, query) =>
  invoke('remote_search', { placeId, dir, query });

export const remotePlaceOpen = (placeId, path, size, name) =>
  invoke('remote_place_open', {
    target: { placeId, path, size, name: name ?? null },
  });

/** 关闭一个远程播放来源（播放结束时调用）。 */
export const remotePlaceClose = (token) =>
  invoke('remote_place_close', { token });

/** 查询远程密文缓存用量 {used, limit, root}。 */
export const remoteCacheUsage = () => invoke('remote_cache_usage');

/** 立即清空远程密文缓存，返回清空后占用。 */
export const remoteCacheClear = () => invoke('remote_cache_clear');

/** 缓存设置变更后让后端按新上限/目录重建缓存。 */
export const remoteCacheApply = (limit, cacheDir) =>
  invoke('remote_cache_apply', { limit, cacheDir });

/** 在系统文件管理器中打开缓存目录（桌面端）。 */
export const remoteCacheOpenDir = () => invoke('remote_cache_open_dir');

/** 查单个远程文件的密文块缓存覆盖情况：
 *  {cached_blocks,total_blocks,cached_bytes,fully_cached}，不下载载荷。 */
export const remoteCacheFileStat = (placeId, path, size) =>
  invoke('remote_cache_file_stat', {
    req: { place_id: placeId, path, size },
  });

/** 列一个对话里的消息（以文件为主线的消息视图）。
 *
 * 只读：不发消息、不做回复关系 / 转发 / reactions / 已读状态。
 * 用途是让用户按时间线找文件——「我记得上周发过一个东西」。
 *
 * 广播频道没有这个视图（ToS 3.3 的 sponsored messages 要求），
 * 后端会回 `tg_broadcast_no_messages`。
 */
export const remoteMessages = (placeId, dir, before = null) =>
  invoke('remote_messages', { placeId, dir, before });

/** 以某条消息为中心拉取一段（引用跳转 / 定位源消息）。
 *  返回 { rows, oldest, newest, found }：rows 升序、两端游标供双向续翻、
 *  found 表示目标是否真的拉到了（拉不到别假装定位成功）。 */
export const remoteMessagesAround = (placeId, dir, around) =>
  invoke('remote_messages_around', { placeId, dir, around });

/** 列出一个远程目录容器里的条目。
 *
 * 参数是 `remotePlaceOpen` 颁发的播放句柄 token，不是位置 id + 路径：
 * 容器明文不在磁盘上、要按需解，而那个句柄背后挂着已建好的远程源与缓存。
 *
 * 只读头部，不下载载荷——几百 MB 的容器也能先「进去看看」。
 */
export const remoteListContainer = (token) =>
  invoke('remote_list_container', { token });

/** 把一个远程文件转为永久缓存。
 *
 * **这是一次真实的下载任务**：会把整个文件的密文块都取到本地，耗时与文件
 * 大小成正比。只打标记的话「永久」只是承诺不是事实——下次离线打开照样失败。
 */
export const remoteCachePin = (placeId, path, size, name) =>
  invoke('remote_cache_pin', {
    req: { place_id: placeId, path, size, name },
  });

/** 取消永久缓存。
 *
 * 内容会搬回临时层，也就是**重新计入上限、重新参与淘汰**，可能很快被清掉。
 * 那正是「取消永久」该有的语义；只改标记的话空间不会真的还回来。
 */
export const remoteCacheUnpin = (placeId, path, size) =>
  invoke('remote_cache_unpin', {
    req: { place_id: placeId, path, size },
  });

/** 上传一批本地文件到远程目录。
 *
 * 逐个返回成败：部分成功是常态，压成一个布尔的话用户不知道哪些传上去了，
 * 再传一次就产生重复文件。
 */
export const remoteUpload = (placeId, dir, paths) =>
  invoke('remote_upload', { placeId, dir, paths });

/** 删除单个远程文件的本地密文块，返回 {freed_bytes}；只读位置也允许。 */
export const remoteCacheRemoveFile = (placeId, path, size) =>
  invoke('remote_cache_remove_file', {
    req: { place_id: placeId, path, size },
  });

/* ---------------- Telegram 扫码登录 ---------------- */

/** 这台机器能不能安全保存 Telegram 登录态。
 *
 * 要在**开始扫码之前**问：答案为否时先告诉用户「这台机器上登录态存不住，
 * 每次启动都要重新扫一次」，而不是等他扫完了才说。
 */
export const telegramCanPersist = () => invoke('telegram_can_persist');

/** 列出所有被转为「永久保留」的文件。
 *
 * 返回 `PinnedFile[]`，字段 `place` / `key` / `total_blocks` / `used_bytes`。
 * `place` 就是位置 id（与 `remote_browse` 用的是同一个值），可以直接拿去
 * `remotePlaceList` 里查名字——但**查不到是正常情况**：位置被移除后这条
 * 永久记录仍在，界面要按「已移除的位置」显示，不能空着。
 *
 * `key` 含文件版本哈希，是缓存里的身份、不是能拿去浏览的路径，
 * 界面当成不透明标识即可。 */
export const remoteCacheListPinned = () => invoke('remote_cache_list_pinned');

/** 取消某个文件的永久保留并把它占的空间释放掉，返回释放的字节数。
 *
 * 按缓存键操作、不碰网络，所以离线时、甚至那个远程位置已经被删掉之后，
 * 依然能清。这里是直接删而不是搬回临时层：从「管理永久缓存」点取消的
 * 动机基本只有腾空间，搬回临时层磁盘一个字节都没少。 */
export const remoteCacheUnpinByKey = (place, key, totalBlocks) =>
  invoke('remote_cache_unpin_by_key', { place, key, totalBlocks });

/** 扫码登录时 session 的暂存账号名，必须与后端 `PENDING_ACCOUNT` 一致。
 *
 * 扫码**先于位置存在**：用户点「添加」时还没有位置 id，而 session 必须在
 * 登录成功那一刻立刻落盘（中途任何失败都会让这次已在服务端生效的登录白费，
 * 重新登录要再扫一次码、还可能撞 FLOOD_WAIT）。后端先落到这个固定名字下，
 * 等位置建好再改名过去。
 *
 * 它不会和真实位置撞名：位置 id 由注册表统一发放、形如 `p<N>`。 */
export const TG_PENDING_ACCOUNT = 'pending';

/** 当前全部传输任务（下载 / 上传 / 永久保留三类汇总）。 */
export const transferList = () => invoke('transfer_list');

/** 取消一条任务。执行体会在下一个分片边界干净退出。 */
export const transferCancel = (id) => invoke('transfer_cancel', { id });

/** 重试一条失败任务。只对可重试的失败有意义（磁盘满 / 认证失效不给按钮），
 *  重跑走断点续传：已缓存的块不会重下。 */
export const transferRetry = (id) => invoke('transfer_retry', { id });

/** 全部暂停 / 全部继续。 */
export const transferPauseAll = (paused) =>
  invoke('transfer_pause_all', { paused });

/** 清除已结束的任务（完成 / 失败 / 取消）。 */
export const transferClearDone = () => invoke('transfer_clear_done');

/** 当前用的是内置 api_id 还是用户自己的。不返回 api_hash（它是凭据）。 */
export const telegramApiIdStatus = () => invoke('telegram_api_id_status');

/** 保存用户自己的 api_id / api_hash。格式不合法时报 tg_bad_api_id。 */
export const telegramApiIdSave = (apiId, apiHash) =>
  invoke('telegram_api_id_save', { apiId, apiHash });

/** 恢复使用内置的那一份。 */
export const telegramApiIdReset = () => invoke('telegram_api_id_reset');

/** 登录之前先自检能不能连到 Telegram。
 *
 * 返回 `{status, elapsed_ms, via_proxy}`（snake_case，同本仓库其它命令），status 是
 * `ok` | `bad_proxy` | `no_route`。三种要分开显示：用户该做的事不同。 */
export const telegramCheckConnection = (proxyUrl) =>
  invoke('telegram_check_connection', { proxyUrl });

/** 批量查一批远程文件的缓存状态（本地查询，不碰网络）。 */
export const remoteCacheFileStats = (reqs) =>
  invoke('remote_cache_file_stats', { reqs });

/** 从某条消息之前再取一页文件（对话即目录的位置才有）。 */
export const remoteBrowseMore = (placeId, dir, before) =>
  invoke('remote_browse_more', { placeId, dir, before });

/** 按媒体分栏列对话：tab=media/file/link/audio/gif，before=0 为首屏，
 *  limit 前端按视口估。 */
export const remoteBrowseTab = (placeId, dir, tab, before, limit) =>
  invoke('remote_browse_tab', { placeId, dir, tab, before: before || 0, limit: limit || 0 });

/** 元数据 blob 落盘缓存（对话列表/文件列表快照）：字节数组进磁盘 LRU。 */
export const remoteMetaPut = (kind, key, bytes) =>
  invoke('remote_meta_put', { kind, key, data: Array.from(bytes) });

/** 读回元数据 blob；未命中返回 null。返回字节数组。 */
export const remoteMetaGet = (kind, key) =>
  invoke('remote_meta_get', { kind, key });

/** 一页多少条。界面据它判断还有没有更多。 */
export const remotePageSize = () => invoke('remote_page_size');

/** 当前对话是否开了「受保护内容」。
 *
 * 只影响是否显示一行告知。非 Telegram 位置恒为 false。 */
export const remoteDirProtected = (placeId, dir) =>
  invoke('remote_dir_protected', { placeId, dir });

/** 给代理输入框的默认值：已有 Telegram 位置的代理 > 系统代理 > 空。
 *
 * 不让用户对着空框猜。开了 Clash 那类「全局代理」的机器上尤其要紧——
 * 它只接管系统代理层的流量，不接管 MTProto 那种裸 TCP，所以「我开了
 * 全局代理」并不等于 omy 能直连，而这一点用户没有理由知道。 */
export const telegramSuggestProxy = () => invoke('telegram_suggest_proxy');

/** 这份登录态可用吗（判的是 auth key 在不在，不是文件在不在）。
 *
 * 多账号之后 session 按账号分别存放，所以必须指明问的是哪一份——
 * 不分账号地问「有没有登录过」，会让加第二个账号时显示成已登录。 */
export const telegramHasSession = (placeId) =>
  invoke('telegram_has_session', { placeId });

/** 只把位置从列表里摘掉，**保留本机登录态**：之后可以直接加回来、不必重扫。
 *
 * 与 `telegramPlaceDeleteAccount` 的区别是后者不可撤销，所以分成两个命令、
 * 界面上也分成两个入口——做成一个带开关的入口，少点一下就会走到
 * 破坏性更强的那一侧。 */
export const telegramPlaceDetach = (placeId) =>
  invoke('telegram_place_detach', { placeId });

/** 删除账号：摘掉位置**并**删掉本机登录态，下次要重新扫码或导入 tdata。 */
export const telegramPlaceDeleteAccount = (placeId) =>
  invoke('telegram_place_delete_account', { placeId });

/** 改这个位置在本机的显示名。只改本地，不动服务端的账号资料。
 *
 * 默认名取服务端昵称，但两个账号完全可能同名（同一个人的大小号），
 * 那时侧栏上就分不清谁是谁，所以必须允许改。 */
export const telegramPlaceRename = (placeId, name) =>
  invoke('telegram_place_rename', { placeId, name });

/** 这个 Telegram 位置的登录态是否已加密（per-place 密码槽）。侧栏据此显示锁标识。 */
export const telegramPlaceEncrypted = (placeId) =>
  invoke('telegram_place_encrypted', { placeId });

/** 显式加密此位置：用**现场输入的密码**保护登录态（与普通文件加密同款）。
 *  kdfProfile: interactive/moderate/sensitive。返回是否真的改变了状态。 */
export const telegramPlaceEncrypt = (placeId, password, kdfProfile = 'moderate') =>
  invoke('telegram_place_encrypt', { placeId, password, kdfProfile });

/** 取消加密此位置：转回默认（机器密钥）保护。需当前有能打开它的密码。 */
export const telegramPlaceDecrypt = (placeId) =>
  invoke('telegram_place_decrypt', { placeId });

/** 用现场输入的密码解锁一个锁定的加密位置。返回是否解出。 */
export const telegramPlaceUnlock = (placeId, password) =>
  invoke('telegram_place_unlock', { placeId, password });

/** 退出账号：连本机保存的登录态一起删掉，下次要重新扫码或导入 tdata。
 *
 * 用的是 `delete_account` 而非 `detach`：用户点「退出」期望登录态真的没了，
 * 而 detach 只摘位置、session 还在，下次打开会发现自己还登着。
 *
 * 登录对话框传 `TG_PENDING_ACCOUNT`（此时位置尚未创建），
 * 命令里的 `reg.remove` 那一步对它是空操作。 */
export const telegramForgetSession = (placeId) =>
  invoke('telegram_place_delete_account', { placeId });

/** 探测 tdata 导入的前提：客户端在不在跑、有没有自动找到 tdata。
 *
 * 返回 `{ client_running, candidates }`。**`candidates` 为空是常态**：
 * 便携版跟着 exe 走、可以在任意盘，自动发现必然落空。此时要**就地**
 * 给路径输入框，而不是显示「未检测到」——那等于把「需要你补个信息」
 * 显示成「不支持」，用户看到就走了。
 */
export const telegramTdataProbe = () => invoke('telegram_tdata_probe');

/** 这个路径像 tdata 吗。用于即时反馈：选错目录当场说，
 *  而不是等用户填完密码、点了导入、跑完一轮解密才报错。 */
export const telegramTdataCheck = (path) => invoke('telegram_tdata_check', { path });

/** 从 tdata 导入登录态并连接，返回位置 id。
 *
 * `passcode` 是 **Telegram Desktop 的本地密码**，不是两步验证的云密码。
 * 有没有设置事先无法预知，所以第一次调用不传；若报
 * `tg_tdata_need_passcode` 再向用户要，然后带着它重调。
 */
export const telegramTdataImport = (path, passcode, proxyUrl) =>
  invoke('telegram_tdata_import', { path, passcode, proxyUrl });

/** 开始扫码登录。立刻返回，进度走 `onTelegramLogin` 推送。
 *
 * `proxyUrl` 可空。后端会先归一化——grammers 只认 socks5://，
 * 而系统代理给出的通常是 http:// 形式。
 */
export const telegramLoginStart = (proxyUrl) =>
  invoke('telegram_login_start', { proxyUrl: proxyUrl || null });

/** 提交两步验证的云密码。 */
export const telegramSubmitPassword = (password) =>
  invoke('telegram_submit_password', { password });

/** 取消登录（关掉登录界面时必须调，否则那条连接会一直占着配额）。 */
export const telegramLoginCancel = () => invoke('telegram_login_cancel');

/** 用已保存的登录态连上 Telegram，并注册成一个远程位置，返回位置 id。
 *
 * 幂等：已经连过就返回原来那个 id，不会在侧栏里堆出两个 Telegram。
 */
export const telegramPlaceConnect = (proxyUrl) =>
  invoke('telegram_place_connect', { proxyUrl: proxyUrl || null });

/**
 * 订阅扫码登录进度。payload 形如 `{ phase, ... }`：
 * `connecting` / `qr` / `migrating` / `need_password` / `done` / `failed`。
 * 返回 unlisten。
 */
export const onTelegramLogin = (handler) =>
  listen('telegram-login', (e) => handler(e.payload));

// ---- 手机号登录（与扫码并列的一条路）----

/** 开始手机号登录。立刻返回，进度走 `onTelegramPhoneLogin` 推送。 */
export const telegramPhoneStart = (proxyUrl) =>
  invoke('telegram_phone_start', { proxyUrl: proxyUrl || null });

/** 提交手机号（含国家码，如 +8613800138000）。 */
export const telegramPhoneSubmitPhone = (phone) =>
  invoke('telegram_phone_submit_phone', { phone });

/** 提交验证码。 */
export const telegramPhoneSubmitCode = (code) =>
  invoke('telegram_phone_submit_code', { code });

/** 提交手机号登录的 2FA 云密码。 */
export const telegramPhoneSubmitPassword = (password) =>
  invoke('telegram_phone_submit_password', { password });

/** 请求重新发码。 */
export const telegramPhoneResend = () => invoke('telegram_phone_resend');

/** 取消手机号登录。 */
export const telegramPhoneCancel = () => invoke('telegram_phone_cancel');

/**
 * 订阅手机号登录进度。payload 形如 `{ phase, ... }`：
 * `connecting` / `awaiting_phone` / `code_sent` / `need_password` /
 * `done` / `failed`。返回 unlisten。
 */
export const onTelegramPhoneLogin = (handler) =>
  listen('telegram-phone-login', (e) => handler(e.payload));


// ---- 虚拟远程位置（本地收藏夹式，只存对真实远程文件的引用）----

/** 列出所有虚拟远程位置，返回 [{ id, name }]（供侧栏）。 */
export const virtualPlaces = () => invoke('virtual_places');

/** 新建一个虚拟远程位置，返回它的 id。 */
export const virtualCreate = (name) => invoke('virtual_create', { name });

/** 在某个虚拟位置的某文件夹下新建子文件夹（parentFolder 为空=根），返回新文件夹 id。 */
export const virtualAddFolder = (placeId, parentFolder, name) =>
  invoke('virtual_add_folder', { placeId, parentFolder, name });

/** 往某个虚拟位置的某文件夹里加一条引用（req 见 AddRefReq，camelCase）。返回 ref_id。
 *  req: { placeId, folder, source, dirId, fileId, snapshotName, snapshotSize } */
export const virtualAddRef = (req) => invoke('virtual_add_ref', { req });

/** 浏览一个虚拟位置的某文件夹（folder 为空=根）。返回 VirtualEntry[]（含源三态）。 */
export const virtualBrowse = (placeId, folder) =>
  invoke('virtual_browse', { placeId, folder });

/** 列出一个虚拟位置的全部文件夹（扁平化+depth），供树形目标选择器。返回 FolderNode[]。 */
export const virtualFolders = (placeId) => invoke('virtual_folders', { placeId });

/** 删除一个虚拟位置。返回是否删了。 */
export const virtualDelete = (placeId) => invoke('virtual_delete', { placeId });
