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

/// 批量改密码的结果。
#[derive(Debug, Default)]
pub struct RekeyReport {
    /// 成功改写的文件数。
    pub changed: usize,
    /// 重新加密了名字的目录数（含根目录）。
    pub dirs_renamed: usize,
    /// 改密码后这棵树的根路径。
    ///
    /// **根目录名会变**：它由 KEK 派生的密钥加密，换密码就得换名字。调用方
    /// 必须用这个值，原来手里的路径已经失效——不返回的话，GUI 刷新列表时
    /// 会指向一个不存在的目录，表现为「改完密码文件夹不见了」。
    pub root: PathBuf,
    /// 失败的：`(路径, 原因)`。
    ///
    /// 逐个记而不是只留一个总的错误码：一棵树里可能只有个别文件出问题
    /// （被别的程序占用、权限不对、单个文件损坏），用户需要知道**是哪些**
    /// 才能去处理，而其余文件已经改好了。
    pub failed: Vec<(PathBuf, String)>,
    /// 是否换掉了文件密钥（轮换）。
    pub payload_rewritten: bool,
    /// 轮换时重写的明文总字节数。非轮换恒为 0。
    pub bytes_rewritten: u64,
}

impl RekeyReport {
    /// 是否整棵树都改成功了。
    #[must_use]
    // 不标 const：Vec::is_empty 在 const 上下文要 Rust 1.87，
    // 本仓库 MSRV 是 1.85
    pub fn is_complete(&self) -> bool {
        self.failed.is_empty()
    }
}

/// 给整棵密文树里的每个文件改密码（增删改），载荷一字节不动。
///
/// `unlock` 用于解开现有文件，`keep` 是改写后应当能打开这些文件的**全部**
/// KEK。语义与 [`crate::keyslot::rewrite_slots`] 完全一致，只是作用于整棵树。
///
/// 目录名也会一并用新密钥重新加密——只改文件会留下一个两个密码都用不了的
/// 状态（新密码开得了文件但解不开目录名，旧密码反之）。所以**根目录名会变**，
/// 新路径在 [`RekeyReport::root`] 里返回。
///
/// # 为什么可以只派生一次 KEK
///
/// 同一棵树里所有文件共用一个 `vault_salt` 与一组 KDF 参数（`encrypt_tree`
/// 把同一个 salt 传给每个文件），所以 Argon2 只需跑一次。这很关键：Argon2
/// 每次几百毫秒，若每个文件都要派生，1000 个文件就是好几分钟。
///
/// 而改写本身只动 384 字节 slot 区 + 32 字节 MAC，所以整棵树的成本约等于
/// 「一次 Argon2 + N 次小文件读写」。
///
/// # 部分失败是**如实报告**，不回滚
///
/// 这是这个操作最需要想清楚的地方。改到第 50 个文件时断电或出错，前 49 个
/// 已经是新密码、后面还是旧密码，于是两个密码各能开一半。
///
/// 三种可能的处理方式，选第三种：
///
/// 1. **回滚**——要先把 N 个文件的原始字节全留一份，一棵大树可能是几十 GB；
///    而且回滚本身也会中途失败，那时状态更难描述。
/// 2. **假装成功**——最糟。用户以为密码已经全换了，实际一半没换；等他哪天
///    用新密码打不开另一半，早已不知道该用哪个旧密码。
/// 3. **如实报告**（本实现）——返回改了哪些、哪些没改。**旧密码不会失效**，
///    所以用户手上一定有一个能打开剩余文件的密码，重跑一次即可补齐。
///
/// 第三种之所以安全，前提是 `keep` 里通常仍包含用户当前用的密码，或者用户
/// 知道旧密码。调用方在文案上必须讲清「哪些文件还是旧密码」，否则用户不知道
/// 该重跑。
///
/// 幂等性：对已经改好的文件重跑会失败（`unlock` 里的旧密码打不开它了），
/// 所以补跑时应当把新旧密码都放进 `unlock`。
///
/// # 为什么不先全部改好再统一落盘
///
/// 那需要把 N 个文件的新字节全部驻留内存。改写只动头部，但 `rewrite_slots`
/// 返回的是**完整文件字节**——一棵 50 GB 的树就是 50 GB 内存。
///
/// # Errors
///
/// - [`Error::MalformedHeader`]：`keep` 为空
/// - [`Error::MalformedHeader`]：`root` 下找不到任何加密文件
///
/// 单个文件的失败不会中断遍历，记进 `failed`。
pub fn rekey_tree(
    root: &Path,
    unlock: &[Kek],
    keep: &[Kek],
    vault_salt: &[u8; 16],
    cipher: CipherId,
) -> Result<RekeyReport> {
    rekey_tree_with_progress(root, unlock, keep, vault_salt, cipher, false, None)
}

