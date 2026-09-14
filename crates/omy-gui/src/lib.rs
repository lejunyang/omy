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
//! 这是本项目的核心安全目标。GUI 层的做法：
//! 解密只发生在协议处理器里，结果随响应发走后立即丢弃，
//! 既不写临时文件，也不在 `AppState` 里长期持有。
//! 配合响应头的 `no-store`，WebView 也不会把它写进磁盘缓存。

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

mod browse;
mod citem;
mod commands;
mod device_cmds;
mod devices;
mod decrypt;
mod encrypt;
mod fileops;
mod keymgmt;
mod lan;
mod mime;
mod plain;
mod protocol;
mod remote;
mod remote_cmds;
mod state;
mod storage;
mod video;

use state::AppState;
use std::sync::Arc;

/// 启动图形界面。
///
/// 桌面端由 `main.rs` 调用；移动端由 Tauri 的 `mobile_entry_point`
/// 宏生成 JNI 入口调用（仅 Android/iOS 构建时启用该属性）。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let shared: commands::Shared = Arc::new(AppState::new());
    let for_protocol = Arc::clone(&shared);
    let device_session: device_cmds::SharedDevices = Arc::new(devices::DeviceSession::new());
    // setup 闭包要用它设置安卓的设备库路径。单独克隆一份：
    // 闭包是 move 的，不能借用外层变量。桌面端不需要这条路——
    // 那里 dirs::config_dir() 本来就能解析出正确位置
    #[cfg(target_os = "android")]
    let for_setup_devices = Arc::clone(&device_session);

    let pair_task: device_cmds::SharedPair = Arc::new(lan::PairTask::new());
    let share_task: device_cmds::SharedShare = Arc::new(lan::ShareTask::new());
    let remote_session: Arc<remote::RemoteSession> = Arc::new(remote::RemoteSession::new());
    let for_protocol_remote = Arc::clone(&remote_session);

    // CDP 端口：仅在设了环境变量时开启，供自动化验证用。
    // 默认不开——远程调试端口意味着任何本地进程都能接管这个
    // WebView，而它里面是解密后的内容。
    let debug_port = std::env::var("OMY_GUI_CDP_PORT").ok();

    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());

    // 安卓的全盘存储访问权限插件。桌面端没有对应概念，整个注册跳过——
    // 不要为了让链式调用整齐而注册一个空插件占位：那会在插件列表里留下
    // 一个永远不做事的条目，之后排查插件相关问题时得先确认它是不是嫌疑人。
    #[cfg(target_os = "android")]
    let builder = builder.plugin(storage::init());

    let result = builder
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
            plain::open_external,
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
            keymgmt::retry_key_files,
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
            #[cfg(target_os = "android")]
            {
                // 安卓的窗口由 tauri.android.conf.json 覆盖成 create: true，
                // Tauri 在 setup 前就按它建好了 WebView，这里只补设备库路径。
                android_setup(app, &for_setup_devices);
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
                let mut args = String::from(
                    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection",
                );
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
        .run(tauri::generate_context!());

    if let Err(e) = result {
        eprintln!("[omy] 启动失败: {e}");
        std::process::exit(1);
    }
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
fn android_setup(app: &tauri::App, devices: &device_cmds::SharedDevices) {
    use tauri::Manager as _;

    let Ok(dir) = app.path().app_config_dir() else {
        eprintln!("[omy] 取不到应用配置目录，设备库将不可用");
        return;
    };

    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("[omy] 建配置目录失败: {e}");
        return;
    }

    devices.set_default_path(dir.join("devices.omy"));
}
