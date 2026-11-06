//! 把本地文件逐个上传到远程目录。

use super::*;

/// 上传一批本地文件到远程目录。
///
/// # 用的是磁盘上的文件名，不是解密后的真名
///
/// 这不是偷懒。用户开了文件名加密，磁盘上就是那串密文名；omy 若
/// 「贴心地」用解密后的真名上传，**等于把加密掉的文件名主动交给服务端**，
/// 前面的加密全白做。原名只显示给用户自己看。
///
/// # 逐个上传而不是并发
///
/// 并发会同时开多个到同一服务端的请求，在限流敏感的 Telegram 上很容易
/// 换来一次 `FLOOD_WAIT`，结果是整体更慢而不是更快。这与 `prefetch_all`
/// 的取舍一致。
///
/// # 一个失败不影响其余
///
/// 逐个记录成败后一起返回。中途 `?` 掉的话，前 N 个已经传上去了而调用方
/// 只看到一个错误，界面上会显示成「全部失败」——与事实不符，
/// 而用户再传一次就会产生重复文件。
///
/// # Errors
///
/// 位置不存在、或目标目录不可写时返回。单个文件的失败不算错误，
/// 在返回值里如实标注。
#[tauri::command]
pub async fn remote_upload(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    place_id: String,
    dir: String,
    paths: Vec<String>,
) -> CmdResult<Vec<UploadOutcome>> {
    crate::telegram_cmds::ensure_connected(
        &reg,
        &place_id,
        &crate::place_keys::unlock_keks(&state),
    )
    .await?;
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);

    // 先问能力位图。不问的话，只读对话上会发一次注定失败的请求，
    // 错误还只有一句 provider 的原始报错
    let caps = store
        .effective_capabilities(&dir)
        .await
        .map_err(|e| to_cmd_err(&e))?;
    if !caps.write {
        crate::applog::warn(
            "upload",
            &format!(
                "place={place_id} dir={} 拒绝：目录只读",
                crate::applog::redact(&dir)
            ),
        );
        return Err(CmdError::code("remote_readonly"));
    }
    crate::applog::info(
        "upload",
        &format!(
            "place={place_id} dir={} 开始上传 {} 个文件",
            crate::applog::redact(&dir),
            paths.len()
        ),
    );

    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let path = std::path::PathBuf::from(&p);
        // 磁盘文件名 —— 见函数文档，绝不能换成解密后的真名
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("file"));

        // 本地文件也走有界流和共享提交：整文件 read 会耗尽内存，直接 PUT
        // 在失败时还会留下顶着最终名的半成品。错误仍逐文件返回，源文件保留。
        let result = async {
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|e| e.to_string())?;
            let size = file.metadata().await.map_err(|e| e.to_string())?.len();
            omy_remote::commit_upload(
                store.as_ref(), caps, &dir, &name, size, Box::new(file), None,
            )
            .await
            .map_err(|e| e.to_string())
        }
        .await;

        match result {
            Ok(entry) => {
                crate::applog::info(
                    "upload",
                    &format!(
                        "place={place_id} ok size={} name={}",
                        entry.size.unwrap_or(0),
                        crate::applog::redact(&name)
                    ),
                );
                out.push(UploadOutcome {
                    path: p,
                    name,
                    ok: true,
                    id: Some(entry.id),
                    error: None,
                });
            }
            Err(e) => {
                // 记结果类型（RPC 错误码/来源报错），不记文件内容
                crate::applog::error(
                    "upload",
                    &format!(
                        "place={place_id} 失败 name={}: {e}",
                        crate::applog::redact(&name)
                    ),
                );
                out.push(UploadOutcome {
                    path: p,
                    name,
                    ok: false,
                    id: None,
                    error: Some(e.to_string()),
                });
            }
        }
    }
    Ok(out)
}

/// 单个文件的上传结果。
///
/// 成败逐个返回而不是整体一个布尔：部分成功是常态（网络抖动、个别文件
/// 超限），压成一个布尔的话界面只能说「失败了」，用户不知道哪些传上去了，
/// 再传一次就产生重复文件。
#[derive(Debug, Clone, serde::Serialize)]
pub struct UploadOutcome {
    /// 本地路径（前端据此对应回它自己的列表）。
    pub path: String,
    /// 上传时用的名字（磁盘名）。
    pub name: String,
    /// 成功与否。
    pub ok: bool,
    /// 成功时的远程条目 id。
    pub id: Option<String>,
    /// 失败原因。
    pub error: Option<String>,
}
