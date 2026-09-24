//! 极速看片 —— MKV 内嵌字幕提取（后端双引擎：ffmpeg 优先 / 原生 EBML 兜底）
//!
//! 背景：v1.0.4 把 MKV 字幕提取搬进了前端流式（read_range 分块喂
//! matroska-subtitles），解决了「文件过大」的硬限制，但 5GB 级文件需要
//! 把全部字节经 IPC 传进 WebView 再用 JS 解析，速度受 IPC 序列化与 JS
//! 单线程解析双重拖累（分钟级）。本模块把提取下沉到 Rust 侧：
//!
//! 引擎 1（ffmpeg）：机器上装了 ffmpeg 时优先使用（跨平台显式解析：
//!   Windows 探测 PATH 与应用目录下的 ffmpeg.exe，Unix 找 PATH 里的
//!   ffmpeg；逐候选 `-version` 验证，坏的应用执行别名自动跳过）——
//!   `ffmpeg -i <file> -map 0:s:<n> -c:s copy -f srt/ass/webvtt pipe:1`
//!   纯 demux + copy，无转码，5GB 文件秒级完成；输出直接是完整字幕
//!   文档（srt/ass/vtt 文本），前端零解析成本。
//! 引擎 2（原生 EBML）：机器上没有 ffmpeg 时使用——手写流式 EBML
//!   游走（与前端 tracks.js 同一套元素子集），顺序扫描 Segment 的
//!   Cluster/BlockGroup，只把目标字幕轨的块 payload 读进内存（通常
//!   几 KB~几 MB），视频/音频块用 seek 直接跳过。解析在原生代码里
//!   完成，速度只受磁盘顺序读限制，比「JS + IPC」快 1~2 个数量级。
//! 引擎 3（前端 JS 流式）：以上两者都失败（如罕见的 lacing 分帧 /
//!   ffmpeg 损坏）时，由前端 subtitles.js 回退到 v1.0.4 的兼容路径。
//!
//! 语义对齐（与 matroska-subtitles 完全一致，保证两条路径产出相同）：
//! - 字幕事件只来自 BlockGroup(0xA0)→Block(0xA1)（带 BlockDuration
//!   0x9B），SimpleBlock(0xA3) 不产字幕（ffmpeg / mkvmerge 均按此封装）；
//! - time = (Cluster.Timestamp + Block 相对时码) × TimecodeScale / 1e6（ms）；
//! - duration = BlockDuration × TimecodeScale / 1e6（ms）；
//! - ASS/SSA 块 payload 是逗号分隔字段：
//!   [ReadOrder,]Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text
//!   （SSA 比 ASS 多跳过一个前导字段），Text 内的逗号重新拼回。
//!
//! 进度：提取在 spawn_blocking 里跑，进度经原子静态暴露给
//! query_extract_progress 命令，前端 await 期间轮询展示百分比
//! （原生引擎按文件偏移；ffmpeg 引擎无法取内部进度，展示引擎名）。

use serde::Serialize;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::Duration;

/* ================= 进度（前端轮询） ================= */

const M_NONE: u8 = 0;
const M_FFMPEG: u8 = 1;
const M_NATIVE: u8 = 2;

static P_RUNNING: AtomicBool = AtomicBool::new(false);
static P_METHOD: AtomicU8 = AtomicU8::new(M_NONE);
static P_BYTES: AtomicU64 = AtomicU64::new(0);
static P_TOTAL: AtomicU64 = AtomicU64::new(0);

/// query_extract_progress 的返回（camelCase，前端直接消费）
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct ProgressSnapshot {
    pub running: bool,
    /// none | ffmpeg | native
    pub method: &'static str,
    pub bytes: u64,
    pub total: u64,
}

/// 提取任务开始（extract_mkv_subtitles 命令入口调用）
pub fn progress_begin() {
    P_RUNNING.store(true, Ordering::SeqCst);
    P_METHOD.store(M_NONE, Ordering::SeqCst);
    P_BYTES.store(0, Ordering::SeqCst);
    P_TOTAL.store(0, Ordering::SeqCst);
}

/// 提取任务结束（含失败路径，命令出口调用）
pub fn progress_end() {
    P_RUNNING.store(false, Ordering::SeqCst);
}

