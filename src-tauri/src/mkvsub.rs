//! 极速看片 —— MKV 内嵌字幕提取（索引跳跃会话 + 全量走读双引擎）
//!
//! v1.0.7 架构（删除 ffmpeg 引擎：实测被索引跳跃全面超越——9.9GB
//! 蓝光重封装 MKV 在 5400rpm HDD 上 ffmpeg 全量 demux ~183s，索引
//! 跳跃冷 ~44s / 热 ~0.5s；SSD 上 33s vs ~4s；且免去外部依赖与
//! 跨平台探测的全部复杂度）：
//!
//! 引擎 ①（索引会话）：Matroska 的 Cues 索引记录每条轨道每个块的
//!   (Cluster 位置, 块内偏移)。subtitle_session_open 解析 SeekHead +
//!   Cues（几百 KB、亚秒级），目标轨的位置表常驻内存（几十 KB）；
//!   之后 subtitle_window(from, to) 按播放进度窗口化直跳取块——
//!   只碰窗口内字幕块的几十 KB 字节，不顺序扫全文件。播放中每
//!   ~90s 补一窗，seek 直接查新位置窗口，首屏字幕亚秒级可用。
//! 引擎 ②（原生全量走读）：无 Cues / 索引缺 rel 定位（罕见封装，
//!   如 mkvmerge --no-cues）时的兜底——手写流式 EBML 游走顺序扫
//!   Segment 的 Cluster/BlockGroup，只把目标轨字幕块读进内存
//!   （通常几 KB~几 MB），视频/音频块用 seek 直接跳过，一次性
//!   返回全部块（进度按文件偏移报百分比）。
//! 引擎 ③（前端 JS 流式）：② 也失败（罕见 lacing 分帧 / 容器损伤）
//!   时由前端 subtitles.js 回退 v1.0.4 的 matroska-subtitles 兼容路径。
//!
//! 语义对齐（各引擎完全一致，保证任何路径产出相同）：
//! - 字幕事件只来自 BlockGroup(0xA0)→Block(0xA1)（带 BlockDuration
//!   0x9B），SimpleBlock(0xA3) 不产字幕（ffmpeg / mkvmerge 均按此封装）；
//! - time = (Cluster.Timestamp + Block 相对时码) × TimecodeScale / 1e6（ms）；
//! - duration = BlockDuration × TimecodeScale / 1e6（ms）；
//! - ASS/SSA 块 payload 是逗号分隔字段：
//!   [ReadOrder,]Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text
//!   （SSA 比 ASS 多跳过一个前导字段），Text 内的逗号重新拼回。
//! - 索引侧：CueTime 只用于窗口选择；块时间仍取「簇时码 + 块相对
//!   时码」的权威计算（每簇时码在会话内跨窗口缓存；实测 MakeMKV
//!   蓝光重封装两者 973/973 一致）。CueRelativePosition 的基准是
//!   Cluster 数据区起点（ID + 尺寸头之后），不是元素起点。
//!
//! 会话：位置表 + 簇时码缓存挂在进程内会话表（上限 8 个，超出逐出
//! 最旧），切轨 / 切文件由前端 subtitle_close 显式关闭。

use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/* ================= 进度（引擎 ② 全量走读，前端轮询） ================= */

static P_RUNNING: AtomicBool = AtomicBool::new(false);
static P_BYTES: AtomicU64 = AtomicU64::new(0);
static P_TOTAL: AtomicU64 = AtomicU64::new(0);

/// query_extract_progress 的返回（camelCase，前端直接消费）
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct ProgressSnapshot {
    pub running: bool,
    pub bytes: u64,
    pub total: u64,
}

/// 全量提取任务开始（extract_mkv_subtitles 命令入口调用）
pub fn progress_begin() {
    P_RUNNING.store(true, Ordering::SeqCst);
    P_BYTES.store(0, Ordering::SeqCst);
    P_TOTAL.store(0, Ordering::SeqCst);
}

/// 全量提取任务结束（含失败路径，命令出口调用）
pub fn progress_end() {
    P_RUNNING.store(false, Ordering::SeqCst);
}

