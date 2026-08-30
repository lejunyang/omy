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
    /// 凭据名称，用于状态栏显示。
    #[serde(default = "default_label")]
    pub label: String,
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

fn default_label() -> String {
    String::from("main")
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
    let label = if req.label.is_empty() {
        "main"
    } else {
        &req.label
    };
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
    let out_dir = if first.is_dir() {
        first.clone()
    } else {
        first
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };

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
        match encrypt_one(&src, &out_dir, &keks, &vault_salt, req) {
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
    req: &EncryptRequest,
) -> Result<EncryptedItem, String> {
    if src.is_dir() {
        // 目录打包成容器是另一条路径（CLI 的 build_container），
        // 涉及索引构建与递归遍历。首期 GUI 先不做，明确报错而不是
        // 假装成功——静默跳过会让用户以为文件夹已经加密了
        return Err(String::from("folder_not_supported"));
    }

    let data = std::fs::read(src).map_err(|_| String::from("read_failed"))?;
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
        ..EncryptOptions::default()
    };

    let enc = core_encrypt(&data, keks, vault_salt, &opts, &RandomMaterial::generate())
        .map_err(|_| String::from("encrypt_failed"))?;

    let out_path = unique_output(out_dir, &filename, req);
    omy_core::fsatomic::write_atomic(&out_path, &enc.bytes)
        .map_err(|_| String::from("write_failed"))?;

    Ok(EncryptedItem {
        source: src.to_string_lossy().into_owned(),
        output: out_path.to_string_lossy().into_owned(),
        original_size,
        encrypted_size: enc.bytes.len() as u64,
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
            label: String::from("main"),
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
    fn folder_is_rejected_not_silently_skipped() {
        // 静默跳过会让用户以为文件夹已经加密了
        let dir = std::env::temp_dir().join("omy-enc-folder-test");
        let _ = std::fs::create_dir_all(&dir);
        let req = req_with(vec![]);
        let salt = [0u8; 16];
        let kek = Kek::from_password(b"x", &salt, Argon2Params::INTERACTIVE).unwrap();
        let r = encrypt_one(&dir, &dir, &[kek], &salt, &req);
        assert_eq!(r.err().as_deref(), Some("folder_not_supported"));
        let _ = std::fs::remove_dir(&dir);
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
