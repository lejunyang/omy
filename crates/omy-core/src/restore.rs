//! 把容器里保存的元数据还原到落盘的文件上。
//!
//! # 为什么单独一个模块
//!
//! [`crate::pack`] 负责采集，这里负责还原，两边共用
//! [`crate::container::EntryMeta`]。放在 core 而不是各前端里的理由与
//! pack 相同：CLI 和 GUI 都要解容器，两份实现迟早分歧——尤其还原策略
//! 本身有大量平台分支，复制一份等于把平台差异也复制一遍。
//!
//! # 为什么「尽力还原」而不是「失败即报错」
//!
//! 文档 05 §4.1 定的策略是**按目标平台能力尽力还原，不支持的项明确
//! 报告**。这两半都不能少：
//!
//! - 只要有一项设不上就整个解密失败，等于让用户为了一个时间戳丢掉
//!   全部文件内容——内容是主体，元数据是附属。
//! - 但静默丢弃更糟：用户以为完整还原了，直到某天发现所有文件的
//!   修改时间都是解密那天。备份场景下这是实质的数据丢失，而且
//!   **不可逆**——原目录往往已经删了。
//!
//! 所以每一项都试，失败的记进 [`RestoreReport`] 交给调用方展示。
//!
//! # 平台差异不猜，按实际能力分支
//!
//! | 项 | Windows | Unix |
//! |---|---|---|
//! | mtime | 支持（100ns 精度） | 支持（1ns） |
//! | btime（创建时间） | 支持 | Linux 无法设置，报告为不支持 |
//! | POSIX 权限位 | 无此概念，报告为不支持 | 支持 |
//! | uid / gid | 无此概念 | **有意不还原**，见下 |
//!
//! uid/gid 在两个平台上都不还原：跨机器的数字 uid 指向的是完全不同的
//! 用户，还原它要么无效要么危险（把文件判给本机的另一个账号）。设置
//! 属主还需要 root，而为了元数据要求提权是本末倒置。仅记录、不还原，
//! 与文档 §4.2 一致。
//!
//! # 目录 mtime：只有新建条目会冲掉它
//!
//! 我起初以为「设置子项的时间会更新父目录的 mtime」，据此把目录拆出来
//! 按深度倒序处理。实测四种操作后发现那是错的：
//!
//! | 操作 | 父目录 mtime 是否改变 |
//! |---|---|
//! | 设置子目录的 mtime | 否 |
//! | 设置子文件的 mtime | 否 |
//! | 在目录里**新建**文件 | 是 |
//! | 在目录里**新建**子目录 | 是 |
//!
//! 也就是说只有目录项的增删会动父目录的 mtime，这与 POSIX 对目录 mtime
//! 的定义一致。真正的约束因此不在本模块内部的处理顺序上，而在调用时机：
//! 必须等全部条目落盘之后再来设时间。排序那段复杂度已删除——留着一段
//! 理由已被证伪的代码，比没有它更糟，下一个人会以为这里有个已知陷阱。

use crate::container::{ContainerIndex, EntryMeta};
use std::path::Path;

/// 一项元数据未能还原的原因。
///
/// 用枚举而不是字符串：这些值要进 JSON 契约、要做翻译键，
/// 字符串散落在各处迟早写错一个，而拼错的键在界面上显示为原文，
/// 没有任何测试会因此变红。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// POSIX 权限位（Windows 无此概念）。
    Mode,
    /// 创建时间（Linux 无法设置）。
    Btime,
    /// 属主 uid / gid（跨机器无意义，有意不还原）。
    Owner,
    /// 扩展属性。
    Xattr,
}

impl UnsupportedKind {
    /// 稳定的字符串代号。
    ///
    /// **改动即破坏契约**：这些值同时是 `--json` 输出的字段值和前端的
    /// 翻译键后缀。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Mode => "mode",
            Self::Btime => "btime",
            Self::Owner => "owner",
            Self::Xattr => "xattr",
        }
    }
}

