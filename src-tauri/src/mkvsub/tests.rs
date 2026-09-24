//! mkvsub.rs 单元测试 —— 合成 MKV 字节流驱动，不依赖外部工具与真实媒体。
//!
//! 覆盖面：
//! - 引擎 ②（全量走读）：SRT 块时间/时长、ASS/SSA 字段拆分、双字节
//!   TrackNumber、未知尺寸 Segment（ffmpeg 01+00…00 形态）、视频块
//!   seek 跳过、稀疏 4GB 大文件；
//! - 引擎 ①（索引会话）：带 SeekHead + Cues 的合成 MKV 上——会话
//!   打开、窗口与全量产出逐块一致、窗口边界互斥、ASS 字段经窗口
//!   不丢、无索引降级标记、定性错误文案对齐、会话失效与逐出、
//!   seek 空洞补窗；
//! - 真实媒体对照：环境变量 KP_TEST_MKV 指向 multi.mkv 时做已知内容
//!   断言（有索引时加验「全片窗口扫 == 全量」）。

use super::*;
use std::io::Write as _;

/* ---------------- EBML 写入辅助（测试专用） ---------------- */

/// vint 尺寸编码（最短宽度，含标记位）
fn vs(v: u64) -> Vec<u8> {
    for w in 1..=8usize {
        if w == 8 || v < (1u64 << (7 * w)) {
            let mut out = Vec::with_capacity(w);
            out.push(((1u64 << (8 - w)) | (v >> (8 * (w - 1)))) as u8);
            for i in (0..w - 1).rev() {
                out.push(((v >> (8 * i)) & 0xff) as u8);
            }
            return out;
        }
    }
    unreachable!()
}

/// 定宽 4 字节原始大端 uint（值 < 2^32）。SeekPosition 的 payload 按
/// 规范是普通 uint（非 vint）；SeekHead 在段首占位，其长度决定后续
/// 全部偏移，必须先验定长。
fn u32be(v: u64) -> Vec<u8> {
    assert!(v < (1 << 32), "u32be 只编码 32 位以内值");
    (v as u32).to_be_bytes().to_vec()
}

/// 元素 = ID 字节 + 尺寸 vint + 数据
fn el(id: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(vs(payload.len() as u64));
    v.extend(payload);
    v
}

/// 无符号整数字段（最短 BE 字节；0 → 空数据）
fn el_u(id: &[u8], val: u64) -> Vec<u8> {
    if val == 0 {
        return el(id, &[]);
    }
    let b = val.to_be_bytes();
    let skip = (val.leading_zeros() / 8) as usize;
    el(id, &b[skip..])
}

fn el_s(id: &[u8], s: &str) -> Vec<u8> {
    el(id, s.as_bytes())
}

/// BlockGroup{Block(track,rel,payload), BlockDuration(dur)}（flags=0 无 lacing）
fn block_group(track: u64, rel: i16, payload: &[u8], dur: u64) -> Vec<u8> {
    let mut blk = vs(track);
    blk.extend(&rel.to_be_bytes());
    blk.push(0x00);
    blk.extend(payload);
    let mut g = el(&[0xa1], &blk);
    g.extend(el_u(&[0x9b], dur));
    el(&[0xa0], &g)
}

/// 一个 Cluster 的规格：时间码 + 字幕块列表 [(相对时码, BlockDuration, 文本)]
struct ClusterSpec {
    tc: u64,
    subs: Vec<(i16, u64, String)>,
}

