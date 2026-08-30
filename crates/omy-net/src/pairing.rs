//! SPAKE2 配对：把一个 6 位数字 PIN 变成双方共享的强密钥。
//!
//! # 为什么必须自己做密钥确认
//!
//! Spike 实测的关键结论（见 `spikes/lan-pairing`）：
//!
//! > **SPAKE2 在 PIN 不同时不报错。** `finish()` 照常返回 `Ok`，
//! > 只是双方拿到不同的密钥。
//!
//! 很容易误以为"PAKE 会自己检测错误密码"，从而漏掉确认步骤。那样的话
//! 攻击者用任意 PIN 都能"成功"握手，直到后续通信全是乱码才暴露——
//! 而那时连接已经建立、静态公钥已经交换。
//!
//! 因此本模块在 SPAKE2 之后**强制**跑一轮密钥确认：双方各自用派生出的
//! 密钥对一个固定串算 MAC 并交换，用常量时间比较。不通过就是 PIN 错。
//!
//! # 一次 PIN 只给一次机会
//!
//! SPAKE2 的安全性建立在"每次协议执行只允许一次在线猜测"上。若允许用
//! 同一个 PIN 重试，攻击者就能把在线猜测变成离线字典攻击。因此
//! [`Pairing`] 的两个方法都**消费 self**，用完即弃；PIN 必须重新生成。

use crate::error::{NetError, Result};
use rand::RngCore as _;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroize as _;

/// PIN 的位数。
///
/// 6 位十进制 = 100 万种可能。对在线攻击足够：每次配对只给一次机会，
/// 且 PIN 用完即弃。再长会显著影响手动输入的体验。
pub const PIN_DIGITS: usize = 6;

/// SPAKE2 的 identity 串。
///
/// 把派生密钥绑定到具体用途上，防止攻击者把配对消息挪用到其他场景。
/// 改动它会导致新旧版本无法配对，等同于协议不兼容。
const PAIRING_ID: &[u8] = b"omy/v1/pairing";

/// 密钥确认时 MAC 的输入串。
///
/// 两侧用**不同**的串，这样一侧的确认值不能直接重放给另一侧。
const CONFIRM_A: &[u8] = b"omy/v1/confirm/initiator";
const CONFIRM_B: &[u8] = b"omy/v1/confirm/responder";

/// 配对中的角色。
///
/// SPAKE2 的对称模式两边代码相同，但**密钥确认**必须区分方向，
/// 否则攻击者可以把 A 发来的确认值原样回给 A。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 发起配对的一方（扫码方 / 输入 PIN 的一方）。
    Initiator,
    /// 展示 PIN 的一方。
    Responder,
}

impl Role {
    /// 本方在确认时使用的串。
    fn own_tag(self) -> &'static [u8] {
        match self {
            Self::Initiator => CONFIRM_A,
            Self::Responder => CONFIRM_B,
        }
    }
    /// 对方在确认时使用的串。
    fn peer_tag(self) -> &'static [u8] {
        match self {
            Self::Initiator => CONFIRM_B,
            Self::Responder => CONFIRM_A,
        }
    }
}

/// 生成一个随机 PIN。
///
/// 用密码学安全的随机源，而不是时间戳或计数器——PIN 的全部安全性
/// 就在于攻击者猜不到它。
///
/// 返回定长的数字字符串，含前导零（如 `"004271"`）。前导零必须保留：
/// 若把 PIN 当整数处理，`004271` 与 `4271` 会派生出不同的密钥，
/// 造成"明明输对了却配对失败"。
#[must_use]
pub fn generate_pin() -> String {
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(PIN_DIGITS);
    for _ in 0..PIN_DIGITS {
        // 逐位取，避免 u32 取模带来的模偏置
        let d = loop {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            let v = b[0];
            // 250 = 25 × 10，拒绝 250..=255 使各数字等概率
            if v < 250 {
                break v % 10;
            }
        };
        s.push(char::from(b'0' + d));
    }
    s
}

