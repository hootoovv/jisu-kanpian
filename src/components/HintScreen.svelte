<script>
  /**
   * 初始 / 扫描中 / 空目录 / 出错 时的引导屏。
   * hint / empty / error 状态都提供一个大号「+」按钮（选文件夹），
   * 与一个次级「打开单个文件」入口（直接播放该文件，不默认显示
   * 文件列表）。视觉沿用极速听书的引导页：黑底、亮黄点缀、居中
   * 构图；图形换成胶片 + 播放三角（看片的视觉符号）。
   */
  let { phase = 'hint', message = '', dragActive = false, onpick = null, onpickfile = null } = $props();

  /** 按钮是否可用（有回调且不在扫描中） */
  const canPick = $derived(typeof onpick === 'function' && phase !== 'scanning');
  const canPickFile = $derived(typeof onpickfile === 'function' && phase !== 'scanning');

  function handleClick(e) {
    e.stopPropagation();
    if (canPick) onpick();
  }

  function handleFileClick(e) {
    e.stopPropagation();
    if (canPickFile) onpickfile();
  }
</script>

<div class="hint" class:drag={dragActive}>
  {#if phase === 'scanning'}
    <div class="spinner"></div>
    <div class="title">{message || '正在扫描…'}</div>
  {:else}
    {#if phase === 'empty'}
      <div class="frame">
        <div class="sprocket top"><i></i><i></i><i></i><i></i></div>
        <div class="playmark" aria-hidden="true">
          <svg viewBox="0 0 24 24"><path d="M8.5 5.2v13.6L20 12z" /></svg>
        </div>
        <div class="sprocket bottom"><i></i><i></i><i></i><i></i></div>
      </div>
      <div class="title">{message || '未找到视频文件'}</div>
    {:else if phase === 'error'}
      <div class="title error">{message || '出错了'}</div>
    {:else}
      <div class="frame">
        <div class="sprocket top"><i></i><i></i><i></i><i></i></div>
        <div class="playmark" aria-hidden="true">
          <svg viewBox="0 0 24 24"><path d="M8.5 5.2v13.6L20 12z" /></svg>
        </div>
        <div class="sprocket bottom"><i></i><i></i><i></i><i></i></div>
      </div>
      <h1 class="title">极速看片</h1>
      <div class="sub">按字母顺序递归观看所有子目录 · 支持 mp4 / webm / m3u8（HLS）等格式</div>
      <div class="sub dim">多音轨 / 内嵌 + 外挂字幕 · 画面旋转缩放 · 按 F1 查看全部快捷键 · 退出自动记住进度</div>
    {/if}

    <!-- 大号 + 按钮：点击弹出系统目录选择对话框（与拖拽等效） -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <button
      type="button"
      class="pick-btn"
      class:disabled={!canPick}
      onclick={handleClick}
      disabled={!canPick}
      aria-label="选择视频文件夹"
      title="选择视频文件夹"
    >
      <span class="plus" aria-hidden="true">+</span>
      <span class="pick-label">选择视频文件夹</span>
    </button>

    <!-- 次级入口：选单个视频文件直接播放（不默认显示文件列表） -->
    <button
      type="button"
      class="file-btn"
      class:disabled={!canPickFile}
      onclick={handleFileClick}
      disabled={!canPickFile}
      aria-label="打开单个视频文件"
      title="直接播放选中的文件（列表按其所在目录构建，可随时按 T 打开）"
    >打开单个视频文件</button>

    {#if phase === 'hint'}
      <div class="sub alt">也可以把「文件夹」或「视频文件」直接拖拽到窗口任意位置</div>
    {:else}
      <div class="sub alt">选择其他文件夹 / 单个文件，或重新拖拽</div>
    {/if}
  {/if}
</div>

<style>
  .hint {
    position: fixed;
    inset: 0;
    background: #000;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 14px;
    z-index: 20;
    padding: 24px;
    text-align: center;
  }
  /* 拖拽悬停时：点亮边框提示松手即可打开 */
  .hint.drag {
    outline: 3px dashed #ffd60a;
    outline-offset: -18px;
  }

  /* 胶片框：上下齿孔 + 中央播放三角（与图标同款视觉） */
  .frame {
    position: relative;
    width: 150px;
    height: 96px;
    border: 3px solid rgba(255, 255, 255, 0.4);
    border-radius: 12px;
    overflow: hidden;
    margin-bottom: 6px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .sprocket {
    position: absolute;
    left: 10px;
    right: 10px;
    display: flex;
    justify-content: space-between;
  }
  .sprocket.top { top: 7px; }
  .sprocket.bottom { bottom: 7px; }
  .sprocket i {
    display: block;
    width: 12px;
    height: 9px;
    border-radius: 2px;
    background: rgba(255, 214, 10, 0.75);
  }
  .drag .frame {
    border-color: #ffd60a;
  }
  .playmark {
    width: 46px;
    height: 46px;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .playmark svg {
    width: 40px;
    height: 40px;
    fill: #3fb950; /* 绿色播放三角：与控制栏播放键呼应 */
    filter: drop-shadow(0 0 6px rgba(63, 185, 80, 0.35));
  }

  .title {
    font-size: 34px;
    font-weight: 700;
    letter-spacing: 2px;
    margin: 0;
    color: #fff;
  }
  .title.error {
    font-size: 18px;
    color: #ff6b6b;
    max-width: 80vw;
    overflow-wrap: anywhere;
  }
  .sub {
    font-size: 14px;
    color: rgba(255, 255, 255, 0.6);
    line-height: 1.6;
    max-width: 80vw;
    overflow-wrap: anywhere;
  }
  .sub.dim {
    color: rgba(255, 255, 255, 0.4);
    font-size: 12.5px;
  }
  .sub.alt {
    margin-top: 6px;
    font-size: 13px;
    color: rgba(255, 255, 255, 0.45);
  }

  /* ---- 大号 + 按钮：主 CTA，与拖拽等效 ---- */
  .pick-btn {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    margin-top: 10px;
    width: 128px;
    height: 128px;
    border-radius: 50%;
    border: 2px solid rgba(255, 214, 10, 0.55);
    background: rgba(255, 214, 10, 0.08);
    color: #ffd60a;
    cursor: pointer;
    user-select: none;
    -webkit-user-select: none;
    transition:
      background 0.12s ease,
      border-color 0.12s ease,
      transform 0.12s ease;
    font-family: inherit;
  }
  .pick-btn:hover {
    background: rgba(255, 214, 10, 0.18);
    border-color: #ffd60a;
    transform: scale(1.05);
  }
  .pick-btn:active {
    background: rgba(255, 214, 10, 0.28);
    transform: scale(0.98);
  }
  .pick-btn.disabled {
    opacity: 0.35;
    pointer-events: none;
  }
  .plus {
    font-size: 64px;
    line-height: 1;
    font-weight: 300;
    /* 视觉居中修正：+ 字符基线偏高 */
    transform: translateY(-2px);
  }
  .pick-label {
    font-size: 12px;
    letter-spacing: 1px;
    color: rgba(255, 255, 255, 0.75);
    transform: translateY(-4px);
  }

  /* ---- 次级入口：打开单个视频文件（低调的文字按钮） ---- */
  .file-btn {
    margin-top: 2px;
    padding: 7px 18px;
    border-radius: 16px;
    border: 1px solid rgba(255, 255, 255, 0.22);
    background: transparent;
    color: rgba(255, 255, 255, 0.65);
    font-size: 13px;
    font-family: inherit;
    letter-spacing: 0.5px;
    cursor: pointer;
    transition:
      border-color 0.12s ease,
      color 0.12s ease,
      background 0.12s ease;
  }
  .file-btn:hover {
    border-color: rgba(255, 214, 10, 0.6);
    color: #ffd60a;
    background: rgba(255, 214, 10, 0.06);
  }
  .file-btn:active {
    background: rgba(255, 214, 10, 0.14);
  }
  .file-btn.disabled {
    opacity: 0.35;
    pointer-events: none;
  }

  .spinner {
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 3px solid rgba(255, 255, 255, 0.15);
    border-top-color: #ffd60a;
    animation: spin 0.8s linear infinite;
    margin-bottom: 8px;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
