//! 目录扫描与排序。
//!
//! 遍历规则（对应需求「按字母顺序递归遍历所有子目录」）：
//! 1. 递归遍历所有子目录（不跟随目录符号链接，防止循环）；
//! 2. 每个目录内：先列出本目录的视频文件，再按字母顺序进入子目录
//!    （即「先看本级文件，再进第一个子目录…」的整季连看顺序）；
//! 3. 文件名 / 目录名使用「自然排序」：忽略大小写，数字按数值比较，
//!    因此 ep2.mp4 排在 ep10.mp4 之前；
//! 4. 只收录含有视频文件的目录；没有任何视频文件的空目录不会
//!    出现在分页列表的目录分组里。
//! 5. 每个目录同时收集外挂字幕文件（.vtt / .srt），供前端按
//!    「同名前缀」匹配挂到对应视频上（WebVTT 外挂字幕）。
//!
//! 与听书软件不同的一点：扫描阶段不做任何容器解析——播放交给
//! WebView 的 <video> 元素（asset 协议直读，天然支持 seek 与音量），
//! 多音轨 / 字幕流信息由前端 mp4box.js 在加载时按需分析（见
//! commands.rs 的 read_range）。

use serde::Serialize;
use std::cmp::Ordering;
use std::fs;
use std::path::{Path, PathBuf};

/// 支持的视频文件扩展名（小写，判断时忽略大小写）。
///
/// - mp4 / m4v / webm 为需求点名的常用格式；
/// - mov / 3gp / ogv / ogg / mkv / avi 为尽力支持：能否播放取决于
///   系统 WebView 的解码器，播放不受影响；
/// - m3u8 为 HLS 播放列表（本地目录内的分片流由 hls.js 拉取）。
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "3gp", "webm", "ogv", "ogg", "mkv", "avi", "m3u8",
];

/// 外挂字幕扩展名（.srt 由前端转成 WebVTT 再挂载）
pub const SUB_EXTS: &[&str] = &["vtt", "srt"];

/// 单个视频文件
#[derive(Serialize, Clone, Debug)]
pub struct VideoFile {
    /// 绝对路径
    pub path: String,
    /// 相对主目录的路径（用 / 分隔），用于顶部信息栏展示
    pub rel: String,
    /// 文件名
    pub name: String,
}

/// 单个外挂字幕文件
#[derive(Serialize, Clone, Debug)]
pub struct SubFile {
    /// 绝对路径
    pub path: String,
    /// 相对主目录的路径
    pub rel: String,
    /// 文件名（不含扩展名），用于与视频同名匹配
    pub stem: String,
    /// 扩展名（小写）
    pub ext: String,
}

/// 一个「含有视频文件」的目录
#[derive(Serialize, Clone, Debug)]
pub struct DirectoryInfo {
    /// 绝对路径
    pub path: String,
    /// 相对主目录的路径
    pub rel: String,
    /// 本目录内的视频文件（已排序）
    pub files: Vec<VideoFile>,
    /// 本目录内的外挂字幕文件（已排序，不含视频的目录也可能有）
    pub subs: Vec<SubFile>,
}

/// 扫描结果
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    /// 规范化后的主目录
    pub root: String,
    /// 有序目录列表（只含至少有一个视频文件的目录）
    pub directories: Vec<DirectoryInfo>,
    /// 视频文件总数
    pub total_files: usize,
}

/// 判断文件名是否为受支持的视频文件
fn is_video(name: &str) -> bool {
    let Some((stem, ext)) = split_ext(name) else {
        return false;
    };
    !stem.is_empty() && VIDEO_EXTS.contains(&ext.as_str())
}

/// 判断文件名是否为外挂字幕文件
fn is_sub(name: &str) -> bool {
    let Some((stem, ext)) = split_ext(name) else {
        return false;
    };
    !stem.is_empty() && SUB_EXTS.contains(&ext.as_str())
}

/// 拆出（不含点的词干, 小写扩展名）；`.hidden` 之类的隐藏文件返回 None
fn split_ext(name: &str) -> Option<(String, String)> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None; // ".vtt" 之类的隐藏文件不算字幕 / 视频文件
    }
    let ext = name[dot + 1..].to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    Some((name[..dot].to_string(), ext))
}

fn path_to_string(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// 递归扫描
fn scan_dir(abs: &Path, rel: &str, out: &mut Vec<DirectoryInfo>, depth: usize) {
    if depth > 64 {
        return; // 防御过深目录
    }
    let entries = match fs::read_dir(abs) {
        Ok(e) => e,
        Err(_) => return, // 无权限等：直接跳过该目录
    };

    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut subs: Vec<(String, PathBuf)> = Vec::new();
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();

    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if ft.is_dir() {
            subdirs.push((name, entry.path()));
        } else if ft.is_file() {
            if is_video(&name) {
                files.push((name, entry.path()));
            } else if is_sub(&name) {
                subs.push((name, entry.path()));
            }
        }
        // 注意：符号链接（目录或文件）不跟随，避免目录环
    }

    files.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    subs.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    subdirs.sort_by(|a, b| natural_cmp(&a.0, &b.0));

    let dir_files: Vec<VideoFile> = files
        .into_iter()
        .map(|(name, path)| VideoFile {
            rel: if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") },
            path: path_to_string(&path),
            name,
        })
        .collect();

    if !dir_files.is_empty() {
        let dir_subs: Vec<SubFile> = subs
            .into_iter()
            .filter_map(|(name, path)| {
                let (stem, ext) = split_ext(&name)?;
                Some(SubFile {
                    rel: if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") },
                    path: path_to_string(&path),
                    stem,
                    ext,
                })
            })
            .collect();
        out.push(DirectoryInfo {
            path: path_to_string(abs),
            rel: rel.to_string(),
            files: dir_files,
            subs: dir_subs,
        });
    }

    for (name, path) in subdirs {
        let child_rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        scan_dir(&path, &child_rel, out, depth + 1);
    }
}