/// 校验用户输入的 PIN 格式。
///
/// # Errors
/// 位数不对或含非数字字符时返回 [`NetError::PairingFailed`]。
pub fn validate_pin(pin: &str) -> Result<()> {
    if pin.chars().count() != PIN_DIGITS || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(NetError::PairingFailed);
    }
    Ok(())
}

/// 配对的第一阶段：已生成本方消息，等待对方消息。
pub struct Pairing {
    inner: spake2::Spake2<spake2::Ed25519Group>,
    role: Role,
}

/// 配对的第二阶段：已算出共享密钥，等待交换确认值。
///
/// `key` 用 `Option` 包裹是为了让 [`Self::verify`] 能在消费 self 时把它
/// 移出来——实现了 `Drop` 的类型不允许直接移出字段。
pub struct PairingConfirm {
    key: Option<Vec<u8>>,
    role: Role,
}

/// 配对成功后的产物。
pub struct PairedKeys {
    /// 用于加密后续配对数据（如静态公钥交换）的会话密钥。
    key: Vec<u8>,
}

impl Pairing {
    /// 开始配对，返回状态与要发给对方的消息。
    ///
    /// # Errors
    /// PIN 格式非法时返回 [`NetError::PairingFailed`]。
    pub fn start(pin: &str, role: Role) -> Result<(Self, Vec<u8>)> {
        validate_pin(pin)?;
        let (inner, msg) = spake2::Spake2::<spake2::Ed25519Group>::start_symmetric(
            &spake2::Password::new(pin.as_bytes()),
            &spake2::Identity::new(PAIRING_ID),
        );
        Ok((Self { inner, role }, msg))
    }

    /// 收到对方消息，算出共享密钥。
    ///
    /// ⚠️ **此时还不能信任对方**。PIN 不匹配时这一步照样成功，
    /// 只是双方密钥不同。必须继续走 [`PairingConfirm`]。
    ///
    /// # Errors
    /// 对方消息格式非法时返回 [`NetError::PairingFailed`]。
    pub fn finish(self, peer_msg: &[u8]) -> Result<(PairingConfirm, Vec<u8>)> {
        let key = self.inner.finish(peer_msg).map_err(|_| NetError::PairingFailed)?;
        let confirm = mac(&key, self.role.own_tag()).to_vec();
        Ok((PairingConfirm { key: Some(key), role: self.role }, confirm))
    }
}

impl PairingConfirm {
    /// 校验对方的确认值。
    ///
    /// 这才是真正判断 PIN 是否匹配的地方。
    ///
    /// # Errors
    /// 确认值不匹配时返回 [`NetError::PairingFailed`]——即 PIN 输错了，
    /// 或存在中间人。
    pub fn verify(mut self, peer_confirm: &[u8]) -> Result<PairedKeys> {
        let Some(key) = self.key.take() else {
            return Err(NetError::PairingFailed);
        };
        let expected = mac(&key, self.role.peer_tag());
        // 常量时间比较：普通 == 会因短路而泄露前多少字节正确，
        // 攻击者可据此逐字节爆破
        if expected.ct_eq(peer_confirm).into() {
            Ok(PairedKeys { key })
        } else {
            // 失败时立即擦除，不留在内存里
            let mut k = key;
            k.zeroize();
            Err(NetError::PairingFailed)
        }
    }
}

impl PairedKeys {
    /// 取出会话密钥的引用。
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    /// 派生一个用途隔离的子密钥。
    ///
    /// 同一个配对密钥可能要用于多个目的（加密静态公钥、派生信道
    /// prologue 等）。用不同 label 派生可确保各用途的密钥互相独立——
    /// 一处泄露不至于波及其他。
    #[must_use]
    pub fn derive(&self, label: &[u8]) -> [u8; 32] {
        mac(&self.key, label)
    }
}

