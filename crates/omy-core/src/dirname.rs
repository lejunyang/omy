//! 树形模式的目录名加密（文档 05 §3.2、§3.3）。
//!
//! # 为什么目录名需要单独一套机制
//!
//! 文件名有地方藏——写进该文件自己的 `TLV_FILENAME`，磁盘上的名字随便叫
//! 什么都行。**目录名没有这个奢侈**：它就是磁盘上的一个真实路径组件，
//! 没有任何文件头可以承载它，只能把密文编码进名字本身。
//!
//! 这带来了三个约束，下面每一条都是踩过的坑或明确的设计决策。
//!
//! # 一、必须用 base32，不能用 base64
//!
//! 这不是风格偏好，两条都是硬伤：
//!
//! | 问题 | base64 | base32 |
//! |---|---|---|
//! | 含 `/` | 是，是路径分隔符 | 否 |
//! | 大小写敏感 | 是 | 否（全大写） |
//!
//! 大小写那条尤其致命：**Windows 与 APFS 默认大小写不敏感**，base64 下
//! 两个只差大小写的密文目录名（`aB...` 与 `Ab...`）在磁盘上会直接撞车，
//! 而且是随机撞——同一个明文目录名每次加密的 nonce 不同，密文不同，
//! 只在偶然差一个字母大小写时才炸。这种缺陷在测试里几乎不可能复现。
//!
//! 代价是长度膨胀 1.6×（base64 是 1.33×），见下条。
//!
//! # 二、长度限制会真的撞上
//!
//! 多数文件系统单个路径组件上限 255 字节。我们的密文结构是
//! `nonce(12) || ciphertext || tag(16)`，再乘 base32 的 1.6×，
//! 于是原始目录名的可用长度只剩约 130 字节——中文按 UTF-8 算一字三节，
//! 也就是 **40 多个汉字**。这不是极端情况，「2024年第三季度市场调研原始
//! 数据备份」这种目录名很常见。
//!
//! 超长的处理：截断 base32 输出 + 追加哈希后缀保证唯一性。此时**完整
//! 目录名无法从名字本身恢复**，所以额外在该目录下放一个
//! [`DIRNAME_SIDECAR`] 文件存完整密文。这是不得已的妥协，但比「加密到
//! 一半突然报名字太长」要好。
//!
//! # 三、密钥是 vault 级的，不是文件级的
//!
//! 目录名不属于任何单个文件，无法用某个文件的 FEK。所以用 KEK 直接派生：
//!
//! ```text
//! dirname_key = HKDF(KEK, salt=vault_salt, info="omy/v1/dirname")
//! ```
//!
//! **这有一个必须写进文档、也必须在 UI 里告知的副作用**：目录结构的
//! 可见性绑定整个 vault，而不是单个文件的 slot。多密码场景下，能解开
//! vault 的**任一**密码都能看到完整目录树。如果有人期望「给同事一个
//! 只能看部分内容的密码」，树形模式做不到——这是设计约束不是缺陷，
//! 但不说清楚就会变成安全事故。
//!
//! # 为什么手写 base32 而不加依赖
//!
//! 与 [`crate::pack`] 里不引入 `walkdir` 同一个理由：core 的依赖目前只有
//! 密码学与格式相关的几个 crate，保持这份克制让 core 能被任意项目复用，
//! 审计面也更小。base32 编解码是三十行的位操作，不值得为它引入一个新的
//! 供应链节点。

use crate::crypto::{CipherId, Kek, NONCE_LEN, SecretKey};
use crate::error::{Error, Result};

/// HKDF info：目录名密钥。
///
/// 与 `crypto.rs` 里那几个 `INFO_*` 同族，但放在这里而不是那边——
/// 目录名是树形模式独有的概念，crypto 层不需要知道它存在。
const INFO_DIRNAME: &[u8] = b"omy/v1/dirname";

/// 密文目录名的后缀。
///
/// 加了后缀才能在扫描时一眼区分「这是加密目录」和「用户自己建的目录」。
/// 与文件用同一个 `.omy`：对用户而言它们是同一种东西的两种形态，
/// 用两个后缀只会让人猜哪个是哪个。
pub const DIR_SUFFIX: &str = ".omy";

/// 存放超长目录名完整密文的边车文件名。
///
/// 放在被截断的那个目录**内部**，而不是父目录里：这样目录被整体移动或
/// 复制时它跟着走。放父目录的话，移动一个目录会让它的名字变成不可恢复。
pub const DIRNAME_SIDECAR: &str = ".omy-name";

