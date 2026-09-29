//! 机器绑定：把密钥交给操作系统的原生凭据库。
//!
//! | 平台 | 后端 | 绑定到 |
//! |---|---|---|
//! | Windows | Credential Manager（内部用 DPAPI）| 这台机器 + 这个 Windows 账户 |
//! | macOS | Keychain Services | 这台机器 + 这个登录钥匙串 |
//! | Linux | Secret Service（D-Bus）| 这个用户的钥匙串，通常随登录解锁 |
//! | Android | 应用私有目录（见下）| 这个应用的沙箱 |
//!
//! # Linux 为什么不用内核 keyutils
//!
//! `keyring` crate 在 Linux 上的默认后端是内核 keyutils，而**内核 keyring
//! 重启就没了**。用它存的远程位置密码，每次开机都要重新输入——这和没有
//! 持久化差不多，偏偏 API 上完全看不出来，只有真的重启一次才会发现。
//!
//! 所以这里显式要求 Secret Service（gnome-keyring / KWallet / KeePassXC
//! 都实现了它）。
//! 代价是 headless 服务器上没有 D-Bus 就用不了，那时**如实报
//! [`Error::NoBackend`]**，让上层告诉用户「装一个 gnome-keyring，或者
//! 这个位置只能每次手动输密码」。
//!
//! # 存入前必须编码成纯文本（兼容所有提供者）
//!
//! Secret Service 规范允许 secret 是任意字节，但提供者的实现质量参差：
//! kwalletd5 及 Plasma 6 早期的 ksecretd 会把 `text/*` 类型的数据经
//! `QString::fromUtf8` 处理，非法字节被替换成 U+FFFD（KDE bug #520509）。
//! 而 keyring crate 硬编码 `text/plain` 且我们存的是 32 字节随机密钥——
//! 在 KDE 上必然被损坏，取回长度不对导致每次都重建密钥（远程密码全部要
//! 重输）。这是 KDE 上大量软件（Zed、Salesforce CLI 等）的共性问题。
//!
//! 不能要求用户换提供者，所以存入前用 hex 编码成纯 ASCII、读回再解码
//! （见 [`encode_key_text`]），对任何提供者及其版本都免疫。
//!
//! 这条是刻意的取舍：宁可明说不支持，也不要悄悄退回一个重启就失效、
//! 或者干脆明文落盘的方案——用户会以为密码被保护着。
//!
//! # Android 为什么不走 Keystore
//!
//! Android Keystore 要通过 JNI 调 Java API，而本 crate 是纯 Rust 且不持有
//! `JNIEnv`。当前实现退回到**应用私有目录**里的密钥文件：它受 Android 的
//! 应用沙箱保护，其他应用读不到，未 root 的设备上也拿不到——保护级别
//! 低于 Keystore（没有硬件支持、也无法要求生物识别），但明显好于把密码
//! 明文写进配置。
//!
//! 接 Keystore 是后续的事，而且正好落在 [`crate::Protector`] 的另一个实现上，
//! 届时上层无需改动。这一点在 `SKILL` 式的抽象里已经预留好了。

use crate::{Error, ProtectKey, Protector, Result, KEY_LEN};
use zeroize::Zeroizing;

/// 文本编码密钥的前缀：识别新旧两种存储格式（见模块文档「存入前必须编码」）。
const TEXT_KEY_PREFIX: &str = "omyhex:";

/// 把密钥编码成 `omyhex:<hex>` 纯 ASCII 字符串，供凭据库存储。
fn encode_key_text(key: &ProtectKey) -> Zeroizing<String> {
    let mut s = String::with_capacity(TEXT_KEY_PREFIX.len() + KEY_LEN * 2);
    s.push_str(TEXT_KEY_PREFIX);
    for b in key.as_ref() {
        use std::fmt::Write;
        let _ = write!(&mut s, "{b:02x}");
    }
    Zeroizing::new(s)
}