/// 当前进度快照
pub fn progress_snapshot() -> ProgressSnapshot {
    ProgressSnapshot {
        running: P_RUNNING.load(Ordering::SeqCst),
        bytes: P_BYTES.load(Ordering::SeqCst),
        total: P_TOTAL.load(Ordering::SeqCst),
    }
}

fn set_bytes(b: u64) {
    P_BYTES.store(b, Ordering::SeqCst);
}

fn set_total(t: u64) {
    P_TOTAL.store(t, Ordering::SeqCst);
}

/* ================= 结果类型（serde → JSON） ================= */

/// 一条字幕块（引擎 ①② 产出；字段与 matroska-subtitles 对齐）
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

/// extract_mkv_subtitles 的返回（引擎 ② 全量走读）
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResult {
    /// native（引擎 ②；前端 ③ 兜底时为 js，由前端自行标记）
    pub method: &'static str,
    /// utf8 | webvtt | ass
    pub kind: String,
    /// native 引擎：ASS 头（CodecPrivate）
    pub header: Option<String>,
    /// native 引擎：字幕块列表
    pub blocks: Option<Vec<SubBlock>>,
}

/* ================= Matroska 元素 ID（本模块用到的子集） ================= */
// （跳过而不解析的元素不占常量位：EBML 头 0x1A45DFA3 等在走读的
//  else/skip 分支按「未识别即跳过」处理，见注释）
const ID_SEGMENT: u64 = 0x18538067;
const ID_SEEKHEAD: u64 = 0x114d9b74;
const ID_SEEK: u64 = 0x4dbb;
const ID_SEEK_ID: u64 = 0x53ab;
const ID_SEEK_POS: u64 = 0x53ac;
const ID_CUES: u64 = 0x1c53bb6b;
const ID_CUE_POINT: u64 = 0xbb;
const ID_CUE_TIME: u64 = 0xb3;
const ID_CUE_TRACKPOS: u64 = 0xb7;
const ID_CUE_TRACK: u64 = 0xf7;
const ID_CUE_CLUSTER_POS: u64 = 0xf1;
const ID_CUE_REL_POS: u64 = 0xf0;
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
const ID_SIMPLE_BLOCK: u64 = 0xa3;
const ID_BLOCK_DURATION: u64 = 0x9b;

/// TrackType：0x11 = 字幕
const TRACK_SUBTITLE: u64 = 0x11;

/// 顺序读缓冲（1MB：块头解析的 syscall 密度与顺序读吞吐的折中）
const BUF: usize = 1 << 20;
/// 目标块 payload 上限（正常字幕块仅几 KB；防御损坏文件的天量分配）
const MAX_SUB_PAYLOAD: u64 = 64 << 20;
/// 无可用索引（引擎 ① 不可用，前端据此降级引擎 ② 全量走读）
const ERR_NO_INDEX: &str = "该文件没有可用的 Cues 索引";
/// 会话表上限：超出逐出最旧（前端异常路径不关会话也不至于泄漏）
const SESSION_CAP: usize = 8;

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

/* ================= 走读（引擎 ② 全量 + ① 的头部/轨道解析） ================= */

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
    /// Tracks.TrackEntry 列表（按容器顺序）
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
/// `target = None`：头模式——解析到 Tracks 完成即止（选轨 / 判型用）；
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

/* ================= 引擎 ②：原生全量走读 ================= */

/// 全量流式提取目标字幕轨（full 走读：内存只驻留字幕块，与文件大小无关）
fn extract_native(path: &str, track_number: u64, file_len: u64) -> Result<ExtractResult, String> {
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
        header: if is_ass {
            Some(String::from_utf8_lossy(&out.t_private).to_string())
        } else {
            None
        },
        blocks: Some(out.blocks),
    })
}

