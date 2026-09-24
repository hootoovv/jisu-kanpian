/**
 * 极速看片 —— Matroska / WebM 轨道分析器（手写 EBML 游走）
 *
 * 作用与 mp4/analyzer.js 对等：只读容器头部（Segment.Info + Segment.Tracks），
 * 拿到全部轨道的编号 / 类型 / 编码 / 语言，驱动音轨菜单与字幕菜单。
 *
 * 为什么手写而不用库：
 * - npm `matroska` 系 Node 专用（fs/http），浏览器不可用（调研记录
 *   见 docs/架构设计.md §3.6）；
 * - 我们只需要「读 TrackEntry」这一个动作，EBML 变长整数 + 顶层元素
 *   遍历约 200 行，与 analyzer.js 手写 box 游走的项目风格一致。
 *
 * Matroska 结构要点（这里只解析用到的子集）：
 * - EBML 头（0x1A45DFA3）→ Segment（0x18538067）为顶层；
 * - Segment 内：Info（0x1549A966：TimecodeScale/Duration）与
 *   Tracks（0x1654AE6B）按规范应位于第一个 Cluster 之前 →
 *   读头部一小段即可命中（从 512KB 起步，截断则扩窗重试至 16MB）；
 * - TrackEntry（0xAE）子元素：TrackNumber(0xD7,u) TrackType(0x83,u：
 *   1=视频 2=音频 0x11=字幕) CodecID(0x86,s) Name(0x536E,utf8)
 *   Language(0x22B59C,s) LanguageIETF(0x22B59D,s)；
 * - Video(0xE0：PixelWidth 0xB0 / PixelHeight 0xBA)、
 *   Audio(0xE1：Channels 0x9F / SamplingFrequency 0xB5)。
 */
import { invoke } from '@tauri-apps/api/core';

/** 初始头部读取窗口；Tracks 被截断时扩窗重试的上限 */
const HEAD_START = 512 * 1024;
const HEAD_MAX = 16 * 1024 * 1024;

/** Matroska 元素 ID（保持与规范一致的数值） */
const ID_SEGMENT = 0x18538067;
const ID_INFO = 0x1549a966;
const ID_TRACKS = 0x1654ae6b;
const ID_TRACK_ENTRY = 0xae;
const ID_TRACK_NUMBER = 0xd7;
const ID_TRACK_TYPE = 0x83;
const ID_CODEC_ID = 0x86;
const ID_NAME = 0x536e;
const ID_LANGUAGE = 0x22b59c;
const ID_LANGUAGE_IETF = 0x22b59d;
const ID_VIDEO = 0xe0;
const ID_AUDIO = 0xe1;
const ID_PIXEL_WIDTH = 0xb0;
const ID_PIXEL_HEIGHT = 0xba;
const ID_CHANNELS = 0x9f;
const ID_SAMPLING_FREQ = 0xb5;
const ID_TIMECODE_SCALE = 0x2ad7b1;
const ID_DURATION = 0x4489;
const ID_DEFAULT_DURATION = 0x23e383;
const ID_EBML = 0x1a45dfa3;
const ID_CLUSTER = 0x1f43b675;

/* ---------------- EBML 基元读取（全部对 Uint8Array 视图操作） ---------------- */

/** 读一个 EBML ID 变长整数（不剥首位标记，直接拼原始位） */
function readId(u8, pos) {
  if (pos >= u8.length) return null;
  const first = u8[pos];
  let len = 0;
  for (let i = 7; i >= 0; i--) {
    if (first & (1 << i)) { len = 8 - i; break; }
  }
  if (!len || pos + len > u8.length) return null;
  let v = 0;
  for (let i = 0; i < len; i++) v = v * 256 + u8[pos + i];
  return { value: v, length: len };
}

/** 读一个 EBML size 变长整数。
 *  返回 { value, length, unknown }——unknown=true 表示「未知尺寸」
 *  （Segment / Cluster 等流式封装常见）：全 1（RFC 8794）或「仅标
 *  记位、其余全 0」的 8 字节形态（ffmpeg 的写法）。
 *
 *  ⚠️ 精度陷阱（实测踩坑）：不能「先按整数读完整 vint 再减标记位」
 *  ——8 字节 vint 的原值在 2^56 量级，超出 JS 安全整数（2^53），
 *  小尺寸（如 82）会被浮点舍入成 80 导致整棵树错位。必须**按字节**
 *  剥掉首字节的标记位再累积；未知尺寸也按字节比对（全 1 / 01+全 0），
 *  不做整数等值判断。 */
