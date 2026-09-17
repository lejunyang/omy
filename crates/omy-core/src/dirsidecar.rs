//! 目录名的钥匙包裹边车（`.omy-name`）。
//!
//! # 为什么目录名要两层结构
//!
//! 原先目录名直接用 `HKDF(KEK, info="omy/v1/dirname")` 派生的密钥加密。
//! 这个做法有一个致命的结构性限制：**目录名只能被一把钥匙解开**。
//!
//! 实测过它的后果（`probe-tree-multikey`）：一棵树用两个密码加密，两个
//! 密码都能打开每一个密文文件，但只有第一个能解开目录名，第二个解密整棵
//! 树时报 `content hash mismatch`——用户看到的是「文件损坏」。恢复码撞的
//! 是同一堵墙。
//!
//! 所以改成与文件一致的两层结构：
//!
//! ```text
//! 目录名密文 = AEAD(DK, 目录名)          DK 是随机的目录密钥
//! 边车       = 每把 KEK 各包一份 DK
//! ```
//!
//! 这样加一把钥匙只是多一份包裹，与目录名本身的长度无关。
//!
//! # 为什么包裹必须放边车文件，不能塞进目录名
//!
//! 算过了（`probe-dirname-length`），这不是取舍而是唯一可行解。
//! 包裹塞进目录名时，以 32 字节的明文目录名为例：
//!
//! | 钥匙数 | 磁盘名长度 | 上限 255 |
//! |---|---|---|
//! | 1 | 196 | 勉强 |
//! | 2 | 292 | 超 |
//! | 4 | 484 | 超 |
//! | 8 | 868 | 超 |
//!
//! 两把钥匙就破了 255（NTFS / ext4 / APFS 的共同下限）。更糟的是 8 把
//! 钥匙时留给明文目录名的额度是 **0 字节**——连一个字都放不下。
//!
//! 包裹放边车则与钥匙数量完全无关：同样 32 字节的明文名恒定 100 字符，
//! 明文目录名要超过 127 字节（约 42 个汉字）才触发截断。
//!
//! # 边车固定 8 槽、空槽填随机
//!
//! 与 [`crate::slot`] 同样的两条可否认性规则，理由也一样：边车长度固定，
//! 看不出这个目录配了几把钥匙；空槽填随机而非填零，否则「哪些槽在用」
//! 直接可见。
//!
//! 这里还多一层收益：边车现在**恒定存在**。原先它只在目录名超长时才出现，
//! 那反而泄露了「这个目录的名字很长」。
//!
//! # 丢了边车会怎样
//!
//! 目录名再也解不开——这是必须在 UI 里说清楚的后果。它和目录是一体的，
//! 不是缓存、不是可再生的索引。放在被加密的那个目录**内部**而不是父目录，
//! 正是为了让它在目录被移动或复制时跟着走。

use crate::crypto::{CipherId, Kek, NONCE_LEN, SecretKey, ZERO_NONCE};
use crate::error::{Error, Result};
use crate::util::Writer;

/// 钥匙包裹边车的文件名。
///
/// 放在被加密的那个目录**内部**，而不是父目录里：这样目录被整体移动或
/// 复制时它跟着走。放父目录的话，移动一个目录会让它的名字变成不可恢复。
///
/// # 与 `.omy-name` 的分工
///
/// 两个边车存的东西完全不同，不能混：
///
/// | 文件 | 存什么 | 何时存在 |
/// |---|---|---|
/// | `.omy-keys` | DK 的 8 份包裹 | **总是** |
/// | `.omy-name` | 目录名完整密文 | 仅当磁盘名被截断 |
///
/// 前者是打开目录名的钥匙，后者是目录名本身。丢掉任何一个，这个目录的
/// 名字都再也解不开。
pub const KEYS_SIDECAR: &str = ".omy-keys";

/// 边车魔数，用于快速排除「这不是我们的文件」。
pub const SIDECAR_MAGIC: &[u8; 8] = b"OMYDNAME";

/// 边车格式版本。
pub const SIDECAR_VERSION: u8 = 1;

/// 目录密钥长度。
pub const DK_LEN: usize = 32;

/// 包裹后的长度：32 字节 DK + 16 字节 AEAD tag。
pub const WRAP_LEN: usize = 48;

/// 固定槽位数，与文件的 Key Slot Area 保持一致。
///
/// 不取更大值：一棵树能用的钥匙数与单个文件能用的应当相同，两个数字
/// 不一致会让「我给文件加了第 9 个密码，为什么树不行」变成一个需要
/// 解释的问题。
pub const SIDECAR_SLOTS: usize = 8;

/// 槽位区总长。
pub const SIDECAR_SLOT_AREA: usize = SIDECAR_SLOTS * WRAP_LEN;

/// 边车文件总长：magic(8) + version(1) + flags(1) + reserved(2) + nonce(24)
/// + 槽位区(384)。
///
/// reserved 那几个字节是为了让 nonce 从 12 字节偏移开始——对齐不是强需求，
/// 但让十六进制 dump 读起来容易得多，排查问题时值这几个字节。
pub const SIDECAR_LEN: usize = 8 + 1 + 1 + 2 + NONCE_LEN + SIDECAR_SLOT_AREA;

