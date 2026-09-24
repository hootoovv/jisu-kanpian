/**
 * 极速看片 —— Matroska 内嵌字幕提取（三级引擎调度）
 *
 * v1.0.5 起提取按三级降级，解决 5GB 级大文件「分钟级」耗时问题：
 * ① ffmpeg：Rust 后端解析出 ffmpeg 时用（跨平台探测，v1.0.6：
 *    Windows 找应用同目录与 PATH 下的 ffmpeg.exe，Unix 找 PATH 里的
 *    ffmpeg；逐候选 -version 验证，坏的应用执行别名自动跳过）——
 *    `ffmpeg -i <file> -map 0:s:<n> -c:s copy -f srt/ass/webvtt pipe:1`
 *    纯 demux + copy 直写 stdout，5GB 文件秒级完成，输出即完整字幕
 *    文档（本模块只做文档 → cue 的轻量转换）；
 * ② 原生 EBML：机器无 ffmpeg（或 ffmpeg 失败）时，后端手写流式
 *    EBML 游走顺序扫 Cluster，只把目标轨字幕块读进内存——解析在
 *    原生代码完成，速度只受磁盘顺序读限制（比「IPC 传全文件字节 +
 *    JS 单线程解析」快 1~2 个数量级）；
 * ③ JS 流式兜底（v1.0.4 路径）：后端两引擎都失败时的最终安全网
 *    ——matroska-subtitles 喂 filestream.js 分块，对罕见的 lacing
 *    分帧 / 容器损伤等后端不兼容形态仍可完成提取。
 * ①② 都在后端 extract_mkv_subtitles 命令里（src-tauri/src/mkvsub.rs），
 * 前端只感知「快路径成功」与「需要回退」两种结果。
 *
 * 降级判据：后端报「定性错误」（轨道不存在 / 不是字幕轨 / 格式未
 * 适配 / 无内容 / 空文件）时直接抛出——这些错误 JS 路径同样会撞，
 * 再扫一遍 5GB 只会白等；容器级错误（读取失败 / lacing / 形状异常）
 * 与命令缺失（旧后端 / mock）才值得花时间走 ③。
 *
 * 进度：后端把进度原子量暴露给 query_extract_progress（native 按文件
 * 偏移出百分比；ffmpeg 拿不到内部进度，展示引擎名），本模块在
 * invoke 挂起期间轮询并转发；③ 路径沿用自己的分块进度。
 */
import { invoke } from '@tauri-apps/api/core';
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
/** 后端进度轮询间隔（invoke 挂起期间；首个 tick 立即触发） */
const PROGRESS_POLL_MS = 300;

/**
 * 定性错误：JS 兜底路径必然撞同样的墙，直接抛给用户（不白扫全文件）。
 * 与 src-tauri/src/mkvsub.rs 的错误文案保持同步。
 */
const DEFINITE_FAIL = [
  '找不到指定的字幕轨',
  '不是字幕轨',
  '未适配',
  '没有可显示的内容',
  '文件为空',
  'PGS',
  'VOBSub'
];

function isDefinitiveFail(message) {
  return DEFINITE_FAIL.some((p) => String(message || '').includes(p));
}

/* ---------------- 时间戳 / 字幕文档解析（① 引擎输出转换） ---------------- */

/** "HH:MM:SS,mmm" / "MM:SS.mmm"（VTT 允许省略时）→ 秒；解析失败 NaN */
function parseStamp(token) {
  const m = String(token || '')
    .trim()
    .replace(',', '.')
    .match(/^(?:(\d+):)?(\d{1,2}):(\d{1,2}(?:\.\d{1,3})?)$/);
  if (!m) return NaN;
  return Number(m[1] || 0) * 3600 + Number(m[2]) * 60 + Number(m[3]);
}

/**
 * 完整 SRT 文档 → cue 列表（ffmpeg 引擎 kind=utf8 的输出即 SRT 全文）。
 * 块间空行分隔；块内 = [索引行]、时间轴行 "… --> …"、正文行…。
 */