/// 提取一条 MKV 内嵌字幕轨（引擎 ②：一次性全量走读）。
/// 无索引文件（引擎 ① 不可用）时的兜底路径；两者都失败 → Err
/// （前端收到后回退 JS 流式兜底）。
pub fn extract_auto(path: &str, track_number: u64) -> Result<ExtractResult, String> {
    let file_len = std::fs::metadata(path)
        .map_err(|e| format!("读取文件信息失败：{e}"))?
        .len();
    if file_len == 0 {
        return Err("文件为空".into());
    }
    // 头模式走读：只到 Tracks（选轨 / 判型），不扫 Cluster
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
    kind_of(&target.codec_id)
        .ok_or("未适配的字幕格式（PGS / VOBSub 等图形字幕暂不支持）")?;
    extract_native(path, track_number, file_len)
}

/* ================= 引擎 ①：Cues 索引会话（窗口化跳跃提取） ================= */

/// 一条索引命中：目标轨某字幕块的定位
#[derive(Clone, Copy, Debug)]
struct CueHit {
    /// 窗口选择用时间（CueTime × scale / 1e6）
    cue_ms: u64,
    /// CueClusterPosition（相对 Segment 数据区）
    cluster: u64,
    /// CueRelativePosition（相对 Cluster 数据区）
    rel: u64,
}

/// 打开的字幕会话（位置表常驻内存，几十 KB 量级）
struct SubtitleSession {
    path: String,
    file_len: u64,
    /// Segment 数据区绝对起点（索引偏移的换算基准）
    seg_data: u64,
    scale: u64,
    track_number: u64,
    kind: &'static str,
    ssa: bool,
    /// ASS 头（CodecPrivate）
    header: Option<String>,
    /// 按 cue_ms 升序的命中表
    hits: Vec<CueHit>,
    /// 簇头缓存（cluster → (簇数据区绝对偏移, Timecode)），跨窗口复用
    cluster_cache: HashMap<u64, (u64, u64)>,
}

/// subtitle_session_open 的返回
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: u64,
    /// utf8 | webvtt | ass
    pub kind: String,
    /// ASS 头（CodecPrivate；非 ASS 轨为 None）
    pub header: Option<String>,
    /// 索引命中数（≈ 该轨字幕块总数）
    pub total_blocks: usize,
}

/// subtitle_window 的返回
#[derive(Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct WindowResult {
    pub blocks: Vec<SubBlock>,
    /// 定位失败被跳过的索引条数（>0 时前端应降级全量提取保证完整性）
    pub skipped: u32,
}

static SESSIONS: Mutex<BTreeMap<u64, SubtitleSession>> = Mutex::new(BTreeMap::new());
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Cues 解析的中间结构（全部轨道；命中表只留目标轨）
#[derive(Clone, Debug)]
struct CueEntry {
    time: u64,
    track: u64,
    cluster: u64,
    rel: Option<u64>,
}

fn parse_seekhead(r: &mut BufReader<File>, end: u64, found: &mut Vec<(u64, u64)>) -> Result<(), String> {
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_SEEK {
            let mut sid = 0u64;
            let mut spos = 0u64;
            while r.stream_position().map_err(perr)? < child_end {
                let Some(c) = read_elem(r).map_err(perr)? else { break };
                let ce = elem_end(&c, child_end);
                if c.id == ID_SEEK_ID {
                    sid = read_uint(r, c.size).map_err(perr)?;
                } else if c.id == ID_SEEK_POS {
                    spos = read_uint(r, c.size).map_err(perr)?;
                } else {
                    skip_to(r, ce).map_err(perr)?;
                }
            }
            found.push((sid, spos));
        }
        skip_to(r, child_end).map_err(perr)?;
    }
    Ok(())
}

