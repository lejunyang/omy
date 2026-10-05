//! omy 图形界面库入口。
//!
//! # 结构
//!
//! | 模块 | 职责 |
//! |---|---|
//! | [`state`] | 会话密钥与文件列表。**解锁状态是数据，不是视图** |
//! | [`protocol`] | `omystream://` 按需解密，继承 Spike S1/S5 的全部约束 |
//! | [`commands`] | 前端唯一入口，只返回错误码不返回文案 |
//! | [`mime`] | MIME 推导，含 SVG / HTML 的安全处理 |
//! | [`devices`] | 设备库会话：本机身份与已配对设备 |
//! | [`lan`] | 局域网发现、配对、共享服务 |
//!
//! # 为什么是库 + 二进制
//!
//! 桌面端以 `omy-gui` 可执行文件运行；移动端（Android/iOS）没有
//! 独立进程入口，Tauri 的宿主 Activity 通过 JNI 加载本库并调用
//! [`run`]。两种入口共用同一份应用逻辑，避免维护两份启动代码。
//!
//! # 明文不落盘
//!
//! 这是内嵌预览的核心安全目标。GUI 层的做法：解密只发生在协议处理器里，
//! 结果随响应发走后立即丢弃，既不写临时文件，也不在 `AppState` 里长期持有；
//! 配合响应头的 `no-store`，WebView 也不会把它写进磁盘缓存。
//!
//! Android WebDAV「外部打开」是用户明确选择的例外：第三方应用无法读取内部
//! `omystream://`，因此单个文件会在应用私有目录保留稳定明文工作副本，并只通过
//! FileProvider 临时授权。完整边界见 [`external_edit`]。

// 与 omy-net 同样的约定：产品代码不允许 panic 路径（GUI 直接面对
// 用户的任意文件，崩溃会丢失正在处理的数据），测试代码另行放宽——
// 测试里的 unwrap 失败就是测试失败，那正是它该做的事。
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]
// Windows 上 cdylib 链接 dll 时，链接器的正常输出（创建 .dll.lib/.exp）
// 会被 rustc 的 linker_messages lint 误报为 warning。这是预期行为，
// 不是问题——否则每次构建都多一条噪音。
#![allow(linker_messages)]

pub mod applog;
mod browse;
mod citem;
mod commands;
mod decrypt;
mod device_cmds;
mod device_key;
mod devices;
mod encrypt;
mod external_edit;
mod fileops;
mod keymgmt;
mod lan;
mod mime;
mod place_cmds;
mod place_files;
mod place_keys;
mod places;
mod plain;
mod password_manager;
mod protocol;
mod remote;
mod remote_cmds;
mod remote_copy;
mod settings;
mod state;
mod storage;
mod telegram_cmds;
pub mod transfers;
mod vault_reg;
mod video;
mod virtual_cmds;
mod virtual_place;

use state::AppState;
use std::sync::Arc;