/// 解码凭据库里的密钥：带新前缀的走 hex 解码；无格式前缀、恰好为
/// [`KEY_LEN`] 字节的视为旧版裸二进制（兼容已发布版本写入的条目）；
/// 其余（如被提供者损坏过的数据）返回 `None`，让上层按「没有」重建。
fn decode_key_text(raw: &[u8]) -> Option<ProtectKey> {
    if let Some(hexpart) = raw.strip_prefix(TEXT_KEY_PREFIX.as_bytes()) {
        if hexpart.len() != KEY_LEN * 2 {
            return None;
        }
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        for (i, pair) in hexpart.chunks_exact(2).enumerate() {
            let hi = char::from(pair[0]).to_digit(16)?;
            let lo = char::from(pair[1]).to_digit(16)?;
            key[i] = u8::try_from((hi << 4) | lo).ok()?;
        }
        return Some(key);
    }
    // 旧格式：直接存的裸 32 字节
    raw.try_into().map(Zeroizing::new).ok()
}

/// 用系统原生凭据库保管密钥。
pub struct MachineProtector {
    /// 服务名，用于在钥匙串里归类。
    service: String,
}

impl MachineProtector {
    /// 新建一个。
    ///
    /// # Errors
    ///
    /// 当前环境没有可用后端时返回 [`Error::NoBackend`]。
    pub fn new(service: &str) -> Result<Self> {
        let p = Self {
            service: service.to_owned(),
        };
        p.probe()?;
        Ok(p)
    }

    /// 探测后端是否真的可用。
    ///
    /// 必须**真的做一次读写**，而不是看平台类型就返回可用：Linux 上
    /// D-Bus 可能没跑、钥匙串可能锁着，这些只有实际操作才会暴露。
    /// 判断错的后果是用户添加位置时才失败，而那时他已经输完密码了。
    fn probe(&self) -> Result<()> {
        let probe_id = "__omy_probe__";
        let key = Zeroizing::new([0u8; KEY_LEN]);
        self.store(probe_id, &key)?;
        let _ = self.delete(probe_id);
        Ok(())
    }
}

// ---------------- 桌面三平台：原生凭据库 ----------------

#[cfg(any(
    target_os = "windows",
    target_os = "macos",
    all(unix, not(target_os = "macos"), not(target_os = "android"))
))]
mod imp {
    use super::{Error, ProtectKey, Result, decode_key_text, encode_key_text};

    /// 把 keyring 的错误翻成我们的分类。
    ///
    /// 区分 `NoEntry` 尤其要紧：它表示「没存过」，是正常情况，
    /// 而不是故障。混为一谈会让首次使用报错。
    fn map_err(e: &keyring::Error) -> Error {
        match e {
            keyring::Error::NoEntry => Error::NotFound,
            keyring::Error::NoStorageAccess(inner) => Error::NoBackend(inner.to_string()),
            // 平台不支持某种用法时也归为「没有后端」，让上层给出
            // 「换一种方式」的提示，而不是含糊的失败
            keyring::Error::PlatformFailure(inner) => Error::NoBackend(inner.to_string()),
            other => Error::Backend(other.to_string()),
        }
    }

    fn entry(service: &str, id: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(service, id).map_err(|e| map_err(&e))
    }

    pub(super) fn retrieve(service: &str, id: &str) -> Result<ProtectKey> {
        let raw = entry(service, id)?
            .get_secret()
            .map_err(|e| map_err(&e))?;
        // 解码两种存储格式（见 decode_key_text）；解不出说明这条记录不是
        // 我们写的、或已被提供者损坏。当成「没有」而不是报错，让上层
        // 重新建一把——否则用户会卡在一个自己无法修复的错误上。
        decode_key_text(raw.as_slice()).ok_or(Error::NotFound)
    }

    pub(super) fn store(service: &str, id: &str, key: &ProtectKey) -> Result<()> {
        // 先编码成纯 ASCII：避免 KWallet 系提供者损坏二进制 secret
        // （见模块文档「存入前必须编码成纯文本」）。
        let text = encode_key_text(key);
        entry(service, id)?
            .set_secret(text.as_bytes())
            .map_err(|e| map_err(&e))
    }

