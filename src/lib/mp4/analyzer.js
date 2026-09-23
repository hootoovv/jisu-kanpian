/**
 * 极速看片 —— MP4 元数据分析器
 *
 * 只读 moov 元数据区（通常几十 KB ~ 几 MB），不碰 mdat 媒体数据：
 * 1. 通过 Rust 的 read_range / stat_file 做**顶层 box 游走**：
 *    moov 在文件头（faststart）就顺序读到；在文件尾（非 faststart）
 *    就对尾部做 'moov' 特征扫描定位，两种情况都只传元数据过 IPC；
 * 2. 把 moov 字节喂给一个「只解析」的 MP4Box 实例（createFile(false)
 *    不保留 mdat），onReady 拿到全部轨道信息：
 *    - video：id / codec / 宽高 / fps（nb_samples ÷ 时长）
 *    - audio：id / codec / 语言 / 声道数（多音轨播放的菜单数据）
 *    - subtitle：id / codec（wvtt / tx3g）/ 语言（内嵌字幕菜单数据）
 *
 * 分析结果驱动三件事：音轨菜单、字幕菜单、是否启用 MSE 多音轨引擎
 * （音轨数 > 1 且 MSE 支持该编解码组合时才走 MSE，否则原生 <video>）。
 */
import { invoke } from '@tauri-apps/api/core';
import { createFile } from 'mp4box';

/** 单次区间读取的窗口大小（box 游走按需取 4KB，moov 整块取） */
const HEAD_BYTES = 16;
const MAX_MOOV = 64 * 1024 * 1024; // moov 超过 64MB 视为异常，放弃分析

/** Rust read_range：返回 ArrayBuffer */
async function readRange(path, offset, length) {
  return await invoke('read_range', { path, offset, length });
}

async function statFile(path) {
  return Number(await invoke('stat_file', { path })) || 0;
}

/**
 * 顶层 box 游走定位 moov。
 * 返回 { offset, size, data(ArrayBuffer) }；找不到返回 null。
 *
 * 两条路径：
 * a) 顺序游走（每次按需读 16 字节头）：适合 moov 在前的常规文件；
 *    遇到超大 mdat 直接按 size 跳过，媒体数据永不过 IPC。
 * b) 尾部特征扫描：顺序游走到 EOF 仍没见到 moov（moov 在文件尾的
 *    非流式封装常见形态），从文件尾往前找 [size][m o o v] 特征，
 *    校验 pos + size == 文件尾 后整块读取。
 */
export async function locateMoov(path) {
  const fileSize = await statFile(path);
  if (!fileSize) return null;

  /* ---- a) 顺序游走 ---- */
  let pos = 0;
  for (let guard = 0; guard < 8192; guard++) {
    if (pos + 8 > fileSize) break;
    let head;
    try {
      head = new DataView(await readRange(path, pos, HEAD_BYTES));
    } catch {
      return null;
    }
    if (head.byteLength < 8) break;
    let size = head.getUint32(0);
    const type =
      String.fromCharCode(head.getUint8(4), head.getUint8(5), head.getUint8(6), head.getUint8(7));
    let hdrSize = 8;
    if (size === 1) {
      // 64 位 largesize
      if (head.byteLength < 16) break;
      size = Number(head.getBigUint64(8));
      hdrSize = 16;
    } else if (size === 0) {
      size = fileSize - pos; // 到文件尾
    }
    if (size < hdrSize || pos + size > fileSize) break; // 异常 box，走尾部扫描
    if (type === 'moov') {
      if (size > MAX_MOOV) return null;
      const data = await readRange(path, pos, size);
      if (!data || data.byteLength !== size) return null;
      return { offset: pos, size, data };
    }
    pos += size; // mdat 等大 box 直接按 size 跳过
  }

  /* ---- b) 尾部特征扫描 ---- */
  const tailLen = Math.min(fileSize, 16 * 1024 * 1024);
  const tailStart = fileSize - tailLen;
  let tail;
  try {
    tail = new Uint8Array(await readRange(path, tailStart, tailLen));
  } catch {
    return null;
  }
  // 从尾部向前找 'moov' 四字符特征（box 头：size 在 type 前 4 字节）
  const dv = new DataView(tail.buffer, tail.byteOffset, tail.byteLength);
  for (let p = tail.length - 8; p >= 4; p--) {
    if (
      tail[p] === 0x6d && tail[p + 1] === 0x6f && tail[p + 2] === 0x6f && tail[p + 3] === 0x76
    ) {
      const size = dv.getUint32(p - 4);
      const boxPos = tailStart + p - 4;
      if (size >= 8 && size <= MAX_MOOV && boxPos + size === fileSize) {
        const data = await readRange(path, boxPos, size);
        if (data && data.byteLength === size) {
          return { offset: boxPos, size, data };
        }
      }
    }
  }
  return null;
}

