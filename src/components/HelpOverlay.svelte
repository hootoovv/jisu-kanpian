<script>
  /** F1 帮助层：列出全部快捷键与鼠标操作（按任意键或点击空白关闭） */
  let { onclose } = $props();

  const keys = [
    ['→ 右方向键', '快进 30 秒'],
    ['← 左方向键', '快退 30 秒'],
    ['↑ 上方向键', '音量 +5%'],
    ['↓ 下方向键', '音量 -5%'],
    ['空格 Space', '播放 / 暂停'],
    ['回车 Enter', '播放 / 暂停'],
    ['PageDown', '下一个文件'],
    ['PageUp', '上一个文件'],
    ['Home', '跳到本文件开头'],
    ['End', '跳到本文件结尾'],
    ['Esc', '全屏时退出全屏；非全屏时退出程序'],
    ['+ 或 =', '画面放大'],
    ['- 或 _', '画面缩小'],
    [',（含 ，）', '退后一帧'],
    ['.（含 。）', '前进一帧'],
    ['R', '顺时针旋转画面 90°'],
    ['X', '关闭当前文件 / 目录，回到引导页（清除位置记忆）'],
    ['I', '显示 / 隐藏顶部信息栏（全屏中仍生效偏好，退出全屏后应用）'],
    ['C', '显示 / 隐藏底部控制栏（全屏中仍生效偏好，退出全屏后应用）'],
    ['T', '显示 / 隐藏右侧文件列表浮层（全屏中仍生效偏好，退出全屏后应用）'],
    ['F1', '显示本帮助']
  ];

  const mouse = [
    ['引导页「+」大按钮', '点击弹出目录选择对话框（与拖拽等效）'],
    ['点击文件列表某行', '播放该文件'],
    ['点击画面空白区域', '播放 / 暂停'],
    ['双击画面', '全屏 / 恢复；全屏 = 纯画面模式，顶栏 / 底栏 / 文件列表一律自动隐藏'],
    ['全屏中的键盘 / 滚轮', '照常可用（快捷键 / 音量不受浮层隐藏影响）'],
    ['播放期间', '自动阻止系统屏保与休眠（暂停 / 退出播放后恢复）'],
    ['放大后拖拽画面', '移动可见的视频画面'],
    ['点击音量图标', '静音（显示静音图标），再次点击解除'],
    ['滚轮向上', '音量 +5%'],
    ['滚轮向下', '音量 -5%'],
    ['拖入文件夹', '打开 / 切换目录']
  ];
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="mask" onclick={onclose} role="presentation">
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="panel" onclick={(e) => e.stopPropagation()}>
    <h2>快捷键与鼠标操作</h2>
    <div class="cols">
      <table>
        <tbody>
          {#each keys as [k, d]}
            <tr>
              <td class="k">{k}</td>
              <td>{d}</td>
            </tr>
          {/each}
        </tbody>
      </table>
      <table>
        <tbody>
          {#each mouse as [k, d]}
            <tr>
              <td class="k">{k}</td>
              <td>{d}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
    <div class="foot">按任意键或点击空白处关闭帮助</div>
  </div>
</div>

<style>
  .mask {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.65);
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .panel {
    background: #16161d;
    border: 1px solid rgba(255, 255, 255, 0.14);
    border-radius: 12px;
    padding: 22px 26px 16px;
    max-width: min(880px, 92vw);
    max-height: 86vh;
    overflow: auto;
    box-shadow: 0 18px 60px rgba(0, 0, 0, 0.6);
  }
  h2 {
    margin: 0 0 16px;
    font-size: 18px;
    letter-spacing: 1px;
    color: #fff;
  }
  .cols {
    display: flex;
    gap: 32px;
    flex-wrap: wrap;
  }
  table {
    border-collapse: collapse;
    font-size: 13.5px;
  }
  td {
    padding: 5px 10px 5px 0;
    color: rgba(255, 255, 255, 0.82);
    line-height: 1.45;
  }
  td.k {
    color: #ffd60a;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
    padding-right: 18px;
  }
  .foot {
    margin-top: 18px;
    padding-top: 12px;
    border-top: 1px solid rgba(255, 255, 255, 0.1);
    font-size: 12.5px;
    color: rgba(255, 255, 255, 0.45);
    text-align: center;
  }
</style>