    pub(super) fn delete(service: &str, id: &str) -> Result<()> {
        match entry(service, id)?.delete_credential() {
            Ok(()) => Ok(()),
            // 已经不在了就是成功：删除必须幂等
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(&e)),
        }
    }

    pub(super) const BACKEND_NAME: &str = if cfg!(target_os = "windows") {
        "Windows 凭据管理器"
    } else if cfg!(target_os = "macos") {
        "macOS 钥匙串"
    } else {
        "Secret Service"
    };
}

// ---------------- Android：应用私有目录 ----------------

#[cfg(target_os = "android")]
mod imp {
    use super::{Error, ProtectKey, Result, KEY_LEN};
    use zeroize::Zeroizing;

    /// 密钥文件放在应用私有目录下。
    ///
    /// 不放外部存储：那里其他应用能读，等于没保护。
    fn key_path(service: &str, id: &str) -> Result<std::path::PathBuf> {
        let base = omy_config::data_dir()
            .ok_or_else(|| Error::Other(String::from("找不到数据目录")))?
            .join("keys");
        std::fs::create_dir_all(&base).map_err(|e| Error::Other(e.to_string()))?;
        // id 可能含斜杠等字符，做一次无损转义再当文件名
        let safe: String = id
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        Ok(base.join(format!("{service}-{safe}.key")))
    }

    pub(super) fn retrieve(service: &str, id: &str) -> Result<ProtectKey> {
        let p = key_path(service, id)?;
        let raw = match std::fs::read(&p) {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::NotFound),
            Err(e) => return Err(Error::Backend(e.to_string())),
        };
        let bytes: [u8; KEY_LEN] = raw.as_slice().try_into().map_err(|_| Error::NotFound)?;
        Ok(Zeroizing::new(bytes))
    }

    pub(super) fn store(service: &str, id: &str, key: &ProtectKey) -> Result<()> {
        let p = key_path(service, id)?;
        // 用原子写，避免写一半掉电留下半截密钥文件——那会让所有凭据
        // 都解不开，而且看起来像「数据损坏」
        omy_core::fsatomic::write_atomic(&p, key.as_slice())
            .map_err(|e| Error::Backend(e.to_string()))
    }

    pub(super) fn delete(service: &str, id: &str) -> Result<()> {
        let p = key_path(service, id)?;
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Backend(e.to_string())),
        }
    }

    pub(super) const BACKEND_NAME: &str = "应用私有存储";
}