/// 启动图形界面。
///
/// 桌面端由 `main.rs` 调用；移动端由 Tauri 的 `mobile_entry_point`
/// 宏生成 JNI 入口调用（仅 Android/iOS 构建时启用该属性）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 最先初始化日志：后面的位置恢复、setup 都可能出错，日志要能记到。
    // 失败只降级不 panic（见 applog::init）。
    match applog::init() {
        Some(p) => applog::info("boot", &format!("omy-gui 启动，日志落点 {}", p.display())),
        None => eprintln!("[omy] 本次不写文件日志（移动端或目录不可写）"),
    }
    let shared: commands::Shared = Arc::new(AppState::new());
    let for_protocol = Arc::clone(&shared);
    let device_session: device_cmds::SharedDevices = Arc::new(devices::DeviceSession::new());
    // setup 闭包要用它设置安卓的设备库路径。单独克隆一份：
    // 闭包是 move 的，不能借用外层变量。桌面端不需要这条路——
    // 那里 dirs::config_dir() 本来就能解析出正确位置
    #[cfg(target_os = "android")]
    let for_setup_devices = Arc::clone(&device_session);

    // 远程存储位置（WebDAV 等）。与上面的 remote_session 不是一回事：
    // 那个是局域网对端设备，这个是有真实目录层级的远程存储。
    let place_registry: Arc<places::PlaceRegistry> = Arc::new(places::PlaceRegistry::new());
    let external_edits = Arc::new(external_edit::ExternalEdits::new());
    #[cfg(target_os = "android")]
    let for_setup_edits = Arc::clone(&external_edits);
    #[cfg(target_os = "android")]
    let for_setup_places = Arc::clone(&place_registry);
    #[cfg(target_os = "android")]
    let for_setup_state = Arc::clone(&shared);

    let pair_task: device_cmds::SharedPair = Arc::new(lan::PairTask::new());
    let share_task: device_cmds::SharedShare = Arc::new(lan::ShareTask::new());
    let remote_session: Arc<remote::RemoteSession> = Arc::new(remote::RemoteSession::new());
    let for_protocol_remote = Arc::clone(&remote_session);

    // 虚拟远程位置注册表：本地「收藏夹式」位置，只存对真实远程文件的引用。
    // 首帧就载入，侧栏要立即显示它们（同真实位置的理由）。载入失败只记日志、
    // 不挡启动——收藏坏了不该让整个应用起不来。
    let virtual_registry: Arc<virtual_place::VirtualRegistry> =
        Arc::new(virtual_place::VirtualRegistry::new());
    if let Err(e) = virtual_registry.load() {
        applog::warn("virtual", &format!("载入虚拟远程位置失败：{e}"));
    }
    // 全局已见 vault 登记表：用于「同密码跨 vault 自动解锁」。
    let vaults: Arc<vault_reg::VaultRegistry> = Arc::new(vault_reg::VaultRegistry::new());
    #[cfg(not(target_os = "android"))]
    if let Err(e) = vaults.load() {
        applog::warn("virtual", &format!("载入 vault 登记表失败：{e}"));
    }
    #[cfg(target_os = "android")]
    let for_setup_vaults = Arc::clone(&vaults);
    // 把每个已加密虚拟位置自身的 KDF salt 也登记进全局表。必须在启动就做：
    // 否则该 salt 只在显式解锁该位置时才进表，用户先在别处（本地文件/Telegram）
    // 用同密码解锁时，全局表没有这个 salt，无法为它派生 KEK，虚拟位置就不会免密
    // 自动解锁（实测「先解本地，Telegram 开了、虚拟位置仍锁」）。
    vaults.register_all(&virtual_registry.encrypted_vault_materials());
    #[cfg(not(target_os = "android"))]
    restore_saved_places(&place_registry, &vaults);
    // 远程播放：全局密文块缓存（只存密文、按上限 LRU）与打开文件句柄表。
    // 句柄表在协议线程与命令间共享，让多次 Range 请求复用同一来源。
    let remote_cache: Arc<place_files::RemoteCache> =
        Arc::new(place_files::RemoteCache::from_config());
    let place_files: Arc<place_files::PlaceFiles> = Arc::new(place_files::PlaceFiles::new());
    let for_protocol_places = Arc::clone(&place_files);
    // 远程列表缩略图句柄表：文件头 token 刷新目录即清；清晰缩略图额外落盘
    // （<缓存根>/rthumbs），重启后直接读盘出清晰图、不重新拉。
    let thumb_disk = place_files::RemoteCache::resolve_root(
        omy_config::Config::load()
            .unwrap_or_default()
            .remote
            .cache_dir
            .clone(),
    )
    .and_then(|remote_root| remote_root.parent().map(|p| p.join("rthumbs")));
    let place_thumbs: Arc<place_files::PlaceThumbs> =
        Arc::new(place_files::PlaceThumbs::with_disk_dir(thumb_disk));
    let for_protocol_thumbs = Arc::clone(&place_thumbs);
    // 远程容器内条目的登记表。与 PlaceFiles 分开：一个管「打开了哪些远程
    // 文件」，一个管「那些文件里的容器条目」，后者跟着前者失效
    let place_containers: Arc<place_files::PlaceContainers> =
        Arc::new(place_files::PlaceContainers::new());
    let for_protocol_containers = Arc::clone(&place_containers);

    // 传输任务表：下载 / 上传 / 永久保留三类共用一张。
    // 任务是跨对话的——用户可能同时在往一个对话传文件、从另一个缓存视频，
    // 分散在各个位置里显示的话，他一切走就看不到也管不了了
    let transfers = Arc::new(transfers::Transfers::default());
    // pin 失败重试要能重跑原操作，而 Task 不带请求参数——用它记 id->请求
    let pin_retry = Arc::new(place_cmds::PinRetryStore::default());
    // 远程复制同样要保存完整源/目标/模式，失败后才能重建同一条管线。
    let copy_retry = Arc::new(remote_copy::CopyRetryStore::default());
    // Telegram 群组批量缓存的队列与子任务重试参数。
    let cache_batches = Arc::new(place_cmds::CacheBatchStore::default());

    // Telegram 扫码登录任务。同一时刻只允许一个：并发扫码会让两条流程抢同一份
    // session，而且必然撞限流——Telegram 对 exportLoginToken 的频率限制很紧。
    let telegram_login: telegram_cmds::SharedLogin = Arc::new(telegram_cmds::LoginTask::new());
    let telegram_phone_login: telegram_cmds::SharedPhoneLogin =
        Arc::new(telegram_cmds::PhoneLoginTask::new());
    // 密码候选里含明文同步密钥，只保留在 Rust 后端。前端拿到的只是摘要和
    // provider UUID；锁定或下一次查询会清掉旧候选。
    let password_managers = Arc::new(password_manager::PasswordManagerState::new());

    // CDP 端口：仅在设了环境变量时开启，供自动化验证用。
    // 默认不开——远程调试端口意味着任何本地进程都能接管这个
    // WebView，而它里面是解密后的内容。
    let debug_port = std::env::var("OMY_GUI_CDP_PORT").ok();

    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());

    // 安卓的全盘存储访问权限插件。桌面端没有对应概念，整个注册跳过——
    // 不要为了让链式调用整齐而注册一个空插件占位：那会在插件列表里留下
    // 一个永远不做事的条目，之后排查插件相关问题时得先确认它是不是嫌疑人。
    #[cfg(target_os = "android")]
    let builder = builder
        .plugin(storage::init())
        .plugin(external_edit::init())
        .plugin(password_manager::init());

    let app = match builder
        .manage(Arc::clone(&shared))
        .manage(Arc::clone(&device_session))
        .manage(Arc::clone(&pair_task))
        .manage(Arc::clone(&share_task))
        .manage(Arc::clone(&remote_session))
        .manage(Arc::clone(&place_registry))
        .manage(Arc::clone(&virtual_registry))
        .manage(Arc::clone(&vaults))
        .manage(Arc::clone(&remote_cache))
        .manage(Arc::clone(&place_files))
        .manage(Arc::clone(&place_thumbs))
        .manage(Arc::clone(&place_containers))
        .manage(Arc::clone(&transfers))
        .manage(Arc::clone(&pin_retry))
        .manage(Arc::clone(&copy_retry))
        .manage(Arc::clone(&cache_batches))
        .manage(Arc::clone(&telegram_login))
        .manage(Arc::clone(&telegram_phone_login))
        .manage(Arc::clone(&password_managers))
        .manage(Arc::clone(&external_edits))
        // 必须是**异步**协议：同步版本会阻塞 WebView 线程，
        // 大文件解密时界面直接卡死（Spike S1 实测）
        .register_asynchronous_uri_scheme_protocol("omystream", move |_ctx, request, responder| {
            let st = Arc::clone(&for_protocol);
            let rm = Arc::clone(&for_protocol_remote);
            let pf = Arc::clone(&for_protocol_places);
            let pt = Arc::clone(&for_protocol_thumbs);
            let pc = Arc::clone(&for_protocol_containers);
            // 解密可能耗时，必须离开 WebView 线程
            std::thread::spawn(move || {
                responder.respond(protocol::handle(&st, &rm, &pf, &pt, &pc, &request));
            });
        })
        .invoke_handler(tauri::generate_handler![
            commands::vault_params_of,
            commands::unlock,
            commands::unlock_directory,
            commands::probe_one,
            commands::lock,
            commands::is_unlocked,
            commands::credential_count,
            commands::scan_directory,
            commands::list_files,
            commands::enrich_file,
            commands::list_container,
            commands::get_language,
            commands::set_language,
            commands::list_roots,
            commands::stream_base,
            commands::pick_folder,
            commands::pick_files,
            browse::browse_directory,
            browse::list_places,
            browse::parent_of,
            storage::storage_access,
            storage::request_storage_access,
            external_edit::external_edit_open,
            external_edit::external_edit_sync_now,
            plain::open_external,
            plain::list_file_associations,
            plain::clear_file_association,
            plain::reveal_in_folder,
            video::video_capabilities,
            video::video_info,
            video::convert_video,
            video::discard_converted,
            encrypt::encrypt_paths,
            decrypt::decrypt_paths,
            fileops::delete_paths,
            fileops::rename_path,
            fileops::create_folder,
            keymgmt::manage_key,
            keymgmt::list_slots,
            device_key::device_key_status,
            device_key::device_key_unlock,
            device_key::device_key_enroll,
            device_key::device_key_forget,
            password_manager::password_manager_status,
            password_manager::password_manager_connect,
            password_manager::password_manager_list,
            password_manager::password_manager_generate,
            password_manager::password_manager_unlock,
            password_manager::password_manager_forget,
            password_manager::password_manager_clear,
            keymgmt::retry_key_files,
            keymgmt::generate_recovery,
            keymgmt::restore_with_recovery,
            device_cmds::device_status,
            device_cmds::open_device_store,
            device_cmds::close_device_store,
            device_cmds::paired_devices,
            device_cmds::rename_device,
            device_cmds::revoke_device,
            device_cmds::discover_devices,
            device_cmds::pair_listen,
            device_cmds::pair_with,
            device_cmds::pair_status,
            device_cmds::pair_cancel,
            device_cmds::start_share,
            device_cmds::stop_share,
            device_cmds::share_status,
            remote_cmds::remote_connect,
            remote_cmds::remote_disconnect,
            remote_cmds::remote_list,
            remote_cmds::remote_status,
            remote_cmds::remote_relock,
            remote_cmds::remote_vaults,
            settings::config_get,
            settings::config_set,
            settings::config_paths,
            settings::ui_log,
            settings::open_log_dir,
            settings::app_about,
            place_cmds::remote_place_add,
            place_cmds::remote_place_list,
            place_cmds::remote_secret_status,
            place_cmds::remote_place_remove,
            place_cmds::remote_browse,
            place_cmds::remote_search,
            place_cmds::remote_messages,
            place_cmds::remote_messages_around,
            place_cmds::remote_messages_after,
            place_cmds::remote_dir_protected,
            place_cmds::remote_browse_more,
            place_cmds::remote_browse_tab,
            place_cmds::remote_page_size,
            place_cmds::telegram_cache_all,
            place_cmds::transfer_list,
            place_cmds::transfer_cancel,
            place_cmds::transfer_pause,
            place_cmds::transfer_retry,
            place_cmds::transfer_pause_all,
            place_cmds::transfer_clear_done,
            place_cmds::remote_effective_caps,
            place_cmds::remote_probe_entry,
            place_cmds::remote_upload,
            remote_copy::remote_copy,
            place_cmds::remote_place_vaults,
            place_cmds::remote_place_open,
            place_cmds::remote_place_close,
            place_cmds::remote_decrypt_to_local,
            place_cmds::remote_cache_usage,
            place_cmds::remote_list_container,
            place_cmds::remote_cache_pin,
            place_cmds::remote_cache_unpin,
            place_cmds::remote_cache_clear,
            place_cmds::remote_cache_apply,
            place_cmds::remote_cache_open_dir,
            place_cmds::remote_cache_file_stat,
            place_cmds::remote_cache_file_stats,
            place_cmds::remote_meta_put,
            place_cmds::remote_meta_get,
            place_cmds::remote_cache_remove_file,
            place_cmds::remote_cache_list_pinned,
            place_cmds::remote_cache_unpin_by_key,
            telegram_cmds::telegram_can_persist,
            telegram_cmds::telegram_suggest_proxy,
            telegram_cmds::telegram_system_proxy,
            telegram_cmds::telegram_check_connection,
            telegram_cmds::telegram_api_id_status,
            telegram_cmds::telegram_api_id_save,
            telegram_cmds::telegram_api_id_reset,
            telegram_cmds::telegram_has_session,
            telegram_cmds::telegram_place_detach,
            telegram_cmds::telegram_place_delete_account,
            telegram_cmds::telegram_place_rename,
            telegram_cmds::telegram_place_encrypted,
            telegram_cmds::telegram_place_encrypt,
            telegram_cmds::telegram_place_decrypt,
            telegram_cmds::telegram_place_unlock,
            telegram_cmds::telegram_place_decrypt_pw,
            telegram_cmds::telegram_forward_targets,
            telegram_cmds::telegram_forward_messages,
            telegram_cmds::telegram_create_self_group,
            virtual_cmds::virtual_places,
            virtual_cmds::virtual_create,
            virtual_cmds::virtual_rename,
            virtual_cmds::virtual_encrypt,
            virtual_cmds::virtual_unlock,
            virtual_cmds::virtual_lock,
            virtual_cmds::virtual_lock_state,
            virtual_cmds::virtual_add_folder,
            virtual_cmds::virtual_rename_folder,
            virtual_cmds::virtual_remove_folder,
            virtual_cmds::virtual_add_ref,
            virtual_cmds::virtual_remove_ref,
            virtual_cmds::virtual_move_ref,
            virtual_cmds::virtual_copy_refs,
            virtual_cmds::virtual_browse,
            virtual_cmds::virtual_folders,
            virtual_cmds::virtual_delete,
            telegram_cmds::telegram_tdata_probe,
            telegram_cmds::telegram_tdata_check,
            telegram_cmds::telegram_tdata_import,
            telegram_cmds::telegram_login_start,
            telegram_cmds::telegram_submit_password,
            telegram_cmds::telegram_login_cancel,
            telegram_cmds::telegram_phone_start,
            telegram_cmds::telegram_phone_submit_phone,
            telegram_cmds::telegram_phone_submit_code,
            telegram_cmds::telegram_phone_submit_password,
            telegram_cmds::telegram_phone_resend,
            telegram_cmds::telegram_phone_cancel,
            telegram_cmds::telegram_place_connect,
        ])
        .setup(move |app| {
            #[cfg(target_os = "android")]
            {
                // 安卓的窗口由 tauri.android.conf.json 覆盖成 create: true，
                // Tauri 在 setup 前就按它建好了 WebView，这里只补设备库路径。
                if android_setup(app, &for_setup_devices) {
                    if let Err(e) = for_setup_vaults.load() {
                        applog::warn("virtual", &format!("载入 vault 登记表失败：{e}"));
                    }
                    restore_saved_places(&for_setup_places, &for_setup_vaults);
                }
                external_edit_setup(app, &for_setup_edits);
                external_edit_timer(
                    app.handle().clone(),
                    Arc::clone(&for_setup_places),
                    Arc::clone(&for_setup_state),
                    Arc::clone(&for_setup_edits),
                );
                let _ = &debug_port;
            }

            // 桌面端自己建窗口：additional_browser_args 没有配置项对应，
            // 只能在构造时给，而 CDP 端口就藏在那串参数里。
            //
            // 所以主配置里那条 window 记的是 create: false，仅供桌面读取尺寸；
            // 安卓靠 tauri.android.conf.json 覆盖成 create: true，由 Tauri
            // 在 setup 之前建好 WebView——移动端没有 WebviewWindowBuilder 这条路。
            //
            // 不要改成「先让配置建好、需要 CDP 时再关掉重建」：close() 只是投递
            // 关闭事件，同步接着建会撞上 label 未释放，而等它释放又会让事件循环
            // 认为最后一个窗口已关闭、直接退出进程。
            #[cfg(not(target_os = "android"))]
            {
                // wry 对 additional_browser_args 用 unwrap_or_else：
                // 一旦自定义就会丢掉默认参数，必须把默认值一并带上，
                // 否则 mini menu 与 SmartScreen 的禁用会失效。
                //
                // --remote-allow-origins=* 是必需的：Chromium 会校验
                // WebSocket 握手的 Origin，不在允许列表时返回 403。
                let mut args =
                    String::from("--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection");
                if let Some(port) = &debug_port {
                    args.push_str(&format!(
                        " --remote-debugging-port={port} \
                         --remote-allow-origins=* \
                         --autoplay-policy=no-user-gesture-required"
                    ));
                    eprintln!("[omy] CDP 已启用，端口 {port}");
                }

                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::App("index.html".into()),
                )
                .title("omy")
                .inner_size(1180.0, 780.0)
                .min_inner_size(720.0, 480.0)
                .additional_browser_args(&args)
                .build()?;
            }
            Ok(())
        })
        .build(tauri::generate_context!())
    {
        Ok(app) => app,
        Err(e) => {
            eprintln!("[omy] 启动失败: {e}");
            std::process::exit(1);
        }
    };

    // 退出钩子：用户开了「关闭应用时清空缓存」就在进程退出前清掉密文块。
    // 必须在 ExitRequested 里做而不是靠前端 beforeunload——后者在崩溃、
    // 被系统回收时根本不触发，而那恰恰是最该不留缓存的场景。
    let exit_cache = Arc::clone(&remote_cache);
    let run_edits = Arc::clone(&external_edits);
    let run_places = Arc::clone(&place_registry);
    let run_state = Arc::clone(&shared);
    app.run(move |app_handle, event| {
        if let tauri::RunEvent::ExitRequested { .. } = event {
            let cfg = omy_config::Config::load().unwrap_or_default();
            if cfg.remote.clear_cache_on_exit {
                let freed = exit_cache.clear();
                eprintln!("[omy] 已按设置在退出时清空远程缓存，释放 {freed} 字节");
            }
        }
        if let tauri::RunEvent::Resumed = event {
            let edits = Arc::clone(&run_edits);
            let places = Arc::clone(&run_places);
            let state = Arc::clone(&run_state);
            let app = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let _ = external_edit::sync_all(&app, &places, &state, &edits).await;
            });
        }
    });
}

