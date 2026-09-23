/**
 * 极速看片 —— 字幕管理（统一走 WebVTT）
 *
 * 三类字幕源，最终都变成 <track> 元素挂到 <video> 上：
 * 1. 外挂 .vtt：直接 <track src=asset协议URL>；
 * 2. 外挂 .srt：fetch 后转成 WebVTT 文本（时间戳逗号→点），
 *    blob URL 再挂 <track>；
 * 3. MP4 内嵌字幕（wvtt / tx3g）：mp4box 抽样（setExtractionOptions
 *    + onSamples），用自带的 VTTin4Parser / TX3GParser 解出纯文本，
 *    生成 WebVTT 文本后同样走 blob URL。
 *
 * hls.js 的字幕轨由 hls 实例自管（subtitleTracks API），这里不经过
 * SubtitleManager —— App 层按 kind 分流。
 */
import { createFile, VTTin4Parser, TX3GParser } from 'mp4box';
import { srtToVtt } from './media.js';

/** 秒 → VTT 时间戳 "HH:MM:SS.mmm" */
function vttStamp(sec) {
  if (!isFinite(sec) || sec < 0) sec = 0;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = Math.floor(sec % 60);
  const ms = Math.round((sec - Math.floor(sec)) * 1000);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${p(h)}:${p(m)}:${p(s)}.${p(ms, 3)}`;
}

/** cue 列表 → WebVTT 全文（内嵌字幕提取后生成 blob 用） */
export function cuesToVtt(cues) {
  const body = cues
    .filter((c) => c && c.text && c.text.trim() && c.end > c.start)
    .map((c) => `${vttStamp(c.start)} --> ${vttStamp(c.end)}\n${c.text.trim()}`)
    .join('\n\n');
  return `WEBVTT\n\n${body}\n`;
}

/**
 * 从 MP4 文件缓冲中提取一条字幕轨的 cue 列表。
 * @param {ArrayBuffer} arrayBuffer 整个 MP4 文件（复用 MSE 引擎的缓冲，
 *   避免二次读盘；原生路径下由调用方另行 fetch）
 * @param {number} trackId 字幕轨 id
 * @param {string} codec 'wvtt' | 'tx3g'（其他格式尽力按 tx3g 文本解析）
 * @returns {Promise<{start:number,end:number,text:string}[]>}
 */
export function extractEmbeddedSubtitles(arrayBuffer, trackId, codec) {
  return new Promise((resolve, reject) => {
    let settled = false;
    const done = (ok, v) => {
      if (!settled) {
        settled = true;
        ok ? resolve(v) : reject(v);
      }
    };
    const mp4box = createFile(true);
    const cues = [];
    const isWvtt = String(codec || '').startsWith('wvtt');
    const vttParser = new VTTin4Parser();
    const tx3gParser = new TX3GParser();

    mp4box.onReady = () => {
      try {
        mp4box.setExtractionOptions(trackId, null, { nbSamples: 500 });
        // 开闸：processSamples 受 sampleProcessingStarted 保护，
        // 不 start() 则 onSamples 永远不会触发（mp4box README 同款模式）
        mp4box.start();
      } catch (e) {
        done(false, e instanceof Error ? e : new Error(String(e)));
      }
    };
    mp4box.onSamples = (id, user, samples) => {
      for (const s of samples) {
        try {
          const start = (s.cts || 0) / (s.timescale || 1000);
          const end = ((s.cts || 0) + (s.duration || 0)) / (s.timescale || 1000);
          let text = '';
          if (isWvtt) {
            text = vttParser.getText(start, end, s.data);
          } else {
            text = tx3gParser.parseSample(s);
          }
          text = String(text || '').replace(/\r/g, '');
          if (text.trim()) cues.push({ start, end, text });
        } catch {
          /* 单条解析失败跳过 */
        }
      }
    };
    mp4box.onError = (e) => done(false, new Error(String(e)));
    try {
      arrayBuffer.fileStart = 0;
      mp4box.appendBuffer(arrayBuffer);
      mp4box.flush();
      done(true, cues);
    } catch (e) {
      done(false, e instanceof Error ? e : new Error(String(e)));
    }
    setTimeout(() => done(false, new Error('字幕提取超时')), 8000);
  });
}

/**
 * 外挂 .srt → VTT 文本（fetch + 转换）
 * @param {string} url asset 协议 URL
 */
export async function loadSrtAsVtt(url) {
  const resp = await fetch(url);
  if (!resp.ok) throw new Error(`读取字幕失败：${resp.status}`);
  return srtToVtt(await resp.text());
}

/**
 * 字幕控制器：管理挂在 <video> 上的动态 <track>。
 * 同一时刻只显示一条；切换即重建（简单可靠，v1 不做复用缓存）。
 */
export class SubtitleManager {
  /** @param {HTMLVideoElement} videoEl */
  constructor(videoEl) {
    this.videoEl = videoEl;
    this.trackEl = null;
    this.objectUrl = null;
    this.currentLabel = '';
  }

  /**
   * 挂载并显示一条字幕。
   * @param {object} opt
   * @param {string} opt.src 直接的 .vtt URL（外挂 vtt）
   * @param {string} opt.vttText VTT 全文（外挂 srt 转换 / 内嵌提取）
   * @param {string} opt.lang 语言代码（srclang）
   * @param {string} opt.label 展示名
   */
  async attach({ src, vttText, lang, label }) {
    this.detach();
    const el = document.createElement('track');
    el.kind = 'subtitles';
    el.label = label || '字幕';
    el.srclang = lang || 'zh';
    if (vttText) {
      this.objectUrl = URL.createObjectURL(new Blob([vttText], { type: 'text/vtt' }));
      el.src = this.objectUrl;
    } else {
      el.src = src;
    }
    this.trackEl = el;
    this.currentLabel = el.label;
    this.videoEl.appendChild(el);
    // 等 track 数据就绪再切 showing，避免偶发不渲染
    el.addEventListener(
      'load',
      () => {
        if (this.trackEl === el && el.track) el.track.mode = 'showing';
      },
      { once: true }
    );
    if (el.track) {
      try {
        el.track.mode = 'showing';
      } catch { /* 事件路径兜底 */ }
    }
  }

  /** 卸载当前字幕 */
  detach() {
    if (this.trackEl) {
      try {
        if (this.trackEl.track) this.trackEl.track.mode = 'disabled';
        this.trackEl.remove();
      } catch { /* 忽略 */ }
      this.trackEl = null;
    }
    if (this.objectUrl) {
      URL.revokeObjectURL(this.objectUrl);
      this.objectUrl = null;
    }
    this.currentLabel = '';
  }
}
