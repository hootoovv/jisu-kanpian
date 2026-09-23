/**
 * 极速看片 —— MSE 多音轨引擎（mp4box.js 整文件内存分段）
 *
 * 为什么需要它：Chromium 系 WebView 的 <video> 元素没有 audioTracks
 * 选择能力，多音轨 MP4 只能用 MSE 自己喂。思路与 mp4box.js 官方
 * 「on-the-fly fragmentation」演示一致：
 *
 * 1. 整个 MP4 一次性读进内存，喂给 createFile(true)（保留 mdat 数据，
 *    之后任意 getSample 都能取到字节）；
 * 2. 给「视频轨 + 当前音轨」各开一个 SourceBuffer，用
 *    setSegmentOptions 注册后 initializeSegmentation('per-track') 生成
 *    每条轨道自己的 init segment；
 * 3. start() 之后 mp4box 把样表重切成 moof/trun 媒体段回调 onSegment，
 *    顺序 append 进各自的 SourceBuffer（MSE 要求等 updateend 才能
 *    追加，队列化处理）；
 * 4. **节流生成**：前瞻缓冲超过 ~24s 就 stop()，播到只剩 ~12s 再
 *    start()，避免一次性把整部片子灌进 MSE；已送出的样例用
 *    releaseUsedSamples 释放，内存稳定在 ~1× 文件大小；
 * 5. seek：stop() → 清空两个 SourceBuffer → mp4box.seek(t, RAP 对齐)
 *    （内部把两条轨道的 nextSample 与分段状态重置到目标点）→ start()；
 * 6. 切音轨：只动音频 SourceBuffer（视频缓冲不动、画面不中断）：
 *    unset 旧轨 → 清空音频 SB → set 新轨 → 重新生成 init →
 *    seekTrack 把新轨单独定位到当前播放位置 → start()。
 *
 * 设计取舍（v1）：多音轨路径需要把整个文件载入内存（数百 MB ~ 2GB
 * 可用；更大的文件请转单音轨或 HLS）。任何一步失败都会抛出，
 * 由 App 层回退到原生 <video> 播放（单音轨），不阻断使用。
 */
import { createFile } from 'mp4box';

/** 每个 onSegment 段的样本数上界（两条轨道必须一致，mp4box 约束） */
const SEG_NB_SAMPLES = 800;
/** 前瞻缓冲目标（秒）：超过即暂停分段生成 */
const AHEAD_TARGET = 24;
/** 前瞻降到该值以下恢复分段生成 */
const AHEAD_RESUME = 12;

/** 单条轨道的 SourceBuffer + 追加队列 */
function makeQueue(sb) {
  return { sb, queue: [] };
}

export class MseEngine {
  /**
   * @param {HTMLVideoElement} video 播放器元素
   * @param {object} opts
   * @param {(reason: Error) => void} opts.onFail 任何一步失败时回调（App 回退原生播放）
   */
  constructor(video, { onFail } = {}) {
    this.video = video;
    this.onFail = onFail || (() => {});
    this.mp4box = null;
    this.ms = null;
    this.objectUrl = null;
    this.vq = null; // 视频轨道队列
    this.aq = null; // 音频轨道队列
    this.videoTrackId = null;
    this.audioTrackId = null;
    this.durationSec = 0;
    this.destroyed = false;
    this._buffer = null; // 整文件 ArrayBuffer（字幕提取复用）
    this._failures = 0;
    this._pendingPrecise = null; // RAP 粗定位后待精确落点的时间（秒）
    this._seeking = false; // seek / 切轨的重定位窗口：拦截 rogue 恢复
  }

  /** 是否已经建好（可用于 seek / 切轨） */
  get ready() {
    return !!this.mp4box && !!this.ms && this.ms.readyState === 'open';
  }

