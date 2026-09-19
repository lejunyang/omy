//! 从 Telegram Desktop 的 `tdata` 目录导入登录态。
//!
//! # 参考实现与署名
//!
//! 本模块的格式解析参考了 [`gotd/td`](https://github.com/gotd/td) 的
//! `session/tdesktop` 包（`qt.go` / `tdesktop.go` / `mtp_authorization.go`），
//! 该项目以 MIT 许可发布：
//!
//! > Copyright (c) 2020 Aleksandr Razumov
//!
//! **没有**参考 `iyear/tdl`（AGPL-3.0）的任何代码。
//!
//! # 为什么值得做
//!
//! 扫码登录每次都要用手机扫一遍，而 Telegram 对频繁登录会限流。桌面端已经
//! 登录过的机器上，`tdata` 里就有可直接复用的 auth key。
//!
//! # 这个格式最容易错的地方：混合字节序
//!
//! 同一个文件里两种字节序都有，而且读错**不会立刻报错**，只会解出一堆乱码，
//! 症状指向「密码不对」或「文件损坏」，完全指不到字节序。四处逐一标注：
//!
//! | 位置 | 字节序 |
//! |---|---|
//! | TDF 尾部 MD5 里的长度字段 | **小端** |
//! | 解密后明文开头的 `fullLen` | **小端** |
//! | 其余一切（数组长度、blockId、dcId、userId…） | **大端** |
//!
//! # 文件命名：只有账号数据用哈希
//!
//! `key_datas`、`settingss`、`usertag`、`prefix` 都是**明文名 + 后缀**；
//! 只有账号数据文件用 [`file_key`] 哈希过的名字。
//!
//! 实测确认：`file_key("key_data")` = `BF1A4223EDEEE925`，而真实样本里
//! 那三个后缀的文件**一个都不存在**，存在的是 `key_datas`。把 `key_data`
//! 也拿去哈希的话，程序会找一个不存在的文件然后报「没有 tdata」——
//! 而 tdata 明明就在那儿，症状完全指不到命名规则。

use std::path::{Path, PathBuf};

/// 解析 tdata 时可能出的问题。
///
/// 分得细是有意的：**「解析失败」与「登录态失效」必须分开**。前者是 omy 的
/// 问题（或格式变了），后者是那份 tdata 本身已经不能用了——给用户的话完全
/// 不同，一个是「换个方式登录」，一个是「这台机器上的桌面端也得重新登录」。
#[derive(Debug, thiserror::Error)]
pub enum TdataError {
    /// 目录不存在或不像 tdata。
    #[error("这个目录里没有 Telegram Desktop 的登录数据")]
    NotTdata,
    /// 找不到 `key_datas`。
    #[error("找不到 key_data 文件，这个 tdata 可能不完整")]
    NoKeyData,
    /// 找不到账号数据文件。
    #[error("找不到账号数据文件")]
    NoAccount,
    /// TDF 容器坏了（魔数 / MD5 / 长度）。
    #[error("文件已损坏：{0}")]
    Corrupt(&'static str),
    /// 需要本地密码（Telegram Desktop 的 passcode），但没提供或不对。
    ///
    /// **与云密码（两步验证）完全不是一回事**，界面文案必须分开——
    /// 用户输错了对象会一直试不对，而且不知道自己在试错东西。
    #[error("这份 tdata 设了本地密码，需要先输入它")]
    NeedPasscode,
    /// 本地密码不对。
    #[error("本地密码不正确")]
    WrongPasscode,
    /// 格式里遇到了没见过的东西。
    ///
    /// 带上一句人能看懂的说明，而不是一个偏移量——偏移量对用户毫无意义，
    /// 而这条是会被显示出来的。
    #[error("无法解析这份 tdata：{0}")]
    Unsupported(String),
    /// 读文件失败。
    #[error("读取失败：{0}")]
    Io(String),
}

/// 一个账号的 MTProto 授权信息。
///
/// **刻意不派生 `Debug`**：里面是 auth key，等同账号凭据。派生了的话，
/// 一句 `dbg!` 或一条错误日志就能把它写进日志文件——这与 `SavedSession`
/// 不派生 Debug 是同一个理由。
pub struct MtpAuthorization {
    /// 用户 id。
    pub user_id: u64,
    /// 主数据中心。
    pub main_dc: i32,
    /// 各数据中心的 auth key：`(dc_id, 256 字节密钥)`。
    pub keys: Vec<(i32, [u8; 256])>,
}

impl MtpAuthorization {
    /// 主 DC 上的那把 key。
    #[must_use]
    pub fn main_key(&self) -> Option<&[u8; 256]> {
        self.keys.iter().find(|(dc, _)| *dc == self.main_dc).map(|(_, k)| k)
    }
}

/// Telegram Desktop 的文件名哈希。
///
/// MD5 之后**每个字节高低 4 位互换**，取大写 hex 的前 16 个字符。
///
/// 那个半字节互换最容易漏——漏了会算出一个完全不同的名字，而那个文件不存在，
/// 于是表现成「找不到账号」，症状指不到哈希实现。
#[must_use]
pub fn file_key(name: &str) -> String {
    use md5::{Digest as _, Md5};
    let digest = Md5::digest(name.as_bytes());
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        // 高低 4 位互换后再转 hex
        let swapped = ((b & 0x0F) << 4) | (b >> 4);
        out.push_str(&format!("{swapped:02X}"));
    }
    out
}