impl Drop for PairedKeys {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl Drop for PairingConfirm {
    fn drop(&mut self) {
        if let Some(k) = self.key.as_mut() {
            k.zeroize();
        }
    }
}

/// 用共享密钥对 tag 算 MAC。
///
/// 用 HMAC-SHA256：omy-core 已经依赖它（HKDF 内部就用 HMAC），
/// 不额外引入原语。SPAKE2 的输出恒为 32 字节，HMAC 对 key 长度无限制，
/// 因此这里不存在长度不合法的情况。
fn mac(key: &[u8], tag: &[u8]) -> [u8; 32] {
    use hmac::Mac as _;
    // HMAC 接受任意长度的 key，new_from_slice 对 SHA256 永不失败
    let mut m = <hmac::Hmac<sha2::Sha256> as hmac::Mac>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("HMAC 接受任意长度 key"));
    m.update(tag);
    m.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 跑一次完整配对。
    fn run(pin_a: &str, pin_b: &str) -> Result<(PairedKeys, PairedKeys)> {
        let (pa, msg_a) = Pairing::start(pin_a, Role::Initiator)?;
        let (pb, msg_b) = Pairing::start(pin_b, Role::Responder)?;
        let (ca, conf_a) = pa.finish(&msg_b)?;
        let (cb, conf_b) = pb.finish(&msg_a)?;
        let ka = ca.verify(&conf_b)?;
        let kb = cb.verify(&conf_a)?;
        Ok((ka, kb))
    }

    #[test]
    fn same_pin_yields_same_key() {
        let (a, b) = run("042719", "042719").expect("相同 PIN 应配对成功");
        assert_eq!(a.as_bytes(), b.as_bytes(), "双方应得到相同密钥");
        assert_eq!(a.as_bytes().len(), 32);
    }

    /// 最关键的一条：PIN 不同必须**失败**。
    ///
    /// 这正是 spike 发现的坑——SPAKE2 自己不会报错，全靠密钥确认。
    #[test]
    fn different_pin_is_rejected() {
        let r = run("042719", "000000");
        assert!(r.is_err(), "PIN 不同必须被密钥确认拒绝");
    }

    /// 定位上一条测试的**失败位置**。
    ///
    /// 只断言"配对失败"是不够的：如果哪天有人把确认逻辑删了，而失败
    /// 恰好来自别处（比如 SPAKE2 报错），上一条测试照样绿。本条把每个
    /// 阶段拆开，明确断言：
    ///
    /// 1. `finish()` 在 PIN 不同时**必须成功**（这是 spike 的核心发现）
    /// 2. 双方由此得到**不同**的确认值
    /// 3. 失败**只发生在** `verify()`
    ///
    /// 少了这条，"密钥确认真的在起作用"就只是一厢情愿。
    #[test]
    fn rejection_happens_exactly_at_key_confirmation() {
        let (pa, msg_a) = Pairing::start("042719", Role::Initiator).expect("start");
        let (pb, msg_b) = Pairing::start("000000", Role::Responder).expect("start");

        // 1. SPAKE2 自身不报错——这正是必须自己做确认的原因
        let (ca, conf_a) = pa
            .finish(&msg_b)
            .expect("SPAKE2 在 PIN 不同时也必须成功，否则本测试的前提已变");
        let (cb, conf_b) = pb
            .finish(&msg_a)
            .expect("SPAKE2 在 PIN 不同时也必须成功，否则本测试的前提已变");

        // 2. 确认值不同
        assert_ne!(conf_a, conf_b, "PIN 不同应导致确认值不同");

        // 3. 失败发生在 verify
        assert!(ca.verify(&conf_b).is_err(), "错误的确认值必须在 verify 被拒");
        assert!(cb.verify(&conf_a).is_err(), "错误的确认值必须在 verify 被拒");
    }

    /// 只错一位也必须拒绝。
    ///
    /// PAKE 的意义就在于弱口令也能派生出强密钥——若"接近的 PIN"
    /// 能产生接近的密钥，攻击者就能做梯度爆破。
    #[test]
    fn one_digit_off_is_rejected() {
        for (a, b) in [
            ("111111", "111112"),
            ("111111", "211111"),
            ("000000", "000001"),
        ] {
            assert!(run(a, b).is_err(), "PIN {a} 与 {b} 只差一位，必须拒绝");
        }
    }