fn parse_cues(r: &mut BufReader<File>, end: u64, out: &mut Vec<CueEntry>) -> Result<(), String> {
    while r.stream_position().map_err(perr)? < end {
        let Some(e) = read_elem(r).map_err(perr)? else { break };
        let child_end = elem_end(&e, end);
        if e.id == ID_CUE_POINT {
            let mut time = 0u64;
            while r.stream_position().map_err(perr)? < child_end {
                let Some(c) = read_elem(r).map_err(perr)? else { break };
                let ce = elem_end(&c, child_end);
                if c.id == ID_CUE_TIME {
                    time = read_uint(r, c.size).map_err(perr)?;
                } else if c.id == ID_CUE_TRACKPOS {
                    let mut track = 0u64;
                    let mut cluster = 0u64;
                    let mut rel = None;
                    while r.stream_position().map_err(perr)? < ce {
                        let Some(d) = read_elem(r).map_err(perr)? else { break };
                        let de = elem_end(&d, ce);
                        match d.id {
                            ID_CUE_TRACK => track = read_uint(r, d.size).map_err(perr)?,
                            ID_CUE_CLUSTER_POS => cluster = read_uint(r, d.size).map_err(perr)?,
                            ID_CUE_REL_POS => rel = Some(read_uint(r, d.size).map_err(perr)?),
                            _ => skip_to(r, de).map_err(perr)?,
                        }
                    }
                    out.push(CueEntry { time, track, cluster, rel });
                    skip_to(r, ce).map_err(perr)?;
                } else {
                    skip_to(r, ce).map_err(perr)?;
                }
            }
        }
        skip_to(r, child_end).map_err(perr)?;
    }
    Ok(())
}

/// 读某 Cluster 的元素头与 Timecode。返回 (簇数据区绝对偏移, Timecode)。
/// CueRelativePosition 的基准是簇数据区起点（ID + 尺寸头之后）。
fn cluster_head(r: &mut BufReader<File>, cluster_abs: u64, file_len: u64) -> Result<(u64, u64), String> {
    skip_to(r, cluster_abs).map_err(perr)?;
    let Some(e) = read_elem(r).map_err(perr)? else {
        return Err("索引指向的 Cluster 不存在".into());
    };
    let data_pos = if e.unknown { cluster_abs } else { e.data_pos };
    // Timecode 正常是簇的第一个子元素；限定在簇头附近找（防御病态封装）
    let guard = e.data_pos.saturating_add(e.size.min(65536)).min(file_len);
    while r.stream_position().map_err(perr)? < guard {
        let Some(c) = read_elem(r).map_err(perr)? else { break };
        let ce = elem_end(&c, guard);
        if c.id == ID_TIMESTAMP {
            return Ok((data_pos, read_uint(r, c.size).map_err(perr)?));
        }
        skip_to(r, ce).map_err(perr)?;
    }
    Ok((data_pos, 0))
}