/// 按 Telegram Desktop 的后缀优先级找一个文件。
///
/// 优先级 `s` → `0` → `1`。**顺序反了会读到迁移前的陈旧文件**——那个文件
/// 能解开、内容也像模像样，只是属于上一次登录，于是拿到一把已经失效的 key，
/// 而错误要等到真正发请求时才出现。
#[must_use]
pub fn resolve_suffixed(dir: &Path, base: &str) -> Option<PathBuf> {
    for suffix in ['s', '0', '1'] {
        let p = dir.join(format!("{base}{suffix}"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// 一个 TDF 容器解开后的内容。
pub struct Tdf {
    /// 数据区（已去掉头部与尾部 MD5）。
    pub data: Vec<u8>,
    /// 写入它的 Telegram Desktop 版本号。
    pub version: u32,
}

/// 解析 TDF 容器。
///
/// 布局：`"TDF$"` + `version(4, 小端)` + `data` + `md5(16)`。
///
/// MD5 覆盖的是 `data ‖ len(data) as u32 小端 ‖ version ‖ "TDF$"`。
/// **那个长度字段是小端**，而文件里其余的数组长度是大端——顺序或字节序错了
/// 校验必然失败，而失败信息只会说「文件损坏」，指不到是自己算错了。
///
/// # Errors
///
/// 魔数不对、长度不足、MD5 对不上时返回。
pub fn parse_tdf(raw: &[u8]) -> Result<Tdf, TdataError> {
    // 4 魔数 + 4 版本 + 16 MD5 = 24，数据可以为空但头尾必须齐
    if raw.len() < 24 {
        return Err(TdataError::Corrupt("文件太短"));
    }
    if &raw[..4] != b"TDF$" {
        return Err(TdataError::Corrupt("不是 Telegram 的数据文件"));
    }
    let version = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]);
    let data = &raw[8..raw.len() - 16];
    let want = &raw[raw.len() - 16..];

    use md5::{Digest as _, Md5};
    let mut h = Md5::new();
    h.update(data);
    // 小端。这一处与文件里其余的大端数组长度不同，是最容易写反的地方之一
    h.update(u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    h.update(version.to_le_bytes());
    h.update(b"TDF$");
    if h.finalize().as_slice() != want {
        return Err(TdataError::Corrupt("校验和不匹配，文件可能被改动过"));
    }

    Ok(Tdf {
        data: data.to_vec(),
        version,
    })
}

/// 从缓冲区按大端读一个数组（4 字节长度 + 内容）。
///
/// Qt 的 `QDataStream` 默认是大端，Telegram Desktop 直接用了它。
///
/// # Errors
///
/// 长度字段读不出、或声称的长度超过剩余字节时返回。
pub fn read_array_be<'a>(buf: &mut &'a [u8]) -> Result<&'a [u8], TdataError> {
    if buf.len() < 4 {
        return Err(TdataError::Corrupt("数组长度字段被截断"));
    }
    let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    // 0xFFFFFFFF 是 Qt 里 QByteArray 的 null 标记，不是一个 4 GiB 的数组。
    // 前一次尝试解析就是在这里把它当成长度，于是偏移全错、往后读全是乱码
    if n == 0xFFFF_FFFF {
        return Err(TdataError::Unsupported(String::from(
            "遇到了空数组标记，这里的结构与预期不同",
        )));
    }
    if buf.len() < 4 + n {
        return Err(TdataError::Corrupt("数组内容被截断"));
    }
    let out = &buf[4..4 + n];
    *buf = &buf[4 + n..];
    Ok(out)
}

/// 读一个大端 `u32`。
///
/// # Errors
///
/// 剩余字节不足时返回。
pub fn read_u32_be(buf: &mut &[u8]) -> Result<u32, TdataError> {
    if buf.len() < 4 {
        return Err(TdataError::Corrupt("整数字段被截断"));
    }
    let v = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    *buf = &buf[4..];
    Ok(v)
}

/// 由 passcode 与 salt 派生出解 `localKey` 用的密钥。
///
/// `PBKDF2-SHA512(SHA512(salt ‖ passcode ‖ salt), salt, iters, 256)`。
///
/// **空 passcode 用 1 次迭代，非空才用 100000 次。** 这不是优化，是格式规定——
/// 用错次数会解不开，而错误表现为「密码不对」，让人去怀疑密码。
#[must_use]
pub fn derive_passcode_key(passcode: &[u8], salt: &[u8]) -> [u8; 256] {
    use hmac::Hmac;
    use sha2::{Digest as _, Sha512};

    let mut h = Sha512::new();
    h.update(salt);
    h.update(passcode);
    h.update(salt);
    let seed = h.finalize();

    let iters: u32 = if passcode.is_empty() { 1 } else { 100_000 };
    let mut out = [0u8; 256];
    // pbkdf2 的返回值只在算法参数非法时才是 Err，这里参数是固定的；
    // 真出错也只会得到全零，下游的 SHA1 校验会拦住
    let _ = pbkdf2::pbkdf2::<Hmac<Sha512>>(&seed, salt, iters, &mut out);
    out
}

/// 解开 Telegram Desktop 的「本地加密」块。
///
/// 布局：`sha1 前 16 字节做 message key` + 密文。密钥调度与 MTProto 的
/// `auth_key` 派生同构，但**用的是 SHA1 且覆盖整个缓冲区**。
///
/// # Errors
///
/// 长度非法、或解出来的 SHA1 对不上（= 密码不对）时返回。
pub fn decrypt_local(encrypted: &[u8], key: &[u8; 256]) -> Result<Vec<u8>, TdataError> {
    use sha1::{Digest as _, Sha1};

    if encrypted.len() < 16 || (encrypted.len() - 16) % 16 != 0 {
        return Err(TdataError::Corrupt("加密块长度非法"));
    }
    let (msg_key, body) = encrypted.split_at(16);

    // 密钥调度与加密侧共用一份（prepare_aes_old）：两边各写一遍的话，
    // 写岔了 round-trip 反而会「通过」——两边用同一个错误算法，
    // 自己跟自己对得上，却解不开真实文件
    let (aes_key, aes_iv) = prepare_aes_old(msg_key, key);

    let mut buf = body.to_vec();
    grammers_crypto::aes::ige_decrypt(&mut buf, &aes_key, &aes_iv);

    // 校验：SHA1 覆盖**整个**解出来的缓冲区（含那个 fullLen 前缀），
    // 不是只覆盖去掉前缀之后的部分。只取前 16 字节与 msg_key 比。
    let check = Sha1::digest(&buf);
    if check[0..16] != *msg_key {
        return Err(TdataError::WrongPasscode);
    }
    Ok(buf)
}

/// 按 Telegram Desktop 的本地加密格式加密一段数据。
///
/// **只给测试用**：产品从不往 tdata 里写东西（那是别人的数据）。
/// 它存在的唯一理由是让 [`decrypt_local`] 能被加解密来回验证——
/// 否则解不开真实文件时，无法区分「实现有 bug」与「真的设了密码」，
/// 只能在两者之间反复猜。
#[cfg(test)]
fn encrypt_local(plain: &[u8], key: &[u8; 256]) -> Vec<u8> {
    use sha1::{Digest as _, Sha1};

    // 补齐到 16 的倍数
    let mut buf = plain.to_vec();
    while buf.len() % 16 != 0 {
        buf.push(0);
    }
    // msgKey = SHA1(整个缓冲区) 的前 16 字节
    let msg_key: [u8; 16] = Sha1::digest(&buf)[0..16]
        .try_into()
        .unwrap_or([0u8; 16]);
    let (aes_key, aes_iv) = prepare_aes_old(&msg_key, key);
    grammers_crypto::aes::ige_encrypt(&mut buf, &aes_key, &aes_iv);

    let mut out = Vec::with_capacity(16 + buf.len());
    out.extend_from_slice(&msg_key);
    out.extend_from_slice(&buf);
    out
}

/// 由 `msgKey` 与 `localKey` 推出 AES 的 key 与 iv。
///
/// 这是 Telegram Desktop 的 `prepareAES_oldmtp`。抽成函数让加密与解密
/// 共用同一份，否则两边各写一遍、写岔了 round-trip 反而会「通过」。
///
/// # 那个 `x = 8` 偏移量
///
/// 所有切片都从 `localKey` 的第 **8** 个字节起算，不是第 0 个。
/// MTProto 的老式 KDF 里 `x` 按方向取 0 或 8，本地存储解密用的是 8。
///
/// 实测判定（真实样本，判据是格式自带的 SHA1 自校验）：
/// `x=0` 校验失败、`x=8` 校验通过；对照组故意给错密码时两个 `x` 都失败，
/// 所以 8 不是碰巧撞上的。
///
/// **写成 0 的话 round-trip 测试照样过**——加密与解密两侧共用本函数，
/// 两边用同一个错误算法自己跟自己对得上，却解不开任何真实文件。
/// 所以另有一条已知答案向量（KAT）钉住这个 8。
///
/// 切片长度照参考实现，逐个标出来（写错任何一个都解不开，
/// 而症状是「密码不正确」，让人去怀疑用户的密码）：
/// ```text
/// sha1_a = SHA1(msgKey ‖ key[x   .. x+32])
/// sha1_b = SHA1(key[32+x .. 32+x+16] ‖ msgKey ‖ key[48+x .. 48+x+16])
/// sha1_c = SHA1(key[64+x .. 64+x+32] ‖ msgKey)
/// sha1_d = SHA1(msgKey ‖ key[96+x .. 96+x+32])
///
/// aes_key = sha1_a[0..8] ‖ sha1_b[8..20] ‖ sha1_c[4..16]
/// aes_iv  = sha1_a[8..20] ‖ sha1_b[0..8] ‖ sha1_c[16..20] ‖ sha1_d[0..8]
/// ```
fn prepare_aes_old(msg_key: &[u8], key: &[u8; 256]) -> ([u8; 32], [u8; 32]) {
    use sha1::{Digest as _, Sha1};

    // 见函数文档：这个 8 是实测钉死的，改成 0 会让所有真实文件解不开
    const X: usize = 8;

    let mut data_a = Vec::with_capacity(16 + 32);
    data_a.extend_from_slice(msg_key);
    data_a.extend_from_slice(&key[X..X + 32]);

    let mut data_b = Vec::with_capacity(16 + 16 + 16);
    data_b.extend_from_slice(&key[32 + X..32 + X + 16]);
    data_b.extend_from_slice(msg_key);
    data_b.extend_from_slice(&key[48 + X..48 + X + 16]);

    let mut data_c = Vec::with_capacity(32 + 16);
    data_c.extend_from_slice(&key[64 + X..64 + X + 32]);
    data_c.extend_from_slice(msg_key);

    let mut data_d = Vec::with_capacity(16 + 32);
    data_d.extend_from_slice(msg_key);
    data_d.extend_from_slice(&key[96 + X..96 + X + 32]);

    let sha_a = Sha1::digest(&data_a);
    let sha_b = Sha1::digest(&data_b);
    let sha_c = Sha1::digest(&data_c);
    let sha_d = Sha1::digest(&data_d);

    let mut aes_key = [0u8; 32];
    aes_key[0..8].copy_from_slice(&sha_a[0..8]);
    aes_key[8..20].copy_from_slice(&sha_b[8..20]);
    aes_key[20..32].copy_from_slice(&sha_c[4..16]);

    let mut aes_iv = [0u8; 32];
    aes_iv[0..12].copy_from_slice(&sha_a[8..20]);
    aes_iv[12..20].copy_from_slice(&sha_b[0..8]);
    aes_iv[20..24].copy_from_slice(&sha_c[16..20]);
    aes_iv[24..32].copy_from_slice(&sha_d[0..8]);

    (aes_key, aes_iv)
}

/// 去掉解密后明文开头的 `fullLen` 前缀。
///
/// **那 4 个字节是小端**（与文件里其余的大端数组长度相反），而且它包含自身
/// 这 4 个字节。读错的话后面所有偏移都会错位，而症状是「解析失败」，
/// 指不到是这里少算或多算了 4 个字节。
///
/// # Errors
///
/// 长度字段不合理时返回。
pub fn strip_full_len(plain: &[u8]) -> Result<&[u8], TdataError> {
    if plain.len() < 4 {
        return Err(TdataError::Corrupt("明文太短"));
    }
    let full = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]) as usize;
    if full < 4 || full > plain.len() {
        return Err(TdataError::Corrupt("明文长度字段不合理"));
    }
    Ok(&plain[4..full])
}