/// Info + Tracks（视频轨 1 + 目标字幕轨 sub_number）
fn head_elements(sub_number: u64, codec: &str, private: &[u8]) -> Vec<u8> {
    let info = el_u(&[0x2a, 0xd7, 0xb1], 1_000_000);
    let mut tracks = Vec::new();
    let mut video = Vec::new();
    video.extend(el_u(&[0xd7], 1));
    video.extend(el_u(&[0x83], 1));
    video.extend(el_s(&[0x86], "V_VP9"));
    let mut ve = el_u(&[0xb0], 64);
    ve.extend(el_u(&[0xba], 64));
    video.extend(el(&[0xe0], &ve));
    tracks.extend(el(&[0xae], &video));
    let mut sub = Vec::new();
    sub.extend(el_u(&[0xd7], sub_number));
    sub.extend(el_u(&[0x83], TRACK_SUBTITLE));
    sub.extend(el_s(&[0x86], codec));
    if !private.is_empty() {
        sub.extend(el(&[0x63, 0xa2], private));
    }
    tracks.extend(el(&[0xae], &sub));
    let mut body = el(&[0x15, 0x49, 0xa9, 0x66], &info);
    body.extend(el(&[0x16, 0x54, 0xae, 0x6b], &tracks));
    body
}

/// EBML 头 + Segment(body) 外壳
fn shell(body: &[u8]) -> Vec<u8> {
    let mut eh = Vec::new();
    eh.extend(el_s(&[0x42, 0x82], "matroska"));
    eh.extend(el_u(&[0x42, 0xf2], 4));
    eh.extend(el_u(&[0x42, 0xf3], 8));
    let mut out = el(&[0x1a, 0x45, 0xdf, 0xa3], &eh);
    out.extend(el(&[0x18, 0x53, 0x80, 0x67], body));
    out
}

/// 组装一颗合成 MKV（无索引：引擎 ② 专用）
fn build_mkv(sub_number: u64, codec: &str, private: &[u8], clusters: &[ClusterSpec]) -> Vec<u8> {
    let junk = vec![0xabu8; 128 * 1024];
    let mut body = head_elements(sub_number, codec, private);
    for c in clusters {
        let mut cl = Vec::new();
        cl.extend(el_u(&[0xe7], c.tc));
        let mut sb = vs(1);
        sb.extend(&0i16.to_be_bytes());
        sb.push(0x80); // keyframe，无 lacing
        sb.extend(&junk);
        cl.extend(el(&[0xa3], &sb)); // 视频块：原生引擎应 seek 跳过
        for (rel, dur, text) in &c.subs {
            cl.extend(block_group(sub_number, *rel, text.as_bytes(), *dur));
        }
        cl.extend(el(&[0xec], &[0u8; 16])); // Void
        body.extend(el(&[0x1f, 0x43, 0xb6, 0x75], &cl));
    }
    shell(&body)
}

