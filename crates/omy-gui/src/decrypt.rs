//! 在 GUI 里把加密文件还原到磁盘。
//!
//! # 这和「明文不落盘」冲突吗
//!
//! 不冲突，但边界必须说清，否则下一个人会以为这个模块违背了既定原则。
//!
//! 「无临时明文文件」（需求 F-06）约束的是**预览**：应用内看图、播视频
//! 时不准偷偷在磁盘上留一份明文。那条原则针对的是**用户没要求落盘、
//! 却因为实现方便而落盘**的情况——用户不知道有这个文件，也不知道它什么
//! 时候被清掉。
//!
//! 本模块是用户**明确要求**把文件取出来（需求 F-02「解密后 bit-for-bit
//! 一致」本就要求有这条路径）。目标路径是用户自己选的，他知道文件在哪。
//! 这两件事的区别不在「有没有明文落盘」，而在**是不是用户的意图**。
//!
//! # 为什么不做「解密后自动打开」
//!
//! 那是需求 F-19（外部应用打开）的事，走的是受控临时文件 + 用完即删，
//! 与这里「按用户指定位置持久保存」是两套语义。混在一个命令里会让
//! 「这个文件会不会被自动删掉」变得难以回答。

use crate::commands::{CmdError, CmdResult, Shared};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{Emitter as _, State};

/// 进度事件名。前端用同名字符串 listen。
///
/// 与加密用**不同**的事件名：两者可能先后发生，共用一个名字会让前端
/// 分不清当前进度条该显示「加密中」还是「还原中」。
pub const DECRYPT_PROGRESS_EVENT: &str = "decrypt://progress";

/// 还原进度事件载荷。
///
/// 字段与 `EncryptProgress` 刻意保持一致，前端可以复用同一个进度条组件。
#[derive(Debug, Clone, Serialize)]
pub struct DecryptProgress {
    /// 当前第几个（从 1 开始，便于直接显示）。
    pub index: usize,
    /// 本批总数。
    pub total_files: usize,
    /// 当前条目名。
    pub name: String,
    /// 已处理字节。
    pub done: u64,
    /// 总字节。
    pub total: u64,
}

/// 前端传来的还原参数。
#[derive(Debug, Clone, Deserialize)]
pub struct DecryptRequest {
    /// 要还原的文件 id（已在会话里登记过的加密文件）。
    pub ids: Vec<String>,
    /// 目标目录。
    ///
    /// `None` 表示还原到加密文件所在目录——与 CLI 不带 `--output-dir`
    /// 时的行为一致。前端「还原到当前目录」走这条。
    #[serde(default)]
    pub target_dir: Option<String>,
    /// 目标已存在同名文件时是否覆盖。
    ///
    /// 默认 `false`：覆盖是不可逆的，默认值必须是安全的那个。
    #[serde(default)]
    pub overwrite: bool,
}

/// 单项还原结果。
#[derive(Debug, Clone, Serialize)]
pub struct RestoredItem {
    /// 加密文件路径。
    pub source: String,
    /// 还原出来的路径（容器则是根目录）。
    pub output: String,
    /// 是否为容器。
    pub is_container: bool,
    /// 容器里的文件数（单文件时为 1）。
    pub files: usize,
    /// 容器里的目录数（单文件时为 0）。
    pub dirs: usize,
    /// 明文字节数。
    pub size: u64,
    /// 因平台限制被改名的条目。
    ///
    /// 必须报：名字被悄悄改掉，用户按原名找不到文件，
    /// 只会以为还原漏了东西（决策 N3）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub adjusted: Vec<String>,
    /// 未能还原的条目：`(路径, 原因代号)`。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<(String, String)>,
    /// 元数据还原情况。
    pub metadata: MetadataNote,
}

