//! 目录扫描：找出 .omy 文件并用已缓存的凭据自动匹配。
//!
//! # 只读文件头，不读整个文件
//!
//! 这是本模块最重要的性质。识别一个 .omy 文件并尝试解锁，**只需读取开头的
//! 若干百字节**：
//!
//! ```text
//! 读 480 B → 比对 magic (8 B) → 解析固定头 (96 B) → 逐凭据试解 slot 区 (384 B)
//!         → 若需文件名，补读 TLV 区（通常再几十到几百字节）
//! ```
//!
//! 载荷完全不碰。一个 4 GiB 的加密视频与一个 1 KiB 的加密文本，扫描成本相同。
//!
//! ⚠️ 注意 [`MIN_PROBE_SIZE`]（480）与 `file::PEEK_SIZE`（96）的区别：
//! 后者只够解析固定头，**不足以尝试解锁**。见 [`MIN_PROBE_SIZE`] 的文档。
//!
//! # 扫描一个文件要花多久
//!
//! 每个文件每个凭据只做一次 HKDF + 一次 AEAD 解包尝试：
//!
//! | 操作 | 耗时 |
//! |---|---|
//! | HKDF-SHA256 | 13.2 µs |
//! | Argon2id（**已被会话缓存，扫描时不执行**） | 180 ms |
//!
//! 实测 500 文件 × 5 密码 = 171 ms。若每个文件都跑 Argon2 则需 450 s（慢 2,627×）。
//!
//! 因此扫描器**必须**接收已解锁的 [`SessionKeys`]，而不是密码。
//!
//! # 不做全盘扫描
//!
//! 按用户要求与平台限制（iOS 根本不允许、Android 的全盘权限过审困难、
//! macOS 需要完全磁盘访问授权），本模块只扫描**调用方明确给定的目录**。

use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::header::{
    FIXED_HEADER_LEN, FixedHeader, HEADER_LEN_FIELD_OFFSET, HEADER_MAC_LEN, MAGIC_FILE,
    SLOT_AREA_OFFSET, TLV_AREA_OFFSET, parse_limits,
};
use crate::session::{CredentialKind, SessionKeys};
use crate::slot::unwrap_fek;
use crate::tlv::{TlvSet, types};

/// 识别一个文件至少需要读取的字节数：固定头 + slot 区 = 480。
///
/// # 为什么不是 `file::PEEK_SIZE`
///
/// `file::PEEK_SIZE` 等于 [`FIXED_HEADER_LEN`]（96），只够解析固定头。
/// 而**判断能否解锁**必须读到 slot 区（偏移 96..480）。若只读 96 字节就去解包
/// slot，会得到 `Truncated` 而被误判为「文件损坏」——实际文件完好无损。
pub const MIN_PROBE_SIZE: usize = TLV_AREA_OFFSET;

/// omy 文件的最小可能大小：完整头部（含空 TLV 与 MAC）。
pub const MIN_FILE_SIZE: usize = TLV_AREA_OFFSET + HEADER_MAC_LEN;

/// 单个已识别文件的扫描结果。
#[derive(Debug)]
pub struct ScanHit {
    /// 文件路径。
    pub path: PathBuf,
    /// 文件总大小（来自文件系统，未读取内容）。
    pub file_size: u64,
    /// 解析出的固定头。
    pub header: FixedHeader,
    /// 匹配结果。
    pub unlock: UnlockOutcome,
}

/// 解锁尝试的结果。
#[derive(Debug)]
pub enum UnlockOutcome {
    /// 某个凭据成功解开。
    Unlocked {
        /// 命中的凭据名称。
        credential: String,
        /// 凭据类型。
        kind: CredentialKind,
        /// 命中的 slot 序号。
        slot_index: usize,
        /// 解出的原始文件名（若文件加密了文件名且 TLV 可读）。
        filename: Option<String>,
        /// 明文大小。
        plaintext_size: u64,
    },
    /// 是 .omy 文件，但当前所有凭据都打不开。
    ///
    /// 注意：这**不代表**文件损坏或密码错误——它可能属于另一个密码集。
    /// UI 应表述为「不属于当前密码」而非「密码错误」。
    Locked,
}