/// 单个路径组件的字节上限。
///
/// 255 是 NTFS / ext4 / APFS 的共同下限。不取更大值——跨平台是硬需求，
/// 按最宽松的文件系统设计会导致在别的平台上打不开。
const MAX_COMPONENT: usize = 255;

/// 截断后保留的 base32 字符数。
///
/// 留出 `~` 分隔符、8 字符哈希与 `.omy` 后缀的空间：
/// `200 + 1 + 8 + 4 = 213`，离 255 还有余量。余量是留给
/// **调用方可能再加前缀**的——例如分片模式的 `.001` 之类。
const TRUNCATE_AT: usize = 200;

/// 截断名的哈希后缀长度（base32 字符数）。
///
/// 5 字节哈希 → base32 正好 8 字符，无填充。取 5 字节而不是 4：
/// 生日界下 4 字节在同一目录几万个子目录时碰撞概率已不可忽略，
/// 5 字节把它推到 2^20 量级，足够了。
const HASH_SUFFIX_LEN: usize = 8;

/// RFC 4648 base32 字母表（全大写，无填充）。
const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// 目录名加密密钥。
///
/// 独立类型而不是裸 `SecretKey`：这样类型系统会阻止把它误用成文件密钥
/// 或反之。两者都是 32 字节，混用不会编译报错但会产出解不开的数据。
#[derive(Debug)]
pub struct DirnameKey(SecretKey);

impl DirnameKey {
    /// 从 KEK 与 vault salt 派生。
    ///
    /// 同一个 vault 内必须得到同一个密钥，否则同一棵树里的目录名会用
    /// 不同密钥加密，解开时部分目录变成乱码。这由 `vault_salt` 保证——
    /// 它在 vault 内是固定的。
    #[must_use]
    pub fn derive(kek: &Kek, vault_salt: &[u8; 16]) -> Self {
        // 复用 crypto 层的 HKDF：那里已经处理了输出长度不可能失败的情形
        Self(kek.derive_dirname_key(vault_salt, INFO_DIRNAME))
    }

    /// 借出底层密钥。仅供本模块内部与测试使用。
    #[must_use]
    const fn as_key(&self) -> &SecretKey {
        &self.0
    }
}

/// 加密一个目录名的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedDirname {
    /// 落在磁盘上的名字（已含 `.omy` 后缀）。
    pub disk_name: String,
    /// 名字被截断时，需要写进 [`DIRNAME_SIDECAR`] 的完整密文。
    ///
    /// `None` 表示 `disk_name` 自身已包含完整信息，不需要边车文件。
    /// 调用方**必须**处理 `Some` 的情况，否则该目录名将无法还原。
    pub sidecar: Option<Vec<u8>>,
}

/// 加密目录名。
///
/// `nonce` 由调用方提供而不是内部随机生成，理由与
/// [`crate::file::RandomMaterial`] 相同：可确定性复现是测试向量的前提。
///
/// # Errors
///
/// - [`Error::MalformedTlv`]：目录名为空、含路径分隔符、或 AEAD 失败
pub fn encrypt_dirname(
    name: &str,
    key: &DirnameKey,
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<EncryptedDirname> {
    encrypt_with_key(name, key.as_key(), nonce, cipher)
}

/// 目录名加密的实际实现，按裸密钥工作。
///
/// 抽出来是因为现在有两种密钥来源（单钥匙派生的 [`DirnameKey`]、随机的
/// [`crate::dirsidecar::DirKey`]），而截断、边车、base32 这些逻辑一个字
/// 都不该有两份——同一套规则实现两遍，迟早在某次改动里只改了一边。
fn encrypt_with_key(
    name: &str,
    key: &SecretKey,
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<EncryptedDirname> {
    if name.is_empty() {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "directory name must not be empty",
        });
    }
    // 含分隔符的「目录名」说明调用方传了路径而不是单个组件。
    // 静默接受会产出一个跨目录的密文名，解开时凭空多出一层
    if name.contains('/') || name.contains('\\') {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "directory name must be a single component",
        });
    }

    // nonce 必须前置存储：解密时要先取出它才能解密，而它本身不是秘密
    let ct = cipher.encrypt(key, nonce, name.as_bytes(), &[])?;
    let mut blob = Vec::with_capacity(NONCE_LEN.saturating_add(ct.len()));
    blob.extend_from_slice(nonce);
    blob.extend_from_slice(&ct);

    let encoded = base32_encode(&blob);

    // 常见情况：编码后没超限，名字自身就是完整信息
    if encoded.len().saturating_add(DIR_SUFFIX.len()) <= MAX_COMPONENT {
        return Ok(EncryptedDirname {
            disk_name: format!("{encoded}{DIR_SUFFIX}"),
            sidecar: None,
        });
    }

    // 超长：截断 + 哈希后缀。哈希算的是**完整密文**而不是明文名——
    // 明文哈希会让相同目录名产出相同后缀，等于泄露「这两个目录同名」
    let digest = crate::util::blake2b_256(&blob);
    let tail = base32_encode(digest.get(..5).unwrap_or(&digest));
    let head = encoded.get(..TRUNCATE_AT).unwrap_or(&encoded);
    Ok(EncryptedDirname {
        disk_name: format!(
            "{head}~{}{DIR_SUFFIX}",
            tail.get(..HASH_SUFFIX_LEN).unwrap_or(&tail)
        ),
        sidecar: Some(blob),
    })
}