/// `dbiMtpAuthorization` 的 key。
///
/// **是 `0x4B`（75），不是 46。** 前一次尝试按 46 找，于是把真正的目标
/// 当成了「未知 block」跳过去，然后在后面的字节里越走越偏。
pub const DBI_MTP_AUTHORIZATION: u32 = 0x4B;

/// 「宽 id」哨兵。
///
/// 授权文件里先读两个 `u32`，拼成 `u64` 若等于这个值，说明后面跟的是
/// 新格式（`u64` userId + `u32` mainDc）；否则是旧格式（两个 u32 分别是
/// userId 与 mainDc）。
///
/// **漏判会把这个哨兵当成真的 user_id**，得到一个荒谬的数字，而且后面
/// 所有字段都会错位。
const WIDE_ID_SENTINEL: u64 = 0xFFFF_FFFF_FFFF_FFFF;

/// 解析 `dbiMtpAuthorization` 的内容。
///
/// # Errors
///
/// 第一个 block 不是 `dbiMtpAuthorization`、或结构与预期不符时返回。
pub fn parse_mtp_authorization(plain: &[u8]) -> Result<MtpAuthorization, TdataError> {
    let mut buf = plain;

    // 授权文件是**专用单块文件**：第一个 u32 必须就是 dbiMtpAuthorization。
    // 不要去遍历 block 链——那 88 个 case 属于 settings，与这里无关。
    // 不等就直接失败，别猜。
    let block_id = read_u32_be(&mut buf)?;
    if block_id != DBI_MTP_AUTHORIZATION {
        return Err(TdataError::Unsupported(format!(
            "账号文件的第一个数据块是 {block_id:#x}，不是预期的授权信息"
        )));
    }

    let serialized = read_array_be(&mut buf)?;
    let mut s = serialized;

    // 身份：先读两个 u32 拼成 u64 看是不是宽 id 哨兵
    let hi = read_u32_be(&mut s)?;
    let lo = read_u32_be(&mut s)?;
    let combined = (u64::from(hi) << 32) | u64::from(lo);
    let (user_id, main_dc) = if combined == WIDE_ID_SENTINEL {
        // 新格式：u64 userId + u32 mainDc
        if s.len() < 12 {
            return Err(TdataError::Corrupt("账号标识被截断"));
        }
        let uid = u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]);
        s = &s[8..];
        let dc = read_u32_be(&mut s)? as i32;
        (uid, dc)
    } else {
        // 旧格式：两个 u32 就是 userId 与 mainDc
        (u64::from(hi), lo as i32)
    };

    let count = read_u32_be(&mut s)? as usize;
    // 上限兜底：一个账号不可能有几万个 DC。不设上限的话，一个损坏的计数
    // 会让我们试图分配巨大的 Vec
    if count > 64 {
        return Err(TdataError::Unsupported(format!(
            "账号文件里声称有 {count} 个密钥，这不合理"
        )));
    }

    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let dc = read_u32_be(&mut s)? as i32;
        // key 是**裸的 256 字节，没有长度前缀**。
        // 按「长度 + 数据」去读必然找不到——前一次尝试就卡在这里。
        if s.len() < 256 {
            return Err(TdataError::Corrupt("密钥被截断"));
        }
        let mut k = [0u8; 256];
        k.copy_from_slice(&s[..256]);
        s = &s[256..];
        keys.push((dc, k));
    }

    if keys.is_empty() {
        return Err(TdataError::Unsupported(String::from(
            "账号文件里没有任何密钥",
        )));
    }

    Ok(MtpAuthorization {
        user_id,
        main_dc,
        keys,
    })
}

