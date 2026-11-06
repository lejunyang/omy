//! Telegram 对话的消息时间线与本机保存的远程元数据。

use super::*;

/// 列一个对话里的消息（以文件为主线的消息视图）。
///
/// # 范围
///
/// 这**不是**聊天客户端：只读、不发消息、不做回复关系 / 转发链 / reactions /
/// 已读状态（文档 §1.3 把它们列在 out of scope）。它的用途是让用户按时间线
/// 找文件——「我记得上周发过一个东西」。
///
/// # 广播频道没有消息视图
///
/// 见 `TelegramStore::messages` 上关于 ToS 3.3 的说明。这里把那个标记映射成
/// 专门的错误码，好让界面给一句能看懂的话而不是笼统的「不支持此操作」。
///
/// # Errors
///
/// 位置不存在、不是 Telegram、是广播频道、网络失败或限流时返回。
#[tauri::command]
pub async fn remote_messages(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    dir: String,
    before: Option<i32>,
) -> CmdResult<Vec<omy_remote::telegram::store::MessageRow>> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        // 只有 Telegram 有「消息」这个概念。WebDAV 上没有，
        // 界面也不该显示那个切换——这里是最后一道
        return Err(CmdError::code("remote_no_messages"));
    };
    // `before` 为 None 是首屏；给了消息号就是「加载更早」——
    // 从那条之前再要一页
    tg.messages(&dir, MESSAGE_PAGE, before)
        .await
        .map_err(|e| to_cmd_err(&e))
}

/// 以某条消息为中心拉取一段（引用跳转 / 定位源消息用）。
///
/// 向更旧取约半屏、向更新取约半屏，让目标落在视口中间，且两头都还能续翻。
/// `around` 是要定位到的消息号（来自媒体条目的 (对话,消息) 或回复 reply_to）。
///
/// # Errors
///
/// 未登录、不是 Telegram 位置、网络失败或限流时返回。
#[tauri::command]
pub async fn remote_messages_around(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    dir: String,
    around: i32,
) -> CmdResult<omy_remote::telegram::store::MessageWindow> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        return Err(CmdError::code("remote_no_messages"));
    };
    // 前后各取半屏：目标居中。用 MESSAGE_PAGE 的一半作两侧量，合起来约一屏。
    let half = MESSAGE_PAGE / 2;
    tg.messages_around(&dir, around, half, half)
        .await
        .map_err(|e| to_cmd_err(&e))
}

/// 拉取某条消息**之后（更新方向）**的一页消息（引用跳转后续翻用）。
///
/// 时间线新消息在顶部：用户跳到历史中间一条后往**上（顶部）**滚，越过这段最新
/// 一条时要能继续取回它之后的消息。普通 [`remote_messages`] 只有 `before`（向更旧）
/// 一个方向，补不齐跳上去之后的那半边，所以单独加这条。
///
/// 返回仍是新→旧的一页，`after` 为游标（exclusive 下界，取 id > after）。
///
/// # Errors
///
/// 未登录、不是 Telegram 位置、网络失败或限流时返回。
#[tauri::command]
pub async fn remote_messages_after(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, crate::commands::Shared>,
    place_id: String,
    dir: String,
    after: i32,
) -> CmdResult<Vec<omy_remote::telegram::store::MessageRow>> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let omy_remote::PlaceStore::Telegram(tg) = place.store.as_ref() else {
        return Err(CmdError::code("remote_no_messages"));
    };
    tg.messages_after(&dir, MESSAGE_PAGE, after)
        .await
        .map_err(|e| to_cmd_err(&e))
}

/// 消息视图一次拉多少条。
///
/// 不是「全部」：活跃对话里可能有上万条，全拉既慢又白占限流配额，
/// 而用户在时间线上一次也看不完这么多。
pub(super) const MESSAGE_PAGE: usize = 25;

/// 存一份元数据 blob（对话列表 / 文件列表快照等）到磁盘 LRU 缓存。
///
/// 前端把 `remoteDirCache` 里的一屏内容序列化成字节传进来，落进 BlockCache
/// 的临时层——**与内容块共用同一 LRU、同一 `cache_limit`、同一淘汰**。重启后
/// 前端用 `remote_meta_get` 读回，实现「重启仍先出缓存、再后台刷新」。
///
/// # 明文落盘的边界
///
/// dialogs / list 这两类 kind 存的是对话名、文件的密文名(远端 id)、大小、
/// 时间等——都是浏览时本就展示、Telegram 侧公开的信息，可明文落盘。**调用方
/// 必须保证不把解密后的 .omy 真实文件名塞进来**（那是隐私）：真实名只在
/// 会话内存里、随 `remote-entry` 识别事件产生，不进入这条落盘路径。
///
/// # Errors
///
/// 从不返回错误：缓存写不进只降级为不缓存，不该让浏览失败。
#[tauri::command]
pub fn remote_meta_put(
    cache: tauri::State<'_, Arc<RemoteCache>>,
    kind: String,
    key: String,
    data: Vec<u8>,
) {
    cache.put_meta(&kind, &key, &data);
}

/// 读一份元数据 blob。未命中返回 `None`（前端据此走网络重取）。
#[tauri::command]
#[must_use]
pub fn remote_meta_get(
    cache: tauri::State<'_, Arc<RemoteCache>>,
    kind: String,
    key: String,
) -> Option<Vec<u8>> {
    cache.get_meta(&kind, &key)
}
