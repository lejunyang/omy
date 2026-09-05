//! omy 图形界面可执行文件入口。
//!
//! 应用逻辑全部在 [`omy_gui_lib`] 库中，这里只负责在桌面端
//! 调用它启动。移动端没有这个二进制，宿主 Activity 直接加载库。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    omy_gui_lib::run();
}