/// 读一个 tdata 目录里的第一个账号。
///
/// # 用户从哪一步点到它（先画路径再看实现）
///
/// ```text
/// 侧栏 ✈️ → 登录对话框 → 「从 Telegram Desktop 导入」
///   → 自动探测常见位置；探测不到就**就地给路径选择器**（便携版必须手动指定）
///   → 若这份 tdata 设了本地密码：要一个输入框，文案必须写「本地密码」
///     而不是「两步验证密码」——用户输错对象会一直试不对且不知道在试错东西
///   → 导入成功 → 与扫码同一条路：落盘 session → 接成远程位置
/// ```
///
/// # 只取第一个账号
///
/// 多账号（`data#2` / `data#3`）**没有样本**，所以这里只处理主账号，
/// 遇到多账号时如实说明而不是猜。见 [`read_tdata`] 的多账号分支。
///
/// # Errors
///
/// 见 [`TdataError`]。特别地，「需要本地密码」与「本地密码不对」是两个不同
/// 的错误，界面据此决定是**要一次输入**还是**说密码错了**。
pub fn read_tdata(dir: &Path, passcode: &str) -> Result<MtpAuthorization, TdataError> {
    if !dir.is_dir() {
        return Err(TdataError::NotTdata);
    }

    // ---- 1. key_data：**明文名**，不哈希 ----
    //
    // 实测确认：file_key("key_data") = BF1A4223EDEEE925，而真实样本里那三个
    // 后缀的文件一个都不存在；存在的是 key_datas。把它也拿去哈希的话，
    // 程序会找一个不存在的文件然后报「没有 tdata」，而 tdata 明明就在那儿。
    let kd_path = resolve_suffixed(dir, "key_data").ok_or(TdataError::NoKeyData)?;
    let kd_raw = std::fs::read(&kd_path).map_err(|e| TdataError::Io(e.to_string()))?;
    let kd = parse_tdf(&kd_raw)?;

    let mut buf = kd.data.as_slice();
    // salt：大端数组，必须正好 32 字节
    let salt = read_array_be(&mut buf)?;
    if salt.len() != 32 {
        return Err(TdataError::Unsupported(format!(
            "盐的长度是 {}，预期 32",
            salt.len()
        )));
    }
    let key_encrypted = read_array_be(&mut buf)?;
    let info_encrypted = read_array_be(&mut buf)?;

    // ---- 2. 解 localKey ----
    let passcode_key = derive_passcode_key(passcode.as_bytes(), salt);
    let key_plain = match decrypt_local(key_encrypted, &passcode_key) {
        Ok(v) => v,
        // 解不开且用户没给密码 → 多半是设了本地密码。
        // 这两种要分开报：一个是「请输入本地密码」，一个是「密码不对」，
        // 合并成一句的话，没设密码的人会被要求输入一个不存在的密码
        Err(TdataError::WrongPasscode) if passcode.is_empty() => {
            return Err(TdataError::NeedPasscode);
        }
        Err(e) => return Err(e),
    };

    // 剥掉 fullLen 之后**直接就是** 256 字节的裸密钥，没有内层数组。
    //
    // 实测（真实样本）：明文总长 272 B（含 AES 补齐）、fullLen = 260 且含
    // 自身那 4 字节、剥完正好 256 B。若按「内层还有一个小端数组长度」去读，
    // 头 4 字节会被解读成 33 亿，然后报「长度超出内容」——
    // 症状指向用户的文件，而真正的问题在这里。
    let key_body = strip_full_len(&key_plain)?;
    if key_body.len() < 256 {
        return Err(TdataError::Unsupported(format!(
            "localKey 只有 {} 字节，预期 256",
            key_body.len()
        )));
    }
    let mut local_key = [0u8; 256];
    local_key.copy_from_slice(&key_body[..256]);

    // ---- 3. 账号列表 ----
    //
    // 解不开这一段不致命：老版本可能没有它，此时按单账号处理。
    let idx = decrypt_local(info_encrypted, &local_key)
        .ok()
        .and_then(|p| strip_full_len(&p).ok().map(<[u8]>::to_vec))
        .and_then(|body| {
            let mut b = body.as_slice();
            // 账号索引列表：count 后面跟 count 个 u32（大端）
            let count = read_u32_be(&mut b).ok()?;
            let mut out = Vec::new();
            for _ in 0..count.min(64) {
                out.push(read_u32_be(&mut b).ok()?);
            }
            Some(out)
        })
        .unwrap_or_default();

    // 多账号没有样本，只取主账号，并在日志里如实说明。
    // 不猜它们的结构——猜错的话会拿到一把属于别的账号的 key
    if idx.len() > 1 {
        eprintln!(
            "[omy] 这份 tdata 里有 {} 个账号，当前只导入主账号（多账号未经真实样本验证）",
            idx.len()
        );
    }
    let first = idx.first().copied().unwrap_or(0);

    // ---- 4. 账号数据文件：**这个才用 file_key 哈希** ----
    let name = if first == 0 {
        String::from("data")
    } else {
        format!("data#{}", first + 1)
    };
    let acc_path = resolve_suffixed(dir, &file_key(&name)).ok_or(TdataError::NoAccount)?;
    let acc_raw = std::fs::read(&acc_path).map_err(|e| TdataError::Io(e.to_string()))?;
    let acc = parse_tdf(&acc_raw)?;

    let mut b = acc.data.as_slice();
    let encrypted = read_array_be(&mut b)?;
    let plain = decrypt_local(encrypted, &local_key)?;
    let body = strip_full_len(&plain)?;

    parse_mtp_authorization(body)
}

