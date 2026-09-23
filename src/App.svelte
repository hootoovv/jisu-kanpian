<script>
  /**
   * 极速看片 —— 主组件
   *
   * 核心思路：
   * 1. 用户拖入目录（或首页「+」点选）→ Rust 递归扫描（自然排序，
   *    同时收集同目录的外挂字幕）→ 得到「目录 → 视频文件」有序结构；
   *    前端摊平成一条观看序列 flat[]；文件列表放在屏幕右侧的独立
   *    浮层（T 键显隐，分页行 lines[]），主窗口整幅留给画面——
   *    纯黑舞台、画面包含式居中、左上角白色文件名；
   * 2. 播放管线按文件类型分流：
   *    - mp4 家族：先经 read_range 只读 moov 做轨道分析（mp4box.js），
   *      多音轨且 MSE 支持时启用 MseEngine（mp4box 分段 + 双
   *      SourceBuffer，音轨可即时切换），否则原生 <video> 直读；
   *    - m3u8：hls.js（WebView 原生支持 HLS 时优先原生）；
   *    - 其余：原生 <video>（asset 协议直读本地文件）；
   * 3. 字幕统一 WebVTT：外挂 .vtt 直挂 <track>，.srt 转换后挂载，
   *    MP4 内嵌 wvtt / tx3g 由 mp4box 抽样提取后生成 VTT blob；
   *    hls.js 的字幕轨走其自带 textTracks；
   * 4. 画面支持旋转（顺时针 90° 步进）与放大缩小（1×~5×），
   *    放大后可拖拽平移（VideoStage 内做屏幕位移 → 画面坐标系的
   *    逆旋变换）；
   * 5. 每个文件的播放位置独立记忆（播完自动清空）；退出时记住当前
   *    文件与位置，下次启动定位到该位置并暂停（断点续看）；
   * 6. X 键：关闭当前目录回到引导页，同时清除该目录的位置记忆；
   *    列表顺序连播，播完最后一个文件即停（本程序没有循环 / 随机）；
   * 7. 全屏 = 纯画面模式：顶部信息栏、底部控制栏、右侧文件列表
   *    一律立即隐藏，屏幕上只留视频画面，退出全屏后按 I / C / T
   *    偏好恢复；播放期间通过 Wake Lock（不可用时后端 set_keep_awake
   *    兜底）阻止系统屏保与休眠，暂停 / 退出播放即解除。
   */
  import { onMount, onDestroy } from 'svelte';
  import { invoke, convertFileSrc } from '@tauri-apps/api/core';
  import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import HintScreen from './components/HintScreen.svelte';
  import InfoBar from './components/InfoBar.svelte';
  import ControlBar from './components/ControlBar.svelte';
  import VideoStage from './components/VideoStage.svelte';
  import FileList from './components/FileList.svelte';
  import HelpOverlay from './components/HelpOverlay.svelte';
  import { clamp } from './lib/format.js';
  import { initWakeLock, setKeepAwake } from './lib/wakelock.js';
  import { MP4_FAMILY, extOf, langLabel, normLang, matchSubs, rewriteHlsPlaylist } from './lib/media.js';
  import { analyzeMp4 } from './lib/mp4/analyzer.js';
  import { MseEngine } from './lib/mp4/mse-engine.js';
  import {
    SubtitleManager,
    extractEmbeddedSubtitles,
    loadSrtAsVtt,
    cuesToVtt
  } from './lib/subtitles.js';

  const win = getCurrentWebviewWindow();

  /* ---------- 常量 ---------- */
  const SEEK_BIG = 30;          // 左右方向键每次快退 / 快进秒数
  const VOL_STEP = 0.05;        // 音量步长（5%）
  const ZOOM_STEP = 0.25;       // 画面缩放步长
  const ZOOM_MIN = 1;
  const ZOOM_MAX = 5;
  const FRAME_FALLBACK_FPS = 30; // 帧率未知时的逐帧步进假设
  const SAVE_DEBOUNCE = 1200;   // 状态持久化防抖（ms）
  const STATUS_TIMEOUT = 4000;  // 底部提示自动消失（ms）
  const POS_MIN_MS = 1000;      // 小于 1 秒的位置不记忆
  const LIST_ROW_H = 24;        // 文件列表浮层行高（固定小字号）

  /* ---------- 响应式 UI 状态（参与模板渲染） ---------- */
  let phase = $state('hint');   // hint | scanning | view | empty | error
  let phaseMessage = $state('');
  let dragActive = $state(false);
  let infoVisible = $state(true);     // I 键：顶部信息栏（默认显示）
  let ctrlVisible = $state(true);     // C 键：底部控制栏（默认显示）
  let listVisible = $state(true);     // T 键：右侧文件列表浮层（默认显示）
  let helpVisible = $state(false);    // F1：帮助层
  let status = $state('');

  let view = $state({ rel: '', dir: '', name: '', pos: 0, total: 0 });
  let playing = $state(false);
  let curTime = $state(0);      // 文本 / 滑块用进度（timeupdate 节流刷新）
  let duration = $state(0);
  let volume = $state(1);
  let muted = $state(false);    // 静音（临时状态，不持久化；调音量自动解除）
  let rotation = $state(0);     // 顺时针累计角度（0/90/180/270）
  let zoom = $state(1);         // 画面缩放（1~5）
  let fullscreen = $state(false); // 全屏 = 纯画面模式（浮层全部立即隐藏）

  let audioOptions = $state([]);   // [{ key, label }]
  let audioValue = $state('');
  let subOptions = $state([]);     // [{ key, label }]
  let subValue = $state('off');

  let currentPage = $state(0);
  let listH = $state(0);
  let positions = $state(new Map()); // Map<path, ms>：每个文件的断点记忆
  let cursorIdx = $state(0); // 当前播放文件下标（$state 供列表高亮）
  let lines = $state([]);    // 分页行：[{type:'dir',rel} | {type:'file',idx,path,rel,name}]

  /* ---------- 核心状态（不参与响应式渲染，纯 JS 管理） ---------- */
  let root = null;        // 主目录绝对路径
  let flat = [];          // 全部视频文件的扁平有序序列 [{path, rel, name, dirIndex, dirPath}]
  let lineOfIdx = [];     // flat 下标 → lines 行号（分页锚定用）
  let subsByDir = new Map(); // 目录路径 → 外挂字幕数组（扫描结果）
  let videoEl = null;     // 唯一的 <video> 元素
  let listEl = $state(null);      // 列表容器（测量可用行数用）
  let pendingSeek = null; // { idx, sec, autoplay }：等 loadedmetadata 后生效
  let currentVideoPath = null; // <video> 实际装载的文件路径（防断点误写）
  let saveTimer = 0;
  let statusTimer = 0;
  let loadGen = 0;        // 媒体加载代次（防止快速切换时旧结果覆盖新文件）
  let unlistenDrop = null;
  let unlistenClose = null;
  let subManager = null;  // 字幕控制器（挂 <track> 用）

  /* ---------- 媒体管线状态 ---------- */
  let mediaInfo = null;   // analyzeMp4 结果（或 null）
  let mediaMode = 'native'; // native | mse | hls
  let engine = null;      // MseEngine 实例（多音轨时）
  let hls = null;         // hls.js 实例（m3u8 时）
  let HlsCtor = null;     // 动态加载的 hls.js 构造器
  let mseLoading = false; // MSE 装载中：loadedmetadata 里不做断点定位
  let fpsKnown = 0;       // 已知帧率（分析 / rVFC 估算）
  let rvfcCount = 0;      // rVFC 帧率估算计数
  let rvfcStart = 0;
  let audioLangPref = 'zh';  // 音轨语言偏好（持久化）
  let subLangPref = 'zh';    // 字幕语言偏好（持久化）

  /* ---------- 派生：布局 ---------- */
  /* 全屏即纯画面模式：顶部信息栏、底部控制栏、右侧文件列表浮层
     一律立即隐藏（无论播放还是暂停），屏幕上只留视频画面。
     退出全屏后按 I / C / T 偏好恢复；全屏中按 I / C / T 仍切换
     偏好（持久化照常），退出后生效。非全屏时始终跟随偏好。 */
  const infoShown = $derived(!fullscreen && infoVisible);
  const ctrlShown = $derived(!fullscreen && ctrlVisible);
  const listShown = $derived(!fullscreen && listVisible);
  const pageSize = $derived(Math.max(1, Math.floor(listH / LIST_ROW_H)));
  const pageCount = $derived(Math.max(1, Math.ceil(lines.length / pageSize)));
  const pageLines = $derived.by(() => {
    const start = currentPage * pageSize;
    return lines.slice(start, start + pageSize);
  });
  const audioLabel = $derived(
    (audioOptions.find((o) => o.key === audioValue) || {}).label || ''
  );
  const subLabel = $derived(
    subValue === 'off' ? '' : (subOptions.find((o) => o.key === subValue) || {}).label || ''
  );

  /* ============================== 播放核心 ============================== */

  function setStatus(msg) {
    status = msg || '';
    clearTimeout(statusTimer);
    if (msg) statusTimer = setTimeout(() => (status = ''), STATUS_TIMEOUT);
  }

  async function tryPlay() {
    try {
      await videoEl.play();
    } catch {
      playing = false;
      setStatus('自动播放被系统拦截 · 按空格开始播放');
    }
  }

  function togglePlay() {
    if (phase !== 'view' || !flat.length || !videoEl) return;
    if (videoEl.paused) tryPlay();
    else videoEl.pause();
  }

  /** 快进 / 快退（秒，可为负） */
  function seekBy(delta) {
    if (phase !== 'view' || !videoEl) return;
    const dur = videoEl.duration;
    if (!isFinite(dur) || dur <= 0) return;
    seekTo(clamp(videoEl.currentTime + delta, 0, Math.max(0, dur - 0.05)));
  }

  async function seekTo(t) {
    if (!videoEl) return;
    // MSE 模式：清缓冲 + mp4box 重定位（返回 RAP 对齐后的实际落点）
    if (mediaMode === 'mse' && engine && engine.ready) {
      const adj = await engine.seekTo(clamp(t, 0, Math.max(0, (duration || t) - 0.05)));
      videoEl.currentTime = isFinite(adj) ? adj : t;
    } else {
      videoEl.currentTime = t;
    }
    curTime = videoEl.currentTime;
    scheduleSave();
  }

  function setVolume(v) {
    volume = clamp(v, 0, 1);
    if (videoEl) videoEl.volume = volume;
    // 主动调音量（滑杆 / 键盘 / 滚轮）自动解除静音
    if (volume > 0 && muted) {
      muted = false;
      if (videoEl) videoEl.muted = false;
    }
    scheduleSave();
  }

  /** 音量图标点击：静音 ⇄ 解除（临时状态，不持久化） */
  function toggleMute() {
    muted = !muted;
    if (videoEl) videoEl.muted = muted;
  }

  /** 旋转：每次顺时针 90° */
  function rotateCW() {
    rotation = (rotation + 90) % 360;
    scheduleSave();
  }

  /** 画面缩放（±步长，钳制 1~5） */
  function zoomBy(delta) {
    const next = clamp(Math.round((zoom + delta) * 100) / 100, ZOOM_MIN, ZOOM_MAX);
    if (next !== zoom) {
      zoom = next;
      if (ctrlVisible) setStatus(`画面 ${zoom.toFixed(2)}×`);
      scheduleSave();
    }
  }

  /** 逐帧步进（dir = ±1）：先暂停，再按已知帧率走一帧 */
  function stepFrame(dir) {
    if (phase !== 'view' || !videoEl || !duration) return;
    if (!videoEl.paused) videoEl.pause();
    const fps = fpsKnown > 1 ? fpsKnown : FRAME_FALLBACK_FPS;
    seekTo(clamp(videoEl.currentTime + dir / fps, 0, Math.max(0, duration - 0.001)));
  }

  /* ============================== 媒体管线 ============================== */

  /** 拆掉上一部片的引擎 / 字幕 / HLS */
  function teardownMedia() {
    mseLoading = false;
    if (engine) {
      engine.destroy();
      engine = null;
    }
    if (hls) {
      try { hls.destroy(); } catch { /* 忽略 */ }
      hls = null;
    }
    if (subManager) subManager.detach();
    if (videoEl) {
      try { videoEl.pause(); } catch { /* 忽略 */ }
      videoEl.removeAttribute('src');
      try { videoEl.load(); } catch { /* 忽略 */ }
    }
    mediaInfo = null;
    mediaMode = 'native';
    fpsKnown = 0;
    rvfcCount = 0;
    audioOptions = [];
    audioValue = '';
    subOptions = [];
    subValue = 'off';
  }

  /** mp4 家族轨道分析（快速：只读 moov） */
  function analyzeEntry(entry) {
    // 分析失败静默回退原生播放（打一条 warn 便于排查）
    return analyzeMp4(entry.path).catch((e) => {
      console.warn('轨道分析失败，回退原生播放', entry.path, e);
      return null;
    });
  }

  /** 按语言偏好挑默认音轨（保存过的偏好 → 界面语言 → 第一条） */
  function pickAudioTrack(tracks) {
    if (!tracks || !tracks.length) return null;
    const byPref = tracks.find((t) => normLang(t.lang) === audioLangPref);
    if (byPref) return byPref;
    const byUi = tracks.find((t) => normLang(t.lang) === 'zh');
    return byUi || tracks[0];
  }

  /**
   * 加载当前文件（playIndex 调用，带代次防护）。
   * 分流：m3u8 → hls.js / 原生 HLS；mp4 家族 → 先分析，
   * 多音轨且 MSE 可用 → MseEngine，否则原生 <video>。
   */
  async function loadMedia(entry) {
    const gen = ++loadGen;
    teardownMedia();
    const url = convertFileSrc(entry.path);
    const ext = extOf(entry.name);
    currentVideoPath = entry.path;

    if (ext === 'm3u8') {
      await loadHls(entry, url, gen);
      return;
    }

    if (MP4_FAMILY.includes(ext)) {
      setStatus('正在分析音轨…');
      const info = await analyzeEntry(entry);
      if (gen !== loadGen) return;
      setStatus('');
      if (info) {
        mediaInfo = info;
        if (info.video) fpsKnown = info.video.fps || 0;
        const multiAudio = info.audioTracks.length > 1;
        if (multiAudio && typeof MediaSource !== 'undefined') {
          const defTrack = pickAudioTrack(info.audioTracks);
          try {
            mseLoading = true;
            engine = new MseEngine(videoEl, {
              onFail: () => onEngineFail(gen)
            });
            await engine.load(url, info, defTrack ? defTrack.id : info.audioTracks[0].id);
            if (gen !== loadGen) return;
            mediaMode = 'mse';
            buildAudioMenuMp4(info, defTrack);
            buildSubtitleMenu(entry, info);
            await applyPendingSeek(); // MSE：装载完成后由这里做断点定位
            mseLoading = false;
            return;
          } catch (e) {
            console.warn('MSE 多音轨装载失败，回退原生播放', e);
            mseLoading = false;
            if (engine) {
              engine.destroy();
              engine = null;
            }
            // 原生回退：pendingSeek 保留，loadedmetadata 后照常断点定位
            if (gen === loadGen) {
              await loadNative(entry, url, gen, info);
            }
            return;
          }
        }
        await loadNative(entry, url, gen, info);
        return;
      }
      // 分析失败（非 mp4 封装 / moov 损坏）：直接原生
      await loadNative(entry, url, gen, null);
      return;
    }

    await loadNative(entry, url, gen, null);
  }

  /** 原生 <video> 直读 */
  async function loadNative(entry, url, gen, info) {
    if (gen !== loadGen) return;
    mediaMode = 'native';
    mediaInfo = info || null;
    if (info && info.video) fpsKnown = info.video.fps || 0;
    videoEl.src = url;
    videoEl.load();
    if (info && info.audioTracks.length > 1) {
      // MSE 不可用 / 编码不支持：菜单保留但切换时提示受限
      const defTrack = pickAudioTrack(info.audioTracks);
      audioOptions = info.audioTracks.map((t) => ({
        key: `mp4:${t.id}`,
        label: langLabel(t.lang, t.name)
      }));
      audioValue = `mp4:${defTrack.id}`;
    } else {
      audioOptions = [{ key: 'native', label: '默认音轨' }];
      audioValue = 'native';
    }
    buildSubtitleMenu(entry, info);
  }

  /** MSE 装载成功后构建音轨菜单（默认选中已装载的轨） */
  function buildAudioMenuMp4(info, defTrack) {
    audioOptions = info.audioTracks.map((t) => ({
      key: `mp4:${t.id}`,
      label: langLabel(t.lang, t.name)
    }));
    audioValue = `mp4:${(defTrack || info.audioTracks[0]).id}`;
  }

  /** HLS 装载（hls.js 优先，WebView 原生支持时直读） */
  async function loadHls(entry, url, gen) {
    const nativeHls = videoEl.canPlayType('application/vnd.apple.mpegurl');
    if (!HlsCtor) {
      try {
        HlsCtor = (await import('hls.js')).default;
      } catch (e) {
        console.warn('hls.js 加载失败', e);
      }
    }
    if (HlsCtor && HlsCtor.isSupported()) {
      mediaMode = 'hls';
      hls = new HlsCtor({ enableWorker: true });
      hls.on(HlsCtor.Events.MANIFEST_PARSED, () => {
        if (gen !== loadGen) return;
        buildHlsMenus(entry);
      });
      hls.on(HlsCtor.Events.ERROR, (_ev, data) => {
        if (gen !== loadGen || !data || !data.fatal) return;
        setStatus('HLS 播放失败：' + (data.details || data.type || '未知错误'));
      });
      // asset 协议把整个路径整体编码（%2F 不是分隔符），hls.js 按 URL
      // 规则解析相对分片会丢目录 —— 先取 m3u8 文本重写为绝对 asset URL
      let srcUrl = url;
      try {
        const resp = await fetch(url);
        if (!resp.ok) throw new Error(`读取播放列表失败：${resp.status}`);
        const text = await resp.text();
        const rewritten = rewriteHlsPlaylist(text, entry.path, convertFileSrc);
        srcUrl = URL.createObjectURL(new Blob([rewritten], { type: 'application/vnd.apple.mpegurl' }));
      } catch (e) {
        console.warn('m3u8 重写失败，按原 URL 装载', e);
      }
      hls.loadSource(srcUrl);
      hls.attachMedia(videoEl);
      return;
    }
    if (nativeHls) {
      // macOS WKWebView 等原生 HLS：直读（音轨 / 字幕菜单尽力而为）
      mediaMode = 'native';
      videoEl.src = url;
      videoEl.load();
      buildSubtitleMenu(entry, null);
      audioOptions = [{ key: 'native', label: '默认音轨' }];
      audioValue = 'native';
      return;
    }
    setStatus('当前 WebView 不支持 HLS 播放');
  }

  /** hls.js 清单解析完 → 构建 hls 音轨 / 字幕菜单 */
  function buildHlsMenus(entry) {
    const at = hls ? hls.audioTracks || [] : [];
    if (at.length > 1) {
      audioOptions = at.map((t) => ({ key: `hls:${t.id}`, label: langLabel(t.lang, t.name) }));
      const def = at.find((t) => normLang(t.lang) === audioLangPref) || at.find((t) => normLang(t.lang) === 'zh') || at[0];
      audioValue = `hls:${def.id}`;
      try { hls.audioTrack = def.id; } catch { /* 忽略 */ }
    } else {
      audioOptions = [{ key: 'native', label: '默认音轨' }];
      audioValue = 'native';
    }
    // 字幕：hls 自带 textTracks（选择时直接设 hls.subtitleTrack）
    const st = hls ? hls.subtitleTracks || [] : [];
    const opts = [{ key: 'off', label: '不启用', lang: '' }];
    for (const t of st) {
      opts.push({ key: `hls:${t.id}`, label: langLabel(t.lang, t.name), lang: t.lang });
    }
    // 合并外挂字幕
    appendExternalSubOptions(entry, opts);
    subOptions = opts;
    applyDefaultSubtitle();
  }

  /** 构建字幕菜单（内嵌 + 外挂 + 不启用） */
  function buildSubtitleMenu(entry, info) {
    const opts = [{ key: 'off', label: '不启用', lang: '' }];
    if (info && info.subtitleTracks.length) {
      for (const t of info.subtitleTracks) {
        opts.push({ key: `mp4:${t.id}`, label: `${langLabel(t.lang, t.name)} · 内嵌`, lang: t.lang });
      }
    }
    appendExternalSubOptions(entry, opts);
    subOptions = opts;
    applyDefaultSubtitle();
  }

  /** 外挂字幕（同名 .vtt / .srt，含 .zh 等语言后缀）追加进菜单 */
  function appendExternalSubOptions(entry, opts) {
    const subs = subsByDir.get(entry.dirPath) || [];
    for (const s of matchSubs(entry.name, subs)) {
      const isSrt = s.ext === 'srt';
      opts.push({
        key: `ext:${s.path}`,
        label: `${langLabel(s.lang)} · 外挂${isSrt ? ' srt' : ''}`,
        lang: s.lang
      });
    }
  }

  /** 默认字幕：保存过的语言偏好 → 界面语言（中文） → 不启用。
   *  内嵌字幕默认只在 MSE 模式自动选中（文件字节已在内存，零成本）；
   *  原生模式下内嵌提取需整文件读入，默认不自动选（手动选择不受限） */
  function applyDefaultSubtitle() {
    const candidates = subOptions.filter((o) => o.key !== 'off');
    if (!candidates.length) {
      subValue = 'off';
      return;
    }
    const usable = (o) => !o.key.startsWith('mp4:') || mediaMode === 'mse';
    const pick = (lang) => {
      const list = candidates.filter(usable);
      return (
        list.find((o) => o.key.startsWith('ext:') && normLang(o.lang) === lang) ||
        list.find((o) => normLang(o.lang) === lang)
      );
    };
    const hit = pick(subLangPref) || pick('zh');
    if (hit) selectSubtitle(hit.key, { silent: true });
    else subValue = 'off';
  }

  /** MSE 运行期失败（append 报错等）：保位置回退原生 */
  function onEngineFail(gen) {
    if (gen !== loadGen || !currentVideoPath) return;
    const entry = flat[cursorIdx];
    if (!entry) return;
    const t = videoEl ? videoEl.currentTime : 0;
    const wasPlaying = playing;
    const info = mediaInfo;
    setStatus('多音轨引擎出错 · 已回退原生播放');
    const g2 = ++loadGen;
    teardownMedia();
    currentVideoPath = entry.path;
    mediaInfo = info;
    // 原生重载（不再走 MSE，保留时间点与播放意图）
    mediaMode = 'native';
    videoEl.src = convertFileSrc(entry.path);
    videoEl.load();
    if (info && info.audioTracks.length > 1) {
      audioOptions = info.audioTracks.map((tr) => ({
        key: `mp4:${tr.id}`,
        label: langLabel(tr.lang, tr.name)
      }));
      audioValue = audioOptions[0].key;
    } else {
      audioOptions = [{ key: 'native', label: '默认音轨' }];
      audioValue = 'native';
    }
    buildSubtitleMenu(entry, info);
    pendingSeek = { idx: cursorIdx, sec: t, autoplay: wasPlaying };
    void g2;
  }

  /* ---------- 音轨 / 字幕选择 ---------- */

  async function selectAudio(key) {
    if (key === audioValue) return;
    const label = (audioOptions.find((o) => o.key === key) || {}).label || '';
    if (key.startsWith('mp4:')) {
      const id = Number(key.slice(4));
      if (mediaMode === 'mse' && engine && engine.ready) {
        try {
          await engine.switchAudio(id);
          audioValue = key;
          setStatus(`音轨：${label}`);
          rememberAudioLang(id);
        } catch {
          setStatus('切换音轨失败');
        }
      } else {
        // 原生播放（MSE 不可用）：Chromium 系 <video> 无法切换音轨
        audioValue = key;
        setStatus(`音轨：${label}（原生播放暂无法切换）`);
      }
    } else if (key.startsWith('hls:')) {
      const id = Number(key.slice(4));
      if (hls) {
        try { hls.audioTrack = id; } catch { /* 忽略 */ }
        audioValue = key;
        setStatus(`音轨：${label}`);
        const tr = (hls.audioTracks || []).find((t) => t.id === id);
        if (tr) {
          audioLangPref = normLang(tr.lang) || audioLangPref;
          scheduleSave();
        }
      }
    }
  }

  function rememberAudioLang(trackId) {
    if (!mediaInfo) return;
    const tr = mediaInfo.audioTracks.find((t) => t.id === trackId);
    if (tr) {
      audioLangPref = normLang(tr.lang) || audioLangPref;
      scheduleSave();
    }
  }

  async function selectSubtitle(key, { silent = false } = {}) {
    if (key === subValue && !silent) return;
    subValue = key;
    const label = (subOptions.find((o) => o.key === key) || {}).label || '';
    if (key === 'off') {
      if (mediaMode === 'hls' && hls) {
        try { hls.subtitleTrack = -1; } catch { /* 忽略 */ }
      }
      if (subManager) subManager.detach();
      if (!silent) setStatus('字幕：不启用');
      return;
    }
    if (key.startsWith('hls:')) {
      const id = Number(key.slice(4));
      if (hls) {
        try { hls.subtitleTrack = id; } catch { /* 忽略 */ }
        const tr = (hls.subtitleTracks || []).find((t) => t.id === id);
        if (tr) {
          subLangPref = normLang(tr.lang) || subLangPref;
          scheduleSave();
        }
        if (!silent) setStatus(`字幕：${label}`);
      }
      return;
    }
    if (key.startsWith('ext:')) {
      const path = key.slice(4);
      const ext = extOf(path);
      try {
        if (ext === 'srt') {
          const vttText = await loadSrtAsVtt(convertFileSrc(path));
          await subManager.attach({ vttText, lang: subLangOfKey(key), label });
        } else {
          await subManager.attach({ src: convertFileSrc(path), lang: subLangOfKey(key), label });
        }
        rememberSubLangFromLabel(key);
        if (!silent) setStatus(`字幕：${label}`);
      } catch {
        setStatus('字幕加载失败');
        subValue = 'off';
      }
      return;
    }
    if (key.startsWith('mp4:')) {
      const id = Number(key.slice(4));
      const tr = mediaInfo ? mediaInfo.subtitleTracks.find((t) => t.id === id) : null;
      if (!tr || !currentVideoPath) {
        subValue = 'off';
        return;
      }
      setStatus('正在提取内嵌字幕…');
      try {
        let buf = engine && engine.buffer ? engine.buffer : null;
        if (!buf) {
          const resp = await fetch(convertFileSrc(currentVideoPath));
          buf = await resp.arrayBuffer();
        }
        const cues = await extractEmbeddedSubtitles(buf, id, tr.codec);
        if (!cues.length) {
          setStatus('该字幕轨没有可显示的内容');
          subValue = 'off';
          return;
        }
        const vttText = cuesToVtt(cues);
        await subManager.attach({ vttText, lang: normLang(tr.lang), label });
        subLangPref = normLang(tr.lang) || subLangPref;
        scheduleSave();
        setStatus(`字幕：${label}`);
      } catch {
        setStatus('内嵌字幕提取失败');
        subValue = 'off';
      }
      return;
    }
  }

  /** 外挂字幕的语言：从菜单构建时的 matchSubs 结果反查 */
  function subLangOfKey(key) {
    if (!key.startsWith('ext:')) return 'zh';
    const path = key.slice(4);
    const entry = flat[cursorIdx];
    if (!entry) return 'zh';
    const subs = subsByDir.get(entry.dirPath) || [];
    const m = matchSubs(entry.name, subs).find((s) => s.path === path);
    return (m && m.lang) || 'zh';
  }

  function rememberSubLangFromLabel(key) {
    const lang = subLangOfKey(key);
    if (lang && lang !== 'und') {
      subLangPref = lang;
      scheduleSave();
    }
  }

  /* ============================== 导航 ============================== */

  /**
   * 跳到（环形）下标 i 并加载播放。
   * - autoplay：加载完成后是否自动播放（断点续看时为 false）；
   * - seekSec：显式定位（秒）；缺省用该文件的断点记忆。
   */
  function playIndex(i, { autoplay = true, seekSec = null } = {}) {
    const n = flat.length;
    if (!n || phase !== 'view' || !videoEl) return;
    syncCurrentPosition(); // 先把旧文件的当前位置入账
    cursorIdx = ((i % n) + n) % n;
    const entry = flat[cursorIdx];
    const dir = entry.rel.length > entry.name.length
      ? entry.rel.slice(0, entry.rel.length - entry.name.length - 1)
      : '';
    view = { rel: entry.rel, dir, name: entry.name, pos: cursorIdx + 1, total: n };

    const ln = lineOfIdx[cursorIdx] ?? 0;
    currentPage = Math.min(Math.floor(ln / pageSize), Math.max(0, pageCount - 1));

    const sec = seekSec !== null && seekSec !== undefined ? seekSec : (positions.get(entry.path) || 0) / 1000;
    pendingSeek = { idx: cursorIdx, sec, autoplay };
    duration = 0;
    curTime = 0;
    playing = false;

    loadMedia(entry);
    scheduleSave();
  }

  function manualNext() {
    if (phase !== 'view' || !flat.length) return;
    playIndex(cursorIdx + 1); // 环形
  }

  function manualPrev() {
    if (phase !== 'view' || !flat.length) return;
    playIndex(cursorIdx - 1); // 环形
  }

  /** 一个文件播放完毕：清断点，顺序连播，列表末尾停止 */
  function onEnded() {
    const path = flat.length ? flat[cursorIdx].path : null;
    if (path) positions.delete(path); // 播放完毕清空该文件的断点记忆
    playing = false;
    curTime = 0;
    if (cursorIdx < flat.length - 1) {
      playIndex(cursorIdx + 1, { autoplay: true });
    } else {
      setStatus('列表播放完毕');
      scheduleSave();
    }
  }

  /* ============================== 分页 ============================== */

  function nextPage() {
    if (phase !== 'view' || pageCount <= 1) return;
    currentPage = (currentPage + 1) % pageCount; // 环形翻页
  }

  /* ============================== 断点记忆 ============================== */

  /** 把当前文件的播放位置写入 / 清出 positions（<1s 或接近末尾不记忆） */
  function syncCurrentPosition() {
    if (!flat.length || !videoEl) return;
    const path = flat[cursorIdx].path;
    if (path !== currentVideoPath) return; // 画面尚未换到游标所指文件
    const t = videoEl.currentTime || 0;
    const dur = videoEl.duration;
    if (!isFinite(dur) || dur <= 0) {
      if (t * 1000 > POS_MIN_MS) positions.set(path, Math.round(t * 1000));
      return;
    }
    if (t * 1000 < POS_MIN_MS || t >= dur - 1) {
      positions.delete(path); // 起始附近不记忆；距末尾不足 1 秒视为已看完
    } else {
      positions.set(path, Math.round(t * 1000));
    }
  }

  /** 清除某个目录（含子目录）下所有文件的断点记忆（X 键用） */
  function prunePositionsUnder(rootPath) {
    const prefix = String(rootPath || '').replace(/[\\/]+$/, '');
    if (!prefix) return;
    for (const key of [...positions.keys()]) {
      if (key === prefix) {
        positions.delete(key);
      } else if (key.startsWith(prefix) && (key[prefix.length] === '/' || key[prefix.length] === '\\')) {
        positions.delete(key);
      }
    }
  }

  function scheduleSave() {
    clearTimeout(saveTimer);
    saveTimer = setTimeout(saveStateNow, SAVE_DEBOUNCE);
  }

  function buildStatePayload(cleared = false) {
    const entry = flat.length ? flat[cursorIdx] : null;
    return {
      state: {
        root: cleared ? null : root,
        file: cleared ? null : (entry ? entry.path : null),
        position_ms: cleared || !entry ? null : (positions.get(entry.path) ?? null),
        volume,
        rotation,
        zoom,
        audio_lang: audioLangPref,
        subtitle_lang: subLangPref,
        info_visible: infoVisible,
        ctrl_visible: ctrlVisible,
        list_visible: listVisible,
        positions: Object.fromEntries(positions)
      }
    };
  }

  async function saveStateNow() {
    if (!root || !flat.length) return;
    syncCurrentPosition();
    try {
      await invoke('save_state', buildStatePayload(false));
    } catch {
      /* 保存失败不打断观看 */
    }
  }

  /* ============================== 打开 / 关闭目录 ============================== */

  /** 首页「+」按钮：弹出系统目录选择对话框（与拖拽等效） */
  async function pickDirectory() {
    if (phase === 'scanning') return;
    try {
      const dir = await openDialog({
        title: '选择视频文件夹',
        directory: true,
        multiple: false
      });
      if (typeof dir === 'string' && dir) openRoot(dir);
    } catch (e) {
      setStatus(`打开目录选择对话框失败：${e}`);
    }
  }

  /** 摊平扫描结果：flat（播放序列）+ lines（分页行，含目录分组头）+ 字幕索引 */
  function buildList(scan) {
    root = scan.root;
    flat = [];
    lineOfIdx = [];
    subsByDir = new Map();
    const builtLines = [];
    scan.directories.forEach((d, di) => {
      subsByDir.set(d.path, d.subs || []);
      if (d.rel) builtLines.push({ type: 'dir', rel: d.rel });
      for (const f of d.files) {
        lineOfIdx[flat.length] = builtLines.length;
        builtLines.push({ type: 'file', idx: flat.length, path: f.path, rel: f.rel, name: f.name });
        flat.push({ path: f.path, rel: f.rel, name: f.name, dirIndex: di, dirPath: d.path });
      }
    });
    lines = builtLines;
  }

  /**
   * 打开一个根目录（拖拽进来的是文件时自动改用其父目录）。
   * restore：上次会话的状态，用于断点续看——定位到退出的位置并暂停。
   */
  async function openRoot(rootPath, restore = null) {
    phase = 'scanning';
    phaseMessage = '正在扫描目录并排序…';
    try {
      videoEl.pause();
    } catch {
      /* 忽略 */
    }
    loadGen++;
    teardownMedia();
    try {
      const scan = await invoke('scan_directory', { root: rootPath });
      buildList(scan);
      if (!flat.length) {
        phase = 'empty';
        phaseMessage = `在 ${scan.root} 及其子目录中没有找到视频文件`;
        return;
      }
      phase = 'view';

      // 各文件断点记忆（含上次退出时正在看的文件）
      positions = new Map(
        Object.entries(restore?.positions || {}).map(([k, v]) => [k, Number(v) || 0])
      );

      let start = 0;
      let autoplay = true;
      let seekSec = null;
      if (restore && restore.file) {
        const idx = flat.findIndex((f) => f.path === restore.file);
        if (idx >= 0) {
          start = idx;
          seekSec = (restore.position_ms || 0) / 1000;
          autoplay = false; // 断点续看：定位到退出位置，暂停等待
        }
      }
      playIndex(start, { autoplay, seekSec });

      const name = root.split(/[\\/]/).filter(Boolean).pop() || root;
      win.setTitle(`极速看片 — ${name}`).catch(() => {});
    } catch (err) {
      phase = 'error';
      phaseMessage = `打开目录失败：${err}`;
    }
  }

  /**
   * X 键：关闭已加载的目录（或单个文件），回到引导页。
   * 同时清除该目录的位置记忆（恢复指针 + 各文件断点），
   * 音量 / 旋转 / 缩放 / 语言等偏好保留。
   */
  async function closeRoot() {
    if (phase === 'hint' || phase === 'scanning') return;
    try {
      videoEl.pause();
    } catch {
      /* 忽略 */
    }
    teardownMedia();
    currentVideoPath = null;
    prunePositionsUnder(root);

    root = null;
    flat = [];
    lineOfIdx = [];
    subsByDir = new Map();
    lines = [];
    cursorIdx = 0;
    currentPage = 0;
    view = { rel: '', dir: '', name: '', pos: 0, total: 0 };
    curTime = 0;
    duration = 0;
    playing = false;
    phase = 'hint';
    phaseMessage = '';
    setStatus('已关闭当前目录（位置记忆已清除）');
    win.setTitle('极速看片').catch(() => {});
    clearTimeout(saveTimer); // 防抖中的保存作废（避免把旧位置写回）
    try {
      await invoke('save_state', buildStatePayload(true));
    } catch {
      /* 保存失败不影响回到引导页 */
    }
  }

  /* ============================== 全屏 ============================== */

  /* 全屏即纯画面模式：fullscreen 一变，infoShown / ctrlShown /
     listShown 三个派生值同时归 false —— 顶部信息栏、底部控制栏、
     右侧文件列表浮层立即全部隐藏，屏幕上只留视频画面。
     退出全屏：三个浮层按 I / C / T 偏好自动恢复，无需其它处理。 */
  async function toggleFullscreen() {
    const want = !fullscreen;
    fullscreen = want;
    let ok = false;
    try {
      await win.setFullscreen(want);
      ok = true;
    } catch {
      ok = false;
    }
    if (!ok) {
      // DOM 全屏兜底（部分平台 / 嵌入场景）
      try {
        if (want) {
          await document.documentElement.requestFullscreen();
          domFsActive = true;
        } else if (document.fullscreenElement) {
          await document.exitFullscreen();
        }
      } catch {
        /* 忽略 */
      }
    }
  }

  let domFsActive = false; // DOM 全屏兜底是否在使用中（退出同步用）

  function onDomFullscreenChange() {
    // 仅同步 DOM 全屏的退出（Tauri 窗口全屏不触发此事件）。
    // fullscreen 一变，三个浮层的显隐由派生值自动接管
    if (!document.fullscreenElement && domFsActive) {
      fullscreen = false;
      domFsActive = false;
    } else if (document.fullscreenElement) {
      domFsActive = true;
    }
  }

  /* ============================== 事件处理 ============================== */

  async function exitApp() {
    await saveStateNow();
    try {
      await win.destroy();
    } catch {
      try {
        await invoke('exit_app');
      } catch {
        /* 忽略 */
      }
    }
  }

  function onKeydown(e) {
    if (e.isComposing) return; // 中文输入法组词中不响应
    const key = e.key;

    // 帮助层打开：任意键关闭（不打断播放）
    if (helpVisible) {
      e.preventDefault();
      helpVisible = false;
      return;
    }

    switch (key) {
      case 'ArrowRight':
        e.preventDefault();
        seekBy(SEEK_BIG);
        break;
      case 'ArrowLeft':
        e.preventDefault();
        seekBy(-SEEK_BIG);
        break;
      case 'ArrowUp':
        e.preventDefault();
        setVolume(volume + VOL_STEP);
        break;
      case 'ArrowDown':
        e.preventDefault();
        setVolume(volume - VOL_STEP);
        break;
      case ' ':
      case 'Enter':
        e.preventDefault();
        togglePlay();
        break;
      case 'PageDown':
        e.preventDefault();
        manualNext();
        break;
      case 'PageUp':
        e.preventDefault();
        manualPrev();
        break;
      case 'Home':
        e.preventDefault();
        if (phase === 'view') seekTo(0); // 本文件开头
        break;
      case 'End':
        e.preventDefault();
        if (phase === 'view' && videoEl && isFinite(videoEl.duration)) {
          seekTo(Math.max(0, videoEl.duration - 0.05)); // 本文件结尾
        }
        break;
      case 'Escape':
        e.preventDefault();
        if (fullscreen) toggleFullscreen(); // 全屏时退出全屏
        else exitApp(); // 非全屏时退出程序
        break;
      case '+':
      case '=':
        e.preventDefault();
        zoomBy(ZOOM_STEP); // 画面放大
        break;
      case '-':
      case '_':
        e.preventDefault();
        zoomBy(-ZOOM_STEP); // 画面缩小
        break;
      case ',':
      case '，':
        e.preventDefault();
        stepFrame(-1); // 退后一帧
        break;
      case '.':
      case '。':
        e.preventDefault();
        stepFrame(1); // 前进一帧
        break;
      case 'r':
      case 'R':
        e.preventDefault();
        rotateCW();
        break;
      case 'x':
      case 'X':
        e.preventDefault();
        closeRoot(); // 已加载目录时关闭并回到引导页，否则忽略
        break;
      case 'i':
      case 'I':
        e.preventDefault();
        if (phase === 'view') {
          infoVisible = !infoVisible;
          scheduleSave();
        }
        break;
      case 'c':
      case 'C':
        e.preventDefault();
        if (phase === 'view') {
          ctrlVisible = !ctrlVisible;
          scheduleSave();
        }
        break;
      case 't':
      case 'T':
        e.preventDefault();
        if (phase === 'view') {
          listVisible = !listVisible;
          scheduleSave();
        }
        break;
      case 'F1':
        e.preventDefault();
        helpVisible = true;
        break;
    }
  }

  function onWheel(e) {
    if (phase !== 'view' || helpVisible) return;
    // 控制栏 / 弹出菜单内：不调节音量（滑杆自身交互优先）
    if (e.target && e.target.closest && (e.target.closest('.ctrlbar') || e.target.closest('.menu'))) return;
    e.preventDefault();
    setVolume(volume + (e.deltaY < 0 ? VOL_STEP : -VOL_STEP));
  }

  function preventContextMenu(e) {
    e.preventDefault();
  }

  /* ============================== 生命周期 ============================== */

  /** 断点定位（loadedmetadata 或 MSE 装载完成后消费） */
  async function applyPendingSeek() {
    if (!pendingSeek || !videoEl) return;
    const { idx, sec, autoplay } = pendingSeek;
    if (idx !== cursorIdx) {
      pendingSeek = null;
      return;
    }
    const dur = isFinite(videoEl.duration) ? videoEl.duration : 0;
    if (sec > 0.05 && (dur === 0 || sec < dur - 0.5)) {
      if (mediaMode === 'mse' && engine && engine.ready) {
        const adj = await engine.seekTo(sec);
        if (videoEl) videoEl.currentTime = isFinite(adj) ? adj : sec;
      } else {
        videoEl.currentTime = sec; // 断点定位
      }
      curTime = videoEl.currentTime;
    }
    if (autoplay) tryPlay();
    pendingSeek = null;
  }

  /** rVFC 帧率估算（播放期间采样 ~1 秒） */
  function startFpsProbe() {
    if (!videoEl || fpsKnown || typeof videoEl.requestVideoFrameCallback !== 'function') return;
    const cb = (now) => {
      if (!videoEl) return;
      if (videoEl.paused) {
        rvfcCount = 0;
        videoEl.requestVideoFrameCallback(cb);
        return;
      }
      if (rvfcCount === 0) rvfcStart = now;
      rvfcCount++;
      if (rvfcCount >= 24) {
        const f = rvfcCount / ((now - rvfcStart) / 1000);
        if (f > 5 && f < 300) fpsKnown = Math.round(f * 100) / 100;
        return;
      }
      videoEl.requestVideoFrameCallback(cb);
    };
    videoEl.requestVideoFrameCallback(cb);
  }

  /* 播放中防止屏保 / 系统休眠：phase 或 playing 变化时同步持锁 /
     解锁（详见 lib/wakelock.js）；组件销毁时在 onDestroy 兜底解锁 */
  $effect(() => {
    setKeepAwake(phase === 'view' && playing);
  });

  $effect(() => {
    if (phase !== 'view') return;
    const el = listEl;
    if (!el) return;
    const measure = () => {
      listH = el.clientHeight;
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  });

  // 行高 / 页大小变化后，把当前页重新锚定到播放文件所在页
  $effect(() => {
    void pageSize;
    void pageCount;
    if (phase !== 'view' || !lineOfIdx.length) return;
    const ln = lineOfIdx[Math.min(cursorIdx, lineOfIdx.length - 1)] ?? 0;
    currentPage = Math.min(Math.floor(ln / pageSize), Math.max(0, pageCount - 1));
  });

  onMount(() => {
    // videoEl 由 VideoStage 的 registerVideo 回调注入；保险起见兜底一次
    if (!videoEl) videoEl = document.querySelector('.stage video');
    if (!videoEl) return;
    subManager = new SubtitleManager(videoEl);
    // ---- <video> 事件 ----
    videoEl.volume = volume;

    videoEl.addEventListener('loadedmetadata', () => {
      duration = isFinite(videoEl.duration) ? videoEl.duration : 0;
      if (!mseLoading) applyPendingSeek(); // MSE 模式由 loadMedia 统一定位
    });
    videoEl.addEventListener('timeupdate', () => {
      const t = videoEl.currentTime;
      if (Math.abs(t - curTime) > 0.05) curTime = t;
      if (engine) engine.tick(); // 分段节流恢复
      scheduleSave();
    });
    videoEl.addEventListener('play', () => {
      playing = true;
      startFpsProbe();
    });
    videoEl.addEventListener('pause', () => {
      playing = false;
      scheduleSave();
    });
    videoEl.addEventListener('ended', onEnded);
    videoEl.addEventListener('error', () => {
      if (phase !== 'view' || !videoEl.getAttribute('src')) return;
      playing = false;
      setStatus('无法播放该文件（系统 WebView 不支持此格式）');
    });

    // ---- 全局键鼠 ----
    window.addEventListener('keydown', onKeydown);
    window.addEventListener('wheel', onWheel, { passive: false });
    window.addEventListener('contextmenu', preventContextMenu);
    document.addEventListener('fullscreenchange', onDomFullscreenChange);

    // ---- 播放期间防屏保 / 休眠（Wake Lock + 后备，见 lib/wakelock.js） ----
    initWakeLock();

    // ---- 拖拽目录（Tauri 原生 drag & drop 事件）----
    (async () => {
      try {
        unlistenDrop = await win.onDragDropEvent((ev) => {
          const p = ev.payload;
          if (p.type === 'enter' || p.type === 'over') {
            dragActive = true;
          } else if (p.type === 'leave') {
            dragActive = false;
          } else if (p.type === 'drop') {
            dragActive = false;
            helpVisible = false;
            if (p.paths && p.paths.length > 0) {
              openRoot(p.paths[0]);
            }
          }
        });
      } catch (e) {
        console.error('drag-drop 监听失败', e);
      }
    })();

    // ---- 关闭窗口前保证位置已保存 ----
    (async () => {
      try {
        unlistenClose = await win.onCloseRequested(async (ev) => {
          try {
            ev.preventDefault();
          } catch {
            /* 旧版本 API 可能没有 preventDefault */
          }
          await saveStateNow();
          try {
            await win.destroy();
          } catch {
            /* 忽略 */
          }
        });
      } catch (e) {
        console.error('close 监听失败', e);
      }
    })();

    // ---- 恢复上次会话（断点续看：定位到退出位置并暂停）----
    (async () => {
      try {
        const st = await invoke('load_state');
        if (st && st.root) {
          if (typeof st.volume === 'number') {
            volume = clamp(st.volume, 0, 1);
            videoEl.volume = volume;
          }
          if (typeof st.rotation === 'number') rotation = ((st.rotation % 360) + 360) % 360;
          if (typeof st.zoom === 'number' && st.zoom >= 1) zoom = clamp(st.zoom, 1, ZOOM_MAX);
          if (typeof st.audio_lang === 'string' && st.audio_lang) audioLangPref = st.audio_lang;
          if (typeof st.subtitle_lang === 'string' && st.subtitle_lang) subLangPref = st.subtitle_lang;
          if (typeof st.info_visible === 'boolean') infoVisible = st.info_visible;
          if (typeof st.ctrl_visible === 'boolean') ctrlVisible = st.ctrl_visible;
          if (typeof st.list_visible === 'boolean') listVisible = st.list_visible;
          await openRoot(st.root, st);
        }
      } catch (e) {
        console.error('恢复会话失败', e);
      }
    })();
  });

  onDestroy(() => {
    window.removeEventListener('keydown', onKeydown);
    window.removeEventListener('wheel', onWheel);
    window.removeEventListener('contextmenu', preventContextMenu);
    document.removeEventListener('fullscreenchange', onDomFullscreenChange);
    if (unlistenDrop) unlistenDrop();
    if (unlistenClose) unlistenClose();
    setKeepAwake(false); // 兜底：组件销毁时解除保持唤醒
    clearTimeout(saveTimer);
    clearTimeout(statusTimer);
  });
</script>

<!-- 主舞台：整幅纯黑画面舞台 + 左上角白色文件名（固定排版）。
     VideoStage 常驻挂载：<video> 元素全生命周期唯一，引擎 / HLS 
     挂载后不会被替换；非 view 阶段被引导屏盖住 -->
<div
  class="stage"
  class:pad-top={phase === 'view' && infoShown}
  class:pad-bottom={phase === 'view' && ctrlShown}
  class:pad-right={phase === 'view' && listShown}
>
  <VideoStage
    fileName={view.name}
    fileDir={view.dir}
    {rotation}
    {zoom}
    registerVideo={(el) => (videoEl = el)}
    onplaypause={togglePlay}
    onfullscreen={toggleFullscreen}
  />
</div>

<!-- 右侧文件列表浮层（T 键显隐，默认显示）：黑色背景亮灰文字，
     独立于主窗口；顶/底分别让位给信息栏与控制栏 -->
{#if phase === 'view' && listShown}
  <div class="list-overlay" class:pad-top={infoShown} class:pad-bottom={ctrlShown}>
    <div class="list-body" bind:this={listEl} aria-label="文件列表">
      <FileList
        lines={pageLines}
        playingIdx={cursorIdx}
        {positions}
        rowHeight={LIST_ROW_H}
        onplay={(idx) => playIndex(idx)}
      />
    </div>
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div class="page-indicator tnum" onclick={nextPage} title="点击翻页">
      第 {Math.min(currentPage + 1, pageCount)} / {pageCount} 页 · 点击翻页
    </div>
  </div>
{/if}

{#if phase !== 'view'}
  <HintScreen {phase} message={phaseMessage} {dragActive} onpick={pickDirectory} />
{/if}

{#if phase === 'view' && infoShown}
  <InfoBar
    rel={view.rel}
    pos={view.pos}
    total={view.total}
    {curTime}
    {duration}
    audioLabel={audioLabel}
    subLabel={subValue === 'off' ? '' : subLabel}
    {rotation}
    {zoom}
  />
{/if}

{#if phase === 'view' && ctrlShown}
  <ControlBar
    {playing}
    {curTime}
    {duration}
    {volume}
    {muted}
    {rotation}
    {fullscreen}
    {audioOptions}
    audioValue={audioValue}
    audioDisabled={audioOptions.length <= 1}
    {subOptions}
    subValue={subValue}
    subDisabled={subOptions.length <= 1}
    ontoggle={togglePlay}
    onprev={manualPrev}
    onnext={manualNext}
    onseek={seekTo}
    onseekby={seekBy}
    onvolume={setVolume}
    onmute={toggleMute}
    onrotate={rotateCW}
    onfullscreen={toggleFullscreen}
    onaudio={selectAudio}
    onsubtitle={selectSubtitle}
  />
{/if}

{#if status}
  <div class="toast">{status}</div>
{/if}

{#if helpVisible}
  <HelpOverlay onclose={() => (helpVisible = false)} />
{/if}

<style>
  .stage {
    position: fixed;
    inset: 0;
    background: #000; /* 纯黑：与列表浮层 / 信息栏 / 控制栏一致的黑色主题 */
    color: #fff;
    overflow: hidden;
    font-size: 20px; /* 固定排版 */
    font-family: 'Microsoft YaHei', 'PingFang SC', 'Noto Sans SC', sans-serif;
  }
  /* 顶 / 底 / 右 Overlay 可见时给主内容留出空间，
     隐藏时画面自动占满（列表浮层隐藏后画面变宽） */
  .stage.pad-top {
    padding-top: 34px;
  }
  .stage.pad-bottom {
    padding-bottom: 104px;
  }
  .stage.pad-right {
    padding-right: min(380px, 38vw);
  }

  /* 右侧文件列表浮层：黑色背景亮灰文字，独立于主窗口 */
  .list-overlay {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(380px, 38vw);
    background: #0b0b0e;
    border-left: 1px solid rgba(255, 255, 255, 0.1);
    z-index: 45; /* 信息栏(50) / 控制栏(60) 之下：顶底让位，不遮挡 */
    display: flex;
    flex-direction: column;
  }
  .list-overlay.pad-top {
    top: 34px;
  }
  .list-overlay.pad-bottom {
    bottom: 104px;
  }
  .list-body {
    flex: 1 1 auto;
    min-height: 0;
    overflow: hidden;
  }
  .page-indicator {
    flex: none;
    padding: 7px 12px;
    border-top: 1px solid rgba(255, 255, 255, 0.07);
    font-size: 11px;
    color: rgba(255, 255, 255, 0.35);
    text-align: right;
    cursor: pointer; /* 回车在本程序里是播放 / 暂停，翻页用点击 */
    font-family: 'Segoe UI', 'Microsoft YaHei', 'PingFang SC', 'Noto Sans SC', sans-serif;
  }
  .page-indicator:hover {
    color: rgba(255, 255, 255, 0.7);
  }
</style>
