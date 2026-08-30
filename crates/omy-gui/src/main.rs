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
//!
//! # 明文不落盘
//!
//! 这是本项目的核心安全目标。GUI 层的做法：
//! 解密只发生在协议处理器里，结果随响应发走后立即丢弃，
//! 既不写临时文件，也不在 `AppState` 里长期持有。
//! 配合响应头的 `no-store`，WebView 也不会把它写进磁盘缓存。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod mime;
mod protocol;
mod state;

use state::AppState;
use std::sync::Arc;

fn main() {
    let shared: commands::Shared = Arc::new(AppState::new());
    let for_protocol = Arc::clone(&shared);

    // CDP 端口：仅在设了环境变量时开启，供自动化验证用。
    // 默认不开——远程调试端口意味着任何本地进程都能接管这个
    // WebView，而它里面是解密后的内容。
    let debug_port = std::env::var("OMY_GUI_CDP_PORT").ok();

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::clone(&shared))
        // 必须是**异步**协议：同步版本会阻塞 WebView 线程，
        // 大文件解密时界面直接卡死（Spike S1 实测）
        .register_asynchronous_uri_scheme_protocol("omystream", move |_ctx, request, responder| {
            let st = Arc::clone(&for_protocol);
            // 解密可能耗时，必须离开 WebView 线程
            std::thread::spawn(move || {
                responder.respond(protocol::handle(&st, &request));
            });
        })
        .invoke_handler(tauri::generate_handler![
            commands::vault_params_of,
            commands::unlock,
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
