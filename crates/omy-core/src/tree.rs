//! 树形模式：逐文件加密并保持目录结构（文档 05 §3，决策 D-05）。
//!
//! # 与容器模式的关系
//!
//! 这是文档 05 §1 里两种文件夹模式中的**模式 B**。容器模式
//! （[`crate::pack`]）把整棵树塞进一个 `.omy`，树形模式在磁盘上保留结构：
//!
//! ```text
//! 明文               容器模式              树形模式
//! 工作资料/     →   a4332ff0.omy    →    MFZWIZLTOQ.omy/
//! ├── 文档/          （单个文件）          ├── NBSWY3DPFQ.omy/
//! │   └── 报告.docx                       │   └── ONXW2ZLUNBSGK.omy
//! └── 照片/                               └── PB2GK3TUN5XA.omy/
//! ```
//!
//! 取舍不是「哪个更好」，两个方向都有人需要（所以 D-05 决定都做）：
//!
//! | | 容器 | 树形 |
//! |---|---|---|
//! | 目录结构 | 完全隐藏 | **泄露文件数、大小、树深度** |
//! | 改一个文件 | 重写整包 | 只重写那一个 |
//! | 云盘同步 | 每次全量 | 增量 |
//! | 单独分享一个文件 | 不行 | 可以 |
//! | 损坏影响面 | 头部坏了全丢 | 只影响那一个 |
//!
//! # 泄露的元数据必须告知用户（N6）
//!
//! 3.2 GB、1247 个文件的目录用树形模式加密后，旁观者能数出文件个数、看出
//! 每个文件的大致尺寸、看出目录有几层。看不到名字和内容，但**看得到形状**。
//! 「照片/2023/私人/」下躺着 400 个 2–5 MB 的文件，即使名字全是乱码也已经
//! 说明了不少事。
//!
//! 这属于威胁模型 N6 已声明的非目标。UI 必须直说，不能让用户以为选了加密
//! 就什么都藏住了——[`TreeReport::leak_notice`] 提供统一的告知文案。
//!
//! # 为什么遍历逻辑抽出来共用
//!
//! 打包时的几个决策——按名字稳定排序、用 `symlink_metadata` 而不是
//! `metadata`、跳过符号链接与非普通文件、深度上限——两种模式必须完全一致。
//! 各写一份的话，哪天给容器模式修了「跳过 socket 文件」，树形模式还在按
//! 老样子处理，同一个目录用两种模式加密会得到不同的内容集合。
//!
//! 所以 [`walk_dir`] 由本模块与 [`crate::pack`] 共用，判定规则只有一处。

use crate::crypto::{CipherId, Kek, NONCE_LEN};
use crate::dirname::{
    DIRNAME_SIDECAR, DirnameKey, EncryptedDirname, decrypt_dirname, encrypt_dirname,
    looks_encrypted,
};
use crate::error::{Error, Result};
use crate::file::{EncryptOptions, RandomMaterial};
use crate::pack::{ProgressFn, SkipReason, SkippedEntry, walk_dir};
use std::path::{Path, PathBuf};

/// 树形模式加密的结果。
#[derive(Debug, Default)]
pub struct TreeReport {
    /// 加密后的根目录（磁盘路径，名字已加密）。
    pub root: PathBuf,
    /// 成功加密的文件数。
    pub files: usize,
    /// 创建的目录数。
    pub dirs: usize,
    /// 需要边车文件才能还原的超长目录名数量。
    ///
    /// 单独报出来是因为它有运维含义：这些目录一旦丢了 `.omy-name`
    /// 就再也认不出原名，用户可能想缩短目录层级重来一次。
    pub long_names: usize,
    /// 被跳过的条目，需如实展示。
    pub skipped: Vec<SkippedEntry>,
}

impl TreeReport {
    /// 树形模式泄露元数据的统一告知文案（N6）。
    ///
    /// 放在 core 而不是各前端：CLI 与 GUI 说法不一致会让人以为是两种不同的
    /// 行为。措辞刻意具体——「泄露元数据」这种说法用户无法据此判断风险，
    /// 「别人能数出你有多少文件」才能。
    #[must_use]
    pub const fn leak_notice() -> &'static str {
        "逐个加密模式下，别人能看到你有多少文件、每个文件多大、目录有几层，但看不到文件名和内容。"
    }
}

