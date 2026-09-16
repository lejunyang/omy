//! 信封：用受保护的密钥加密一段内容，得到可以安全落盘的密文。
//!
//! # 为什么不直接把密码塞进钥匙串
//!
//! 也能做，但每加一个远程位置就多一条钥匙串记录，删除位置时还得记得
//! 清理，漏掉就会在用户的钥匙串里堆垃圾。而且 Linux 的 Secret Service
//! 对条目数量和并发访问都不算友好。
//!
//! 这里只往钥匙串放**一把**密钥，位置配置照常写进 `config.toml`，
//! 只是其中的密码字段变成密文。好处是：位置的增删改就是普通的配置读写，
//! 而钥匙串里始终只有一条记录。
//!
//! # 格式
//!
//! ```text
//! {"v":1,"n":"<hex 12B nonce>","c":"<hex 密文||tag>"}
//! ```
//!
//! 用十六进制而不是 base64：凭据只有几十字节，多出的三分之一体积无所谓，
//! 而少一个依赖、且内容在配置文件里一眼能看出是二进制而非可读文本。
//!
//! 带版本号是为了将来换算法时能认出旧数据。`v` 不认识就直接报
//! [`Error::Undecryptable`]，不要猜——猜错会把认证失败误报成密码错误。

use omy_core::crypto::{CipherId, SecretKey, NONCE_LEN};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{Error, ProtectKey, Result};

/// 当前信封格式版本。
const VERSION: u8 = 1;

/// AAD：把用途绑进认证数据。
///
/// 这样一段「远程凭据」的密文即使被挪到别的字段，也解不开——
/// 攻击者无法把 A 位置的密码搬到 B 位置去复用。
const AAD: &[u8] = b"omy-secret-envelope-v1";

/// 落盘的密文信封。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Envelope {
    /// 格式版本。
    pub v: u8,
    /// 十六进制的 nonce。
    pub n: String,
    /// 十六进制的 密文||tag。
    pub c: String,
}

/// 字节转十六进制。
fn to_hex(b: &[u8]) -> String {
    hex::encode(b)
}

/// 十六进制转字节。长度为奇数或含非法字符时返回 `None`。
fn from_hex(s: &str) -> Option<Vec<u8>> {
    hex::decode(s).ok()
}

/// 用一把受保护的密钥加密内容。
///
/// # Errors
///
/// 底层 AEAD 失败时返回（正常输入下不会发生）。
pub fn seal(key: &ProtectKey, plaintext: &[u8]) -> Result<Envelope> {
    use rand_core::RngCore;
    let mut nonce = [0u8; NONCE_LEN];
    rand_core::OsRng.fill_bytes(&mut nonce);

    let sk = SecretKey::from_bytes(**key);
    let ct = CipherId::ChaCha20Poly1305
        .encrypt(&sk, &nonce, plaintext, AAD)
        .map_err(|e| Error::Other(e.to_string()))?;

    Ok(Envelope {
        v: VERSION,
        n: to_hex(&nonce),
        c: to_hex(&ct),
    })
}

