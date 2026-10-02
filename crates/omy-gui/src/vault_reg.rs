//! 全局「已见过的 vault」登记表。
//!
//! # 为什么需要它
//!
//! KEK = `Argon2id(密码, vault_salt)`，**与 vault_salt 绑定**：同一个密码在
//! 不同 salt 下派生出的 KEK 完全不同，不能跨 vault 互解（实测 `NoMatchingSlot`）。
//! 所以「同一密码解锁虚拟位置后，让散落各处的本地/远程 .omy 也自动解锁」不能
//! 只靠某一把 KEK，必须在输密码时为**每个相关 vault 的 salt** 各派生一把。
//!
//! 这张表登记应用**见过的全部 vault 派生材料**（salt + Argon2 参数），来源：
//! - 本地扫描 / 浏览目录时读到的每个 .omy 文件头；
//! - 远程位置列出的加密文件头（`remote_place_vaults`）；
//! - `vault_params_of` 探测目录；
//! - 既有加密虚拟位置落盘里记录的 vault 材料（升级/迁移用）。
//!
//! 只存 salt 与 KDF 参数——它们**不是秘密**（.omy 文件头里本就明文带着），
//! 不存任何密码或密钥。真正的 KEK 仍只在会话里、用用户当次输入的密码现派生。
//!
//! 同一 salt 去重：一张表里 vault 数量通常很小（用户不会有几百个独立密码库），
//! 输一次密码为每个 vault 跑一次 Argon2 的代价可接受，与 `unlock` 多 vault
//! 既有设计一致。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// 一个 vault 的派生材料（salt + Argon2 参数）。
///
/// 定义在 [`omy_remote::virtuals`]，GUI 与虚拟位置业务共用同一份；这里 re-export
/// 是为了让既有 `crate::vault_reg::VaultMaterial` 路径继续可用。
pub use omy_remote::virtuals::VaultMaterial;

/// 全局 vault 登记表。键为 salt 十六进制（值里也含 salt，键只用于去重）。
pub struct VaultRegistry {
    inner: Mutex<BTreeMap<String, VaultMaterial>>,
    /// true 时不落盘（单测用，避免污染真实数据目录）。
    in_memory: bool,
}

impl Default for VaultRegistry {
    fn default() -> Self {
        Self { inner: Mutex::new(BTreeMap::new()), in_memory: false }
    }
}

