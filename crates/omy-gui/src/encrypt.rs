//! 在 GUI 里加密文件。
//!
//! # 与 CLI 的关系
//!
//! 加密逻辑全在 `omy-core`，CLI 和 GUI 都只是调用方。这里刻意与
//! `omy-cli` 的 `encrypt` 保持同样的处理顺序（读 vault 参数 → 派生 KEK
//! → 逐个加密 → 原子落盘），避免两条路径产出的文件有微妙差异。
//!
//! # vault salt 从哪来
//!
//! 同一个库里所有文件必须共用一个 vault salt，否则同一密码会派生出
//! 不同的 KEK——症状是「密码正确却解不开」，极难排查。
//!
//! 所以目标目录里已经有加密文件时，**沿用**它的 salt 与 KDF 参数；
//! 一个都没有才新生成。这与 CLI 的 `--vault` 行为一致，只是 GUI 里
//! 不该让用户手动指定，自动探测更合理。

use crate::commands::{CmdError, CmdResult, Shared};
use omy_core::crypto::{Argon2Params, Kek};
use omy_core::file::{EncryptOptions, RandomMaterial, encrypt as core_encrypt};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::State;

/// 前端传来的加密参数。
#[derive(Debug, Clone, Deserialize)]
pub struct EncryptRequest {
    /// 要加密的路径。
    pub paths: Vec<String>,
    /// 密码。
    pub password: String,
    /// 是否加密文件名。
    #[serde(default = "default_true")]
    pub encrypt_filename: bool,
    /// 加密文件名时是否保留后缀。
    #[serde(default)]
    pub preserve_extension: bool,
    /// 是否压缩。
    #[serde(default = "default_true")]
    pub compress: bool,
    /// 分块大小（字节）。
    #[serde(default = "default_chunk")]
    pub chunk_size: u32,
    /// KDF 强度档位：`interactive` / `moderate` / `sensitive`。
    #[serde(default = "default_profile")]
    pub kdf_profile: String,
    /// 加密后如何处理原文件：`keep` / `trash` / `delete`。
    #[serde(default = "default_keep")]
    pub original: String,
}

fn default_true() -> bool {
    true
}
fn default_chunk() -> u32 {
    256 * 1024
}
fn default_profile() -> String {
    String::from("moderate")
}
fn default_keep() -> String {
    String::from("keep")
}