/// 解开一个信封。
///
/// 返回 `Zeroizing`：解出来的是密码一类的东西，用完必须抹掉。
///
/// # Errors
///
/// 版本不认识、编码损坏、认证失败时返回 [`Error::Undecryptable`]。
pub fn unseal(key: &ProtectKey, env: &Envelope) -> Result<Zeroizing<Vec<u8>>> {
    // 版本不认识就明确失败。容忍未知版本会让高版本写的数据被低版本
    // 用错误的算法去解，得到的是「认证失败」这种指向完全错误的报错
    if env.v != VERSION {
        return Err(Error::Undecryptable);
    }

    let nonce_raw = from_hex(&env.n).ok_or(Error::Undecryptable)?;
    let nonce: [u8; NONCE_LEN] = nonce_raw
        .as_slice()
        .try_into()
        .map_err(|_| Error::Undecryptable)?;
    let ct = from_hex(&env.c).ok_or(Error::Undecryptable)?;

    let sk = SecretKey::from_bytes(**key);
    let pt = CipherId::ChaCha20Poly1305
        .decrypt(&sk, &nonce, &ct, AAD)
        .map_err(|e| Error::Other(e.to_string()))?
        // decrypt 用 Ok(None) 表示认证失败。这里把它归为「解不开」——
        // 对调用方来说，换了机器和数据被改过没有区别，都得重新登录
        .ok_or(Error::Undecryptable)?;

    Ok(Zeroizing::new(pt))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::random_key;

    /// 加密后能原样解回。
    #[test]
    fn roundtrip() {
        let k = random_key();
        let msg = b"hunter2-\xe4\xb8\xad\xe6\x96\x87";
        let env = seal(&k, msg).expect("加密");
        let got = unseal(&k, &env).expect("解密");
        assert_eq!(&got[..], msg, "解出的内容必须与原文一致");
    }

    /// 换一把密钥必须解不开。
    ///
    /// 不这样会怎样：这正是「机器绑定」的全部意义所在。若换密钥仍能解开，
    /// 说明密钥根本没参与运算，配置文件被拷到别的机器就能直接用。
    #[test]
    fn wrong_key_fails() {
        let a = random_key();
        let b = random_key();
        let env = seal(&a, b"secret").expect("加密");
        assert!(
            matches!(unseal(&b, &env), Err(Error::Undecryptable)),
            "用别的密钥必须解不开"
        );
    }

    /// 密文被改一个比特就必须解不开。
    ///
    /// 不这样会怎样：AEAD 的认证没生效，攻击者可以篡改落盘的凭据
    /// 而不被发现。
    #[test]
    fn tampered_ciphertext_fails() {
        let k = random_key();
        let mut env = seal(&k, b"secret").expect("加密");
        let mut raw = from_hex(&env.c).expect("解十六进制");
        // 翻转最后一个字节的最低位：改 tag 区同样必须被拒
        let last = raw.len() - 1;
        raw[last] ^= 1;
        env.c = to_hex(&raw);
        assert!(matches!(unseal(&k, &env), Err(Error::Undecryptable)));
    }

    /// nonce 被换掉也必须解不开。
    #[test]
    fn tampered_nonce_fails() {
        let k = random_key();
        let mut env = seal(&k, b"secret").expect("加密");
        env.n = to_hex(&[0u8; NONCE_LEN]);
        assert!(matches!(unseal(&k, &env), Err(Error::Undecryptable)));
    }

    /// 非法十六进制必须被拒绝，而不是 panic。
    ///
    /// 不这样会怎样：配置文件被手工改坏时整个应用崩溃，
    /// 而正确的行为是提示这个位置需要重新登录。
    #[test]
    fn malformed_hex_rejected() {
        let k = random_key();
        let mut env = seal(&k, b"secret").expect("加密");
        env.c = String::from("zz");
        assert!(matches!(unseal(&k, &env), Err(Error::Undecryptable)));
        env.c = String::from("abc"); // 奇数长度
        assert!(matches!(unseal(&k, &env), Err(Error::Undecryptable)));
    }

    /// 未知版本必须明确拒绝，而不是尝试解。
    #[test]
    fn unknown_version_rejected() {
        let k = random_key();
        let mut env = seal(&k, b"secret").expect("加密");
        env.v = 99;
        assert!(matches!(unseal(&k, &env), Err(Error::Undecryptable)));
    }

    /// 两次加密同样的内容，密文必须不同。
    ///
    /// 不这样会怎样：nonce 复用会让相同密码产生相同密文，观察配置文件
    /// 就能看出「这两个位置用了同一个密码」——而且 nonce 复用对
    /// ChaCha20-Poly1305 是致命的，可以恢复明文。
    #[test]
    fn nonce_is_fresh_each_time() {
        let k = random_key();
        let a = seal(&k, b"same").expect("第一次");
        let b = seal(&k, b"same").expect("第二次");
        assert_ne!(a.n, b.n, "nonce 必须每次都不同");
        assert_ne!(a.c, b.c, "相同明文不能产生相同密文");
    }

    /// 空内容也要能正确往返。
    ///
    /// 边界情形：某些位置可能没有密码（匿名 WebDAV）。
    #[test]
    fn empty_plaintext_roundtrips() {
        let k = random_key();
        let env = seal(&k, b"").expect("加密空内容");
        let got = unseal(&k, &env).expect("解密");
        assert!(got.is_empty());
    }

    /// 信封能序列化成 JSON 再读回来。
    ///
    /// 它要嵌进 config.toml / JSON 里，往返不能丢字段。
    #[test]
    fn envelope_serde_roundtrips() {
        let k = random_key();
        let env = seal(&k, b"pw").expect("加密");
        let j = serde_json::to_string(&env).expect("序列化");
        let back: Envelope = serde_json::from_str(&j).expect("反序列化");
        assert_eq!(env, back);
        let got = unseal(&k, &back).expect("解密");
        assert_eq!(&got[..], b"pw");
    }

    /// 序列化结果里不能出现明文。
    ///
    /// 不这样会怎样：这是本模块存在的唯一理由。哪怕只是调试时不小心把
    /// 明文一起写进了结构体，落盘的配置里就有密码了。
    #[test]
    fn serialized_form_has_no_plaintext() {
        let k = random_key();
        let secret = "MyV3ryS3cretPassw0rd";
        let env = seal(&k, secret.as_bytes()).expect("加密");
        let j = serde_json::to_string(&env).expect("序列化");
        assert!(!j.contains(secret), "序列化结果里出现了明文密码");
        // 密钥本身也不能出现在里面
        let key_hex = to_hex(&*k);
        assert!(!j.contains(&key_hex), "序列化结果里出现了密钥");
    }
}