/// 当前进度快照
pub fn progress_snapshot() -> ProgressSnapshot {
    ProgressSnapshot {
        running: P_RUNNING.load(Ordering::SeqCst),
        method: match P_METHOD.load(Ordering::SeqCst) {
            M_FFMPEG => "ffmpeg",
            M_NATIVE => "native",
            _ => "none",
        },
        bytes: P_BYTES.load(Ordering::SeqCst),
        total: P_TOTAL.load(Ordering::SeqCst),
    }
}

fn set_method(m: u8) {
    P_METHOD.store(m, Ordering::SeqCst);
}

fn set_bytes(b: u64) {
    P_BYTES.store(b, Ordering::SeqCst);
}

fn set_total(t: u64) {
    P_TOTAL.store(t, Ordering::SeqCst);
}

/* ================= 结果类型（serde → JSON） ================= */

/// 一条字幕块（native 引擎产出；字段与 matroska-subtitles 对齐）
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SubBlock {
    /// 起始时间（ms）
    pub time: u64,
    /// 时长（ms，来自 BlockDuration；0 的块会被前端过滤）
    pub duration: u64,
    /// 文本（utf8/webvtt 即全文；ass 为 Dialogue 的 Text 字段）
    pub text: String,
    // 以下为 ASS/SSA 的 Dialogue 字段（重组 ASS 文档用，见前端 ass.js）
    pub layer: String,
    pub style: String,
    pub name: String,
    #[serde(rename = "marginL")]
    pub margin_l: String,
    #[serde(rename = "marginR")]
    pub margin_r: String,
    #[serde(rename = "marginV")]
    pub margin_v: String,
    pub effect: String,
}

/// extract_mkv_subtitles 的返回
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResult {
    /// ffmpeg | native（前端 ③ 兜底时为 js，由前端自行标记）
    pub method: &'static str,
    /// utf8 | webvtt | ass
    pub kind: String,
    /// ffmpeg 引擎：完整字幕文档文本（srt / ass / vtt）
    pub text: Option<String>,
    /// native 引擎：ASS 头（CodecPrivate）
    pub header: Option<String>,
    /// native 引擎：字幕块列表
    pub blocks: Option<Vec<SubBlock>>,
}

/* ================= Matroska 元素 ID（本模块用到的子集） ================= */
// （跳过而不解析的元素不占常量位：EBML 头 0x1A45DFA3、SimpleBlock
//  0xA3 等在走读的 else/skip 分支按「未识别即跳过」处理，见注释）
const ID_SEGMENT: u64 = 0x18538067;
const ID_INFO: u64 = 0x1549a966;
const ID_TIMECODE_SCALE: u64 = 0x2ad7b1;
const ID_TRACKS: u64 = 0x1654ae6b;
const ID_TRACK_ENTRY: u64 = 0xae;
const ID_TRACK_NUMBER: u64 = 0xd7;
const ID_TRACK_TYPE: u64 = 0x83;
const ID_CODEC_ID: u64 = 0x86;
const ID_CODEC_PRIVATE: u64 = 0x63a2;
const ID_CLUSTER: u64 = 0x1f43b675;
const ID_TIMESTAMP: u64 = 0xe7;
const ID_BLOCK_GROUP: u64 = 0xa0;
const ID_BLOCK: u64 = 0xa1;
const ID_BLOCK_DURATION: u64 = 0x9b;

/// TrackType：0x11 = 字幕
const TRACK_SUBTITLE: u64 = 0x11;

/// 顺序读缓冲（1MB：块头解析的 syscall 密度与顺序读吞吐的折中）
const BUF: usize = 1 << 20;
/// 目标块 payload 上限（正常字幕块仅几 KB；防御损坏文件的天量分配）
const MAX_SUB_PAYLOAD: u64 = 64 << 20;
/// ffmpeg 整体超时（demux 50GB @ HDD 量级的保守值）
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(600);

/// CodecID → 字幕类型（与前端 tracks.js subtitleKindOf 一致）
fn kind_of(codec_id: &str) -> Option<&'static str> {
    let c = codec_id.trim().to_ascii_uppercase();
    match c.as_str() {
        "S_TEXT/UTF8" => Some("utf8"),
        "S_TEXT/WEBVTT" | "S_WEBVTT" => Some("webvtt"),
        "S_TEXT/ASS" | "S_ASS" | "S_TEXT/SSA" | "S_SSA" => Some("ass"),
        _ => None, // PGS / VOBSub / KATE 等图形或未适配格式
    }
}

