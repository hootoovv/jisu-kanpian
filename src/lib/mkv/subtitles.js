/**
 * 极速看片 —— Matroska 内嵌字幕提取（索引会话 + 全量兜底）
 *
 * v1.0.7 三级降级（删除 ffmpeg 引擎——实测被索引跳跃全面超越：
 * 9.9GB 蓝光重封装在 5400rpm HDD 上 ffmpeg 全量 demux ~183s，索引
 * 跳跃首窗亚秒、全片冷 ~44s / 热 ~0.5s）：
 * ① 索引会话（Cues 跳跃）：subtitle_session_open 解析容器 Cues
 *    索引（几百 KB、亚秒级），目标轨位置表常驻后端内存；之后按
 *    播放窗口 subtitle_window 直跳取块——只碰窗口内字幕块的几十
 *    KB 字节，不顺序扫全文件。首屏字幕亚秒级可用，播放中每 ~90s
 *    补一窗，seek 即查即得（本模块的 MkvSessionSubtitlePlayer）。
 * ② 原生全量走读：无 Cues 索引（罕见封装，如 mkvmerge --no-cues）
 *    或会话路径失败时，后端一次性全文件顺序扫（extract_mkv_subtitles
 *    命令，进度按文件偏移报百分比）——本模块的 extractMkvSubtitles。
 * ③ JS 流式兜底（v1.0.4 路径）：② 也失败（罕见的 lacing 分帧 /
 *    容器损伤）时的最终安全网——matroska-subtitles 喂 filestream.js
 *    分块，内存恒定。
 *
 * 降级判据：后端报「定性错误」（轨道不存在 / 不是字幕轨 / 格式未
 * 适配 / 无内容 / 空文件）时直接抛出——这些错误低级路径同样会撞，
 * 再扫一遍 5GB 只会白等；「没有可用的 Cues 索引」与容器级错误
 * （读取失败 / lacing / 形状异常）才值得降级；命令缺失（旧后端 /
 * mock）→ 会话路径返回 null，调用方直接走 ②③。
 *
 * 进度：仅 ② 需要等待（分钟级）——后端把进度原子量暴露给
 * query_extract_progress（按文件偏移出百分比），本模块在 invoke
 * 挂起期间轮询并转发；① 每窗毫秒级无需进度；③ 沿用自己的分块进度。
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

/* ---------------- 索引会话（① Cues 跳跃的窗口参数） ---------------- */

/** 单窗时长：一次直跳预取的对白时间跨度 */
const WINDOW_SPAN_MS = 90_000;
/** 起窗后向边距：覆盖「seek 落在长字幕事件中间」的场景（实测蓝光
 *  重封装存在 10s~25s 的事件，30s 起步保守够用） */
const WINDOW_BACK_MS = 30_000;
/** 播放头距已覆盖末端不足该值时预取下一窗 */
const WINDOW_AHEAD_MS = 30_000;

/**
 * 定性错误：低级路径必然撞同样的墙，直接抛给用户（不白扫全文件）。
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

/* ---------------- 时间戳 / 字幕文档解析（②③ 输出转换） ---------------- */

/** "HH:MM:SS,mmm" / "MM:SS.mmm"（VTT 允许省略时）→ 秒；解析失败 NaN */
function parseStamp(token) {
  const m = String(token || '')
    .trim()
    .replace(',', '.')
    .match(/^(?:(\d+):)?(\d{1,2}):(\d{1,2}(?:\.\d{1,3})?)$/);
  if (!m) return NaN;
  return Number(m[1] || 0) * 3600 + Number(m[2]) * 60 + Number(m[3]);
}

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

/** 窗口/全量的字幕块 → cue 列表（ASS：头 + 块重组 → ass-compiler） */
function blocksToCues(blocks, kind, header) {
  if (kind === 'ass') {
    return assToCues(assDocFromBlocks(header || '', blocks));
  }
  return plainBlocksToCues(blocks);
}

/**
 * 后端 ExtractResult（② 全量）→ cue 列表。
 * 字段对齐见 src-tauri/src/mkvsub.rs 的 ExtractResult / SubBlock。
 * 转换失败（空内容 / ASS 解析失败）抛错——由调用方决定是否降级 ③。
 */
function backendResultToCues(res) {
  if (!res || !res.method) throw new Error('后端返回格式异常');
  const blocks = Array.isArray(res.blocks) ? res.blocks : [];
  if (!blocks.length) throw new Error('该字幕轨没有可显示的内容');
  const cues = blocksToCues(blocks, res.kind, res.header || '');
  if (!cues.length) throw new Error('该字幕轨没有可显示的内容');
  return cues;
}

