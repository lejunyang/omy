//! 用户可见的完整文件永久缓存。
//!
//! 临时缓存仍由 [`crate::cache::BlockCache`] 按块管理；本模块只负责永久层，
//! 将远端原始文件保存为普通文件，并用稳定远程身份组织目录。

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Component, Path, PathBuf};

use blake2::digest::{Update, VariableOutput};
use serde::{Deserialize, Serialize};

use crate::virtuals::SourceRef;

const META_FILE: &str = ".omy-pin.json";
const PART_FILE: &str = ".omy-pin.part";
const FORMAT_VERSION: u32 = 1;

/// 一个完整文件在永久层中的稳定目标。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTarget {
    /// 远程源的跨机器稳定身份。
    pub source: SourceRef,
    /// 源内的稳定条目 id（Telegram 为 chat+message，WebDAV 为路径）。
    pub item_id: String,
    /// 服务端原文件名。
    pub server_name: String,
    /// 远端版本键。相同条目被覆盖后用它拒绝复用旧文件。
    pub version: String,
    /// 完整远端原文件字节数。
    pub total_size: u64,
}

impl PinnedTarget {
    #[must_use]
    pub fn new(
        source: SourceRef,
        item_id: impl Into<String>,
        server_name: impl Into<String>,
        version: impl Into<String>,
        total_size: u64,
    ) -> Self {
        Self {
            source,
            item_id: item_id.into(),
            server_name: server_name.into(),
            version: version.into(),
            total_size,
        }
    }
}

/// 设置页展示的一项永久文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PinnedFile {
    /// 远程源类型（telegram / webdav）。
    pub kind: String,
    /// 可读的稳定来源标签；不含密码或令牌。
    pub source: String,
    /// 源内条目 id。
    pub item_id: String,
    /// 服务端原文件名。
    pub name: String,
    /// 相对永久根的条目目录；取消永久时作为不透明句柄传回后端。
    pub relative_dir: String,
    /// 完整文件实际占用字节数。
    pub used_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PinMeta {
    format: u32,
    source: SourceRef,
    item_id: String,
    server_name: String,
    stored_name: String,
    version: String,
    total_size: u64,
}

impl PinMeta {
    fn from_target(target: &PinnedTarget) -> Self {
        Self {
            format: FORMAT_VERSION,
            source: target.source.clone(),
            item_id: target.item_id.clone(),
            server_name: target.server_name.clone(),
            stored_name: safe_server_name(&target.server_name),
            version: target.version.clone(),
            total_size: target.total_size,
        }
    }
}

#[derive(Debug)]
pub(crate) struct PinPaths {
    pub dir: PathBuf,
    pub data: PathBuf,
    pub part: PathBuf,
    pub meta: PathBuf,
}

pub(crate) fn paths(root: &Path, target: &PinnedTarget) -> Option<PinPaths> {
    let rel = relative_dir(target)?;
    let dir = root.join(rel);
    let data = dir.join(safe_server_name(&target.server_name));
    Some(PinPaths {
        dir: dir.clone(),
        data,
        part: dir.join(PART_FILE),
        meta: dir.join(META_FILE),
    })
}

/// 完整且版本匹配时返回本地文件路径。
#[must_use]
pub(crate) fn complete_path(root: &Path, target: &PinnedTarget) -> Option<PathBuf> {
    complete_path_inner(root, target, true)
}

/// 在尚未读到 `.omy` 头部、因而还算不出版本键时，按稳定来源、条目与大小
/// 读取候选完整文件。拿到头部后调用方仍会构造正式目标并再次校验版本。
#[must_use]
pub(crate) fn candidate_range(
    root: &Path,
    source: &SourceRef,
    item_id: &str,
    total_size: u64,
    offset: u64,
    len: u64,
) -> Option<Vec<u8>> {
    let path = candidate_path(root, source, item_id, total_size)?;
    read_file_range(&path, total_size, offset, len)
}

/// 尚未算出版本键时，仅按稳定来源、条目与大小确认完整永久文件存在。
#[must_use]
pub(crate) fn candidate_path(
    root: &Path,
    source: &SourceRef,
    item_id: &str,
    total_size: u64,
) -> Option<PathBuf> {
    let dir = root.join(relative_dir_for(source, item_id)?);
    let raw = std::fs::read(dir.join(META_FILE)).ok()?;
    let meta: PinMeta = serde_json::from_slice(&raw).ok()?;
    let identity_matches = meta.format == FORMAT_VERSION
        && meta.source == *source
        && meta.item_id == item_id
        && meta.total_size == total_size
        && meta.stored_name == safe_server_name(&meta.server_name);
    if !identity_matches {
        return None;
    }
    let path = dir.join(meta.stored_name);
    let info = std::fs::metadata(&path).ok()?;
    (info.is_file() && info.len() == total_size).then_some(path)
}

