//! 远程位置的「独立密码槽」——给每个远程位置一份自己的密钥，接进 omy 现有
//! 多密码（KEK）体系，实现「访问前需解锁、但已解锁过的密码能直接开」。
//!
//! # 为什么要这一层（方案二）
//!
//! Telegram 远程位置的 session 里含 auth key，等同账号凭据，必须加密落盘。
//! 早先它用系统凭据库里的一把随机机器密钥加密（见 `session.rs` 的
//! `protect_key`），那是「随机磁盘保护」——重装/换机即失效，且**与用户的
//! omy 密码无关**：用户输不输 omy 密码，session 都能被本机自动解出。
//!
//! 用户要的是：**这个位置也要密码保护，而且已经输过的 omy 密码能直接开它，
//! 不用再输一遍。** 这正是 omy 文件的多 slot 模型：
//!
//! - 每个位置有自己的一小片 **slot 区**（`SLOT_CAP` 个槽），包裹一把随机的
//!   **位置数据密钥 PDK**；
//! - 每个槽 = 用某把 KEK 经 `Kek::derive_slot_key(place_uuid, idx)` 派生的
//!   包裹密钥，对 PDK 做 AEAD；
//! - 解锁时拿会话里**已解锁的每一把 KEK** 逐槽去试，任一槽能解开 PDK 即解锁。
//!
//! 「独立 slot」与「复用已解锁态」不矛盾：**槽是每位置独立的（HKDF salt 是
//! 位置自己的 uuid），但同一把密码派生的 KEK 可以是多个位置各自的一个槽**。
//! 于是「解锁了某个库/输过某个密码」就能顺带打开所有把它设为槽的位置——
//! 这跟 `.omy` 文件「一个密码开多份文件」是同一个机制，只是把文件换成位置。
//!
//! # PDK 与 session 的关系
//!
//! session 明文用 `omy_secret::seal(PDK 当作 32 字节 ProtectKey)` 加密。所以：
//! **解不出 PDK ⇒ 解不出 session**。PDK 本身只存在于内存，落盘的只有「被各槽
//! 包裹的 PDK」和「用 PDK 加密的 session 信封」，两者都不含明文 PDK。
//!
//! # 与 `keyslot.rs`（.omy 文件的 slot）的关系
//!
//! 复用的是**同一套派生**（`Kek::derive_slot_key`、`CipherId`、`ZERO_NONCE`），
//! 但**不复用 .omy 的 384 字节头部格式**：位置的槽区是独立 JSON 结构，因为它
//! 不背负 .omy 文件那些约束（可否认性的定长头、TLV、载荷 MAC）。共享的只是
//! 「用 KEK 包裹一把随机密钥、任一 KEK 能开即解锁」这条逻辑。新增/修改包裹
//! 方式时，这里与 `slot.rs`/`keyslot.rs` 都要一起看。
//!
//! # 明文 / 密文边界
//!
//! - **加密**：PDK（被各槽包裹）、session 明文（被 PDK 加密）。
//! - **明文**：位置 uuid、各槽用了哪种凭据类型（label/kind，不含秘密）、
//!   userid、显示名。userid 与名称明文是**有意**的：去重靠 userid、侧栏要显示
//!   名称、锁定态下也要能判「这个账号已添加」。它们是公开信息（数字 id、
//!   用户自己看得到的名字），不构成新的泄露面。

use omy_core::crypto::{Argon2Params, CipherId, Kek, SecretKey, ZERO_NONCE, KEY_LEN};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// 一个位置最多几个密码槽。
///
/// 与 .omy 文件的 8 槽同量级：够放「主密码 + 恢复用途 + 设备密钥 + 几个额外
/// 密码」，又不至于让槽区无谓变大。
pub const SLOT_CAP: usize = 8;

/// PDK（位置数据密钥）字节数。与 omy 的对称密钥等长。
const PDK_LEN: usize = KEY_LEN;

