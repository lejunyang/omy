//! 打开远程文件：识别 / 解锁、发放播放令牌、解密到本地，以及读取远程容器的条目表。

use super::*;

/// 打开远程 `.omy` 的结果。
///
/// 三种结局必须分开，前端据此给三种不同的界面：
/// - `unlocked=true, token=Some`：可播放，拿 token 去请求 `omystream://pfile/`；
/// - `unlocked=false, not_encrypted=false`：是 omy 但当前会话密码打不开（不是网络问题）；
/// - `not_encrypted=true`：根本不是 omy 文件（远程普通文件，本期只能下载/外部打开）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct OpenPlaceResult {
    /// 播放令牌；仅解锁成功时为 `Some`。
    pub token: Option<String>,
    /// 是否成功解锁。
    pub unlocked: bool,
    /// 是否为非 omy 的普通文件。
    pub not_encrypted: bool,
    /// 解密后的真实文件名。
    pub name: Option<String>,
    /// 预览类别 video/audio/image/text/other。
    pub kind: Option<String>,
    /// 解密内容的 MIME。
    pub mime: Option<String>,
    /// 明文大小。
    pub size: Option<u64>,
    /// 密文大小（任何时候都可见）。
    pub encrypted_size: u64,
}

/// 取当前远程目录里所有加密文件的 vault 参数（用于解锁）。
///
/// # 为什么需要这条命令
///
/// 顶栏的密码入口走 `unlock_directory`，它要一个**本地目录**去探测 vault。
/// 远程位置没有本地目录，于是 `tryUnlock` 第一行的 `if (!state.cwd)` 直接
/// 返回——用户在远程位置里输密码**什么都不会发生，连错误都没有**，
/// 看到的是「密码输了没反应，文件一直锁着」。
///
/// 局域网那条线早就有对应的 `remote_vaults`，云盘这条漏了。
///
/// 去重按 salt：同一个 vault 里所有文件的 salt 相同，重复派生纯属浪费
/// （Argon2 每次都要几百毫秒）。
///
/// # Errors
///
/// 位置不存在时返回。读不到头部的条目直接跳过而不是报错——
/// 那些文件的失败已经由 `probe_failed` 表达过了。
#[tauri::command]
pub async fn remote_place_vaults(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    state: tauri::State<'_, Shared>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    vault_reg: tauri::State<'_, Arc<crate::vault_reg::VaultRegistry>>,
    place_id: String,
    dir: String,
) -> CmdResult<Vec<crate::commands::VaultParams>> {
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

    let entries = store.list(&dir).await.map_err(|e| to_cmd_err(&e))?;

    let cache_snap = cache.snapshot();
    let stable_source = PlaceRegistry::place_source(&place);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for e in entries {
        if e.is_dir {
            continue;
        }
        let size = e.size.unwrap_or(0);
        // 只读头部，载荷一个字节都不碰。
        //
        // 这条路径与浏览用的是同一批头部：解锁时走到这里，多半刚刚才浏览过
        // 同一个目录，走缓存就不必把那十几个头部再下一遍
        let Ok(bytes) = fetch_full_header(
            store.as_ref(),
            &e.id,
            size,
            cache_snap.as_ref(),
            &place_id,
            stable_source.as_ref(),
        )
        .await
        else {
            continue;
        };
        let Ok(h) = omy_core::file::peek_header(&bytes) else {
            continue;
        };
        let salt = crate::commands::hex_of(&h.vault_salt);
        if seen.insert(salt.clone()) {
            out.push(crate::commands::VaultParams {
                salt,
                m_kib: h.argon2_m_kib,
                t: h.argon2_t,
                p: h.argon2_p,
            });
        }
    }
    // 远程位置见过的 vault 也登记进全局表：同密码解锁虚拟位置/本地文件时
    // 才能为这些 salt 重派生 KEK。salt 来自明文文件头，不涉及密钥。
    let materials: Vec<crate::vault_reg::VaultMaterial> = out
        .iter()
        .filter_map(|v| {
        crate::commands::parse_salt(&v.salt).map(|salt| crate::vault_reg::VaultMaterial {
                salt,
                m_kib: v.m_kib,
                t: v.t,
                p: v.p,
            })
        })
        .collect();
    vault_reg.register_all(&materials);
    Ok(out)
}