impl Protector for MachineProtector {
    fn name(&self) -> &'static str {
        imp::BACKEND_NAME
    }

    fn retrieve(&self, id: &str) -> Result<ProtectKey> {
        imp::retrieve(&self.service, id)
    }

    fn store(&self, id: &str, key: &ProtectKey) -> Result<()> {
        imp::store(&self.service, id, key)
    }

    fn delete(&self, id: &str) -> Result<()> {
        imp::delete(&self.service, id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取一个可用的后端，拿不到就说明这台机器上真的不可用。
    ///
    /// # 为什么打印得这么醒目
    ///
    /// 这三个测试在没有后端时会跳过，于是**显示为通过却什么都没验**。
    /// 已经踩过：Windows 凭据管理器存满（cmdkey 也写不进去）时，三个
    /// 测试一起静默跳过，看上去全绿，实际零覆盖。
    ///
    /// 用 `--nocapture` 跑就能看到这行。真要在 CI 上强制验证，
    /// 设 `OMY_REQUIRE_KEYSTORE=1`，那时拿不到后端直接失败而不是跳过。
    fn backend_or_skip(service: &str) -> Option<MachineProtector> {
        match MachineProtector::new(service) {
            Ok(p) => Some(p),
            Err(e) => {
                let msg = format!("!!! 跳过（未验证任何东西）：凭据后端不可用 —— {e}");
                assert!(
                    std::env::var("OMY_REQUIRE_KEYSTORE").is_err(),
                    "{msg}\n设了 OMY_REQUIRE_KEYSTORE 就必须真的验证"
                );
                eprintln!("{msg}");
                None
            }
        }
    }

    /// 真实钥匙串的读写往返。
    ///
    /// 这个测试会真的写进系统凭据库，所以用一个带进程 id 的服务名，
    /// 避免并行测试互相踩，并在结束时清理。
    #[test]
    fn roundtrip_on_real_backend() {
        let service = format!("omy-test-{}", std::process::id());
        let Some(p) = backend_or_skip(&service) else {
            return;
        };

        let id = "place-1";
        let key = crate::random_key();
        p.store(id, &key).expect("写入");
        let got = p.retrieve(id).expect("读回");
        assert_eq!(*got, *key, "读回的密钥必须与写入的一致");

        p.delete(id).expect("删除");
        assert!(
            matches!(p.retrieve(id), Err(Error::NotFound)),
            "删除后必须报 NotFound，而不是返回旧值或别的错误"
        );
    }

    /// 没存过必须是 `NotFound`，不能是别的错误。
    ///
    /// 不这样会怎样：`retrieve_or_create` 靠这个分支判断「首次使用」，
    /// 归错类会让它把可恢复的首次场景当成硬错误抛出去。
    #[test]
    fn missing_key_is_not_found() {
        let service = format!("omy-test-missing-{}", std::process::id());
        let Some(p) = backend_or_skip(&service) else {
            return;
        };
        assert!(matches!(p.retrieve("never-stored"), Err(Error::NotFound)));
    }

    /// 机器绑定档不要求用户当场验证身份。
    ///
    /// 这条断言看着平凡，但它固定了与生物识别档的分界：将来加了那一档，
    /// 界面靠这个标志决定要不要提示「需要指纹」，写反了会让用户在
    /// 无感档上看到莫名其妙的验证提示。
    #[test]
    fn machine_protector_needs_no_presence() {
        let service = format!("omy-test-presence-{}", std::process::id());
        let Some(p) = backend_or_skip(&service) else {
            return;
        };
        assert!(!p.requires_user_presence());
    }

    /// 后端不可用时必须报 `NoBackend`，而不是别的错误或假装成功。
    ///
    /// 这条**不依赖环境**：无论这台机器的凭据库能不能用，
    /// `MachineProtector::new` 的结果都只能是成功、或者一个明确的
    /// `NoBackend`。归错类会让上层把「这台机器不支持」显示成
    /// 「操作失败，请重试」，用户重试一百次也没用。
    #[test]
    fn unavailable_backend_is_classified() {
        let service = format!("omy-test-class-{}", std::process::id());
        match MachineProtector::new(&service) {
            Ok(_) => { /* 可用，这条不适用 */ }
            Err(e) => assert!(
                matches!(e, Error::NoBackend(_)),
                "后端用不了时必须是 NoBackend，实际是 {e:?}"
            ),
        }
    }

    #[test]
    fn key_text_roundtrips_and_accepts_legacy_raw() {
        let key = Zeroizing::new([7u8; KEY_LEN]);
        let text = encode_key_text(&key);
        assert!(text.is_ascii(), "编码结果必须纯 ASCII");
        let back = decode_key_text(text.as_bytes()).expect("解码");
        assert_eq!(back.as_ref(), key.as_ref(), "往返必须逐字节一致");

        // 旧版裸 32 字节条目必须仍能读（已发布版本的用户不能被迫重输密码）
        let legacy = [192u8; KEY_LEN];
        let got = decode_key_text(&legacy).expect("旧格式可读");
        assert_eq!(got.as_ref(), legacy.as_slice());

        // 被提供者损坏（长度不对）或内容不是 hex 的条目 → None
        assert!(decode_key_text(b"not a key").is_none());
        let mut bad = TEXT_KEY_PREFIX.to_string();
        bad.push_str(&"zz".repeat(KEY_LEN));
        assert!(decode_key_text(bad.as_bytes()).is_none());
    }
}