/// 位置密钥槽的持久化结构。
///
/// `Debug` 只暴露 uuid 与各槽的 kind/label（均非秘密）；`wrapped` 是**密文**
/// （被包裹的 PDK），不是明文密钥，打印它不泄露 PDK。
#[derive(Debug, Serialize, Deserialize)]
pub struct PlaceSlots {
    /// 本位置的 uuid，作为 slot 密钥派生的 HKDF salt。
    ///
    /// 每位置独立 ⇒ 同一把 KEK 在不同位置派生出的包裹密钥不同，槽不会跨位置
    /// 误开。用 uuid 而非位置 id 字符串：id 可能被改名逻辑复用，uuid 生成后
    /// 不变。
    pub uuid: [u8; 16],
    /// 加密算法（与包裹时一致）。
    pub cipher_id: u8,
    /// 各密码槽。顺序即 slot_index，**下标是包裹密钥的一部分，不能重排**。
    pub slots: Vec<Slot>,
    /// 用户**现场输入的密码**派生 KEK 所需的 KDF 材料（salt + Argon2 参数）。
    ///
    /// 这是「远程位置加密当成普通 omy 文件」的关键：文件加密时密码 KEK 由
    /// `Kek::from_password(密码, vault_salt, 参数)` 派生，salt/参数存在文件头里
    /// 供解锁重派生。位置加密同理——把它存在这里，`unlock_with_password` 才能
    /// 用同一 salt/参数把用户输的密码重新派生成能开某个槽的 KEK。
    ///
    /// `None` 表示这个位置只用「会话已解锁的 KEK」建的槽（旧的、S1 那版），没有
    /// 独立密码槽。`#[serde(default)]` 保证既有 fmt2 文件反序列化不报错。
    #[serde(default)]
    pub pw_kdf: Option<PwKdf>,
}

/// 现场密码槽的 KDF 材料。明文存（salt/参数不是秘密，密码本身从不落盘）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PwKdf {
    /// Argon2 的 salt（每位置随机 16 字节），与 `.omy` 的 vault_salt 同角色。
    pub salt: [u8; 16],
    /// Argon2 内存成本（KiB）。
    pub m_kib: u32,
    /// Argon2 迭代次数。
    pub t: u32,
    /// Argon2 并行度。
    pub p: u32,
}

impl PwKdf {
    /// 还原成 [`Argon2Params`]。
    #[must_use]
    pub fn params(&self) -> Argon2Params {
        Argon2Params { m_kib: self.m_kib, t: self.t, p: self.p }
    }
    /// 用这份 KDF 材料把明文密码派生成 KEK。
    ///
    /// # Errors
    ///
    /// Argon2 参数非法时返回 [`PlaceKeyError::Wrap`]（归到「派生失败」）。
    pub fn derive(&self, password: &[u8]) -> Result<Kek, PlaceKeyError> {
        Kek::from_password(password, &self.salt, self.params()).map_err(|_| PlaceKeyError::Wrap)
    }
}

/// 单个密码槽。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Slot {
    /// 这个槽用哪种凭据（`vault`/`device`/`recovery`/`portable`/`machine`）。
    ///
    /// 明文，只用于 UI 显示与调试，不含秘密。`machine` 是无库回退时用系统
    /// 凭据库那把机器密钥占的槽。
    pub kind: String,
    /// 显示名（用户给的名字或默认），明文，不含秘密。
    pub label: String,
    /// 被这把 KEK 的包裹密钥 AEAD 加密后的 PDK（`密文 || tag`）。
    ///
    /// 用 `derive_slot_key(KEK, place_uuid, slot_index)` 作密钥、`ZERO_NONCE`
    /// 作 nonce——安全性依据同 .omy：包裹密钥由 `(KEK, uuid, idx)` 唯一确定，
    /// 同一密钥不复用，零 nonce 不会导致 nonce 复用。
    pub wrapped: Vec<u8>,
}

/// 位置密钥体系可能出的问题。
#[derive(Debug, thiserror::Error)]
pub enum PlaceKeyError {
    /// 没有任何已解锁的 KEK 能开这个位置的槽——即「未解锁」，是正常态，不是损坏。
    #[error("位置未解锁：没有已输入的密码能打开它")]
    Locked,
    /// 一个槽都没有——等于永远打不开，拒绝生成（对应 .omy 的「keep 为空」）。
    #[error("拒绝生成没有任何密码槽的位置，它将永远无法打开")]
    NoSlots,
    /// 槽数超过上限。
    #[error("密码槽数量超过上限 {max}")]
    TooManySlots {
        /// 上限。
        max: usize,
    },
    /// 加密算法编号不认识。
    #[error("未知的加密算法编号")]
    BadCipher,
    /// AEAD 包裹/解包失败（正常输入下不会发生）。
    #[error("密钥包裹失败")]
    Wrap,
}

