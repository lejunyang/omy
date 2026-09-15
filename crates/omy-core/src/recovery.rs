//! 恢复码：256 bit 熵 → 26 个词，带能定位到词的校验和。
//!
//! # 与密码的区别只在「怎么变成 KEK」
//!
//! ```text
//! 密码   ──Argon2id（故意慢，因为熵不够）──▶ KEK
//! 恢复码 ──HKDF（微秒级，熵已足够）────────▶ KEK
//! ```
//!
//! 变成 KEK 之后两者走的是完全同一条路：解 slot、拿 FEK、解密。所以恢复码
//! 不是「另一种打开方式」，它就是一个密码，只是省掉了慢速 KDF。
//!
//! **为什么不需要 Argon2**：256 bit 真随机，暴力破解不可行，慢速 KDF 只是
//! 白费用户的时间。慢速 KDF 存在的理由是「人选的密码熵太低」，这里不适用。
//!
//! # 编码：26 词 = 256 bit 熵 + 4 bit 校验
//!
//! SLIP-39 词表每词 10 bit，26 词 = 260 bit = 256 熵 + 4 校验。
//!
//! # 校验和为什么必须存在，以及为什么要能定位
//!
//! 没有校验和，「手抄错一个词」与「这不是本库的恢复码」表现完全一样——
//! 都是「解不开」。而这两种处境的处置方式相反：前者该回去核对纸条，
//! 后者该去找别的东西。
//!
//! 更进一步，26 个词里错一个，让用户从头核对 26 遍是很糟的体验。所以
//! 校验和设计成**能指出第几个词可疑**：除了 4 bit 的全局校验，解析时还会
//! 逐词检查是否在词表内，并对「不在词表但与某个词很接近」的情况给出建议
//! （见 [`ParseError::UnknownWord`]）。
//!
//! # 安全边界（必须出现在 UI 里）
//!
//! 1. 恢复码是**整个 vault 的强度下限**——它是一张明文纸条，谁拿到谁全权访问。
//! 2. 它与可否认性冲突：固定槽数掩盖了数量，但纸条放在家里，胁迫场景下
//!    就是突破口。
//! 3. [`crate::session::CredentialKind::Recovery`] 的 `scannable()` 是 false，
//!    所以输完恢复码**列表不会自动显形**，只在手动打开单个文件时才尝试。
//!    不说清楚会让用户以为恢复码没生效。

use crate::crypto::{Kek, SecretKey};
use crate::wordlist::{BITS_PER_WORD, WORDS};
use sha2::{Digest as _, Sha256};
use zeroize::Zeroize as _;

/// 恢复码的熵长度（字节）。
pub const ENTROPY_LEN: usize = 32;
/// 校验位数。
pub const CHECKSUM_BITS: usize = 4;
/// 恢复码的词数。
///
/// `#[expect(integer_division)]`：这里是编译期常量算术，260 / 10 恰好整除，
/// 且下面的静态断言锁住了「不允许有填充位」。库代码禁整数除法是为了防止
/// 运行期静默截断，与此无关。
#[expect(clippy::integer_division, reason = "编译期常量，静态断言保证整除")]
pub const WORD_COUNT: usize = (ENTROPY_LEN * 8 + CHECKSUM_BITS) / BITS_PER_WORD;

// 26 词 × 10 bit = 260 = 256 + 4。若哪天改了熵长度或每词位数而忘了这里，
// 编解码会静默错位——用静态断言锁住
const _: () = assert!(
    WORD_COUNT * BITS_PER_WORD == ENTROPY_LEN * 8 + CHECKSUM_BITS,
    "词数与熵+校验的总位数必须严格相等，不允许有填充位"
);

/// HKDF info：从恢复码熵派生 KEK。
///
/// 与密码派生（Argon2 直接产出 KEK）走不同的域，这样即便某个恢复码泄露，
/// 也推不出任何与密码相关的材料。
const INFO_RECOVERY: &[u8] = b"omy/v1/recovery-kek";