/// 解密目录名。
///
/// `sidecar` 是从 [`DIRNAME_SIDECAR`] 读到的内容（若该文件存在）。
/// 名字被截断过时必须提供，否则无法还原。
///
/// # Errors
///
/// - [`Error::MalformedTlv`]：不是密文目录名、base32 非法、密文过短
/// - [`Error::MalformedTlv`]：名字被截断但未提供 sidecar
/// - [`Error::ContentHashMismatch`]：AEAD 认证失败（密钥不对或被篡改）
pub fn decrypt_dirname(
    disk_name: &str,
    sidecar: Option<&[u8]>,
    key: &DirnameKey,
    cipher: CipherId,
) -> Result<String> {
    decrypt_with_key(disk_name, sidecar, key.as_key(), cipher)
}

/// 目录名解密的实际实现，按裸密钥工作。理由同 [`encrypt_with_key`]。
fn decrypt_with_key(
    disk_name: &str,
    sidecar: Option<&[u8]>,
    key: &SecretKey,
    cipher: CipherId,
) -> Result<String> {
    let stem = disk_name.strip_suffix(DIR_SUFFIX).ok_or(Error::MalformedTlv {
        tlv_type: 0,
        reason: "not an encrypted directory name",
    })?;

    // 截断过的名字里有 `~`，此时名字本身不足以还原，必须靠 sidecar。
    // 明确报错而不是尝试解半截密文：后者会得到 AEAD 认证失败，
    // 那个错误信息会把人引向「密码不对」的错误方向
    let blob = if stem.contains('~') {
        sidecar
            .ok_or(Error::MalformedTlv {
                tlv_type: 0,
                reason: "truncated directory name requires sidecar file",
            })?
            .to_vec()
    } else {
        base32_decode(stem)?
    };

    let (nonce, ct) = blob.split_at_checked(NONCE_LEN).ok_or(Error::MalformedTlv {
        tlv_type: 0,
        reason: "encrypted directory name too short to contain a nonce",
    })?;
    let mut n = [0u8; NONCE_LEN];
    n.copy_from_slice(nonce);

    let plain = cipher
        .decrypt(key, &n, ct, &[])?
        // 认证失败最可能的原因是密钥不对（换了密码 / 不是这个 vault 的目录）。
        // 用 ContentHashMismatch 而不是自造变体：它归到 Corrupted 退出码，
        // 与其它 AEAD 失败一致
        .ok_or(Error::ContentHashMismatch)?;

    String::from_utf8(plain).map_err(|_| Error::MalformedTlv {
        tlv_type: 0,
        reason: "decrypted directory name is not valid UTF-8",
    })
}