/// 对外入口：扫描一个根目录。
/// 若传入的是文件路径，则自动改用其所在目录（对拖拽文件更友好）。
pub fn scan(root: &str) -> Result<ScanResult, String> {
    let p = PathBuf::from(root);
    let dir = if p.is_dir() {
        p
    } else if p.is_file() {
        let parent = p
            .parent()
            .ok_or_else(|| "无法获取父目录".to_string())?
            .to_path_buf();
        if parent.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            parent
        }
    } else {
        return Err(format!("路径不存在：{root}"));
    };

    let mut directories = Vec::new();
    scan_dir(&dir, "", &mut directories, 0);
    let total_files: usize = directories.iter().map(|d| d.files.len()).sum();

    Ok(ScanResult {
        root: path_to_string(&dir),
        directories,
        total_files,
    })
}

/// 自然排序比较：忽略大小写；连续数字按数值大小比较。
/// natural_cmp("ep2.mp4", "ep10.mp4") == Less
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().flat_map(char::to_lowercase).collect();
    let b: Vec<char> = b.chars().flat_map(char::to_lowercase).collect();
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            // 各自截取整段数字
            let sa = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sb = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let da: String = a[sa..i].iter().collect();
            let db: String = b[sb..j].iter().collect();
            // 数值比较：先去掉前导零比长度，再比字典序
            let ta = da.trim_start_matches('0');
            let tb = db.trim_start_matches('0');
            match ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)) {
                Ordering::Equal => { /* 数值相等则继续向后比较 */ }
                other => return other,
            }
        } else {
            match a[i].cmp(&b[j]) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                other => return other,
            }
        }
    }
    // 先结束（更短）者排前面
    (a.len() - i).cmp(&(b.len() - j))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_basics() {
        assert_eq!(natural_cmp("ep2.mp4", "ep10.mp4"), Ordering::Less);
        assert_eq!(natural_cmp("EP2.MP4", "ep10.mp4"), Ordering::Less); // 忽略大小写
        assert_eq!(natural_cmp("a.mp4", "a.mp4"), Ordering::Equal);
        assert_eq!(natural_cmp("a1.mp4", "a2.mp4"), Ordering::Less);
        assert_eq!(natural_cmp("b.mp4", "a10.mp4"), Ordering::Greater);
        assert_eq!(natural_cmp("0007.mp4", "7.mp4"), Ordering::Equal); // 前导零不影响数值
        assert_eq!(natural_cmp("a.mp4", "a1.mp4"), Ordering::Less);
        assert_eq!(natural_cmp("第3集.mp4", "第21集.mp4"), Ordering::Less); // 非 ASCII 前缀 + 数字
    }

    #[test]
    fn scan_orders_files_then_dirs() {
        let base = std::env::temp_dir().join(format!("jkp-scan-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let root = base.join("root");
        fs::create_dir_all(root.join("zz")).unwrap();
        fs::create_dir_all(root.join("aa")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        for f in ["b.mp4", "a.mp4", "ep10.mp4", "ep2.mp4", "c.txt"] {
            fs::write(root.join(f), b"x").unwrap();
        }
        fs::write(root.join("b.vtt"), b"x").unwrap(); // 外挂字幕与视频同名
        fs::write(root.join("zz").join("c.mkv"), b"x").unwrap();
        fs::write(root.join("aa").join("d.webm"), b"x").unwrap();
        fs::write(root.join("aa").join("e.txt"), b"x").unwrap(); // 非视频文件应被忽略

        let res = scan(root.to_str().unwrap()).unwrap();
        assert_eq!(res.total_files, 6);
        assert_eq!(res.directories.len(), 3); // root、aa、zz（empty 没视频文件，跳过）
        assert_eq!(res.directories[0].rel, "");
        let names: Vec<&str> = res.directories[0].files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["a.mp4", "b.mp4", "ep2.mp4", "ep10.mp4"]);
        assert_eq!(res.directories[0].subs.len(), 1); // b.vtt 被收录
        assert_eq!(res.directories[0].subs[0].stem, "b");
        assert_eq!(res.directories[1].rel, "aa");
        assert_eq!(res.directories[1].files.len(), 1);
        assert_eq!(res.directories[2].rel, "zz");
        assert_eq!(res.directories[2].files[0].rel, "zz/c.mkv");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_accepts_file_path_uses_parent() {
        let base = std::env::temp_dir().join(format!("jkp-file-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("x.mp4"), b"x").unwrap();
        let res = scan(base.join("x.mp4").to_str().unwrap()).unwrap();
        assert_eq!(res.total_files, 1);
        assert!(res.root.ends_with(base.file_name().unwrap().to_string_lossy().as_ref()));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_ext_case_insensitive() {
        assert!(is_video("A.MP4"));
        assert!(is_video("Movie.MOV"));
        assert!(is_video("x.webm"));
        assert!(is_video("stream.m3u8"));
        assert!(!is_video("x.txt"));
        assert!(!is_video(".mp4")); // 隐藏文件
        assert!(!is_video("mp4")); // 无扩展名
        assert!(is_sub("CHS.SRT"));
        assert!(is_sub("note.vtt"));
        assert!(!is_sub("x.ass")); // v1 只支持 vtt / srt
    }
}