impl UnlockOutcome {
    /// 是否成功解锁。
    #[must_use]
    pub const fn is_unlocked(&self) -> bool {
        matches!(self, Self::Unlocked { .. })
    }
}

/// 扫描配置。
#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// 是否递归子目录。
    pub recursive: bool,
    /// 最大递归深度（`recursive` 为真时生效），防止符号链接环导致无限递归。
    pub max_depth: usize,
    /// 是否跟随符号链接。默认 `false`——跟随会带来环路与越权读取风险。
    pub follow_symlinks: bool,
    /// 只检查扩展名匹配的文件。为 `None` 时检查所有文件（伪装文件没有 .omy 扩展名）。
    pub extension_filter: Option<String>,
    /// 单次扫描的文件数上限，防止误选巨大目录导致界面卡死。
    pub max_files: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            recursive: true,
            max_depth: 32,
            follow_symlinks: false,
            // 默认不按扩展名过滤：伪装模式下文件可能叫 .jpg
            extension_filter: None,
            max_files: 100_000,
        }
    }
}

impl ScanOptions {
    /// 只扫描 `.omy` 扩展名，速度更快但会漏掉伪装文件。
    #[must_use]
    pub fn omy_only() -> Self {
        Self {
            extension_filter: Some(crate::FILE_EXTENSION.to_owned()),
            ..Self::default()
        }
    }
}

/// 扫描统计，用于向用户展示进度与结果。
#[derive(Debug, Default, Clone, Copy)]
pub struct ScanStats {
    /// 检查过的文件总数。
    pub files_examined: usize,
    /// 识别为 .omy 的文件数。
    pub omy_found: usize,
    /// 成功解锁的文件数。
    pub unlocked: usize,
    /// 因 I/O 错误跳过的文件数。
    pub io_errors: usize,
    /// 头部损坏或版本不支持的文件数。
    pub malformed: usize,
}

/// 扫描结果集合。
#[derive(Debug, Default)]
pub struct ScanResult {
    /// 所有识别为 .omy 的文件。
    pub hits: Vec<ScanHit>,
    /// 统计信息。
    pub stats: ScanStats,
}

impl ScanResult {
    /// 仅返回成功解锁的条目。
    #[must_use]
    pub fn unlocked(&self) -> Vec<&ScanHit> {
        self.hits.iter().filter(|h| h.unlock.is_unlocked()).collect()
    }
}

/// 从已打开的文件中读取恰好 `n` 字节（允许短读到 EOF）。
fn read_exact_upto(f: &mut File, n: usize) -> std::io::Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    let mut filled = 0;
    while filled < n {
        let n_read = f.read(buf.get_mut(filled..).unwrap_or(&mut []))?;
        if n_read == 0 {
            break;
        }
        filled = filled.saturating_add(n_read);
    }
    buf.truncate(filled);
    Ok(buf)
}