/// 进度回调：`(当前文件名, 第几个, 共几个, 该文件已处理字节, 该文件总字节)`。
///
/// 带上文件内的字节进度而不只是「第 i / n 个」：轮换一个 4 GB 的视频要几十
/// 秒，只报文件序号的话进度条会长时间停在同一格，用户无从判断是在跑还是
/// 卡死了。改密码时字节进度恒为 (0, 0)，调用方据此只显示文件序号即可。
pub type RekeyProgressFn<'a> = &'a mut dyn FnMut(&str, usize, usize, u64, u64);

/// 同 [`rekey_tree`]，另外支持轮换文件密钥与进度上报。
///
/// # `rotate` 的代价
///
/// `false` 时只重建每个文件 384 字节的 slot 区，一棵几万文件的树也是秒级。
/// `true` 时要把每个文件的载荷完整读一遍、用新 FEK 写一遍，耗时与总数据量
/// 成正比——几十 GB 的树要跑很久。
///
/// # 为什么轮换值得做
///
/// 改密码只换外层包裹，FEK 没变：**攻击者手里若有旧文件副本，仍能用旧密码
/// 打开那个副本**。只有换掉 FEK 并重写载荷，才能让旧密码与这份数据彻底
/// 无关（已经流出去的副本仍是独立密文，本操作对它无能为力——它永远可以
/// 被旧密码打开，这一点必须让用户知道）。
///
/// # 中途失败不会毁数据
///
/// 每个文件各自 `write_atomic`，所以任一时刻每个文件要么是完整的旧密文、
/// 要么是完整的新密文，不存在写坏一半的文件。若同时换了密码，部分失败会
/// 留下「一半新密码、一半旧密码」——报告里如实列出是哪些，两个密码都提供
/// 就能重跑补齐。
pub fn rekey_tree_with_progress(
    root: &Path,
    unlock: &[Kek],
    keep: &[Kek],
    vault_salt: &[u8; 16],
    cipher: CipherId,
    rotate: bool,
    mut progress: Option<RekeyProgressFn<'_>>,
) -> Result<RekeyReport> {
    if keep.is_empty() {
        return Err(Error::MalformedHeader {
            reason: "refusing to leave files with zero key slots; they could never be opened again",
        });
    }
    // 先确认这确实是一棵密文树。不查的话，对一个普通目录执行会得到
    // 「0 个文件已改写」——看起来像成功，其实什么都没发生
    if find_any_file(root).is_none() {
        return Err(Error::MalformedHeader {
            reason: "no encrypted file found under the given directory",
        });
    }

    // 目录名密钥由**第一个** KEK 派生（与 encrypt_tree / decrypt_tree 一致）。
    // 旧的用来解出原名，新的用来重新加密
    let old_kek = unlock.first().ok_or(Error::TooManySlots { got: 0, max: 8 })?;
    let new_kek = keep.first().ok_or(Error::TooManySlots { got: 0, max: 8 })?;
    let old_dkey = DirnameKey::derive(old_kek, vault_salt);
    let new_dkey = DirnameKey::derive(new_kek, vault_salt);
    // 密钥没变就完全不碰目录名。`add` 保留原密码作第一个 KEK 时正是这种
    // 情形：改名既没必要，又会让调用方以为整棵结构变了，增量备份还要重传
    // 全部目录项
    let dirname_changed = !same_dirname_key(&old_dkey, &new_dkey, cipher);

    let mut rep = RekeyReport { payload_rewritten: rotate, ..RekeyReport::default() };
    // 只有轮换才值得先数一遍：改密码是秒级的，为了显示「第 i / n 个」
    // 多遍历一次目录反而占了操作本身可观的比例
    let total = if rotate { count_files(root) } else { 0 };
    let mut ctx = RekeyCtx {
        unlock,
        keep,
        old_dkey: &old_dkey,
        new_dkey: &new_dkey,
        cipher,
        dirname_changed,
        rotate,
        total,
        done: 0,
    };
    rekey_into(root, &mut ctx, &mut progress, &mut rep);

    // 根目录自己的名字最后改：它一改，调用方手里的 root 路径就失效了。
    // 新路径通过 RekeyReport::root 返回
    if !dirname_changed {
        rep.root = root.to_path_buf();
        return Ok(rep);
    }
    rep.root = match rename_dir(root, &old_dkey, &new_dkey, cipher) {
        Ok(renamed) => {
            if renamed {
                rep.dirs_renamed = rep.dirs_renamed.saturating_add(1);
            }
            find_renamed(root, &new_dkey, cipher).unwrap_or_else(|| root.to_path_buf())
        }
        Err(err) => {
            rep.failed.push((root.to_path_buf(), err));
            root.to_path_buf()
        }
    };
    Ok(rep)
}