/// 单项还原失败的记录。
#[derive(Debug, Clone)]
pub struct RestoreFailure {
    /// 条目路径（相对容器根，用 `/` 连接）。
    pub path: String,
    /// 具体是哪一项没设上。
    pub item: &'static str,
    /// 操作系统给出的原因。
    pub reason: String,
}

/// 还原报告。
///
/// # 为什么把「平台不支持」和「尝试了但失败」分开
///
/// 两者对用户的含义完全不同，合并会误导：
///
/// - 不支持是**平台的既有事实**，换台 Linux 解开就能拿到，不是出错，
///   所以文档要求配一句「完整元数据仍保存在加密文件中」。
/// - 失败是**这次操作出了问题**（文件被占用、权限不足、路径有问题），
///   同一台机器重试可能就好了，需要用户处理。
///
/// 把前者报成错误会让人以为文件坏了；把后者报成"平台限制"会让人
/// 放弃重试，白丢元数据。
#[derive(Debug, Clone, Default)]
pub struct RestoreReport {
    /// 成功还原修改时间的条目数。
    pub mtime_restored: usize,
    /// 成功还原权限位的条目数。
    pub mode_restored: usize,
    /// 当前平台不支持的项，及涉及的条目数。
    ///
    /// 用 `Vec` 而不是 `HashMap`：项数固定只有四个，顺序稳定便于测试
    /// 与展示，也免得引入哈希遍历顺序不定的问题。
    pub unsupported: Vec<(UnsupportedKind, usize)>,
    /// 尝试过但失败的项。
    pub failures: Vec<RestoreFailure>,
}

impl RestoreReport {
    /// 是否有任何需要告知用户的内容。
    #[must_use]
    pub fn has_notes(&self) -> bool {
        !self.unsupported.is_empty() || !self.failures.is_empty()
    }

    /// 记一项平台不支持，计数累加。
    fn note_unsupported(&mut self, kind: UnsupportedKind) {
        if let Some(slot) = self.unsupported.iter_mut().find(|(k, _)| *k == kind) {
            slot.1 = slot.1.saturating_add(1);
        } else {
            self.unsupported.push((kind, 1));
        }
    }
}

/// 把索引里记录的元数据还原到 `root` 下已落盘的条目上。
///
/// `root` 是容器根目录在磁盘上的实际位置（即 `idx.root` 对应的那个
/// 目录）。条目相对路径由索引给出。
///
/// # 必须在全部条目落盘之后调用
///
/// 这是本函数唯一的顺序约束，但它在调用方那边：**新建**目录项会更新
/// 父目录的 mtime，所以只要还有文件要写，就不能开始设目录的时间——
/// 否则刚设好的目录时间会被随后写入的子项冲掉。症状是「文件的时间对了、
/// 目录的时间还是现在」，且只在非空目录上出现，空目录一切正常，很容易
/// 被当成偶发问题。
///
/// 反过来，**设置**子项的时间不会动父目录（实测见模块文档），所以本函数
/// 内部处理条目的先后无所谓，不需要按深度排序。
///
/// # 不返回 `Result`
///
/// 单项失败不该中断还原，也不该让整个解密失败——内容已经写好了。
/// 所有问题都进报告，由调用方决定怎么展示。
#[must_use]
pub fn restore_metadata(root: &Path, idx: &ContainerIndex) -> RestoreReport {
    let mut rep = RestoreReport::default();

    // 一个循环处理所有类型。曾经把目录拆出来按深度倒序处理，理由是
    // 「设子项会更新父目录的 mtime」——实测证明那是错的（见模块文档），
    // 于是删掉了那段排序
    for e in &idx.entries {
        let p = root.join(e.path.join(std::path::MAIN_SEPARATOR_STR));
        if !p.exists() {
            // 落盘阶段跳过或改名的条目，那边已经报告过，这里不重复记
            continue;
        }
        apply(&p, &e.meta, &e.display_path(), &mut rep);
    }

    rep
}

