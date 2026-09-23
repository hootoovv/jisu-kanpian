<script>
  /**
   * 顶部信息栏（I 键切换显隐，Overlay 覆盖在内容上方）：
   * 左：相对主目录的目录 / 文件名；右：播放进度（百分比 + 00:00:00）
   * 与序号，随播放动态更新；另有当前音轨 / 字幕选择与缩放旋转状态
   * （非默认时显示）。黑主题：深色底 + 白字 + 底边线，
   * 与主窗口背景拉开层次；不拦截鼠标。
   */
  import { formatTime, formatPercent, formatZoom } from '../lib/format.js';

  let {
    rel = '',
    pos = 0,
    total = 0,
    curTime = 0,
    duration = 0,
    audioLabel = '',
    subLabel = '',
    rotation = 0,
    zoom = 1
  } = $props();
</script>

<div class="infobar">
  <span class="rel" title={rel}>{rel || '—'}</span>
  {#if audioLabel}<span class="dim">音轨 {audioLabel}</span>{/if}
  {#if subLabel}<span class="dim">字幕 {subLabel}</span>{/if}
  {#if rotation !== 0 || zoom !== 1}
    <span class="dim tnum">{formatZoom(zoom)}{rotation !== 0 ? ` · ${rotation}°` : ''}</span>
  {/if}
  <span class="pct tnum">{formatPercent(curTime, duration)}</span>
  <span class="time tnum">{formatTime(curTime)} / {formatTime(duration)}</span>
  <span class="count tnum">{pos} / {total}</span>
</div>

<style>
  .infobar {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    height: 34px;
    display: flex;
    align-items: center;
    gap: 18px;
    padding: 0 14px;
    background: rgba(10, 10, 12, 0.96); /* 黑主题深色底 */
    border-bottom: 1px solid rgba(255, 255, 255, 0.09);
    color: #fff;
    font-size: 13px;
    z-index: 50;
    pointer-events: none; /* 信息栏不拦截鼠标，点击可穿透 */
  }
  .rel {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dim {
    flex: none;
    color: rgba(255, 255, 255, 0.5); /* 次要信息：半透明白 */
    white-space: nowrap;
  }
  .pct {
    flex: none;
    color: #7ee787; /* 绿色强调：与已播进度条呼应 */
    min-width: 52px;
    text-align: right;
  }
  .time {
    flex: none;
  }
  .count {
    flex: none;
    opacity: 0.75;
  }
</style>