/* ---------------- ① 索引会话：打开 / 窗口取块 ---------------- */

/**
 * 打开 MKV 字幕索引会话（引擎 ①）。
 * @returns {Promise<object|null>} 成功 → 会话对象（window/close 方法）；
 *   无索引 / 容器级错误 / 命令缺失（旧后端 / mock）→ null（调用方走
 *   全量降级）；定性错误（轨道 / 格式）→ throw（与 ② 同套文案）
 */
export async function openMkvSubtitleSession(path, trackNumber, track) {
  const kind = track?.codec || 'utf8';
  if (['pgs', 'vobsub', 'kate', 'unknown'].includes(kind)) {
    throw new Error(kind === 'unknown' ? '未适配的字幕格式' : '图形字幕（PGS/VOBSub）暂不支持');
  }
  try {
    const s = await invoke('subtitle_session_open', { path, trackNumber });
    if (!s || !s.sessionId) throw new Error('会话返回格式异常');
    const header = s.header || '';
    return {
      id: Number(s.sessionId),
      kind: s.kind || kind,
      header,
      totalBlocks: Number(s.totalBlocks) || 0,
      /**
       * 取一个播放窗口的 cue 列表 [fromMs, toMs)。
       * skipped > 0（索引条目未命中）抛错——由播放器触发全量降级，
       * 不静默丢字幕。
       */
      async window(fromMs, toMs) {
        const r = await invoke('subtitle_window', {
          sessionId: s.sessionId,
          fromMs: Math.max(0, Math.floor(fromMs)),
          toMs: Math.max(0, Math.ceil(toMs))
        });
        if (!r || !Array.isArray(r.blocks)) throw new Error('窗口返回格式异常');
        if (Number(r.skipped) > 0) throw new Error('索引跳跃不完整');
        return blocksToCues(r.blocks, s.kind || kind, header);
      },
      close() {
        invoke('subtitle_close', { sessionId: s.sessionId }).catch(() => { /* 关闭失败不致命 */ });
      }
    };
  } catch (e) {
    const msg = e && e.message ? e.message : String(e);
    if (isDefinitiveFail(msg)) throw e;
    return null; // 无索引 / 容器级问题 / 命令缺失 → 全量降级
  }
}

/* ---------------- ① 索引会话：窗口播放器（边播边取） ---------------- */

function cueKey(c) {
  return `${c.start}\u0000${c.end}\u0000${c.text}`;
}

/**
 * 索引会话播放器：首窗挂 <track>，播放中按需补窗（addCue 追加），
 * seek 落到未覆盖区间即查即得；cue 以 (start,end,text) 去重。
 * 任一窗口失败（含 skipped>0 / 会话失效）→ onFail 一次性回调，
 * 由 App 层降级全量提取。
 */
export class MkvSessionSubtitlePlayer {
  /**
   * @param {object} session openMkvSubtitleSession 的返回
   * @param {HTMLVideoElement} videoEl
   * @param {import('../subtitles.js').SubtitleManager} manager
   * @param {object} opt { lang, label, cuesToVtt, onFail }
   */
  constructor(session, videoEl, manager, opt = {}) {
    this.session = session;
    this.videoEl = videoEl;
    this.manager = manager;
    this.lang = opt.lang || 'zh';
    this.label = opt.label || '字幕';
    this.cuesToVtt = opt.cuesToVtt;
    this.onFail = opt.onFail || null;
    this.coverFrom = Infinity; // 已覆盖区间 [coverFrom, coverUntil)
    this.coverUntil = -Infinity;
    this.attached = false;
    this.seen = new Set();
    this.busy = false;
    this.failed = false;
    this.destroyed = false;
  }

  /** 挂载首窗（当前播放位置起，含后向边距；对白未开始也可为空窗） */
  async start() {
    const t = (this.videoEl.currentTime || 0) * 1000;
    try {
      const from = Math.max(0, t - WINDOW_BACK_MS);
      const cues = await this.session.window(from, t + WINDOW_SPAN_MS);
      if (this.destroyed) return;
      this.coverFrom = from;
      this.coverUntil = t + WINDOW_SPAN_MS;
      const fresh = this.remember(cues);
      await this.manager.attach({
        vttText: this.cuesToVtt(fresh),
        lang: this.lang,
        label: this.label
      });
      this.attached = true;
    } catch (e) {
      this.fail(e);
    }
  }