/// 打开索引会话：解析 SeekHead + Cues，目标轨位置表常驻内存。
/// 定性错误（轨道不存在 / 非字幕轨 / 格式未适配 / 空文件）与
/// extract_auto 文案一致（前端按同套判据短路）；索引不可用报
/// ERR_NO_INDEX（前端据此降级引擎 ②）。
pub fn session_open(path: &str, track_number: u64) -> Result<SessionInfo, String> {
    let file_len = std::fs::metadata(path)
        .map_err(|e| format!("读取文件信息失败：{e}"))?
        .len();
    if file_len == 0 {
        return Err("文件为空".into());
    }
    let file = File::open(path).map_err(|e| format!("打开文件失败：{e}"))?;
    let mut r = BufReader::with_capacity(BUF, file);

    // 顶层：EBML 头 →（顶层 SeekHead，若有）→ Segment 头
    let mut seeks: Vec<(u64, u64)> = Vec::new(); // (元素ID, 相对 Segment 数据区)
    let mut seg_data = 0u64;
    let mut seg_end = file_len;
    loop {
        let Some(e) = read_elem(&mut r).map_err(perr)? else { break };
        let end = elem_end(&e, file_len);
        if e.id == ID_SEEKHEAD {
            parse_seekhead(&mut r, end, &mut seeks)?;
        } else if e.id == ID_SEGMENT {
            seg_data = e.data_pos;
            seg_end = end;
            break;
        } else {
            skip_to(&mut r, end).map_err(perr)?;
        }
    }

    // Segment 子元素：SeekHead（MakeMKV / ffmpeg 放段内，mkvmerge 放
    // 顶层——两种都收）/ Info / Tracks 正常解析，其余（Cluster 等）只读
    // 元素头即跳。拿到轨道表 + Cues 位置（SeekHead 指路或逐头扫到）即止
    let mut head = WalkOut::new();
    let mut cues_abs: Option<u64> = None;
    while r.stream_position().map_err(perr)? < seg_end {
        let Some(c) = read_elem(&mut r).map_err(perr)? else { break };
        let child_end = elem_end(&c, seg_end);
        match c.id {
            ID_SEEKHEAD => parse_seekhead(&mut r, child_end, &mut seeks)?,
            ID_INFO => walk_info(&mut r, child_end, &mut head)?,
            ID_TRACKS => walk_tracks(&mut r, child_end, &mut head)?,
            ID_CUES => {
                cues_abs = Some(c.data_pos);
                if head.metas.is_empty() {
                    skip_to(&mut r, child_end).map_err(perr)?; // 病态序：Cues 在 Tracks 前
                } else {
                    break;
                }
            }
            _ => skip_to(&mut r, child_end).map_err(perr)?, // Cluster 只跳元素头
        }
        if !head.metas.is_empty() && cues_abs.is_none() {
            if let Some((_, p)) = seeks.iter().find(|(id, _)| *id == ID_CUES) {
                cues_abs = Some(seg_data + p);
                break;
            }
        }
    }

    // 定性校验（文案与 extract_auto 一致）
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

    // 索引可用性：无 Cues / 指向异常 / 目标轨无条目 / 任一条目缺 rel
    // 定位 → 整体放弃（不部分跳跃，防止悄悄漏字幕）
    let Some(cues_at) = cues_abs else {
        return Err(ERR_NO_INDEX.into());
    };
    skip_to(&mut r, cues_at).map_err(perr)?;
    let Some(e) = read_elem(&mut r).map_err(perr)? else {
        return Err(ERR_NO_INDEX.into());
    };
    if e.id != ID_CUES {
        return Err(ERR_NO_INDEX.into());
    }
    let mut entries = Vec::new();
    parse_cues(&mut r, elem_end(&e, file_len), &mut entries)?;
    let mut hits: Vec<CueHit> = Vec::new();
    for c in entries.iter().filter(|c| c.track == track_number) {
        let Some(rel) = c.rel else {
            return Err(ERR_NO_INDEX.into());
        };
        hits.push(CueHit {
            cue_ms: (c.time as u128 * head.scale as u128 / 1_000_000) as u64,
            cluster: c.cluster,
            rel,
        });
    }
    if hits.is_empty() {
        return Err(ERR_NO_INDEX.into());
    }
    hits.sort_unstable_by(|a, b| (a.cue_ms, a.cluster, a.rel).cmp(&(b.cue_ms, b.cluster, b.rel)));
    hits.dedup_by(|a, b| a.cluster == b.cluster && a.rel == b.rel);

    let info = SessionInfo {
        session_id: NEXT_SESSION.fetch_add(1, Ordering::SeqCst),
        kind: kind.to_string(),
        header: if kind == "ass" {
            Some(String::from_utf8_lossy(&target.codec_private).to_string())
        } else {
            None
        },
        total_blocks: hits.len(),
    };
    let session = SubtitleSession {
        path: path.to_string(),
        file_len,
        seg_data,
        scale: head.scale,
        track_number,
        kind,
        ssa: is_ssa(&target.codec_id),
        header: info.header.clone(),
        hits,
        cluster_cache: HashMap::new(),
    };
    let mut map = SESSIONS.lock().map_err(|_| "字幕会话表锁定失败")?;
    while map.len() >= SESSION_CAP {
        map.pop_first(); // 逐出最旧（id 最小）
    }
    map.insert(info.session_id, session);
    Ok(info)
}