/// 「要打开哪个远程条目」。
///
/// 四个字段描述的是同一件事，收成结构体而不是平铺成参数。
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTarget {
    /// 远程位置 id。
    pub place_id: String,
    /// 条目 id（WebDAV 是路径，Telegram 是 `tg:<对话>:<消息>`）。
    pub path: String,
    /// 条目字节数。
    pub size: u64,
    /// 条目显示名，用于普通文件 MIME 推断。
    pub name: Option<String>,
    /// 服务端原文件名，用于定位完整永久原文件。
    pub server_name: Option<String>,
}

/// 打开一个远程 `.omy`，登记可播放来源并返回令牌。
///
/// # Errors
///
/// 位置不存在、头部读取失败或来源构造失败时返回结构化错误。
#[tauri::command]
pub async fn remote_place_open(
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    files: tauri::State<'_, Arc<PlaceFiles>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    state: tauri::State<'_, Shared>,
    target: OpenTarget,
) -> CmdResult<OpenPlaceResult> {
    // 收成一个结构体而不是四个平铺参数：它们本来就是一组——
    // 都在描述「要打开哪个条目」。顺带让参数个数回到 clippy 的限制内，
    // 那个限制在这里提示的是真实问题，不该用 allow 盖过去。
    let OpenTarget {
        place_id,
        path,
        size,
        name,
        server_name,
    } = target;
    // 已有完整永久副本时先走本地：换机复制后即使当前网络不可用，也能从已有
    // 条目或虚拟引用直接打开。没有完整副本才连接远端，避免把损坏/残缺文件当成离线成品。
    if !has_complete_pinned_candidate(&reg, &cache, &place_id, &path, size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);
    let stable_source = PlaceRegistry::place_source(&place);

    // 读完整头部（识别窗口 + 按需补读到 header_len），载荷一个字节都不碰。
    // 走缓存：打开一个文件之前基本都先浏览过它所在的目录，那时头部已经取过
    let header = fetch_full_header(
        store.as_ref(),
        &path,
        size,
        cache.snapshot().as_ref(),
        &place_id,
        stable_source.as_ref(),
    )
    .await
    .map_err(|e| to_cmd_err(&e))?;

    let parsed = match omy_core::file::peek_header(&header) {
        Ok(h) => h,
        Err(_) => {
            // 不是 omy 文件 —— 在远程位置里这是**正常情况而不是错误**。
            //
            // Telegram 位置的全部意义就是「把频道里的文件、图片、视频当远程
            // 文件用」，普通文件才是主体内容。原先这里直接返回「不是加密文件」
            // 且不给 token，于是界面上所有普通文件都打不开、右键也没有菜单
            // （菜单要求条目可激活）。
            //
            // 普通文件不需要解密，按原始字节转发即可。
            // 用调用方给的名字推 MIME。不能只看 id：Telegram 的 id 形如
            // `tg:<对话>:<消息>`，里面根本没有扩展名，光靠它每个文件都会被判成
            // application/octet-stream，于是图片视频统统不预览。
            let label = name.clone().unwrap_or_else(|| path.clone());
            let (kind, mime) = crate::mime::by_extension(&label);
            let rt = tokio::runtime::Handle::current();
            let source = RemoteSource::new_plain(
                Arc::clone(&store),
                place_id.clone(),
                path.clone(),
                size,
                cache.snapshot(),
                rt,
            )
            .with_pinned_identity(
                stable_source
                    .clone()
                    .ok_or_else(|| CmdError::code("remote_source_unidentified"))?,
                server_name
                    .clone()
                    .or_else(|| name.clone())
                    .unwrap_or_else(|| path.clone()),
            );
            let token = files.insert(OpenPlaceFile {
                header: Vec::new(),
                plain: Some(size),
                source,
                mime: mime.clone(),
            });
            crate::applog::info(
                "open",
                &format!(
                    "place={place_id} file={} -> 普通文件（非 omy），mime={mime}",
                    crate::applog::redact(&path)
                ),
            );
            return Ok(OpenPlaceResult {
                token,
                unlocked: true,
                not_encrypted: true,
                name: None,
                kind: Some(kind.to_owned()),
                mime: Some(mime),
                size: Some(size),
                encrypted_size: size,
            });
        }
    };

    let keks: Vec<Kek> = state
        .with_session(|s| {
            s.all_for(&parsed.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();

    let opened = if keks.is_empty() {
        None
    } else {
        omy_core::file::open(&header, &keks).ok()
    };

    let Some(opened) = opened else {
        // 是 omy 但当前密码集打不开——明确告诉前端是「未解锁」，
        // 不能混进 not_encrypted，否则用户会以为文件没加密
        crate::applog::info(
            "open",
            &format!(
                "place={place_id} file={} -> omy 已识别但当前密码集打不开（未解锁）",
                crate::applog::redact(&path)
            ),
        );
        return Ok(OpenPlaceResult {
            token: None,
            unlocked: false,
            not_encrypted: false,
            name: None,
            kind: None,
            mime: None,
            size: None,
            encrypted_size: size,
        });
    };

    let real_name = opened.filename().ok();
    let plaintext_size = opened.header.plaintext_size;
    let (kind, mime) = match &real_name {
        Some(n) => {
            let (k, m) = crate::mime::by_extension(n);
            (k.to_owned(), m)
        }
        None => (
            crate::mime::kind::OTHER.to_owned(),
            String::from("application/octet-stream"),
        ),
    };

    // RemoteSource 只在构造时 peek 一次头部，之后按 1 MiB 块按需拉载荷。
    // 缓存句柄取全局共享的一份，多个文件共用同一磁盘缓存与上限。
    //
    // 用 tokio 的原始 Handle 而非 tauri::async_runtime::handle()：后者是
    // tauri 自己的 RuntimeHandle 包装，而 RemoteSource 要在同步协议线程里
    // block_on，需要的是具体的 tokio 句柄。本命令本身就跑在 tokio 线程上。
    let rt = tokio::runtime::Handle::current();
    let source = RemoteSource::new(
        Arc::clone(&store),
        place_id.clone(),
        path.clone(),
        &header,
        size,
        cache.snapshot(),
        rt,
    )
    .map_err(|e| {
        CmdError::with(
            "remote_open_failed",
            serde_json::json!({ "detail": e.to_string() }),
        )
    })?
    .with_pinned_identity(
        stable_source.ok_or_else(|| CmdError::code("remote_source_unidentified"))?,
        server_name.or(name).unwrap_or_else(|| path.clone()),
    );

    let holder = OpenPlaceFile {
        header: header.clone(),
        plain: None,
        source,
        mime: mime.clone(),
    };
    let token = files
        .insert(holder)
        .ok_or_else(|| CmdError::code("remote_open_failed"))?;

    // 记「已解锁」但**不记 real_name**——那是解密后的真名，属于明文内容
    crate::applog::info(
        "open",
        &format!(
            "place={place_id} file={} -> 已解锁，mime={mime}",
            crate::applog::redact(&path)
        ),
    );
    Ok(OpenPlaceResult {
        token: Some(token),
        unlocked: true,
        not_encrypted: false,
        name: real_name,
        kind: Some(kind),
        mime: Some(mime),
        size: Some(plaintext_size),
        encrypted_size: size,
    })
}

/// 关闭一个远程播放来源（播放结束时调用），释放句柄。
#[tauri::command]
pub fn remote_place_close(files: tauri::State<'_, Arc<PlaceFiles>>, token: String) {
    files.remove(&token);
}

/// 每次落盘的明文窗口：远程文件可能是好几 GB 的视频，绝不能像本地还原那样
/// 先把整份明文解进内存；按 1 MiB 窗口「读一段密文→解密→写盘」流式推进。
pub(super) const LOCAL_WRITE_CHUNK: u64 = 1024 * 1024;

/// 定位一个远程文件所需的三元组：哪个位置、位置内路径、总字节数。
///
/// 多个远程命令都要这三样（打开、解密到本地、未来的写操作），合成一个嵌套
/// 请求结构，既避免 Tauri 命令参数过多，也让前端传参口径统一。嵌套结构走
/// serde 默认的 snake_case，与 `EncryptRequest` 等一致。
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct RemoteFileRef {
    pub place_id: String,
    pub path: String,
    pub size: u64,
    /// 界面显示名，只用于传输列表。
    #[serde(default)]
    pub name: Option<String>,
    /// 服务端原文件名。永久缓存按它保存完整原文件，不能用 `.omy` 内部解出的
    /// 明文名代替，否则落盘内容与扩展名不匹配。
    #[serde(default)]
    pub server_name: Option<String>,
}

/// 「解密到本地」结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteDecryptResult {
    /// 最终写出的明文文件绝对路径。
    pub saved_path: String,
    /// 明文文件名（取自头部 TLV，已做路径清洗）。
    pub name: String,
    /// 写出的明文字节数。
    pub bytes: u64,
}

/// 把一个已解锁的远程 `.omy` **流式解密到本地目录**。
///
/// 这是只读位置也保留的主要用途（见原型能力矩阵）：服务器上始终只有密文，
/// 明文只在本机生成。复用播放链路同一套 `RemoteSource`（块对齐 + 密文缓存）
/// 与 `read_source_range`（header 偏移 / 压缩索引 / 解密），不允许出现第二套
/// 解密读取实现。
///
/// 能力安全边界：前端只对「已解锁单文件」放出口子，但这里仍独立校验——
/// 未解锁、非加密、加密文件夹（容器）一律拒绝，devtools 直接 invoke 也绕不过。
#[tauri::command]
pub async fn remote_decrypt_to_local(
    app: tauri::AppHandle,
    reg: tauri::State<'_, Arc<PlaceRegistry>>,
    cache: tauri::State<'_, Arc<RemoteCache>>,
    state: tauri::State<'_, Shared>,
    req: RemoteFileRef,
    dest_dir: String,
) -> CmdResult<RemoteDecryptResult> {
    // name 只给传输列表显示用，这条路径用不到，显式忽略
    let RemoteFileRef {
        place_id,
        path,
        size,
        server_name,
        ..
    } = req;
    if !has_complete_pinned_candidate(&reg, &cache, &place_id, &path, size) {
        crate::telegram_cmds::ensure_connected(
            &reg,
            &place_id,
            &crate::place_keys::unlock_keks(&state),
        )
        .await?;
    }
    let place = reg
        .get(&place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);
    let stable_source = PlaceRegistry::place_source(&place);

    let header = fetch_full_header(
        store.as_ref(),
        &path,
        size,
        cache.snapshot().as_ref(),
        &place_id,
        stable_source.as_ref(),
    )
    .await
    .map_err(|e| to_cmd_err(&e))?;
    let parsed =
        omy_core::file::peek_header(&header).map_err(|_| CmdError::code("not_an_omy_file"))?;

    let keks: Vec<Kek> = state
        .with_session(|s| {
            s.all_for(&parsed.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })
        .unwrap_or_default();
    if keks.is_empty() {
        return Err(CmdError::code("locked"));
    }

    // 先 open 一次做能力判定与取名/大小；真正解密在阻塞线程里再 open 一次
    // （OpenedFile 不长期持有，与 omystream 每请求重开一致）。
    let probe = omy_core::file::open(&header, &keks).map_err(|_| CmdError::code("locked"))?;
    // 容器（加密文件夹）需要整份索引后解压，无法走单文件流式，本期明确拒绝，
    // 不静默产出一个打不开的东西
    if probe.is_container() {
        return Err(CmdError::code("remote_container_unsupported"));
    }
    let raw_name = probe
        .filename()
        .map_err(|_| CmdError::code("decrypt_failed"))?;
    // 文件名来自加密文件内部 TLV，是不可信输入，必须清洗，否则一个构造的
    // TLV（如 ../../x）就能把明文写到目标目录之外
    let safe = omy_core::unpack::sanitize_filename(&raw_name);
    let total = probe.header.plaintext_size;

    let dest = std::path::PathBuf::from(&dest_dir);
    let final_path = dest.join(&safe);
    // 默认不覆盖，与本地还原一致
    if final_path.exists() {
        return Err(CmdError::code("target_exists"));
    }
    // 先写 .part 再原子改名：中途失败/取消不会留下一个看起来完整的半成品
    let part_path = dest.join(format!("{safe}.part"));

    let cache_snap = cache.snapshot();
    let rt = tokio::runtime::Handle::current();
    let app2 = app.clone();
    let name_for_event = safe.clone();
    // 闭包会拿走 part_path 的所有权，错误清理另留一份
    let part_cleanup = part_path.clone();
    let join = tokio::task::spawn_blocking(move || -> Result<RemoteDecryptResult, String> {
        // RemoteSource 的读方法内部用保存的 Handle block_on 网络请求，
        // 必须在阻塞线程里调（async 线程里 block_on 当前 runtime 会 panic）
        let source =
            RemoteSource::new(store, place_id, path.clone(), &header, size, cache_snap, rt)
                .map_err(|e| e.to_string())?
                .with_pinned_identity(
                    stable_source.ok_or_else(|| "remote_source_unidentified".to_string())?,
                    server_name.unwrap_or_else(|| path.clone()),
                );
        let opened = omy_core::file::open(&header, &keks).map_err(|_| "locked".to_string())?;

        use std::io::Write;
        let _ = std::fs::remove_file(&part_path);
        let mut out = std::fs::File::create(&part_path).map_err(|_| "write_failed".to_string())?;

        let mut done: u64 = 0;
        let mut last_pct: Option<u8> = None;
        while done < total {
            let want = (total - done).min(LOCAL_WRITE_CHUNK);
            let chunk = omy_core::source::read_source_range(&source, &opened, done, want)
                .map_err(|_| "decrypt_failed".to_string())?;
            if chunk.is_empty() {
                return Err("decrypt_failed".to_string());
            }
            out.write_all(&chunk)
                .map_err(|_| "write_failed".to_string())?;
            done = done.saturating_add(chunk.len() as u64);

            // 按整百分比节流，否则大文件发几千次事件，IPC 反而拖慢传输
            let pct = u8::try_from(done.saturating_mul(100).checked_div(total).unwrap_or(100))
                .unwrap_or(100);
            if last_pct != Some(pct) {
                last_pct = Some(pct);
                let _ = app2.emit(
                    DECRYPT_PROGRESS_EVENT,
                    DecryptProgress {
                        index: 1,
                        total_files: 1,
                        name: name_for_event.clone(),
                        done,
                        total,
                    },
                );
            }
        }
        out.sync_all().map_err(|_| "write_failed".to_string())?;
        drop(out);

        // rename 前再查一次存在性，堵住并发下的覆盖
        if final_path.exists() {
            let _ = std::fs::remove_file(&part_path);
            return Err("target_exists".to_string());
        }
        std::fs::rename(&part_path, &final_path).map_err(|_| "write_failed".to_string())?;
        Ok(RemoteDecryptResult {
            saved_path: final_path.to_string_lossy().into_owned(),
            name: name_for_event,
            bytes: done,
        })
    })
    .await;

    match join {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(code)) => {
            let _ = std::fs::remove_file(&part_cleanup);
            Err(CmdError::code(&code))
        }
        Err(_) => {
            let _ = std::fs::remove_file(&part_cleanup);
            Err(CmdError::code("decrypt_failed"))
        }
    }
}

/// 仅取头部、构造一个远程源，供只需要文件结构而不解密明文的缓存操作使用。
///
/// 与播放/解密不同，这里不需要 KEK：缓存里只有密文，统计覆盖情况、删除本地
/// 密文块都不触及明文，因此锁定（未解锁）的文件也允许查/移除它的缓存。
///
/// # 普通文件同样适用
///
/// 远程位置里普通文件是主体内容（Telegram 频道里的图片、视频、文档），
/// 它们走同一套块缓存，也同样值得永久保留——离线看视频正是最典型的场景。
/// 原先这里对非 omy 文件一律返回 `not_an_omy_file`，于是 pin / unpin /
/// 缓存统计 / 清除四条命令**全都拒绝普通文件**，界面上那几个菜单项
/// 对它们永远不出现。
pub(crate) async fn build_remote_source_with_cache(
    reg: &PlaceRegistry,
    cache: Option<omy_remote::cache::BlockCache>,
    req: &RemoteFileRef,
) -> CmdResult<RemoteSource<PlaceStore>> {
    let place = reg
        .get(&req.place_id)
        .ok_or_else(|| CmdError::code("remote_no_such_place"))?;
    let store = Arc::clone(&place.store);
    let source_ref = PlaceRegistry::place_source(&place)
        .ok_or_else(|| CmdError::code("remote_source_unidentified"))?;
    let header = fetch_full_header(
        store.as_ref(),
        &req.path,
        req.size,
        cache.as_ref(),
        &req.place_id,
        Some(&source_ref),
    )
    .await
    .map_err(|e| to_cmd_err(&e))?;
    let rt = tokio::runtime::Handle::current();

    let source_ref = PlaceRegistry::place_source(&place)
        .ok_or_else(|| CmdError::code("remote_source_unidentified"))?;
    let server_name = req
        .server_name
        .clone()
        .or_else(|| req.name.clone())
        .unwrap_or_else(|| req.path.clone());
    let source = match RemoteSource::new(
        Arc::clone(&store),
        req.place_id.clone(),
        req.path.clone(),
        &header,
        req.size,
        cache.clone(),
        rt.clone(),
    ) {
        Ok(src) => src,
        Err(_) => RemoteSource::new_plain(
            store,
            req.place_id.clone(),
            req.path.clone(),
            req.size,
            cache,
            rt,
        ),
    };
    Ok(source.with_pinned_identity(source_ref, server_name))
}

pub(crate) async fn build_remote_source(
    reg: &PlaceRegistry,
    cache: &RemoteCache,
    req: &RemoteFileRef,
) -> CmdResult<RemoteSource<PlaceStore>> {
    build_remote_source_with_cache(reg, cache.snapshot(), req).await
}

/// 列出一个**远程**目录容器里的条目。
///
/// # 与本地 `list_container` 的分工
///
/// 索引解析、条目映射、MIME 推导全部复用 `commands::items_from_index`——
/// 那段逻辑与密文从哪来无关，写第二份迟早漂移。这里只负责把远程那份头部
/// 取过来、并按远程句柄登记 token。
///
/// # 只读头部，不下载载荷
///
/// 索引在 TLV 区，读头部就够。这正是远程容器能「进去看看」而不必先拉下
/// 几百 MB 的原因。
///
/// # 参数为什么是句柄而不是位置 id + 路径
///
/// 容器明文不在磁盘上、要按需解，而 `remote_place_open` 颁发的句柄背后正好
/// 挂着已经建好的远程源与密文缓存。顺带也让「句柄失效 → 容器 token 跟着
/// 失效」自动成立。
///
/// # Errors
///
/// 句柄不存在、当前会话打不开、或这个文件不是目录容器时返回。
#[tauri::command]
pub async fn remote_list_container(
    state: tauri::State<'_, Shared>,
    files: tauri::State<'_, Arc<PlaceFiles>>,
    containers: tauri::State<'_, Arc<PlaceContainers>>,
    token: String,
) -> CmdResult<Vec<crate::commands::ContainerItem>> {
    let Some(f) = files.get(&token) else {
        return Err(CmdError::code("remote_file_closed"));
    };
    let header = f.header.clone();
    let shared: Shared = Arc::clone(&state);
    let idx = tauri::async_runtime::spawn_blocking(move || {
        let h = omy_core::file::peek_header(&header).ok()?;
        let keks: Vec<omy_core::crypto::Kek> = shared.with_session(|s| {
            s.all_for(&h.vault_salt)
                .into_iter()
                .map(|c| c.kek)
                .collect()
        })?;
        let opened = omy_core::file::open(&header, &keks).ok()?;
        opened.folder_index().ok()
    })
    .await
    .map_err(|_| CmdError::code("internal"))?;

    let Some(idx) = idx else {
        return Err(CmdError::code("not_a_container"));
    };

    let mut items = crate::commands::items_from_index(&idx);
    for it in &mut items {
        if let (Some(off), Some(size)) = (it.offset, it.size) {
            it.token = containers.register(crate::place_files::PlaceContainerRef {
                file_token: token.clone(),
                inner_path: it.path.clone(),
                offset: off,
                size,
                mime: it.mime.clone(),
            });
        }
    }
    Ok(items)
}
