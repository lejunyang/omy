//! omy 图形界面。
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
//! # 明文不落盘
//!
//! 这是本项目的核心安全目标。GUI 层的做法：
//! 解密只发生在协议处理器里，结果随响应发走后立即丢弃，
//! 既不写临时文件，也不在 `AppState` 里长期持有。
//! 配合响应头的 `no-store`，WebView 也不会把它写进磁盘缓存。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// 与 omy-net 同样的约定：产品代码不允许 panic 路径（GUI 直接面对
// 用户的任意文件，崩溃会丢失正在处理的数据），测试代码另行放宽——
// 测试里的 unwrap 失败就是测试失败，那正是它该做的事。
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)
)]

mod browse;
mod commands;
mod device_cmds;
mod devices;
mod encrypt;
mod lan;
mod mime;
mod protocol;
mod remote;
mod remote_cmds;
mod state;

use state::AppState;
use std::sync::Arc;

fn main() {
    let shared: commands::Shared = Arc::new(AppState::new());
    let for_protocol = Arc::clone(&shared);
    let device_session: device_cmds::SharedDevices = Arc::new(devices::DeviceSession::new());
    let pair_task: device_cmds::SharedPair = Arc::new(lan::PairTask::new());
    let share_task: device_cmds::SharedShare = Arc::new(lan::ShareTask::new());
    let remote_session: Arc<remote::RemoteSession> = Arc::new(remote::RemoteSession::new());
    let for_protocol_remote = Arc::clone(&remote_session);

    // CDP 端口：仅在设了环境变量时开启，供自动化验证用。
    // 默认不开——远程调试端口意味着任何本地进程都能接管这个
    // WebView，而它里面是解密后的内容。
    let debug_port = std::env::var("OMY_GUI_CDP_PORT").ok();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::clone(&shared))
        .manage(Arc::clone(&device_session))
        .manage(Arc::clone(&pair_task))
        .manage(Arc::clone(&share_task))
        .manage(Arc::clone(&remote_session))
        // 必须是**异步**协议：同步版本会阻塞 WebView 线程，
        // 大文件解密时界面直接卡死（Spike S1 实测）
        .register_asynchronous_uri_scheme_protocol("omystream", move |_ctx, request, responder| {
            let st = Arc::clone(&for_protocol);
            let rm = Arc::clone(&for_protocol_remote);
            // 解密可能耗时，必须离开 WebView 线程
            std::thread::spawn(move || {
                responder.respond(protocol::handle(&st, &rm, &request));
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
            commands::get_language,
            commands::set_language,
            commands::list_roots,
            commands::stream_base,
            commands::pick_folder,
            commands::pick_files,
            browse::browse_directory,
            browse::list_places,
            browse::parent_of,
            encrypt::encrypt_paths,
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
        ])
        .setup(move |app| {
            let mut builder = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("omy")
            .inner_size(1180.0, 780.0)
            .min_inner_size(720.0, 480.0);

            if let Some(port) = &debug_port {
                // wry 对 additional_browser_args 用 unwrap_or_else：
                // 一旦自定义就会丢掉默认参数，必须把默认值一并带上，
                // 否则 mini menu 与 SmartScreen 的禁用会失效。
                //
                // --remote-allow-origins=* 是必需的：Chromium 会校验
                // WebSocket 握手的 Origin，不在允许列表时返回 403。
                let args = format!(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                     --remote-debugging-port={port} \
                     --remote-allow-origins=* \
                     --autoplay-policy=no-user-gesture-required"
                );
                eprintln!("[omy] CDP 已启用，端口 {port}");
                builder = builder.additional_browser_args(&args);
            }

            builder.build()?;
            Ok(())
        })
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("[omy] 启动失败: {e}");
        std::process::exit(1);
    }
}
