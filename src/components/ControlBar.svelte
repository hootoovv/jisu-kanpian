<script>
  /**
   * 底部控制栏（C 键切换显隐，Overlay 覆盖在内容上方）。
   *
   * 布局与极速听书同款（两行式）：
   *   最左：播放 / 暂停大按钮（垂直居中，跨两行）；
   *   右侧上半行（靠右对齐）：上一文件 / 下一文件、快退 / 快进 30 秒、
   *     音轨选择（弹出式菜单，单音轨时禁用）、字幕选择（弹出式菜单，
   *     含「不启用」，无字幕流时禁用）、旋转（顺时针 90°）、全屏、
   *     音量图标（点击静音 / 解除）+ 音量 slider；
   *   右侧下半行：播放位置 slider（占满可用宽度，已播部分绿色填充）+
   *     播放位置 / 总时长文字（同行最右侧）。
   *
   * 音轨 / 字幕的弹出菜单：向上弹出、点击外部关闭、当前项打勾。
   * 拖动进度条时本地接管滑块显示（不被 rAF 的进度回写打断），
   * 松手后回到「跟随播放」状态。
   */
  import { formatTime } from '../lib/format.js';

  let {
    playing = false,
    curTime = 0,
    duration = 0,
    volume = 1,
    muted = false,
    rotation = 0,
    fullscreen = false,
    audioOptions = [],   // [{ key, label, disabled? }]
    audioValue = '',     // 当前选中的 key
    audioDisabled = true,
    subOptions = [],     // [{ key, label, disabled? }]
    subValue = 'off',
    subDisabled = true,
    ontoggle = null,
    onprev = null,
    onnext = null,
    onseek = null,
    onseekby = null,     // (±30)
    onvolume = null,
    onmute = null,
    onrotate = null,
    onfullscreen = null,
    onaudio = null,      // (key)
    onsubtitle = null    // (key)
  } = $props();

  // 拖动进度条期间本地接管滑块显示（不被 rAF 的进度回写打断）
  let dragging = $state(false);
  let dragVal = $state(0);
  // 弹出菜单：'audio' | 'sub' | null
  let openMenu = $state(null);

  const audioLabel = $derived(
    (audioOptions.find((o) => o.key === audioValue) || {}).label || '默认'
  );
  const subLabel = $derived(
    subValue === 'off' ? '不启用' : (subOptions.find((o) => o.key === subValue) || {}).label || ''
  );
  const seekFill = $derived(
    duration > 0 ? Math.min(100, (curTime / duration) * 100).toFixed(2) : 0
  );

  function onSeekInput(e) {
    dragging = true;
    dragVal = Number(e.target.value);
    onseek?.(dragVal);
  }
  function onSeekEnd(e) {
    dragging = false;
    onseek?.(Number(e.target.value));
  }
  function onVolInput(e) {
    onvolume?.(Number(e.target.value));
  }

  function toggleMenu(which) {
    openMenu = openMenu === which ? null : which;
  }
  function closeMenu() {
    openMenu = null;
  }
  function pickAudio(key) {
    openMenu = null;
    const opt = audioOptions.find((o) => o.key === key);
    if (!opt || opt.disabled) return; // 不支持编码的轨：置灰不可选（见 App 层标注）
    if (key !== audioValue) onaudio?.(key);
  }
  function pickSub(key) {
    openMenu = null;
    const opt = subOptions.find((o) => o.key === key);
    if (!opt || opt.disabled) return; // 图形字幕等：置灰不可选
    if (key !== subValue) onsubtitle?.(key);
  }
</script>