function readSize(u8, pos) {
  if (pos >= u8.length) return null;
  const first = u8[pos];
  let len = 0;
  for (let i = 7; i >= 0; i--) {
    if (first & (1 << i)) { len = 8 - i; break; }
  }
  if (!len || pos + len > u8.length) return null;

  // 首字节数据位掩码（len=1 → 低 7 位，…，len=8 → 0 位）
  const firstMask = 0xff >> len;
  let v = first & firstMask;
  for (let i = 1; i < len; i++) v = v * 256 + u8[pos + i];

  // 未知尺寸的两种字节形态（见 docstring）
  let unknown = false;
  if (first === 0xff && firstMask === 0x7f) {
    unknown = true; // 单字节 0xFF（全 1）
  } else {
    let allFF = first === 0xff;
    for (let i = 1; allFF && i < len; i++) allFF = u8[pos + i] === 0xff;
    if (allFF) unknown = true;
    if (len === 8 && first === 0x01) {
      let allZero = true;
      for (let i = 1; allZero && i < len; i++) allZero = u8[pos + i] === 0x00;
      if (allZero) unknown = true; // ffmpeg：01 00…00
    }
  }
  return { value: v, length: len, unknown };
}

/** 读无符号整数字段值（>6 字节的罕见值用浮点权值兜底） */
function readUint(u8, pos, size) {
  if (pos + size > u8.length) return null;
  let v = 0;
  for (let i = 0; i < size && i < 6; i++) v = v * 256 + u8[pos + i];
  for (let i = 6; i < size; i++) v += u8[pos + i] * Math.pow(256, i);
  return v;
}

/** 读浮点字段值（4/8 字节；其它长度返回 null） */
function readFloat(u8, pos, size) {
  try {
    if (size === 4) return new DataView(u8.buffer, u8.byteOffset + pos, 4).getFloat32(0, false);
    if (size === 8) return new DataView(u8.buffer, u8.byteOffset + pos, 8).getFloat64(0, false);
  } catch {
    /* 越界 */
  }
  return null;
}

/** 读 ASCII / UTF-8 字符串字段值 */
function readString(u8, pos, size) {
  if (pos + size > u8.length) return null;
  try {
    return new TextDecoder('utf-8').decode(u8.subarray(pos, pos + size)).replace(/\0+$/, '');
  } catch {
    return '';
  }
}

/**
 * 遍历 [start, end) 里的一层子元素，对每个元素回调
 * cb(id, headerLen, dataStart, dataSize)（返回 true 提前终止）。
 *
 * 未知尺寸（size.unknown）的元素按「延伸到本层末尾」处理——只有
 * master 元素允许未知尺寸（流式封装的 Segment / Cluster），这正是
 * 我们需要的语义：Segment 撑满读取窗口、Cluster 之后的都跳过。
 *
 * 返回值：
 *  - null：本层完整遍历（或 cb 主动终止）；
 *  - 'truncated'：还有元素但数据不全。
 *
 * allowClipped=true 时（顶层专用）：数据超出 end 的元素以裁剪后的
 * 尺寸回调一次再返回 'truncated'——Segment 的 size 几乎永远大于
 * 读取窗口，没有此模式顶层根本无法命中 Segment。
 */
function scanChildren(u8, start, end, cb, allowClipped = false) {
  let p = start;
  while (p < end) {
    const id = readId(u8, p);
    if (id == null) return 'truncated';
    const size = readSize(u8, p + id.length);
    if (size == null) return 'truncated';
    const headerLen = id.length + size.length;
    const dataStart = p + headerLen;
    const dataEnd = size.unknown ? end : dataStart + size.value;
    if (dataEnd > end) {
      if (allowClipped && dataStart <= end) {
        cb(id.value, headerLen, dataStart, end - dataStart);
      }
      return 'truncated';
    }
    if (cb(id.value, headerLen, dataStart, dataEnd - dataStart)) return null;
    p = dataEnd;
  }
  return null;
}

