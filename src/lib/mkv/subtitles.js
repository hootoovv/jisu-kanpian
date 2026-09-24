/**
 * 极速看片 —— Matroska 内嵌字幕提取（matroska-subtitles 驱动，流式）
 *
 * 流程：按需加载浏览器 bundle（loader.js）→ SubtitleParser 流式喂入
 * （filestream.js 的 readFileChunks：Rust read_range 按块吐字节，内存
 * 占用恒定，与文件大小无关——v1.0.3 的整文件内存 + 4GB 预检已废除）
 * → 收集目标轨道的全部字幕块 → 按轨道类型转换：
 * - utf8 / webvtt：块 text 即最终文本（SRT 行内标签 <i> 等保留，与
 *   WebVTT 语法兼容）；
 * - ass / ssa：块 text 只是 Dialogue 最后一个字段，重组完整 ASS 文
 *   档（assDocFromBlocks）→ ass-compiler 解析 → 富文本 cue；
 * - pgs / vobsub / kate / unknown：图形或未适配格式 → 抛错（App 提示）。
 *
 * 大文件取舍：提取耗时只受磁盘顺序读速度限制（5GB ≈ SSD 数秒 / HDD
 * 数十秒），调用方经 onProgress 展示百分比；shouldAbort 返回 true 时
 * 立即中止并返回 null（静默取消，丢弃已收集的块）。
 */
import { loadMatroskaSubtitles } from './loader.js';
import { assDocFromBlocks, assToCues } from './ass.js';
import { readFileChunks } from '../filestream.js';

/**
 * 喂入间隙看门狗：单块「IPC 读取 + 解析」超过该时长视为卡死
 * （磁盘掉线 / IPC 挂起），整体中止。远大于正常单块耗时（<1s）。
 */
const WATCHDOG_MS = 30_000;
/** parser.end() 之后等待 finish 事件的收尾时限 */
const FINISH_TIMEOUT_MS = 15_000;

/**
 * 流式提取一条 MKV 内嵌字幕轨。
 * @param {string} path 视频文件绝对路径
 * @param {number} fileSize 文件总大小（stat_file 结果，进度分母）
 * @param {number} trackNumber 目标轨道的 TrackNumber
 * @param {object} track 轨道描述（tracks.js 的 subtitleTracks 项）
 * @param {object} [opt]
 * @param {(bytes:number,total:number)=>void} [opt.onProgress] 进度回调（每块一次）
 * @param {()=>boolean} [opt.shouldAbort] 返回 true 则静默取消（resolve null）
 * @param {number} [opt.chunkBytes] 单块大小（默认 4MB；冒烟测试用小值
 *   验证跨块边界的解析正确性）
 * @returns {Promise<{start:number,end:number,text:string}[]|null>}
 *   null = 被调用方取消（不算失败，不提示）
 */
export async function extractMkvSubtitles(path, fileSize, trackNumber, track, opt = {}) {
  const kind = track?.codec || 'utf8';
  if (['pgs', 'vobsub', 'kate', 'unknown'].includes(kind)) {
    throw new Error(kind === 'unknown' ? '未适配的字幕格式' : '图形字幕（PGS/VOBSub）暂不支持');
  }

  const MatroskaSubtitles = await loadMatroskaSubtitles();

  return await new Promise((resolve, reject) => {
    const parser = new MatroskaSubtitles.SubtitleParser();
    const blocks = [];
    let header = track?.header || '';
    let sawTarget = false;
    let settled = false;
    let watchdog = 0;
    let finishTimer = 0;

    const done = (ok, v) => {
      if (settled) return;
      settled = true;
      clearTimeout(watchdog);
      clearTimeout(finishTimer);
      try { parser.destroy?.(); } catch { /* 忽略 */ }
      ok ? resolve(v) : reject(v);
    };
    const armWatchdog = () => {
      clearTimeout(watchdog);
      watchdog = setTimeout(
        () => done(false, new Error('读取超时（磁盘或 IPC 无响应）')),
        WATCHDOG_MS
      );
    };

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
    parser.on('error', (e) => {
      done(false, new Error(`MKV 解析出错：${e && e.message ? e.message : e}`));
    });
    parser.on('finish', () => {
      if (!sawTarget) {
        done(false, new Error('该字幕轨没有可显示的内容'));
        return;
      }
      try {
        if (kind === 'ass') {
          // ASS：重组文档 → ass-compiler → 富文本 cue
          const cues = assToCues(assDocFromBlocks(header, blocks));
          done(cues.length > 0, cues.length > 0 ? cues : new Error('ASS 字幕解析失败'));
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
          done(cues.length > 0, cues.length > 0 ? cues : new Error('该字幕轨没有可显示的内容'));
        }
      } catch (e) {
        done(false, e instanceof Error ? e : new Error(String(e)));
      }
    });

    (async () => {
      try {
        let fed = 0;
        armWatchdog();
        // 冒烟测试钩子：压缩分块以覆盖「EBML 元素跨块边界」的解析路径
        // （正常传 undefined → readFileChunks 用默认 4MB）
        const chunkOverride =
          typeof window !== 'undefined' ? Number(window.__kpMkvChunkBytes) || 0 : 0;
        for await (const chunk of readFileChunks(
          path,
          Number(fileSize) || 0,
          opt.chunkBytes || (chunkOverride > 0 ? chunkOverride : undefined)
        )) {
          if (opt.shouldAbort && opt.shouldAbort()) {
            done(true, null); // 静默取消：丢弃已收集的块
            return;
          }
          // 同步解析一块；跨块的残缺 EBML 元素由 parser 内部缓冲拼接
          parser.write(chunk);
          fed += chunk.byteLength;
          if (opt.onProgress) {
            try { opt.onProgress(fed, Number(fileSize) || 0); } catch { /* 进度异常不致命 */ }
          }
          armWatchdog();
        }
        if (settled) return; // 期间已因取消 / 错误落定
        clearTimeout(watchdog);
        finishTimer = setTimeout(
          () => done(false, new Error('MKV 字幕提取超时')),
          FINISH_TIMEOUT_MS
        );
        parser.end();
      } catch (e) {
        done(false, e instanceof Error ? e : new Error(String(e)));
      }
    })();
  });
}