/**
 * 解析 moov → 结构化轨道信息。
 * 返回 null 表示解析失败（调用方回退原生播放）。
 *
 * 注意：mp4box 的 getInfo() 会读 this.ftyp.major_brand，moov-only
 * 缓冲会因缺 ftyp 抛 TypeError —— 这里在 moov 前拼一个 24 字节的
 * 合成 ftyp（isom），既满足 getInfo，也不影响轨道表解析。
 */
export async function analyzeMp4(path) {
  const moov = await locateMoov(path);
  if (!moov) return null;

  return await new Promise((resolve) => {
    const mp4box = createFile(false); // 只解析，不保留 mdat 数据
    let settled = false;
    const done = (v) => {
      if (!settled) {
        settled = true;
        resolve(v);
      }
    };
    mp4box.onReady = (info) => {
      try {
        done(extractInfo(info));
      } catch {
        done(null);
      }
    };
    mp4box.onError = () => done(null);
    try {
      // 合成 ftyp（size 24, 'isom', minor 0x200, compatible isom/mp41）
      const FTYP = new Uint8Array([
        0, 0, 0, 24, 102, 116, 121, 112, 105, 115, 111, 109,
        0, 0, 2, 0, 105, 115, 111, 109, 109, 112, 52, 49
      ]);
      const combined = new Uint8Array(FTYP.length + moov.data.byteLength);
      combined.set(FTYP, 0);
      combined.set(new Uint8Array(moov.data), FTYP.length);
      const ab = combined.buffer;
      ab.fileStart = 0; // 独立喂入：ftyp + moov 连续，fileStart 从 0 计
      const next = mp4box.appendBuffer(ab);
      void next;
      mp4box.flush();
    } catch {
      done(null);
    }
    // 兜底：某些畸形 moov 可能既不 onReady 也不 onError
    setTimeout(() => done(null), 3000);
  });
}

/** mp4box info → 精简结构 */
function extractInfo(info) {
  if (!info || !info.tracks || !info.tracks.length) return null;
  const videoTracks = [];
  const audioTracks = [];
  const subtitleTracks = [];
  for (const t of info.tracks) {
    const base = {
      id: t.id,
      codec: t.codec || '',
      lang: t.language || 'und',
      name: t.name || ''
    };
    if (t.type === 'video' || t.video) {
      videoTracks.push({
        ...base,
        width: t.video?.width || t.track_width || 0,
        height: t.video?.height || t.track_height || 0,
        fps: estimateFps(t),
        nbSamples: t.nb_samples || 0
      });
    } else if (t.type === 'audio' || t.audio) {
      audioTracks.push({
        ...base,
        channels: t.audio?.channel_count || 0,
        sampleRate: t.audio?.sample_rate || 0,
        nbSamples: t.nb_samples || 0
      });
    } else if (t.type === 'subtitles' || /^(wvtt|tx3g|stpp|sbtt)/.test(t.codec || '')) {
      subtitleTracks.push({ ...base, nbSamples: t.nb_samples || 0 });
    }
  }
  const durationSec = (() => {
    const d = info.duration && info.timescale ? info.duration / info.timescale : 0;
    return isFinite(d) && d > 0 ? d : 0;
  })();
  return {
    durationSec,
    isFragmented: !!info.isFragmented,
    video: videoTracks[0] || null,
    videoTracks,
    audioTracks,
    subtitleTracks
  };
}

/** 视频轨帧率估算：nb_samples / 轨道时长（VBR 下是平均值，逐帧步进够用） */
function estimateFps(track) {
  const dur = track.samples_duration && track.timescale ? track.samples_duration / track.timescale : 0;
  if (!dur || !track.nb_samples) return 0;
  const fps = track.nb_samples / dur;
  return fps > 0 && isFinite(fps) ? fps : 0;
}
