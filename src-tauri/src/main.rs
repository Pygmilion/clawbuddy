// Windows：release 下用 windows 子系统，避免弹出黑色控制台窗口（gateway 日志走文件/诊断导出）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri_app_lib::run()
}