/// 对单个已存在的路径应用元数据。
fn apply(path: &Path, meta: &EntryMeta, display: &str, rep: &mut RestoreReport) {
    if let Some(ns) = meta.mtime_ns {
        match set_mtime(path, ns) {
            Ok(()) => rep.mtime_restored = rep.mtime_restored.saturating_add(1),
            Err(e) => rep.failures.push(RestoreFailure {
                path: display.to_owned(),
                item: "mtime",
                reason: e.to_string(),
            }),
        }
    }

    if meta.btime_ns.is_some() {
        // 创建时间：Windows 能设，但 std 没有提供接口（需要 SetFileTime
        // 或 FileTimes::set_created，后者仍是 unstable）。Linux 根本
        // 不允许设置。两种情况对用户是同一件事——这次拿不到，但值还在
        // 加密文件里，换个环境或将来版本能还原，所以报告为不支持而不是
        // 失败：报成失败会让人反复重试一件必然失败的事。
        rep.note_unsupported(UnsupportedKind::Btime);
    }

    if let Some(mode) = meta.mode {
        match set_mode(path, mode) {
            Ok(true) => rep.mode_restored = rep.mode_restored.saturating_add(1),
            Ok(false) => rep.note_unsupported(UnsupportedKind::Mode),
            Err(e) => rep.failures.push(RestoreFailure {
                path: display.to_owned(),
                item: "mode",
                reason: e.to_string(),
            }),
        }
    }

    if meta.uid.is_some() || meta.gid.is_some() {
        // 有意不还原，理由见模块文档
        rep.note_unsupported(UnsupportedKind::Owner);
    }

    if !meta.xattrs.is_empty() {
        rep.note_unsupported(UnsupportedKind::Xattr);
    }
}

/// 设置修改时间。
///
/// 用 `std::fs::FileTimes`（1.75 稳定）而不是第三方 crate：core 的依赖
/// 只有密码学与格式相关的几个，为一个时间戳引入 `filetime` 不值得。
fn set_mtime(path: &Path, ns: i128) -> std::io::Result<()> {
    use std::fs::FileTimes;

    let t = ns_to_system_time(ns).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("时间戳 {ns} 超出可表示范围"),
        )
    })?;

    // 必须用 OpenOptions 而不是 File::open：Windows 上设置时间需要写
    // 权限，只读句柄会得到「拒绝访问」。而目录在 Windows 上不能用普通
    // 的 write(true) 打开，要走 FILE_FLAG_BACKUP_SEMANTICS——所以目录
    // 和文件的打开方式不同，见 open_for_times。
    let f = open_for_times(path)?;
    f.set_times(FileTimes::new().set_modified(t))
}

/// 打开一个路径以便设置时间戳。
///
/// 目录与文件的打开方式在 Windows 上不同，这里把差异收在一处。
#[cfg(windows)]
fn open_for_times(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_BACKUP_SEMANTICS = 0x0200_0000
    // 不带这个标志无法获得目录的句柄，CreateFile 会返回「拒绝访问」，
    // 于是目录的 mtime 永远设不上——而文件一切正常，很容易误判成
    // 「目录时间还原不了是平台限制」。它不是限制，只是要换个打开方式。
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0x0200_0000)
        .open(path)
        .or_else(|_| {
            // 目录不接受 write(true)，退回只读 + BACKUP_SEMANTICS
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(0x0200_0000)
                .open(path)
        })
}

/// 见 windows 版本的说明。Unix 上目录可以直接用只读句柄设时间。
#[cfg(not(windows))]
fn open_for_times(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .or_else(|_| std::fs::File::open(path))
}