/// SSA（而非 ASS）：Dialogue 前导字段数不同
fn is_ssa(codec_id: &str) -> bool {
    let c = codec_id.trim().to_ascii_uppercase();
    c == "S_TEXT/SSA" || c == "S_SSA"
}

/* ================= EBML 基元（流式读取） ================= */

/// 首字节标记位定出 vint 宽度（1..=8）；0x00 无标记位 → None
fn marker_len(first: u8) -> Option<u8> {
    for i in 0..8 {
        if first & (0x80 >> i) != 0 {
            return Some(i + 1);
        }
    }
    None
}

/// 读一个 EBML 元素 ID（保留标记位的原值）。
/// 干净 EOF（0 字节）或首字节无标记位（填充 0x00）→ Ok(None)。
fn read_vint_id(r: &mut impl Read) -> io::Result<Option<(u64, u8)>> {
    let mut b = [0u8; 1];
    if r.read(&mut b)? == 0 {
        return Ok(None);
    }
    let Some(len) = marker_len(b[0]) else {
        return Ok(None); // 无标记位：按流末尾处理（尾部填充）
    };
    if len == 1 {
        return Ok(Some((b[0] as u64, 1)));
    }
    let mut rest = vec![0u8; len as usize - 1];
    r.read_exact(&mut rest)?;
    let mut v = b[0] as u64;
    for &x in &rest {
        v = (v << 8) | x as u64;
    }
    Ok(Some((v, len)))
}

/// 读一个 EBML 尺寸 vint（剥标记位取数值）。
/// 返回 (value, 字节数, 是否未知尺寸)。未知尺寸两种形态：
/// 全 1 数据位（RFC 8794）与 ffmpeg 的 8 字节 `01 00…00`。
/// ⚠️ 必须按字节剥标记位再累积——8 字节 vint 原值超 JS 安全整数，
/// 逐字节处理才不丢精度（前端 tracks.js 同款教训，Rust 侧保持一致）。
fn read_vint_size(r: &mut impl Read) -> io::Result<Option<(u64, u8, bool)>> {
    let mut b = [0u8; 1];
    if r.read(&mut b)? == 0 {
        return Ok(None);
    }
    let first = b[0];
    let Some(len) = marker_len(first) else {
        return Ok(None);
    };
    let len = len as usize;
    // ⚠️ len=8（ffmpeg 未知尺寸形态 01 00…00）时 0xff>>8 会溢出 panic，须特判
    let first_mask: u8 = if len >= 8 { 0 } else { 0xff >> len };
    let mut v = (first & first_mask) as u64;
    let mut all_ones = first & first_mask == first_mask;
    let mut ffmpeg_form = len == 8 && first == 0x01;
    for _ in 1..len {
        let mut x = [0u8; 1];
        r.read_exact(&mut x)?;
        if x[0] != 0xff {
            all_ones = false;
        }
        if x[0] != 0x00 {
            ffmpeg_form = false;
        }
        v = (v << 8) | x[0] as u64;
    }
    if len == 1 {
        // 单字节 0xFF：数据位全 1
        all_ones = first == 0xff;
        ffmpeg_form = false;
    }
    Ok(Some((v, len as u8, all_ones || ffmpeg_form)))
}

/// 读无符号整数字段值（尺寸 > 8 时只保留低 8 字节的稳健处理）
fn read_uint(r: &mut impl Read, size: u64) -> io::Result<u64> {
    let mut head = [0u8; 8];
    let head_len = size.min(8) as usize;
    if head_len > 0 {
        r.read_exact(&mut head[..head_len])?;
    }
    let mut discard = [0u8; 64];
    let mut left = size.saturating_sub(8);
    while left > 0 {
        let take = left.min(discard.len() as u64) as usize;
        r.read_exact(&mut discard[..take])?;
        left -= take as u64;
    }
    let mut v = 0u64;
    for i in 0..head_len {
        v = (v << 8) | head[i] as u64;
    }
    Ok(v)
}