/// 加密一棵目录树。
///
/// `out_parent` 是加密后根目录的**父目录**；根目录名本身会被加密。
///
/// # 内存
///
/// 与容器模式不同，这里**逐个文件**读取和加密，内存占用是单个最大文件的
/// 大小，而不是整棵树。这是树形模式的一个实际优势：容器模式打包 50 GB
/// 目录需要 50 GB 内存，树形模式不需要。
///
/// # Errors
///
/// - 根路径不是目录
/// - 目录名加密失败
/// - 写盘失败（磁盘满、权限不足）
///
/// 单个文件读不出来**不算错误**，记进 `skipped` 继续——与容器模式一致。
pub fn encrypt_tree(
    root: &Path,
    out_parent: &Path,
    keks: &[Kek],
    vault_salt: &[u8; 16],
    opts: &EncryptOptions,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<TreeReport> {
    if !root.is_dir() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "encrypt_tree expects a directory",
        )));
    }
    if keks.is_empty() {
        return Err(Error::TooManySlots { got: 0, max: 8 });
    }

    let kek = keks.first().ok_or(Error::TooManySlots { got: 0, max: 8 })?;
    let dkey = DirnameKey::derive(kek, vault_salt);

    let root_name = root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("folder"));

    let mut rep = TreeReport::default();

    // 根目录名也要加密，否则最外层直接暴露「工作资料」
    let enc_root = encrypt_dirname(&root_name, &dkey, &random_nonce(), opts.cipher)?;
    let out_root = out_parent.join(&enc_root.disk_name);
    create_encrypted_dir(&out_root, &enc_root, &mut rep)?;
    rep.root = out_root.clone();

    // 明文相对路径 → 密文磁盘路径。目录必须先建好，子项才知道该往哪写；
    // 而 walk_dir 保证父目录先于子项被访问，所以查表时一定已经有值
    let mut mapping: std::collections::HashMap<Vec<String>, PathBuf> =
        std::collections::HashMap::new();
    mapping.insert(Vec::new(), out_root.clone());

    let mut walk_skipped = Vec::new();
    walk_dir(root, &mut walk_skipped, &mut |item| {
        let Some(parent_rel) = item.comps.split_last().map(|(_, p)| p.to_vec()) else {
            return Ok(());
        };
        let Some(parent_out) = mapping.get(&parent_rel).cloned() else {
            // 父目录不在表里说明它被跳过了（读不出来 / 太深），
            // 子项自然也进不去。如实记录而不是静默丢弃
            rep.skipped.push(SkippedEntry {
                path: item.comps.join("/"),
                reason: SkipReason::Unreadable,
            });
            return Ok(());
        };
        let Some(name) = item.comps.last() else {
            return Ok(());
        };

        if item.is_dir {
            let enc = encrypt_dirname(name, &dkey, &random_nonce(), opts.cipher)?;
            let dir_out = parent_out.join(&enc.disk_name);
            create_encrypted_dir(&dir_out, &enc, &mut rep)?;
            mapping.insert(item.comps.clone(), dir_out);
        } else {
            let Ok(data) = std::fs::read(&item.path) else {
                rep.skipped.push(SkippedEntry {
                    path: item.comps.join("/"),
                    reason: SkipReason::Unreadable,
                });
                return Ok(());
            };
            let size = data.len() as u64;

            // 每个文件独立加密：独立 FEK、独立 file_uuid。
            // 文件名写进它自己的 TLV，磁盘名只保留密文哈希——
            // 文件名不需要 base32 那套（有 TLV 可用），用哈希更短
            let mut fopts = opts.clone();
            fopts.filename = Some(name.clone());
            let rnd = RandomMaterial::generate();
            let enc = crate::file::encrypt(&data, keks, vault_salt, &fopts, &rnd)?;

            // 磁盘名取 file_uuid 的十六进制：唯一、定长、不泄露原名长度。
            // 不用文件名的哈希：同名文件会得到同名密文，泄露「这两个文件同名」
            let disk_name = format!("{}.omy", hex16(&rnd.file_uuid));
            crate::fsatomic::write_atomic(&parent_out.join(&disk_name), &enc.bytes)?;

            rep.files = rep.files.saturating_add(1);
            if let Some(cb) = progress.as_deref_mut() {
                cb(&item.comps.join("/"), size);
            }
        }
        Ok(())
    })?;

    rep.skipped.extend(walk_skipped);
    Ok(rep)
}