/// 一把待写入的槽：KEK + 它的类型与显示名。
///
/// 不派生 `Clone`：`Kek` 刻意没有 `Clone`（见 `crypto.rs`），按引用用即可。
pub struct SlotKey<'a> {
    /// 包裹用的 KEK（来自会话里已解锁的某个凭据，或机器密钥）。
    pub kek: &'a Kek,
    /// 凭据类型串，进 `Slot::kind`。
    pub kind: &'a str,
    /// 显示名，进 `Slot::label`。
    pub label: &'a str,
}

impl PlaceSlots {
    /// 新建一个位置的槽区：随机生成 PDK，用 `keys` 里的每把 KEK 各包一个槽。
    ///
    /// 返回 `(槽区, PDK)`。PDK 是 `Zeroizing`，调用方拿它去 `seal` session
    /// 后即可丢弃；它**不落盘**。
    ///
    /// # Errors
    ///
    /// `keys` 为空 → [`PlaceKeyError::NoSlots`]（没有槽的位置永远打不开）；
    /// 超过 [`SLOT_CAP`] → [`PlaceKeyError::TooManySlots`]；AEAD 失败 →
    /// [`PlaceKeyError::Wrap`]。
    pub fn create(
        keys: &[SlotKey<'_>],
        cipher: CipherId,
    ) -> Result<(Self, Zeroizing<[u8; PDK_LEN]>), PlaceKeyError> {
        if keys.is_empty() {
            return Err(PlaceKeyError::NoSlots);
        }
        if keys.len() > SLOT_CAP {
            return Err(PlaceKeyError::TooManySlots { max: SLOT_CAP });
        }
        let mut uuid = [0u8; 16];
        omy_core::util::fill_random(&mut uuid);
        let mut pdk = Zeroizing::new([0u8; PDK_LEN]);
        omy_core::util::fill_random(pdk.as_mut());

        let mut slots = Vec::with_capacity(keys.len());
        for (i, k) in keys.iter().enumerate() {
            let idx = u16::try_from(i).map_err(|_| PlaceKeyError::TooManySlots { max: SLOT_CAP })?;
            let wrapped = wrap_pdk(k.kek, &uuid, idx, cipher, &pdk)?;
            slots.push(Slot {
                kind: k.kind.to_owned(),
                label: k.label.to_owned(),
                wrapped,
            });
        }
        Ok((
            Self {
                uuid,
                cipher_id: cipher as u8,
                slots,
                pw_kdf: None,
            },
            pdk,
        ))
    }

    /// 用**现场输入的密码**建一个位置的槽区（远程位置加密当成普通 omy 文件）。
    ///
    /// 与 [`Self::create`] 的区别：这里的槽[0]是「密码槽」——KEK 由
    /// `Kek::from_password(password, 随机salt, params)` 派生，salt/参数存进
    /// `pw_kdf` 供解锁重派生。`extra` 是**当前会话已解锁的 KEK**，各建一个额外
    /// 槽，好让「已经输过的 omy 密码能直接开这个位置」——这不是特殊逻辑，就是
    /// 多给几个槽。**机器密钥绝不能进 `extra`**（调用方保证）：它本机自动可得，
    /// 放进来就等于没加密。
    ///
    /// # Errors
    ///
    /// 密码为空 → [`PlaceKeyError::NoSlots`]（没有真正保护的位置不该建）；
    /// 派生/包裹失败 → [`PlaceKeyError::Wrap`]；槽超限 → [`PlaceKeyError::TooManySlots`]。
    pub fn create_with_password(
        password: &[u8],
        params: Argon2Params,
        extra: &[SlotKey<'_>],
        cipher: CipherId,
    ) -> Result<(Self, Zeroizing<[u8; PDK_LEN]>), PlaceKeyError> {
        if password.is_empty() {
            // 空密码 = 没有真正的保护，拒绝——与 .omy 不接受空密码同理
            return Err(PlaceKeyError::NoSlots);
        }
        if extra.len() + 1 > SLOT_CAP {
            return Err(PlaceKeyError::TooManySlots { max: SLOT_CAP });
        }
        let mut salt = [0u8; 16];
        omy_core::util::fill_random(&mut salt);
        let pw_kdf = PwKdf { salt, m_kib: params.m_kib, t: params.t, p: params.p };
        let pw_kek = pw_kdf.derive(password)?;

        // 槽[0] 是密码槽，其后是会话已解锁的 KEK 各一个槽。
        let pw_slot = SlotKey { kek: &pw_kek, kind: "password", label: "密码" };
        let mut keys: Vec<SlotKey<'_>> = Vec::with_capacity(extra.len() + 1);
        keys.push(pw_slot);
        for e in extra {
            keys.push(SlotKey { kek: e.kek, kind: e.kind, label: e.label });
        }
        let (mut slots, pdk) = Self::create(&keys, cipher)?;
        slots.pw_kdf = Some(pw_kdf);
        Ok((slots, pdk))
    }

    /// 用**现场输入的密码**（+ 会话里已解锁的 KEK）尝试解开 PDK。
    ///
    /// 先用 `pw_kdf` 把密码派生成 KEK 去试各槽；再用传入的 `keks`（会话已解锁的）
    /// 去试——这就是「输过的 omy 密码直接开」。没有 `pw_kdf`（旧的纯 KEK 槽）时
    /// 只试 `keks`。
    ///
    /// # Errors
    ///
    /// 都开不了 → [`PlaceKeyError::Locked`]。
    pub fn unlock_with_password(
        &self,
        password: &[u8],
        keks: &[Kek],
    ) -> Result<Zeroizing<[u8; PDK_LEN]>, PlaceKeyError> {
        // 先试现场密码（若这个位置有密码槽）
        if let Some(kdf) = &self.pw_kdf
            && !password.is_empty()
                && let Ok(pw_kek) = kdf.derive(password)
                    && let Ok(pdk) = self.unlock(&[pw_kek]) {
                        return Ok(pdk);
                    }
        // 再试会话已解锁的 KEK（复用已解锁态）
        self.unlock(keks)
    }

    /// 尝试用会话里已解锁的一批 KEK 解开 PDK。
    ///
    /// 逐 KEK × 逐槽去试：只要某把 KEK 能解开某个槽，就返回 PDK。这就是
    /// 「输过的密码能直接开这个位置」——那把密码的 KEK 已经在会话里，命中它
    /// 自己包的那个槽即可。
    ///
    /// # 为什么全试而不是记住哪把开哪槽
    ///
    /// 记住映射要落盘一个「KEK→槽」的对应，等于泄露「这个位置和那个库是同一
    /// 个密码」。逐个试的代价只是几次 HKDF + AEAD（微秒级），换来的是槽与
    /// 密码的关系不落盘。这与扫描 .omy 文件「拿每把 KEK 试每个 slot」同理。
    ///
    /// # Errors
    ///
    /// 没有任何 KEK 能开 → [`PlaceKeyError::Locked`]（未解锁，正常态）。
    pub fn unlock(&self, keks: &[Kek]) -> Result<Zeroizing<[u8; PDK_LEN]>, PlaceKeyError> {
        let cipher = CipherId::from_u8(self.cipher_id).map_err(|_| PlaceKeyError::BadCipher)?;
        for (i, slot) in self.slots.iter().enumerate() {
            let idx = match u16::try_from(i) {
                Ok(v) => v,
                Err(_) => continue,
            };
            for kek in keks {
                if let Some(pdk) = unwrap_pdk(kek, &self.uuid, idx, cipher, &slot.wrapped) {
                    return Ok(pdk);
                }
            }
        }
        Err(PlaceKeyError::Locked)
    }

    /// 这批 KEK 里有没有任一把能解开本位置（用于「是否已解锁」判断，不取 PDK）。
    #[must_use]
    pub fn can_unlock(&self, keks: &[Kek]) -> bool {
        self.unlock(keks).is_ok()
    }
}

/// 用一把 KEK 的槽密钥包裹 PDK。
fn wrap_pdk(
    kek: &Kek,
    uuid: &[u8; 16],
    idx: u16,
    cipher: CipherId,
    pdk: &[u8; PDK_LEN],
) -> Result<Vec<u8>, PlaceKeyError> {
    let wrap_key = kek.derive_slot_key(uuid, idx);
    cipher
        .encrypt(&wrap_key, &ZERO_NONCE, pdk, &[])
        .map_err(|_| PlaceKeyError::Wrap)
}

/// 用一把 KEK 的槽密钥尝试解开 PDK；不匹配返回 `None`（不是错误，是密码不对）。
fn unwrap_pdk(
    kek: &Kek,
    uuid: &[u8; 16],
    idx: u16,
    cipher: CipherId,
    wrapped: &[u8],
) -> Option<Zeroizing<[u8; PDK_LEN]>> {
    let wrap_key = kek.derive_slot_key(uuid, idx);
    let plain = cipher.decrypt(&wrap_key, &ZERO_NONCE, wrapped, &[]).ok()??;
    if plain.len() != PDK_LEN {
        return None;
    }
    let mut pdk = Zeroizing::new([0u8; PDK_LEN]);
    pdk.copy_from_slice(&plain);
    Some(pdk)
}

/// 把 PDK 当作 `omy_secret::ProtectKey`（32 字节）用来 seal/unseal session。
///
/// `ProtectKey` 就是 `Zeroizing<[u8; 32]>`，PDK 正好是 32 字节，直接借过去。
#[must_use]
pub fn pdk_as_protect_key(pdk: &Zeroizing<[u8; PDK_LEN]>) -> omy_secret::ProtectKey {
    Zeroizing::new(**pdk)
}

/// 由一把机器密钥（系统凭据库那把 32 字节 ProtectKey）构造一个 KEK，
/// 用于「用户没有任何 omy 库/密码时」的回退槽。
///
/// 这样即使用户从没设过 omy 密码，位置仍是加密的（机器绑定），只是解锁不需要
/// 输密码——等同于旧行为，但统一走了 slot 模型，将来用户设了库密码可以再
/// `add` 一个密码槽升级保护。
#[must_use]
pub fn kek_from_machine_key(key: &omy_secret::ProtectKey) -> Kek {
    Kek::from_key(SecretKey::from_bytes(**key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omy_core::crypto::SecretKey;

    /// 造一把测试 KEK，避开 Argon2 开销（本模块逻辑与 KEK 怎么来的无关）。
    fn kek(seed: u8) -> Kek {
        Kek::from_key(SecretKey::from_bytes(core::array::from_fn(|i| {
            (i as u8).wrapping_mul(31) ^ seed
        })))
    }

    fn slotkey<'a>(kek: &'a Kek, label: &'a str) -> SlotKey<'a> {
        SlotKey { kek, kind: "vault", label }
    }

    /// 创建后，包这个位置的那把 KEK 必须能解开 PDK；解出的 PDK 前后一致。
    ///
    /// 不这样会怎样：PDK 解不回来 ⇒ session 永远 unseal 不了 ⇒ 用户明明输了
    /// 对密码却打不开位置。
    #[test]
    fn created_slot_unlocks_with_its_key() {
        let a = kek(0x11);
        let (slots, pdk) = PlaceSlots::create(&[slotkey(&a, "主密码")], CipherId::ChaCha20Poly1305)
            .expect("建槽");
        let back = slots.unlock(&[kek(0x11)]).expect("同一把 KEK 必须能解开");
        assert_eq!(&pdk[..], &back[..], "解出的 PDK 必须与创建时一致");
    }

    /// 一个密码开多个位置：同一把 KEK 分别建两个位置，都要能被它解开，
    /// 且两个位置的 PDK **不同**（各自随机）。
    ///
    /// 不这样会怎样：这正是「输过的密码直接开其它位置」赖以成立的性质；
    /// 而 PDK 若相同，一个位置泄露会连累另一个。
    #[test]
    fn one_password_opens_multiple_places_with_distinct_pdks() {
        let pw = kek(0x22);
        let (s1, pdk1) = PlaceSlots::create(&[slotkey(&pw, "p")], CipherId::ChaCha20Poly1305).unwrap();
        let (s2, pdk2) = PlaceSlots::create(&[slotkey(&pw, "p")], CipherId::ChaCha20Poly1305).unwrap();
        assert!(s1.can_unlock(&[kek(0x22)]), "同一密码要能开位置1");
        assert!(s2.can_unlock(&[kek(0x22)]), "同一密码要能开位置2");
        assert_ne!(&pdk1[..], &pdk2[..], "两个位置的 PDK 必须各自独立");
        // uuid 也必须不同，否则 slot 密钥会跨位置相同
        assert_ne!(s1.uuid, s2.uuid, "两个位置的 uuid 必须不同");
    }

    /// 未解锁：没有任一 KEK 能开时返回 Locked（正常态），不是别的错误。
    ///
    /// 不这样会怎样：把「未解锁」和「损坏」混为一谈，界面就无法只提示
    /// 「请先解锁」而不吓唬用户说数据坏了。
    #[test]
    fn wrong_keys_yield_locked_not_corrupt() {
        let a = kek(0x33);
        let (slots, _) = PlaceSlots::create(&[slotkey(&a, "主")], CipherId::ChaCha20Poly1305).unwrap();
        let err = slots.unlock(&[kek(0x99), kek(0xAA)]).unwrap_err();
        assert!(matches!(err, PlaceKeyError::Locked), "应为未解锁，实际 {err:?}");
    }

    /// 多槽：任意一把参与创建的 KEK 都能解开；没参与的开不了。
    #[test]
    fn any_enrolled_key_unlocks_multi_slot() {
        let main = kek(0x41);
        let dev = kek(0x42);
        let (slots, pdk) = PlaceSlots::create(
            &[
                SlotKey { kek: &main, kind: "vault", label: "主密码" },
                SlotKey { kek: &dev, kind: "device", label: "设备密钥" },
            ],
            CipherId::ChaCha20Poly1305,
        )
        .unwrap();
        // 只给设备密钥也要能开
        let back = slots.unlock(&[kek(0x42)]).expect("设备密钥必须能开");
        assert_eq!(&pdk[..], &back[..]);
        // 只给主密码也要能开
        assert!(slots.unlock(&[kek(0x41)]).is_ok(), "主密码必须能开");
        // 都不给则 Locked
        assert!(matches!(slots.unlock(&[kek(0x55)]).unwrap_err(), PlaceKeyError::Locked));
    }

    /// session 用 PDK seal 后，只有解出同一个 PDK 才能 unseal。
    ///
    /// 这条把「解不出 PDK ⇒ 解不出 session」这条关键性质钉死：它是本方案
    /// 「未解锁就读不到 auth key」的实质保证。
    #[test]
    fn session_sealed_with_pdk_needs_the_pdk() {
        let a = kek(0x61);
        let (slots, pdk) = PlaceSlots::create(&[slotkey(&a, "主")], CipherId::ChaCha20Poly1305).unwrap();
        let secret = b"pretend this is an auth key";
        let env = omy_secret::seal(&pdk_as_protect_key(&pdk), secret).expect("seal");

        // 正确 KEK → 解出 PDK → unseal 成功
        let opened = slots.unlock(&[kek(0x61)]).expect("解锁");
        let plain = omy_secret::unseal(&pdk_as_protect_key(&opened), &env).expect("unseal");
        assert_eq!(&plain[..], secret);

        // 错误 KEK → Locked → 拿不到 PDK → 无从 unseal
        assert!(slots.unlock(&[kek(0x62)]).is_err(), "错误密码必须解不出 session");
    }

    /// 空槽被拒：没有任何槽的位置永远打不开，等于销毁。
    #[test]
    fn empty_slots_rejected() {
        let err = PlaceSlots::create(&[], CipherId::ChaCha20Poly1305).unwrap_err();
        assert!(matches!(err, PlaceKeyError::NoSlots));
    }

    /// 超过上限被拒，而不是静默丢弃多余的。
    #[test]
    fn too_many_slots_rejected() {
        let keks: Vec<Kek> = (0..(SLOT_CAP + 1) as u8).map(kek).collect();
        let keys: Vec<SlotKey<'_>> = keks.iter().map(|k| slotkey(k, "x")).collect();
        let err = PlaceSlots::create(&keys, CipherId::ChaCha20Poly1305).unwrap_err();
        assert!(matches!(err, PlaceKeyError::TooManySlots { max: 8 }), "实际 {err:?}");
    }

    /// 落盘的槽区里**不能**出现 PDK 的明文字节。
    ///
    /// 不这样会怎样：PDK 明文进了 JSON，等于 session 的加密白做——谁拿到槽区
    /// 文件就能解出 auth key。这条是本模块的核心安全断言。
    #[test]
    fn serialized_slots_never_contain_plaintext_pdk() {
        let a = kek(0x71);
        let (slots, pdk) = PlaceSlots::create(&[slotkey(&a, "主")], CipherId::ChaCha20Poly1305).unwrap();
        let json = serde_json::to_vec(&slots).expect("序列化");
        assert!(
            !json.windows(PDK_LEN).any(|w| w == &pdk[..]),
            "槽区落盘不得出现 PDK 明文"
        );
    }

    /// 现场密码往返：用 typed 密码建槽，同一密码能解出、且能 unseal session。
    ///
    /// 不这样会怎样：这是「远程位置加密当普通文件」的核心——用户右键输的密码
    /// 必须能在下次解锁时重新派生出开槽的 KEK。派生 salt/参数存不对，用户输对
    /// 密码也打不开自己的位置。用弱 Argon2 参数避免测试太慢。
    #[test]
    fn typed_password_roundtrips() {
        let weak = Argon2Params::TEST_WEAK;
        let (slots, pdk) = PlaceSlots::create_with_password(b"hunter2", weak, &[], CipherId::ChaCha20Poly1305)
            .expect("建密码槽");
        assert!(slots.pw_kdf.is_some(), "密码槽必须落 KDF 材料供解锁重派生");
        // 正确密码：能解出同一个 PDK
        let back = slots.unlock_with_password(b"hunter2", &[]).expect("对密码必须能开");
        assert_eq!(&pdk[..], &back[..], "解出的 PDK 要与建槽时一致");
        // session 用 PDK 封，正确密码能 unseal
        let env = omy_secret::seal(&pdk_as_protect_key(&pdk), b"auth key").expect("seal");
        let opened = slots.unlock_with_password(b"hunter2", &[]).expect("解锁");
        let plain = omy_secret::unseal(&pdk_as_protect_key(&opened), &env).expect("unseal");
        assert_eq!(&plain[..], b"auth key");
    }

    /// 错误密码 → Locked（不是别的错误），空密码也开不了。
    #[test]
    fn wrong_password_locked() {
        let weak = Argon2Params::TEST_WEAK;
        let (slots, _) = PlaceSlots::create_with_password(b"correct", weak, &[], CipherId::ChaCha20Poly1305).unwrap();
        assert!(matches!(slots.unlock_with_password(b"wrong", &[]).unwrap_err(), PlaceKeyError::Locked));
        assert!(matches!(slots.unlock_with_password(b"", &[]).unwrap_err(), PlaceKeyError::Locked));
    }

    /// 空密码建槽被拒（没有真正保护的位置不该建）。
    #[test]
    fn empty_password_rejected() {
        let err = PlaceSlots::create_with_password(b"", Argon2Params::TEST_WEAK, &[], CipherId::ChaCha20Poly1305).unwrap_err();
        assert!(matches!(err, PlaceKeyError::NoSlots));
    }

    /// 「已解锁的 KEK 直接开」：密码槽 + 一个会话 KEK 额外槽，两者都能各自开。
    ///
    /// 不这样会怎样：这是「输过的 omy 密码不用再输」赖以成立的性质——
    /// 加密时把会话 KEK 也建了槽，之后那把 KEK 在会话里就能直接开。
    #[test]
    fn session_kek_also_opens_password_place() {
        let weak = Argon2Params::TEST_WEAK;
        let session = kek(0x51);
        let extra = [SlotKey { kek: &session, kind: "vault", label: "库密码" }];
        let (slots, pdk) = PlaceSlots::create_with_password(b"pw", weak, &extra, CipherId::ChaCha20Poly1305).unwrap();
        // 只给密码：能开
        assert_eq!(&slots.unlock_with_password(b"pw", &[]).unwrap()[..], &pdk[..]);
        // 只给会话 KEK（没给密码）：也能开
        assert_eq!(&slots.unlock_with_password(b"", &[kek(0x51)]).unwrap()[..], &pdk[..]);
        // 都不对：Locked
        assert!(matches!(slots.unlock_with_password(b"nope", &[kek(0x99)]).unwrap_err(), PlaceKeyError::Locked));
    }

    /// 机器密钥回退：由机器 ProtectKey 造的 KEK 能建槽也能开。
    #[test]
    fn machine_key_fallback_roundtrips() {
        let mk: omy_secret::ProtectKey = Zeroizing::new(core::array::from_fn(|i| (i as u8) ^ 0x3C));
        let k = kek_from_machine_key(&mk);
        let (slots, _) = PlaceSlots::create(
            &[SlotKey { kek: &k, kind: "machine", label: "本机" }],
            CipherId::ChaCha20Poly1305,
        )
        .unwrap();
        // 用同一把机器密钥重建的 KEK 必须能开
        assert!(slots.can_unlock(&[kek_from_machine_key(&mk)]), "机器密钥槽必须能开");
    }
}