impl VaultRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 仅驻内存、永不读写磁盘的登记表（单测用）。
    #[cfg_attr(not(test), allow(dead_code))]
    #[must_use]
    pub fn new_in_memory() -> Self {
        Self { inner: Mutex::new(BTreeMap::new()), in_memory: true }
    }

    /// 落盘路径：数据目录下 `vaults.json`。取不到数据目录或内存模式返回 None。
    fn store_path(&self) -> Option<PathBuf> {
        if self.in_memory {
            return None;
        }
        omy_config::data_dir().map(|d| d.join("vaults.json"))
    }

    /// 从磁盘载入。文件不存在/损坏都不当致命错误（这是缓存性质的登记表，
    /// 扫描时会重新填）；损坏时记 warn 但返回 Ok，避免拖垮启动。
    pub fn load(&self) -> std::io::Result<()> {
        let Some(path) = self.store_path() else { return Ok(()) };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        // 兼容两种历史形态：[VaultMaterial] 或 {salt: {...}}；这里统一按数组读。
        let list: Vec<VaultMaterial> = serde_json::from_str(&text).unwrap_or_default();
        if let Ok(mut m) = self.inner.lock() {
            for v in list {
                m.insert(v.salt_hex(), v);
            }
        }
        Ok(())
    }

    /// 登记一批 vault 材料（按 salt 去重）。有新增才落盘。
    pub fn register_all(&self, vaults: &[VaultMaterial]) {
        let changed = {
            let Ok(mut m) = self.inner.lock() else { return };
            let before = m.len();
            for v in vaults {
                m.entry(v.salt_hex()).or_insert_with(|| v.clone());
            }
            m.len() != before
        };
        if changed {
            let _ = self.save();
        }
    }

    /// 当前全部 vault 材料（顺序稳定，按 salt 排序）。
    #[must_use]
    pub fn list(&self) -> Vec<VaultMaterial> {
        self.inner.lock().map(|m| m.values().cloned().collect()).unwrap_or_default()
    }

    /// 用一个**已证实正确**的密码，为表里**所有** vault 各派生一把 KEK 装入会话。
    ///
    /// 调用方必须先用别的途径确认密码正确（真解开了某个文件 / 某个位置），
    /// 否则会把一个错误密码撒到每个 salt 上——错密码虽解不开任何东西、也会被
    /// 指纹去重，但白跑 N 次 Argon2 且污染凭据列表。
    ///
    /// 每个 salt 都现派生（KEK 与 salt 绑定，跨 salt 不通用）；`add_kek` 按
    /// (salt, 指纹) 去重，同一密码重复调用不会产生多条。派生失败的单个 vault
    /// 跳过，不影响其它。
    pub fn install_everywhere(
        &self,
        session: &mut omy_core::session::SessionKeys,
        password: &[u8],
        label: &str,
    ) {
        use omy_core::crypto::Kek;
        use omy_core::session::CredentialKind;
        for v in self.list() {
            let params = omy_core::crypto::Argon2Params { m_kib: v.m_kib, t: v.t, p: v.p };
            if let Ok(kek) = Kek::from_password(password, &v.salt, params) {
                session.add_kek(label, CredentialKind::Vault, &v.salt, kek);
            }
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = self.store_path() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let list = self.list();
        let text = serde_json::to_string_pretty(&list)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        omy_core::fsatomic::write_atomic(&path, text.as_bytes())
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn v(salt: u8, m: u32) -> VaultMaterial {
        VaultMaterial { salt: [salt; 16], m_kib: m, t: 1, p: 1 }
    }

    #[test]
    fn salt_hex_is_lowercase_32_chars() {
        let m = v(0xAB, 1);
        let h = m.salt_hex();
        assert_eq!(h.len(), 32);
        assert_eq!(h, "ab".repeat(16));
    }

    #[test]
    fn register_all_dedups_by_salt() {
        let reg = VaultRegistry::new_in_memory();
        reg.register_all(&[v(1, 100), v(1, 200), v(2, 100)]);
        let all = reg.list();
        // 同 salt 只保留第一份——去重失败会变成 3 条，重复 Argon2 派生浪费几百毫秒
        assert_eq!(all.len(), 2);
        // 先登记的参数保留：后到的同 salt 不应覆盖
        assert_eq!(all.iter().find(|x| x.salt == [1u8; 16]).unwrap().m_kib, 100);
    }

    #[test]
    fn register_again_is_idempotent_and_sorted() {
        let reg = VaultRegistry::new_in_memory();
        reg.register_all(&[v(3, 1), v(1, 1)]);
        reg.register_all(&[v(2, 1), v(1, 1)]);
        let salts: Vec<[u8; 16]> = reg.list().iter().map(|x| x.salt).collect();
        // BTreeMap 按 salt_hex 排序，顺序稳定（落盘/遍历结果可预测）
        assert_eq!(salts, vec![[1u8; 16], [2u8; 16], [3u8; 16]]);
    }

    #[test]
    fn in_memory_registry_writes_nothing() {
        let reg = VaultRegistry::new_in_memory();
        reg.register_all(&[v(9, 1)]);
        // 不应 panic、不应找到任何落盘路径
        assert!(reg.store_path().is_none());
        assert_eq!(reg.list().len(), 1);
    }

    // 核心回归：一个已证实正确的密码，经全局表为每个 salt 重派生后，能解开
    // 用**不同 salt** 加密的文件。这正是「解锁本地文件后 Telegram/虚拟位置自动
    // 解锁」及其反向的保证。只装单把 saltA 的 KEK 会解不开 saltB（NoMatchingSlot），
    // 所以这里必须真实地分别加密两个 vault 并各解一次。
    #[test]
    fn install_everywhere_unlocks_cross_salt_files() {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file;
        use omy_core::session::SessionKeys;

        let pw = b"132";
        let weak = Argon2Params::TEST_WEAK;
        let salt_a = [7u8; 16];
        let salt_b = [9u8; 16]; // 与 A 不同：模拟「本地文件 vs Telegram 位置」两个库

        let kek_a = Kek::from_password(pw, &salt_a, weak).expect("kdf A");
        let enc_a = file::encrypt(
            b"local file",
            &[kek_a.duplicate()],
            &salt_a,
            &file::EncryptOptions::default(),
            &file::RandomMaterial::generate(),
        )
        .expect("encrypt A");
        let kek_b = Kek::from_password(pw, &salt_b, weak).expect("kdf B");
        let enc_b = file::encrypt(
            b"telegram place secret",
            &[kek_b.duplicate()],
            &salt_b,
            &file::EncryptOptions::default(),
            &file::RandomMaterial::generate(),
        )
        .expect("encrypt B");

        // 用户只在 A（本地）解锁：会话原本只有 saltA 的 KEK
        let reg = VaultRegistry::new_in_memory();
        reg.register_all(&[
            VaultMaterial { salt: salt_a, m_kib: weak.m_kib, t: weak.t, p: weak.p },
            VaultMaterial { salt: salt_b, m_kib: weak.m_kib, t: weak.t, p: weak.p },
        ]);
        let mut sess = SessionKeys::new();
        sess.add_password("main", &salt_a, "132", weak).expect("add pw A");

        // 撒布前：saltB 解不开（没有它的 KEK）
        assert!(file::open(&enc_b.bytes, &sess.all_keks()).is_err());

        // 撒布后：两个 vault 都能解开
        reg.install_everywhere(&mut sess, pw, "main");
        assert!(file::open(&enc_a.bytes, &sess.all_keks()).is_ok(), "saltA 仍可解");
        assert!(file::open(&enc_b.bytes, &sess.all_keks()).is_ok(), "saltB 必须随同密码自动解开");
    }

    // 错误密码撒布后解不开任何东西（指纹去重也不会让它冒充真密码）：
    // 调用方应先验证密码，但即便误用，install_everywhere 也不会制造假解锁。
    #[test]
    fn install_everywhere_with_wrong_password_does_not_unlock() {
        use omy_core::crypto::{Argon2Params, Kek};
        use omy_core::file;
        use omy_core::session::SessionKeys;

        let weak = Argon2Params::TEST_WEAK;
        let salt = [3u8; 16];
        let real = Kek::from_password(b"correct", &salt, weak).expect("kdf");
        let enc = file::encrypt(
            b"secret",
            &[real.duplicate()],
            &salt,
            &file::EncryptOptions::default(),
            &file::RandomMaterial::generate(),
        )
        .expect("encrypt");

        let reg = VaultRegistry::new_in_memory();
        reg.register_all(&[VaultMaterial { salt, m_kib: weak.m_kib, t: weak.t, p: weak.p }]);
        let mut sess = SessionKeys::new();
        reg.install_everywhere(&mut sess, b"wrong", "main");
        assert!(file::open(&enc.bytes, &sess.all_keks()).is_err());
    }
}