  /**
   * 加载并启动多音轨 MSE 播放。
   * @param {string} fileUrl asset 协议 URL（fetch 整文件）
   * @param {object} meta analyzeMp4 的结果（需要 video / audioTracks）
   * @param {number} audioTrackId 初始音轨
   */
  async load(fileUrl, meta, audioTrackId) {
    const videoTrackId = meta.video ? meta.video.id : null;
    if (videoTrackId == null) throw new Error('没有视频轨');
    if (typeof MediaSource === 'undefined' || !MediaSource.isTypeSupported) {
      throw new Error('WebView 不支持 MSE');
    }
    const vMime = `video/mp4; codecs="${meta.video.codec}"`;
    const aTrack = meta.audioTracks.find((t) => t.id === audioTrackId) || meta.audioTracks[0];
    const aMime = `audio/mp4; codecs="${aTrack.codec}"`;
    if (!MediaSource.isTypeSupported(vMime)) throw new Error(`MSE 不支持视频编码：${meta.video.codec}`);
    if (!MediaSource.isTypeSupported(aMime)) throw new Error(`MSE 不支持音频编码：${aTrack.codec}`);

    // ---- 1. 整文件读入内存（多音轨路径的核心代价，文档已说明取舍）----
    const resp = await fetch(fileUrl);
    if (!resp.ok) throw new Error(`读取文件失败：${resp.status}`);
    this._buffer = await resp.arrayBuffer();

    // ---- 2. MediaSource 挂到 <video>（sourceopen 后才能加 SourceBuffer）----
    this.ms = new MediaSource();
    this.objectUrl = URL.createObjectURL(this.ms);
    this.video.src = this.objectUrl;
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('MSE sourceopen 超时')), 8000);
      this.ms.addEventListener('sourceopen', () => { clearTimeout(timer); resolve(); }, { once: true });
    });

    this.vq = makeQueue(this.ms.addSourceBuffer(vMime));
    this.aq = makeQueue(this.ms.addSourceBuffer(aMime));
    this.durationSec = meta.durationSec || 0;
    if (this.durationSec > 0) this.ms.duration = this.durationSec;
    for (const q of [this.vq, this.aq]) {
      q.sb.addEventListener('updateend', () => this._onUpdateEnd(q));
      q.sb.addEventListener('error', () => this._fail(new Error('SourceBuffer 追加失败')));
    }

    // ---- 3. 喂给 mp4box（onReady 在 appendBuffer 内同步触发）----
    this.mp4box = createFile(true); // 保留 mdat：之后任意 getSample 都有数据
    this.mp4box.onError = (e) => this._fail(new Error(String(e)));
    this.mp4box.onSegment = (id, user, buffer, sampleNumber) => {
      this._onSegment(id, user, buffer, sampleNumber);
    };
    let info = null;
    this.mp4box.onReady = (i) => { info = i; };
    this._buffer.fileStart = 0;
    this.mp4box.appendBuffer(this._buffer);
    if (!info) throw new Error('mp4box 未能解析出 moov');

    // ---- 4. 注册两条轨道并生成 init segment ----
    this.videoTrackId = videoTrackId;
    this.audioTrackId = aTrack.id;
    const opts = { nbSamples: SEG_NB_SAMPLES };
    this.mp4box.setSegmentOptions(this.videoTrackId, this.vq, opts);
    this.mp4box.setSegmentOptions(this.audioTrackId, this.aq, opts);
    const initSegs = this.mp4box.initializeSegmentation('per-track');
    for (const seg of initSegs) {
      seg.user.queue.push(seg.buffer);
    }
    this._pump(this.vq);
    this._pump(this.aq);

    // ---- 5. 开闸：onSegment 依节流策略推进 ----
    this.mp4box.start();

    // ---- 6. 等 init 段被 MSE 消化（loadedmetadata 到来）再返回：
    //      调用方随后做断点定位时 duration / currentTime 都是就绪的，
    //      否则 seek 会被记成「默认起始位置」并在装载完成后丢失 ----
    await new Promise((resolve) => {
      if (this.destroyed) return resolve();
      if (this.video.readyState >= 1) return resolve();
      const timer = setTimeout(resolve, 4000); // 保险丝：异常流不卡死
      this.video.addEventListener(
        'loadedmetadata',
        () => {
          clearTimeout(timer);
          resolve();
        },
        { once: true }
      );
    });
  }

  /** 拿到内部文件缓冲（内嵌字幕提取复用，避免二次 fetch） */
  get buffer() {
    return this._buffer;
  }

  /* ============================== 分段回调 ============================== */

  _onSegment(id, user, buffer, sampleNumber) {
    if (this.destroyed) return;
    user.queue.push(buffer);
    // 已送出的样例立即释放，内存稳定在 ~1× 文件大小
    try { this.mp4box.releaseUsedSamples(id, sampleNumber); } catch { /* 忽略 */ }
    this._pump(user);
    // 节流：前瞻够远就暂停生成（start/stop 可随时往复）
    if (this._aheadSec() > AHEAD_TARGET) this.mp4box.stop();
  }

  _onUpdateEnd(q) {
    this._pump(q);
    this._ensureGenerating();
    this._tryPreciseAdjust();
  }

  /** 队列驱动的顺序追加（MSE 要求上一个 updateend 后才能追加下一个） */
  _pump(q) {
    if (this.destroyed || !q || q.sb.updating || !q.queue.length) return;
    const buf = q.queue.shift();
    try {
      q.sb.appendBuffer(buf);
    } catch (e) {
      this._fail(e instanceof Error ? e : new Error(String(e)));
    }
  }

  /** 前瞻不足时恢复生成（timeupdate / updateend 都会尝试） */
  _ensureGenerating() {
    if (this.destroyed || !this.mp4box || !this.ready || this._seeking) return;
    if (!this.mp4box.sampleProcessingStarted && this._aheadSec() < AHEAD_RESUME) {
      this.mp4box.start();
    }
  }

  /** 前瞻秒数：取各 SourceBuffer 的最小值（交集决定可播范围；
   *  切换音轨后音频轨清空，若只看视频轨会误判「缓冲充足」而永不续流） */
  _aheadSec() {
    const qs = [this.vq, this.aq].filter(Boolean);
    if (!qs.length) return 0;
    let ahead = Infinity;
    for (const q of qs) {
      try {
        const b = q.sb.buffered;
        let end = -1;
        for (let i = 0; i < b.length; i++) {
          if (b.end(i) > this.video.currentTime) end = Math.max(end, b.end(i));
        }
        ahead = Math.min(ahead, end === -1 ? 0 : Math.max(0, end - this.video.currentTime));
      } catch {
        ahead = 0;
      }
    }
    return ahead === Infinity ? 0 : ahead;
  }

  /* ============================== seek / 切轨 ============================== */

  /**
   * MSE 语义的 seek：清空缓冲 → mp4box.seek（RAP 对齐，重置两条轨道）
   * → 恢复生成。返回实际落点时间（调用方把 video.currentTime 设为它）。
   *
   * 两级落点：粗定位到目标前最近的关键帧（RAP），待该处媒体段被
   * 追加进 SourceBuffer 后再自动精确跳到目标时间（_pendingPrecise），
   * 让断点恢复 / 手动跳转都能落在请求的帧上。
   *
   * 快路径：目标时间已在两条轨道的已缓冲范围内时直接返回原值
   * （不清缓冲、不重分段）——逐帧步进也因此可以精确落点。
   */
  async seekTo(t) {
    if (!this.ready) return t;
    const tt = Math.max(0, t);
    this._pendingPrecise = null;
    if (this._bufferedContains(this.vq, tt) && this._bufferedContains(this.aq, tt)) {
      return tt; // 已缓冲：调用方直接设 currentTime 即可
    }
    const box = this.mp4box;
    box.stop();
    // 重定位窗口：flush 期间的 updateend 会触发 _ensureGenerating，
    // 若不拦截会在旧轨道状态上抢先 start()，把随后的定位彻底搅乱
    this._seeking = true;
    try {
      await Promise.all([this._flushSb(this.vq), this._flushSb(this.aq)]);
      // 只对「视频 + 当前音轨」两条工作轨定位：mp4box.seek() 会对全部
      // 轨道（含字幕等稀疏采样轨）取最小落点，把时间拉偏到字幕样本处
      const vTrak = this.videoTrackId != null ? box.getTrackById(this.videoTrackId) : null;
      const aTrak = this.audioTrackId != null ? box.getTrackById(this.audioTrackId) : null;
      let rap = tt;
      if (vTrak) {
        rap = box.seekTrack(tt, true, vTrak).time;
      }
      if (aTrak) {
        box.seekTrack(tt, true, aTrak); // 音频轨同步重定位（落点以视频 RAP 为准）
      }
      if (!isFinite(rap) || rap < 0) rap = t;
      if (Math.abs(rap - tt) > 0.05) this._pendingPrecise = tt; // 落点差超过半帧才需要精调
      return rap;
    } finally {
      this._seeking = false;
      this.mp4box.start(); // 无条件开闸：start() 总会推进 processSamples
    }
  }

  /** RAP 粗定位后的精确落点：目标时间被缓冲覆盖即生效（tick / updateend 驱动） */
  _tryPreciseAdjust() {
    const t = this._pendingPrecise;
    if (t === null || this.destroyed || !this.ready) return;
    // 用户已经播过去了就不再回跳
    if (this.video.currentTime > t + 0.3) {
      this._pendingPrecise = null;
      return;
    }
    if (this._bufferedContains(this.vq, t) && this._bufferedContains(this.aq, t)) {
      this._pendingPrecise = null;
      try {
        this.video.currentTime = t;
      } catch {
        /* 忽略 */
      }
    }
  }

  /** 某条 SourceBuffer 的已缓冲范围是否覆盖 t（±50ms 容差） */
  _bufferedContains(q, t) {
    if (!q) return false;
    try {
      const b = q.sb.buffered;
      for (let i = 0; i < b.length; i++) {
        if (t >= b.start(i) - 0.05 && t <= b.end(i) + 0.05) return true;
      }
    } catch {
      /* 忽略 */
    }
    return false;
  }

  /**
   * 切换音轨：视频缓冲不动，音频 SourceBuffer 清空后重挂新轨，
   * seekTrack 把新轨单独定位到当前播放位置。
   */
  async switchAudio(trackId) {
    if (!this.ready || trackId === this.audioTrackId) return;
    const box = this.mp4box;
    box.stop();
    if (this.audioTrackId != null) box.unsetSegmentOptions(this.audioTrackId);
    this.audioTrackId = trackId;

    this._seeking = true;
    try {
      await this._flushSb(this.aq);
    } finally {
      this._seeking = false;
    }
    box.setSegmentOptions(trackId, this.aq, { nbSamples: SEG_NB_SAMPLES });
    // 新轨道的 init segment（只取音频那条，视频的忽略）
    const initSegs = box.initializeSegmentation('per-track');
    const init = initSegs.find((s) => s.id === trackId);
    if (init) {
      this.aq.queue.push(init.buffer);
      this._pump(this.aq);
    }
    // 只把新音轨定位到当前播放位置（视频轨 nextSample 不动）
    const trak = box.getTrackById(trackId);
    if (trak) box.seekTrack(Math.max(0, this.video.currentTime), true, trak);
    // 直接开闸：此时音频轨为空，_aheadSec 语义已变，不依赖 ensure
    this.mp4box.start();
    this._ensureGenerating();
  }

  /** 等 SourceBuffer 空闲（append/remove 完成后触发 updateend） */
  _waitIdle(q) {
    return new Promise((resolve) => {
      if (!q.sb.updating) return resolve();
      let done = false;
      const fin = () => { if (!done) { done = true; resolve(); } };
      q.sb.addEventListener('updateend', fin, { once: true });
      setTimeout(fin, 1500); // 保险丝：万一 updateend 丢失
    });
  }

  /** 清空一条 SourceBuffer（清空追加队列 + 移除全部已缓冲范围） */
  async _flushSb(q) {
    if (this.destroyed || !q) return;
    q.queue.length = 0;
    try {
      if (q.sb.updating) {
        try { q.sb.abort(); } catch { /* 忽略 */ }
        await this._waitIdle(q);
      }
      const b = q.sb.buffered;
      if (b.length) {
        await new Promise((resolve) => {
          let done = false;
          const fin = () => { if (!done) { done = true; resolve(); } };
          q.sb.addEventListener('updateend', fin, { once: true });
          setTimeout(fin, 1500);
          try { q.sb.remove(0, b.end(b.length - 1) + 0.1); } catch { fin(); }
        });
      }
    } catch { /* 尽力而为 */ }
  }

  /** 供 App 在 timeupdate 里驱动节流恢复与精确落点 */
  tick() {
    this._ensureGenerating();
    this._tryPreciseAdjust();
  }

  /* ============================== 生命周期 ============================== */

  _fail(err) {
    if (this.destroyed) return;
    this._failures++;
    try { if (this.mp4box) this.mp4box.stop(); } catch { /* 忽略 */ }
    this.onFail(err);
  }

  destroy() {
    this.destroyed = true;
    try { if (this.mp4box) this.mp4box.stop(); } catch { /* 忽略 */ }
    if (this.objectUrl) {
      URL.revokeObjectURL(this.objectUrl);
      this.objectUrl = null;
    }
    try {
      if (this.ms && this.ms.readyState === 'open') {
        for (const q of [this.vq, this.aq]) {
          if (q) this.ms.removeSourceBuffer(q.sb);
        }
        this.ms.endOfStream();
      }
    } catch { /* 忽略 */ }
    this.ms = null;
    this.vq = this.aq = null;
    this.mp4box = null;
  }
}
