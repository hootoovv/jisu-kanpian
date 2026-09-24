//! Tauri 命令：前端通过 invoke(...) 调用的后端接口。
//!
//! | 命令                    | 作用                                             |
//! |-------------------------|--------------------------------------------------|
//! | scan_directory          | 递归扫描目录（自然排序），返回有序结构           |
//! | read_range              | 按区间读取文件字节（mp4box.js 分析 moov 用）     |
//! | stat_file               | 读取文件总大小（moov 尾部定位用）                |
//! | subtitle_session_open   | MKV 字幕索引会话（引擎 ①：解析 Cues 位置表）     |
//! | subtitle_window         | 按播放窗口直跳取字幕块（引擎 ①，毫秒级）         |
//! | subtitle_close          | 关闭字幕会话（切轨 / 切文件时调用，幂等）        |
//! | extract_mkv_subtitles   | MKV 内嵌字幕全量提取（引擎 ②：原生 EBML 走读）   |
//! | query_extract_progress  | 全量提取进度轮询（前端 await 期间展示百分比）    |
//! | load_state              | 读取上次的观看位置与播放偏好                     |
//! | save_state              | 保存当前观看位置与各项偏好                       |
//! | set_keep_awake          | 播放期间阻止屏保 / 休眠（Wake Lock 不可用时兜底）|
//! | exit_app                | 退出程序（兜底用，正常走窗口 destroy）           |
//!
//! 播放本身不占命令位：前端 <video> 元素通过 asset 协议直接读取
//! 本地文件，seek / 音量 / 暂停全部由 WebView 原生完成，不走 IPC。
//! 多音轨 / 字幕分析也不在后端做：前端 mp4box.js 通过 read_range
//! 只取 moov 元数据区（通常几十 KB~几 MB），避免整文件过 IPC。

use crate::state::PlayerState;

/// 单次区间读取上限（8 MB）：防止误传超大 length 打爆 IPC
const MAX_RANGE_LEN: u64 = 8 * 1024 * 1024;

/// 扫描目录（异步：重扫描在阻塞线程池中执行，不卡 UI）
#[tauri::command]
pub async fn scan_directory(root: String) -> Result<crate::scanner::ScanResult, String> {
    tauri::async_runtime::spawn_blocking(move || crate::scanner::scan(&root))
        .await
        .map_err(|e| format!("扫描任务执行失败：{e}"))?
}

/// 读取文件总大小（moov 尾部定位用）
#[tauri::command]
pub fn stat_file(path: String) -> Result<u64, String> {
    std::fs::metadata(&path)
        .map(|m| m.len())
        .map_err(|e| format!("读取文件信息失败：{e}"))
}

/// 按区间读取文件字节（二进制 IPC，不经 JSON 序列化）。
/// 前端用它做 MP4 顶层 box 游走：moov 在文件头就只读头部，
/// 在文件尾就只读尾部，mdat 媒体数据永不过 IPC。
#[tauri::command]
pub fn read_range(path: String, offset: u64, length: u64) -> Result<tauri::ipc::Response, String> {
    use std::io::{Read, Seek, SeekFrom};

    if length == 0 {
        return Ok(tauri::ipc::Response::new(Vec::new()));
    }
    let len = length.min(MAX_RANGE_LEN);
    let mut file = std::fs::File::open(&path).map_err(|e| format!("打开文件失败：{e}"))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("读取文件信息失败：{e}"))?;
    if offset >= meta.len() {
        return Ok(tauri::ipc::Response::new(Vec::new()));
    }
    let len = len.min(meta.len() - offset);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| format!("定位读取位置失败：{e}"))?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf)
        .map_err(|e| format!("读取文件内容失败：{e}"))?;
    Ok(tauri::ipc::Response::new(buf))
}

/// 打开 MKV 字幕索引会话（引擎 ①：解析 SeekHead + Cues，目标轨位置表
/// 常驻内存，亚秒级）。无可用索引时返回 Err（前端降级全量提取）。
/// 解析在阻塞线程池执行。
#[tauri::command]
pub async fn subtitle_session_open(
    path: String,
    track_number: u64,
) -> Result<crate::mkvsub::SessionInfo, String> {
    tauri::async_runtime::spawn_blocking(move || crate::mkvsub::session_open(&path, track_number))
        .await
        .map_err(|e| format!("会话任务执行失败：{e}"))?
}

/// 取一个播放窗口的字幕块（引擎 ①：按索引直跳，只碰窗口内字节的
/// 几十 KB；毫秒级）。skipped > 0 表示有索引条目未命中，前端应降级
/// 全量提取保证完整性。
#[tauri::command]
pub async fn subtitle_window(
    session_id: u64,
    from_ms: u64,
    to_ms: u64,
) -> Result<crate::mkvsub::WindowResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::mkvsub::session_window(session_id, from_ms, to_ms)
    })
    .await
    .map_err(|e| format!("窗口任务执行失败：{e}"))?
}

/// 关闭字幕索引会话（切轨 / 切文件时调用；幂等）
#[tauri::command]
pub fn subtitle_close(session_id: u64) {
    crate::mkvsub::session_close(session_id);
}

/// 提取 MKV 内嵌字幕轨（引擎 ②：一次性全量走读；无索引文件的兜底
/// 路径，进度经 query_extract_progress 轮询）。均失败时前端回退
/// JS 流式兼容路径（引擎 ③）。
/// 解析在阻塞线程池执行。
#[tauri::command]
pub async fn extract_mkv_subtitles(
    path: String,
    track_number: u64,
) -> Result<crate::mkvsub::ExtractResult, String> {
    crate::mkvsub::progress_begin();
    let out = tauri::async_runtime::spawn_blocking(move || {
        crate::mkvsub::extract_auto(&path, track_number)
    })
    .await
    .map_err(|e| format!("提取任务执行失败：{e}"));
    crate::mkvsub::progress_end();
    out?
}

/// 查询内嵌字幕提取进度（extract_mkv_subtitles 运行期间由前端轮询）
#[tauri::command]
pub fn query_extract_progress() -> crate::mkvsub::ProgressSnapshot {
    crate::mkvsub::progress_snapshot()
}

/// 读取上次会话状态
#[tauri::command]
pub fn load_state() -> PlayerState {
    crate::state::load()
}

/// 保存当前会话状态
#[tauri::command]
pub fn save_state(state: PlayerState) -> Result<(), String> {
    crate::state::save(state)
}

/// 播放期间阻止屏保 / 系统休眠（前端 Wake Lock API 不可用时的兜底）。
/// 幂等：重复 true / false 无副作用；进程退出后各平台自动解除。
#[tauri::command]
pub fn set_keep_awake(active: bool) {
    crate::power::set_keep_awake(active);
}

/// 直接退出程序（兜底方案）
#[tauri::command]
pub fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}
