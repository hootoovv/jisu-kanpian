<script>
  /**
   * 分页文件列表（每目录一组，按自然排序铺开成一条观看序列）。
   *
   * 与极速听书同款：列表住在屏幕右侧的独立浮层（T 键显隐）：
   * - 黑色背景 + 亮灰色文字，字号固定 13px（行高由父组件传入
   *   固定 24px），紧凑不抢主窗口画面的戏；
   * - 目录变化时插入一条暗色的目录分组行（根目录不插）；
   * - 正在播放的行高亮（左侧绿色强调条 + 播放指示）；
   *   「默认选中当前播放的文件」：列表始终锚定在播放文件所在页；
   * - 有断点记忆的文件右侧显示「已看 mm:ss」徽标；
   * - 点击某一行：跳播该文件；
   * - 翻页通过点击浮层底部的页码指示（回车在本程序里是播放 /
   *   暂停，不再兼任翻页键）。
   */
  import { formatPosShort } from '../lib/format.js';

  let {
    lines = [],        // 当前页的行（{type:'dir',rel} | {type:'file',idx,path,rel,name}）
    playingIdx = -1,   // 播放中的文件下标（高亮）
    positions = null,  // Map<path, ms> 断点记忆（响应式）
    rowHeight = 24,
    onplay = null
  } = $props();

  function play(e, idx) {
    e.stopPropagation();
    onplay?.(idx);
  }
</script>

<div class="filelist">
  {#each lines as line (line.type === 'dir' ? `d-${line.rel}` : `f-${line.idx}`)}
    {#if line.type === 'dir'}
      <div class="dir-row" style="height:{rowHeight}px">
        <span class="dir-name" title={line.rel}>{line.rel}/</span>
      </div>
    {:else}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="file-row"
        class:playing={line.idx === playingIdx}
        style="height:{rowHeight}px"
        onclick={(e) => play(e, line.idx)}
        role="button"
        tabindex="-1"
        aria-label={line.name}
      >
        <span class="idx tnum">{line.idx + 1}</span>
        <span class="ic" aria-hidden="true">{line.idx === playingIdx ? '▶' : ''}</span>
        <span class="name" title={line.rel}>{line.name}</span>
        {#if line.idx !== playingIdx && positions && positions.get(line.path)}
          <span class="badge tnum">已看 {formatPosShort(positions.get(line.path))}</span>
        {/if}
      </div>
    {/if}
  {/each}
</div>

<style>
  .filelist {
    height: 100%;
    overflow: hidden;
    color: #c6c6c6; /* 亮灰文字 */
    font-size: 13px; /* 固定小字号：不放大、不抢戏 */
    font-family: 'Segoe UI', 'Microsoft YaHei', 'PingFang SC', 'Noto Sans SC', sans-serif;
  }
  .file-row {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 0 12px 0 9px;
    cursor: pointer;
    overflow: hidden;
    border-left: 3px solid transparent;
  }
  .file-row:hover {
    background: rgba(255, 255, 255, 0.06);
  }
  .file-row.playing {
    border-left-color: #3fb950; /* 绿色强调：与播放键呼应 */
    background: rgba(63, 185, 80, 0.12);
    color: #eafbe7;
    font-weight: 600;
  }
  .idx {
    flex: none;
    min-width: 3em;
    text-align: right;
    font-size: 11px;
    opacity: 0.4;
  }
  .ic {
    flex: none;
    width: 0.9em;
    font-size: 10px;
    color: #3fb950;
  }
  .name {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    flex: none;
    font-size: 10.5px;
    opacity: 0.5;
    border: 1px solid currentColor;
    border-radius: 999px;
    padding: 1px 7px;
  }
  .dir-row {
    display: flex;
    align-items: center;
    padding: 0 12px;
    border-top: 1px solid rgba(255, 255, 255, 0.07);
  }
  .dir-name {
    font-size: 11px;
    opacity: 0.45;
    letter-spacing: 0.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