/// 用目录密钥加密目录名（两层结构）。
///
/// 与 [`encrypt_dirname`] 的区别只在密钥来源：这里收的是随机生成的
/// [`DirKey`]，它本身被每把 KEK 各包一份放进边车。
///
/// # 为什么要有这个而不是改 `encrypt_dirname` 的签名
///
/// [`DirnameKey`] 由单把 KEK 派生，结构上只能被那一把钥匙解开——这正是
/// 多密码和恢复码在树上用不了的根因。两者的**密钥语义不同**，共用一个
/// 函数名会让调用方分不清自己拿的是哪一种，而拿错的后果是目录名解不开。
///
/// # Errors
///
/// 同 [`encrypt_dirname`]。
pub fn encrypt_dirname_with(
    name: &str,
    dk: &crate::dirsidecar::DirKey,
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<EncryptedDirname> {
    encrypt_with_key(name, dk.as_key(), nonce, cipher)
}

/// 用目录密钥解密目录名（两层结构）。
///
/// `long_name` 是名字被截断时存下来的完整密文。注意它与「钥匙包裹边车」
/// 是**两回事**：前者存的是目录名密文，后者存的是 DK 的包裹。
///
/// # Errors
///
/// 同 [`decrypt_dirname`]。
pub fn decrypt_dirname_with(
    disk_name: &str,
    long_name: Option<&[u8]>,
    dk: &crate::dirsidecar::DirKey,
    cipher: CipherId,
) -> Result<String> {
    decrypt_with_key(disk_name, long_name, dk.as_key(), cipher)
}

/// 判断一个磁盘名是否可能是密文目录名。
///
/// 只做**廉价**判断（后缀 + 字符集），不尝试解密。扫描时对每个目录都要
/// 调一次，解密的开销不该出现在这条路径上。
#[must_use]
pub fn looks_encrypted(disk_name: &str) -> bool {
    let Some(stem) = disk_name.strip_suffix(DIR_SUFFIX) else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    // 全大写 base32 字符集，允许一个 `~`（截断标记）
    stem.bytes()
        .all(|b| b == b'~' || B32.contains(&b))
}

/// base32 编码（RFC 4648，无填充）。
///
/// 每 5 字节 → 8 字符。手写而不用 crate，理由见模块文档。
fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5).saturating_mul(8));
    // 用 u64 累加器而不是逐位移：5 字节一组正好 40 位，放得下且不会溢出
    for chunk in data.chunks(5) {
        let mut acc = 0u64;
        for (i, &b) in chunk.iter().enumerate() {
            // 高位在前：第 0 字节占最高 8 位
            let shift = 32usize.saturating_sub(i.saturating_mul(8));
            acc |= u64::from(b) << shift;
        }
        // 5 字节产 8 字符，不足 5 字节时按位数向上取整
        let chars = chunk.len().saturating_mul(8).div_ceil(5);
        for i in 0..chars {
            let shift = 35usize.saturating_sub(i.saturating_mul(5));
            let idx = ((acc >> shift) & 0x1F) as usize;
            out.push(char::from(*B32.get(idx).unwrap_or(&b'A')));
        }
    }
    out
}

/// base32 解码（RFC 4648，无填充）。
///
/// # Errors
///
/// [`Error::MalformedTlv`]：出现字母表之外的字符。
fn base32_decode(s: &str) -> Result<Vec<u8>> {
    // 容量预估，除法截断无妨（少算一字节只是多一次 realloc）。
    // 用 div_ceil 而不是 `/`：clippy 的 integer_division 在加密代码里是
    // 有价值的告警，不该为一处容量计算把它关掉
    let mut out = Vec::with_capacity(s.len().saturating_mul(5).div_ceil(8));
    let mut acc = 0u64;
    let mut bits = 0u32;
    for b in s.bytes() {
        let Some(v) = B32.iter().position(|&c| c == b) else {
            return Err(Error::MalformedTlv {
                tlv_type: 0,
                reason: "invalid base32 character in directory name",
            });
        };
        acc = (acc << 5) | v as u64;
        bits = bits.saturating_add(5);
        if bits >= 8 {
            bits = bits.saturating_sub(8);
            // 取出高 8 位
            let byte = ((acc >> bits) & 0xFF) as u8;
            out.push(byte);
        }
    }
    // 余下不足 8 位的是编码填充，必须为 0。非 0 说明数据被改过——
    // 不检查的话篡改者可以在这里塞信息而不影响解码结果
    if bits > 0 && (acc & ((1u64 << bits).saturating_sub(1))) != 0 {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "non-zero base32 padding bits",
        });
    }
    Ok(out)
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "测试里用断言即可"
)]
mod tests {
    use super::*;
    use crate::crypto::Argon2Params;

    fn test_key() -> DirnameKey {
        let kek = Kek::from_password(b"pw", &[7u8; 16], Argon2Params::TEST_WEAK).unwrap();
        DirnameKey::derive(&kek, &[7u8; 16])
    }