  /** timeupdate 驱动：播放头逼近已覆盖末端时预取下一窗并追加 */
  async tick() {
    if (this.destroyed || this.failed || !this.attached || this.busy) return;
    const t = (this.videoEl.currentTime || 0) * 1000;
    if (t + WINDOW_AHEAD_MS <= this.coverUntil) return;
    this.busy = true;
    try {
      // 从已覆盖末端续到 min(播放头+SPAN, 末端+SPAN)：区间连续无重叠
      const to = Math.min(t + WINDOW_SPAN_MS, this.coverUntil + WINDOW_SPAN_MS);
      const cues = await this.session.window(this.coverUntil, to);
      if (this.destroyed) return;
      this.coverUntil = to;
      const fresh = this.remember(cues);
      if (fresh.length) this.manager.appendCues(fresh);
    } catch (e) {
      this.fail(e);
    } finally {
      this.busy = false;
    }
  }

  /** seek 驱动：落到已覆盖区间之外（前进越过末端 / 倒退到始端前）
   *  即查新窗追加（去重防重复挂载） */
  async onSeek() {
    if (this.destroyed || this.failed || !this.attached || this.busy) return;
    const t = (this.videoEl.currentTime || 0) * 1000;
    if (t >= this.coverFrom && t < this.coverUntil) return;
    this.busy = true;
    try {
      const from = Math.max(0, t - WINDOW_BACK_MS);
      const to = t + WINDOW_SPAN_MS;
      const cues = await this.session.window(from, to);
      if (this.destroyed) return;
      this.coverFrom = Math.min(this.coverFrom, from);
      this.coverUntil = Math.max(this.coverUntil, to);
      const fresh = this.remember(cues);
      if (fresh.length) this.manager.appendCues(fresh);
    } catch (e) {
      this.fail(e);
    } finally {
      this.busy = false;
    }
  }

  remember(cues) {
    const fresh = [];
    for (const c of cues || []) {
      const k = cueKey(c);
      if (!this.seen.has(k)) {
        this.seen.add(k);
        fresh.push(c);
      }
    }
    return fresh;
  }

  fail(e) {
    if (this.failed || this.destroyed) return;
    this.failed = true;
    try {
      if (this.onFail) this.onFail(e);
    } catch { /* 回调异常不再传播 */ }
  }

  destroy() {
    this.destroyed = true;
    try {
      this.session.close();
    } catch { /* 忽略 */ }
  }
}

/* ---------------- 后端进度轮询（② invoke 挂起期间） ---------------- */

/**
 * 轮询 query_extract_progress 并转发 opt.onProgress(bytes, total)。
 * 返回停止函数；首个 tick 立即触发（快速任务也至少上报一次）。
 */
function pollBackendProgress(onProgress, shouldAbort) {
  let stopped = false;
  const tick = () => {
    if (stopped) return;
    if (shouldAbort && shouldAbort()) return; // 结果将被调用方丢弃，不再刷状态
    invoke('query_extract_progress')
      .then((s) => {
        if (stopped || !s || !s.running) return;
        onProgress(Number(s.bytes) || 0, Number(s.total) || 0);
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

/* ---------------- 对外入口：全量降级（② → ③） ---------------- */

/**
 * 全量提取一条 MKV 内嵌字幕轨（引擎 ②③：无索引文件的兜底路径）。
 * @param {string} path 视频文件绝对路径
 * @param {number} fileSize 文件总大小（stat_file 结果，③ 路径进度分母）
 * @param {number} trackNumber 目标轨道的 TrackNumber
 * @param {object} track 轨道描述（tracks.js 的 subtitleTracks 项）
 * @param {object} [opt]
 * @param {(bytes:number,total:number)=>void} [opt.onProgress]
 *   ② 路径进度回调（按文件偏移百分比）；③ 路径两参（百分比）
 * @param {()=>boolean} [opt.shouldAbort] 返回 true 则静默取消（resolve null；
 *   仅 ③ 路径可中断，② 由调用方按代次丢弃结果）
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

  /* ② 后端原生全量走读（src-tauri/src/mkvsub.rs） */
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
    if (isDefinitiveFail(backendErr)) throw e; // 定性失败：低级路径必撞同墙，不白扫
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
        const cues = blocksToCues(blocks, kind, header);
        done(cues.length > 0, cues.length > 0 ? cues : new Error('该字幕轨没有可显示的内容'));
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
