<script>
  /**
   * 视频舞台（主窗口画面区）。
   *
   * - <video> 按原始宽高「包含式」铺满舞台（等比缩放、居中、不裁切），
   *   元素尺寸精确等于画面显示尺寸——这样 rotate / scale / translate
   *   的变换数学都以画面自身为中心，旋转 90° 后也不会出现空角；
   * - 变换链：transform: rotate(R) scale(S) translate(P)，鼠标拖拽的
   *   屏幕位移先旋转回画面坐标系（R⁻¹）再除以缩放（S）得到 P 的
   *   增量，任意旋转角下拖拽方向都跟手；
   * - 平移被钳制在「画面边缘不脱离舞台」的范围内；
   * - 单击 = 播放 / 暂停，双击 = 全屏 / 恢复（260ms 计时器区分），
   *   放大后的拖拽不触发单击；任何缩放级别下点击都有效
   *   （press 始终记录，仅放大时额外开启拖拽平移）；
   * - 左上角白色文件名块（固定排版，与听书舞台同款），不拦截鼠标。
   */
  import { formatZoom } from '../lib/format.js';
  import { untrack } from 'svelte';

  let {
    fileName = '',
    fileDir = '',
    rotation = 0,        // 0 / 90 / 180 / 270（顺时针）
    zoom = 1,            // 1 = 原始大小
    registerVideo = null, // (el) => void：把 <video> 交给 App 管理
    onplaypause = null,
    onfullscreen = null
  } = $props();

  let stageEl = $state(null);
  let videoEl = $state(null);
  let box = $state({ w: 0, h: 0 });   // 画面的包含式显示尺寸（px）
  let stage = { w: 0, h: 0 };          // 舞台可用区（非响应式，测量用）
  let pan = $state({ x: 0, y: 0 });    // 平移（画面坐标系，px）
  let dragging = $state(false);        // 拖拽中（驱动 transform 过渡开关）
  let showGrab = $state(false);

  /* ---------- 把 <video> 交给 App ---------- */
  $effect(() => {
    if (videoEl && typeof registerVideo === 'function') registerVideo(videoEl);
  });

  /* ---------- 包含式尺寸：画面宽高 / 舞台宽高 → 显示盒 ---------- */
  function measure() {
    if (!stageEl || !videoEl) return;
    stage.w = stageEl.clientWidth;
    stage.h = stageEl.clientHeight;
    const vw = videoEl.videoWidth || 0;
    const vh = videoEl.videoHeight || 0;
    if (!vw || !vh || !stage.w || !stage.h) {
      box = { w: 0, h: 0 };
      return;
    }
    const k = Math.min(stage.w / vw, stage.h / vh);
    box = { w: Math.round(vw * k), h: Math.round(vh * k) };
    clampPan();
  }

  $effect(() => {
    const el = stageEl;
    if (!el) return;
    const ro = new ResizeObserver(() => measure());
    ro.observe(el);
    measure();
    return () => ro.disconnect();
  });

  $effect(() => {
    if (!videoEl) return;
    const onMeta = () => measure();
    videoEl.addEventListener('loadedmetadata', onMeta);
    videoEl.addEventListener('resize', onMeta);
    return () => {
      videoEl.removeEventListener('loadedmetadata', onMeta);
      videoEl.removeEventListener('resize', onMeta);
    };
  });

  /* ---------- 平移钳制：画面边缘不脱离舞台 ---------- */
  function extent() {
    // 画面经 rotate×scale 后在屏幕上占据的包围盒
    const rotated = rotation % 180 !== 0;
    const w = (rotated ? box.h : box.w) * zoom;
    const h = (rotated ? box.w : box.h) * zoom;
    return { w, h };
  }

  function clampPan() {
    const e = extent();
    const maxX = Math.max(0, (e.w - stage.w) / 2) / zoom;
    const maxY = Math.max(0, (e.h - stage.h) / 2) / zoom;
    const nx = Math.max(-maxX, Math.min(maxX, pan.x));
    const ny = Math.max(-maxY, Math.min(maxY, pan.y));
    // 值不变不写：避免 effect 读 pan 又写 pan 的自触发循环
    if (nx !== pan.x || ny !== pan.y) pan = { x: nx, y: ny };
  }

  // 缩放 / 旋转 / 画面尺寸变化后重新钳制；回到 1× 时平移归零。
  // clampPan 会读 pan —— 用 untrack 隔离依赖，防止自触发循环
  $effect(() => {
    void zoom;
    void rotation;
    void box.w;
    void box.h;
    if (zoom <= 1.0001) {
      if (pan.x !== 0 || pan.y !== 0) pan = { x: 0, y: 0 };
    } else {
      untrack(() => clampPan());
    }
  });

  const transformStyle = $derived(
    `rotate(${rotation}deg) scale(${zoom}) translate(${pan.x.toFixed(2)}px, ${pan.y.toFixed(2)}px)`
  );

  /* ---------- 拖拽平移（放大后）：屏幕位移 → 画面坐标系 ---------- */
  let press = null; // { x, y, panX, panY, moved }

  function onPointerDown(e) {
    if (!onplaypause && !onfullscreen) return;
    if (e.button !== 0) return;
    // 任何缩放级别都记录按下（pointerup 时区分「点击」与「拖拽」）；
    // 只有放大后才开启拖拽平移与指针捕获。
    // 此前 press 仅在放大时记录，导致默认缩放下单击 / 双击完全失效。
    press = { x: e.clientX, y: e.clientY, panX: pan.x, panY: pan.y, moved: false };
    if (zoom > 1.0001) {
      dragging = true;
      showGrab = true;
      try { stageEl.setPointerCapture(e.pointerId); } catch { /* 忽略 */ }
    }
  }

  function onPointerMove(e) {
    if (!press) return;
    const dx = e.clientX - press.x;
    const dy = e.clientY - press.y;
    if (Math.abs(dx) + Math.abs(dy) > 4) press.moved = true;
    if (!press.moved) return;
    if (zoom <= 1.0001) return; // 未放大：位移只用于取消点击判定，不平移
    // 屏幕位移 d → 画面位移 e = R⁻¹·d / S（顺时针旋转 R 后的逆变换）
    let ex = 0;
    let ey = 0;
    const r = ((rotation % 360) + 360) % 360;
    if (r === 0) { ex = dx; ey = dy; }
    else if (r === 90) { ex = dy; ey = -dx; }
    else if (r === 180) { ex = -dx; ey = -dy; }
    else { ex = -dy; ey = dx; } // 270
    pan = { x: press.panX + ex / zoom, y: press.panY + ey / zoom };
    clampPan();
  }

  function onPointerUp(e) {
    if (press && !press.moved) {
      handleClick(); // 未拖拽 → 单击 / 双击判定
    }
    press = null;
    dragging = false;
    showGrab = false;
    try { stageEl?.releasePointerCapture?.(e.pointerId); } catch { /* 忽略 */ }
  }

  /* ---------- 单击 / 双击区分（260ms 计时器） ---------- */
  let clickTimer = 0;

  function handleClick() {
    if (clickTimer) {
      // 第二次点击：双击 → 全屏
      clearTimeout(clickTimer);
      clickTimer = 0;
      onfullscreen?.();
    } else {
      clickTimer = setTimeout(() => {
        clickTimer = 0;
        onplaypause?.();
      }, 260);
    }
  }

  $effect(() => () => clearTimeout(clickTimer));
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div
  class="stage"
  bind:this={stageEl}
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  role="presentation"
>
  <!-- 左上角文件名：固定排版，不拦截鼠标 -->
  <div class="now-title" aria-hidden="true">
    <div class="now-name" title={fileName}>{fileName || '—'}</div>
    {#if fileDir}
      <div class="now-rel" title={fileDir}>{fileDir}/</div>
    {/if}
  </div>

  <!-- 缩放 / 旋转指示：非默认时右下角轻提示 -->
  {#if rotation !== 0 || zoom !== 1}
    <div class="hud tnum">{formatZoom(zoom)}{rotation !== 0 ? ` · ${rotation}°` : ''}</div>
  {/if}

  <!-- 唯一的 <video>：元数据就绪前占满舞台等待首帧；就绪后按
       包含式显示盒设定尺寸并挂上 rotate/scale/translate 变换。
       始终是同一个元素（MSE / HLS 挂载后不能被替换） -->
  <!-- svelte-ignore a11y_media_has_caption -->
  <video
    bind:this={videoEl}
    class:waiting={box.w === 0}
    class:pannable={zoom > 1.0001}
    class:grabbing={showGrab}
    class:anim={!dragging && box.w > 0}
    style:width={box.w > 0 ? `${box.w}px` : '100%'}
    style:height={box.w > 0 ? `${box.h}px` : '100%'}
    style:transform={box.w > 0 ? transformStyle : 'none'}
    preload="auto"
    playsinline
  ></video>
</div>

<style>
  .stage {
    position: relative;   /* 相对定位：跟随父舞台（App）的内容盒，
                             顶/底/右 Overlay 可见时自动让位 */
    width: 100%;
    height: 100%;
    background: #000; /* 纯黑舞台：与各浮层一致的黑色主题 */
    display: flex;
    align-items: center;
    justify-content: center;
    overflow: hidden;
    touch-action: none; /* 拖拽平移自己接管 */
  }
  video {
    display: block;
    background: #000;
    outline: none;
  }
  video.waiting {
    visibility: hidden; /* 元数据未就绪：占位但不可见 */
  }
  video.pannable {
    cursor: grab;
  }
  video.grabbing {
    cursor: grabbing;
  }
  /* 拖拽中不做 transform 过渡（跟手），缩放 / 旋转时平滑过渡 */
  video.anim {
    transition: transform 0.16s ease;
  }
  video:not(.pannable) {
    cursor: default;
  }

  /* 左上角文件名块：固定排版（20 号 / 0.4 行高），白色 */
  .now-title {
    position: absolute;
    top: 14px;
    left: 18px;
    right: 18px;
    z-index: 2;
    pointer-events: none; /* 不拦截画面点击 */
    display: flex;
    flex-direction: column;
    gap: 8px; /* 20 号字 × 0.4 行高 */
    overflow: hidden;
    font-family: 'Microsoft YaHei', 'PingFang SC', 'Noto Sans SC', sans-serif;
  }
  .now-name {
    font-size: 20px;
    color: #fff; /* 白色：适应纯黑舞台 */
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-shadow: 0 1px 3px rgba(0, 0, 0, 0.8);
  }
  .now-rel {
    font-size: 12.4px; /* 20 号字 × 0.62 */
    color: rgba(255, 255, 255, 0.55); /* 半透白的路径小字 */
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    text-shadow: 0 1px 3px rgba(0, 0, 0, 0.8);
  }

  /* 右下角缩放 / 旋转 HUD */
  .hud {
    position: absolute;
    right: 18px;
    bottom: 12px;
    z-index: 2;
    pointer-events: none;
    font-size: 12.5px;
    color: rgba(255, 255, 255, 0.6);
    background: rgba(0, 0, 0, 0.55);
    border: 1px solid rgba(255, 255, 255, 0.16);
    border-radius: 6px;
    padding: 3px 9px;
    letter-spacing: 0.5px;
  }
</style>