/// 读定长字节串
fn read_bytes(r: &mut impl Read, size: u64) -> io::Result<Vec<u8>> {
    if size > MAX_SUB_PAYLOAD {
        // 通用读路径的防御：正常元素（字符串/私有数据/字幕块）远小于
        // 64MB；超大尺寸只可能来自损坏容器，直接拒绝防止天量分配
        return Err(io::Error::new(io::ErrorKind::InvalidData, "元素尺寸异常"));
    }
    let mut v = vec![0u8; size as usize];
    if size > 0 {
        r.read_exact(&mut v)?;
    }
    Ok(v)
}

/// 读字符串字段（UTF-8 宽容解码，剥尾部 NUL）
fn read_string(r: &mut impl Read, size: u64) -> io::Result<String> {
    let raw = read_bytes(r, size)?;
    let s = String::from_utf8_lossy(&raw);
    Ok(s.trim_end_matches('\0').to_string())
}

/// 元素头
struct Elem {
    id: u64,
    size: u64,
    unknown: bool,
    /// 数据区起始（含头长度后的绝对偏移）
    data_pos: u64,
}

/// 读元素头（ID + 尺寸）；EOF / 填充 → None
fn read_elem(r: &mut BufReader<File>) -> io::Result<Option<Elem>> {
    let Some((id, _)) = read_vint_id(r)? else {
        return Ok(None);
    };
    let Some((size, _, unknown)) = read_vint_size(r)? else {
        return Ok(None);
    };
    let data_pos = r.stream_position()?;
    Ok(Some(Elem { id, size, unknown, data_pos }))
}

/// 元素数据区终点（未知尺寸 → 层边界；越界 → 夹到层边界）
fn elem_end(e: &Elem, bound: u64) -> u64 {
    if e.unknown {
        bound
    } else {
        e.data_pos.saturating_add(e.size).min(bound)
    }
}

/// 对齐到绝对偏移（跳过任意距离；BufReader::seek 自带缓冲内调整）
fn skip_to(r: &mut BufReader<File>, pos: u64) -> io::Result<()> {
    if r.stream_position()? != pos {
        r.seek(SeekFrom::Start(pos))?;
    }
    Ok(())
}

/* ================= 走读 ================= */

/// 轨道表条目（TrackEntry 感兴趣的子集）
#[derive(Clone, Debug, Default)]
struct TrackMeta {
    number: u64,
    kind: u64,
    codec_id: String,
    codec_private: Vec<u8>,
}

/// 走读累积状态
#[derive(Default)]
struct WalkOut {
    /// Info.TimecodeScale（ns/单位，默认 1e6）
    scale: u64,
    /// Tracks.TrackEntry 列表（按容器顺序 = ffmpeg 流顺序）
    metas: Vec<TrackMeta>,
    /// 目标轨字幕块（full 模式）
    blocks: Vec<SubBlock>,
    /// 目标轨是否找到（full 模式，Tracks 解析后）
    t_found: bool,
    t_kind: String,
    t_ssa: bool,
    t_private: Vec<u8>,
}

impl WalkOut {
    fn new() -> Self {
        Self { scale: 1_000_000, ..Default::default() }
    }
}

/// 顶层走读入口。
/// `target = None`：头模式——解析到 Tracks 完成即止（ffmpeg 引擎选轨用）；
/// `target = Some(n)`：全量模式——继续扫全部 Cluster 收集目标轨字幕块。
fn walk_mkv(r: &mut BufReader<File>, file_len: u64, target: Option<u64>) -> Result<WalkOut, String> {
    let mut out = WalkOut::new();
    loop {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let end = elem_end(&e, file_len);
        if e.id == ID_SEGMENT {
            walk_segment(r, end, target, &mut out)?;
            if target.is_none() && !out.metas.is_empty() {
                break; // 头模式：Tracks 已到手
            }
        } else {
            // EBML 头 / SeekHead / 尾部垃圾：跳过
            skip_to(r, end).map_err(perr)?;
        }
        if r.stream_position().map_err(perr)? >= file_len {
            break;
        }
    }
    Ok(out)
}