/* ---------------- 轨道表解析 ---------------- */

/** TrackEntry 内容 → 轨道描述（与 analyzeMp4 的轨道结构对齐） */
function parseTrackEntry(u8, start, end) {
  const track = {
    id: 0,           // TrackNumber（Matroska 块里引用的编号）
    kind: 'other',   // video | audio | subtitle | other
    codecId: '',     // 如 A_AAC / S_TEXT/ASS
    lang: 'und',
    name: '',
    width: 0,
    height: 0,
    channels: 0,
    sampleRate: 0,
    defaultDurationNs: 0
  };
  scanChildren(u8, start, end, (id, _h, ds, size) => {
    switch (id) {
      case ID_TRACK_NUMBER: track.id = readUint(u8, ds, size) || 0; break;
      case ID_TRACK_TYPE: {
        const t = readUint(u8, ds, size) || 0;
        track.kind = t === 1 ? 'video' : t === 2 ? 'audio' : t === 0x11 ? 'subtitle' : 'other';
        break;
      }
      case ID_CODEC_ID: track.codecId = readString(u8, ds, size) || ''; break;
      case ID_NAME: track.name = readString(u8, ds, size) || ''; break;
      case ID_LANGUAGE: track.lang = (readString(u8, ds, size) || 'und').toLowerCase(); break;
      case ID_LANGUAGE_IETF: track.lang = (readString(u8, ds, size) || track.lang).toLowerCase(); break;
      case ID_DEFAULT_DURATION: track.defaultDurationNs = readUint(u8, ds, size) || 0; break;
      case ID_VIDEO:
        scanChildren(u8, ds, ds + size, (vid, _vh, vds, vsz) => {
          if (vid === ID_PIXEL_WIDTH) track.width = readUint(u8, vds, vsz) || 0;
          else if (vid === ID_PIXEL_HEIGHT) track.height = readUint(u8, vds, vsz) || 0;
          return false;
        });
        break;
      case ID_AUDIO:
        scanChildren(u8, ds, ds + size, (aid, _ah, ads, asz) => {
          if (aid === ID_CHANNELS) track.channels = readUint(u8, ads, asz) || 0;
          else if (aid === ID_SAMPLING_FREQ) track.sampleRate = Math.round(readFloat(u8, ads, asz) || 0);
          return false;
        });
        break;
      default:
        break;
    }
    return false;
  });
  return track;
}

/** CodecID → 字幕类型（matroska-subtitles 的 type 命名对齐，便于复用） */
export function subtitleKindOf(codecId) {
  const c = String(codecId || '').toUpperCase();
  if (c === 'S_TEXT/UTF8') return 'utf8';
  if (c === 'S_TEXT/SSA' || c === 'S_SSA' || c === 'S_TEXT/ASS' || c === 'S_ASS') return 'ass';
  if (c === 'S_TEXT/WEBVTT' || c === 'S_WEBVTT') return 'webvtt';
  if (c === 'S_HDMV/PGS') return 'pgs';
  if (c === 'S_VOBSUB') return 'vobsub';
  if (c === 'S_KATE') return 'kate';
  return '';
}

/** CodecID → 友好编码名（音轨菜单展示用） */
function codecLabelOf(codecId) {
  const c = String(codecId || '').toUpperCase().replace(/^A_/, '');
  if (c.startsWith('AAC')) return 'AAC';
  if (c === 'OPUS') return 'Opus';
  if (c === 'VORBIS') return 'Vorbis';
  if (c === 'MP3' || c === 'MPEG/L3') return 'MP3';
  if (c === 'FLAC') return 'FLAC';
  if (c === 'AC3') return 'AC-3';
  if (c === 'EAC3') return 'E-AC-3';
  if (c === 'ALAC') return 'ALAC';
  if (c === 'TRUEHD') return 'TrueHD';
  if (c === 'DTS') return 'DTS';
  if (c === 'PCM/INT/LIT') return 'PCM';
  return c || '未知编码';
}

/**
 * 解析一段头部缓冲 → analyzeMp4 同构的元数据。
 * 返回 null 表示这段缓冲还不足以定位完整的 Info+Tracks（调用方扩窗重读）。
 */