/// 可管理模式下的边车总长：在基础长度之后追加加密的槽位目录。
///
/// 目录 24 字节 + AEAD tag 16 字节 = 40。与文件那边同理，定长是刻意的：
/// 变长会让边车长度泄露这棵树配了几把钥匙。
pub const SIDECAR_LEN_MANAGED: usize = SIDECAR_LEN + crate::slotdir::DIRECTORY_LEN + 16;

/// 边车 flags 位：这棵树带槽位目录（可管理模式）。
///
/// 放在明文的第 9 字节。与文件头的 `flags::SLOT_DIRECTORY` 同一个判据：
/// 「omy 支持两种模式」是产品事实，如实公开它不泄露属于这个用户的东西；
/// 目录**内容**才是用户秘密，所以那部分用 DK 加密。
pub const SIDECAR_FLAG_MANAGED: u8 = 1 << 0;

/// HKDF info：把 DK 派生成槽位目录的加密密钥。
///
/// 不直接拿 DK 当密钥用：DK 已经在给目录名做 AEAD，同一个密钥用于两处
/// 不同用途时，一处的 nonce 复用会波及另一处。
const INFO_SIDECAR_DIR: &[u8] = b"omy/v1/sidecar-slotdir";

/// HKDF info：把 KEK 派生成边车的包裹密钥。
///
/// 与目录名密钥的 info 不同域：同一个 KEK 派生出的两个密钥不能相等，
/// 否则一处的密文可以拿到另一处去试。
const INFO_SIDECAR_WRAP: &[u8] = b"omy/v1/dirname-wrap";

/// 随机生成的目录密钥。
///
/// 独立类型而不是裸 [`SecretKey`]：类型系统会阻止把它和 KEK、FEK 混用。
/// 三者都是 32 字节，混用不会编译报错，但会产出解不开的数据。
#[derive(Debug)]
pub struct DirKey(SecretKey);

impl DirKey {
    /// 随机生成一个新的目录密钥。
    #[must_use]
    pub fn generate() -> Self {
        let mut raw = [0u8; DK_LEN];
        crate::util::fill_random(&mut raw);
        Self(SecretKey::from_bytes(raw))
    }

    /// 从原始字节还原（解包时用）。
    #[must_use]
    pub const fn from_bytes(raw: [u8; DK_LEN]) -> Self {
        Self(SecretKey::from_bytes(raw))
    }

    /// 借出底层密钥。
    #[must_use]
    pub const fn as_key(&self) -> &SecretKey {
        &self.0
    }

    /// 复制一份。目录密钥在一次操作里常要用于多处（解名、重新加密）。
    #[must_use]
    pub fn duplicate(&self) -> Self {
        Self(SecretKey::from_bytes(*self.0.as_bytes()))
    }

    /// 派生槽位目录的加密密钥。
    ///
    /// 不直接拿 DK 本身当密钥：它已经在给目录名做 AEAD，同一个密钥用于
    /// 两处不同用途时，一处的 nonce 复用会波及另一处。
    #[must_use]
    fn slotdir_key(&self, vault_salt: &[u8; 16]) -> SecretKey {
        crate::crypto::derive_from_secret(&self.0, vault_salt, INFO_SIDECAR_DIR)
    }
}

/// 这份边车是不是可管理模式。
///
/// 只看明文的 flags 位，不需要密钥——与文件头的 `SLOT_DIRECTORY` 同理，
/// 「用了哪种模式」是产品事实而非用户秘密。
#[must_use]
pub fn is_managed(blob: &[u8]) -> bool {
    blob.get(..8) == Some(SIDECAR_MAGIC.as_slice())
        && blob.get(9).is_some_and(|f| f & SIDECAR_FLAG_MANAGED != 0)
}

/// 从可管理模式的边车里解出槽位目录。
///
/// # Errors
///
/// 边车不是可管理模式、长度不对、或目录密文解不开时返回错误。
/// **不返回空目录**：那会让界面显示「一把钥匙都没有」，而它明明打得开。
pub fn read_slot_directory(
    blob: &[u8],
    dk: &DirKey,
    vault_salt: &[u8; 16],
    cipher: CipherId,
) -> Result<crate::slotdir::SlotDirectory> {
    if !is_managed(blob) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "this sidecar has no slot directory (deniable mode)",
        });
    }
    if blob.len() != SIDECAR_LEN_MANAGED {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "managed sidecar has wrong length",
        });
    }
    let ct = blob.get(SIDECAR_LEN..).ok_or(Error::MalformedTlv {
        tlv_type: 0,
        reason: "managed sidecar truncated before slot directory",
    })?;
    let key = dk.slotdir_key(vault_salt);
    // 用 ZERO_NONCE：这个密钥只加密这一条消息，且 DK 每棵树各不相同
    let plain = cipher
        .decrypt(&key, &ZERO_NONCE, ct, &[])?
        .ok_or(Error::MalformedTlv {
            tlv_type: 0,
            reason: "slot directory failed authentication",
        })?;
    crate::slotdir::SlotDirectory::decode(&plain)
}

/// 把槽位目录加密后追加到边车末尾。
fn append_slot_directory(
    base: Vec<u8>,
    dir: &crate::slotdir::SlotDirectory,
    dk: &DirKey,
    vault_salt: &[u8; 16],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    let key = dk.slotdir_key(vault_salt);
    let ct = cipher.encrypt(&key, &ZERO_NONCE, &dir.encode(), &[])?;
    let mut out = base;
    // flags 位要在明文里置上，否则读取侧不会去找目录
    if let Some(f) = out.get_mut(9) {
        *f |= SIDECAR_FLAG_MANAGED;
    }
    out.extend_from_slice(&ct);
    debug_assert_eq!(out.len(), SIDECAR_LEN_MANAGED, "可管理边车必须恰好 {SIDECAR_LEN_MANAGED} 字节");
    Ok(out)
}