/// 元数据还原情况，供前端展示。
///
/// 不直接把 core 的 `RestoreReport` 序列化出去：那里面的
/// `UnsupportedKind` 是 Rust 枚举，跨到 JS 需要一个稳定形状。
///
/// # `not_recorded` 这个代号
///
/// 除了 core 给的那几种，这里多一个 `not_recorded`，用于**单文件**加密：
/// 单文件的时间戳从来没被保存过——格式里预留了 `ORIGINAL_META`
/// (TLV 0x0008)，但至今没有任何代码写入或读取它。
///
/// 所以这不是「还原失败」，而是「压根没有值可还原」，必须和前两类分开。
/// 静默不提的代价是：用户解开单个文件发现时间是「现在」，会以为是 bug，
/// 或者以为自己记错了；明说之后他还知道有个替代方案——加密整个文件夹
/// 走容器路径，时间是保住的。
#[derive(Debug, Clone, Default, Serialize)]
pub struct MetadataNote {
    /// 成功还原修改时间的条目数。
    pub mtime_restored: usize,
    /// 成功还原权限位的条目数（Windows 上恒为 0）。
    pub mode_restored: usize,
    /// 当前平台不支持的项：`(代号, 条目数)`。
    ///
    /// 代号而不是译好的文案：翻译在前端做，后端塞中文会让
    /// 英文界面出现中文。
    pub unsupported: Vec<(String, usize)>,
    /// 尝试过但失败的项：`(路径, 项代号, 原因)`。
    ///
    /// 与 `unsupported` 分开的理由见 `omy_core::restore`：前者换个平台
    /// 就能拿到，后者是这次的问题、重试可能就好。混在一起会让用户
    /// 要么以为文件坏了，要么放弃重试白丢元数据。
    pub failures: Vec<(String, String, String)>,
}

impl From<&omy_core::restore::RestoreReport> for MetadataNote {
    fn from(r: &omy_core::restore::RestoreReport) -> Self {
        Self {
            mtime_restored: r.mtime_restored,
            mode_restored: r.mode_restored,
            unsupported: r
                .unsupported
                .iter()
                .map(|(k, n)| (String::from(k.code()), *n))
                .collect(),
            failures: r
                .failures
                .iter()
                .map(|f| (f.path.clone(), String::from(f.item), f.reason.clone()))
                .collect(),
        }
    }
}

/// 整批还原结果。
#[derive(Debug, Clone, Serialize)]
pub struct DecryptSummary {
    /// 成功项。
    pub items: Vec<RestoredItem>,
    /// 失败项：`(id 或路径, 错误码)`。
    ///
    /// 单个失败不让整批回滚：用户选了 20 个文件，其中一个的密码不在
    /// 会话里，没有理由让另外 19 个也白干。
    pub failed: Vec<(String, String)>,
}

/// 把选中的加密文件还原到磁盘。
///
/// # Errors
///
/// - `empty_selection`：没有选任何文件
/// - `target_not_a_dir`：指定的目标不是目录
/// - `internal`：线程调度失败
#[tauri::command]
pub async fn decrypt_paths(
    app: tauri::AppHandle,
    state: State<'_, Shared>,
    req: DecryptRequest,
) -> CmdResult<DecryptSummary> {
    if req.ids.is_empty() {
        return Err(CmdError::code("empty_selection"));
    }

    // 目标目录先校验再开工。等到解完密才发现目标不对，白算一遍
    // Argon2（几百毫秒到数秒），而且错误信息来得莫名其妙
    if let Some(d) = &req.target_dir {
        let p = Path::new(d);
        if !p.is_dir() {
            return Err(CmdError::code("target_not_a_dir"));
        }
    }

    // 先在异步侧把 id 解析成路径：state 不是 Send，不能带进 spawn_blocking
    let mut jobs: Vec<(String, PathBuf)> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    for id in &req.ids {
        match state.file(id) {
            Some(e) if e.unlocked => jobs.push((id.clone(), PathBuf::from(&e.path))),
            // 未解锁的必须报出来，不能静默跳过：用户选了 5 个、
            // 只出来 3 个，得知道另外 2 个去哪了
            Some(_) => failed.push((id.clone(), String::from("locked"))),
            None => failed.push((id.clone(), String::from("file_not_found"))),
        }
    }

    let handle: Shared = std::sync::Arc::clone(&state);

    // 大文件解密 + 写盘是重活，必须离开异步执行器
    tauri::async_runtime::spawn_blocking(move || {
        let mut items = Vec::new();
        let total_files = jobs.len();
        for (i, (id, path)) in jobs.iter().enumerate() {
            let pos = i.saturating_add(1);
            match restore_one(&handle, path, &req, Some((&app, pos, total_files))) {
                Ok(item) => items.push(item),
                Err(code) => failed.push((id.clone(), code)),
            }
        }
        Ok(DecryptSummary { items, failed })
    })
    .await
    .map_err(|_| CmdError::code("internal"))?
}

