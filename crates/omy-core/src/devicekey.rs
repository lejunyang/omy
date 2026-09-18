//! 设备密钥：把一把随机 KEK 交给这台机器的安全硬件保管。
//!
//! # 它是什么
//!
//! 一个普通的 slot，和密码槽、恢复码槽并列。区别只在**这把 KEK 从哪来**：
//!
//! | 槽位 | KEK 的来源 |
//! |---|---|
//! | 密码 | Argon2 从密码派生 |
//! | 恢复码 | HKDF 从 26 个词的熵派生 |
//! | 设备密钥 | **随机生成**，密文交给 TPM 保管 |
//!
//! 前两者可以随时重新算出来（记得密码/拿着纸条就行），设备密钥不能——
//! 它只存在于那台机器的硬件里。这个差别决定了它的全部性质。
//!
//! # 为什么绑 `vault_salt`
//!
//! `vault_salt` 在同一个 vault 内是相同的（见 [`crate::header::FixedHeader`]），
//! 所以拿它当保管 id 意味着「一个库共用一把设备密钥」：
//!
//! - 给库里任一文件挂上设备密钥，同库其它文件也能用同一把开
//! - 换个库自动隔离，不会互相解开
//!
//! 换成绑文件路径就不成立了：文件改名、移动之后 id 就变了，密钥还在
//! TPM 里但再也对不上——表现为「明明开过一次，第二天打不开了」。
//!
//! # 它绝不能是唯一凭据
//!
//! 换机器、重装系统、清除 TPM、重置 Hello——任一发生，这个槽就永久
//! 解不开。所以挂设备密钥时必须已经有一个密码槽，删密码槽时也必须
//! 拦住「只剩设备密钥」的状态。
//!
//! 这一条不是提醒，是需要代码强制的：用户很容易觉得「有指纹就够了」，
//! 而代价要到换电脑那天才显现，那时数据已经拿不回来了。
//!
//! # 它不比密码更安全
//!
//! TPM 挡得住硬盘被偷和换机器解密，挡不住**正在这台机器上以你的身份
//! 运行的恶意程序**——它可以在你按下指纹之后的窗口里发起解封。所以
//! 设备密钥的定位是**省去每次输密码**，不是「更安全」。
//!
//! 界面上必须这么说。反过来说会让用户因为开了它而把密码设得更弱，
//! 那是净损失。

use crate::crypto::{Kek, SecretKey};
use hkdf::Hkdf;
use sha2::Sha256;

/// HKDF info：把硬件保管的那把密钥派生成 KEK。
///
/// 与恢复码、目录名等用不同的 info：同一份熵派生出的两把密钥不能相等，
/// 否则一处的密文可以拿到另一处去试。
const INFO_DEVICE: &[u8] = b"omy/v1/device-key";

/// 硬件保管的那把密钥的长度。
pub const SECRET_LEN: usize = 32;

/// 保管 id 用的前缀，避免与别的用途撞名。
const ID_PREFIX: &str = "vault-";

/// 一个 vault 的设备密钥在硬件里的保管 id。
///
/// 用 `vault_salt` 而不是文件路径：路径会变（改名、移动），用它当 id
/// 会让密钥一改名就对不上——密钥还在 TPM 里，但再也找不到。
#[must_use]
pub fn slot_id(vault_salt: &[u8; 16]) -> String {
    let mut s = String::with_capacity(ID_PREFIX.len().saturating_add(32));
    s.push_str(ID_PREFIX);
    for b in vault_salt {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 把硬件保管的密钥派生成 KEK。
///
/// 不直接把那 32 字节当 KEK 用：保管层（`omy-secret`）与加密层是两个
/// 独立的信任域，中间过一次 HKDF 才能保证「保管层泄露一把密钥」不等于
/// 「直接拿到 vault 的 KEK」——虽然实际影响有限，但这层隔离几乎免费。
#[must_use]
pub fn kek_from_secret(secret: &[u8; SECRET_LEN], vault_salt: &[u8; 16]) -> Kek {
    let hk = Hkdf::<Sha256>::new(Some(vault_salt), secret);
    let mut out = [0u8; 32];
    // 32 字节远小于 SHA-256 的 255*32 上限，不可能失败
    hk.expand(INFO_DEVICE, &mut out)
        .unwrap_or_else(|_| unreachable!("HKDF expand 32 bytes cannot fail"));
    Kek::from_key(SecretKey::from_bytes(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_id_is_stable_and_vault_scoped() {
        // 同一个库要得到同一个 id，否则每次挂设备密钥都会在 TPM 里
        // 多留一把用不到的密钥，而旧的那把再也找不到
        let a = [7u8; 16];
        assert_eq!(slot_id(&a), slot_id(&a), "同一个 salt 必须得到同一个 id");

        // 不同库必须隔离，否则 A 库的设备密钥能开 B 库
        let mut b = a;
        b[0] = 8;
        assert_ne!(slot_id(&a), slot_id(&b), "不同 salt 必须得到不同 id");

        // 十六进制编码：id 会当文件名用，必须只含安全字符
        assert!(
            slot_id(&a).strip_prefix(ID_PREFIX).is_some_and(|h| h
                .chars()
                .all(|c| c.is_ascii_hexdigit())),
            "id 里出现了非十六进制字符"
        );
    }

    #[test]
    fn same_secret_different_vault_gives_different_kek() {
        // 关键性质：同一台机器上的两个库即使拿到同一把硬件密钥，
        // 派生出的 KEK 也必须不同——否则 A 库的设备密钥能开 B 库。
        //
        // 这条由 HKDF 的 salt 保证。不这样会怎样：所有库共用一把 KEK，
        // 而用户以为它们是隔离的
        let secret = [0x5Au8; SECRET_LEN];
        let k1 = kek_from_secret(&secret, &[1u8; 16]);
        let k2 = kek_from_secret(&secret, &[2u8; 16]);
        assert_ne!(
            k1.fingerprint(&[0u8; 16]),
            k2.fingerprint(&[0u8; 16]),
            "不同 vault 派生出了相同的 KEK"
        );
    }

    #[test]
    fn derivation_is_deterministic() {
        // 同样的输入必须得到同样的 KEK，否则今天挂上的设备密钥
        // 明天就开不了了
        let secret = [0x33u8; SECRET_LEN];
        let salt = [9u8; 16];
        assert_eq!(
            kek_from_secret(&secret, &salt).fingerprint(&salt),
            kek_from_secret(&secret, &salt).fingerprint(&salt),
        );
    }

    #[test]
    fn different_secret_gives_different_kek() {
        // 反证：换一把硬件密钥必须得到不同的 KEK。
        // 否则「重新生成设备密钥」这个操作等于什么都没做
        let salt = [9u8; 16];
        let k1 = kek_from_secret(&[1u8; SECRET_LEN], &salt);
        let k2 = kek_from_secret(&[2u8; SECRET_LEN], &salt);
        assert_ne!(k1.fingerprint(&salt), k2.fingerprint(&salt));
    }
}