/// 把一把 KEK 派生成边车的包裹密钥。
///
/// salt 用 `vault_salt` 而不是目录路径：路径会变（改名、移动），用它做
/// salt 会让目录一改名边车就失效。
fn wrap_key(kek: &Kek, vault_salt: &[u8; 16]) -> SecretKey {
    kek.derive_dirname_key(vault_salt, INFO_SIDECAR_WRAP)
}

/// 构建边车内容。
///
/// `keks` 中第 `i` 把钥匙占第 `i` 个槽，其余槽填随机字节。
///
/// # Errors
///
/// - [`Error::TooManySlots`]：钥匙数超过 8
/// - AEAD 加密失败
pub fn build_sidecar(
    keks: &[Kek],
    dk: &DirKey,
    vault_salt: &[u8; 16],
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    if keks.is_empty() {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "refusing to build a sidecar with zero keys; the directory name could never be decrypted",
        });
    }
    if keks.len() > SIDECAR_SLOTS {
        return Err(Error::TooManySlots { got: keks.len(), max: SIDECAR_SLOTS });
    }

    let mut w = Writer::with_capacity(SIDECAR_LEN);
    w.bytes(SIDECAR_MAGIC);
    w.u8(SIDECAR_VERSION);
    w.bytes(&[0u8; 3]);
    w.bytes(nonce);

    for kek in keks {
        let wk = wrap_key(kek, vault_salt);
        // 用 ZERO_NONCE：包裹密钥每把钥匙各不相同，且只用这一次，
        // 与 slot 区同理——nonce 复用的风险来自同一密钥加密多条消息
        let wrapped = cipher.encrypt(&wk, &ZERO_NONCE, dk.as_key().as_bytes(), &[])?;
        debug_assert_eq!(wrapped.len(), WRAP_LEN, "包裹后必须是 48 字节");
        w.bytes(&wrapped);
    }

    // 剩余槽填随机——可否认性的关键，绝不能改成填零
    let used = keks.len().saturating_mul(WRAP_LEN);
    w.random(SIDECAR_SLOT_AREA.saturating_sub(used));

    let out = w.into_vec();
    debug_assert_eq!(out.len(), SIDECAR_LEN, "边车必须恰好 {SIDECAR_LEN} 字节");
    Ok(out)
}

/// 在保留原有包裹的前提下，用新的钥匙集合重建边车。
///
/// # 为什么需要这个，而不是直接 [`build_sidecar`]
///
/// 文件槽位那边有 [`crate::keyslot::OtherSlots::Carry`]：改密码时，
/// 认不出来的槽位会被原样搬过去，所以挂在文件上的恢复码不会因为改密码
/// 而失效。边车必须有**同样的语义**，否则会出现一种很难查的半残状态——
/// 恢复码还能打开每一个文件，却打不开目录名，解密整棵树报
/// 「content hash mismatch」。
///
/// 实测确认过：没有这个函数时，换一次密码恢复码就从边车里消失了。
///
/// `old` 是原边车内容；解不出来或格式不对时退化成 [`build_sidecar`]，
/// 因为那时也搬不了什么。
///
/// # Errors
///
/// 同 [`build_sidecar`]。
pub fn rebuild_sidecar_carrying(
    old: Option<&[u8]>,
    keep: &[Kek],
    dk: &DirKey,
    vault_salt: &[u8; 16],
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    if keep.is_empty() {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "refusing to build a sidecar with zero keys; the directory name could never be decrypted",
        });
    }
    if keep.len() > SIDECAR_SLOTS {
        return Err(Error::TooManySlots { got: keep.len(), max: SIDECAR_SLOTS });
    }

    // 先照常放 keep。它们占前面的槽，与 build_sidecar 一致
    let mut w = Writer::with_capacity(SIDECAR_LEN);
    w.bytes(SIDECAR_MAGIC);
    w.u8(SIDECAR_VERSION);
    w.bytes(&[0u8; 3]);
    w.bytes(nonce);

    for kek in keep {
        let wk = wrap_key(kek, vault_salt);
        let wrapped = cipher.encrypt(&wk, &ZERO_NONCE, dk.as_key().as_bytes(), &[])?;
        w.bytes(&wrapped);
    }

    // 再把认不出来的旧槽原样搬过来。
    //
    // 为什么可以原样搬：包裹密钥只依赖 (KEK, vault_salt)，与槽位下标无关，
    // 所以同一份 48 字节挪到任何位置都仍然解得开。这与文件槽位不同——
    // 那边的包裹密钥绑定 slot_index，搬运时下标必须对得上
    let mut written = keep.len();
    if let Some(prev) = old {
        // 两种长度都接受：可管理模式的边车在基础长度之后还追加了
        // 加密目录，只认 SIDECAR_LEN 的话它的旧包裹一个都搬不过来——
        // 表现为「换密码后恢复码打不开目录名了」
        let known_len = prev.len() == SIDECAR_LEN || prev.len() == SIDECAR_LEN_MANAGED;
        if known_len && prev.get(..8) == Some(SIDECAR_MAGIC.as_slice()) {
            let slots_start = 12 + NONCE_LEN;
            for i in 0..SIDECAR_SLOTS {
                if written >= SIDECAR_SLOTS {
                    break;
                }
                let from = slots_start.saturating_add(i.saturating_mul(WRAP_LEN));
                let to = from.saturating_add(WRAP_LEN);
                let Some(slot) = prev.get(from..to) else { continue };
                // 能被 keep 里某把钥匙解开的槽已经重新写过了，再搬一份
                // 只是浪费槽位
                let already = keep.iter().any(|k| {
                    let wk = wrap_key(k, vault_salt);
                    matches!(cipher.decrypt(&wk, &ZERO_NONCE, slot, &[]), Ok(Some(_)))
                });
                if already {
                    continue;
                }
                w.bytes(slot);
                written = written.saturating_add(1);
            }
        }
    }

    let used = written.saturating_mul(WRAP_LEN);
    w.random(SIDECAR_SLOT_AREA.saturating_sub(used));

    let out = w.into_vec();
    debug_assert_eq!(out.len(), SIDECAR_LEN, "边车必须恰好 {SIDECAR_LEN} 字节");
    Ok(out)
}