    /// 确认值必须区分方向，否则可以重放。
    #[test]
    fn confirmation_is_direction_bound() {
        let pin = "123456";
        let (pa, msg_a) = Pairing::start(pin, Role::Initiator).expect("start");
        let (pb, msg_b) = Pairing::start(pin, Role::Responder).expect("start");
        let (ca, conf_a) = pa.finish(&msg_b).expect("finish");
        let (_cb, _conf_b) = pb.finish(&msg_a).expect("finish");
        // 把 A 自己的确认值回给 A：必须拒绝
        assert!(
            ca.verify(&conf_a).is_err(),
            "自己的确认值不能通过自己的校验，否则可被反射攻击"
        );
    }

    #[test]
    fn generated_pin_is_well_formed() {
        for _ in 0..200 {
            let p = generate_pin();
            assert_eq!(p.chars().count(), PIN_DIGITS);
            assert!(p.chars().all(|c| c.is_ascii_digit()));
            assert!(validate_pin(&p).is_ok());
        }
    }

    /// 前导零必须保留，否则"输对了却配不上"。
    #[test]
    fn pin_keeps_leading_zeros() {
        let mut saw_leading_zero = false;
        for _ in 0..3000 {
            let p = generate_pin();
            assert_eq!(p.len(), PIN_DIGITS, "长度恒定，不能因数值小而变短");
            if p.starts_with('0') {
                saw_leading_zero = true;
            }
        }
        assert!(saw_leading_zero, "3000 次里应出现前导零（概率上必然）");
    }

    #[test]
    fn pin_digits_are_reasonably_uniform() {
        // 检查没有明显的模偏置：每个数字出现频率应接近 1/10
        let mut counts = [0usize; 10];
        let n = 6000;
        for _ in 0..n {
            for c in generate_pin().chars() {
                let d = c.to_digit(10).expect("应为数字") as usize;
                if let Some(slot) = counts.get_mut(d) {
                    *slot += 1;
                }
            }
        }
        let total: usize = counts.iter().sum();
        let expect = total / 10;
        for (d, &c) in counts.iter().enumerate() {
            // 用整数运算避免精度转换告警：判断 |c - expect| * 100 < expect * 15
            let diff = c.abs_diff(expect);
            assert!(
                diff * 100 < expect * 15,
                "数字 {d} 出现 {c} 次、期望约 {expect} 次，偏差过大，可能存在模偏置"
            );
        }
    }

    #[test]
    fn rejects_malformed_pin() {
        for bad in ["", "12345", "1234567", "12345a", "12 456", "１２３４５６"] {
            assert!(validate_pin(bad).is_err(), "应拒绝非法 PIN: {bad:?}");
            assert!(Pairing::start(bad, Role::Initiator).is_err());
        }
    }

    #[test]
    fn rejects_garbage_peer_message() {
        let (p, _) = Pairing::start("111111", Role::Initiator).expect("start");
        assert!(p.finish(&[0u8; 33]).is_err(), "垃圾消息应被拒绝");
    }

    #[test]
    fn derive_separates_purposes() {
        let (a, _b) = run("246810", "246810").expect("配对应成功");
        let k1 = a.derive(b"purpose-1");
        let k2 = a.derive(b"purpose-2");
        assert_ne!(k1, k2, "不同用途应派生出不同密钥");
        assert_eq!(k1, a.derive(b"purpose-1"), "同一用途应可重现");
    }

    /// 每次配对都应产生不同的密钥，即使 PIN 相同。
    #[test]
    fn each_session_is_fresh() {
        let (a1, _) = run("555555", "555555").expect("配对应成功");
        let (a2, _) = run("555555", "555555").expect("配对应成功");
        assert_ne!(
            a1.as_bytes(),
            a2.as_bytes(),
            "同一 PIN 的两次配对必须产生不同密钥，否则会话密钥可被重放"
        );
    }
}