/// 取一个播放窗口的字幕块：命中表按 cue_ms 二分出 [from_ms, to_ms)
/// 的条目，按文件偏移排序后逐条直跳（只碰窗口内块的几十 KB 字节）。
/// 返回 skipped > 0 表示有索引条目定位失败，前端应降级全量提取。
pub fn session_window(session_id: u64, from_ms: u64, to_ms: u64) -> Result<WindowResult, String> {
    let mut map = SESSIONS.lock().map_err(|_| "字幕会话表锁定失败")?;
    let Some(s) = map.get_mut(&session_id) else {
        return Err("字幕会话已失效".into());
    };
    let lo = s.hits.partition_point(|h| h.cue_ms < from_ms);
    let hi = s.hits.partition_point(|h| h.cue_ms < to_ms);
    if lo >= hi {
        return Ok(WindowResult::default()); // 空窗（无对白的时段）
    }
    // 拷出命中后按 (cluster, rel) 排序：文件偏移升序 ≈ 磁头顺扫
    let mut sel: Vec<CueHit> = s.hits[lo..hi].to_vec();
    sel.sort_unstable_by_key(|h| (h.cluster, h.rel));

    let file = File::open(&s.path).map_err(|e| format!("打开文件失败：{e}"))?;
    let mut r = BufReader::with_capacity(BUF, file);
    let mut walk = WalkOut {
        scale: s.scale,
        t_found: true,
        t_kind: s.kind.to_string(),
        t_ssa: s.ssa,
        ..Default::default()
    };
    let mut skipped: u32 = 0;
    for h in &sel {
        let (cdata, tc) = match s.cluster_cache.get(&h.cluster) {
            Some(v) => *v,
            None => {
                let v = cluster_head(&mut r, s.seg_data + h.cluster, s.file_len)?;
                s.cluster_cache.insert(h.cluster, v);
                v
            }
        };
        skip_to(&mut r, cdata + h.rel).map_err(perr)?;
        let Some(b) = read_elem(&mut r).map_err(perr)? else {
            skipped += 1;
            continue;
        };
        let bend = if b.unknown {
            b.data_pos + (1 << 20)
        } else {
            b.data_pos + b.size
        };
        match b.id {
            ID_BLOCK_GROUP => {
                match walk_block_group(&mut r, bend.min(s.file_len), tc, s.track_number, &mut walk) {
                    Ok(()) => {}
                    Err(_) => skipped += 1, // lacing / 尺寸异常等：计入跳过，前端降级
                }
            }
            // 个别封装的索引直指裸 Block / SimpleBlock（无 BlockDuration）
            ID_BLOCK | ID_SIMPLE_BLOCK => {
                match read_block_head(&mut r, b.size) {
                    Ok((track, rel, flags, plen)) if track == s.track_number && flags & 0x06 == 0 => {
                        match read_bytes(&mut r, plen) {
                            Ok(payload) => {
                                let units = tc as i128 + rel as i128;
                                let time = (units.max(0) * s.scale as i128 / 1_000_000) as u64;
                                walk.blocks.push(build_sub_block(payload, time, 0, s.kind, s.ssa));
                            }
                            Err(_) => skipped += 1,
                        }
                    }
                    Ok(_) => {} // 非目标轨 / lacing：索引本应指向目标轨，略过
                    Err(_) => skipped += 1,
                }
            }
            _ => skipped += 1, // 定位点不是块元素（索引损坏 / 基准不符）
        }
    }
    if skipped > 0 && walk.blocks.is_empty() {
        return Err(format!("索引跳跃失败（{skipped} 条未命中）"));
    }
    Ok(WindowResult { blocks: walk.blocks, skipped })
}

/// 关闭会话（切轨 / 切文件时前端显式调用；幂等）
pub fn session_close(session_id: u64) {
    if let Ok(mut map) = SESSIONS.lock() {
        map.remove(&session_id);
    }
}

/* ================= 单元测试（独立 crate 亦可跑） ================= */
// 显式 #[path]：默认约定（mkvsub/tests.rs）在「独立验证 crate 以
// #[path] 引入本文件」时不可达（子模块解析基址不同），显式相对
// 路径在两种挂载方式下都指向同一文件。
#[cfg(test)]
#[path = "mkvsub/tests.rs"]
mod tests;
