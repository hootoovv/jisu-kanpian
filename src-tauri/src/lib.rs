mod commands;
mod mkvsub;
mod power;
mod scanner;
mod state;

use tauri::WindowEvent;

/// 极速看片 应用入口
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .on_window_event(|_window, event| {
            // 兜底：窗口关闭请求时，把最近一次状态再落盘一次
            if let WindowEvent::CloseRequested { .. } = event {
                state::flush();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::scan_directory,
            commands::read_range,
            commands::stat_file,
            commands::extract_mkv_subtitles,
            commands::query_extract_progress,
            commands::load_state,
            commands::save_state,
            commands::set_keep_awake,
            commands::exit_app
        ])
        .run(tauri::generate_context!())
        .expect("极速看片启动失败");
}