/// 组装「段内 SeekHead + 尾部 Cues」的合成 MKV（引擎 ① 会话路径用）。
/// 每个字幕块一条 CuePoint（CueTime = 簇时码+块相对时码，含
/// CueRelativePosition），与 MakeMKV / mkvmerge 的真实封装形态一致。
fn build_mkv_indexed(sub_number: u64, codec: &str, private: &[u8], clusters: &[ClusterSpec]) -> Vec<u8> {
    // 段内布局：[SeekHead][Info/Tracks][Clusters…][Cues]。
    // SeekHead 指向 Cues，而 Cues 位置依赖 SeekHead 自身长度——
    // SeekPosition 用定宽 4 字节 vint，长度与取值无关，可两步定长
    let seekhead_probe = {
        let mut sk = el(&[0x53, 0xab], &[0x1c, 0x53, 0xbb, 0x6b]); // SeekID = Cues
        sk.extend(el(&[0x53, 0xac], &u32be(0)));
        el(&[0x11, 0x4d, 0x9b, 0x74], &el(&[0x4d, 0xbb], &sk))
    };
    let base = seekhead_probe.len() as u64; // 后续元素在段数据区的基础偏移

    let mut body = head_elements(sub_number, codec, private);
    struct SubPos {
        cue: u64,    // CueTime（TimecodeScale 单位）
        cluster: u64, // Cluster 元素头相对段数据区
        rel: u64,    // 块元素相对簇数据区
    }
    let mut positions: Vec<SubPos> = Vec::new();
    let junk = vec![0xabu8; 64 * 1024];
    for c in clusters {
        let cluster_seg_off = base + body.len() as u64;
        let mut cl = Vec::new();
        cl.extend(el_u(&[0xe7], c.tc));
        let mut sb = vs(1);
        sb.extend(&0i16.to_be_bytes());
        sb.push(0x80);
        sb.extend(&junk);
        cl.extend(el(&[0xa3], &sb));
        for (rel, dur, text) in &c.subs {
            positions.push(SubPos { cue: c.tc + *rel as u64, cluster: cluster_seg_off, rel: cl.len() as u64 });
            cl.extend(block_group(sub_number, *rel, text.as_bytes(), *dur));
        }
        cl.extend(el(&[0xec], &[0u8; 16]));
        body.extend(el(&[0x1f, 0x43, 0xb6, 0x75], &cl));
    }
    let cues_seg_off = base + body.len() as u64;
    let mut cues = Vec::new();
    for p in &positions {
        let mut tp = el_u(&[0xf7], sub_number); // CueTrack
        tp.extend(el_u(&[0xf1], p.cluster)); // CueClusterPosition
        tp.extend(el_u(&[0xf0], p.rel)); // CueRelativePosition
        let mut point = el_u(&[0xb3], p.cue); // CueTime
        point.extend(el(&[0xb7], &tp)); // CueTrackPositions
        cues.extend(el(&[0xbb], &point)); // CuePoint
    }
    body.extend(el(&[0x1c, 0x53, 0xbb, 0x6b], &cues));

    let mut sk = el(&[0x53, 0xab], &[0x1c, 0x53, 0xbb, 0x6b]);
    sk.extend(el(&[0x53, 0xac], &u32be(cues_seg_off)));
    let seekhead = el(&[0x11, 0x4d, 0x9b, 0x74], &el(&[0x4d, 0xbb], &sk));
    assert_eq!(seekhead.len(), seekhead_probe.len(), "定宽编码下 SeekHead 长度必须稳定");

    let mut all = seekhead;
    all.extend(body);
    shell(&all)
}

/// 组装「未知尺寸 Segment」前缀（ffmpeg 8 字节 01+00…00 形态）：
/// 返回 EBML 头 + Segment头(未知尺寸) + Info + Tracks，正文（Clusters）由调用方追加
fn build_head_unknown_segment(sub_number: u64, codec: &str, private: &[u8]) -> Vec<u8> {
    let full = build_mkv(sub_number, codec, private, &[]);
    // 定位 Segment 头与 Info（合成树里 ID 唯一，窗口查找即可）
    let seg_at = full
        .windows(4)
        .position(|w| w == [0x18, 0x53, 0x80, 0x67])
        .expect("合成数据里必有 Segment 头");
    let info_at = full
        .windows(4)
        .position(|w| w == [0x15, 0x49, 0xa9, 0x66])
        .expect("合成数据里必有 Info");
    let mut out = full[..seg_at].to_vec(); // EBML 头
    out.extend([0x18, 0x53, 0x80, 0x67]);
    out.extend([0x01, 0, 0, 0, 0, 0, 0, 0]); // 未知尺寸（ffmpeg 形态）
    out.extend_from_slice(&full[info_at..]); // Info + Tracks
    out
}

fn tmp_path(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join("jisu-kanpian-mkvsub-tests");
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{name}.mkv"))
}

fn write_tmp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = tmp_path(name);
    std::fs::write(&p, bytes).unwrap();
    p
}

/* ---------------- 引擎 ②：全量走读 ---------------- */

#[test]
fn native_srt_blocks() {
    let mkv = build_mkv(
        3,
        "S_TEXT/UTF8",
        &[],
        &[
            ClusterSpec {
                tc: 0,
                subs: vec![(0, 2500, "第一句".into()), (3000, 2000, "第二句".into())],
            },
            ClusterSpec {
                tc: 60_000,
                subs: vec![(500, 4000, "一小时处的字幕".into())],
            },
        ],
    );
    let p = write_tmp("native-srt", &mkv);
    let res = extract_native(&p.to_string_lossy(), 3, mkv.len() as u64).unwrap();
    assert_eq!(res.method, "native");
    assert_eq!(res.kind, "utf8");
    assert!(res.header.is_none());
    let blocks = res.blocks.unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!((blocks[0].time, blocks[0].duration), (0, 2500));
    assert_eq!((blocks[1].time, blocks[1].duration), (3000, 2000));
    // cluster 时码 60000 + 相对 500 = 60500 单位 × 1e6ns = 60500ms
    assert_eq!((blocks[2].time, blocks[2].duration), (60_500, 4000));
    assert_eq!(blocks[2].text, "一小时处的字幕");
}