/// 构建带槽位目录的边车（可管理模式）。
///
/// `keks[i]` 占第 `i` 个槽，`dir` 必须与之对应——目录说 slot 2 是恢复码，
/// 那么 `keks[2]` 就得真是那把恢复码派生出来的 KEK。对不上的话用户会
///照着目录去删错东西。
///
/// # Errors
///
/// 同 [`build_sidecar`]。
pub fn build_sidecar_managed(
    keks: &[Kek],
    dir: &crate::slotdir::SlotDirectory,
    dk: &DirKey,
    vault_salt: &[u8; 16],
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    let base = build_sidecar(keks, dk, vault_salt, nonce, cipher)?;
    append_slot_directory(base, dir, dk, vault_salt, cipher)
}

/// 按计划精确改写边车的槽位（可管理模式）。
///
/// `plans[i]` 严格对应第 `i` 个槽。与文件那边的
/// [`crate::keyslot::rewrite_slots_managed`] 语义一致，但有一处关键差异：
/// 边车的包裹密钥只依赖 `(KEK, vault_salt)`、**与槽位下标无关**，所以
/// `Keep` 在这里是真正的「原样搬那 48 字节」，不像文件那边还要求下标对齐。
///
/// # Errors
///
/// - 旧边车格式不对、或不是可管理模式
/// - 计划会让一个槽都不剩（写出去就再也解不开目录名了）
pub fn rewrite_sidecar_managed(
    old: &[u8],
    plans: &[SidecarPlan],
    dir: &crate::slotdir::SlotDirectory,
    dk: &DirKey,
    vault_salt: &[u8; 16],
    nonce: &[u8; NONCE_LEN],
    cipher: CipherId,
) -> Result<Vec<u8>> {
    if !is_managed(old) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "refusing precise rewrite on a deniable-mode sidecar",
        });
    }
    if old.len() != SIDECAR_LEN_MANAGED {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "managed sidecar has wrong length",
        });
    }
    // 一个都不留会写出谁也解不开目录名的边车。必须在写盘前拦住：
    // 写完才发现解不开，用户同时失去目录名和访问它的办法
    if !plans.iter().any(|p| matches!(p, SidecarPlan::Keep | SidecarPlan::Write(_))) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "refusing to leave the sidecar with zero usable keys",
        });
    }

    let mut w = Writer::with_capacity(SIDECAR_LEN);
    w.bytes(SIDECAR_MAGIC);
    w.u8(SIDECAR_VERSION);
    w.u8(SIDECAR_FLAG_MANAGED);
    w.bytes(&[0u8; 2]);
    w.bytes(nonce);

    let slots_start = 12 + NONCE_LEN;
    for (i, plan) in plans.iter().enumerate().take(SIDECAR_SLOTS) {
        match plan {
            SidecarPlan::Keep => {
                let from = slots_start.saturating_add(i.saturating_mul(WRAP_LEN));
                let to = from.saturating_add(WRAP_LEN);
                match old.get(from..to) {
                    Some(slot) => {
                        w.bytes(slot);
                    }
                    None => {
                        w.random(WRAP_LEN);
                    }
                }
            }
            SidecarPlan::Write(kek) => {
                let wk = wrap_key(kek, vault_salt);
                let wrapped = cipher.encrypt(&wk, &ZERO_NONCE, dk.as_key().as_bytes(), &[])?;
                w.bytes(&wrapped);
            }
            // 空槽填随机而非填零：填零的话「哪些槽在用」直接可见，
            // 而边车的可否认性正建立在这条上
            SidecarPlan::Clear => {
                w.random(WRAP_LEN);
            }
        }
    }
    // 计划不足 8 条时补满
    let written = plans.len().min(SIDECAR_SLOTS);
    w.random(SIDECAR_SLOT_AREA.saturating_sub(written.saturating_mul(WRAP_LEN)));

    append_slot_directory(w.into_vec(), dir, dk, vault_salt, cipher)
}