fn walk_segment(
    r: &mut BufReader<File>,
    seg_end: u64,
    target: Option<u64>,
    out: &mut WalkOut,
) -> Result<(), String> {
    let mut saw_tracks = false;
    while r.stream_position().map_err(perr)? < seg_end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let end = elem_end(&e, seg_end);
        if e.data_pos >= seg_end {
            break; // 头已越界（损坏）：交给外层对齐
        }
        match e.id {
            ID_INFO => walk_info(r, end, out)?,
            ID_TRACKS => {
                walk_tracks(r, end, out)?;
                saw_tracks = true;
                if let Some(n) = target {
                    // 目标轨信息就位（后续 Cluster 组装 ASS 字段用）
                    if let Some(m) = out.metas.iter().find(|m| m.number == n) {
                        out.t_found = true;
                        out.t_kind = kind_of(&m.codec_id).unwrap_or("").to_string();
                        out.t_ssa = is_ssa(&m.codec_id);
                        out.t_private = m.codec_private.clone();
                    }
                }
            }
            ID_CLUSTER => {
                if target.is_none() {
                    if saw_tracks {
                        break; // 头模式完成（Tracks 已在手，Cluster 不再读）
                    }
                    // 病态文件（Tracks 在 Cluster 之后）：跳过继续找
                    skip_to(r, end).map_err(perr)?;
                } else if let Some(n) = target {
                    walk_cluster(r, end, n, out)?;
                    set_bytes(r.stream_position().map_err(perr)?); // 进度按文件偏移
                } else {
                    skip_to(r, end).map_err(perr)?;
                }
            }
            _ => skip_to(r, end).map_err(perr)?, // SeekHead/Cues/Tags/Chapters/Void…
        }
    }
    skip_to(r, seg_end).map_err(perr)?; // 早退 / 异常路径统一对齐到层边界
    Ok(())
}

fn walk_info(r: &mut BufReader<File>, end: u64, out: &mut WalkOut) -> Result<(), String> {
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_TIMECODE_SCALE {
            out.scale = read_uint(r, e.size).map_err(perr)?.max(1);
        } else {
            skip_to(r, child_end).map_err(perr)?;
        }
    }
    skip_to(r, end).map_err(perr)?;
    Ok(())
}

fn walk_tracks(r: &mut BufReader<File>, end: u64, out: &mut WalkOut) -> Result<(), String> {
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_TRACK_ENTRY {
            walk_track_entry(r, child_end, out)?;
        } else {
            skip_to(r, child_end).map_err(perr)?;
        }
    }
    skip_to(r, end).map_err(perr)?;
    Ok(())
}

fn walk_track_entry(r: &mut BufReader<File>, end: u64, out: &mut WalkOut) -> Result<(), String> {
    let mut m = TrackMeta::default();
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        match e.id {
            ID_TRACK_NUMBER => m.number = read_uint(r, e.size).map_err(perr)?,
            ID_TRACK_TYPE => m.kind = read_uint(r, e.size).map_err(perr)?,
            ID_CODEC_ID => m.codec_id = read_string(r, e.size).map_err(perr)?,
            ID_CODEC_PRIVATE => m.codec_private = read_bytes(r, e.size).map_err(perr)?,
            _ => skip_to(r, child_end).map_err(perr)?,
        }
    }
    skip_to(r, end).map_err(perr)?;
    out.metas.push(m);
    Ok(())
}

fn walk_cluster(
    r: &mut BufReader<File>,
    end: u64,
    target: u64,
    out: &mut WalkOut,
) -> Result<(), String> {
    let mut cluster_tc: u64 = 0;
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_TIMESTAMP {
            cluster_tc = read_uint(r, e.size).map_err(perr)?;
        } else if e.id == ID_BLOCK_GROUP {
            walk_block_group(r, child_end, cluster_tc, target, out)?;
        } else {
            // SimpleBlock(0xA3) 也走这里被跳过——与 matroska-subtitles
            // 语义一致：字幕事件只来自 BlockGroup（见模块注释）
            skip_to(r, child_end).map_err(perr)?;
        }
    }
    skip_to(r, end).map_err(perr)?;
    Ok(())
}

/// Block 头（track vint + int16 相对时码 + flags），返回 (track, 相对时码, flags, payload 剩余字节数)
fn read_block_head(r: &mut BufReader<File>, payload_total: u64) -> io::Result<(u64, i16, u8, u64)> {
    let Some((track, tl, _)) = read_vint_size(r)? else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "块头不完整"));
    };
    let mut tc = [0u8; 2];
    r.read_exact(&mut tc)?;
    let rel = i16::from_be_bytes(tc);
    let mut f = [0u8; 1];
    r.read_exact(&mut f)?;
    let payload = payload_total.saturating_sub(tl as u64 + 3);
    Ok((track, rel, f[0], payload))
}