#[test]
fn native_ass_field_split() {
    let mkv = build_mkv(
        4,
        "S_TEXT/ASS",
        b"[Script Info]\nHDR-XYZ",
        &[ClusterSpec {
            tc: 1000,
            subs: vec![(
                20,
                3000,
                "7,0,StyleQ,N,1,2,3,FX,你好, 带逗号的文本".into(),
            )],
        }],
    );
    let p = write_tmp("native-ass", &mkv);
    let res = extract_native(&p.to_string_lossy(), 4, mkv.len() as u64).unwrap();
    assert_eq!(res.kind, "ass");
    assert_eq!(res.header.as_deref(), Some("[Script Info]\nHDR-XYZ"));
    let b = &res.blocks.unwrap()[0];
    // 时间：cluster 1000 + rel 20 = 1020ms
    assert_eq!((b.time, b.duration), (1020, 3000));
    assert_eq!(b.text, "你好, 带逗号的文本");
    assert_eq!((b.layer.as_str(), b.style.as_str(), b.name.as_str()), ("0", "StyleQ", "N"));
    assert_eq!(
        (b.margin_l.as_str(), b.margin_r.as_str(), b.margin_v.as_str(), b.effect.as_str()),
        ("1", "2", "3", "FX")
    );
}

#[test]
fn native_ssa_leading_field() {
    let mkv = build_mkv(
        4,
        "S_TEXT/SSA",
        b"[Script Info]\nSSA-HDR",
        &[ClusterSpec {
            tc: 0,
            subs: vec![(0, 1000, "9,1,StyleS,N,4,5,6,E2,SSA 文本".into())],
        }],
    );
    let p = write_tmp("native-ssa", &mkv);
    let res = extract_native(&p.to_string_lossy(), 4, mkv.len() as u64).unwrap();
    assert_eq!(res.kind, "ass");
    let b = &res.blocks.unwrap()[0];
    // SSA 比 ASS 多跳过一个前导字段：Style 取 f[2]（"StyleS"），Layer 无值 → 0
    assert_eq!(b.style, "StyleS");
    assert_eq!(b.layer, "0");
    assert_eq!(b.text, "SSA 文本");
    assert_eq!(res.header.as_deref(), Some("[Script Info]\nSSA-HDR"));
}

#[test]
fn native_two_byte_track_number() {
    // TrackNumber=200 → 块头的 track vint 为 2 字节（0x40 0xC8）
    let mkv = build_mkv(
        200,
        "S_TEXT/UTF8",
        &[],
        &[ClusterSpec { tc: 5000, subs: vec![(10, 900, "大编号轨道".into())] }],
    );
    let p = write_tmp("native-track200", &mkv);
    let res = extract_native(&p.to_string_lossy(), 200, mkv.len() as u64).unwrap();
    let b = &res.blocks.unwrap()[0];
    assert_eq!((b.time, b.text.as_str()), (5010, "大编号轨道"));
}

#[test]
fn native_unknown_size_segment() {
    // ffmpeg 流式封装形态：Segment 尺寸 = 01 00…00
    let mut mkv = build_head_unknown_segment(3, "S_TEXT/UTF8", &[]);
    let cluster = {
        let mut cl = el_u(&[0xe7], 2000u64);
        cl.extend(block_group(3, 7, "未知尺寸段里的字幕".as_bytes(), 1500));
        el(&[0x1f, 0x43, 0xb6, 0x75], &cl)
    };
    mkv.extend(cluster);
    let p = write_tmp("native-unknown-seg", &mkv);
    let res = extract_native(&p.to_string_lossy(), 3, mkv.len() as u64).unwrap();
    let b = &res.blocks.unwrap()[0];
    assert_eq!((b.time, b.duration, b.text.as_str()), (2007, 1500, "未知尺寸段里的字幕"));
}