/// 把 tdata 里的授权信息转成 omy 的落盘登录态。
///
/// # 为什么地址要从 `SessionData::default()` 取
///
/// tdata 里只有 `(dcId, authKey)`，**没有数据中心的 IP**。grammers 自带
/// 一份主数据中心地址表，直接用它，而不是自己抄一份 IP 进代码——
/// 抄的那份不会跟着上游更新，等 Telegram 换了地址就连不上，
/// 而症状是「网络超时」，完全指不到一张过期的硬编码表。
///
/// # 只带 auth key 的那些 DC
///
/// tdata 里没有的 DC 不放进去。放一个没有 key 的进去不会报错，
/// 但它对「免去重新登录」毫无帮助，只会让 `has_auth_key` 之类的判断更难读。
///
/// # 这不等于「登录态有效」
///
/// 转换成功只说明格式解析对了。那份 tdata 可能是几个月前写的，
/// 服务端早已把它注销。**必须真的发一次请求才知道**，见调用方。
#[must_use]
pub fn to_saved_session(auth: &MtpAuthorization, api_id: i32) -> crate::telegram::session::SavedSession {
    let defaults = grammers_session::SessionData::default();
    let mut dc_options = Vec::new();
    for (dc_id, key) in &auth.keys {
        // 地址表里没有的 DC 直接跳过：那多半是个媒体专用 DC 或者
        // 上游还不认识的编号，硬塞一个假地址进去只会在连接时超时
        if let Some(base) = defaults.dc_options.get(dc_id) {
            dc_options.push(grammers_session::types::DcOption {
                id: *dc_id,
                ipv4: base.ipv4,
                ipv6: base.ipv6,
                auth_key: Some(*key),
            });
        }
    }
    crate::telegram::session::SavedSession {
        api_id,
        home_dc: auth.main_dc,
        dc_options,
    }
}

/// 常见的 tdata 位置。
///
/// **便携版不在这里面**——它跟着 exe 走，可能在任何地方，所以界面必须
/// 提供手动指定（§12.3.4 的第一条前提）。只靠自动探测的话，便携版用户
/// 会看到一句「没找到」然后无路可走。
#[must_use]
pub fn common_locations() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        out.push(Path::new(&appdata).join("Telegram Desktop").join("tdata"));
    }
    out.retain(|p| p.is_dir());
    out
}

/// 这个目录看起来像 tdata 吗（只看结构，不解密）。
///
/// 给界面做「选对目录了吗」的即时反馈用：选错目录时当场说，
/// 而不是等用户点了导入、跑完一轮解密才报错。
#[must_use]
pub fn looks_like_tdata(dir: &Path) -> bool {
    dir.is_dir() && resolve_suffixed(dir, "key_data").is_some()
}

