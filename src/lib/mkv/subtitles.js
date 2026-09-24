/**
 * 极速看片 —— Matroska 内嵌字幕提取（matroska-subtitles 驱动）
 *
 * 流程：按需加载浏览器 bundle（loader.js）→ SubtitleParser 流式喂入
 * 整个文件（fetch asset URL，64KB 分块模拟流）→ 收集目标轨道的全部
 * 字幕块 → 按轨道类型转换：
 * - utf8 / webvtt：块 text 即最终文本（SRT 行内标签 <i> 等保留，与
 *   WebVTT 语法兼容）；
 * - ass / ssa：块 text 只是 Dialogue 最后一个字段，重组完整 ASS 文
 *   档（assDocFromBlocks）→ ass-compiler 解析 → 富文本 cue；
 * - pgs / vobsub / kate / unknown：图形或未适配格式 → 抛错（App 提示）。
 *
 * 与 MP4 内嵌字幕相同的取舍：提取需要把整个文件读进内存（复用
 * MSE 引擎缓冲或单独 fetch），超大文件由调用方按 stat_file 预检。
 */
import { loadMatroskaSubtitles } from './loader.js';
import { assDocFromBlocks, assToCues } from './ass.js';

/** 单块喂入大小（模拟流式，覆盖块跨分块边界的场景） */
const CHUNK = 64 * 1024;
/** 解析保险丝：喂完后等待 finish 的最长时间 */
const FINISH_TIMEOUT = 12000;
/** 文件大小上限：超过则拒绝提取（内存保护，与 MP4 原生路径一致） */
export const MAX_EXTRACT_BYTES = 4 * 1024 * 1024 * 1024;

/**
 * 提取一条 MKV 内嵌字幕轨。
 * @param {ArrayBuffer|Uint8Array} buffer 整个文件字节
 * @param {number} trackNumber 目标轨道的 TrackNumber
 * @param {object} track 轨道描述（tracks.js 的 subtitleTracks 项）
 * @returns {Promise<{start:number,end:number,text:string}[]>}
 */
export async function extractMkvSubtitles(buffer, trackNumber, track) {
  const kind = track?.codec || 'utf8';
  if (['pgs', 'vobsub', 'kate', 'unknown'].includes(kind)) {
    throw new Error(kind === 'unknown' ? '未适配的字幕格式' : '图形字幕（PGS/VOBSub）暂不支持');
  }

  const MatroskaSubtitles = await loadMatroskaSubtitles();
  const u8 = buffer instanceof Uint8Array ? buffer : new Uint8Array(buffer);

  const parser = new MatroskaSubtitles.SubtitleParser();
  const blocks = [];
  let header = track?.header || '';
  let sawTarget = false;

  return await new Promise((resolve, reject) => {
    let settled = false;
    const done = (ok, v) => {
      if (settled) return;
      settled = true;
      try { parser.destroy?.(); } catch { /* 忽略 */ }
      ok ? resolve(v) : reject(v);
    };
    const timer = setTimeout(() => done(false, new Error('MKV 字幕提取超时')), FINISH_TIMEOUT);

    parser.on('tracks', (tracks) => {
      // 拿官方 parser 的 CodecPrivate（ASS 头）——手写分析器刻意不读它
      const t = (tracks || []).find((x) => x.number === trackNumber);
      if (t && t.header) header = t.header;
    });
    parser.on('subtitle', (sub, n) => {
      if (n !== trackNumber) return;
      sawTarget = true;
      blocks.push(sub);
    });
    parser.on('finish', () => {
      clearTimeout(timer);
      if (!sawTarget) {
        done(false, new Error('该字幕轨没有可显示的内容'));
        return;
      }
      try {
        if (kind === 'ass') {
          // ASS：重组文档 → ass-compiler → 富文本 cue
          const cues = assToCues(assDocFromBlocks(header, blocks));
          if (!cues.length) {
            done(false, new Error('ASS 字幕解析失败'));
            return;
          }
          done(true, cues);
        } else {
          // SRT / WebVTT：块文本即内容
          const cues = blocks
            .filter((b) => b && String(b.text || '').trim())
            .map((b) => {
              const start = (b.time || 0) / 1000;
              return {
                start,
                end: start + (b.duration || 0) / 1000,
                text: String(b.text).replace(/\r/g, '').trim()
              };
            })
            .filter((c) => c.end > c.start);
          if (!cues.length) {
            done(false, new Error('该字幕轨没有可显示的内容'));
            return;
          }
          done(true, cues);
        }
      } catch (e) {
        done(false, e instanceof Error ? e : new Error(String(e)));
      }
    });
    parser.on('error', (e) => {
      clearTimeout(timer);
      done(false, new Error(`MKV 解析出错：${e && e.message ? e.message : e}`));
    });

    try {
      for (let off = 0; off < u8.length; off += CHUNK) {
        parser.write(u8.subarray(off, Math.min(off + CHUNK, u8.length)));
      }
      parser.end();
    } catch (e) {
      clearTimeout(timer);
      done(false, e instanceof Error ? e : new Error(String(e)));
    }
  });
}
