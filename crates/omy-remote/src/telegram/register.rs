//! 登录态收编成位置的**纯业务逻辑**：GUI 与 CLI 共用同一份。
//!
//! # 为什么要抽出来
//!
//! 登录成功那一刻还没有位置 id：session 先落在
//! [`session::PENDING_ACCOUNT`](super::session::PENDING_ACCOUNT) 名下，拿到服务端
//! user id 之后，才要按 user id 去重、分配一个 `pN`、再写进配置。这段
//! 「pending → 位置」的判定与拼装曾经在 CLI 与 GUI 各写一份；两处一旦漂移，
//! 就会出现「同一个账号在命令行加过、在界面里又建了一个位置」这种彼此看不见
//! 的重复。所以纯判定只在这里，两端的命令层只负责把它包在网络/界面外壳里。
//!
//! # 它**不做**什么
//!
//! 不碰磁盘、不连网、不读写配置文件。它只是「给我当前位置列表 + user id +
//! 想显示的名字，我告诉你该用哪个 id、并把记录加进去」。真正的落盘
//! （`save_current` / `adopt_pending`）与配置锁由调用方在外面包。这样测它
//! 不需要凭据库，也不需要真连 Telegram。

use std::collections::HashSet;

use omy_config::SavedPlace;

use crate::placebook::allocate_place_id;
use crate::telegram::session::session_path_of;

/// 把一份刚登录成功的 Telegram 账号收编成位置，就地改 `places`。
///
/// 按 `user_id` 去重：命中已有 Telegram 位置就**复用它的 id**（不新增记录），
/// 否则分配一个新的 `pN` 并追加一条记录。返回 `(位置 id, 是否命中已有账号)`。
///
/// # 顺序约定
///
/// 先决定 id、再落 session——session 文件的名字正是按 id 命名的，反过来会让
/// 「session 叫什么」有两个来源，迟早对不上，表现是「登录了但重启后还要再登」。
///
/// # 为什么判据是 user_id 不是昵称
///
/// 昵称会重复、会被用户改掉；只有服务端 user id 在服务端唯一，才能判断
/// 「这两个位置是不是同一个账号」。拿昵称判重，换个头像改名就会把老账号
/// 当成新账号再加一遍。
pub fn fold_into_places(
    places: &mut Vec<SavedPlace>,
    user_id: Option<i64>,
    label: &str,
) -> (String, bool) {
    if let Some(uid) = user_id
        && let Some(existing) = places
            .iter()
            .find(|p| p.kind == "telegram" && p.user_id == Some(uid))
    {
        return (existing.id.clone(), true);
    }

    let taken: HashSet<String> = places.iter().map(|p| p.id.clone()).collect();
    // 排除「已不在配置里、但磁盘上还留着 session 文件」的 id：典型是被移除的
    // 位置没来得及清干净的残骸。撞上它会让新账号读到一份属于别人的登录态。
    let blocked = |id: &str| session_path_of(id).map(|p| p.exists()).unwrap_or(false);
    let id = allocate_place_id(&taken, &blocked);

    places.push(SavedPlace {
        id: id.clone(),
        name: label.to_string(),
        kind: String::from("telegram"),
        // Telegram 位置只记账号身份，连接不存 URL（与 GUI 的 telegram_saved_places 同形）。
        url: String::new(),
        username: String::new(),
        vendor: String::new(),
        writable: true,
        secret: None,
        user_id,
    });
    (id, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tg(id: &str, uid: Option<i64>) -> SavedPlace {
        SavedPlace {
            id: String::from(id),
            name: String::from("x"),
            kind: String::from("telegram"),
            url: String::new(),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: uid,
        }
    }

    /// 命中已有 user_id 时必须复用其 id、且**不新增记录**。
    ///
    /// 不这样会怎样：同一个账号扫码/导入两次，配置里出现两条同 id 的记录
    /// （或两个 id 指向同一份登录态），改一个另一个不一致。
    #[test]
    fn duplicate_reuses_id_without_pushing() {
        let mut places = vec![tg("p1", Some(42)), tg("p2", Some(7))];
        let (id, dup) = fold_into_places(&mut places, Some(42), "新名字");
        assert_eq!(id, "p1");
        assert!(dup);
        assert_eq!(places.len(), 2, "命中已有账号不得追加记录");
        // 名字不覆盖：昵称是用户可改的标签，收编旧账号时不强行改名
        assert_eq!(places[0].name, "x");
    }

    /// 全新 user_id 分配一个 pN 并追加。
    #[test]
    fn new_account_allocates_and_pushes() {
        let mut places = vec![tg("p1", Some(42))];
        let (id, dup) = fold_into_places(&mut places, Some(99), "第二个号");
        assert_eq!(id, "p2", "取最大序号 +1");
        assert!(!dup);
        assert_eq!(places.len(), 2);
        assert_eq!(places[1].user_id, Some(99));
        assert_eq!(places[1].name, "第二个号");
        assert_eq!(places[1].kind, "telegram");
    }

    /// user_id 未知（None）时当作新账号，绝不误命中。
    ///
    /// 不这样会怎样：拿不到 user_id 时若误判成某个已有账号，会把这次登录
    /// 收编成别人的位置。
    #[test]
    fn unknown_user_id_is_treated_as_new() {
        let mut places = vec![tg("p1", Some(42))];
        let (id, dup) = fold_into_places(&mut places, None, "?");
        assert_eq!(id, "p2");
        assert!(!dup);
        assert_eq!(places.len(), 2);
    }

    /// 去重只认 Telegram 位置：WebDAV 位置即便 user_id 撞了也不能命中。
    #[test]
    fn dedup_ignores_webdav_kind() {
        let mut places = vec![SavedPlace {
            id: String::from("p1"),
            name: String::from("dav"),
            kind: String::from("webdav"),
            url: String::from("https://dav/"),
            username: String::new(),
            vendor: String::new(),
            writable: true,
            secret: None,
            user_id: Some(42),
        }];
        let (id, dup) = fold_into_places(&mut places, Some(42), "tg");
        assert_eq!(id, "p2", "WebDAV 的 user_id 不算命中 Telegram 账号");
        assert!(!dup);
    }
}