#[test]
fn native_missing_track_and_empty_track() {
    let mkv = build_mkv(3, "S_TEXT/UTF8", &[], &[]);
    let p = write_tmp("native-empty", &mkv);
    // 不存在的轨道号
    let err = extract_native(&p.to_string_lossy(), 9, mkv.len() as u64).unwrap_err();
    assert!(err.contains("找不到"), "实际错误：{err}");
    // 存在但没有任何字幕块
    let err = extract_native(&p.to_string_lossy(), 3, mkv.len() as u64).unwrap_err();
    assert!(err.contains("没有可显示"), "实际错误：{err}");
}

#[test]
fn native_pgs_rejected_at_entry() {
    let mkv = build_mkv(3, "S_HDMV/PGS", &[], &[ClusterSpec { tc: 0, subs: vec![(0, 100, "x".into())] }]);
    let p = write_tmp("native-pgs", &mkv);
    let err = extract_auto(&p.to_string_lossy(), 3).unwrap_err();
    assert!(err.contains("未适配") || err.contains("图形"), "实际错误：{err}");
}

#[test]
fn head_walk_resolves_track_table() {
    // 头走读（不扫 Cluster）即可拿到轨道表与编码
    let mkv = build_mkv(
        5,
        "S_TEXT/ASS",
        b"[Script Info]\nH",
        &[ClusterSpec { tc: 0, subs: vec![(0, 100, "b".into())] }],
    );
    let file = File::open(write_tmp("head-1", &mkv)).unwrap();
    let out = walk_mkv(&mut BufReader::with_capacity(BUF, file), mkv.len() as u64, None).unwrap();
    assert_eq!(out.metas.len(), 2);
    assert_eq!(out.metas[1].number, 5);
    assert_eq!(out.metas[1].codec_id, "S_TEXT/ASS");
    assert_eq!(out.metas[1].kind, TRACK_SUBTITLE);
    assert!(!out.metas[1].codec_private.is_empty());
}

/* ---------------- 大文件 / 巨块 seek（引擎 ②） ---------------- */