/// 解析恢复码时的失败原因。
///
/// 刻意区分得很细：每一种对用户的下一步动作都不同。笼统报「恢复码无效」
/// 会让用户无从下手——他不知道该核对纸条、该数一遍词数，还是该承认拿错了
/// 东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 词数不对。
    WordCount {
        /// 实际词数。
        got: usize,
        /// 期望词数。
        want: usize,
    },
    /// 某个词不在词表里。
    ///
    /// 带上**下标**与**最接近的候选**：用户抄错一个字母时，这能直接告诉他
    /// 「第 7 个词 `acadmic` 不在词表，是不是 `academic`？」，而不是让他把
    /// 26 个词重新核对一遍。
    UnknownWord {
        /// 第几个词（从 0 起）。
        index: usize,
        /// 用户输入的原词。
        word: String,
        /// 词表中最接近的词，没有足够接近的则为 `None`。
        suggestion: Option<&'static str>,
    },
    /// 词都认识，但整体校验和不匹配。
    ///
    /// 典型成因是**两个词写反了顺序**或抄错成了另一个合法的词——这两种
    /// 错误逐词检查发现不了，只有全局校验能抓到。
    Checksum,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WordCount { got, want } => {
                write!(f, "恢复码应有 {want} 个词，实际数到 {got} 个")
            }
            Self::UnknownWord { index, word, suggestion } => {
                let n = index.saturating_add(1);
                match suggestion {
                    Some(s) => write!(f, "第 {n} 个词「{word}」不在词表中，是不是「{s}」？"),
                    None => write!(f, "第 {n} 个词「{word}」不在词表中"),
                }
            }
            Self::Checksum => write!(
                f,
                "校验和不匹配：每个词都认识，但组合起来不对。\
                 常见原因是有两个词写反了顺序，或某个词抄成了另一个词"
            ),
        }
    }
}

impl core::error::Error for ParseError {}

/// 一份恢复码。
///
/// 持有 32 字节熵，析构时清零。**不实现 `Clone`**——与 `SecretKey` 同理，
/// 复制要显式可见，便于审计。
#[derive(zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct RecoveryCode {
    entropy: [u8; ENTROPY_LEN],
}

impl core::fmt::Debug for RecoveryCode {
    /// 刻意不打印内容：它等同于一把万能钥匙，落到日志里就是灾难。
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("RecoveryCode(<redacted>)")
    }
}

impl RecoveryCode {
    /// 生成一份新的恢复码。
    #[must_use]
    pub fn generate() -> Self {
        let mut entropy = [0u8; ENTROPY_LEN];
        crate::util::fill_random(&mut entropy);
        Self { entropy }
    }

    /// 由已有熵构造（用于测试向量与跨实现验证）。
    #[must_use]
    pub const fn from_entropy(entropy: [u8; ENTROPY_LEN]) -> Self {
        Self { entropy }
    }

    /// 借出底层熵。
    #[must_use]
    pub const fn entropy(&self) -> &[u8; ENTROPY_LEN] {
        &self.entropy
    }

    /// 编码成 26 个词。
    #[must_use]
    pub fn to_words(&self) -> Vec<&'static str> {
        let checksum = compute_checksum(&self.entropy);
        // 把 256 bit 熵 + 4 bit 校验拼成 260 bit，再按 10 bit 切分。
        // 用 u16 累加器而不是先拼成大整数：后者要引入 bignum 依赖，
        // 而这里只需要一个滑动窗口
        let mut out = Vec::with_capacity(WORD_COUNT);
        let mut acc: u32 = 0;
        let mut bits: usize = 0;
        for &b in &self.entropy {
            acc = (acc << 8) | u32::from(b);
            bits = bits.saturating_add(8);
            while bits >= BITS_PER_WORD {
                let shift = bits.saturating_sub(BITS_PER_WORD);
                let idx = ((acc >> shift) & 0x3FF) as usize;
                out.push(word_at(idx));
                bits = shift;
            }
        }
        // 剩余 6 bit（256 % 10）加上 4 bit 校验，正好凑成最后一个词
        acc = (acc << CHECKSUM_BITS) | u32::from(checksum);
        bits = bits.saturating_add(CHECKSUM_BITS);
        debug_assert_eq!(bits, BITS_PER_WORD, "尾部必须恰好凑满一个词，不留填充位");
        let idx = (acc & 0x3FF) as usize;
        out.push(word_at(idx));