fn walk_block_group(
    r: &mut BufReader<File>,
    end: u64,
    cluster_tc: u64,
    target: u64,
    out: &mut WalkOut,
) -> Result<(), String> {
    let mut blk: Option<(i16, u8, Vec<u8>)> = None; // (相对时码, flags, payload)
    let mut duration: u64 = 0;
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_BLOCK {
            let (track, rel, flags, plen) = read_block_head(r, e.size).map_err(perr)?;
            if track == target && out.t_found {
                if plen > MAX_SUB_PAYLOAD {
                    return Err("字幕块尺寸异常（容器损坏？）".into());
                }
                blk = Some((rel, flags, read_bytes(r, plen).map_err(perr)?));
            } else {
                // 非目标轨（视频/音频块）：只消费了块头，整块 seek 跳过
                skip_to(r, child_end).map_err(perr)?;
            }
        } else if e.id == ID_BLOCK_DURATION {
            duration = read_uint(r, e.size).map_err(perr)?;
        } else {
            skip_to(r, child_end).map_err(perr)?;
        }
    }
    skip_to(r, end).map_err(perr)?;
    if let Some((rel, flags, payload)) = blk {
        if flags & 0x06 != 0 {
            // lacing 分帧的字幕块实践中不存在（ffmpeg/mkvmerge 均不产），
            // 交还前端 JS 兜底路径（matroska-subtitles 的 EBML 库可解）
            return Err("字幕块使用了 lacing 分帧，原生引擎跳过".into());
        }
        let units = cluster_tc as i128 + rel as i128;
        let scale = out.scale as i128;
        let time_ms = (units.max(0) * scale / 1_000_000) as u64;
        let dur_ms = (duration as i128 * scale / 1_000_000) as u64;
        out.blocks.push(build_sub_block(payload, time_ms, dur_ms, &out.t_kind, out.t_ssa));
    }
    Ok(())
}

/// 块 payload → SubBlock（ASS/SSA 字段拆分与 matroska-subtitles 对齐）
fn build_sub_block(payload: Vec<u8>, time: u64, duration: u64, kind: &str, ssa: bool) -> SubBlock {
    let raw = String::from_utf8_lossy(&payload).to_string();
    if kind == "ass" {
        let f: Vec<&str> = raw.split(',').collect();
        if f.len() >= 9 {
            // [ReadOrder,] Layer, Style, Name, MarginL, MarginR, MarginV, Effect, Text…
            // SSA 比 ASS 多一个前导字段（Marked），Layer 无值 → 0
            let (layer, style, name, ml, mr, mv, eff) = if ssa {
                ("0", f[2], f[3], f[4], f[5], f[6], f[7])
            } else {
                (f[1], f[2], f[3], f[4], f[5], f[6], f[7])
            };
            return SubBlock {
                time,
                duration,
                text: f[8..].join(","),
                layer: layer.to_string(),
                style: style.to_string(),
                name: name.to_string(),
                margin_l: ml.to_string(),
                margin_r: mr.to_string(),
                margin_v: mv.to_string(),
                effect: eff.to_string(),
            };
        }
        // 字段不足（非标准封装）：整段当文本，保底不丢内容
        return SubBlock { time, duration, text: raw, ..Default::default() };
    }
    SubBlock { time, duration, text: raw, ..Default::default() }
}

fn perr(e: io::Error) -> String {
    format!("读取 MKV 失败：{e}")
}

/* ================= 引擎 1：ffmpeg ================= */

/// Windows 下抑制子进程控制台窗口（Tauri GUI 应用 spawn 控制台程序会闪黑窗）
#[cfg(windows)]
fn no_console_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}
#[cfg(not(windows))]
fn no_console_window(_cmd: &mut Command) {}

/* ---------- ffmpeg 解析（跨平台：Windows 显式 ffmpeg.exe） ---------- */