/// 两个目录名密钥是否等价。
///
/// `DirnameKey` 不暴露比较（密钥不该随便比），所以用同一个固定 nonce 加密
/// 同一个探针名字，看密文是否一致——AEAD 在密钥、nonce、明文都相同时输出
/// 确定，所以密文相同即密钥相同。
///
/// 探针名字用不可能与真实目录冲突的常量；nonce 固定是刻意的，这里要的正是
/// 确定性，与「加密真实目录名必须用随机 nonce」是两回事。
fn same_dirname_key(a: &DirnameKey, b: &DirnameKey, cipher: CipherId) -> bool {
    const PROBE: &str = "omy-dirname-key-probe";
    let nonce = [0u8; NONCE_LEN];
    let ea = crate::dirname::encrypt_dirname(PROBE, a, &nonce, cipher);
    let eb = crate::dirname::encrypt_dirname(PROBE, b, &nonce, cipher);
    match (ea, eb) {
        (Ok(x), Ok(y)) => x == y,
        // 算不出来就当作「变了」：宁可多改一次名，也不要该改却没改——
        // 后者会留下两个密码都用不了的树
        _ => false,
    }
}

/// 改名后在父目录里找回这棵树的新路径。
///
/// 不靠 `rename_dir` 直接返回新路径：它可能因为「名字本来就解不开」而没改，
/// 那时原路径仍然有效。统一在这里按「能用新密钥解开的那个目录」来找。
fn find_renamed(orig: &Path, new_dkey: &DirnameKey, cipher: CipherId) -> Option<PathBuf> {
    if orig.is_dir() {
        return Some(orig.to_path_buf());
    }
    let parent = orig.parent()?;
    std::fs::read_dir(parent).ok()?.flatten().map(|e| e.path()).find(|p| {
        if !p.is_dir() {
            return false;
        }
        let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let sc = std::fs::read(p.join(crate::dirname::DIRNAME_SIDECAR)).ok();
        crate::dirname::decrypt_dirname(&n, sc.as_deref(), new_dkey, cipher).is_ok()
    })
}

/// 递归改写一层：先文件，再子目录名。
///
/// # 目录名必须一起改
///
/// 目录名由 `DirnameKey::derive(keep[0], vault_salt)` 加密，KEK 一换密钥就
/// 变了。只改文件的话会留下一个**两个密码都用不了**的状态：新密码开得了
/// 文件却解不开目录名（进不去），旧密码解得开目录名却开不了文件。这比
/// 「不能改密码」糟得多，所以两者必须在同一个操作里完成。
///
/// # 为什么自底向上
///
/// 先递归进子目录处理完，回来再重命名它。反过来做的话，重命名之后手里的
/// 路径就失效了，后续遍历会找不到文件。
/// 遍历时共享的那一组参数。
///
/// 打成一个结构体而不是继续加形参：递归函数已经有 8 个参数了，再加轮换
/// 开关、文件总数和进度回调会到 11 个，调用点根本读不出谁是谁。
struct RekeyCtx<'a> {
    unlock: &'a [Kek],
    keep: &'a [Kek],
    old_dkey: &'a DirnameKey,
    new_dkey: &'a DirnameKey,
    cipher: CipherId,
    dirname_changed: bool,
    rotate: bool,
    /// 待处理的密文文件总数，用于「第 i / n 个」。
    total: usize,
    /// 已处理到第几个。
    done: usize,
}

fn rekey_into(
    dir: &Path,
    ctx: &mut RekeyCtx<'_>,
    progress: &mut Option<RekeyProgressFn<'_>>,
    rep: &mut RekeyReport,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        rep.failed.push((dir.to_path_buf(), String::from("read_dir failed")));
        return;
    };
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            subdirs.push(p);
            continue;
        }
        // 只处理真正的密文文件。边车 `.omy-name` 的扩展名不是 `omy`，
        // 与 find_any_file 用同一条判据
        if !p.extension().is_some_and(|x| x == "omy") {
            continue;
        }
        ctx.done = ctx.done.saturating_add(1);
        let idx = ctx.done;
        let total = ctx.total;
        // 磁盘名而不是解出来的原名：这一步还没解密，为了显示一个名字去解
        // 整个文件头没有道理，何况用户在列表里看到的本来也是这个
        let shown = p.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut on_bytes = |done: u64, bytes_total: u64| {
            if let Some(cb) = progress.as_deref_mut() {
                cb(&shown, idx, total, done, bytes_total);
            }
        };
        match rekey_one(&p, ctx.unlock, ctx.keep, ctx.rotate, &mut on_bytes) {
            Ok(written) => {
                rep.changed = rep.changed.saturating_add(1);
                rep.bytes_rewritten = rep.bytes_rewritten.saturating_add(written);
            }
            Err(err) => rep.failed.push((p, err)),
        }
    }
    // 自底向上：先把子目录内部处理完，再改它自己的名字
    for sub in subdirs {
        rekey_into(&sub, ctx, progress, rep);
        if !ctx.dirname_changed {
            continue;
        }
        match rename_dir(&sub, ctx.old_dkey, ctx.new_dkey, ctx.cipher) {
            Ok(true) => rep.dirs_renamed = rep.dirs_renamed.saturating_add(1),
            // false = 名字本来就解不开（不是我们加密的目录），跳过不算失败
            Ok(false) => {}
            Err(err) => rep.failed.push((sub, err)),
        }
    }
}