/// 还原单个加密文件。
fn restore_one(
    state: &Shared,
    src: &Path,
    req: &DecryptRequest,
    progress: Option<(&tauri::AppHandle, usize, usize)>,
) -> Result<RestoredItem, String> {
    let data = std::fs::read(src).map_err(|_| String::from("read_failed"))?;
    let header =
        omy_core::file::peek_header(&data).map_err(|_| String::from("not_an_omy_file"))?;

    // 用会话里已有的凭据，不再问密码：文件能出现在列表里就说明
    // 它已经解锁过了。再弹一次密码框是在问用户三秒前刚输过的东西
    let keks: Vec<omy_core::crypto::Kek> = state
        .with_session(|s| {
            s.all_for(&header.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();
    if keks.is_empty() {
        return Err(String::from("locked"));
    }

    let opened = omy_core::file::open(&data, &keks).map_err(|_| String::from("locked"))?;

    // 目标目录：未指定则用加密文件所在目录，与 CLI 不带 --output-dir 一致
    let target = match &req.target_dir {
        Some(d) => PathBuf::from(d),
        None => src
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    };

    let name_for_event = opened
        .filename()
        .ok()
        .or_else(|| src.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| String::from("unnamed"));

    // 进度按整百分比节流，理由同加密侧：不节流的话大文件会发几千次
    // 事件，IPC 开销反而拖慢解密
    let mut last_pct = u8::MAX;
    let total = header.plaintext_size;
    let plain = {
        let mut cb = |done: u64, _t: u64| {
            let Some((app, pos, total_files)) = progress else {
                return;
            };
            let pct = done
                .saturating_mul(100)
                .checked_div(total)
                .and_then(|v| u8::try_from(v).ok())
                .unwrap_or(100);
            if pct == last_pct {
                return;
            }
            last_pct = pct;
            let _ = app.emit(
                DECRYPT_PROGRESS_EVENT,
                DecryptProgress {
                    index: pos,
                    total_files,
                    name: name_for_event.clone(),
                    done,
                    total,
                },
            );
        };
        opened
            .decrypt_all_with_progress(&data, Some(&mut cb))
            .map_err(|_| String::from("decrypt_failed"))?
    };
    let size = plain.len() as u64;

    if opened.is_container() {
        let idx = opened
            .folder_index()
            .map_err(|_| String::from("bad_container_index"))?;
        // 覆盖检查放在解密之后：容器根名在索引里，不解密拿不到
        let root = target.join(omy_core::unpack::sanitize_filename(&idx.root));
        if !req.overwrite && root.exists() {
            return Err(String::from("target_exists"));
        }
        // 落盘与元数据还原都在 core，与 CLI 共用同一份实现
        let rep = omy_core::unpack::extract_container(&idx, &plain, &target)
            .map_err(|_| String::from("extract_failed"))?;
        return Ok(RestoredItem {
            source: src.to_string_lossy().into_owned(),
            output: rep.root.to_string_lossy().into_owned(),
            is_container: true,
            files: rep.files,
            dirs: rep.dirs,
            size,
            adjusted: rep.adjusted,
            skipped: rep
                .skipped
                .iter()
                .map(|s| (s.path.clone(), String::from(s.reason)))
                .collect(),
            metadata: MetadataNote::from(&rep.metadata),
        });
    }

    // 单文件：文件名取自头部 TLV，回退到去掉 .omy 的密文名
    let name = opened.filename().unwrap_or_else(|_| {
        src.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("decrypted"))
    });
    // 必须过 sanitize：文件名来自加密文件内部，是不可信输入。
    // 不处理的话，一个精心构造的 TLV 就能让明文写到别处去
    let out_path = target.join(omy_core::unpack::sanitize_filename(&name));
    if !req.overwrite && out_path.exists() {
        return Err(String::from("target_exists"));
    }
    omy_core::fsatomic::write_atomic(&out_path, &plain)
        .map_err(|_| String::from("write_failed"))?;

    Ok(RestoredItem {
        source: src.to_string_lossy().into_owned(),
        output: out_path.to_string_lossy().into_owned(),
        is_container: false,
        files: 1,
        dirs: 0,
        size,
        adjusted: Vec::new(),
        skipped: Vec::new(),
        // 单文件**没有**保存过 mtime，所以这里不是「还原失败」而是
        // 「压根没记录」。见 `not_recorded` 的说明
        metadata: MetadataNote {
            unsupported: vec![(String::from("not_recorded"), 1)],
            ..MetadataNote::default()
        },
    })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_note_keeps_unsupported_and_failures_apart() {
        // 两者混在一起会让用户误判：平台限制是既有事实（换台机器就有），
        // 设置失败是这次的问题（重试可能就好）
        let r = omy_core::restore::RestoreReport {
            mtime_restored: 3,
            unsupported: vec![(omy_core::restore::UnsupportedKind::Btime, 2)],
            failures: vec![omy_core::restore::RestoreFailure {
                path: String::from("a.txt"),
                item: "mtime",
                reason: String::from("被占用"),
            }],
            ..omy_core::restore::RestoreReport::default()
        };

        let n = MetadataNote::from(&r);
        assert_eq!(n.mtime_restored, 3);
        assert_eq!(n.unsupported, vec![(String::from("btime"), 2)]);
        assert_eq!(n.failures.len(), 1);
        assert_eq!(n.failures[0].0, "a.txt");
    }

    #[test]
    fn unsupported_uses_stable_codes_not_translated_text() {
        // 后端塞中文会让英文界面出现中文。code() 同时是翻译键后缀，
        // 改动即破坏前端翻译
        let r = omy_core::restore::RestoreReport {
            unsupported: vec![
                (omy_core::restore::UnsupportedKind::Mode, 1),
                (omy_core::restore::UnsupportedKind::Owner, 1),
            ],
            ..omy_core::restore::RestoreReport::default()
        };
        let n = MetadataNote::from(&r);
        assert_eq!(
            n.unsupported,
            vec![(String::from("mode"), 1), (String::from("owner"), 1)]
        );
    }

    #[test]
    fn empty_report_produces_empty_note() {
        // 没有元数据可还原时不该冒出空壳提示
        let n = MetadataNote::from(&omy_core::restore::RestoreReport::default());
        assert_eq!(n.mtime_restored, 0);
        assert!(n.unsupported.is_empty());
        assert!(n.failures.is_empty());
    }

    #[test]
    fn progress_event_name_differs_from_encrypt() {
        // 共用一个事件名会让前端分不清进度条该显示「加密中」还是
        // 「还原中」——两者可能先后发生
        assert_ne!(
            DECRYPT_PROGRESS_EVENT,
            crate::encrypt::ENCRYPT_PROGRESS_EVENT
        );
    }

    #[test]
    fn overwrite_defaults_to_false() {
        // 覆盖不可逆，默认值必须是安全的那个。
        // 前端漏传这个字段时不该变成「默认覆盖」
        let req: DecryptRequest =
            serde_json::from_str(r#"{"ids":["a"]}"#).expect("最小请求应能反序列化");
        assert!(!req.overwrite, "默认不覆盖");
        assert!(req.target_dir.is_none(), "默认还原到源文件所在目录");
    }
}