function srtDocToCues(text) {
  return docBlocksToCues(text, { endSettings: false });
}

/**
 * 完整 WebVTT 文档 → cue 列表（ffmpeg 引擎 kind=webvtt 的输出）。
 * 与 SRT 的差异：允许 "MM:SS.mmm" 短时间戳；时间轴行尾可带
 * align/line 等排版设置（丢弃，WebVTT cue 级设置本模块不透传）。
 */
function vttDocToCues(text) {
  return docBlocksToCues(text, { endSettings: true });
}

function docBlocksToCues(text, { endSettings }) {
  const cues = [];
  const blocks = String(text || '')
    .replace(/\r+\n/g, '\n')
    .replace(/^\uFEFF/, '') // BOM
    .split(/\n{2,}/);
  for (const block of blocks) {
    const lines = block.trim().split('\n');
    const ti = lines.findIndex((l) => l.includes('-->'));
    if (ti < 0) continue; // WEBVTT 头 / NOTE / STYLE / 索引行…非 cue 块
    const [startTok, endPart] = lines[ti].split('-->');
    const endTok = endSettings ? String(endPart || '').trim().split(/\s+/)[0] : endPart;
    const start = parseStamp(startTok);
    const end = parseStamp(endTok);
    if (!isFinite(start) || !isFinite(end)) continue;
    const body = lines
      .slice(ti + 1)
      .join('\n')
      .replace(/\n{3,}/g, '\n\n')
      .trim();
    if (body && end > start) cues.push({ start, end, text: body });
  }
  return cues;
}

/* ---------------- 后端 ExtractResult → cue 列表 ---------------- */

/** native 引擎块（Rust SubBlock，camelCase）→ cue（utf8 / webvtt 轨） */
function plainBlocksToCues(blocks) {
  return blocks
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
}

/**
 * 后端（① ffmpeg / ② native）的 ExtractResult → cue 列表。
 * 字段对齐见 src-tauri/src/mkvsub.rs 的 ExtractResult / SubBlock。
 * 转换失败（空内容 / ASS 解析失败）抛错——由调用方决定是否降级 ③。
 */
function backendResultToCues(res) {
  if (!res || !res.method) throw new Error('后端返回格式异常');
  if (res.method === 'ffmpeg') {
    // ffmpeg 输出即完整字幕文档：ass 含 CodecPrivate 头 + 全部 Dialogue
    const text = String(res.text || '').trim();
    if (!text) throw new Error('后端未返回字幕内容');
    let cues;
    if (res.kind === 'ass') {
      cues = assToCues(text);
    } else if (res.kind === 'webvtt') {
      cues = vttDocToCues(text);
    } else {
      cues = srtDocToCues(text);
    }
    if (!cues.length) throw new Error('该字幕轨没有可显示的内容');
    return cues;
  }
  // native：ASS 头（CodecPrivate）+ 字幕块（字段与 matroska-subtitles 对齐）
  const blocks = Array.isArray(res.blocks) ? res.blocks : [];
  if (!blocks.length) throw new Error('该字幕轨没有可显示的内容');
  let cues;
  if (res.kind === 'ass') {
    cues = assToCues(assDocFromBlocks(res.header || '', blocks));
  } else {
    cues = plainBlocksToCues(blocks);
  }
  if (!cues.length) throw new Error('该字幕轨没有可显示的内容');
  return cues;
}

/* ---------------- 后端进度轮询（invoke 挂起期间） ---------------- */

/**
 * 轮询 query_extract_progress 并转发 opt.onProgress(bytes, total, method)。
 * 返回停止函数；首个 tick 立即触发（快速任务也至少上报一次引擎名）。
 */