/// 读取识别与解锁所需的头部字节。
///
/// 分两步，避免为小文件读多余数据、也避免为大头部读不够：
/// 1. 先读 [`MIN_PROBE_SIZE`]（480 B）= 固定头 + slot 区
/// 2. 解析出 `header_len` 后，若 TLV 区还没读到，补读剩余部分
///
/// 返回的字节数**至少**覆盖到 slot 区末尾；能读到 TLV 时也一并返回，
/// 这样取文件名无需再次访问磁盘。
fn read_header_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = read_exact_upto(&mut f, MIN_PROBE_SIZE)?;

    // 太短，不可能是 omy 文件
    if buf.len() < FIXED_HEADER_LEN {
        return Ok(buf);
    }
    // magic 不符时直接返回，省掉后续读取
    if buf.get(..MAGIC_FILE.len()) != Some(&MAGIC_FILE[..]) {
        return Ok(buf);
    }

    // 从固定头取 header_len。
    // 这里直接按偏移读而不调用 FixedHeader::parse，是因为 parse 会做完整校验、
    // 需要 slot 区之后的数据；此刻我们只想知道「还要再读多少」。
    let Some(raw) = buf.get(HEADER_LEN_FIELD_OFFSET..HEADER_LEN_FIELD_OFFSET + 4) else {
        return Ok(buf);
    };
    let mut le = [0u8; 4];
    le.copy_from_slice(raw);
    let header_len = u32::from_le_bytes(le) as usize;

    // 上界保护：不可信输入不得驱动大额分配
    if header_len > parse_limits::MAX_HEADER_LEN as usize || header_len <= buf.len() {
        return Ok(buf);
    }

    // 补读 TLV 区与 MAC
    let extra = header_len.saturating_sub(buf.len());
    let mut rest = read_exact_upto(&mut f, extra)?;
    buf.append(&mut rest);
    Ok(buf)
}

/// 对单个文件尝试识别与解锁。
///
/// 返回 `Ok(None)` 表示这不是 .omy 文件（最常见情况，应静默跳过）。
///
/// # Errors
///
/// 文件可识别为 .omy 但头部损坏、版本不支持时返回相应错误。
pub fn probe_file(path: &Path, session: &SessionKeys) -> Result<Option<ScanHit>> {
    let prefix = match read_header_bytes(path) {
        Ok(p) => p,
        Err(e) => {
            return Err(Error::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", path.display()),
            )));
        }
    };

    // 最快的排除路径：magic 不匹配直接走
    if prefix.len() < MAGIC_FILE.len() || prefix.get(..MAGIC_FILE.len()) != Some(&MAGIC_FILE[..]) {
        return Ok(None);
    }

    let header = FixedHeader::parse(&prefix)?;
    let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    // 用会话中适用于本文件 vault_salt 的凭据逐一尝试。
    // 注意：这里只取 scannable 的（vault/device），portable/recovery 成本太高。
    let creds = session.scannable_for(&header.vault_salt);
    let slot_area =
        prefix.get(SLOT_AREA_OFFSET..TLV_AREA_OFFSET).ok_or(Error::Truncated {
            context: "slot area",
            need: TLV_AREA_OFFSET,
            got: prefix.len(),
        })?;

    for cred in &creds {
        let keks = [cred.kek.duplicate()];
        if let Ok((fek, _, slot_index)) =
            unwrap_fek(slot_area, &keks, &header.file_uuid, header.cipher_id)
        {
            // 解锁成功，顺便取出文件名（在同一段已读的前缀里，无需再读盘）
            let filename = read_filename(&prefix, &header, &fek);
            return Ok(Some(ScanHit {
                path: path.to_path_buf(),
                file_size,
                unlock: UnlockOutcome::Unlocked {
                    credential: cred.label.clone(),
                    kind: cred.kind,
                    slot_index: usize::from(slot_index),
                    filename,
                    plaintext_size: header.plaintext_size,
                },
                header,
            }));
        }
    }

    Ok(Some(ScanHit { path: path.to_path_buf(), file_size, header, unlock: UnlockOutcome::Locked }))
}

/// 从已读取的前缀中解出文件名。
///
/// 失败返回 `None`——文件名不是必需项，取不到不应让整个扫描失败。
fn read_filename(prefix: &[u8], header: &FixedHeader, fek: &crate::crypto::Fek) -> Option<String> {
    let tlv_end = (header.header_len as usize).checked_sub(HEADER_MAC_LEN)?;
    let blob = prefix.get(TLV_AREA_OFFSET..tlv_end)?;
    let set = TlvSet::parse(blob).ok()?;
    let plain = set.decrypt_value(types::FILENAME, fek, header.cipher_id).ok()?;
    crate::tlv::unpad_filename(&plain).ok()
}