fn restore_saved_places(
    place_registry: &Arc<places::PlaceRegistry>,
    vaults: &Arc<vault_reg::VaultRegistry>,
) {
    // 迁移旧 Telegram 位置遗留的代理 url：放进共享跨进程锁的读-改-写里，
    // 在磁盘最新配置上改这一处，不整体覆盖。启动时跑一次。
    if let Err(e) = omy_config::Config::update(|fresh| {
        settings::clear_telegram_place_urls(fresh);
        Ok::<(), omy_config::Error>(())
    }) {
        eprintln!("[omy] 清理旧 Telegram 位置代理失败：{e}");
    }
    // 配置读不出来不算错误（首次运行就没有配置），静默用空列表。
    let Ok(cfg) = omy_config::Config::load() else {
        return;
    };
    let (n, need_login) = place_registry.restore(&cfg.remote);
    if n > 0 {
        eprintln!("[omy] 已恢复 {n} 个远程位置，其中 {need_login} 个需要重新登录");
    }

    // 旧版本的单文件 Telegram 登录态迁成第一个 Telegram 位置的。
    if omy_remote::telegram::session::has_legacy_session() {
        match place_registry.telegram_ids().first() {
            Some(first) => match omy_remote::telegram::session::migrate_legacy_to(first) {
                Ok(true) => eprintln!("[omy] 已把旧版 Telegram 登录态迁移到第一个账号"),
                Ok(false) => {}
                Err(e) => eprintln!("[omy] 迁移旧版 Telegram 登录态失败：{e}"),
            },
            None => eprintln!("[omy] 检测到旧版 Telegram 登录态，但没有对应的位置，暂不迁移"),
        }
    }

    // 冷启动就登记 Telegram vault 材料，让同密码跨位置自动解锁仍然生效。
    for tid in place_registry.telegram_ids() {
        if let Ok(Some(v)) = omy_remote::telegram::session::place_vault(&tid) {
            vaults.register_all(&[vault_reg::VaultMaterial {
                salt: v.salt,
                m_kib: v.m_kib,
                t: v.t,
                p: v.p,
            }]);
        }
    }
}