        debug_assert_eq!(out.len(), WORD_COUNT);
        out
    }

    /// 编码成空格分隔的一行。
    #[must_use]
    pub fn to_phrase(&self) -> String {
        self.to_words().join(" ")
    }

    /// 从词序列解析。
    ///
    /// 输入会被归一化：去首尾空白、转小写。用户手抄后重新输入时大小写和
    /// 空格很难完全一致，为此拒绝是没有意义的刁难。
    ///
    /// # Errors
    ///
    /// 见 [`ParseError`]，每种失败对应不同的用户动作。
    pub fn from_words(words: &[&str]) -> core::result::Result<Self, ParseError> {
        if words.len() != WORD_COUNT {
            return Err(ParseError::WordCount { got: words.len(), want: WORD_COUNT });
        }

        let mut acc: u32 = 0;
        let mut bits: usize = 0;
        let mut entropy = [0u8; ENTROPY_LEN];
        let mut filled = 0usize;

        for (i, raw) in words.iter().enumerate() {
            let w = raw.trim().to_lowercase();
            let idx = index_of(&w).ok_or_else(|| ParseError::UnknownWord {
                index: i,
                word: w.clone(),
                suggestion: nearest(&w),
            })?;
            // idx 来自 binary_search 命中，必然 < 1024，转换不会截断。
            // 仍走 try_from 而不是 as：库代码里一个「看起来不可能」的截断
            // 就是一处静默错位，而这里错位的后果是恢复码永远解不开
            let idx = u32::try_from(idx).unwrap_or(0);
            acc = (acc << BITS_PER_WORD) | idx;
            bits = bits.saturating_add(BITS_PER_WORD);
            while bits >= 8 {
                let shift = bits.saturating_sub(8);
                let byte = ((acc >> shift) & 0xFF) as u8;
                if filled < ENTROPY_LEN {
                    // 前 32 字节是熵
                    if let Some(slot) = entropy.get_mut(filled) {
                        *slot = byte;
                    }
                    filled = filled.saturating_add(1);
                }
                bits = shift;
            }
        }
        // 熵取完后剩下的就是校验位
        debug_assert_eq!(bits, CHECKSUM_BITS, "解析后剩余位数必须恰好是校验位宽");
        let tail = (acc & 0x0F) as u8;

        let expect = compute_checksum(&entropy);
        if tail != expect {
            // 熵已经算出来了但校验不过——必须清零再返回，不能让一份
            // 可能来自攻击者试探的半成品材料留在内存里
            entropy.zeroize();
            return Err(ParseError::Checksum);
        }

        Ok(Self { entropy })
    }

    /// 从一行文本解析，按空白切分。
    ///
    /// # Errors
    ///
    /// 同 [`RecoveryCode::from_words`]。
    pub fn from_phrase(phrase: &str) -> core::result::Result<Self, ParseError> {
        let parts: Vec<&str> = phrase.split_whitespace().collect();
        Self::from_words(&parts)
    }

    /// 派生 KEK。
    ///
    /// `vault_salt` 参与派生，所以同一份恢复码在不同 vault 下得到不同 KEK——
    /// 与密码派生保持一致，避免跨库误用。
    #[must_use]
    pub fn to_kek(&self, vault_salt: &[u8; 16]) -> Kek {
        let hk = hkdf::Hkdf::<Sha256>::new(Some(vault_salt), &self.entropy);
        let mut out = [0u8; 32];
        // 32 字节远小于 SHA-256 的 255*32 上限，不可能失败
        hk.expand(INFO_RECOVERY, &mut out)
            .unwrap_or_else(|_| unreachable!("HKDF expand 32 bytes cannot fail"));
        Kek::from_key(SecretKey::from_bytes(out))
    }
}