#[test]
fn native_sparse_4gb_seek() {
    // 真实大 MKV 的语义验证：字幕块散布全部 Cluster，视频/音频块用
    // seek 直接跳过（不逐字节读）。构造：头部 Cluster（头部字幕）+
    // 尾部 Cluster（内含一个 ~3.9GB 的巨型视频 SimpleBlock + 尾部
    // 字幕 BlockGroup）——巨块载荷是稀疏零洞，引擎应一次 seek 跨过
    // 4GB（> 2^31）区间直达尾部字幕块，全程只读真实落盘的几 KB。
    // （不伪造零洞之外的垃圾：零填充按流尾处理是引擎既定语义。）
    let mut head = build_head_unknown_segment(3, "S_TEXT/UTF8", &[]);
    let head_cluster = {
        let mut cl = el_u(&[0xe7], 0u64);
        cl.extend(block_group(3, 0, "头部字幕".as_bytes(), 1000));
        el(&[0x1f, 0x43, 0xb6, 0x75], &cl)
    };
    head.extend(head_cluster);
    // 尾部字幕块（绝对偏移 tail_at 处）：cluster 时码 3_600_000（1 小时，ms 单位）
    let tail = block_group(3, 25, "尾部字幕".as_bytes(), 2000);
    let tail_at: u64 = 3_900_000_000;

    let path = tmp_path("sparse-4gb");
    let file_len;
    {
        let mut f = File::create(&path).unwrap();
        f.write_all(&head).unwrap();
        let cluster_data_start = f.stream_position().unwrap();
        // 尾部 Cluster 头 + Timecode + 巨型 SimpleBlock 头。
        // 此处所有 vint（cluster_len / sb_payload_len，均 ~3.9GB）都落在
        // 5 字节域 [2^28, 2^35)，头部宽度可先验固定：
        // cluster 元素头 = ID(4) + 尺寸(5) = 9；SimpleBlock 头 = ID(1) + 尺寸(5) = 6
        let tc = el_u(&[0xe7], 3_600_000u64);
        let cluster_hdr_len: u64 = 4 + 5;
        let sb_hdr_len: u64 = 1 + 5;
        let payload_start = cluster_data_start + cluster_hdr_len + tc.len() as u64 + sb_hdr_len;
        let sb_payload_len = tail_at - payload_start; // SimpleBlock 载荷：4B 块头 + 稀疏零洞
        let cluster_len = tail_at + tail.len() as u64 - (cluster_data_start + cluster_hdr_len);
        // 5 字节 vint 成立域断言（算术依赖）
        assert_eq!(vs(cluster_len).len(), 5);
        assert_eq!(vs(sb_payload_len).len(), 5);
        f.write_all(&[0x1f, 0x43, 0xb6, 0x75]).unwrap();
        f.write_all(&vs(cluster_len)).unwrap();
        f.write_all(&tc).unwrap();
        f.write_all(&[0xa3]).unwrap();
        f.write_all(&vs(sb_payload_len)).unwrap();
        // SimpleBlock 载荷真头：track vint + int16 相对时码 + flags（关键帧，无 lacing）
        f.write_all(&vs(1)).unwrap();
        f.write_all(&0i16.to_be_bytes()).unwrap();
        f.write_all(&[0x80]).unwrap();
        debug_assert_eq!(f.stream_position().unwrap(), payload_start + 4);
        // 稀疏零洞直到 tail_at（不落盘），再写尾部字幕块
        f.set_len(tail_at).unwrap();
        f.seek(SeekFrom::Start(tail_at)).unwrap();
        f.write_all(&tail).unwrap();
        file_len = f.stream_position().unwrap();
    }

    let t0 = std::time::Instant::now();
    let res = extract_native(&path.to_string_lossy(), 3, file_len).unwrap();
    let dt = t0.elapsed();
    let blocks = res.blocks.unwrap();
    assert_eq!(blocks.len(), 2, "头部 + 尾部各一块");
    assert_eq!(blocks[0].text, "头部字幕");
    assert_eq!((blocks[1].time, blocks[1].text.as_str()), (3_600_025, "尾部字幕"));
    // 4GB 巨块必须被 seek 跳过（顺序读零洞在本环境会拖到分钟级）
    assert!(dt.as_secs() < 30, "稀疏 4GB 解析耗时异常：{dt:?}");
}

/* ---------------- 引擎 ①：索引会话（单套件串行，共享全局会话表） ---------------- */