/// 枚举一个 Telegram Desktop 安装目录下的 tdata 候选。
///
/// **排除 `tupdates`**：那底下是更新器解压出来的临时副本，里面也有一个
/// 形似 tdata 的目录，但它不含真实登录态。把它列出来的话，用户会看到一个
/// 选了必然失败的「幽灵账号」，而失败原因完全无从解释。
#[must_use]
pub fn scan_install_dir(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if looks_like_tdata(&root.join("tdata")) {
        out.push(root.join("tdata"));
    }
    if looks_like_tdata(root) {
        out.push(root.to_path_buf());
    }
    // tupdates 下面的一律不要，理由见函数文档
    out.retain(|p| {
        !p.components().any(|c| c.as_os_str() == "tupdates")
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一份 `dbiMtpAuthorization` 的**合成**序列化数据。
    ///
    /// 结构照 gotd/td 的 `mtp_authorization_test.go`（MIT,
    /// Copyright (c) 2020 Aleksandr Razumov）里那份向量：
    /// 宽 id 哨兵 + u64 userId + u32 mainDc + count + count×(dcId ‖ 256 字节裸 key)。
    ///
    /// **合成而不是用真实 tdata**：真实样本只有一份、含真账号凭据、
    /// 而且别的机器上根本没有。绑死在它上面的话，CI 与任何没有 tdata 的
    /// 开发机都跑不了这条回归。
    fn synth_authorization(user_id: u64, main_dc: i32, dcs: &[i32]) -> Vec<u8> {
        let mut inner = Vec::new();
        // 宽 id 哨兵：两个 u32 拼起来是 0xFFFF_FFFF_FFFF_FFFF
        inner.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        inner.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        inner.extend_from_slice(&user_id.to_be_bytes());
        inner.extend_from_slice(&(main_dc as u32).to_be_bytes());
        inner.extend_from_slice(&u32::try_from(dcs.len()).unwrap_or(0).to_be_bytes());
        for (i, dc) in dcs.iter().enumerate() {
            inner.extend_from_slice(&(*dc as u32).to_be_bytes());
            // 每把 key 用算式生成而不是 [0xAA; 256]：
            // 长串重复字节会让本机安全软件把测试二进制判成可疑并删掉
            // （AGENTS.md 记过这条），而且各不相同更容易测出错位
            let key: Vec<u8> = (0..256)
                .map(|j| (j as u8).wrapping_mul(7) ^ (i as u8).wrapping_mul(31))
                .collect();
            inner.extend_from_slice(&key);
        }

        // 外层：blockId + 大端数组
        let mut out = Vec::new();
        out.extend_from_slice(&DBI_MTP_AUTHORIZATION.to_be_bytes());
        out.extend_from_slice(&u32::try_from(inner.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(&inner);
        out
    }

    /// 合成向量能被完整解出来。
    ///
    /// 不这样会怎样：没有这条，整个解析器只能靠一份真实 tdata 验证，
    /// 而那份样本不在 CI 上、也不在别人的开发机上——等于这段代码没有回归。
    #[test]
    fn parses_synthetic_authorization() {
        let raw = synth_authorization(123_456_789, 2, &[1, 2, 4, 5]);
        let a = parse_mtp_authorization(&raw).expect("合成向量应当能解析");
        assert_eq!(a.user_id, 123_456_789);
        assert_eq!(a.main_dc, 2);
        assert_eq!(a.keys.len(), 4);
        assert_eq!(
            a.keys.iter().map(|(d, _)| *d).collect::<Vec<_>>(),
            vec![1, 2, 4, 5]
        );
        // 主 DC 那把要能取到，而且确实是第 2 把
        let main = a.main_key().expect("主 DC 的 key 应当存在");
        let want: Vec<u8> = (0..256)
            .map(|j| (j as u8).wrapping_mul(7) ^ 1u8.wrapping_mul(31))
            .collect();
        assert_eq!(&main[..], &want[..], "主 DC 取到的不是对应那把 key");
    }

    /// 旧格式（没有宽 id 哨兵）也要能解。
    ///
    /// 不这样会怎样：旧版桌面端写的 tdata 里两个 u32 直接就是
    /// userId 与 mainDc。把哨兵判丢了会把一个真实 user_id 误当哨兵，
    /// 于是往后多读 12 字节，所有字段错位——而错误只会说「解析失败」。
    #[test]
    fn parses_legacy_narrow_id() {
        let mut inner = Vec::new();
        inner.extend_from_slice(&777u32.to_be_bytes()); // userId
        inner.extend_from_slice(&5u32.to_be_bytes()); // mainDc
        inner.extend_from_slice(&1u32.to_be_bytes()); // count
        inner.extend_from_slice(&5u32.to_be_bytes()); // dcId
        inner.extend_from_slice(&vec![0u8; 256]);

        let mut raw = Vec::new();
        raw.extend_from_slice(&DBI_MTP_AUTHORIZATION.to_be_bytes());
        raw.extend_from_slice(&u32::try_from(inner.len()).unwrap_or(0).to_be_bytes());
        raw.extend_from_slice(&inner);

        let a = parse_mtp_authorization(&raw).expect("旧格式应当能解析");
        assert_eq!(a.user_id, 777, "旧格式的 userId 不该被当成哨兵");
        assert_eq!(a.main_dc, 5);
    }

    /// 第一个 block 不是授权信息时必须**明确失败**，不能去遍历 block 链。
    ///
    /// 不这样会怎样：授权文件是专用单块文件，第一个 u32 就该是
    /// `dbiMtpAuthorization`。去遍历的话，一个结构不同的文件会被当成
    /// 「一堆未知 block」静默跳过，最后返回一个空结果——
    /// 而真正的问题是「这根本不是授权文件」。
    #[test]
    fn rejects_wrong_first_block() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x2Eu32.to_be_bytes()); // 46，前一次尝试猜错的那个值
        raw.extend_from_slice(&0u32.to_be_bytes());
        let e = parse_mtp_authorization(&raw);
        assert!(
            matches!(e, Err(TdataError::Unsupported(_))),
            "第一个 block 不对时要明确报不支持，而不是静默跳过"
        );
    }

    /// `0xFFFFFFFF` 是 Qt 的 null 标记，不是一个 4 GiB 的数组。
    ///
    /// 不这样会怎样：把它当长度会让偏移全错、往后读全是乱码，
    /// 而症状是「文件损坏」——前一次尝试正是卡在这里。
    #[test]
    fn null_array_marker_is_not_a_length() {
        let raw = [0xFFu8, 0xFF, 0xFF, 0xFF, 1, 2, 3, 4];
        let mut b = raw.as_slice();
        assert!(matches!(
            read_array_be(&mut b),
            Err(TdataError::Unsupported(_))
        ));
    }

    /// `file_key` 的半字节互换不能漏。
    ///
    /// 不这样会怎样：漏了会算出一个完全不同的文件名，那个文件不存在，
    /// 于是表现成「找不到账号」，症状指不到哈希实现。
    ///
    /// 期望值来自真实样本的目录名（那个文件确实叫这个名字），
    /// 不是我自己算一遍再抄过来——那样等于拿实现验实现。
    #[test]
    fn file_key_matches_real_sample_names() {
        assert_eq!(file_key("data"), "D877F783D5D3EF8C");
        // 长度必须是 16：取的是前 8 字节的 hex
        assert_eq!(file_key("data#2").len(), 16);
        // 大写：Telegram Desktop 用的是大写 hex，小写会找不到文件
        assert!(file_key("data").chars().all(|c| !c.is_lowercase()));
    }

    /// TDF 校验必须真的能拦住改动。
    ///
    /// 不这样会怎样：一个被改过的文件会被当成正常文件解析，
    /// 而解出来的东西是垃圾——错误延后到某个更难解释的地方。
    #[test]
    fn tdf_checksum_catches_tampering() {
        // 造一个合法的 TDF
        let data = b"hello tdata".to_vec();
        let version = 6_006_002u32;
        let mut raw = Vec::new();
        raw.extend_from_slice(b"TDF$");
        raw.extend_from_slice(&version.to_le_bytes());
        raw.extend_from_slice(&data);
        {
            use md5::{Digest as _, Md5};
            let mut h = Md5::new();
            h.update(&data);
            h.update(u32::try_from(data.len()).unwrap_or(0).to_le_bytes());
            h.update(version.to_le_bytes());
            h.update(b"TDF$");
            raw.extend_from_slice(&h.finalize());
        }
        let ok = parse_tdf(&raw).expect("合法的 TDF 应当能解析");
        assert_eq!(ok.data, data);
        assert_eq!(ok.version, version);

        // 改一个字节，校验必须失败
        let mut bad = raw.clone();
        bad[9] ^= 0x01;
        assert!(matches!(parse_tdf(&bad), Err(TdataError::Corrupt(_))));

        // 魔数不对也要拦
        let mut wrong_magic = raw.clone();
        wrong_magic[0] = b'X';
        assert!(matches!(parse_tdf(&wrong_magic), Err(TdataError::Corrupt(_))));
    }

    /// MD5 里的长度字段是**小端**。
    ///
    /// 不这样会怎样：写成大端的话所有真实文件都校验不过，
    /// 而错误是「文件损坏」——让人去怀疑用户的 tdata，而不是自己的实现。
    #[test]
    fn tdf_length_field_is_little_endian() {
        let data = vec![7u8; 260]; // 260 的大小端表示不同，能区分出来
        let version = 1u32;
        let mut raw = Vec::new();
        raw.extend_from_slice(b"TDF$");
        raw.extend_from_slice(&version.to_le_bytes());
        raw.extend_from_slice(&data);
        {
            use md5::{Digest as _, Md5};
            let mut h = Md5::new();
            h.update(&data);
            // 故意用**大端**算，模拟写错的实现
            h.update(u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
            h.update(version.to_le_bytes());
            h.update(b"TDF$");
            raw.extend_from_slice(&h.finalize());
        }
        assert!(
            matches!(parse_tdf(&raw), Err(TdataError::Corrupt(_))),
            "长度字段按大端算出来的 MD5 必须被拒——说明我们用的是小端"
        );
    }

    /// 空 passcode 与非空 passcode 的迭代次数不同。
    ///
    /// 不这样会怎样：一律用 100000 次的话，没设本地密码的 tdata（绝大多数）
    /// 全都解不开，而错误是「密码不正确」——用户会去试各种密码，
    /// 而他根本没设过密码。
    #[test]
    fn empty_passcode_uses_one_iteration() {
        let salt = [3u8; 32];
        let a = derive_passcode_key(b"", &salt);
        let b = derive_passcode_key(b"x", &salt);
        assert_ne!(&a[..], &b[..], "空与非空必须派生出不同的密钥");

        // 空 passcode 用 1 次迭代：自己按 1 次算一遍必须一致
        use hmac::Hmac;
        use sha2::{Digest as _, Sha512};
        let mut h = Sha512::new();
        h.update(salt);
        h.update(b"");
        h.update(salt);
        let seed = h.finalize();
        let mut want = [0u8; 256];
        let _ = pbkdf2::pbkdf2::<Hmac<Sha512>>(&seed, &salt, 1, &mut want);
        assert_eq!(&a[..], &want[..], "空 passcode 必须是 1 次迭代");
    }

    /// `strip_full_len` 读的是**小端**且长度含自身。
    ///
    /// 不这样会怎样：少算或多算 4 个字节，后面所有偏移都错位，
    /// 而症状是「解析失败」，指不到这里。
    #[test]
    fn full_len_is_little_endian_and_includes_itself() {
        // fullLen = 8：4 字节自身 + 4 字节内容
        let mut plain = 8u32.to_le_bytes().to_vec();
        plain.extend_from_slice(&[1, 2, 3, 4]);
        plain.extend_from_slice(&[9, 9, 9, 9]); // 填充，不该被返回
        let body = strip_full_len(&plain).expect("应当能剥掉前缀");
        assert_eq!(body, &[1, 2, 3, 4], "长度含自身，且只返回它覆盖的部分");

        // 长度不合理要拒绝
        let bad = 3u32.to_le_bytes().to_vec();
        assert!(matches!(strip_full_len(&bad), Err(TdataError::Corrupt(_))));
    }

    /// 后缀优先级必须是 s → 0 → 1。
    ///
    /// 不这样会怎样：顺序反了会读到**迁移前的陈旧文件**。它能解开、
    /// 内容也像模像样，只是属于上一次登录——于是拿到一把已经失效的 key，
    /// 而错误要等到真正发请求时才出现，那时已经很难联想到是读错了文件。
    #[test]
    fn suffix_priority_prefers_s() {
        let d = std::env::temp_dir().join(format!("omy_tdata_suffix_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("建临时目录");
        std::fs::write(d.join("key_data0"), b"old").expect("写 0");
        std::fs::write(d.join("key_datas"), b"new").expect("写 s");
        let got = resolve_suffixed(&d, "key_data").expect("应当找得到");
        assert!(
            got.file_name().and_then(|s| s.to_str()) == Some("key_datas"),
            "必须优先 s 后缀，实际拿到 {got:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 枚举安装目录时必须排除 `tupdates`。
    ///
    /// 不这样会怎样：更新器在 tupdates 下解压出一个形似 tdata 的目录，
    /// 但它不含真实登录态。列出来的话用户会看到一个选了必然失败的
    /// 「幽灵账号」，而失败原因完全无从解释。
    #[test]
    fn scan_excludes_tupdates_ghost() {
        let base = std::env::temp_dir().join(format!("omy_tdata_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let ghost = base.join("tupdates").join("temp").join("tdata");
        std::fs::create_dir_all(&ghost).expect("建幽灵目录");
        std::fs::write(ghost.join("key_datas"), b"x").expect("写");
        let real = base.join("tdata");
        std::fs::create_dir_all(&real).expect("建真目录");
        std::fs::write(real.join("key_datas"), b"x").expect("写");

        let found = scan_install_dir(&base);
        assert!(
            found.iter().all(|p| !p.to_string_lossy().contains("tupdates")),
            "tupdates 下的幽灵项必须被排除，实际 {found:?}"
        );
        assert_eq!(found.len(), 1, "只该找到真正的那个 tdata");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// 加解密必须能来回。
    ///
    /// 这条是**诊断工具**而不只是回归：真实 tdata 解不开时，靠它区分
    /// 「实现有 bug」与「那份 tdata 真的设了本地密码」。没有它的话，
    /// 只能在改实现和怪用户之间反复猜。
    ///
    /// 注意加密侧与解密侧共用同一个 `prepare_aes_old`——各写一遍的话，
    /// 两边用同一个错误算法也能对上，这条就成了假断言。
    #[test]
    fn local_encryption_round_trips() {
        // 用算式生成而不是重复字节：长串重复会让本机安全软件删掉测试二进制
        let key: [u8; 256] = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x5C);
        let plain: Vec<u8> = (0..300).map(|i| (i % 251) as u8).collect();

        let enc = encrypt_local(&plain, &key);
        let dec = decrypt_local(&enc, &key).expect("自己加密的必须能自己解开");
        // 解出来会带补齐，前 300 字节必须一致
        assert!(dec.len() >= plain.len());
        assert_eq!(&dec[..plain.len()], &plain[..], "来回之后内容变了");
    }

    /// 已知答案向量：钉住 `prepare_aes_old` 的 `x = 8`。
    ///
    /// **为什么 round-trip 不够**：加密与解密共用 `prepare_aes_old`，
    /// 把 `x` 改成 0 之后两边仍然自洽，round-trip 照样全绿，
    /// 但所有真实 tdata 都解不开。实测已证实这一点——
    /// x=0 时真实样本的 SHA1 自校验失败、x=8 通过。
    ///
    /// 期望值是**按参考规范独立算出来的**（用 Python 照
    /// gotd/td `prepareAES` 的公式算），不是跑这段 Rust 再抄回来——
    /// 后者等于拿实现验实现，实现错了向量跟着错，测试永远通过。
    ///
    /// 不这样会怎样：有人「顺手把那个看起来多余的 +8 去掉」时，
    /// 14 条测试全绿，而 tdata 导入对每一个用户都失效，
    /// 报的还是「本地密码不正确」。
    #[test]
    fn aes_schedule_matches_known_answer() {
        // 与 round-trip 那条同一条算式，不用重复字节串
        let key: [u8; 256] = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x5C);
        let msg_key: [u8; 16] = core::array::from_fn(|i| (i as u8).wrapping_mul(3) ^ 0xA1);

        let want_key: [u8; 32] = [
            0xDE, 0xD3, 0xB8, 0x92, 0xEC, 0x6B, 0x40, 0x47, 0xBE, 0xCF, 0xA9, 0xF7, 0x84, 0x62,
            0xA4, 0xE3, 0xBB, 0xB2, 0xEC, 0xB4, 0xE8, 0x59, 0x54, 0xAC, 0x63, 0x53, 0x58, 0xB4,
            0x46, 0xF2, 0x25, 0x56,
        ];
        let want_iv: [u8; 32] = [
            0x90, 0xD9, 0xA3, 0x64, 0x3F, 0x16, 0x16, 0x0E, 0x15, 0x4F, 0x5C, 0xF1, 0x80, 0xC1,
            0x5A, 0x3E, 0x05, 0xA6, 0xF4, 0x31, 0x2C, 0xB4, 0x32, 0x33, 0xA5, 0x15, 0x62, 0xB3,
            0xBA, 0x75, 0x49, 0x94,
        ];

        let (got_key, got_iv) = prepare_aes_old(&msg_key, &key);
        assert_eq!(got_key, want_key, "aes_key 与参考规范不符（x 偏移量是不是被改了？）");
        assert_eq!(got_iv, want_iv, "aes_iv 与参考规范不符（x 偏移量是不是被改了？）");
    }

    /// 换一把密钥必须解不开。
    ///
    /// 不这样会怎样：上一条只证明「自己能解自己」，而一个恒返回成功的
    /// 实现也满足它。这条补上另一半——错密钥必须被 SHA1 校验拦住，
    /// 那正是「本地密码不对」这个判断的依据。
    #[test]
    fn wrong_key_is_rejected() {
        let key: [u8; 256] = core::array::from_fn(|i| (i as u8).wrapping_mul(11) ^ 0x5C);
        let other: [u8; 256] = core::array::from_fn(|i| (i as u8).wrapping_mul(13) ^ 0x21);
        let enc = encrypt_local(b"hello", &key);
        assert!(
            matches!(decrypt_local(&enc, &other), Err(TdataError::WrongPasscode)),
            "错密钥必须被拒，否则「密码不对」这个判断没有依据"
        );
    }

    /// 「需要本地密码」与「密码不对」是两个错误。
    ///
    /// 不这样会怎样：合并成一句的话，没设过密码的人会被要求输入一个
    /// 不存在的密码，而设了密码输错的人看不出自己是输错了。
    #[test]
    fn passcode_errors_are_distinguishable() {
        let need = TdataError::NeedPasscode.to_string();
        let wrong = TdataError::WrongPasscode.to_string();
        assert_ne!(need, wrong);
        // 文案必须说「本地密码」，与云密码（两步验证）区分开——
        // 用户输错对象会一直试不对，而且不知道自己在试错东西
        assert!(need.contains("本地密码"), "实际：{need}");
        assert!(wrong.contains("本地密码"), "实际：{wrong}");
    }
}