/// 设置 POSIX 权限位。
///
/// 返回 `Ok(false)` 表示当前平台没有这个概念——与「设置失败」是两件
/// 不同的事，调用方要分开报告。
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(true)
}

/// 见 unix 版本。Windows 没有 POSIX 权限位。
///
/// **不做「只读位近似」**：把 `0o444` 映射成 Windows 只读属性看着贴心，
/// 实际是用一个语义不同的东西冒充还原成功——用户会以为权限回来了。
/// 如实报告不支持，并告诉他值还在文件里。
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> std::io::Result<bool> {
    Ok(false)
}

/// Unix 纳秒转 `SystemTime`。
///
/// 负数（1970 年之前）也要能处理：老文件的时间戳确实可能早于纪元，
/// 直接当成 0 会把「1965 年」显示成「1970 年」，是在编造数据。
fn ns_to_system_time(ns: i128) -> Option<std::time::SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = ns.div_euclid(1_000_000_000);
    let sub = ns.rem_euclid(1_000_000_000);
    let sub_ns = u32::try_from(sub).ok()?;
    if secs >= 0 {
        let s = u64::try_from(secs).ok()?;
        UNIX_EPOCH.checked_add(Duration::new(s, sub_ns))
    } else {
        // rem_euclid 保证 sub 非负，所以这里的秒数要往前多减一秒再补回。
        // 用 checked_neg 而不是 -secs：i128::MIN 取反会溢出，虽然这个
        // 值不可能来自真实时间戳，但它来自文件里的字节，而文件内容是
        // 不可信输入——core 里任何 panic 路径都是拒绝服务缺陷
        let s = u64::try_from(secs.checked_neg()?).ok()?;
        UNIX_EPOCH.checked_sub(Duration::new(s, 0))?.checked_add(
            Duration::from_nanos(u64::from(sub_ns)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::{ContainerBuilder, EntryMeta};

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("omy-restore-{tag}"));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// 一个具体的、非"现在"的时间：2019-03-07 14:25:36.123456789 UTC
    const T: i128 = 1_551_968_736_123_456_789;

    #[test]
    fn mtime_is_actually_written_to_disk() {
        // 这条抓的是本轮修的缺陷本体：元数据一直被采集并编码进容器，
        // 但解包时无人读取，解出来的文件时间是"现在"。
        // 不这样测就只能看到"函数返回了 Ok"，而那与磁盘上的时间无关。
        let root = tmp("mtime");
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a.txt".into()], 1, None, EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.mtime_restored, 1);

        let got = std::fs::metadata(root.join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        let want = ns_to_system_time(T).unwrap();
        let d = got
            .duration_since(want)
            .unwrap_or_else(|e| e.duration())
            .as_secs();
        assert!(d < 2, "磁盘上的 mtime 应当接近 {want:?}，实际 {got:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn dir_and_file_times_both_restored_in_real_extract_order() {
        // 模拟真实解包流程：先把所有条目写到盘上，再统一还原元数据。
        // 这是 extract 实际的调用方式，也是唯一被支持的顺序。
        let root = tmp("dirorder");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/c.txt"), b"c").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_dir(vec!["sub".into()], EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        b.add_file(vec!["sub".into(), "c.txt".into()], 1, None, EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.mtime_restored, 2, "目录和文件都该设上");

        let want = ns_to_system_time(T).unwrap();
        for rel in ["sub", "sub/c.txt"] {
            let p = root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
            let got = std::fs::metadata(&p).unwrap().modified().unwrap();
            let d = got
                .duration_since(want)
                .unwrap_or_else(|e| e.duration())
                .as_secs();
            assert!(d < 2, "{rel} 的 mtime 不对：{got:?}");
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn creating_entries_after_restore_clobbers_dir_mtime() {
        // 这条是上面那条的对照，锁住「必须在全部条目落盘之后调用」这个
        // 约束**真的存在**。没有它，那句文档就只是一句声明，谁也不知道
        // 违反了会怎样，将来有人为了「边解边设省一次遍历」把调用挪进
        // 落盘循环里，测试不会有任何反应。
        //
        // 顺带修正一个我自己写错的注释：起初以为设置子项时间也会冲掉
        // 父目录，据此加了按深度排序。实测证明只有**新建**条目才会。
        let root = tmp("clobber");
        std::fs::create_dir_all(root.join("sub")).unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_dir(vec!["sub".into()], EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let _ = restore_metadata(&root, &idx);
        let want = ns_to_system_time(T).unwrap();
        let after_restore = std::fs::metadata(root.join("sub"))
            .unwrap()
            .modified()
            .unwrap();
        assert!(
            after_restore
                .duration_since(want)
                .unwrap_or_else(|e| e.duration())
                .as_secs()
                < 2,
            "还原后目录时间应当已设好"
        );

        // 顺序颠倒：还原之后又新建了一个条目
        std::fs::write(root.join("sub/late.txt"), b"x").unwrap();
        let after_write = std::fs::metadata(root.join("sub"))
            .unwrap()
            .modified()
            .unwrap();
        assert_ne!(
            after_restore, after_write,
            "新建条目会冲掉目录 mtime——这正是必须最后还原的原因"
        );

        // 而设置子项的时间不会：这是当初那段排序复杂度被删掉的依据
        let before = after_write;
        let mut b2 = ContainerBuilder::new("r");
        b2.add_file(vec!["sub".into(), "late.txt".into()], 1, None, EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let _ = restore_metadata(&root, &b2.finish().unwrap());
        let after_child_times = std::fs::metadata(root.join("sub"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            before, after_child_times,
            "设置子项时间不该改变父目录 mtime；若这条失败，按深度排序的\
             复杂度就有必要重新加回来"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn owner_is_reported_unsupported_not_silently_dropped() {
        // uid/gid 有意不还原。但"有意不做"和"忘了做"在代码里长得一样，
        // 区别只体现在有没有告诉用户。这条锁住"必须告诉用户"。
        let root = tmp("owner");
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a.txt".into()], 1, None, EntryMeta {
            uid: Some(1000),
            gid: Some(1000),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert!(rep.has_notes(), "不还原就必须报告");
        assert_eq!(
            rep.unsupported,
            vec![(UnsupportedKind::Owner, 1)],
            "应当只报 owner 一项"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(not(unix))]
    #[test]
    fn mode_reported_unsupported_on_windows_not_faked() {
        // Windows 上不能把 mode 映射成只读属性冒充成功。
        // 这条确保它进的是 unsupported（可换平台还原）而不是
        // failures（像是出了错），也不是 mode_restored（谎报成功）。
        let root = tmp("mode-win");
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a.txt".into()], 1, None, EntryMeta {
            mode: Some(0o644),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.mode_restored, 0, "不得谎报还原成功");
        assert!(rep.failures.is_empty(), "平台不支持不是失败");
        assert_eq!(rep.unsupported, vec![(UnsupportedKind::Mode, 1)]);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn mode_is_actually_applied_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("mode-unix");
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a.txt".into()], 1, None, EntryMeta {
            mode: Some(0o600),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.mode_restored, 1);
        let m = std::fs::metadata(root.join("a.txt")).unwrap();
        assert_eq!(m.permissions().mode() & 0o777, 0o600);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_entries_are_skipped_without_failure() {
        // 落盘阶段跳过或改名的条目在这里找不到。那边已经报告过了，
        // 这里再记一次会让用户看到同一个问题的两种说法，
        // 还会把"文件名被调整"说成"元数据还原失败"。
        let root = tmp("missing");

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["ghost.txt".into()], 1, None, EntryMeta {
            mtime_ns: Some(T),
            ..EntryMeta::default()
        })
        .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.mtime_restored, 0);
        assert!(rep.failures.is_empty(), "条目不存在不算还原失败");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_meta_produces_no_notes() {
        // 没有元数据可还原时必须安静。否则每次解密都弹一堆
        // "未能还原"，真正需要注意的报告就被淹没了。
        let root = tmp("empty");
        std::fs::write(root.join("a.txt"), b"x").unwrap();

        let mut b = ContainerBuilder::new("r");
        b.add_file(vec!["a.txt".into()], 1, None, EntryMeta::default())
            .unwrap();
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert!(!rep.has_notes(), "无元数据时不该有任何提示");
        assert_eq!(rep.mtime_restored, 0);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unsupported_counts_accumulate_across_entries() {
        // 报告要说"1247 个文件的权限位"，不是重复 1247 行。
        // 计数不累加的话，大目录的报告会长到没人看。
        let root = tmp("accum");
        for n in ["a", "b", "c"] {
            std::fs::write(root.join(format!("{n}.txt")), b"x").unwrap();
        }

        let mut b = ContainerBuilder::new("r");
        for n in ["a", "b", "c"] {
            b.add_file(vec![format!("{n}.txt")], 1, None, EntryMeta {
                uid: Some(1000),
                ..EntryMeta::default()
            })
            .unwrap();
        }
        let idx = b.finish().unwrap();

        let rep = restore_metadata(&root, &idx);
        assert_eq!(rep.unsupported.len(), 1, "同一项只占一条");
        assert_eq!(rep.unsupported[0].1, 3, "计数应累加到 3");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pre_epoch_timestamps_round_trip() {
        // 1970 年之前的时间戳确实存在于老文件上。当成 0 处理会把
        // "1965 年"显示成"1970 年"——编造一个看起来合理的错误值，
        // 比明确报告不知道更糟。
        let ns: i128 = -150_000_000_000_000_000; // 1965 年左右
        let t = ns_to_system_time(ns).expect("纪元前时间应可表示");
        let back = std::time::UNIX_EPOCH
            .duration_since(t)
            .expect("应当早于纪元");
        assert_eq!(
            i128::try_from(back.as_nanos()).unwrap(),
            -ns,
            "纪元前时间戳往返后应当一致"
        );
    }

    #[test]
    fn sub_second_precision_is_not_dropped() {
        // 只用整秒测的话，把纳秒部分整个丢掉的实现也能通过。
        //
        // 但也不能断言纳秒完全相等：Windows 的 SystemTime 底层是
        // FILETIME，粒度 100ns，实测 123_456_789 会变成 123_456_700。
        // 这是文档 05 §4.2 已经写明并接受的精度损失（「Windows 100ns
        // 精度 / Unix 1ns，精度损失可接受」），不是缺陷。
        //
        // 所以判据是「误差在 1µs 内」：把纳秒整个丢掉的实现会得到 0，
        // 差 123 毫秒，仍然过不了。若改成 `!= 0` 就没这个抓错能力了，
        // 任何非零值都能糊弄过去。
        let ns: i128 = 1_551_968_736_123_456_789;
        let t = ns_to_system_time(ns).unwrap();
        let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
        let got = i64::from(d.subsec_nanos());
        let diff = (got - 123_456_789_i64).abs();
        assert!(
            diff < 1_000,
            "亚秒部分不能被丢弃：期望约 123456789ns，实际 {got}ns（差 {diff}ns）"
        );
    }

    #[test]
    fn unsupported_codes_are_stable() {
        // 这些字符串是 JSON 字段值与翻译键后缀，改了会静默破坏前端：
        // 拼错的翻译键在界面上显示为原文，没有测试会因此变红。
        assert_eq!(UnsupportedKind::Mode.code(), "mode");
        assert_eq!(UnsupportedKind::Btime.code(), "btime");
        assert_eq!(UnsupportedKind::Owner.code(), "owner");
        assert_eq!(UnsupportedKind::Xattr.code(), "xattr");
    }
}