/// 建目录并在需要时写入边车文件。
fn create_encrypted_dir(path: &Path, enc: &EncryptedDirname, rep: &mut TreeReport) -> Result<()> {
    std::fs::create_dir_all(path)?;
    rep.dirs = rep.dirs.saturating_add(1);
    // 超长名的完整密文必须落盘，否则该目录名永远无法还原。
    // 这一步失败要报错而不是忽略——忽略的话用户拿到一个解不开名字的目录
    if let Some(sc) = &enc.sidecar {
        crate::fsatomic::write_atomic(&path.join(DIRNAME_SIDECAR), sc)?;
        rep.long_names = rep.long_names.saturating_add(1);
    }
    Ok(())
}

/// 还原一棵加密目录树。
///
/// `enc_root` 是密文根目录，`out_parent` 是还原目标的父目录。
///
/// # Errors
///
/// - `enc_root` 不是目录，或名字不像密文目录名
/// - 目录名解密失败（密码不对）
/// - 写盘失败
pub fn decrypt_tree(
    enc_root: &Path,
    out_parent: &Path,
    keks: &[Kek],
    vault_salt: &[u8; 16],
    cipher: CipherId,
    mut progress: Option<ProgressFn<'_>>,
) -> Result<TreeReport> {
    if !enc_root.is_dir() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "decrypt_tree expects a directory",
        )));
    }
    let kek = keks.first().ok_or(Error::TooManySlots { got: 0, max: 8 })?;
    let dkey = DirnameKey::derive(kek, vault_salt);

    let enc_name = enc_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let plain_root = decrypt_dir_name_at(enc_root, &enc_name, &dkey, cipher)?;

    let out_root = out_parent.join(&plain_root);
    std::fs::create_dir_all(&out_root)?;

    let mut rep = TreeReport { root: out_root.clone(), dirs: 1, ..TreeReport::default() };
    decrypt_into(enc_root, &out_root, keks, &dkey, cipher, &mut rep, &mut progress)?;
    Ok(rep)
}

