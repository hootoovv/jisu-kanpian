//! mkvsub.rs 单元测试 —— 合成 MKV 字节流驱动，不依赖外部工具与真实媒体。
//!
//! 覆盖面：
//! - 原生解析：SRT 块时间/时长、ASS/SSA 字段拆分、双字节 TrackNumber、
//!   未知尺寸 Segment（ffmpeg 01+00…00 形态）、视频块 seek 跳过；
//! - 稀疏 4GB 大文件：验证大偏移 seek 与「内存只驻留字幕块」的流式语义；
//! - ffmpeg 引擎：PATH 有 ffmpeg 时端到端（真实子进程），无则跳过；
//! - 引擎降级：ffmpeg 失败 → 原生接手；
//! - 真实媒体对照：环境变量 KP_TEST_MKV 指向 multi.mkv 时做已知内容断言。

use super::*;
use std::io::Write as _;
use std::time::Instant;

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

/// 组装一颗合成 MKV（EBML 头 + 已知尺寸 Segment + Info/Tracks/Clusters）
fn build_mkv(sub_number: u64, codec: &str, private: &[u8], clusters: &[ClusterSpec]) -> Vec<u8> {
    // Info（TimecodeScale = 1e6 → 1 单位 = 1ms）
    let info = el_u(&[0x2a, 0xd7, 0xb1], 1_000_000);
    // Tracks：视频轨(1) + 目标字幕轨
    let mut tracks = Vec::new();
    let mut video = Vec::new();
    video.extend(el_u(&[0xd7], 1));
    video.extend(el_u(&[0x83], 1));
    video.extend(el_s(&[0x86], "V_VP9"));
    // Video 元素（PixelWidth/Height）：ffmpeg 的 matroska demuxer 对
    // 视频轨硬性要求它存在，缺了 read_header 直接 Invalid data
    // （ffprobe 7.1 实测；不加则引擎①的端到端测试无法跑）
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
    // Clusters：视频垃圾块（SimpleBlock，track 1）+ 字幕 BlockGroup + Void
    let junk = vec![0xabu8; 128 * 1024];
    let mut body = Vec::new();
    body.extend(el(&[0x15, 0x49, 0xa9, 0x66], &info));
    body.extend(el(&[0x16, 0x54, 0xae, 0x6b], &tracks));
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
    // EBML 头 + Segment
    let mut eh = Vec::new();
    eh.extend(el_s(&[0x42, 0x82], "matroska"));
    eh.extend(el_u(&[0x42, 0xf2], 4));
    eh.extend(el_u(&[0x42, 0xf3], 8));
    let mut out = el(&[0x1a, 0x45, 0xdf, 0xa3], &eh);
    out.extend(el(&[0x18, 0x53, 0x80, 0x67], &body));
    out
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

/* ---------------- 原生引擎 ---------------- */

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

/* ---------------- 大文件 / 巨块 seek ---------------- */

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

    let t0 = Instant::now();
    let res = extract_native(&path.to_string_lossy(), 3, file_len).unwrap();
    let dt = t0.elapsed();
    let blocks = res.blocks.unwrap();
    assert_eq!(blocks.len(), 2, "头部 + 尾部各一块");
    assert_eq!(blocks[0].text, "头部字幕");
    assert_eq!((blocks[1].time, blocks[1].text.as_str()), (3_600_025, "尾部字幕"));
    // 4GB 巨块必须被 seek 跳过（顺序读零洞在本环境会拖到分钟级）
    assert!(dt.as_secs() < 30, "稀疏 4GB 解析耗时异常：{dt:?}");
}

/* ---------------- ffmpeg 解析（跨平台探测） ---------------- */

#[test]
fn ffmpeg_names_match_platform() {
    let names = ffmpeg_candidate_names();
    // Windows 必须显式探测 .exe（winget / scoop / choco / 官方构建的
    // 分发形态一律是 ffmpeg.exe）；其它平台是裸名 ffmpeg
    if cfg!(windows) {
        assert_eq!(names[0], "ffmpeg.exe", "Windows 首选 ffmpeg.exe");
        assert!(names.contains(&"ffmpeg"), "无扩展名形态兜底");
    } else {
        assert_eq!(names, &["ffmpeg"]);
    }
}