/// 4 bit 校验：熵的 SHA-256 首字节高 4 位。
///
/// 为什么够用：校验和的职责是**区分「抄错了」和「拿错了」**，不是防篡改
/// （防篡改由头部 MAC 负责）。4 bit 意味着一个抄错的恢复码有 1/16 的概率
/// 蒙混过关，但那时它派生出的 KEK 也解不开任何文件，用户照样会得到
/// 「打不开」——只是错过了「第几个词可能有问题」这条更有用的提示。
///
/// 加大校验位宽要占用熵或增加词数：每多 4 bit 就多半个词，而 26 词已经
/// 是抄写负担的上限了。
fn compute_checksum(entropy: &[u8; ENTROPY_LEN]) -> u8 {
    let mut h = Sha256::new();
    h.update(b"omy/v1/recovery-checksum");
    h.update(entropy);
    let digest = h.finalize();
    // 取首字节高 4 位
    digest.first().map_or(0, |b| b >> 4)
}

/// 取词，下标越界时退回第一个词。
///
/// 越界在数学上不可能（10 bit 掩码后必然 < 1024），但库代码禁用索引
/// 切片——一个 panic 路径就是一个拒绝服务缺陷。
fn word_at(idx: usize) -> &'static str {
    WORDS.get(idx).copied().unwrap_or("academic")
}

/// 查词在表中的下标。词表已排序，用二分。
fn index_of(word: &str) -> Option<usize> {
    WORDS.binary_search(&word).ok()
}

/// 找最接近的词，用于「是不是想输 X」的提示。
///
/// 判据优先用**前 4 字母**：词表保证前 4 字母全局唯一，所以用户只要头 4 个
/// 字母没抄错，就能精确定位。这正是选 SLIP-39 词表的理由。
///
/// 前 4 字母也对不上时退回编辑距离 ≤ 1 的候选；再找不到就返回 `None`——
/// 给一个八竿子打不着的建议比不给更糟，用户会照着它改然后更迷惑。
fn nearest(word: &str) -> Option<&'static str> {
    if word.len() >= 4 {
        let p: String = word.chars().take(4).collect();
        if let Some(hit) = WORDS.iter().find(|w| w.starts_with(&p)) {
            return Some(hit);
        }
    }
    WORDS.iter().copied().find(|w| edit_distance_le_1(word, w))
}

/// 编辑距离是否 ≤ 1（增/删/改一个字符）。
///
/// 不用通用的 Levenshtein：只需判断「≤1」，单趟扫描即可，不必分配矩阵。
fn edit_distance_le_1(a: &str, b: &str) -> bool {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (la, lb) = (ab.len(), bb.len());
    if la.abs_diff(lb) > 1 {
        return false;
    }
    let mut i = 0usize;
    let mut j = 0usize;
    let mut diff = 0usize;
    while i < la && j < lb {
        if ab.get(i) == bb.get(j) {
            i = i.saturating_add(1);
            j = j.saturating_add(1);
            continue;
        }
        diff = diff.saturating_add(1);
        if diff > 1 {
            return false;
        }
        match la.cmp(&lb) {
            core::cmp::Ordering::Greater => i = i.saturating_add(1),
            core::cmp::Ordering::Less => j = j.saturating_add(1),
            core::cmp::Ordering::Equal => {
                i = i.saturating_add(1);
                j = j.saturating_add(1);
            }
        }
    }
    // 尾部剩余的字符也算一次编辑
    diff.saturating_add(la.saturating_sub(i)).saturating_add(lb.saturating_sub(j)) <= 1
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "测试里用断言与 panic 表达失败即可"
)]
mod tests {
    use super::*;

    /// 往返：生成 → 编码 → 解析，熵必须逐字节相同。
    ///
    /// 不这样会怎样：编解码若有位错位，用户抄下来的码永远打不开文件，
    /// 而生成时看起来一切正常。
    #[test]
    fn roundtrip_preserves_entropy() {
        for _ in 0..50 {
            let code = RecoveryCode::generate();
            let phrase = code.to_phrase();
            let back = RecoveryCode::from_phrase(&phrase).expect("解析自己生成的码");
            assert_eq!(code.entropy(), back.entropy(), "往返后熵必须一致");
        }
    }