/// 解析 ffmpeg 可执行文件路径（结果缓存，进程生命周期内只探测一次）。
///
/// 搜索序（`ffmpeg_search_dirs` × `ffmpeg_candidate_names`）：
/// 1. Windows 先查「应用 exe 同目录」——便携部署（把 ffmpeg.exe 放在
///    播放器旁边）无需配置 PATH 即可被找到；
/// 2. 再查 PATH 各目录：Windows 找 `ffmpeg.exe`（NTFS 大小写不敏感，
///    `FFMPEG.EXE` 等变体天然命中；无扩展名的 `ffmpeg` 形态兜底），
///    Unix 找 `ffmpeg` 且要求可执行位；
/// 3. 每个命中的候选先跑 `-version` 验证可运行性——Windows 应用执行
///    别名（WindowsApps 下的零字节残影 stub）文件存在却无法启动，
///    逐候选验证可自动跳过坏别名，不挡 PATH 里靠后的真 ffmpeg。
///
/// 空的 PATH 项（shell 语义 = 当前目录）跳过，不搜 CWD。
/// 解析结果缓存后在 extract_ffmpeg 里直接作为程序路径调用，
/// 不再依赖 CreateProcess / posix_spawn 的按名搜索语义。
fn ffmpeg_exe() -> Option<&'static Path> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE.get_or_init(find_ffmpeg).as_deref()
}

/// 机器上有可用的 ffmpeg 吗（探测结果缓存）
fn ffmpeg_available() -> bool {
    ffmpeg_exe().is_some()
}

/// 候选文件名：Windows 显式带 `.exe` 扩展名（winget / scoop / choco /
/// 官方构建在 Windows 上的分发形态一律是 ffmpeg.exe）
fn ffmpeg_candidate_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg"]
    }
}

/// PATH 值 → 搜索目录序列（跳过空项；不读 env 的纯函数，便于单测）
fn dirs_from_path(path_var: &OsStr) -> Vec<PathBuf> {
    std::env::split_paths(path_var)
        .filter(|d| !d.as_os_str().is_empty())
        .collect()
}

/// 目录搜索序：Windows 应用目录优先（便携部署），随后 PATH 各项
fn ffmpeg_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(windows) {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                dirs.push(dir.to_path_buf());
            }
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(dirs_from_path(&path));
    }
    dirs
}

/// 候选文件是否具备启动条件（存在、是普通文件；Unix 还要求可执行位）
fn is_runnable_candidate(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    if !md.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // owner / group / other 任一可执行位即可
        if md.permissions().mode() & 0o111 == 0 {
            return false;
        }
    }
    true
}

/// 试跑 `-version`：坏别名（WindowsApps 残影 stub）、架构不符的
/// 二进制在此过滤（能启动但退出非 0 同样不算可用）
fn probe_ffmpeg(exe: &Path) -> bool {
    let mut cmd = Command::new(exe);
    cmd.arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_console_window(&mut cmd);
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

fn find_ffmpeg() -> Option<PathBuf> {
    for dir in ffmpeg_search_dirs() {
        for name in ffmpeg_candidate_names() {
            let cand = dir.join(name);
            if is_runnable_candidate(&cand) && probe_ffmpeg(&cand) {
                return Some(cand);
            }
        }
    }
    None
}

/// 用 ffmpeg 提取第 sub_index 条字幕轨（0 起，按容器轨道顺序），
/// `-c:s copy` 纯封装拷贝直写 stdout（无临时文件、无转码）。
fn extract_ffmpeg(path: &str, sub_index: usize, kind: &str, file_len: u64) -> Result<ExtractResult, String> {
    // 直接用解析缓存里的绝对路径：Windows 上不依赖 CreateProcess
    // 的按名搜索（其搜索序含 CWD，且对 .exe / .bat 的解析不可控）
    let exe = ffmpeg_exe().ok_or("未找到可用的 ffmpeg")?;
    set_method(M_FFMPEG);
    set_total(file_len);
    set_bytes(0);
    let fmt = match kind {
        "ass" => "ass",
        "webvtt" => "webvtt",
        _ => "srt",
    };
    let mut cmd = Command::new(exe);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i", path])
        .arg("-map")
        .arg(format!("0:s:{sub_index}"))
        .args(["-c:s", "copy", "-f", fmt, "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_console_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg 启动失败：{e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    // stdout / stderr 双线程收流（防管道写满死锁）+ 看门狗超时击杀
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let th_out = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        let _ = tx.send(v);
    });
    let th_err = thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stderr.read_to_end(&mut v);
        v
    });
    let out = match rx.recv_timeout(FFMPEG_TIMEOUT) {
        Ok(v) => v,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("ffmpeg 提取超时".into());
        }
    };
    let err = th_err.join().unwrap_or_default();
    let _ = th_out.join();
    let status = child.wait().map_err(|e| format!("ffmpeg 等待失败：{e}"))?;
    if !status.success() {
        let msg = String::from_utf8_lossy(&err);
        let msg = msg.trim();
        return Err(if msg.is_empty() {
            format!("ffmpeg 退出码 {status}")
        } else {
            format!("ffmpeg：{msg}")
        });
    }
    let text = String::from_utf8_lossy(&out).trim().to_string();
    if text.is_empty() {
        return Err("ffmpeg 未输出字幕内容".into());
    }
    Ok(ExtractResult {
        method: "ffmpeg",
        kind: kind.to_string(),
        text: Some(text),
        header: None,
        blocks: None,
    })
}