/// 递归还原一层。
///
/// 用递归而不是显式栈：还原时的深度受**密文目录树**限制，而那棵树是我们
/// 自己按 `MAX_DEPTH` 建出来的，不会超。打包侧面对的是用户给的任意目录，
/// 才需要显式栈防爆栈。
fn decrypt_into(
    enc_dir: &Path,
    out_dir: &Path,
    keks: &[Kek],
    dkey: &DirnameKey,
    cipher: CipherId,
    rep: &mut TreeReport,
    progress: &mut Option<ProgressFn<'_>>,
) -> Result<()> {
    let rd = std::fs::read_dir(enc_dir)?;
    let mut items: Vec<_> = rd.flatten().collect();
    items.sort_by_key(std::fs::DirEntry::file_name);

    for item in items {
        let name = item.file_name().to_string_lossy().into_owned();
        // 边车文件是我们自己的记账，不属于用户数据
        if name == DIRNAME_SIDECAR {
            continue;
        }
        let path = item.path();
        let Ok(md) = path.symlink_metadata() else {
            rep.skipped.push(SkippedEntry {
                path: name,
                reason: SkipReason::Unreadable,
            });
            continue;
        };

        if md.is_dir() {
            let plain = decrypt_dir_name_at(&path, &name, dkey, cipher)?;
            let sub_out = out_dir.join(&plain);
            std::fs::create_dir_all(&sub_out)?;
            rep.dirs = rep.dirs.saturating_add(1);
            decrypt_into(&path, &sub_out, keks, dkey, cipher, rep, progress)?;
        } else if md.is_file() {
            let Ok(bytes) = std::fs::read(&path) else {
                rep.skipped.push(SkippedEntry {
                    path: name,
                    reason: SkipReason::Unreadable,
                });
                continue;
            };
            // 不是 omy 文件就跳过：用户可能自己往密文目录里放了东西，
            // 拿它去解密只会得到一堆认证失败的噪音
            if !crate::file::is_omy_file(&bytes) {
                rep.skipped.push(SkippedEntry {
                    path: name,
                    reason: SkipReason::NotRegular,
                });
                continue;
            }
            let opened = crate::file::open(&bytes, keks)?;
            let plain_name = opened.filename()?;
            let data = opened.decrypt_all(&bytes)?;
            let size = data.len() as u64;

            // 文件名来自密文内部，可能含路径分隔符或 `..`——那是攻击手法。
            // 必须过 sanitize，与容器模式共用同一套规则
            let safe = crate::unpack::sanitize_filename(&plain_name);
            crate::fsatomic::write_atomic(&out_dir.join(&safe), &data)?;
            rep.files = rep.files.saturating_add(1);
            if let Some(cb) = progress.as_deref_mut() {
                cb(&safe, size);
            }
        }
    }
    Ok(())
}

/// 解密一个目录的名字，必要时读它内部的边车文件。
fn decrypt_dir_name_at(
    dir: &Path,
    disk_name: &str,
    dkey: &DirnameKey,
    cipher: CipherId,
) -> Result<String> {
    if !looks_encrypted(disk_name) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "directory name is not an encrypted name",
        });
    }
    // 只有截断过的名字才需要边车文件。无条件去读会让每个目录多一次
    // 失败的 open 系统调用，大树上是可观的开销
    let sidecar = if disk_name.contains('~') {
        Some(std::fs::read(dir.join(DIRNAME_SIDECAR))?)
    } else {
        None
    };
    decrypt_dirname(disk_name, sidecar.as_deref(), dkey, cipher)
}

/// 生成随机 nonce。
///
/// 走 [`crate::util::fill_random`] 而不是自己碰 `rand_core`：熵源不可用时
/// 的处置策略（快速失败而非降级）只该有一处决定。
fn random_nonce() -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    crate::util::fill_random(&mut n);
    n
}

/// 在密文树里找任意一个加密文件，用来取 vault 参数。
///
/// # 为什么需要它
///
/// 单个 `.omy` 文件的头部自带 `vault_salt` 与 KDF 参数，而**目录没有头部**。
/// 可是解目录名又必须先有 KEK，KEK 又要靠 salt 和参数才能派生——鸡生蛋。
/// 解法是从树里任取一个文件读它的头部：同一个 vault 内这些参数本就一致，
/// 取哪个都一样。
///
/// 三个入口都要用（CLI 解目录、GUI 加密后自动解锁、GUI 浏览解目录名），
/// 所以放在 core 里只留一份。新增判定（比如将来要跳过别的边车文件）
/// 只需改这里。
///
/// 深度优先向下找：根目录下可能只有子目录而没有直接的文件。
#[must_use]
pub fn find_any_file(root: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.extension().is_some_and(|x| x == "omy") {
            // 边车文件叫 `.omy-name`，扩展名不是 `omy`，所以不会被误取。
            // 但它确实以 `.omy` 开头，靠「名字含 .omy」判断就会中招
            return Some(p);
        }
    }
    dirs.into_iter().find_map(|d| find_any_file(&d))
}

/// 16 字节转小写十六进制。
fn hex16(b: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for byte in b {
        // 手写而不用 format! 循环：这在每个文件上都要跑一次
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let hi = usize::from(byte >> 4);
        let lo = usize::from(byte & 0x0F);
        s.push(char::from(*HEX.get(hi).unwrap_or(&b'0')));
        s.push(char::from(*HEX.get(lo).unwrap_or(&b'0')));
    }
    s
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::indexing_slicing, reason = "测试里用断言即可")]
mod tests {
    use super::*;
    use crate::crypto::Argon2Params;