fn complete_path_inner(root: &Path, target: &PinnedTarget, check_version: bool) -> Option<PathBuf> {
    let p = paths(root, target)?;
    let raw = std::fs::read(&p.meta).ok()?;
    let meta: PinMeta = serde_json::from_slice(&raw).ok()?;
    let identity_matches = meta.format == FORMAT_VERSION
        && meta.source == target.source
        && meta.item_id == target.item_id
        && meta.server_name == target.server_name
        && meta.total_size == target.total_size
        && meta.stored_name == safe_server_name(&target.server_name);
    if !identity_matches || (check_version && meta.version != target.version) {
        return None;
    }
    let len = std::fs::metadata(&p.data).ok()?.len();
    (len == target.total_size).then_some(p.data)
}

/// 从完整永久文件读取一个区间。
#[must_use]
pub(crate) fn read_range(
    root: &Path,
    target: &PinnedTarget,
    offset: u64,
    len: u64,
) -> Option<Vec<u8>> {
    let path = complete_path(root, target)?;
    read_file_range(&path, target.total_size, offset, len)
}

fn read_file_range(path: &Path, total_size: u64, offset: u64, len: u64) -> Option<Vec<u8>> {
    let end = offset.checked_add(len)?;
    if end > total_size {
        return None;
    }
    let size = usize::try_from(len).ok()?;
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut out = vec![0u8; size];
    file.read_exact(&mut out).ok()?;
    Some(out)
}

/// 准备写入目录；旧版本或残留半成品会被清掉。
pub(crate) fn prepare(root: &Path, target: &PinnedTarget) -> std::io::Result<PinPaths> {
    let p = paths(root, target)
        .ok_or_else(|| std::io::Error::other("远程身份或条目 id 无法生成稳定永久路径"))?;
    std::fs::create_dir_all(&p.dir)?;
    if complete_path(root, target).is_none() {
        let _ = std::fs::remove_file(&p.data);
        let _ = std::fs::remove_file(&p.meta);
    }
    let _ = std::fs::remove_file(&p.part);
    Ok(p)
}

/// 将已写完并同步过的 `.part` 原子提交为完整文件，再写元数据。
pub(crate) fn commit(root: &Path, target: &PinnedTarget) -> std::io::Result<u64> {
    let p = paths(root, target)
        .ok_or_else(|| std::io::Error::other("远程身份或条目 id 无法生成稳定永久路径"))?;
    let actual = std::fs::metadata(&p.part)?.len();
    if actual != target.total_size {
        return Err(std::io::Error::other(format!(
            "永久文件长度不符：应为 {} 字节，实际 {actual} 字节",
            target.total_size
        )));
    }
    if p.data.exists() {
        std::fs::remove_file(&p.data)?;
    }
    std::fs::rename(&p.part, &p.data)?;
    let raw = serde_json::to_vec_pretty(&PinMeta::from_target(target))
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    omy_core::fsatomic::write_atomic(&p.meta, &raw)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(actual)
}

/// 删除指定稳定目标的完整永久文件。
pub(crate) fn remove(root: &Path, target: &PinnedTarget) -> std::io::Result<u64> {
    let Some(p) = paths(root, target) else {
        return Ok(0);
    };
    remove_dir(&p.dir)
}

/// 按设置页返回的不透明相对目录删除一项；拒绝绝对路径与 `..`。
pub(crate) fn remove_relative(root: &Path, relative: &str) -> std::io::Result<u64> {
    let rel = Path::new(relative);
    if rel.is_absolute() || rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        return Err(std::io::Error::other("非法的永久缓存相对路径"));
    }
    let mut comps = rel.components();
    let Some(Component::Normal(kind)) = comps.next() else {
        return Err(std::io::Error::other("永久缓存相对路径为空"));
    };
    if kind != "telegram" && kind != "webdav" {
        return Err(std::io::Error::other("永久缓存相对路径不属于已知远程类型"));
    }
    remove_dir(&root.join(rel))
}

fn remove_dir(dir: &Path) -> std::io::Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let used = data_bytes_in_dir(dir);
    std::fs::remove_dir_all(dir)?;
    Ok(used)
}

#[must_use]
pub(crate) fn list(root: &Path) -> Vec<PinnedFile> {
    let mut out = Vec::new();
    collect_meta(root, root, &mut out);
    out.sort_by(|a, b| {
        (&a.kind, &a.source, &a.item_id, &a.name).cmp(&(&b.kind, &b.source, &b.item_id, &b.name))
    });
    out
}