#[test]
fn path_parse_skips_empty_entries() {
    // "a;;b" 形态含空项（shell 语义 = 当前目录），必须剔除：
    // 一是行为确定（结果不随 CWD 漂移），二是防不受控目录里的同名文件
    let raw = if cfg!(windows) {
        OsStr::new("C:\\tools;;C:\\bin")
    } else {
        OsStr::new("/usr/bin::/opt/tools")
    };
    let dirs = dirs_from_path(raw);
    assert_eq!(dirs.len(), 2, "空 PATH 项应被跳过：{dirs:?}");
    assert!(dirs.iter().all(|d| !d.as_os_str().is_empty()));
}

#[test]
#[cfg(unix)]
fn candidate_needs_exec_bit() {
    use std::os::unix::fs::PermissionsExt;
    let d = std::env::temp_dir().join("jisu-kanpian-mkvsub-tests");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("ffmpeg-execbit-probe");
    std::fs::write(&p, b"#!sh\n").unwrap();
    let set_mode = |mode: u32| {
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    set_mode(0o644); // 无任何可执行位
    assert!(!is_runnable_candidate(&p), "无可执行位的文件不应成为候选");
    set_mode(0o755);
    assert!(is_runnable_candidate(&p), "带可执行位的普通文件应是候选");
    let _ = std::fs::remove_file(&p);
}

#[test]
fn resolved_exe_runs_and_matches_platform_name() {
    if !ffmpeg_available() {
        eprintln!("[skip] PATH 无 ffmpeg");
        return;
    }
    let exe = ffmpeg_exe().expect("available 为真时必有解析结果");
    // 解析出的路径必须能独立跑起来（不是只检查存在性）
    assert!(probe_ffmpeg(exe), "解析出的路径必须可运行");
    // 文件名符合平台约定（Windows 大小写不敏感 → 统一小写比较）
    let name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
    if cfg!(windows) {
        assert!(
            name == "ffmpeg.exe" || name == "ffmpeg",
            "Windows 候选文件名不符：{name}"
        );
    } else {
        assert_eq!(name, "ffmpeg");
    }
    // 解析结果必须落在真实存在的目录里（非相对 CWD 的幽灵路径）
    assert!(exe.parent().is_some_and(|d| d.is_dir()), "父目录应存在：{exe:?}");
}

/* ---------------- ffmpeg 引擎 / 降级 ---------------- */

#[test]
fn ffmpeg_engine_end_to_end() {
    if !ffmpeg_available() {
        eprintln!("[skip] PATH 无 ffmpeg，跳过 ffmpeg 引擎端到端");
        return;
    }
    let mkv = build_mkv(
        3,
        "S_TEXT/UTF8",
        &[],
        &[
            ClusterSpec { tc: 0, subs: vec![(500, 2500, "Hello ffmpeg".into())] },
            ClusterSpec { tc: 10_000, subs: vec![(0, 3000, "第二句 bye".into())] },
        ],
    );
    let p = write_tmp("ffmpeg-e2e", &mkv);
    let res = extract_auto(&p.to_string_lossy(), 3).unwrap();
    assert_eq!(res.method, "ffmpeg", "有 ffmpeg 时应首选 ffmpeg");
    let text = res.text.unwrap();
    assert!(text.contains("Hello ffmpeg"), "ffmpeg 输出：{text}");
    assert!(text.contains("第二句 bye"), "ffmpeg 输出：{text}");
    assert!(text.contains("-->"), "SRT 应含时间轴：{text}");
}

#[test]
fn ffmpeg_failure_falls_back_to_native() {
    if !ffmpeg_available() {
        eprintln!("[skip] PATH 无 ffmpeg");
        return;
    }
    let mkv = build_mkv(3, "S_TEXT/UTF8", &[], &[ClusterSpec {
        tc: 0,
        subs: vec![(100, 900, "fallback".into())],
    }]);
    let p = write_tmp("ffmpeg-fallback", &mkv);
    // 越界字幕轨序号：ffmpeg 报错 → 原生引擎（按 TrackNumber）成功接手
    let bad = extract_ffmpeg(&p.to_string_lossy(), 9, "srt", mkv.len() as u64);
    assert!(bad.is_err());
    let res = extract_native(&p.to_string_lossy(), 3, mkv.len() as u64).unwrap();
    assert_eq!(res.blocks.unwrap()[0].text, "fallback");
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

    // 有 ffmpeg 时三级链路也应全通
    if ffmpeg_available() {
        let r = extract_auto(&path, srt.number).unwrap();
        let cues = r.text.unwrap().matches("-->").count();
        assert_eq!(cues, 6, "ffmpeg SRT 输出应含 6 条时间轴");
    }
}