/// 数一棵树里有多少个密文文件。
///
/// 轮换前先数一遍才能显示「第 i / n 个」。多走一次目录遍历的代价远小于
/// 轮换本身（那要把每个文件的载荷读写一遍），而没有总数的进度条只能转圈，
/// 用户无从判断还要等多久。
fn count_files(dir: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut n: usize = 0;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            n = n.saturating_add(count_files(&p));
        } else if p.extension().is_some_and(|x| x == "omy") {
            n = n.saturating_add(1);
        }
    }
    n
}

/// 用新密钥重新加密一个目录的名字。
///
/// 返回 `false` 表示这个目录名用旧密钥解不开——它多半不是本 vault 的产物，
/// 跳过而不是报错。返回 `true` 表示确实改名了。
fn rename_dir(
    dir: &Path,
    old_dkey: &DirnameKey,
    new_dkey: &DirnameKey,
    cipher: CipherId,
) -> core::result::Result<bool, String> {
    let disk = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if !crate::dirname::looks_encrypted(&disk) {
        return Ok(false);
    }
    let sidecar_path = dir.join(crate::dirname::DIRNAME_SIDECAR);
    let sidecar = std::fs::read(&sidecar_path).ok();
    let Ok(plain) = crate::dirname::decrypt_dirname(&disk, sidecar.as_deref(), old_dkey, cipher)
    else {
        return Ok(false);
    };

    // nonce 每次重新随机：同一个名字用同一个密钥加密两次应当得到不同密文，
    // 否则「改密码前后磁盘名没变」会泄露「这两个目录同名」
    let enc = crate::dirname::encrypt_dirname(&plain, new_dkey, &random_nonce(), cipher)
        .map_err(|e| e.to_string())?;

    let parent = dir.parent().ok_or_else(|| String::from("no parent"))?;
    let dst = parent.join(&enc.disk_name);

    // 边车要在改名**之前**写好：改完名再写的话，中途失败会留下一个
    // 名字截断、却没有边车的目录——那个名字就永远解不开了
    if let Some(blob) = enc.sidecar.as_ref() {
        std::fs::write(dir.join(crate::dirname::DIRNAME_SIDECAR), blob)
            .map_err(|e| format!("write sidecar: {e}"))?;
    } else if sidecar.is_some() {
        // 新名字不需要边车，旧的必须删掉：留着的话解名时会优先读它，
        // 拿到的是用旧密钥加密的内容
        std::fs::remove_file(&sidecar_path).map_err(|e| format!("remove sidecar: {e}"))?;
    }

    if dst == dir {
        return Ok(true);
    }
    std::fs::rename(dir, &dst).map_err(|e| format!("rename: {e}"))?;
    Ok(true)
}