    fn nonce(seed: u8) -> [u8; NONCE_LEN] {
        core::array::from_fn(|i| (i as u8).wrapping_mul(3) ^ seed)
    }

    #[test]
    fn roundtrips_ascii_and_chinese() {
        let k = test_key();
        for name in ["docs", "工作资料", "a b c", "photos.2024"] {
            let e = encrypt_dirname(name, &k, &nonce(1), CipherId::ChaCha20Poly1305).unwrap();
            assert!(e.sidecar.is_none(), "{name} 不该需要边车文件");
            let back =
                decrypt_dirname(&e.disk_name, None, &k, CipherId::ChaCha20Poly1305).unwrap();
            assert_eq!(back, name);
        }
    }

    #[test]
    fn encoded_name_has_no_slash_and_is_uppercase() {
        // 这条守护「必须用 base32」的两个理由。base64 会在这里失败：
        // 它含 `/`（路径分隔符）且大小写混合（Windows 上会撞名）
        let k = test_key();
        let e = encrypt_dirname("工作资料", &k, &nonce(2), CipherId::ChaCha20Poly1305).unwrap();
        let stem = e.disk_name.strip_suffix(DIR_SUFFIX).unwrap();
        assert!(!stem.contains('/'), "不能含路径分隔符");
        assert!(!stem.contains('\\'), "不能含反斜杠");
        assert!(
            stem.bytes().all(|b| B32.contains(&b)),
            "只能出现 base32 字母表内的字符，实际 {stem}"
        );
    }

    #[test]
    fn case_insensitive_filesystems_cannot_collide() {
        // 大小写不敏感的文件系统上，只有「密文名不含小写」才能保证
        // 两个不同密文不会折叠成同一个磁盘名。逐字符检查大写
        let k = test_key();
        for seed in 0..40u8 {
            let e = encrypt_dirname("x", &k, &nonce(seed), CipherId::ChaCha20Poly1305).unwrap();
            let stem = e.disk_name.strip_suffix(DIR_SUFFIX).unwrap();
            assert_eq!(
                stem,
                stem.to_uppercase(),
                "第 {seed} 个密文名含小写字母，在 Windows/APFS 上会与其它名字折叠"
            );
        }
    }

    #[test]
    fn different_nonce_yields_different_ciphertext() {
        // 相同目录名必须产出不同密文，否则「这两个目录同名」会泄露
        let k = test_key();
        let a = encrypt_dirname("同一个名字", &k, &nonce(1), CipherId::ChaCha20Poly1305).unwrap();
        let b = encrypt_dirname("同一个名字", &k, &nonce(2), CipherId::ChaCha20Poly1305).unwrap();
        assert_ne!(a.disk_name, b.disk_name, "相同明文不该产出相同密文名");
        // 但都要能解回来
        for e in [a, b] {
            let back =
                decrypt_dirname(&e.disk_name, None, &k, CipherId::ChaCha20Poly1305).unwrap();
            assert_eq!(back, "同一个名字");
        }
    }

    #[test]
    fn long_name_is_truncated_with_sidecar() {
        // 130 字节以上的名字会超过 255 组件上限。这不是极端情况：
        // 40 多个汉字就到了
        let k = test_key();
        let long = "很长的目录名".repeat(40); // 6 字 × 40 = 240 字 = 720 字节
        let e = encrypt_dirname(&long, &k, &nonce(3), CipherId::ChaCha20Poly1305).unwrap();

        assert!(
            e.disk_name.len() <= MAX_COMPONENT,
            "截断后必须落在文件系统上限内，实际 {} 字节",
            e.disk_name.len()
        );
        assert!(e.disk_name.contains('~'), "截断名要有 ~ 标记");
        let sc = e.sidecar.as_deref().expect("截断时必须给出边车内容");

        let back = decrypt_dirname(&e.disk_name, Some(sc), &k, CipherId::ChaCha20Poly1305).unwrap();
        assert_eq!(back, long, "靠边车文件要能完整还原");
    }