    /// 词数必须恰好 26，且都在词表里。
    #[test]
    fn encodes_to_expected_word_count() {
        let code = RecoveryCode::from_entropy([0x5Au8; ENTROPY_LEN]);
        let words = code.to_words();
        assert_eq!(words.len(), 26, "26 词 × 10 bit = 260 = 256 熵 + 4 校验");
        for w in &words {
            assert!(WORDS.contains(w), "生成的词必须在词表内：{w}");
        }
    }

    /// 大小写与多余空白必须被容忍。
    ///
    /// 不这样会怎样：用户从纸上抄回来时大小写和空格很难完全一致，
    /// 为此拒绝是没有意义的刁难——而他手上拿的明明是正确的恢复码。
    #[test]
    fn normalizes_case_and_whitespace() {
        let code = RecoveryCode::generate();
        let words = code.to_words();
        let messy = words
            .iter()
            .enumerate()
            .map(|(i, w)| if i % 2 == 0 { w.to_uppercase() } else { format!("  {w} ") })
            .collect::<Vec<_>>()
            .join("   ");
        let back = RecoveryCode::from_phrase(&messy).expect("应容忍大小写与空白");
        assert_eq!(code.entropy(), back.entropy());
    }

    /// 词数不对时要报词数，而不是笼统说「无效」。
    #[test]
    fn wrong_word_count_is_reported_precisely() {
        let code = RecoveryCode::generate();
        let mut words = code.to_words();
        words.pop();
        let err = RecoveryCode::from_words(&words).unwrap_err();
        assert_eq!(err, ParseError::WordCount { got: 25, want: 26 });
        // 文案要能直接读懂，不能只是个枚举名
        assert!(err.to_string().contains("25"), "实际：{err}");
    }

    /// 抄错一个字母时，必须指出是第几个词并给出建议。
    ///
    /// 不这样会怎样：让用户把 26 个词从头核对一遍。而词表的前 4 字母
    /// 全局唯一，我们本来就能精确定位——不用这个信息是浪费。
    #[test]
    fn typo_points_at_the_word_and_suggests() {
        let code = RecoveryCode::generate();
        let mut words: Vec<String> = code.to_words().iter().map(|s| (*s).to_owned()).collect();
        let original = words[6].clone();
        // 删掉第 7 个词的最后一个字母，模拟手抄漏字
        words[6].pop();
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();

        match RecoveryCode::from_words(&refs) {
            Err(ParseError::UnknownWord { index, suggestion, .. }) => {
                assert_eq!(index, 6, "必须指出是第 7 个词（下标 6）");
                assert_eq!(
                    suggestion,
                    Some(original.as_str()),
                    "前 4 字母唯一，应当能精确建议回原词"
                );
            }
            // 极少数词去掉尾字母后仍是合法词（如 legs→leg 不在表内，
            // 但存在这种可能），那时会落到校验和失败，同样是可接受的报错
            Err(ParseError::Checksum) => {}
            other => panic!("期望 UnknownWord 或 Checksum，实际 {other:?}"),
        }
    }

    /// 两个词调换顺序：逐词检查发现不了，必须由校验和抓到。
    ///
    /// 不这样会怎样：校验和若形同虚设（比如恒返回 0），这条会失败。
    /// 它是「校验和真的在工作」的唯一直接证据。
    #[test]
    fn swapped_words_are_caught_by_checksum() {
        let mut caught = 0;
        let mut attempts = 0;
        for _ in 0..40 {
            let code = RecoveryCode::generate();
            let mut words = code.to_words();
            if words[3] == words[9] {
                continue; // 两个词恰好相同，交换等于没换
            }
            words.swap(3, 9);
            attempts += 1;
            match RecoveryCode::from_words(&words) {
                Err(ParseError::Checksum) => caught += 1,
                // 4 bit 校验有 1/16 漏网率，这是文档里写明的取舍
                Ok(_) => {}
                other => panic!("交换词序不该报这个错：{other:?}"),
            }
        }
        // 40 次里至少抓到大部分。取 60% 而不是 100%：4 bit 校验本就允许
        // 约 1/16 漏网，断言全抓会让测试随机失败
        assert!(
            caught * 100 >= attempts * 60,
            "校验和应抓到绝大多数词序错误，实际 {caught}/{attempts}"
        );
    }