/// 改写单个文件。错误转成字符串，好让调用方逐个展示。
fn rekey_one(
    path: &Path,
    unlock: &[Kek],
    keep: &[Kek],
    rotate: bool,
    on_bytes: &mut dyn FnMut(u64, u64),
) -> core::result::Result<u64, String> {
    let data = std::fs::read(path).map_err(|e| format!("read: {e}"))?;

    // 两条路径只差这一步：改密码只重建 384 字节的 slot 区，轮换要把载荷
    // 读一遍再写一遍。前后的检查（自证可打开、原子写回）完全共用——
    // 分成两个函数的话，将来加一项检查就必然漏掉一边
    let (bytes, written) = if rotate {
        // 读与写各占一半：把两个阶段拼成一条 0→100，用户看到的是一条
        // 匀速推进的进度，而不是「跑到 100% 又归零重来一遍」
        //
        // 走 RefCell 是因为 rotate_fek_with_progress 要两个独立的
        // `&mut dyn FnMut`，而它们都得往同一个回调里写——借用检查器不接受
        // 两个闭包同时可变借用。运行期两者其实不重叠（解密跑完才轮到加密）
        let sink = core::cell::RefCell::new(on_bytes);
        let mut dec = |done: u64, total: u64| {
            if let Ok(mut f) = sink.try_borrow_mut() {
                f(done, total.saturating_mul(2));
            }
        };
        let mut enc = |done: u64, total: u64| {
            if let Ok(mut f) = sink.try_borrow_mut() {
                f(total.saturating_add(done), total.saturating_mul(2));
            }
        };
        let out = crate::reencrypt::rotate_fek_with_progress(
            &data,
            unlock,
            keep,
            &crate::file::RandomMaterial::generate(),
            Some(&mut dec),
            Some(&mut enc),
        )
        .map_err(|e| e.to_string())?;
        (out.bytes, out.plaintext_size)
    } else {
        let out = crate::keyslot::rewrite_slots(&data, unlock, keep).map_err(|e| e.to_string())?;
        (out.bytes, 0)
    };

    // 写回前自证每个保留密码都能打开新字节。逐个验而不是只验第一个：
    // add 最容易犯的错是新密码能开、原密码被挤掉，只验一个正好漏掉。
    //
    // 顺序不能反——先覆盖再发现打不开，用户就同时失去了文件和访问权
    for k in keep {
        crate::file::open(&bytes, &[k.duplicate()])
            .map_err(|_| String::from("rewrite verify failed"))?;
    }

    crate::fsatomic::write_atomic(path, &bytes).map_err(|e| format!("write: {e}"))?;
    Ok(written)
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
    use crate::header::FixedHeader;

    /// 造一棵小树并加密，返回 (工作目录, 密文根, salt, cipher)。
    ///
    /// cipher 一并返回而不是让调用方硬写常量：写错的话目录名会用错算法
    /// 解密、报 content hash mismatch，看起来像「加密坏了」这种严重缺陷。
    fn tree_fixture(tag: &str) -> (PathBuf, PathBuf, [u8; 16], CipherId) {
        let root = std::env::temp_dir().join(format!("omy-rekey-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"AAA").unwrap();
        std::fs::write(src.join("sub/b.txt"), b"BBB").unwrap();

        let salt = [7u8; 16];
        let kek = Kek::from_password(b"old", &salt, Argon2Params::TEST_WEAK).unwrap();
        let out = root.join("enc");
        std::fs::create_dir_all(&out).unwrap();
        let opts = crate::file::EncryptOptions::default();
        let rep = encrypt_tree(&src, &out, &[kek], &salt, &opts, None).unwrap();
        (root, rep.root, salt, opts.cipher)
    }

    fn kek_of(pw: &[u8], salt: &[u8; 16]) -> Kek {
        Kek::from_password(pw, salt, Argon2Params::TEST_WEAK).unwrap()
    }

    #[test]
    fn rekey_tree_changes_every_file_and_old_password_stops_working() {
        let (root, enc_root, salt, cipher) = tree_fixture("change");
        let old = kek_of(b"old", &salt);
        let new = kek_of(b"new", &salt);

        let rep =
            rekey_tree(&enc_root, &[old.duplicate()], &[new.duplicate()], &salt, cipher).unwrap();
        assert_eq!(rep.changed, 2, "两个文件都该改到；漏掉深层文件是最可能的缺陷");
        assert!(rep.is_complete(), "失败项: {:?}", rep.failed);
        // 根目录名由 KEK 派生，换密码后必须跟着变，否则新密码进不去
        assert_ne!(rep.root, enc_root, "根目录名没变——新密码将解不开它");
        assert!(rep.root.is_dir(), "返回的新根路径不存在");
        assert!(!enc_root.exists(), "旧根路径还在，说明是复制而不是改名");
        let enc_root = rep.root.clone();

        // 正面：新密码能打开每一个文件
        let mut checked = 0;
        for f in all_files(&enc_root) {
            let data = std::fs::read(&f).unwrap();
            let opened = crate::file::open(&data, &[new.duplicate()]).unwrap();
            // 取出原文件名：能解出来就说明 FEK 是对的（文件名用 FEK 加密）
            assert!(opened.filename().unwrap().ends_with(".txt"));
            checked += 1;
            // 反证：旧密码必须打不开了，否则「改密码」根本没生效
            assert!(
                crate::file::open(&data, &[old.duplicate()]).is_err(),
                "旧密码仍能打开 {f:?}——改密码没生效，而用户以为已经换了"
            );
        }
        assert_eq!(checked, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_keeps_payload_byte_identical() {
        // 改密码只该动 384 字节 slot 区 + 32 字节 MAC。若实现退化成
        // 「解密后重新加密」，载荷和 file_uuid 都会变——对增量备份来说
        // 整棵树都要重传，而且大文件会慢几个数量级
        let (root, enc_root, salt, cipher) = tree_fixture("payload");
        // 按**磁盘文件名**索引而不是完整路径：目录名会随改密码变化，
        // 完整路径在 rekey 之后就失效了。文件名是随机 uuid，不会变
        let before: std::collections::BTreeMap<String, Vec<u8>> = all_files(&enc_root)
            .into_iter()
            .map(|p| {
                let d = std::fs::read(&p).unwrap();
                (p.file_name().unwrap().to_string_lossy().into_owned(), d)
            })
            .collect();

        let old = kek_of(b"old", &salt);
        let new = kek_of(b"new", &salt);
        let rep = rekey_tree(&enc_root, &[old], &[new], &salt, cipher).unwrap();

        for p in all_files(&rep.root) {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let old_bytes = before.get(&name).expect("改密码后出现了新文件名——uuid 不该变");
            let new_bytes = std::fs::read(&p).unwrap();
            assert_eq!(old_bytes.len(), new_bytes.len(), "长度变了：{p:?}");
            let old_bytes = old_bytes.clone();
            // 头部固定区 + slot 区 + MAC 之后的部分必须逐字节相同。
            // 用 header_len 定位载荷起点，不靠硬编码偏移
            let h = FixedHeader::parse(&new_bytes).unwrap();
            let start = h.header_len as usize;
            assert_eq!(
                &old_bytes[start..],
                &new_bytes[start..],
                "载荷被改写了：{p:?}——说明退化成了解密后重新加密"
            );
            // file_uuid 也必须不变
            let h0 = FixedHeader::parse(&old_bytes).unwrap();
            assert_eq!(h0.file_uuid, h.file_uuid, "file_uuid 变了：{p:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_refuses_empty_keep() {
        // keep 为空会留下永远打不开的文件，等同于销毁数据
        let (root, enc_root, salt, cipher) = tree_fixture("empty");
        let old = kek_of(b"old", &salt);
        assert!(rekey_tree(&enc_root, &[old], &[], &salt, cipher).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_rejects_a_plain_directory() {
        // 对普通目录执行必须报错，不能返回「0 个已改写」——那看着像成功，
        // 而用户其实选错了目录
        let root = std::env::temp_dir().join("omy-rekey-plain");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("x.txt"), b"not encrypted").unwrap();
        let salt = [7u8; 16];
        let k = kek_of(b"pw", &salt);
        assert!(
            rekey_tree(&root, &[k.duplicate()], &[k], &salt, CipherId::ChaCha20Poly1305).is_err(),
            "对普通目录应当报错，而不是静默返回 0 个已改写"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_reports_wrong_password_per_file_without_touching_them() {
        // 密码不对时：一个都不该改，且要逐个报出来
        let (root, enc_root, salt, cipher) = tree_fixture("wrongpw");
        let before: Vec<Vec<u8>> =
            all_files(&enc_root).iter().map(|p| std::fs::read(p).unwrap()).collect();

        let bad = kek_of(b"bad", &salt);
        let new = kek_of(b"new", &salt);
        let rep = rekey_tree(&enc_root, &[bad], &[new], &salt, cipher).unwrap();
        assert_eq!(rep.changed, 0);
        assert_eq!(rep.failed.len(), 2, "两个文件都该报失败");
        assert!(!rep.is_complete());

        // 磁盘上必须一字节没动。写坏了才报错的实现会让用户既没改成密码、
        // 又丢了文件
        let after: Vec<Vec<u8>> =
            all_files(&enc_root).iter().map(|p| std::fs::read(p).unwrap()).collect();
        assert_eq!(before, after, "密码不对却动了文件");
        // 目录名也不该动：密码都不对，改名只会让树更难恢复
        assert!(enc_root.is_dir(), "密码不对却把根目录改名了");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_can_add_a_second_password_keeping_the_first() {
        // add 的语义：两个密码都能开。最容易犯的错是新密码把原密码挤掉
        let (root, enc_root, salt, cipher) = tree_fixture("add");
        let old = kek_of(b"old", &salt);
        let extra = kek_of(b"extra", &salt);
        let rep = rekey_tree(
            &enc_root,
            &[old.duplicate()],
            &[old.duplicate(), extra.duplicate()],
            &salt,
            cipher,
        )
        .unwrap();
        assert!(rep.is_complete(), "{:?}", rep.failed);
        // add 保留了原密码作为第一个 KEK，所以目录名密钥不变、根路径不变。
        // 这是 add 与 change 的一个可观察差别
        assert_eq!(rep.root, enc_root, "add 保留原密码时根目录名不该变");

        for f in all_files(&enc_root) {
            let data = std::fs::read(&f).unwrap();
            assert!(crate::file::open(&data, &[old.duplicate()]).is_ok(), "原密码被挤掉了");
            assert!(crate::file::open(&data, &[extra.duplicate()]).is_ok(), "新密码不生效");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_rotate_replaces_the_ciphertext_unlike_a_plain_rekey() {
        // 轮换与改密码的**唯一**可观察差别就是这个：载荷密文变没变。
        // 少了这条，一个「悄悄忽略 rotate 参数」的实现能通过其余所有断言
        let (root, enc_root, salt, cipher) = tree_fixture("rot");
        let old = kek_of(b"old", &salt);
        let new = kek_of(b"new", &salt);

        let before: std::collections::BTreeMap<String, Vec<u8>> = all_files(&enc_root)
            .into_iter()
            .map(|p| {
                let d = std::fs::read(&p).unwrap();
                (p.file_name().unwrap().to_string_lossy().into_owned(), d)
            })
            .collect();

        let rep = rekey_tree_with_progress(
            &enc_root,
            &[old.duplicate()],
            &[new.duplicate()],
            &salt,
            cipher,
            true,
            None,
        )
        .unwrap();
        assert!(rep.is_complete(), "{:?}", rep.failed);
        assert!(rep.payload_rewritten, "报告里要标明换了文件密钥");
        assert!(rep.bytes_rewritten > 0, "重写的明文字节数不该是 0");

        // 轮换会换掉 file_uuid，所以磁盘名（uuid）也会变——按名字配不上，
        // 正好说明确实换了。逐字节比对全体内容：只要有一个文件的字节
        // 原封不动，就是没真的轮换
        let after: Vec<Vec<u8>> =
            all_files(&rep.root).iter().map(|p| std::fs::read(p).unwrap()).collect();
        assert_eq!(after.len(), before.len(), "文件数不该变");
        for bytes in &after {
            assert!(
                !before.values().any(|b| b == bytes),
                "有文件的密文一字节没变——rotate 没生效"
            );
        }

        // 换完还得能用：整棵树用新密码解得开，内容一致
        let out = root.join("dec");
        std::fs::create_dir_all(&out).unwrap();
        let d = decrypt_tree(&rep.root, &out, &[new], &salt, cipher, None).unwrap();
        assert_eq!(d.files, 2);
        assert_eq!(std::fs::read(d.root.join("a.txt")).unwrap(), b"AAA");
        assert_eq!(std::fs::read(d.root.join("sub/b.txt")).unwrap(), b"BBB");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_without_rotate_keeps_the_ciphertext() {
        // 反证。上一条若因为「所有路径都重写密文」而通过，这条会失败——
        // 改密码必须只动 slot 区，载荷一字节不改（否则增量备份要重传整份）
        //
        // 单独造 fixture 而不用 tree_fixture：那个写的是 3 字节明文，载荷
        // 密文才 23 字节，比对尾部时很容易倒着切进头部区——而头部本来就
        // 该变（slot 区重建了），于是断言必然失败，看着像产品缺陷
        const N: usize = 2000;
        let root = std::env::temp_dir().join("omy-rekey-norot");
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let body: Vec<u8> = (0..N).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("big.bin"), &body).unwrap();
        let salt = [7u8; 16];
        let out = root.join("enc");
        std::fs::create_dir_all(&out).unwrap();
        let opts = crate::file::EncryptOptions::default();
        let cipher = opts.cipher;
        let old = kek_of(b"old", &salt);
        let new = kek_of(b"new", &salt);
        let enc_root =
            encrypt_tree(&src, &out, &[old.duplicate()], &salt, &opts, None).unwrap().root;

        let before: std::collections::BTreeMap<String, Vec<u8>> = all_files(&enc_root)
            .into_iter()
            .map(|p| {
                let d = std::fs::read(&p).unwrap();
                (p.file_name().unwrap().to_string_lossy().into_owned(), d)
            })
            .collect();
        assert_eq!(before.len(), 1);

        let rep =
            rekey_tree_with_progress(&enc_root, &[old], &[new], &salt, cipher, false, None)
                .unwrap();
        assert!(!rep.payload_rewritten);
        assert_eq!(rep.bytes_rewritten, 0, "没轮换就不该报重写字节数");

        for p in all_files(&rep.root) {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            // 磁盘名就是 file_uuid：改密码不换它，所以按名字一定配得上。
            // 配不上说明 file_uuid 变了，那正是我们要防的
            let old_bytes = before.get(&name).expect("改密码不该换掉磁盘名（file_uuid）");
            let new_bytes = std::fs::read(&p).unwrap();
            assert_eq!(old_bytes.len(), new_bytes.len(), "长度变了：{p:?}");
            // 载荷在文件末尾，长度是明文 + AEAD tag。往回取这么多一定落在
            // 载荷内，不会切进会变的头部区
            let tail = old_bytes.len().saturating_sub(N);
            assert_eq!(&old_bytes[tail..], &new_bytes[tail..], "载荷变了：{p:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_rotate_reports_progress_for_every_file() {
        // 进度回调是 GUI 进度条的唯一数据来源。不测的话，一个「压根不调
        // 回调」的实现完全无声——界面上表现为进度条一直停在 0
        let (root, enc_root, salt, cipher) = tree_fixture("prog");
        let old = kek_of(b"old", &salt);

        let mut seen: Vec<(String, usize, usize)> = Vec::new();
        let mut max_ratio = 0.0f64;
        let mut cb = |name: &str, idx: usize, total: usize, done: u64, bytes: u64| {
            let key = (name.to_owned(), idx, total);
            if !seen.contains(&key) {
                seen.push(key);
            }
            if bytes > 0 {
                #[expect(clippy::cast_precision_loss, reason = "测试里比个比例够用")]
                let r = done as f64 / bytes as f64;
                if r > max_ratio {
                    max_ratio = r;
                }
            }
        };
        let rep = rekey_tree_with_progress(
            &enc_root,
            &[old.duplicate()],
            &[old],
            &salt,
            cipher,
            true,
            Some(&mut cb),
        )
        .unwrap();
        assert!(rep.is_complete(), "{:?}", rep.failed);

        assert_eq!(seen.len(), 2, "两个文件都该汇报进度，实际 {seen:?}");
        for (_, idx, total) in &seen {
            assert_eq!(*total, 2, "总数应当是文件数");
            assert!(*idx >= 1 && *idx <= 2, "序号越界: {idx}");
        }
        // 序号必须各不相同：都报 1 的话进度条永远停在第一个
        assert_ne!(seen[0].1, seen[1].1, "两个文件报了同一个序号");
        // 进度要真的推进到接近完成，而不是只发一次 0
        assert!(max_ratio > 0.5, "进度最高只到 {max_ratio}，没有推进到末尾");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_rotate_can_keep_the_same_password() {
        // 「密码不变、只让旧副本作废」是轮换的正当用法。要求必须换密码
        // 会让这个需求无法表达
        let (root, enc_root, salt, cipher) = tree_fixture("samepw");
        let old = kek_of(b"old", &salt);
        let before: Vec<Vec<u8>> =
            all_files(&enc_root).iter().map(|p| std::fs::read(p).unwrap()).collect();

        let rep = rekey_tree_with_progress(
            &enc_root,
            &[old.duplicate()],
            &[old.duplicate()],
            &salt,
            cipher,
            true,
            None,
        )
        .unwrap();
        assert!(rep.is_complete(), "{:?}", rep.failed);
        // 密码没变 → 目录名密钥没变 → 根路径不该变
        assert_eq!(rep.root, enc_root, "密码没变时不该改目录名");

        // 但密文必须变了：这正是「让旧副本作废」的实质
        let after: Vec<Vec<u8>> =
            all_files(&enc_root).iter().map(|p| std::fs::read(p).unwrap()).collect();
        for b in &after {
            assert!(!before.contains(b), "密文没变，旧副本仍然等价");
        }
        // 原密码还能用
        let out = root.join("dec");
        std::fs::create_dir_all(&out).unwrap();
        let d = decrypt_tree(&enc_root, &out, &[old], &salt, cipher, None).unwrap();
        assert_eq!(d.files, 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rekey_tree_leaves_directory_names_openable() {
        // 目录名由 keep 的**第一个** KEK 派生。如果改密码后第一个 KEK 变了，
        // 目录名就解不开、整个分支进不去。这条守的是「换密码后树还能浏览」
        let (root, enc_root, salt, cipher) = tree_fixture("dirname");
        let old = kek_of(b"old", &salt);
        let new = kek_of(b"new", &salt);
        let rk =
            rekey_tree(&enc_root, &[old.duplicate()], &[new.duplicate()], &salt, cipher).unwrap();
        assert!(rk.is_complete(), "{:?}", rk.failed);
        assert_eq!(rk.dirs_renamed, 2, "根 + 子目录都该改名");

        // 用新密码解整棵树。这一条是整个改动的核心：只改文件不改目录名的话，
        // 新密码开得了文件却解不开目录名，旧密码反之——两个密码都用不了
        let out = root.join("dec");
        std::fs::create_dir_all(&out).unwrap();
        let rep = decrypt_tree(&rk.root, &out, &[new], &salt, cipher, None).unwrap();
        assert_eq!(rep.files, 2, "换密码后整棵树应当仍能解开，失败: {:?}", rep.skipped);
        assert!(rep.root.join("sub/b.txt").is_file(), "子目录结构没还原对");

        // 反证：旧密码不该还能解开这棵树
        let out2 = root.join("dec-old");
        std::fs::create_dir_all(&out2).unwrap();
        assert!(
            decrypt_tree(&rk.root, &out2, &[old], &salt, cipher, None).is_err(),
            "旧密码仍能解开整棵树——改密码没生效"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 收集树里所有密文文件，按路径排序好让比较稳定。
    fn all_files(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        walk(root, &mut out);
        out.sort();
        out
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "omy") {
                out.push(p);
            }
        }
    }

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
