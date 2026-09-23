//! 会话状态持久化：记住上次观看的主目录 / 文件 / 位置，
//! 以及播放偏好（音量 / 旋转 / 缩放 / 语言偏好 / 各浮层可见性），
//! 另外为**每个文件**单独记住播放位置（播放完毕的自动清空）。
//!
//! 状态文件位置（用户配置目录）：
//! - Windows: %APPDATA%\jisu-kanpian\state.json
//! - macOS:   ~/Library/Application Support/jisu-kanpian/state.json
//! - Linux:   ~/.config/jisu-kanpian/state.json
//!
//! 前端在播放过程中防抖调用 save_state 落盘；
//! 窗口关闭事件（CloseRequested）时后端再兜底写一次。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// 播放器状态（snake_case 序列化，与前端 invoke 字段一一对应）
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct PlayerState {
    /// 上次的主目录
    pub root: Option<String>,
    /// 上次正在观看的文件（绝对路径）
    pub file: Option<String>,
    /// 上次退出时该文件的播放位置（毫秒）
    #[serde(default)]
    pub position_ms: Option<u64>,
    /// 音量（0.0 ~ 1.0）
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// 画面旋转角度（0 / 90 / 180 / 270，顺时针）
    #[serde(default)]
    pub rotation: u16,
    /// 画面缩放倍数（1.0 = 原始大小）
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    /// 音轨语言偏好（ISO 639 代码，默认界面语言 zh）
    #[serde(default = "default_lang")]
    pub audio_lang: String,
    /// 字幕语言偏好（ISO 639 代码，默认界面语言 zh）
    #[serde(default = "default_lang")]
    pub subtitle_lang: String,
    /// 顶部信息栏是否可见（默认显示）
    #[serde(default = "default_true")]
    pub info_visible: bool,
    /// 底部控制栏是否可见（默认显示）
    #[serde(default = "default_true")]
    pub ctrl_visible: bool,
    /// 右侧文件列表浮层是否可见（T 键切换，默认显示）
    #[serde(default = "default_true")]
    pub list_visible: bool,
    /// 每个文件的播放位置（毫秒），键为绝对路径；播放完毕的条目会被清空
    #[serde(default)]
    pub positions: HashMap<String, u64>,
}

fn default_volume() -> f32 {
    1.0
}
fn default_zoom() -> f32 {
    1.0
}
fn default_lang() -> String {
    "zh".into()
}
fn default_true() -> bool {
    true
}

impl Default for PlayerState {
    fn default() -> Self {
        PlayerState {
            root: None,
            file: None,
            position_ms: None,
            volume: default_volume(),
            rotation: 0,
            zoom: default_zoom(),
            audio_lang: default_lang(),
            subtitle_lang: default_lang(),
            info_visible: true,
            ctrl_visible: true,
            list_visible: true,
            positions: HashMap::new(),
        }
    }
}

fn state_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("jisu-kanpian"))
}

fn state_file() -> Option<PathBuf> {
    state_dir().map(|d| d.join("state.json"))
}

/// 内存里的最近一次状态（供窗口关闭时兜底落盘）
fn last_state() -> &'static Mutex<Option<PlayerState>> {
    static LAST: OnceLock<Mutex<Option<PlayerState>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

pub fn load() -> PlayerState {
    let Some(path) = state_file() else {
        return PlayerState::default();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return PlayerState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save(state: PlayerState) -> Result<(), String> {
    if let Ok(mut cell) = last_state().lock() {
        *cell = Some(state.clone());
    }
    let Some(dir) = state_dir() else {
        return Err("无法定位用户配置目录".into());
    };
    fs::create_dir_all(&dir).map_err(|e| format!("创建配置目录失败：{e}"))?;
    let text = serde_json::to_string_pretty(&state).map_err(|e| format!("序列化失败：{e}"))?;
    fs::write(dir.join("state.json"), text).map_err(|e| format!("写入状态文件失败：{e}"))
}

/// 兜底保存：窗口关闭事件里调用
pub fn flush() {
    let snapshot = match last_state().lock() {
        Ok(g) => g.clone(),
        Err(_) => return,
    };
    if let Some(s) = snapshot {
        let _ = save(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_roundtrip() {
        let mut s = PlayerState {
            root: Some("/a/movies".into()),
            file: Some("/a/movies/s01/1.mp4".into()),
            position_ms: Some(65000),
            volume: 0.35,
            rotation: 90,
            zoom: 1.25,
            audio_lang: "zh".into(),
            subtitle_lang: "off".into(),
            info_visible: false,
            ctrl_visible: true,
            list_visible: false,
            positions: HashMap::new(),
        };
        s.positions.insert("/a/movies/s01/1.mp4".into(), 65000);
        s.positions.insert("/a/movies/s01/2.mp4".into(), 12000);

        let text = serde_json::to_string(&s).unwrap();
        let back: PlayerState = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);

        let empty: PlayerState = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, PlayerState::default());
        assert!((empty.volume - 1.0).abs() < 1e-6);
        assert!(empty.info_visible && empty.ctrl_visible);
        assert!(empty.list_visible); // T 键浮层默认显示
        assert_eq!(empty.rotation, 0);
        assert!((empty.zoom - 1.0).abs() < 1e-6);
        assert_eq!(empty.audio_lang, "zh");
    }

    #[test]
    fn state_reads_v1_legacy_json() {
        // 只有三件套的旧状态文件：其余字段回退默认值
        let legacy = r#"{"root":"/a","file":"/a/1.mp4","position_ms":3000}"#;
        let st: PlayerState = serde_json::from_str(legacy).unwrap();
        assert_eq!(st.root.as_deref(), Some("/a"));
        assert_eq!(st.file.as_deref(), Some("/a/1.mp4"));
        assert_eq!(st.position_ms, Some(3000));
        assert!((st.volume - 1.0).abs() < 1e-6);
        assert_eq!(st.rotation, 0);
        assert_eq!(st.audio_lang, "zh"); // 未记录时回退界面语言
        assert!(st.positions.is_empty());
        assert!(st.list_visible);
    }

    #[test]
    fn state_ignores_unknown_fields() {
        // 未来版本新增字段（或旧版本残留字段）被 serde 自动忽略，无损升级
        // 注意：内容含 "#（颜色值），必须用 r##"…"## 双井号定界，否则 "# 会提前终止字符串
        let future = r##"{"root":"/a","volume":0.5,"appearance":{"bg":"#000"},"speed":1.5}"##;
        let st: PlayerState = serde_json::from_str(future).unwrap();
        assert_eq!(st.root.as_deref(), Some("/a"));
        assert!((st.volume - 0.5).abs() < 1e-6);
        assert_eq!(st.rotation, 0);
        // 再落盘时未知字段不再出现
        let text = serde_json::to_string(&st).unwrap();
        assert!(!text.contains("appearance"));
        assert!(!text.contains("speed"));
    }
}