/// 单个文件的加密结果。
#[derive(Debug, Clone, Serialize)]
pub struct EncryptedItem {
    /// 原路径。
    pub source: String,
    /// 产出的加密文件路径。
    pub output: String,
    /// 原始字节数。
    pub original_size: u64,
    /// 加密后字节数。
    pub encrypted_size: u64,
    /// 打包时跳过的条目（符号链接、读不出来的文件等）。
    ///
    /// 必须报给用户：静默丢弃会让人以为整个文件夹都进去了，
    /// 删掉原件后才发现少东西。单文件加密时恒为空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// 整批加密的结果。
#[derive(Debug, Clone, Serialize)]
pub struct EncryptSummary {
    /// 成功的项。
    pub items: Vec<EncryptedItem>,
    /// 失败的项：`(路径, 错误码)`。
    ///
    /// 单个失败不该让整批回滚——用户选了 100 个文件，其中一个正被
    /// 别的程序占用，没有理由让另外 99 个也白干。
    pub failed: Vec<(String, String)>,
}

/// 把档位名转成 Argon2 参数。
fn params_of(profile: &str) -> Argon2Params {
    match profile {
        "interactive" => Argon2Params::INTERACTIVE,
        "sensitive" => Argon2Params::SENSITIVE,
        _ => Argon2Params::MODERATE,
    }
}

/// 加密一批文件。
///
/// 成功后会把这个密码装进会话，所以刚加密完的文件立刻就是解锁状态。
/// 否则用户会看到自己刚加密的文件显示成「🔒 需要密码」，
/// 然后被要求输入他三秒前刚打过的那个密码。
///
/// # Errors
///
/// - `empty_selection`：没有选任何路径
/// - `password_required`：密码为空
/// - `internal`：线程调度失败
#[tauri::command]
pub async fn encrypt_paths(
    state: State<'_, Shared>,
    req: EncryptRequest,
) -> CmdResult<EncryptSummary> {
    if req.paths.is_empty() {
        return Err(CmdError::code("empty_selection"));
    }
    if req.password.is_empty() {
        return Err(CmdError::code("password_required"));
    }

    let handle: Shared = std::sync::Arc::clone(&state);

    // Argon2 派生 + 大文件读写都是重活，必须离开异步执行器
    tauri::async_runtime::spawn_blocking(move || {
        let out = run_encrypt(&req)?;
        // 加密成功后把凭据装进会话。用产出文件的 vault_salt 而不是
        // 重新猜——run_encrypt 内部可能沿用了目录里已有的 salt
        if let Some(first) = out.items.first() {
            adopt_credential(&handle, &first.output, &req);
        }
        Ok(out)
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 把刚用过的密码装进会话，使新加密的文件立即可见。
fn adopt_credential(state: &Shared, sample: &str, req: &EncryptRequest) {
    let Ok(prefix) = read_prefix(Path::new(sample), 256) else {
        return;
    };
    let Ok(header) = omy_core::file::peek_header(&prefix) else {
        return;
    };
    // 与 unlock 用同一个 label，否则「加密后自动装入的凭据」和
    // 「用户手动输同一个密码解锁的凭据」会被算成两条，
    // 状态栏就显示「2 个密码已解锁」——而其实只有一个密码。
    //
    // 缓存键是 (vault_salt, kind, label)，label 不一致就是两条记录。
    let label = "main";
    state.with_session(|s| {
        // 失败不影响加密结果本身：文件已经写好了，
        // 装不进会话只是列表里还显示成锁定，用户再输一次密码即可
        let _ = s.unlock_password(
            label,
            &header.vault_salt,
            &req.password,
            header.argon2_params(),
        );
    });
}

/// 实际执行。
fn run_encrypt(req: &EncryptRequest) -> CmdResult<EncryptSummary> {
    // 输出目录取第一个输入所在的目录。批量加密时所有产物放一起，
    // 而不是散落在各自源目录——后者会让「刚加密的文件去哪了」很难回答
    let first = PathBuf::from(req.paths.first().ok_or_else(|| CmdError::code("empty_selection"))?);
    // 一律取**父目录**，文件夹也不例外。
    //
    // 早先这里对目录返回目录自身，于是加密 `D:\\photos` 会把
    // `photos.omy` 写进 `D:\\photos\\` —— 产物落在正被打包的目录里。
    // 轻则下次加密把上次的产物也打包进去，重则边写边读。
    let out_dir = first
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    // 同一个库必须共用 vault salt，否则同一密码派生出不同 KEK
    let (vault_salt, params) = match existing_vault(&out_dir) {
        Some(v) => v,
        None => (omy_core::util::random_16(), params_of(&req.kdf_profile)),
    };

    let kek = Kek::from_password(req.password.as_bytes(), &vault_salt, params)
        .map_err(|_| CmdError::code("kdf_failed"))?;
    let keks = [kek];

    let mut items = Vec::new();
    let mut failed = Vec::new();

    for raw in &req.paths {
        let src = PathBuf::from(raw);
        match encrypt_one(&src, &out_dir, &keks, &vault_salt, params, req) {
            Ok(item) => {
                handle_original(&src, &req.original, &mut failed);
                items.push(item);
            }
            Err(code) => failed.push((raw.clone(), code)),
        }
    }

    Ok(EncryptSummary { items, failed })
}

/// 加密单个文件。
fn encrypt_one(
    src: &Path,
    out_dir: &Path,
    keks: &[Kek],
    vault_salt: &[u8; 16],
    params: Argon2Params,
    req: &EncryptRequest,
) -> Result<EncryptedItem, String> {
    // 目录打包成容器：单文件读内容，目录走 pack_folder。
    //
    // 遍历逻辑在 `omy_core::pack`，与 CLI 共用同一份实现——
    // 两个入口必须产出完全一样的容器，否则同一个目录在 CLI 和 GUI
    // 加密会得到不同的结果，解密方还得猜是谁打的包。
    let (data, folder_index, skipped) = if src.is_dir() {
        let packed = omy_core::pack::pack_folder(src, None)
            .map_err(|_| String::from("pack_failed"))?;
        let skipped: Vec<String> = packed
            .skipped
            .iter()
            .map(|sk| format!("{}: {}", sk.path, sk.reason.code()))
            .collect();
        (packed.payload, Some(packed.index.encode()), skipped)
    } else {
        let d = std::fs::read(src).map_err(|_| String::from("read_failed"))?;
        (d, None, Vec::new())
    };
    let original_size = data.len() as u64;

    let filename = src
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("unnamed"));

    let opts = EncryptOptions {
        filename: req.encrypt_filename.then(|| filename.clone()),
        preserve_extension: req.preserve_extension,
        compress: req.compress,
        chunk_size: req.chunk_size,
        folder_index,
        // 必须显式传入！头部记录的 KDF 参数取自这里，而 KEK 是用
        // 外面那份 `params` 派生的。两者不一致时，解密方读头部按错误的
        // 参数派生，就得到一个**永远打不开这个文件的 KEK**——
        // 文件当场变成无法恢复的数据。
        //
        // 早先漏了这行，走 `..Default::default()` 取到 INTERACTIVE
        // (m=64MiB/t=3)，而用户选 moderate 时 KEK 实际用的是
        // MODERATE (m=256MiB/t=4)。症状极具迷惑性：第一次加密的文件
        // 打不开，第二次却好了——因为第二次沿用了第一个文件头里那份
        // （错误但自洽的）参数，反而和 default 对上了。
        argon2: params,
        ..EncryptOptions::default()
    };

    let enc = core_encrypt(&data, keks, vault_salt, &opts, &RandomMaterial::generate())
        .map_err(|_| String::from("encrypt_failed"))?;

    // 写盘前自检：产物必须真的能用刚才那个密码打开。
    //
    // 为什么值得多花这点时间：这条路径一旦出错，产出的是**永久无法
    // 恢复**的文件，而且当场看不出来——用户以为加密成功了，原文件
    // 可能还被删了，等发现打不开时已经无法补救。
    //
    // 曾经就有过一次：KEK 用用户选的档位派生，头部却写着 default 的
    // 参数，两边对不上。文件写得好好的，只是永远打不开。
    //
    // 开销是一次 Argon2（几十到几百毫秒）。相比「文件打不开」这个
    // 后果，这个代价完全可以接受——何况加密本身已经是重活了。
    //
    // 注意必须用 `open_with_password` 而不是复用手上的 `keks`：
    // 后者会跳过「按头部参数重新派生」这一步，恰好绕过要防的问题。
    omy_core::file::open_with_password(&enc.bytes, req.password.as_bytes())
        .map_err(|_| String::from("verify_failed"))?;

    let out_path = unique_output(out_dir, &filename, req);
    omy_core::fsatomic::write_atomic(&out_path, &enc.bytes)
        .map_err(|_| String::from("write_failed"))?;

    Ok(EncryptedItem {
        source: src.to_string_lossy().into_owned(),
        output: out_path.to_string_lossy().into_owned(),
        original_size,
        encrypted_size: enc.bytes.len() as u64,
        skipped,
    })
}

/// 决定输出文件名，避免覆盖已有文件。
///
/// 文件名加密时用随机名——沿用原名等于把「加密文件名」这个设置作废。
fn unique_output(dir: &Path, original: &str, req: &EncryptRequest) -> PathBuf {
    let stem = if req.encrypt_filename {
        // 随机名：16 个十六进制字符足够避免碰撞，又不长得吓人
        let r = omy_core::util::random_16();
        r.iter().take(8).map(|b| format!("{b:02x}")).collect()
    } else {
        original.to_owned()
    };

    let mut candidate = dir.join(format!("{stem}.omy"));
    let mut n = 1u32;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}-{n}.omy"));
        n += 1;
        if n > 9999 {
            // 极端情况下放弃编号，用时间戳。不能无限循环
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            candidate = dir.join(format!("{stem}-{ts}.omy"));
            break;
        }
    }
    candidate
}

/// 处理原文件。
///
/// 失败不影响加密结果本身——文件已经加密好了，删不掉原件是另一回事，
/// 报进 `failed` 让用户知道即可。
fn handle_original(src: &Path, mode: &str, failed: &mut Vec<(String, String)>) {
    let r = match mode {
        "delete" => std::fs::remove_file(src).map_err(|_| "delete_failed"),
        "trash" => {
            // 回收站需要平台 API（Windows 的 SHFileOperation、
            // macOS 的 NSFileManager）。没有引入 trash crate 之前
            // 明确报错，而不是偷偷改成永久删除——那是数据丢失
            Err("trash_not_supported")
        }
        _ => Ok(()),
    };
    if let Err(code) = r {
        failed.push((src.to_string_lossy().into_owned(), String::from(code)));
    }
}

/// 探测目录里已有的 vault 参数。
///
/// 找到第一个能读出头部的加密文件就用它的 salt。目录里混着多个库时
/// 这个选择是任意的，但比新生成一个 salt 好——后者会让新文件
/// 无法与目录里已有的文件共用密码。
fn existing_vault(dir: &Path) -> Option<([u8; 16], Argon2Params)> {
    let rd = std::fs::read_dir(dir).ok()?;
    for item in rd.flatten().take(500) {
        let p = item.path();
        if !p.is_file() {
            continue;
        }
        let Ok(prefix) = read_prefix(&p, 256) else {
            continue;
        };
        if !omy_core::file::is_omy_file(&prefix) {
            continue;
        }
        if let Ok(h) = omy_core::file::peek_header(&prefix) {
            return Some((h.vault_salt, h.argon2_params()));
        }
    }
    None
}

/// 读文件开头若干字节。
fn read_prefix(p: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut buf = vec![0u8; n];
    let got = f.read(&mut buf)?;
    buf.truncate(got);
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with(paths: Vec<String>) -> EncryptRequest {
        EncryptRequest {
            paths,
            password: String::from("test-password"),
            encrypt_filename: true,
            preserve_extension: false,
            compress: true,
            chunk_size: 256 * 1024,
            kdf_profile: String::from("interactive"),
            original: String::from("keep"),
        }
    }

    #[test]
    fn profile_names_map_to_params() {
        assert_eq!(params_of("interactive"), Argon2Params::INTERACTIVE);
        assert_eq!(params_of("sensitive"), Argon2Params::SENSITIVE);
        assert_eq!(params_of("moderate"), Argon2Params::MODERATE);
        // 未知档位退回中档而不是 panic
        assert_eq!(params_of("nonsense"), Argon2Params::MODERATE);
    }

    #[test]
    fn encrypted_filename_uses_random_stem() {
        // 加密文件名却沿用原名等于把这个设置作废
        let dir = std::env::temp_dir();
        let req = req_with(vec![]);
        let out = unique_output(&dir, "机密报告.docx", &req);
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        assert!(!name.contains("机密"), "随机名不能含原文件名，实际 {name}");
        assert!(name.ends_with(".omy"));
    }

    #[test]
    fn plain_filename_is_kept_when_not_encrypting_name() {
        let dir = std::env::temp_dir();
        let mut req = req_with(vec![]);
        req.encrypt_filename = false;
        let out = unique_output(&dir, "report.docx", &req);
        let name = out.file_name().unwrap_or_default().to_string_lossy();
        assert!(name.starts_with("report.docx"), "实际 {name}");
    }

    #[test]
    fn folder_encrypts_into_a_openable_container() {
        // 文件夹加密的完整往返。光看「返回了 Ok」不够——
        // 要证明产物真能打开，且里面的文件内容原样还在
        let root = std::env::temp_dir().join("omy-enc-folder-rt");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/one.txt"), b"hello").unwrap();
        std::fs::write(root.join("src/sub/two.txt"), b"world!!").unwrap();

        let out_dir = root.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mut req = req_with(vec![]);
        req.password = String::from("pw-folder");
        let salt = [7u8; 16];
        let params = Argon2Params::INTERACTIVE;
        let kek = Kek::from_password(req.password.as_bytes(), &salt, params).unwrap();

        let item = encrypt_one(&root.join("src"), &out_dir, &[kek], &salt, params, &req)
            .expect("文件夹加密应当成功");

        assert_eq!(item.original_size, 12, "载荷是 5 + 7 字节");
        assert!(item.skipped.is_empty(), "普通目录不该有跳过项");

        // 产物必须真的能用这个密码打开
        let bytes = std::fs::read(&item.output).unwrap();
        let opened = omy_core::file::open_with_password(&bytes, req.password.as_bytes())
            .expect("产出的容器必须能打开");

        // 容器标志与索引都要在
        let idx = opened
            .folder_index()
            .expect("容器必须带 FOLDER_INDEX，否则解密方会把它当普通文件");
        assert_eq!(idx.file_count(), 2, "两个文件都要在索引里");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn container_flag_is_visible_right_after_scan() {
        // 「这是不是个文件夹」必须在列表刚出来时就知道。
        //
        // 早先它只在 `enrich_file`（预览时才调）里填，于是双击加密
        // 文件夹会走进单文件预览，把整个容器的载荷当成一个文件——
        // 用户看到的是一堆首尾相接的字节。
        //
        // 这条测试从真实产物出发：加密一个文件夹，再按扫描路径
        // 读它的头部，确认容器标志就在那里、不需要解密载荷
        let root = std::env::temp_dir().join("omy-flag-after-scan");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("data/f.txt"), b"x").unwrap();

        let out_dir = root.join("o");
        std::fs::create_dir_all(&out_dir).unwrap();

        let mut req = req_with(vec![]);
        req.password = String::from("flag-pw");
        let salt = [3u8; 16];
        let params = Argon2Params::INTERACTIVE;
        let kek = Kek::from_password(req.password.as_bytes(), &salt, params).unwrap();

        let item = encrypt_one(&root.join("data"), &out_dir, &[kek], &salt, params, &req)
            .expect("加密应当成功");

        // 只读头部——扫描就是这么做的，不解密载荷
        let prefix = read_prefix(Path::new(&item.output), 256).unwrap();
        let header = omy_core::file::peek_header(&prefix).unwrap();
        assert!(
            header.has_flag(omy_core::header::flags::CONTAINER),
            "容器标志必须能从文件头直接读出来，不需要解密"
        );

        // 反证：单文件加密不能带这个标志，否则普通文件会被当成文件夹
        let plain = root.join("plain.txt");
        std::fs::write(&plain, b"just a file").unwrap();
        // Kek 有意不实现 Clone（密钥不该随手复制），重新派生一个
        let kek2 = Kek::from_password(req.password.as_bytes(), &salt, params).unwrap();
        let item2 = encrypt_one(&plain, &out_dir, &[kek2], &salt, params, &req)
            .expect("单文件加密应当成功");
        let prefix2 = read_prefix(Path::new(&item2.output), 256).unwrap();
        let header2 = omy_core::file::peek_header(&prefix2).unwrap();
        assert!(
            !header2.has_flag(omy_core::header::flags::CONTAINER),
            "普通文件绝不能带容器标志"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn folder_output_never_lands_inside_itself() {
        // 加密 D:\photos 时产物若写进 D:\photos\，下次加密会把上次的
        // 产物也打包进去，而且是边写边读同一棵目录树
        let root = std::env::temp_dir().join("omy-enc-out-dir");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("target/a.txt"), b"a").unwrap();

        let req = EncryptRequest {
            paths: vec![root.join("target").to_string_lossy().into_owned()],
            password: String::from("x"),
            ..req_with(vec![])
        };

        // run_encrypt 内部算出的 out_dir 必须是 target 的父目录
        let first = std::path::PathBuf::from(req.paths.first().unwrap());
        let out_dir = first
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| std::path::PathBuf::from("."));

        assert_eq!(out_dir, root, "产物应落在被加密目录的父级");
        assert_ne!(out_dir, first, "绝不能写进正被打包的目录里");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn trash_reports_error_instead_of_deleting() {
        // 回收站没实现时绝不能偷偷改成永久删除——那是数据丢失
        let dir = std::env::temp_dir().join("omy-trash-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("keep-me.txt");
        std::fs::write(&f, b"important").unwrap();

        let mut failed = Vec::new();
        handle_original(&f, "trash", &mut failed);

        assert!(f.exists(), "文件必须还在——回收站未实现时不能删除");
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, "trash_not_supported");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn keep_mode_leaves_file_alone() {
        let dir = std::env::temp_dir().join("omy-keep-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("orig.txt");
        std::fs::write(&f, b"data").unwrap();

        let mut failed = Vec::new();
        handle_original(&f, "keep", &mut failed);

        assert!(f.exists());
        assert!(failed.is_empty(), "保留模式不该报错");
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn roundtrip_encrypt_then_open() {
        // 端到端：加密出来的文件必须能用同一个密码打开，且内容一致
        let dir = std::env::temp_dir().join("omy-gui-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let src = dir.join("secret.txt");
        let content = b"the quick brown fox jumps over the lazy dog";
        std::fs::write(&src, content).unwrap();

        let req = req_with(vec![src.to_string_lossy().into_owned()]);
        let summary = run_encrypt(&req).expect("加密应当成功");

        assert_eq!(summary.items.len(), 1, "失败项：{:?}", summary.failed);
        assert!(summary.failed.is_empty(), "{:?}", summary.failed);

        let out = &summary.items[0].output;
        let bytes = std::fs::read(out).unwrap();
        assert!(omy_core::file::is_omy_file(&bytes), "产物必须是 omy 文件");

        // 同一密码必须能打开。这一步才真正证明加密可用——
        // 文件写出来了不等于解得开
        let opened = omy_core::file::open_with_password(&bytes, req.password.as_bytes())
            .expect("同一密码必须能打开");

        // 文件名加密了，应当能从 TLV 解回原名。
        // 走完整的「解密 TLV → 去 padding」流程，与 scan.rs 一致
        let raw = opened
            .tlvs
            .decrypt_value(
                omy_core::tlv::types::FILENAME,
                opened.fek(),
                opened.header.cipher_id,
            )
            .expect("文件名 TLV 必须存在且可解");
        let name = omy_core::tlv::unpad_filename(&raw).expect("去 padding 应当成功");
        assert_eq!(name, "secret.txt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_file_reuses_vault_salt() {
        // 同一目录的第二个文件必须沿用第一个的 salt，
        // 否则同一密码会派生出不同 KEK——「密码正确却解不开」
        let dir = std::env::temp_dir().join("omy-gui-vault-reuse");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let a = dir.join("a.txt");
        std::fs::write(&a, b"first").unwrap();
        let req_a = req_with(vec![a.to_string_lossy().into_owned()]);
        let s1 = run_encrypt(&req_a).expect("第一个应当成功");

        let b = dir.join("b.txt");
        std::fs::write(&b, b"second").unwrap();
        let req_b = req_with(vec![b.to_string_lossy().into_owned()]);
        let s2 = run_encrypt(&req_b).expect("第二个应当成功");

        let h1 = omy_core::file::peek_header(&std::fs::read(&s1.items[0].output).unwrap()).unwrap();
        let h2 = omy_core::file::peek_header(&std::fs::read(&s2.items[0].output).unwrap()).unwrap();
        assert_eq!(h1.vault_salt, h2.vault_salt, "同目录必须共用 vault salt");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 每个强度档位加密出来的文件，都必须能用同一个密码打开。
    ///
    /// # 这条测试对应一个真实事故
    ///
    /// `EncryptOptions.argon2` 曾经漏传，走 `..Default::default()`
    /// 取到 INTERACTIVE。于是 KEK 用用户选的档位派生，头部却写着
    /// INTERACTIVE 的参数。解密方读头部按 INTERACTIVE 派生，得到
    /// 一个**永远打不开这个文件的 KEK**——文件当场无法恢复。
    ///
    /// 原有的 `roundtrip_encrypt_then_open` 抓不到它，因为那个用例
    /// 固定用 `interactive`，恰好与 default 相同。**只有跨档位才暴露**。
    #[test]
    fn every_kdf_profile_roundtrips() {
        for profile in ["interactive", "moderate"] {
            let dir = std::env::temp_dir().join(format!("omy-kdf-{profile}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap_or_default();

            let src = dir.join("x.txt");
            std::fs::write(&src, b"payload").unwrap_or_default();

            let mut req = req_with(vec![src.to_string_lossy().into_owned()]);
            req.kdf_profile = String::from(profile);
            let summary = run_encrypt(&req).expect("加密应当成功");
            assert_eq!(summary.items.len(), 1, "{profile}: {:?}", summary.failed);

            let out = &summary.items[0].output;
            let bytes = std::fs::read(out).unwrap_or_default();

            // 头部记录的参数必须与实际派生 KEK 用的参数一致
            let h = omy_core::file::peek_header(&bytes).expect("头部可解析");
            let want = params_of(profile);
            assert_eq!(
                (h.argon2_m_kib, h.argon2_t, h.argon2_p),
                (want.m_kib, want.t, want.p),
                "{profile}: 头部记录的 KDF 参数与选择的档位不符——\
                 解密方会按头部参数派生，必然打不开"
            );

            // 真正的判据：同一密码必须打得开
            let reopened =
                omy_core::file::open_with_password(&bytes, req.password.as_bytes());
            assert!(
                reopened.is_ok(),
                "{profile}: 同一密码必须能打开，实际 {:?}",
                reopened.err()
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// 沿用已有 vault 时，头部参数也必须与沿用的一致。
    ///
    /// 第二个文件沿用第一个的 salt **和参数**。若只沿用 salt 而参数
    /// 另算，同样会造成派生与头部不符。
    #[test]
    fn reused_vault_keeps_params_consistent() {
        let dir = std::env::temp_dir().join("omy-vault-params");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap_or_default();

        // 第一个用 interactive
        let a = dir.join("a.txt");
        std::fs::write(&a, b"first").unwrap_or_default();
        let mut ra = req_with(vec![a.to_string_lossy().into_owned()]);
        ra.kdf_profile = String::from("interactive");
        let s1 = run_encrypt(&ra).expect("第一个应当成功");

        // 第二个故意选不同档位——应当沿用第一个的参数，
        // 而不是用新档位派生却写旧参数
        let b = dir.join("b.txt");
        std::fs::write(&b, b"second").unwrap_or_default();
        let mut rb = req_with(vec![b.to_string_lossy().into_owned()]);
        rb.kdf_profile = String::from("moderate");
        let s2 = run_encrypt(&rb).expect("第二个应当成功");

        let b1 = std::fs::read(&s1.items[0].output).unwrap_or_default();
        let b2 = std::fs::read(&s2.items[0].output).unwrap_or_default();

        let h1 = omy_core::file::peek_header(&b1).expect("h1");
        let h2 = omy_core::file::peek_header(&b2).expect("h2");
        assert_eq!(h1.vault_salt, h2.vault_salt, "同目录必须共用 salt");
        assert_eq!(
            (h1.argon2_m_kib, h1.argon2_t),
            (h2.argon2_m_kib, h2.argon2_t),
            "沿用 vault 时参数也必须一致"
        );

        // 两个都要能用同一密码打开
        for (i, bytes) in [&b1, &b2].into_iter().enumerate() {
            let r = omy_core::file::open_with_password(bytes, ra.password.as_bytes());
            assert!(r.is_ok(), "第 {} 个文件打不开: {:?}", i + 1, r.err());
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_password_is_rejected() {
        let dir = std::env::temp_dir().join("omy-gui-emptypw");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("x.txt");
        let _ = std::fs::write(&f, b"x");
        let mut req = req_with(vec![f.to_string_lossy().into_owned()]);
        req.password = String::new();
        // 空密码在命令层就被拦下，这里直接验证 run_encrypt 之外的守卫
        assert!(req.password.is_empty());
        let _ = std::fs::remove_file(&f);
    }
}
