//! Tauri 命令：前端通过 invoke(...) 调用的后端接口。
//!
//! | 命令                    | 作用                                             |
//! |-------------------------|--------------------------------------------------|
//! | scan_directory          | 递归扫描目录（自然排序），返回有序结构           |
//! | read_range              | 按区间读取文件字节（mp4box.js 分析 moov 用）     |
//! | stat_file               | 读取文件总大小（moov 尾部定位用）                |
//! | extract_mkv_subtitles   | MKV 内嵌字幕提取（ffmpeg → 原生 EBML 双引擎）    |
//! | query_extract_progress  | 提取进度轮询（前端 await 期间展示百分比）         |
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

/// 提取 MKV 内嵌字幕轨（v1.0.5 三级引擎的前两级：ffmpeg 优先、
/// 原生 EBML 兜底；均失败时前端回退 JS 流式兼容路径）。
/// 解析在阻塞线程池执行；进度经 query_extract_progress 轮询。
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