fn collect_meta(root: &Path, dir: &Path, out: &mut Vec<PinnedFile>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_meta(root, &path, out);
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) != Some(META_FILE) {
            continue;
        }
        let Ok(raw) = std::fs::read(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<PinMeta>(&raw) else {
            continue;
        };
        if meta.format != FORMAT_VERSION {
            continue;
        }
        let Some(parent) = path.parent() else {
            continue;
        };
        let data = parent.join(&meta.stored_name);
        let Ok(info) = std::fs::metadata(&data) else {
            continue;
        };
        if !info.is_file() || info.len() != meta.total_size {
            continue;
        }
        let Ok(relative) = parent.strip_prefix(root) else {
            continue;
        };
        out.push(PinnedFile {
            kind: meta.source.kind.clone(),
            source: source_label(&meta.source),
            item_id: meta.item_id,
            name: meta.server_name,
            relative_dir: relative.to_string_lossy().replace('\\', "/"),
            used_bytes: info.len(),
        });
    }
}

#[must_use]
pub(crate) fn used(root: &Path) -> u64 {
    list(root).iter().map(|f| f.used_bytes).sum()
}

fn data_bytes_in_dir(dir: &Path) -> u64 {
    let Ok(raw) = std::fs::read(dir.join(META_FILE)) else {
        return 0;
    };
    let Ok(meta) = serde_json::from_slice::<PinMeta>(&raw) else {
        return 0;
    };
    std::fs::metadata(dir.join(meta.stored_name))
        .map(|m| if m.is_file() { m.len() } else { 0 })
        .unwrap_or(0)
}

fn relative_dir(target: &PinnedTarget) -> Option<PathBuf> {
    relative_dir_for(&target.source, &target.item_id)
}

fn relative_dir_for(source: &SourceRef, item_id: &str) -> Option<PathBuf> {
    match source.kind.as_str() {
        "telegram" => {
            let user = source.telegram_user_id?;
            let rest = item_id.strip_prefix("tg:")?;
            let (chat, message) = rest.split_once(':')?;
            chat.parse::<i64>().ok()?;
            message.parse::<i32>().ok()?;
            Some(
                PathBuf::from("telegram")
                    .join(user.to_string())
                    .join(chat)
                    .join(message),
            )
        }
        "webdav" => {
            let url = source.webdav_url.as_deref()?;
            let username = source.webdav_username.as_deref().unwrap_or("");
            let parsed = url::Url::parse(url).ok()?;
            let host = parsed.host_str().unwrap_or("webdav");
            let host = safe_component(host);
            let source_hash = digest_hex(&[url.as_bytes(), b"\0", username.as_bytes()]);
            let item_hash = digest_hex(&[item_id.as_bytes()]);
            Some(
                PathBuf::from("webdav")
                    .join(format!("{host}-{}", &source_hash[..12]))
                    .join(item_hash),
            )
        }
        _ => None,
    }
}

fn source_label(source: &SourceRef) -> String {
    match source.kind.as_str() {
        "telegram" => source
            .telegram_user_id
            .map(|id| format!("Telegram {id}"))
            .unwrap_or_else(|| String::from("Telegram")),
        "webdav" => {
            let url = source.webdav_url.as_deref().unwrap_or("WebDAV");
            let user = source.webdav_username.as_deref().unwrap_or("");
            if user.is_empty() {
                url.to_owned()
            } else {
                format!("{url} ({user})")
            }
        }
        other => other.to_owned(),
    }
}

/// 保持合法文件名原样；仅转义跨平台禁止字符、尾随点/空格和 Windows 保留名。
fn safe_server_name(name: &str) -> String {
    if name.is_empty() {
        return String::from("unnamed");
    }

    let trailing_start = name
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (ch != ' ' && ch != '.').then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    let mut out = String::new();
    for (index, ch) in name.char_indices() {
        let forbidden = ch == '%'
            || ch == '/'
            || ch == '\\'
            || ch == '<'
            || ch == '>'
            || ch == ':'
            || ch == '"'
            || ch == '|'
            || ch == '?'
            || ch == '*'
            || ch.is_control()
            || (index >= trailing_start && (ch == ' ' || ch == '.'));
        if forbidden {
            for b in ch.to_string().as_bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(ch);
        }
    }
    let stem = out.split('.').next().unwrap_or("");
    let reserved = matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    );
    if reserved
        || out.eq_ignore_ascii_case(META_FILE)
        || out.eq_ignore_ascii_case(PART_FILE)
        || out == "."
        || out == ".."
    {
        let first = out.as_bytes().first().copied().unwrap_or(b'_');
        out.replace_range(..1, &format!("%{first:02X}"));
    }
    out
}

fn safe_component(value: &str) -> String {
    let safe = safe_server_name(value);
    if safe.len() <= 64 {
        safe
    } else {
        digest_hex(&[value.as_bytes()])
    }
}