#[cfg(target_os = "android")]
fn external_edit_setup(app: &tauri::App, edits: &Arc<external_edit::ExternalEdits>) {
    let root = match external_edit::native_root(app.handle()) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("[omy] 取不到外部编辑目录: {e}");
            return;
        }
    };
    if let Err(e) = edits.configure(root) {
        eprintln!("[omy] 恢复外部编辑状态失败: {e}");
        return;
    }
    // 进程重启会丢失 Java FileObserver 实例；状态恢复后立即为仍可编辑的稳定
    // 工作文件重建监听。即使重建失败，启动/前台的指纹比较仍是兜底。
    external_edit::restore_watchers(app.handle(), edits);
}

#[cfg(target_os = "android")]
fn external_edit_timer(
    app: tauri::AppHandle,
    places: Arc<places::PlaceRegistry>,
    state: commands::Shared,
    edits: Arc<external_edit::ExternalEdits>,
) {
    tauri::async_runtime::spawn(async move {
        // FileObserver 只标脏，30 秒窗口把同一次保存产生的多组事件合并成一次上传。
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            ticker.tick().await;
            let _ = external_edit::sync_all(&app, &places, &state, &edits).await;
        }
    });
}

/// 安卓启动时的路径准备。
///
/// 设备库（本机身份 + 已配对设备）默认落在 `dirs::config_dir()`，
/// 那个函数在安卓上读 `$HOME`——应用进程里没有这个变量，于是返回
/// `None`，配对功能会直接报「没有可用的存储位置」，局域网共享根本
/// 用不起来。
///
/// 路径从 Tauri 的 `PathResolver` 取而不是硬编码 `/data/data/<包名>`：
/// 包名写在 tauri.conf.json 里，在代码里再写一遍就是两处真相。
#[cfg(target_os = "android")]
fn android_setup(app: &tauri::App, devices: &device_cmds::SharedDevices) -> bool {
    use tauri::Manager as _;

    let (Ok(config), Ok(cache), Ok(data)) = (
        app.path().app_config_dir(),
        app.path().app_cache_dir(),
        app.path().app_data_dir(),
    ) else {
        eprintln!("[omy] 取不到应用私有目录，配置、凭据与设备库将不可用");
        return false;
    };

    for dir in [&config, &cache, &data] {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("[omy] 建应用私有目录失败: {e}");
            return false;
        }
    }
    if let Err(e) = omy_config::set_android_dirs(config.clone(), cache, data) {
        eprintln!("[omy] 注入应用私有目录失败: {e}");
        return false;
    }

    devices.set_default_path(config.join("devices.omy"));
    true
}