/* ================= 引擎 2：原生 EBML ================= */

/// 原生流式提取目标字幕轨（full 走读：内存只驻留字幕块，与文件大小无关）
fn extract_native(path: &str, track_number: u64, file_len: u64) -> Result<ExtractResult, String> {
    set_method(M_NATIVE);
    set_total(file_len);
    set_bytes(0);
    let file = File::open(path).map_err(|e| format!("打开文件失败：{e}"))?;
    let mut r = BufReader::with_capacity(BUF, file);
    let out = walk_mkv(&mut r, file_len, Some(track_number))?;
    if !out.t_found {
        return Err("找不到指定的字幕轨".into());
    }
    if out.blocks.is_empty() {
        return Err("该字幕轨没有可显示的内容".into());
    }
    let is_ass = out.t_kind == "ass";
    Ok(ExtractResult {
        method: "native",
        kind: out.t_kind.clone(),
        text: None,
        header: if is_ass {
            Some(String::from_utf8_lossy(&out.t_private).to_string())
        } else {
            None
        },
        blocks: Some(out.blocks),
    })
}

/* ================= 入口：三级引擎调度（命令层调用） ================= */

/// 提取一条 MKV 内嵌字幕轨：
/// ① 解析到 ffmpeg（PATH / Windows 应用目录，见 ffmpeg_exe）→ ffmpeg 提取（最快）；
/// ② 失败 / 无 ffmpeg → 原生 EBML 流式解析；
/// 两者都失败 → Err（前端收到后回退 JS 流式兜底）。
pub fn extract_auto(path: &str, track_number: u64) -> Result<ExtractResult, String> {
    let file_len = std::fs::metadata(path)
        .map_err(|e| format!("读取文件信息失败：{e}"))?
        .len();
    if file_len == 0 {
        return Err("文件为空".into());
    }
    // 头模式走读：只到 Tracks（选轨 / 判型 / ffmpeg 序号），不扫 Cluster
    let file = File::open(path).map_err(|e| format!("打开文件失败：{e}"))?;
    let mut r = BufReader::with_capacity(BUF, file);
    let head = walk_mkv(&mut r, file_len, None)?;
    let target = head
        .metas
        .iter()
        .find(|m| m.number == track_number)
        .ok_or("找不到指定的字幕轨")?;
    if target.kind != TRACK_SUBTITLE {
        return Err("指定的轨道不是字幕轨".into());
    }
    let kind = kind_of(&target.codec_id)
        .ok_or("未适配的字幕格式（PGS / VOBSub 等图形字幕暂不支持）")?;
    // ffmpeg 的流序号 = 字幕轨在容器轨道顺序中的位置（0 起）
    let sub_index = head
        .metas
        .iter()
        .filter(|m| m.kind == TRACK_SUBTITLE)
        .position(|m| m.number == track_number)
        .unwrap_or(0);
    if ffmpeg_available() {
        if let Ok(res) = extract_ffmpeg(path, sub_index, kind, file_len) {
            return Ok(res);
        }
        // ffmpeg 失败（个别封装 / 映射不兼容）→ 原生引擎接手
    }
    extract_native(path, track_number, file_len)
}

/* ================= 单元测试（独立 crate 亦可跑） ================= */
// 显式 #[path]：默认约定（mkvsub/tests.rs）在「独立验证 crate 以
// #[path] 引入本文件」时不可达（子模块解析基址不同），显式相对
// 路径在两种挂载方式下都指向同一文件。
#[cfg(test)]
#[path = "mkvsub/tests.rs"]
mod tests;