<!-- 点击空白处关闭弹出菜单 -->
<svelte:window onclick={closeMenu} />

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="ctrlbar" onclick={(e) => e.stopPropagation()} role="presentation">
  <!-- 播放 / 暂停：最左侧放大按钮（跨两行垂直居中） -->
  <button
    class="play"
    type="button"
    onclick={() => ontoggle?.()}
    aria-label={playing ? '暂停' : '播放'}
    title={playing ? '暂停（空格）' : '播放（空格）'}
  >
    {#if playing}
      <svg viewBox="0 0 24 24" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1.2" /><rect x="14" y="5" width="4" height="14" rx="1.2" /></svg>
    {:else}
      <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5.5v13l11-6.5z" /></svg>
    {/if}
  </button>

  <div class="stack">
    <!-- 上半行：导航 / 快进退 / 音轨 / 字幕 / 旋转 / 全屏 / 音量（靠右） -->
    <div class="row row-top">
      <button class="ic-btn" type="button" onclick={() => onprev?.()} title="上一个文件（PageUp）" aria-label="上一个文件">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 5h2.4v14H6zM19.5 5.8v12.4L9.8 12z" /></svg>
      </button>
      <button class="ic-btn" type="button" onclick={() => onnext?.()} title="下一个文件（PageDown）" aria-label="下一个文件">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M15.6 5H18v14h-2.4zM4.5 5.8v12.4l9.7-6.2z" /></svg>
      </button>

      <button class="ic-btn" type="button" onclick={() => onseekby?.(-30)} title="快退 30 秒（←）" aria-label="快退 30 秒">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5V1L7 6l5 5V7a6 6 0 1 1-6 6H4a8 8 0 1 0 8-8z" /></svg>
      </button>
      <button class="ic-btn" type="button" onclick={() => onseekby?.(30)} title="快进 30 秒（→）" aria-label="快进 30 秒">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5V1l5 5-5 5V7a6 6 0 1 0 6 6h2a8 8 0 1 1-8-8z" /></svg>
      </button>

      <span class="gap"></span>

      <!-- 音轨选择：弹出式菜单（单音轨禁用） -->
      <div class="menu-host">
        <button
          class="pill"
          class:active={openMenu === 'audio'}
          class:off={audioDisabled}
          type="button"
          disabled={audioDisabled}
          onclick={() => toggleMenu('audio')}
          title={audioDisabled ? '本文件只有一条音轨' : '选择音轨'}
          aria-label="选择音轨"
          aria-haspopup="menu"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4zm12.5 3a4.5 4.5 0 0 0-2.5-4v8a4.5 4.5 0 0 0 2.5-4zM14 2.2v2.1c3.1.7 5.4 3.4 5.4 6.7s-2.3 6-5.4 6.7v2.1c4.2-.8 7.4-4.5 7.4-8.8s-3.2-8-7.4-8.8z" /></svg>
          <span class="pill-txt">音轨 · {audioLabel}</span>
          <svg class="caret" viewBox="0 0 24 24" aria-hidden="true"><path d="M7 10l5 5 5-5z" /></svg>
        </button>
        {#if openMenu === 'audio'}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <div class="menu" role="menu">
            {#each audioOptions as opt (opt.key)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="menu-item"
                class:sel={opt.key === audioValue}
                class:dim={opt.disabled}
                role="menuitem"
                tabindex="-1"
                title={opt.disabled ? opt.hint || '当前 WebView 不支持该编码' : ''}
                onclick={() => pickAudio(opt.key)}
              >
                <span class="check">{opt.key === audioValue ? '✓' : ''}</span>
                <span class="menu-label">{opt.label}</span>
              </div>
            {/each}
          </div>
        {/if}
      </div>

      <!-- 字幕选择：弹出式菜单（含「不启用」，无字幕流禁用） -->
      <div class="menu-host">
        <button
          class="pill"
          class:active={openMenu === 'sub'}
          class:off={subDisabled}
          type="button"
          disabled={subDisabled}
          onclick={() => toggleMenu('sub')}
          title={subDisabled ? '本文件没有可用的字幕流' : '选择字幕'}
          aria-label="选择字幕"
          aria-haspopup="menu"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 5h18v14H3zm2 2v10h14V7zm2 3h5v1.6H7zm0 3.4h8v1.6H7zm9-3.4h1.5v1.6H16zm-2 3.4h3.5v1.6H14z" /></svg>
          <span class="pill-txt">字幕 · {subValue === 'off' ? '关' : subLabel}</span>
          <svg class="caret" viewBox="0 0 24 24" aria-hidden="true"><path d="M7 10l5 5 5-5z" /></svg>
        </button>
        {#if openMenu === 'sub'}
          <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
          <div class="menu" role="menu">
            {#each subOptions as opt (opt.key)}
              <!-- svelte-ignore a11y_click_events_have_key_events -->
              <!-- svelte-ignore a11y_no_static_element_interactions -->
              <div
                class="menu-item"
                class:sel={opt.key === subValue}
                class:dim={opt.disabled}
                role="menuitem"
                tabindex="-1"
                title={opt.disabled ? opt.hint || '不支持的字幕格式' : ''}
                onclick={() => pickSub(opt.key)}
              >
                <span class="check">{opt.key === subValue ? '✓' : ''}</span>
                <span class="menu-label">{opt.label}</span>
              </div>
            {/each}
          </div>
        {/if}
      </div>

      <!-- 旋转：每次顺时针 90° -->
      <button
        class="pill"
        type="button"
        onclick={() => onrotate?.()}
        title="顺时针旋转 90°（R）"
        aria-label="旋转画面"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M16.5 3l1.8 1.8a9 9 0 1 0 2 6.7h-2.1a7 7 0 1 1-1.7-4.9L18.6 9H14V3h2.5z" /></svg>
        <span class="pill-txt tnum">{rotation}°</span>
      </button>

      <!-- 全屏 -->
      <button
        class="ic-btn"
        type="button"
        onclick={() => onfullscreen?.()}
        title={fullscreen ? '退出全屏（Esc）' : '全屏（双击画面）'}
        aria-label={fullscreen ? '退出全屏' : '全屏'}
      >
        {#if fullscreen}
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 8H4V5.5h1.5V4H8v4zm8 8h4v2.5h-1.5V20H16v-4zM8 16H4v-2.5h1.5V12H8v4zm8-8h4v2.5h-1.5V12H16V8z" /></svg>
        {:else}
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 4h6v1.5H5.5V10H4zM14 4h6v6h-1.5V5.5H14zM4 14h1.5v4.5H10V20H4zM18.5 14H20v6h-6v-1.5h4.5z" /></svg>
        {/if}
      </button>

      <span class="gap"></span>

      <div class="vol" class:muted>
        <!-- 音量图标：点击静音（换静音图标），再点解除 -->
        <button
          class="mute-btn"
          type="button"
          onclick={() => onmute?.()}
          aria-label={muted ? '解除静音' : '静音'}
          title={muted ? '点击解除静音' : '点击静音'}
        >
          {#if muted}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4zm14.7 3 2.1-2.1-1.4-1.4-2.1 2.1-2.1-2.1-1.4 1.4 2.1 2.1-2.1 2.1 1.4 1.4 2.1-2.1 2.1 2.1 1.4-1.4-2.1-2.1z" /></svg>
          {:else if volume >= 0.5}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4zm12.5 3A4.5 4.5 0 0 0 14 8v8a4.5 4.5 0 0 0 2.5-4zM14 2.2v2.1c3.1.7 5.4 3.4 5.4 6.7s-2.3 6-5.4 6.7v2.1c4.2-.8 7.4-4.5 7.4-8.8s-3.2-8-7.4-8.8z" /></svg>
          {:else if volume > 0}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4zm12.5 3A4.5 4.5 0 0 0 14 8v8a4.5 4.5 0 0 0 2.5-4z" /></svg>
          {:else}
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 9v6h4l5 4V5L8 9H4z" /></svg>
          {/if}
        </button>
        <input
          class="vslider"
          type="range"
          min="0"
          max="1"
          step="0.01"
          value={volume}
          oninput={onVolInput}
          onchange={(e) => e.target.blur()}
          aria-label="音量"
        />
      </div>
    </div>

    <!-- 下半行：进度 slider 占满可用宽度（已播绿色） + 时间文字在最右 -->
    <div class="row row-bottom">
      <input
        class="seek"
        type="range"
        min="0"
        max={duration || 0}
        step="0.1"
        value={dragging ? dragVal : curTime}
        oninput={onSeekInput}
        onchange={onSeekEnd}
        onpointerup={onSeekEnd}
        aria-label="播放位置"
        disabled={!duration}
        style:background="linear-gradient(to right, #3fb950 {seekFill}%, rgba(255,255,255,0.22) {seekFill}%)"
      />
      <span class="times tnum">{formatTime(dragging ? dragVal : curTime)} / {formatTime(duration)}</span>
    </div>
  </div>
</div>

<style>
  .ctrlbar {
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    height: 104px; /* 与极速听书同款：容纳上下两行 */
    display: flex;
    align-items: center;
    gap: 22px;
    padding: 14px 22px;
    background: rgba(10, 10, 12, 0.96);
    border-top: 1px solid rgba(255, 255, 255, 0.09);
    color: #fff;
    z-index: 60;
  }

  /* ---- 播放 / 暂停：最左侧，比其他按钮尺寸要大 ---- */
  .play {
    flex: none;
    width: 60px;
    height: 60px;
    border-radius: 50%;
    border: none;
    background: #238636;
    color: #fff;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.12s ease, transform 0.12s ease;
  }
  .play:hover {
    background: #2ea043;
    transform: scale(1.05);
  }
  .play:active {
    transform: scale(0.96);
  }
  .play svg {
    width: 31px;
    height: 31px;
    fill: #fff;
  }

  /* ---- 右侧区域：上下两行 ---- */
  .stack {
    flex: 1 1 auto;
    min-width: 0;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 13px;
  }
  .row-top {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
    min-height: 38px;
  }
  .row-bottom {
    display: flex;
    align-items: center;
    gap: 14px;
  }
  .gap {
    flex: none;
    width: 10px;
    border-left: 1px solid rgba(255, 255, 255, 0.12); /* 功能分组分隔线 */
    align-self: stretch;
    margin: 2px 3px;
  }

  /* ---- 图标按钮（上一 / 下一 / 快退 / 快进 / 全屏） ---- */
  .ic-btn {
    flex: none;
    width: 34px;
    height: 34px;
    padding: 0;
    border-radius: 8px;
    border: 1px solid rgba(255, 255, 255, 0.14);
    background: rgba(255, 255, 255, 0.05);
    color: rgba(255, 255, 255, 0.72);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: all 0.12s ease;
  }
  .ic-btn svg {
    width: 17px;
    height: 17px;
    fill: currentColor;
  }
  .ic-btn:hover {
    border-color: rgba(255, 255, 255, 0.4);
    color: #fff;
    background: rgba(255, 255, 255, 0.1);
  }

  /* ---- 胶囊按钮（音轨 / 字幕 / 旋转） ---- */
  .pill {
    display: flex;
    align-items: center;
    gap: 6px;
    height: 34px;
    padding: 0 11px;
    border-radius: 999px;
    border: 1px solid rgba(255, 255, 255, 0.18);
    background: rgba(255, 255, 255, 0.05);
    color: rgba(255, 255, 255, 0.72);
    font-size: 12.5px;
    cursor: pointer;
    white-space: nowrap;
    transition: all 0.12s ease;
  }
  .pill svg {
    width: 15px;
    height: 15px;
    fill: currentColor;
  }
  .pill .caret {
    width: 13px;
    height: 13px;
    opacity: 0.7;
  }
  .pill:hover {
    border-color: rgba(255, 255, 255, 0.4);
    color: #fff;
  }
  .pill.active {
    background: rgba(46, 160, 67, 0.22);
    border-color: #2ea043;
    color: #7ee787;
  }
  .pill.off {
    opacity: 0.35;
    cursor: default;
    pointer-events: none; /* 单音轨 / 无字幕时禁用 */
  }
  .pill-txt {
    max-width: 12em;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* ---- 弹出式菜单（音轨 / 字幕） ---- */
  .menu-host {
    position: relative;
  }
  .menu {
    position: absolute;
    right: 0;
    bottom: calc(100% + 10px);
    min-width: 180px;
    max-height: 320px;
    overflow: auto;
    background: #16161d;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 10px;
    box-shadow: 0 12px 40px rgba(0, 0, 0, 0.6);
    padding: 5px;
    z-index: 5;
  }
  .menu-item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 7px;
    font-size: 13px;
    color: rgba(255, 255, 255, 0.85);
    cursor: pointer;
  }
  .menu-item:hover {
    background: rgba(255, 255, 255, 0.08);
  }
  .menu-item.sel {
    color: #7ee787; /* 选中项：绿色（与播放键呼应） */
  }
  .menu-item.dim {
    color: rgba(255, 255, 255, 0.28); /* 不支持编码的轨 / 图形字幕：置灰不可选 */
    cursor: default;
  }
  .menu-item.dim:hover {
    background: transparent;
  }
  .check {
    flex: none;
    width: 1.1em;
    font-size: 12.5px;
  }
  .menu-label {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* ---- 音量：图标可点击（静音切换）+ slider ---- */
  .vol {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .mute-btn {
    width: 30px;
    height: 30px;
    padding: 0;
    border-radius: 50%;
    border: none;
    background: transparent;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: background 0.12s ease;
  }
  .mute-btn:hover {
    background: rgba(255, 255, 255, 0.1);
  }
  .mute-btn svg {
    width: 19px;
    height: 19px;
    fill: rgba(255, 255, 255, 0.72);
    transition: fill 0.12s ease;
  }
  .mute-btn:hover svg {
    fill: #fff;
  }
  .vol.muted .mute-btn svg {
    fill: #e3b341; /* 静音时琥珀色警示 */
  }
  .vol.muted .vslider {
    opacity: 0.45; /* 静音时滑杆半透明，值保持不变便于解除后恢复 */
  }

  /* ---- 时间文字：下半行最右侧 ---- */
  .times {
    flex: none;
    font-size: 13px;
    color: rgba(255, 255, 255, 0.85);
    letter-spacing: 0.5px;
  }

  /* ---- range 滑杆统一样式 ---- */
  input[type='range'] {
    -webkit-appearance: none;
    appearance: none;
    height: 4px;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.22);
    outline: none;
    cursor: pointer;
  }
  input[type='range']:disabled {
    opacity: 0.4;
    cursor: default;
  }
  input[type='range']::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    background: #fff;
    border: none;
    cursor: pointer;
  }
  .seek {
    flex: 1 1 auto; /* 占据该行全部可用水平宽度 */
    min-width: 40px;
  }
  .vslider {
    flex: none;
    width: 92px;
  }
</style>