    fn kek() -> Kek {
        Kek::from_password(b"pw", &[3u8; 16], Argon2Params::TEST_WEAK).unwrap()
    }

    /// 造一棵测试树。
    fn make_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("omy-tree-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("文档/草稿")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("a.txt"), b"content of a").unwrap();
        std::fs::write(root.join("文档/报告.docx"), b"report body").unwrap();
        std::fs::write(root.join("文档/草稿/draft.md"), b"# draft").unwrap();
        root
    }

    fn opts() -> EncryptOptions {
        EncryptOptions { argon2: Argon2Params::TEST_WEAK, ..EncryptOptions::default() }
    }

    #[test]
    fn find_any_file_skips_the_name_sidecar() {
        // 边车文件叫 `.omy-name`，它以 `.omy` 开头但不是加密文件。
        // 把它当成样本去读头部只会失败，而失败的表现是「解不开目录名」，
        // 会被误认为密码不对。
        //
        // 这条同时守「只有子目录时要能往下找」：根目录下没有直接的文件。
        let root = std::env::temp_dir().join("omy-find-any-sidecar");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        // 边车放在**上层**，真文件放在下层：如果实现按广度优先且不排除
        // 边车，就会先撞上边车
        std::fs::write(root.join(crate::dirname::DIRNAME_SIDECAR), b"not a file").unwrap();
        std::fs::write(root.join("sub/aabb.omy"), b"pretend header").unwrap();

        let got = find_any_file(&root).expect("应当找到子目录里的 .omy");
        assert_eq!(
            got.file_name().and_then(|s| s.to_str()),
            Some("aabb.omy"),
            "拿到的必须是真加密文件而不是边车，实际 {got:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn find_any_file_returns_none_for_tree_without_files() {
        // 只有目录没有文件时必须返回 None 而不是随便给一个路径。
        // 给错的话调用方会拿它去读头部，得到一个含糊的 IO 错误
        let root = std::env::temp_dir().join("omy-find-any-empty");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        assert!(find_any_file(&root).is_none(), "空树应当返回 None");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn roundtrips_whole_tree() {
        let src = make_tree("round");
        let enc_parent = std::env::temp_dir().join("omy-tree-round-enc");
        let dec_parent = std::env::temp_dir().join("omy-tree-round-dec");
        let _ = std::fs::remove_dir_all(&enc_parent);
        let _ = std::fs::remove_dir_all(&dec_parent);
        std::fs::create_dir_all(&enc_parent).unwrap();
        std::fs::create_dir_all(&dec_parent).unwrap();

        let ks = [kek()];
        let rep = encrypt_tree(&src, &enc_parent, &ks, &[3u8; 16], &opts(), None).unwrap();
        assert_eq!(rep.files, 3, "三个文件都要加密");
        assert_eq!(rep.dirs, 4, "根 + 文档 + 草稿 + empty");

        let out = decrypt_tree(
            &rep.root,
            &dec_parent,
            &ks,
            &[3u8; 16],
            CipherId::ChaCha20Poly1305,
            None,
        )
        .unwrap();

        // 逐个核对内容，而不只看文件数——数量对但内容串位是最难查的形态
        assert_eq!(
            std::fs::read(out.root.join("a.txt")).unwrap(),
            b"content of a"
        );
        assert_eq!(
            std::fs::read(out.root.join("文档/报告.docx")).unwrap(),
            b"report body"
        );
        assert_eq!(
            std::fs::read(out.root.join("文档/草稿/draft.md")).unwrap(),
            b"# draft"
        );
        assert!(out.root.join("empty").is_dir(), "空目录也要还原");
        assert_eq!(
            out.root.file_name().unwrap().to_string_lossy(),
            "omy-tree-round",
            "根目录名要还原成原名"
        );

        for p in [&src, &enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn no_plaintext_name_appears_on_disk() {
        // 这是树形模式的核心承诺：结构可见但名字不可见。
        // 遍历密文树的所有路径，确认没有任何明文名残留
        let src = make_tree("names");
        let enc_parent = std::env::temp_dir().join("omy-tree-names-enc");
        let _ = std::fs::remove_dir_all(&enc_parent);
        std::fs::create_dir_all(&enc_parent).unwrap();

        let ks = [kek()];
        let rep = encrypt_tree(&src, &enc_parent, &ks, &[3u8; 16], &opts(), None).unwrap();

        let mut all = Vec::new();
        collect_paths(&rep.root, &mut all);
        let joined = all.join("|");
        for leaked in ["文档", "草稿", "报告", "draft", "a.txt", "empty"] {
            assert!(
                !joined.contains(leaked),
                "明文名 {leaked} 出现在密文树的路径里：{joined}"
            );
        }

        for p in [&src, &enc_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn structure_is_preserved_and_therefore_leaked() {
        // 反证：树形模式**故意**保留结构。这条断言存在的意义是让
        // 「泄露了什么」变成可验证的事实，而不是文档里一句提醒——
        // 哪天有人以为这个模式也藏结构，这条会告诉他不是
        let src = make_tree("leak");
        let enc_parent = std::env::temp_dir().join("omy-tree-leak-enc");
        let _ = std::fs::remove_dir_all(&enc_parent);
        std::fs::create_dir_all(&enc_parent).unwrap();

        let ks = [kek()];
        let rep = encrypt_tree(&src, &enc_parent, &ks, &[3u8; 16], &opts(), None).unwrap();

        // 不解密也能数出：3 个文件、4 层结构
        let mut files = 0;
        let mut dirs = 0;
        count(&rep.root, &mut files, &mut dirs);
        assert_eq!(files, 3, "文件数对外可见——这正是 N6 声明的泄露");
        assert_eq!(dirs, 4, "目录数同样可见");

        // 文案必须具体到「能数出多少文件」，而不是抽象的「泄露元数据」
        let notice = TreeReport::leak_notice();
        assert!(notice.contains("多少文件"), "告知文案要说清泄露什么");
        assert!(notice.contains("看不到文件名"), "也要说清藏住了什么");

        for p in [&src, &enc_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn wrong_password_cannot_decrypt_tree() {
        let src = make_tree("wrongpw");
        let enc_parent = std::env::temp_dir().join("omy-tree-wrongpw-enc");
        let dec_parent = std::env::temp_dir().join("omy-tree-wrongpw-dec");
        for p in [&enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
            std::fs::create_dir_all(p).unwrap();
        }

        let rep =
            encrypt_tree(&src, &enc_parent, &[kek()], &[3u8; 16], &opts(), None).unwrap();

        let bad = Kek::from_password(b"wrong", &[3u8; 16], Argon2Params::TEST_WEAK).unwrap();
        assert!(
            decrypt_tree(
                &rep.root,
                &dec_parent,
                &[bad],
                &[3u8; 16],
                CipherId::ChaCha20Poly1305,
                None
            )
            .is_err(),
            "错密码必须解不开"
        );

        for p in [&src, &enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn same_content_files_get_different_disk_names() {
        // 磁盘名取 file_uuid 而不是名字/内容的哈希。若取后者，
        // 两个同名或同内容的文件会得到相同磁盘名——既撞名，
        // 又泄露「这两个文件一样」
        let root = std::env::temp_dir().join("omy-tree-dup");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("d1")).unwrap();
        std::fs::create_dir_all(root.join("d2")).unwrap();
        std::fs::write(root.join("d1/same.txt"), b"identical").unwrap();
        std::fs::write(root.join("d2/same.txt"), b"identical").unwrap();

        let enc_parent = std::env::temp_dir().join("omy-tree-dup-enc");
        let _ = std::fs::remove_dir_all(&enc_parent);
        std::fs::create_dir_all(&enc_parent).unwrap();

        let rep =
            encrypt_tree(&root, &enc_parent, &[kek()], &[3u8; 16], &opts(), None).unwrap();
        let mut names = Vec::new();
        collect_file_names(&rep.root, &mut names);
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1], "同名同内容的文件不该得到相同磁盘名");

        for p in [&root, &enc_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn long_directory_name_survives_roundtrip() {
        // 超长目录名走截断 + 边车文件那条路。这条端到端确认边车真的
        // 被写进去且被读出来——单测里 encrypt_dirname 返回 sidecar
        // 不代表调用方写了它
        let root = std::env::temp_dir().join("omy-tree-long");
        let _ = std::fs::remove_dir_all(&root);
        let long = "很长的目录名".repeat(35);
        std::fs::create_dir_all(root.join(&long)).unwrap();
        std::fs::write(root.join(&long).join("x.txt"), b"deep").unwrap();

        let enc_parent = std::env::temp_dir().join("omy-tree-long-enc");
        let dec_parent = std::env::temp_dir().join("omy-tree-long-dec");
        for p in [&enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
            std::fs::create_dir_all(p).unwrap();
        }

        let rep =
            encrypt_tree(&root, &enc_parent, &[kek()], &[3u8; 16], &opts(), None).unwrap();
        assert!(rep.long_names >= 1, "该报告有超长目录名");

        let out = decrypt_tree(
            &rep.root,
            &dec_parent,
            &[kek()],
            &[3u8; 16],
            CipherId::ChaCha20Poly1305,
            None,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(out.root.join(&long).join("x.txt")).unwrap(),
            b"deep",
            "超长目录名要能完整还原"
        );

        for p in [&root, &enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn sidecar_file_is_not_restored_as_user_data() {
        // 边车文件是我们自己的记账。还原时把它当用户文件写出来，
        // 用户会在自己的目录里看到一个莫名的 .omy-name
        let root = std::env::temp_dir().join("omy-tree-sc");
        let _ = std::fs::remove_dir_all(&root);
        let long = "长目录名".repeat(45);
        std::fs::create_dir_all(root.join(&long)).unwrap();
        std::fs::write(root.join(&long).join("f.txt"), b"x").unwrap();

        let enc_parent = std::env::temp_dir().join("omy-tree-sc-enc");
        let dec_parent = std::env::temp_dir().join("omy-tree-sc-dec");
        for p in [&enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
            std::fs::create_dir_all(p).unwrap();
        }

        let rep =
            encrypt_tree(&root, &enc_parent, &[kek()], &[3u8; 16], &opts(), None).unwrap();
        let out = decrypt_tree(
            &rep.root,
            &dec_parent,
            &[kek()],
            &[3u8; 16],
            CipherId::ChaCha20Poly1305,
            None,
        )
        .unwrap();

        let mut all = Vec::new();
        collect_paths(&out.root, &mut all);
        assert!(
            !all.iter().any(|p| p.contains(DIRNAME_SIDECAR)),
            "还原结果里不该出现边车文件：{all:?}"
        );
        // 上面那条断言不够：把 `name == DIRNAME_SIDECAR` 的跳过删掉后，
        // 边车文件会被 is_omy_file 拦下、不会写出去，所以那条依然通过。
        // 但它会以 NotRegular 出现在 skipped 里——用户看到一个莫名的
        // 「跳过了 .omy-name」，会以为自己的数据丢了东西。
        // 干净的树还原后 skipped 必须是空的
        assert!(
            out.skipped.is_empty(),
            "干净的树还原后不该有跳过项，实际 {:?}",
            out.skipped
        );

        for p in [&root, &enc_parent, &dec_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn malicious_filename_in_ciphertext_cannot_escape() {
        // 文件名来自**密文内部**，是攻击者可控的：拿到密钥的人（或另一个
        // 实现）可以造一个 TLV_FILENAME 写着 `../../evil.txt` 的文件。
        // 还原时若不过 sanitize，写盘位置就跑到目标目录之外去了。
        //
        // 变异测试抓出来的缺口：原有的 roundtrip 测试用的都是正常文件名，
        // 把 sanitize 那行删掉照样全绿。这条断言必须用真正带 `../` 的名字。
        //
        // 逃逸落点必须留在自己的沙盒里。第一版把 dec_parent 直接放在
        // 系统临时目录下，于是变异跑的时候真的在 %TEMP% 根上写出了
        // `escaped.txt`——它留在那儿，导致**还原源码后这条测试依然失败**，
        // 看起来像产品有问题。所以套一层 sandbox/a/b：`../..` 最远只能
        // 逃到 sandbox 内，随 remove_dir_all 一起清掉。
        let sandbox = std::env::temp_dir().join("omy-tree-evil-sandbox");
        let _ = std::fs::remove_dir_all(&sandbox);
        let enc_parent = sandbox.join("enc");
        let dec_parent = sandbox.join("a/b");
        for p in [&enc_parent, &dec_parent] {
            std::fs::create_dir_all(p).unwrap();
        }

        // 先正常加密一棵树，拿到密文根目录
        let src = make_tree("evil");
        let ks = [kek()];
        let rep = encrypt_tree(&src, &enc_parent, &ks, &[3u8; 16], &opts(), None).unwrap();

        // 手工造一个文件名带路径逃逸的加密文件，塞进密文目录
        let mut evil = opts();
        evil.filename = Some(String::from("../../escaped.txt"));
        let rnd = RandomMaterial::generate();
        let ef = crate::file::encrypt(b"pwned", &ks, &[3u8; 16], &evil, &rnd).unwrap();
        std::fs::write(rep.root.join("deadbeef.omy"), &ef.bytes).unwrap();

        let out = decrypt_tree(
            &rep.root,
            &dec_parent,
            &ks,
            &[3u8; 16],
            CipherId::ChaCha20Poly1305,
            None,
        )
        .unwrap();

        // 还原根目录之外的每一层都不能出现那个文件
        for up in [sandbox.join("a"), sandbox.clone(), dec_parent.clone()] {
            let leaked = up.join("escaped.txt");
            assert!(!leaked.exists(), "文件逃出了还原根目录：{}", leaked.display());
        }
        // 内容应当落在还原根目录内，名字里的分隔符被替换掉
        let mut names = Vec::new();
        collect_file_names(&out.root, &mut names);
        assert!(
            names.iter().any(|n| n.contains("escaped.txt") && !n.contains('/')),
            "逃逸名应被清洗后留在根目录内，实际 {names:?}"
        );

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&sandbox);
    }

    #[test]
    fn rejects_non_directory() {
        let f = std::env::temp_dir().join("omy-tree-not-dir.txt");
        std::fs::write(&f, b"x").unwrap();
        assert!(
            encrypt_tree(&f, &std::env::temp_dir(), &[kek()], &[3u8; 16], &opts(), None).is_err(),
            "对文件调用必须报错"
        );
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn progress_reports_every_file() {
        let src = make_tree("prog");
        let enc_parent = std::env::temp_dir().join("omy-tree-prog-enc");
        let _ = std::fs::remove_dir_all(&enc_parent);
        std::fs::create_dir_all(&enc_parent).unwrap();

        let mut seen = Vec::new();
        let mut cb = |n: &str, s: u64| seen.push((n.to_owned(), s));
        let rep =
            encrypt_tree(&src, &enc_parent, &[kek()], &[3u8; 16], &opts(), Some(&mut cb)).unwrap();
        assert_eq!(seen.len(), rep.files, "每个文件都要回调一次");

        for p in [&src, &enc_parent] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    // ---- 测试辅助 ----

    fn collect_paths(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            out.push(e.file_name().to_string_lossy().into_owned());
            if e.path().is_dir() {
                collect_paths(&e.path(), out);
            }
        }
    }

    fn collect_file_names(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if e.path().is_dir() {
                collect_file_names(&e.path(), out);
            } else {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }

    fn count(dir: &Path, files: &mut usize, dirs: &mut usize) {
        *dirs = dirs.saturating_add(1);
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            if e.path().is_dir() {
                count(&e.path(), files, dirs);
            } else {
                *files = files.saturating_add(1);
            }
        }
    }
}