function pollBackendProgress(onProgress, shouldAbort) {
  let stopped = false;
  const tick = () => {
    if (stopped) return;
    if (shouldAbort && shouldAbort()) return; // 结果将被调用方丢弃，不再刷状态
    invoke('query_extract_progress')
      .then((s) => {
        if (stopped || !s || !s.running) return;
        onProgress(Number(s.bytes) || 0, Number(s.total) || 0, s.method);
      })
      .catch(() => { /* 轮询失败不影响提取本体 */ });
  };
  tick();
  const h = setInterval(tick, PROGRESS_POLL_MS);
  return () => {
    stopped = true;
    clearInterval(h);
  };
}

/* ---------------- 对外入口：三级调度 ---------------- */

/**
 * 提取一条 MKV 内嵌字幕轨（三级引擎，见模块注释）。
 * @param {string} path 视频文件绝对路径
 * @param {number} fileSize 文件总大小（stat_file 结果，③ 路径进度分母）
 * @param {number} trackNumber 目标轨道的 TrackNumber
 * @param {object} track 轨道描述（tracks.js 的 subtitleTracks 项）
 * @param {object} [opt]
 * @param {(bytes:number,total:number,method?:string)=>void} [opt.onProgress]
 *   进度回调；①② 路径带第三参 method（'ffmpeg'|'native'，ffmpeg 无
 *   内部进度，调用方应展示引擎名而非百分比），③ 路径两参（百分比）
 * @param {()=>boolean} [opt.shouldAbort] 返回 true 则静默取消（resolve null；
 *   仅 ③ 路径可中断，①② 由调用方按代次丢弃结果）
 * @param {number} [opt.chunkBytes] ③ 路径单块大小（默认 4MB；冒烟测试
 *   用小值验证跨块边界的解析正确性）
 * @returns {Promise<{start:number,end:number,text:string}[]|null>}
 *   null = 被调用方取消（不算失败，不提示）
 */
export async function extractMkvSubtitles(path, fileSize, trackNumber, track, opt = {}) {
  const kind = track?.codec || 'utf8';
  if (['pgs', 'vobsub', 'kate', 'unknown'].includes(kind)) {
    throw new Error(kind === 'unknown' ? '未适配的字幕格式' : '图形字幕（PGS/VOBSub）暂不支持');
  }

  /* ①② 后端双引擎（ffmpeg → 原生 EBML，见 src-tauri/src/mkvsub.rs） */
  let backendErr = null;
  try {
    const stopPoll = opt.onProgress ? pollBackendProgress(opt.onProgress, opt.shouldAbort) : null;
    let res;
    try {
      res = await invoke('extract_mkv_subtitles', { path, trackNumber });
    } finally {
      if (stopPoll) stopPoll();
    }
    const cues = backendResultToCues(res);
    if (typeof window !== 'undefined') window.__kpExtractMethod = res.method; // 测试钩子
    return cues;
  } catch (e) {
    backendErr = e instanceof Error ? e.message : String(e);
    if (isDefinitiveFail(backendErr)) throw e; // 定性失败：JS 路径必撞同墙，不白扫
    // 容器级失败 / 旧后端无此命令 / 输出形状异常 → ③ 兜底
    if (typeof window !== 'undefined') window.__kpExtractFallbackReason = backendErr;
  }

  /* ③ JS 流式兜底（v1.0.4 路径：matroska-subtitles） */
  const cues = await extractViaJs(path, fileSize, trackNumber, track, opt);
  if (cues && typeof window !== 'undefined') window.__kpExtractMethod = 'js'; // 测试钩子
  return cues;
}

/**
 * ③ JS 流式兜底：按块读文件喂 matroska-subtitles（内存占用恒定，
 * 与文件大小无关），收集目标轨全部字幕块后按轨道类型转换。
 * 跨块的残缺 EBML 元素由 parser 内部缓冲拼接。
 */
async function extractViaJs(path, fileSize, trackNumber, track, opt = {}) {
  const kind = track?.codec || 'utf8';

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
          const cues = plainBlocksToCues(blocks);
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