    #[test]
    fn truncated_names_of_same_plaintext_still_differ() {
        // 变异测试抓出来的缺口：截断名的哈希后缀若算的是**明文名**，
        // 同名的两个长目录会得到完全相同的后缀，于是观察者不用解密就知道
        // 「这两个目录同名」——而藏住目录名正是这个模式的全部意义。
        //
        // 前一条 different_nonce_yields_different_ciphertext 抓不到它：
        // 那里用的是短名字，走的是不截断的分支，压根没碰到哈希后缀。
        let k = test_key();
        let long = "同一个很长的目录名".repeat(30);
        let a = encrypt_dirname(&long, &k, &nonce(11), CipherId::ChaCha20Poly1305).unwrap();
        let b = encrypt_dirname(&long, &k, &nonce(12), CipherId::ChaCha20Poly1305).unwrap();

        assert!(a.disk_name.contains('~') && b.disk_name.contains('~'), "两者都该被截断");
        assert_ne!(a.disk_name, b.disk_name, "同名长目录不该产出相同的磁盘名");

        // 后缀本身也要不同：只比整个名字的话，前 200 字符已经不同就够了，
        // 而后缀相同仍然泄露信息
        let suffix = |s: &str| -> String {
            s.rsplit_once('~').map(|(_, t)| t.to_owned()).unwrap_or_default()
        };
        assert_ne!(
            suffix(&a.disk_name),
            suffix(&b.disk_name),
            "哈希后缀相同就泄露了「这两个目录同名」"
        );

        // 而且都要能靠边车文件还原
        for e in [a, b] {
            let sc = e.sidecar.as_deref().unwrap();
            let back =
                decrypt_dirname(&e.disk_name, Some(sc), &k, CipherId::ChaCha20Poly1305).unwrap();
            assert_eq!(back, long);
        }
    }

    #[test]
    fn truncated_name_without_sidecar_reports_clearly() {
        // 不给 sidecar 时必须明确报「需要边车文件」，而不是去解半截密文
        // 得到 AEAD 认证失败——后者会把人引向「密码不对」的错误方向
        let k = test_key();
        let long = "长".repeat(200);
        let e = encrypt_dirname(&long, &k, &nonce(4), CipherId::ChaCha20Poly1305).unwrap();
        let err = decrypt_dirname(&e.disk_name, None, &k, CipherId::ChaCha20Poly1305).unwrap_err();
        match err {
            Error::MalformedTlv { reason, .. } => {
                assert!(reason.contains("sidecar"), "错误应指明缺边车文件，实际 {reason}");
            }
            other => panic!("应报缺边车文件，实际 {other:?}"),
        }
    }

    #[test]
    fn wrong_key_fails_authentication() {
        let k = test_key();
        let e = encrypt_dirname("secret", &k, &nonce(5), CipherId::ChaCha20Poly1305).unwrap();

        let other_kek =
            Kek::from_password(b"different", &[7u8; 16], Argon2Params::TEST_WEAK).unwrap();
        let other = DirnameKey::derive(&other_kek, &[7u8; 16]);
        assert!(
            decrypt_dirname(&e.disk_name, None, &other, CipherId::ChaCha20Poly1305).is_err(),
            "错误密钥必须解不开"
        );
    }

    #[test]
    fn different_vault_salt_yields_different_key() {
        // 目录名密钥绑定 vault_salt。这条守护「不同 vault 的目录名互不可见」
        let kek = Kek::from_password(b"pw", &[7u8; 16], Argon2Params::TEST_WEAK).unwrap();
        let a = DirnameKey::derive(&kek, &[7u8; 16]);
        let b = DirnameKey::derive(&kek, &[9u8; 16]);
        let e = encrypt_dirname("x", &a, &nonce(6), CipherId::ChaCha20Poly1305).unwrap();
        assert!(
            decrypt_dirname(&e.disk_name, None, &b, CipherId::ChaCha20Poly1305).is_err(),
            "换了 vault_salt 就不该解得开"
        );
    }

    #[test]
    fn rejects_path_components() {
        // 传进来一个路径而不是单个组件，说明调用方用错了。
        // 静默接受会产出跨目录的密文名，解开时凭空多一层
        let k = test_key();
        for bad in ["a/b", "a\\b", ""] {
            assert!(
                encrypt_dirname(bad, &k, &nonce(7), CipherId::ChaCha20Poly1305).is_err(),
                "{bad:?} 必须被拒绝"
            );
        }
    }

    #[test]
    fn base32_matches_rfc4648_vectors() {
        // RFC 4648 §10 的官方测试向量。手写编解码必须对齐标准，
        // 否则别的实现解不开我们的目录名
        assert_eq!(base32_encode(b""), "");
        assert_eq!(base32_encode(b"f"), "MY");
        assert_eq!(base32_encode(b"fo"), "MZXQ");
        assert_eq!(base32_encode(b"foo"), "MZXW6");
        assert_eq!(base32_encode(b"foob"), "MZXW6YQ");
        assert_eq!(base32_encode(b"fooba"), "MZXW6YTB");
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
    }