fn digest_hex(parts: &[&[u8]]) -> String {
    let mut h = blake2::Blake2bVar::new(16)
        .unwrap_or_else(|_| blake2::Blake2bVar::new(16).expect("blake2 16 字节输出合法"));
    for p in parts {
        h.update(p);
    }
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).ok();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omy_pinned_{}_{}", std::process::id(), name));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("建目录");
        dir
    }

    #[test]
    fn telegram_path_uses_user_chat_message_and_server_name() {
        let root = temp("telegram-path");
        let target = PinnedTarget::new(
            SourceRef::telegram(42),
            "tg:-10088:19",
            "video.mp4",
            "plain12",
            12,
        );
        let p = paths(&root, &target).expect("路径");
        assert_eq!(
            p.data,
            root.join("telegram")
                .join("42")
                .join("-10088")
                .join("19")
                .join("video.mp4"),
            "换机后本地位置序号变化，不应改变永久路径"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn webdav_path_is_stable_but_separates_accounts() {
        let root = temp("webdav-path");
        let a = PinnedTarget::new(
            SourceRef::webdav("https://dav.example.com/root/".into(), "alice".into()),
            "/movies/a.mkv",
            "a.mkv",
            "plain10",
            10,
        );
        let same = a.clone();
        let b = PinnedTarget::new(
            SourceRef::webdav("https://dav.example.com/root/".into(), "bob".into()),
            "/movies/a.mkv",
            "a.mkv",
            "plain10",
            10,
        );
        assert_eq!(
            paths(&root, &a).unwrap().data,
            paths(&root, &same).unwrap().data
        );
        assert_ne!(
            paths(&root, &a).unwrap().data,
            paths(&root, &b).unwrap().data
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn server_filename_is_preserved_or_deterministically_escaped() {
        assert_eq!(safe_server_name("movie.mkv"), "movie.mkv");
        assert_eq!(safe_server_name(" leading.txt"), " leading.txt");
        assert_eq!(safe_server_name("a:b?.txt"), "a%3Ab%3F.txt");
        assert_eq!(safe_server_name("tail. "), "tail%2E%20");
        assert_eq!(safe_server_name("CON.txt"), "%43ON.txt");
        assert_eq!(safe_server_name(".OMY-PIN.JSON"), "%2EOMY-PIN.JSON");
        assert_ne!(safe_server_name("tail. "), safe_server_name("tail."));
        assert_ne!(safe_server_name("a:b"), safe_server_name("a%3Ab"));
    }

    #[test]
    fn commit_list_read_and_remove_roundtrip() {
        let root = temp("roundtrip");
        let target =
            PinnedTarget::new(SourceRef::telegram(7), "tg:11:22", "photo.jpg", "plain5", 5);
        let p = prepare(&root, &target).expect("准备");
        std::fs::write(&p.part, b"hello").expect("写 part");
        assert_eq!(commit(&root, &target).expect("提交"), 5);
        assert_eq!(
            read_range(&root, &target, 1, 3).as_deref(),
            Some(&b"ell"[..])
        );
        let entries = list(&root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "photo.jpg");
        assert_eq!(entries[0].used_bytes, 5);
        assert_eq!(
            remove_relative(&root, &entries[0].relative_dir).expect("移除"),
            5
        );
        assert!(list(&root).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn mismatched_metadata_and_data_are_not_reused_after_merge_conflict() {
        let root = temp("merge-conflict");
        let old = PinnedTarget::new(
            SourceRef::telegram(7),
            "tg:11:22",
            "photo.jpg",
            "old-version",
            5,
        );
        let p = prepare(&root, &old).expect("准备旧版本");
        std::fs::write(&p.part, b"hello").expect("写旧版本");
        commit(&root, &old).expect("提交旧版本");

        let new = PinnedTarget::new(
            SourceRef::telegram(7),
            "tg:11:22",
            "photo.jpg",
            "new-version",
            6,
        );
        let meta = serde_json::to_vec_pretty(&PinMeta::from_target(&new)).expect("新元数据");
        std::fs::write(&p.meta, meta).expect("模拟合并工具只覆盖元数据");

        assert!(
            candidate_path(&root, &new.source, &new.item_id, new.total_size).is_none(),
            "数据与元数据不是同一版本时必须回源，不能把合并冲突当成完整永久文件"
        );
        assert!(
            complete_path(&root, &new).is_none(),
            "正式版本校验也必须拒绝数据/元数据错配"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn remove_relative_rejects_escape() {
        let root = temp("escape");
        assert!(remove_relative(&root, "../outside").is_err());
        assert!(remove_relative(&root, "C:/outside").is_err());
        assert!(remove_relative(&root, "unknown/x").is_err());
        std::fs::remove_dir_all(root).ok();
    }
}