// 会话表是进程级全局：拆多个 #[test] 并行跑会互相逐出，故合成一个
// 串行套件覆盖全部会话行为（子场景用块作用域隔离）。
#[test]
fn session_engine_suite() {
    let clusters = vec![
        ClusterSpec { tc: 0, subs: vec![(0, 2500, "第一句".into()), (3000, 2000, "第二句".into())] },
        ClusterSpec { tc: 60_000, subs: vec![(500, 4000, "一小时处的字幕".into())] },
        ClusterSpec { tc: 120_000, subs: vec![(0, 1000, "两小时处".into())] },
    ];

    /* 窗口与全量逐块一致 + 边界互斥 + 空窗 */
    {
        let mkv = build_mkv_indexed(3, "S_TEXT/UTF8", &[], &clusters);
        let p = write_tmp("session-parity", &mkv);
        let path = p.to_string_lossy().to_string();
        let file_len = mkv.len() as u64;

        let info = session_open(&path, 3).unwrap();
        assert_eq!(info.kind, "utf8");
        assert_eq!(info.total_blocks, 4, "每个字幕块一条索引");
        assert!(info.header.is_none());

        // 引擎 ② 全量作对照
        let full = extract_native(&path, 3, file_len).unwrap().blocks.unwrap();

        // 全片两窗覆盖：[0, 90s) + [90s, 180s)，边界互斥不重不漏
        let w1 = session_window(info.session_id, 0, 90_000).unwrap();
        let w2 = session_window(info.session_id, 90_000, 180_000).unwrap();
        assert_eq!((w1.skipped, w2.skipped), (0, 0));
        assert_eq!(w1.blocks.len(), 3, "0ms / 3000ms / 60500ms 在窗 1");
        assert_eq!(w2.blocks.len(), 1, "120000ms 在窗 2");
        let mut merged = w1.blocks;
        merged.extend(w2.blocks);
        merged.sort_by_key(|b| b.time);
        assert_eq!(merged, full, "窗口并集应与全量走读逐块一致");

        // 对白之外的空窗：0 块、0 跳过
        let w3 = session_window(info.session_id, 200_000, 300_000).unwrap();
        assert_eq!((w3.blocks.len(), w3.skipped), (0, 0));

        // seek 空洞：只看过 [0,10s) 后直接跳查 [100s,200s) 也能取到
        let w4 = session_window(info.session_id, 0, 10_000).unwrap();
        assert_eq!(w4.blocks.len(), 2);
        let w5 = session_window(info.session_id, 100_000, 200_000).unwrap();
        assert_eq!(w5.blocks.len(), 1, "120000ms 的块");
        assert_eq!(w5.blocks[0].text, "两小时处");
        session_close(info.session_id);
    }

    /* ASS 字段经窗口路径不丢（style/layer/margins/effect + 头） */
    {
        let mkv = build_mkv_indexed(
            4,
            "S_TEXT/ASS",
            b"[Script Info]\nHDR-Q",
            &[ClusterSpec {
                tc: 1000,
                subs: vec![(20, 3000, "7,0,StyleW,N,1,2,3,FX,窗口里的对白".into())],
            }],
        );
        let p = write_tmp("session-ass", &mkv);
        let path = p.to_string_lossy().to_string();
        let info = session_open(&path, 4).unwrap();
        assert_eq!(info.kind, "ass");
        assert_eq!(info.header.as_deref(), Some("[Script Info]\nHDR-Q"));
        let w = session_window(info.session_id, 0, 60_000).unwrap();
        assert_eq!(w.skipped, 0);
        let b = &w.blocks[0];
        assert_eq!((b.time, b.duration, b.text.as_str()), (1020, 3000, "窗口里的对白"));
        assert_eq!((b.layer.as_str(), b.style.as_str(), b.name.as_str()), ("0", "StyleW", "N"));
        assert_eq!(
            (b.margin_l.as_str(), b.margin_r.as_str(), b.margin_v.as_str(), b.effect.as_str()),
            ("1", "2", "3", "FX")
        );
        session_close(info.session_id);
    }

    /* 无索引：引擎 ① 报降级标记，引擎 ② 全量仍可用 */
    {
        let mkv = build_mkv(3, "S_TEXT/UTF8", &[], &clusters); // 无 SeekHead / Cues
        let p = write_tmp("session-noindex", &mkv);
        let path = p.to_string_lossy().to_string();
        let err = session_open(&path, 3).unwrap_err();
        assert!(err.contains("Cues"), "应报索引缺失：{err}");
        let res = extract_auto(&path, 3).unwrap(); // 降级路径
        assert_eq!(res.method, "native");
        assert_eq!(res.blocks.unwrap().len(), 4);
    }

    /* 定性错误文案与全量入口一致（前端按同套判据短路不走兜底） */
    {
        let mkv = build_mkv_indexed(3, "S_TEXT/UTF8", &[], &clusters);
        let p = write_tmp("session-definite", &mkv);
        let path = p.to_string_lossy().to_string();
        let err = session_open(&path, 9).unwrap_err();
        assert!(err.contains("找不到"), "实际：{err}");
        let err = session_open(&path, 1).unwrap_err(); // 视频轨
        assert!(err.contains("不是字幕轨"), "实际：{err}");
    }
    {
        let mkv = build_mkv_indexed(3, "S_HDMV/PGS", &[], &[ClusterSpec { tc: 0, subs: vec![(0, 100, "x".into())] }]);
        let p = write_tmp("session-pgs", &mkv);
        let err = session_open(&p.to_string_lossy(), 3).unwrap_err();
        assert!(err.contains("未适配") || err.contains("图形"), "实际：{err}");
    }

    /* 会话失效（不存在 / 已关闭 / 被逐出） */
    {
        let err = session_window(999_999, 0, 1000).unwrap_err();
        assert!(err.contains("失效"), "实际：{err}");

        let mkv = build_mkv_indexed(3, "S_TEXT/UTF8", &[], &clusters);
        let p = write_tmp("session-evict", &mkv);
        let path = p.to_string_lossy().to_string();
        let ids: Vec<u64> = (0..SESSION_CAP as u64 + 2)
            .map(|_| session_open(&path, 3).unwrap().session_id) // 逐个打开
            .collect();
        // 全部打开后：最旧的（ids 首个）应已被逐出
        let err = session_window(ids[0], 0, 200_000).unwrap_err();
        assert!(err.contains("失效"), "最旧会话应被逐出：{err}");
        // 最新的仍可用
        let w = session_window(*ids.last().unwrap(), 0, 200_000).unwrap();
        assert_eq!(w.blocks.len(), 4);
        for id in &ids[1..] {
            session_close(*id);
        }
    }
}