    #[test]
    fn base32_matches_python_stdlib_at_length_boundaries() {
        // RFC 向量只覆盖长度 1–6 的 ASCII，而手写位操作最容易错在
        // **长度 mod 5 的边界**上（余 1/2/3/4 字节时该产出几个字符、
        // 末尾几位怎么填）。那几个向量恰好能被一个写错移位量的实现凑对。
        //
        // 下面的参考值由 Python `base64.b32encode` 独立生成——用另一个
        // 实现做参照，而不是拿自己的输出当预期值（后者只能证明
        // 「函数是确定性的」）。
        let data: Vec<u8> = (0..256u32).map(|i| ((i * 11) ^ 0x5B) as u8).collect();
        let expected: &[(usize, &str)] = &[
            (1, "LM"),
            (2, "LNIA"),
            (3, "LNIE2"),
            (4, "LNIE26Q"),
            (5, "LNIE26TX"),
            (6, "LNIE26TXNQ"),
            (7, "LNIE26TXNQMQ"),
            (8, "LNIE26TXNQMRM"),
            (31, "LNIE26TXNQMRMAZYGURN7VGB73V6BHMKQ66KTJSTJBCXE33ECE"),
            (32, "LNIE26TXNQMRMAZYGURN7VGB73V6BHMKQ66KTJSTJBCXE33ECEHA"),
            (33, "LNIE26TXNQMRMAZYGURN7VGB73V6BHMKQ66KTJSTJBCXE33ECEHDW"),
        ];
        for &(len, want) in expected {
            assert_eq!(
                base32_encode(&data[..len]),
                want,
                "长度 {len}（mod 5 = {}）与 Python 标准库不一致",
                len % 5
            );
        }

        // 再覆盖全部 1..=64 的字符数。上面只在 11 个长度上比对，
        // 这条能抓到「余数长度算错」——漏掉末尾不满 5 位的组，
        // 或者多补一个字符
        for len in 1..=64usize {
            let want_chars = (len * 8).div_ceil(5);
            assert_eq!(
                base32_encode(&data[..len]).len(),
                want_chars,
                "长度 {len} 应产出 {want_chars} 个字符"
            );
        }
    }

    #[test]
    fn base32_roundtrips_arbitrary_bytes() {
        // 各字节都不同，按错误偏移拼接更容易被测出来
        let data: Vec<u8> = (0..=255u8).map(|i| i.wrapping_mul(11) ^ 0x5B).collect();
        for len in [1usize, 2, 3, 4, 5, 6, 7, 8, 31, 32, 33, 255] {
            let slice = &data[..len];
            let enc = base32_encode(slice);
            let dec = base32_decode(&enc).unwrap();
            assert_eq!(dec, slice, "长度 {len} 往返失败");
        }
    }

    #[test]
    fn base32_rejects_invalid_chars() {
        // 小写字母、数字 0/1/8/9 都不在字母表里（避免与 O/I/B/g 混淆）
        for bad in ["mzxw", "MZXW0", "MZXW1", "MZXW8", "MZ=XW"] {
            assert!(base32_decode(bad).is_err(), "{bad} 应被拒绝");
        }
    }

    #[test]
    fn base32_rejects_nonzero_padding_bits() {
        // 编码后余下的位必须为 0。不检查的话篡改者能在这里塞信息，
        // 而解码结果看起来完全正常
        assert!(base32_decode("MY").is_ok(), "MY 的填充位是 0");
        // "MZ" = 01100 11001 → 取出 0x66 后余 1 位为 1
        assert!(base32_decode("MZ").is_err(), "非零填充位必须被拒绝");
    }

    #[test]
    fn looks_encrypted_is_precise() {
        let k = test_key();
        let e = encrypt_dirname("x", &k, &nonce(8), CipherId::ChaCha20Poly1305).unwrap();
        assert!(looks_encrypted(&e.disk_name));

        // 用户自己建的目录不该被误判——否则扫描时会拿它去解密并报错，
        // 界面上出现一堆「无法解密」的噪音
        for plain in ["docs", "工作资料.omy", "my-stuff.omy", ".omy", "docs.txt"] {
            assert!(!looks_encrypted(plain), "{plain} 不该被判为密文目录名");
        }
    }
}