/// 可管理模式下，一个边车槽位要怎么处理。
///
/// 不 derive Clone：`Kek` 刻意没有 Clone（密钥不该被随手复制）。
#[derive(Debug)]
pub enum SidecarPlan {
    /// 原样搬运这一槽。
    Keep,
    /// 用这把钥匙重新包裹 DK。
    Write(Kek),
    /// 清成随机字节。
    Clear,
}
/// 从边车里解出目录密钥。
///
/// 对每把候选钥匙尝试每个槽，返回 `(DirKey, nonce)`。
///
/// # Errors
///
/// - [`Error::MalformedTlv`]：长度不对、magic 不匹配、版本不认识
/// - [`Error::NoMatchingSlot`]：所有组合都失败
///
/// 最后一种**不代表边车损坏**，也可能只是手头的钥匙不属于这棵树，
/// 面向用户的文案必须涵盖两种可能。
pub fn open_sidecar(
    blob: &[u8],
    keks: &[Kek],
    vault_salt: &[u8; 16],
    cipher: CipherId,
) -> Result<(DirKey, [u8; NONCE_LEN])> {
    open_sidecar_at(blob, keks, vault_salt, cipher).map(|(dk, nonce, _)| (dk, nonce))
}

/// 同 [`open_sidecar`]，另外返回命中的**槽位下标**。
///
/// 精确管理需要这个下标：remove 要避免删掉当前正在用的那把钥匙，
/// change 要就地替换它。`open_sidecar` 不返回它是因为绝大多数调用方
/// 只关心能不能解开。
///
/// # Errors
///
/// 同 [`open_sidecar`]。
pub fn open_sidecar_at(
    blob: &[u8],
    keks: &[Kek],
    vault_salt: &[u8; 16],
    cipher: CipherId,
) -> Result<(DirKey, [u8; NONCE_LEN], usize)> {
    // 可管理模式的边车更长，两种都要接受
    if blob.len() != SIDECAR_LEN && blob.len() != SIDECAR_LEN_MANAGED {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "dirname sidecar has wrong length",
        });
    }
    if blob.get(..8) != Some(SIDECAR_MAGIC.as_slice()) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "dirname sidecar magic mismatch",
        });
    }
    if blob.get(8) != Some(&SIDECAR_VERSION) {
        return Err(Error::MalformedTlv {
            tlv_type: 0,
            reason: "unsupported dirname sidecar version",
        });
    }

    let nonce_start = 12;
    let slots_start = nonce_start + NONCE_LEN;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(
        blob.get(nonce_start..slots_start).ok_or(Error::MalformedTlv {
            tlv_type: 0,
            reason: "dirname sidecar truncated before nonce",
        })?,
    );

    for kek in keks {
        let wk = wrap_key(kek, vault_salt);
        for i in 0..SIDECAR_SLOTS {
            let from = slots_start.saturating_add(i.saturating_mul(WRAP_LEN));
            let to = from.saturating_add(WRAP_LEN);
            let Some(slot) = blob.get(from..to) else { continue };
            // decrypt 返回 Result<Option<..>>：Err 是用法错误（长度之类），
            // Ok(None) 才是「认证失败」。这里两者都只意味着「这把钥匙不对，
            // 换下一个」，不该中断整个尝试
            let Ok(Some(plain)) = cipher.decrypt(&wk, &ZERO_NONCE, slot, &[]) else {
                continue;
            };
            if plain.len() != DK_LEN {
                continue;
            }
            let mut raw = [0u8; DK_LEN];
            raw.copy_from_slice(&plain);
            return Ok((DirKey::from_bytes(raw), nonce, i));
        }
    }
    Err(Error::NoMatchingSlot)
}

#[cfg(test)]
#[expect(clippy::panic, reason = "测试里 panic 就是断言失败的表达方式")]
mod tests {
    use super::*;
    use crate::crypto::Argon2Params;

    fn kek(pw: &str, salt: &[u8; 16]) -> Kek {
        // TEST_WEAK 而不是生产档位：这些测试只关心包裹与解包的逻辑，
        // 用 64 MiB 的 Argon2 会让整个测试文件慢上两个数量级
        Kek::from_password(pw.as_bytes(), salt, Argon2Params::TEST_WEAK).expect("派生 KEK")
    }

    fn nonce_of(n: u8) -> [u8; NONCE_LEN] {
        core::array::from_fn(|i| (i as u8).wrapping_mul(3) ^ n)
    }

    #[test]
    fn every_key_opens_the_same_dir_key() {
        let salt = [9u8; 16];
        let keks = vec![kek("one", &salt), kek("two", &salt), kek("three", &salt)];
        let dk = DirKey::generate();
        let n = nonce_of(1);
        let blob = build_sidecar(&keks, &dk, &salt, &n, CipherId::ChaCha20Poly1305).expect("构建");

        // 不这样会怎样：这正是整个改造要解决的问题。若只有第一把钥匙
        // 能解开，多密码和恢复码在树上依旧只能看第一把的脸色
        for (i, k) in keks.iter().enumerate() {
            let (got, got_n) =
                open_sidecar(&blob, std::slice::from_ref(k), &salt, CipherId::ChaCha20Poly1305)
                    .unwrap_or_else(|e| panic!("第 {i} 把钥匙应能解开边车：{e}"));
            assert_eq!(got.as_key().as_bytes(), dk.as_key().as_bytes(), "第 {i} 把解出的 DK 不对");
            assert_eq!(got_n, n, "nonce 应原样取回");
        }
    }