/* ---------------- 真实媒体对照（可选） ---------------- */

/// KP_TEST_MKV=/path/multi.mkv cargo test real_media -- --nocapture
#[test]
fn real_media_parity() {
    let Ok(path) = std::env::var("KP_TEST_MKV") else {
        eprintln!("[skip] 未设置 KP_TEST_MKV");
        return;
    };
    let file_len = std::fs::metadata(&path).unwrap().len();
    // 头走读找 SRT / ASS 轨
    let file = File::open(&path).unwrap();
    let head = walk_mkv(&mut BufReader::with_capacity(BUF, file), file_len, None).unwrap();
    let srt = head.metas.iter().find(|m| m.codec_id == "S_TEXT/UTF8").expect("应有 SRT 轨");
    let ass = head.metas.iter().find(|m| m.codec_id == "S_TEXT/ASS").expect("应有 ASS 轨");

    let res = extract_native(&path, srt.number, file_len).unwrap();
    let blocks = res.blocks.unwrap();
    assert_eq!(blocks.len(), 6, "multi.mkv 的 SRT 轨应为 6 块");
    assert_eq!(blocks[0].text, "欢迎观看极速看片");

    let res = extract_native(&path, ass.number, file_len).unwrap();
    let blocks = res.blocks.unwrap();
    assert_eq!(blocks.len(), 4, "multi.mkv 的 ASS 轨应为 4 块");
    assert!(blocks[0].text.contains("内嵌ASS字幕"));
    assert_eq!(blocks[0].style, "Default");

    // 全量入口（引擎 ②）可走通
    let r = extract_auto(&path, srt.number).unwrap();
    assert_eq!(r.method, "native");

    // 有索引时：全片窗口扫的并集应与全量逐块一致
    if let Ok(info) = session_open(&path, srt.number) {
        let full = extract_native(&path, srt.number, file_len).unwrap().blocks.unwrap();
        let max_t = full.last().map(|b| b.time).unwrap_or(0) + 1;
        let mut merged = Vec::new();
        let mut from = 0u64;
        while from <= max_t {
            let w = session_window(info.session_id, from, from + 60_000).unwrap();
            assert_eq!(w.skipped, 0, "索引条目应全部命中");
            merged.extend(w.blocks);
            from += 60_000;
        }
        merged.sort_by_key(|b| b.time);
        assert_eq!(merged, full, "窗口并集 == 全量");
        session_close(info.session_id);
    } else {
        eprintln!("[info] multi.mkv 无可用索引，跳过会话对照");
    }
}