/// 扫描一个目录。
///
/// # 性能
///
/// 每个文件只读 [`PEEK_SIZE`] 字节。`session` 中的 KEK 已派生完毕，
/// 扫描过程**不执行 Argon2**。
///
/// # Errors
///
/// 目录不存在或不可读时返回 [`Error::Io`]。单个文件的错误会被计入
/// [`ScanStats`] 并跳过，不中断整体扫描——一个坏文件不该让整次扫描失败。
pub fn scan_dir(root: &Path, session: &SessionKeys, opts: &ScanOptions) -> Result<ScanResult> {
    if !root.is_dir() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("not a directory: {}", root.display()),
        )));
    }
    let mut out = ScanResult::default();
    let mut stack = vec![(root.to_path_buf(), 0usize)];

    while let Some((dir, depth)) = stack.pop() {
        if out.stats.files_examined >= opts.max_files {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(_) => {
                // 无权限的目录直接跳过，不中断扫描
                out.stats.io_errors = out.stats.io_errors.saturating_add(1);
                continue;
            }
        };

        for entry in rd.flatten() {
            if out.stats.files_examined >= opts.max_files {
                break;
            }
            let path = entry.path();

            // 用 symlink_metadata 判断类型，避免跟随链接
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                out.stats.io_errors = out.stats.io_errors.saturating_add(1);
                continue;
            };

            if meta.is_symlink() && !opts.follow_symlinks {
                continue;
            }

            let is_dir = if meta.is_symlink() {
                path.is_dir() // 已确认允许跟随
            } else {
                meta.is_dir()
            };

            if is_dir {
                if opts.recursive && depth < opts.max_depth {
                    stack.push((path, depth.saturating_add(1)));
                }
                continue;
            }

            // 扩展名过滤（可选）
            if let Some(want) = opts.extension_filter.as_deref() {
                let matches = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(want));
                if !matches {
                    continue;
                }
            }

            // 小于最小文件大小的直接跳过，省一次 open
            if meta.len() < MIN_FILE_SIZE as u64 {
                continue;
            }

            out.stats.files_examined = out.stats.files_examined.saturating_add(1);

            match probe_file(&path, session) {
                Ok(Some(hit)) => {
                    out.stats.omy_found = out.stats.omy_found.saturating_add(1);
                    if hit.unlock.is_unlocked() {
                        out.stats.unlocked = out.stats.unlocked.saturating_add(1);
                    }
                    out.hits.push(hit);
                }
                Ok(None) => {} // 不是 omy 文件，正常跳过
                Err(Error::Io { .. }) => {
                    out.stats.io_errors = out.stats.io_errors.saturating_add(1);
                }
                Err(_) => {
                    // magic 对但头部有问题——可能是损坏或更新版本
                    out.stats.malformed = out.stats.malformed.saturating_add(1);
                }
            }
        }
    }

    Ok(out)
}

/// 扫描多个目录。
///
/// # Errors
///
/// 所有目录都不可访问时返回最后一个错误；只要有一个成功即返回合并结果。
pub fn scan_dirs(
    roots: &[PathBuf],
    session: &SessionKeys,
    opts: &ScanOptions,
) -> Result<ScanResult> {
    let mut merged = ScanResult::default();
    let mut last_err = None;
    let mut any_ok = false;

    for root in roots {
        match scan_dir(root, session, opts) {
            Ok(r) => {
                any_ok = true;
                merged.hits.extend(r.hits);
                let s = &mut merged.stats;
                s.files_examined = s.files_examined.saturating_add(r.stats.files_examined);
                s.omy_found = s.omy_found.saturating_add(r.stats.omy_found);
                s.unlocked = s.unlocked.saturating_add(r.stats.unlocked);
                s.io_errors = s.io_errors.saturating_add(r.stats.io_errors);
                s.malformed = s.malformed.saturating_add(r.stats.malformed);
            }
            Err(e) => last_err = Some(e),
        }
    }

    if any_ok {
        Ok(merged)
    } else {
        Err(last_err.unwrap_or_else(|| {
            Error::Io(std::io::Error::other("no directories scanned"))
        }))
    }
}