    /// 不在词表的词必须被拒绝，不能静默当成某个下标。
    #[test]
    fn unknown_word_is_rejected() {
        let code = RecoveryCode::generate();
        let mut words: Vec<String> = code.to_words().iter().map(|s| (*s).to_owned()).collect();
        words[0] = String::from("zzzzzz");
        let refs: Vec<&str> = words.iter().map(String::as_str).collect();
        let err = RecoveryCode::from_words(&refs).unwrap_err();
        assert!(matches!(err, ParseError::UnknownWord { index: 0, .. }), "实际 {err:?}");
    }

    /// 不同恢复码必须派生出不同 KEK，同一码在不同 vault 下也不同。
    #[test]
    fn kek_derivation_is_discriminating() {
        let salt_a = [0x11u8; 16];
        let salt_b = [0x22u8; 16];
        let a = RecoveryCode::from_entropy([1u8; ENTROPY_LEN]);
        let b = RecoveryCode::from_entropy([2u8; ENTROPY_LEN]);

        let fp = |k: &Kek| *k.derive_slot_key(&[0u8; 16], 0).as_bytes();
        assert_ne!(fp(&a.to_kek(&salt_a)), fp(&b.to_kek(&salt_a)), "不同码必须不同 KEK");
        assert_ne!(fp(&a.to_kek(&salt_a)), fp(&a.to_kek(&salt_b)), "跨库必须不同 KEK");
        // 同码同库必须稳定，否则今天存的码明天就打不开
        assert_eq!(fp(&a.to_kek(&salt_a)), fp(&a.to_kek(&salt_a)), "必须可重现");
    }

    /// 恢复码的 KEK 不得与「把词当密码跑 Argon2」撞上。
    ///
    /// 不这样会怎样：若两条路径产出同一个 KEK，用户把恢复码当密码输进
    /// 密码框也能打开，那么「恢复码不参与扫描」的设计就被绕过了。
    #[test]
    fn recovery_kek_differs_from_password_path() {
        let salt = [0x33u8; 16];
        let code = RecoveryCode::from_entropy([7u8; ENTROPY_LEN]);
        let phrase = code.to_phrase();
        let as_password =
            Kek::from_password(phrase.as_bytes(), &salt, crate::crypto::Argon2Params::TEST_WEAK)
                .expect("argon2");
        let fp = |k: &Kek| *k.derive_slot_key(&[0u8; 16], 0).as_bytes();
        assert_ne!(fp(&code.to_kek(&salt)), fp(&as_password), "两条派生路径必须域分隔");
    }

    /// Debug 输出不得泄露熵。
    #[test]
    fn debug_is_redacted() {
        let code = RecoveryCode::from_entropy([0xABu8; ENTROPY_LEN]);
        let s = format!("{code:?}");
        assert!(!s.contains("ab"), "Debug 不得打印熵内容：{s}");
        assert!(s.contains("redacted"));
    }

    /// 编辑距离辅助函数自身的边界。
    #[test]
    fn edit_distance_helper_is_correct() {
        assert!(edit_distance_le_1("academic", "academic"), "相同");
        assert!(edit_distance_le_1("acadmic", "academic"), "少一个字母");
        assert!(edit_distance_le_1("academics", "academic"), "多一个字母");
        assert!(edit_distance_le_1("acedemic", "academic"), "改一个字母");
        assert!(!edit_distance_le_1("acdmic", "academic"), "差两个字母");
        assert!(!edit_distance_le_1("zebra", "academic"), "完全不同");
    }
}