function parseHead(u8) {
  let durationSec = 0;
  let timecodeScale = 1e6; // 纳秒（规范默认值）
  const tracks = [];
  let sawTracks = false;

  // 顶层：EBML 头（完整）→ Segment（size 几乎必然超出窗口 → 裁剪模式）
  let segStart = -1;
  let segEnd = -1;
  scanChildren(
    u8, 0, u8.length,
    (id, _h, ds, size) => {
      if (id === ID_EBML) return false; // EBML 头：跳过
      if (id === ID_SEGMENT) {
        segStart = ds;
        segEnd = Math.min(ds + size, u8.length);
        return true;
      }
      return false;
    },
    true
  );
  if (segStart === -1) return null;

  // Segment 内第一层：找 Info 与 Tracks（Cluster 之前的头部元素）
  let truncated = false;
  scanChildren(u8, segStart, segEnd, (id, _h, ds, size) => {
    if (id === ID_INFO) {
      scanChildren(u8, ds, ds + size, (iid, _ih, ids, isz) => {
        if (iid === ID_TIMECODE_SCALE) timecodeScale = readUint(u8, ids, isz) || 1e6;
        else if (iid === ID_DURATION) {
          const f = readFloat(u8, ids, isz);
          if (typeof f === 'number' && isFinite(f) && f > 0) {
            durationSec = (f * timecodeScale) / 1e9; // Duration 的单位是 TimecodeScale
          }
        }
        return false;
      });
    } else if (id === ID_TRACKS) {
      const r = scanChildren(u8, ds, ds + size, (tid, _th, tds, tsz) => {
        if (tid === ID_TRACK_ENTRY) tracks.push(parseTrackEntry(u8, tds, tds + tsz));
        return false;
      });
      if (r === 'truncated') truncated = true;
      else sawTracks = true;
    }
    return id === ID_CLUSTER; // 命中 Cluster 即终止：头部元素应已到齐
  });
  if (truncated || !sawTracks) return null;

  // 轨道分类 → 与 analyzeMp4 相同的出口结构
  const videoTracks = [];
  const audioTracks = [];
  const subtitleTracks = [];
  for (const t of tracks) {
    if (t.kind === 'video') {
      videoTracks.push({
        id: t.id, codec: codecLabelOf(t.codecId), lang: t.lang || 'und', name: t.name,
        width: t.width, height: t.height,
        fps: t.defaultDurationNs > 0 ? Math.round((1e9 / t.defaultDurationNs) * 100) / 100 : 0
      });
    } else if (t.kind === 'audio') {
      audioTracks.push({
        id: t.id, codec: codecLabelOf(t.codecId), codecId: t.codecId,
        lang: t.lang || 'und', name: t.name, channels: t.channels, sampleRate: t.sampleRate
      });
    } else if (t.kind === 'subtitle') {
      subtitleTracks.push({
        id: t.id, codec: subtitleKindOf(t.codecId) || 'unknown',
        codecId: t.codecId, lang: t.lang || 'und', name: t.name
      });
    }
  }
  if (!videoTracks.length) return null; // 纯音频 mkv 交给原生逻辑（无画面不进播放器主流程）
  return {
    container: 'matroska',
    durationSec,
    video: videoTracks[0],
    videoTracks,
    audioTracks,
    subtitleTracks
  };
}

/**
 * 分析一个 Matroska / WebM 文件（只读头部轨道表，媒体数据不过 IPC）。
 * 返回结构与 analyzeMp4 对齐；失败返回 null（调用方回退原生播放）。
 * @param {string} path 绝对路径
 */
export async function analyzeMkv(path) {
  const fileSize = Number(await invoke('stat_file', { path })) || 0;
  if (!fileSize) return null;

  let win = Math.min(fileSize, HEAD_START);
  while (true) {
    const head = new Uint8Array(await invoke('read_range', { path, offset: 0, length: win }));
    const info = parseHead(head);
    if (info) return info;
    if (win >= Math.min(fileSize, HEAD_MAX) || win >= fileSize) return null;
    win = Math.min(fileSize, win * 4); // 指数扩窗：512K → 2M → 8M → 16M
  }
}