    #[test]
    fn length_is_constant_regardless_of_key_count() {
        let salt = [3u8; 16];
        let dk = DirKey::generate();
        let n = nonce_of(2);
        let all = vec![kek("a", &salt), kek("b", &salt), kek("c", &salt), kek("d", &salt)];

        // 不这样会怎样：长度随钥匙数变化，就能一眼数出这棵树配了几把钥匙
        for count in 1..=4 {
            let blob = build_sidecar(
                all.get(..count).unwrap_or(&all),
                &dk,
                &salt,
                &n,
                CipherId::ChaCha20Poly1305,
            )
            .expect("构建");
            assert_eq!(blob.len(), SIDECAR_LEN, "{count} 把钥匙时长度变了");
        }
    }

    #[test]
    fn unused_slots_are_not_zero_filled() {
        let salt = [4u8; 16];
        let dk = DirKey::generate();
        let blob = build_sidecar(
            &[kek("only", &salt)],
            &dk,
            &salt,
            &nonce_of(3),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        // 不这样会怎样：空槽填零的话，「这个目录只配了 1 把钥匙」
        // 直接可见，可否认性当场失效
        let slots_start = 12 + NONCE_LEN;
        let unused = blob.get(slots_start + WRAP_LEN..).expect("应有剩余槽");
        assert!(
            unused.iter().any(|&b| b != 0),
            "未使用的槽被填成了零，可否认性失效"
        );
    }

    #[test]
    fn carrying_keeps_unknown_wraps_but_discarding_drops_them() {
        // 重建边车时，Carry 要保住认不出来的旧包裹（典型是恢复码），
        // 而调用方选择丢弃时必须真的丢掉。
        //
        // 不这样会怎样：Carry 那半边若失效，改一次密码就把恢复码从
        // 边车里抹了——恢复码还能打开每个文件，却解不开目录名，
        // 用户拿它解密整棵树得到「文件损坏」。
        //
        // 而丢弃那半边若失效（等于永远 Carry），一个本该被作废的钥匙
        // 仍能从边车解出 DK。今天文件槽位挡着所以没后果，但两层一旦
        // 不同步它就是真实的泄露。这条正是为了不让那半边成为死代码。
        let salt = [11u8; 16];
        let main_key = kek("main", &salt);
        let reco = kek("recovery-code", &salt);
        let dk = DirKey::generate();
        let cipher = CipherId::ChaCha20Poly1305;

        let original = build_sidecar(
            &[main_key.duplicate(), reco.duplicate()],
            &dk,
            &salt,
            &nonce_of(20),
            cipher,
        )
        .expect("构建");

        // 只拿 main 重建：reco 对 rebuild 来说是「认不出来的槽」
        let carried = rebuild_sidecar_carrying(
            Some(&original),
            &[main_key.duplicate()],
            &dk,
            &salt,
            &nonce_of(21),
            cipher,
        )
        .expect("Carry 重建");
        assert!(
            open_sidecar(&carried, &[reco.duplicate()], &salt, cipher).is_ok(),
            "Carry 应当保住恢复码的包裹"
        );
        assert!(
            open_sidecar(&carried, &[main_key.duplicate()], &salt, cipher).is_ok(),
            "Carry 之后 main 自己也得还能用"
        );

        // 不传旧边车 = 丢弃：恢复码必须再也解不开
        let dropped = rebuild_sidecar_carrying(
            None,
            &[main_key.duplicate()],
            &dk,
            &salt,
            &nonce_of(22),
            cipher,
        )
        .expect("Discard 重建");
        assert!(
            open_sidecar(&dropped, &[reco], &salt, cipher).is_err(),
            "丢弃时旧包裹必须真的没了——留着等于那把钥匙没被作废"
        );
        assert!(
            open_sidecar(&dropped, &[main_key], &salt, cipher).is_ok(),
            "丢弃不该连保留的钥匙一起丢"
        );
    }

    #[test]
    fn wrong_key_is_rejected() {
        let salt = [5u8; 16];
        let dk = DirKey::generate();
        let blob = build_sidecar(
            &[kek("right", &salt)],
            &dk,
            &salt,
            &nonce_of(4),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");
        let wrong = kek("wrong", &salt);
        assert!(
            matches!(
                open_sidecar(&blob, &[wrong], &salt, CipherId::ChaCha20Poly1305),
                Err(Error::NoMatchingSlot)
            ),
            "不属于这棵树的钥匙必须被拒绝"
        );
    }

    #[test]
    fn sidecar_is_bound_to_vault_salt() {
        let salt_a = [1u8; 16];
        let salt_b = [2u8; 16];
        let k = kek("same-password", &salt_a);
        let dk = DirKey::generate();
        let blob =
            build_sidecar(&[k], &dk, &salt_a, &nonce_of(5), CipherId::ChaCha20Poly1305)
                .expect("构建");

        // 不这样会怎样：换个 vault 用同一把 KEK 就能解开别人的目录名。
        // 这里换的是解包时用的 salt，KEK 本身不变——正是要锁住这一点
        let k2 = kek("same-password", &salt_a);
        assert!(
            open_sidecar(&blob, &[k2], &salt_b, CipherId::ChaCha20Poly1305).is_err(),
            "边车必须绑定 vault_salt"
        );
    }

    #[test]
    fn refuses_to_build_with_no_keys() {
        // 不这样会怎样：产出一个谁都解不开的边车，目录名永久丢失，
        // 而调用方拿到的是 Ok
        let salt = [6u8; 16];
        assert!(
            build_sidecar(
                &[],
                &DirKey::generate(),
                &salt,
                &nonce_of(6),
                CipherId::ChaCha20Poly1305
            )
            .is_err(),
            "零把钥匙必须拒绝"
        );
    }

    #[test]
    fn rejects_corrupted_header() {
        let salt = [8u8; 16];
        let dk = DirKey::generate();
        let good = build_sidecar(
            &[kek("k", &salt)],
            &dk,
            &salt,
            &nonce_of(7),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        // 不这样会怎样：把任意文件当边车读，会拿一堆垃圾去试解密，
        // 报出来的错误是「钥匙不对」而不是「这不是边车」，排查方向全错
        let mut bad_magic = good.clone();
        if let Some(b) = bad_magic.get_mut(0) {
            *b ^= 0xFF;
        }
        assert!(open_sidecar(&bad_magic, &[kek("k", &salt)], &salt, CipherId::ChaCha20Poly1305)
            .is_err());

        let mut bad_ver = good.clone();
        if let Some(b) = bad_ver.get_mut(8) {
            *b = 99;
        }
        assert!(open_sidecar(&bad_ver, &[kek("k", &salt)], &salt, CipherId::ChaCha20Poly1305)
            .is_err());

        let short = good.get(..SIDECAR_LEN - 1).unwrap_or(&good).to_vec();
        assert!(open_sidecar(&short, &[kek("k", &salt)], &salt, CipherId::ChaCha20Poly1305)
            .is_err());
    }
    // ---------- 可管理模式（树形槽位目录） ----------

    #[test]
    fn managed_sidecar_roundtrips_the_directory() {
        // 最基础的一条：写进去的目录要能原样读出来，flags 位也要置上。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let dk = DirKey::generate();
        let mut dir = crate::slotdir::SlotDirectory::new();
        dir.set(0, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("set 0");

        let blob = build_sidecar_managed(
            &[owner.duplicate()],
            &dir,
            &dk,
            &salt,
            &nonce_of(1),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        assert!(is_managed(&blob), "flags 位没置上");
        assert_eq!(blob.len(), SIDECAR_LEN_MANAGED);

        let (got_dk, _, idx) =
            open_sidecar_at(&blob, &[owner], &salt, CipherId::ChaCha20Poly1305).expect("解开");
        assert_eq!(idx, 0, "命中的应当是 slot 0");
        let got = read_slot_directory(&blob, &got_dk, &salt, CipherId::ChaCha20Poly1305)
            .expect("读目录");
        assert_eq!(got, dir, "目录内容对不上");
    }

    #[test]
    fn deniable_sidecar_refuses_to_give_a_directory() {
        // 反证：可否认模式必须报错，而不是返回一个空目录。
        //
        // 不这样会怎样：返回空目录会让界面显示「这棵树一把钥匙都没有」，
        // 而它明明打得开——用户会以为目录坏了。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let dk = DirKey::generate();
        let blob = build_sidecar(
            &[owner.duplicate()],
            &dk,
            &salt,
            &nonce_of(2),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        assert!(!is_managed(&blob));
        assert!(
            read_slot_directory(&blob, &dk, &salt, CipherId::ChaCha20Poly1305).is_err(),
            "可否认模式不该给出目录"
        );
    }

    #[test]
    fn managed_length_is_constant_regardless_of_key_count() {
        // 长度恒定是可否认性的基础：变长会让边车大小泄露这棵树配了
        // 几把钥匙，而那正是这个格式想保住的东西。
        let salt = [9u8; 16];
        let dk = DirKey::generate();
        let dir = crate::slotdir::SlotDirectory::new();
        let mut lens = Vec::new();
        for n in 1..=4usize {
            let keks: Vec<Kek> = (0..n).map(|i| kek(&format!("pw{i}"), &salt)).collect();
            let blob = build_sidecar_managed(
                &keks,
                &dir,
                &dk,
                &salt,
                &nonce_of(3),
                CipherId::ChaCha20Poly1305,
            )
            .expect("构建");
            lens.push(blob.len());
        }
        assert!(
            lens.iter().all(|&l| l == SIDECAR_LEN_MANAGED),
            "长度随钥匙数变化了：{lens:?}"
        );
    }

    #[test]
    fn precise_rewrite_removes_one_key_and_keeps_the_rest() {
        // 这是树形可管理模式存在的理由：精确踢掉一把钥匙而保住其它的。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let mate = kek("mate", &salt);
        let reco = kek("reco", &salt);
        let dk = DirKey::generate();

        let mut dir = crate::slotdir::SlotDirectory::new();
        dir.set(0, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("0");
        dir.set(1, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("1");
        dir.set(2, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Recovery))
            .expect("2");

        let blob = build_sidecar_managed(
            &[owner.duplicate(), mate.duplicate(), reco.duplicate()],
            &dir,
            &dk,
            &salt,
            &nonce_of(4),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        // 前提自证：三把钥匙一开始都能解开
        for (name, k) in [("owner", &owner), ("mate", &mate), ("reco", &reco)] {
            assert!(
                open_sidecar(&blob, &[k.duplicate()], &salt, CipherId::ChaCha20Poly1305).is_ok(),
                "{name} 一开始就该能解开"
            );
        }

        // 只踢掉 slot 1 的协作者
        let mut after = dir;
        after.set(1, crate::slotdir::SlotEntry::empty()).expect("清空");
        let mut plans = vec![SidecarPlan::Keep, SidecarPlan::Clear, SidecarPlan::Keep];
        plans.resize_with(SIDECAR_SLOTS, || SidecarPlan::Clear);

        let out = rewrite_sidecar_managed(
            &blob,
            &plans,
            &after,
            &dk,
            &salt,
            &nonce_of(4),
            CipherId::ChaCha20Poly1305,
        )
        .expect("改写");

        assert!(
            open_sidecar(&out, &[owner], &salt, CipherId::ChaCha20Poly1305).is_ok(),
            "主密码不该受影响"
        );
        assert!(
            open_sidecar(&out, &[reco], &salt, CipherId::ChaCha20Poly1305).is_ok(),
            "恢复码必须保住——这正是可管理模式的全部意义"
        );
        assert!(
            open_sidecar(&out, &[mate], &salt, CipherId::ChaCha20Poly1305).is_err(),
            "被踢掉的协作者必须再也解不开"
        );
    }

    #[test]
    fn precise_rewrite_refuses_to_orphan_the_directory_name() {
        // 全 Clear 会写出一份谁也解不开的边车，目录名就此永远丢失。
        // 必须在写盘前拦住。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let dk = DirKey::generate();
        let dir = crate::slotdir::SlotDirectory::new();
        let blob = build_sidecar_managed(
            &[owner],
            &dir,
            &dk,
            &salt,
            &nonce_of(5),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        let plans: Vec<SidecarPlan> = (0..SIDECAR_SLOTS).map(|_| SidecarPlan::Clear).collect();
        assert!(
            rewrite_sidecar_managed(
                &blob,
                &plans,
                &dir,
                &dk,
                &salt,
                &nonce_of(5),
                CipherId::ChaCha20Poly1305,
            )
            .is_err(),
            "一个槽都不留必须报错"
        );
    }

    #[test]
    fn precise_rewrite_rejects_a_deniable_sidecar() {
        // 走错路径要明确报错，而不是写出一份 flags 与实际不一致的边车——
        // 那种损坏不会立刻显现。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let dk = DirKey::generate();
        let blob = build_sidecar(
            &[owner.duplicate()],
            &dk,
            &salt,
            &nonce_of(6),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        let mut plans = vec![SidecarPlan::Keep];
        plans.resize_with(SIDECAR_SLOTS, || SidecarPlan::Clear);
        assert!(
            rewrite_sidecar_managed(
                &blob,
                &plans,
                &crate::slotdir::SlotDirectory::new(),
                &dk,
                &salt,
                &nonce_of(6),
                CipherId::ChaCha20Poly1305,
            )
            .is_err(),
            "对可否认模式的边车应当拒绝"
        );
    }

    #[test]
    fn precise_rewrite_fills_cleared_slots_with_random() {
        // 已有的 unused_slots_are_not_zero_filled 只覆盖 build_sidecar，
        // 不覆盖精确改写的 Clear 分支——变异测试把这个缺口抓了出来。
        //
        // 不这样会怎样：被删掉的那个槽填零，「这里原来有人、现在删了」
        // 直接可见，而边车的可否认性正建立在「看不出哪些槽在用」上。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let mate = kek("mate", &salt);
        let dk = DirKey::generate();
        let mut dir = crate::slotdir::SlotDirectory::new();
        dir.set(0, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("0");
        dir.set(1, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("1");

        let blob = build_sidecar_managed(
            &[owner, mate],
            &dir,
            &dk,
            &salt,
            &nonce_of(8),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        let mut after = dir;
        after.set(1, crate::slotdir::SlotEntry::empty()).expect("清空");
        let mut plans = vec![SidecarPlan::Keep, SidecarPlan::Clear];
        plans.resize_with(SIDECAR_SLOTS, || SidecarPlan::Clear);
        let out = rewrite_sidecar_managed(
            &blob,
            &plans,
            &after,
            &dk,
            &salt,
            &nonce_of(8),
            CipherId::ChaCha20Poly1305,
        )
        .expect("改写");

        // 整个槽位区（含被清掉的那些）都不该出现整段零
        let slots_start = 12 + NONCE_LEN;
        let area = out
            .get(slots_start..slots_start + SIDECAR_SLOT_AREA)
            .expect("槽位区");
        let zeros = vec![0u8; WRAP_LEN];
        for i in 0..SIDECAR_SLOTS {
            let from = i * WRAP_LEN;
            let slot = area.get(from..from + WRAP_LEN).expect("槽");
            assert_ne!(slot, zeros.as_slice(), "slot {i} 被填成了零");
        }
    }

    #[test]
    fn wrong_key_cannot_read_the_directory() {
        // 目录是加密的：拿不到 DK 就读不到它。这条守的是
        // 「对解不开这棵树的人，两种模式一样什么都看不出来」。
        let salt = [9u8; 16];
        let owner = kek("owner", &salt);
        let outsider = DirKey::generate();
        let dk = DirKey::generate();
        let mut dir = crate::slotdir::SlotDirectory::new();
        dir.set(0, crate::slotdir::SlotEntry::of(crate::slotdir::SlotKind::Vault))
            .expect("set");

        let blob = build_sidecar_managed(
            &[owner],
            &dir,
            &dk,
            &salt,
            &nonce_of(7),
            CipherId::ChaCha20Poly1305,
        )
        .expect("构建");

        assert!(
            read_slot_directory(&blob, &outsider, &salt, CipherId::ChaCha20Poly1305).is_err(),
            "用别的 DK 不该读出目录"
        );
    }
}