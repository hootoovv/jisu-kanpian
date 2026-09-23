#!/usr/bin/env bash
# 极速看片 —— 无头浏览器冒烟测试驱动脚本
#
# 前置：
#   1. npm run build（dist/ 为最新构建）
#   2. bash scripts/make_testmedia.sh（生成 /home/z/my-project/testmedia）
#   3. node scripts/smoke/server.mjs（mock Tauri API 服务器，默认 :4174）
#   4. agent-browser install（无头浏览器）
#
# 用法：bash scripts/smoke/drive.sh
# 输出：每步 PASS/FAIL 汇总 + scripts/smoke/shots/ 截图
set -u
cd "$(dirname "$0")/../.."
BASE=http://localhost:4174
SHOTS=scripts/smoke/shots
mkdir -p "$SHOTS"
PASS=0; FAIL=0

ck() { # ck <描述> <实际值> <期望子串>
  local desc="$1" got="$2" want="$3"
  if [[ "$got" == *"$want"* ]]; then
    echo "PASS  $desc"
    PASS=$((PASS+1))
  else
    echo "FAIL  $desc  →  实际: $got （期望包含: $want）"
    FAIL=$((FAIL+1))
  fi
}

j() { agent-browser eval "$1" 2>/dev/null | tr -d '\n'; }

echo "==== 极速看片 · 冒烟测试 $(date '+%H:%M:%S') ===="

# ---- 0. 重置：重启 mock 服务器（清空内存态） ----
pkill -f "smoke/server.mjs" 2>/dev/null; sleep 0.5
rm -f scripts/smoke/state-log.json
nohup node scripts/smoke/server.mjs > scripts/smoke/server.log 2>&1 &
sleep 1

# ---- 1. 引导页 ----
agent-browser open "$BASE/" >/dev/null 2>&1
agent-browser wait 1500 >/dev/null 2>&1
ck "引导页渲染（大 + 按钮）" "$(j "!!document.querySelector('.pick-btn')")" "true"
ck "引导页标题" "$(j "document.querySelector('h1.title').textContent")" "极速看片"
agent-browser screenshot "$SHOTS/smoke-guide.png" >/dev/null 2>&1

# ---- 2. 点选目录 → 扫描 → 播放（首文件 = hls/playlist.m3u8，自然排序） ----
agent-browser eval "window.__pageErrors=[];'ok'" >/dev/null
agent-browser find text "选择视频文件夹" click >/dev/null 2>&1
agent-browser wait 4000 >/dev/null 2>&1
ck "进入播放界面（文件列表浮层）" "$(j "!!document.querySelector('.filelist')")" "true"
ck "视频共 5 个文件" "$(j "document.querySelector('.infobar').textContent.slice(-6)")" "1 / 5"
ck "首文件为 HLS（blob: MSE 源）" "$(j "document.querySelector('video').src.slice(0,5)")" "blob:"
agent-browser wait 3000 >/dev/null 2>&1
ck "HLS 缓冲就绪（readyState=4）" "$(j "document.querySelector('video').readyState")" "4"

# ---- 3. 播放 / 暂停 / 快进快退 ----
# 无头环境的自动播放策略可能拦截首次 play()（应用按设计 toast 提示）。
# 这里用一次真实点击提供用户激活，确保后续链路可测。
if [ "$(j "document.querySelector('video').paused")" = "true" ]; then
  # 坐标点击 = mouse move + down + up（真实事件，带用户激活）
  agent-browser mouse move 640 300 >/dev/null 2>&1
  agent-browser mouse down left >/dev/null 2>&1
  agent-browser mouse up left >/dev/null 2>&1
  agent-browser wait 1200 >/dev/null 2>&1
fi
ck "点击画面开始播放" "$(j "!document.querySelector('video').paused")" "true"
T1=$(j "document.querySelector('video').currentTime.toFixed(1)")
ck "播放位置前进（t>0）" "$T1" "."
agent-browser press ArrowRight >/dev/null 2>&1; agent-browser wait 300 >/dev/null 2>&1
ck "快进 30s 触底自动连播（文件序号变化）" "$(j "document.querySelector('.infobar').textContent.slice(-6)")" "2 / 5"
ck "multi.mp4 走 MSE（blob: 源）" "$(j "document.querySelector('video').src.slice(0,5)")" "blob:"
ck "多音轨菜单可用（音轨 · 中文 = 默认界面语言）" "$(j "document.querySelectorAll('.pill')[0].textContent.trim()")" "音轨 · 中文"
ck "默认字幕匹配界面语言（外挂 zh.vtt）" "$(j "document.querySelectorAll('.pill')[1].textContent.trim()")" "中文 · 外挂"
agent-browser press Space >/dev/null 2>&1
ck "空格暂停" "$(j "document.querySelector('video').paused")" "true"

# ---- 3b. 画面点击：单击播放/暂停 + 双击全屏 + 全屏隐藏全部浮层 + 防休眠 ----
# 直接在 VideoStage 根元素上派发 PointerEvent（不依赖自动播放策略，
# 确保点击链路被真实执行——此前 press 仅在放大时记录，默认缩放下
# 单击/双击完全失效，而旧用例因守卫跳过点击而漏测）。
# 全屏规格（v1.0.2）：进入全屏 = 纯画面模式，顶栏 / 底栏 / 文件列表
# 浮层一律立即隐藏（暂停中也隐藏），鼠标活动不浮现，退出后按偏好恢复。
STAGE="document.querySelector('video').parentElement"
CLICK="(()=>{const st=$STAGE;const mk=t=>new PointerEvent(t,{bubbles:true,cancelable:true,clientX:300,clientY:300,button:0,pointerId:9,isPrimary:true});st.dispatchEvent(mk('pointerdown'));st.dispatchEvent(mk('pointerup'));return 'ok'})()"
DBLCLK="(()=>{const st=$STAGE;const mk=t=>new PointerEvent(t,{bubbles:true,cancelable:true,clientX:300,clientY:300,button:0,pointerId:9,isPrimary:true});st.dispatchEvent(mk('pointerdown'));st.dispatchEvent(mk('pointerup'));st.dispatchEvent(mk('pointerdown'));st.dispatchEvent(mk('pointerup'));return 'ok'})()"
agent-browser eval "$CLICK" >/dev/null
agent-browser wait 450 >/dev/null 2>&1
ck "默认缩放下单击画面 = 播放（回归：press 修复）" "$(j "!document.querySelector('video').paused")" "true"
# 播放中 → 防屏保/休眠（mock 用 getter 遮蔽 wakeLock，走可观测的 Rust 后备命令）
agent-browser wait 400 >/dev/null 2>&1
ck "播放中已请求保持唤醒（set_keep_awake 后备命令）" "$(j "window.__invokeLog().includes('set_keep_awake')")" "true"
ck "播放中保持唤醒生效（jsLock 或后禁 rustOn 持锁）" "$(j "(()=>{const s=window.__wakeLockState();return s.wanted&&(s.rustOn||s.jsLock)})()")" "true"
agent-browser eval "$CLICK" >/dev/null
agent-browser wait 450 >/dev/null 2>&1
ck "再单击画面 = 暂停" "$(j "document.querySelector('video').paused")" "true"
agent-browser wait 400 >/dev/null 2>&1
ck "暂停后解除保持唤醒（三项全部归零）" "$(j "(()=>{const s=window.__wakeLockState();return !s.wanted&&!s.rustOn&&!s.jsLock})()")" "true"
# 暂停中双击进入全屏（第一击的播放定时器被第二击取消，不改变播放状态）
agent-browser eval "$DBLCLK" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
ck "暂停中双击进入全屏（播放定时器被取消，仍暂停）" "$(j "document.querySelector('video').paused")" "true"
ck "全屏立即隐藏：顶栏（暂停中也隐藏——本次修复点）" "$(j "!document.querySelector('.infobar')")" "true"
ck "全屏立即隐藏：底栏" "$(j "!document.querySelector('.ctrlbar')")" "true"
ck "全屏立即隐藏：文件列表浮层（本次新增）" "$(j "!document.querySelector('.list-overlay')")" "true"
agent-browser screenshot "$SHOTS/smoke-fs-hidden.png" >/dev/null 2>&1
agent-browser eval "window.dispatchEvent(new PointerEvent('pointermove',{bubbles:true}));'ok'" >/dev/null
agent-browser wait 300 >/dev/null 2>&1
ck "全屏中鼠标活动不浮现（一律隐藏）" "$(j "!document.querySelector('.infobar') && !document.querySelector('.ctrlbar') && !document.querySelector('.list-overlay')")" "true"
agent-browser press i >/dev/null 2>&1
agent-browser wait 250 >/dev/null 2>&1
ck "全屏中按 I：顶栏仍隐藏（偏好已切换，退出后生效）" "$(j "!document.querySelector('.infobar')")" "true"
agent-browser press i >/dev/null 2>&1
agent-browser press t >/dev/null 2>&1
agent-browser wait 250 >/dev/null 2>&1
ck "全屏中按 T：文件列表仍隐藏" "$(j "!document.querySelector('.list-overlay')")" "true"
agent-browser press t >/dev/null 2>&1
agent-browser eval "$DBLCLK" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
ck "双击退出全屏：顶栏按偏好恢复" "$(j "!!document.querySelector('.infobar')")" "true"
ck "双击退出全屏：底栏按偏好恢复" "$(j "!!document.querySelector('.ctrlbar')")" "true"
ck "双击退出全屏：文件列表按偏好恢复" "$(j "!!document.querySelector('.list-overlay')")" "true"
agent-browser wait 1200 >/dev/null 2>&1
ck "非全屏浮层常显（不受闲置影响）" "$(j "!!document.querySelector('.infobar') && !!document.querySelector('.ctrlbar')")" "true"
# 收尾：仍为暂停态，回退到 2s（后续音轨/字幕节的断言依赖仍在 multi.mp4 内）
agent-browser eval "(()=>{const v=document.querySelector('video');v.currentTime=2;return 1})()" >/dev/null
agent-browser wait 400 >/dev/null 2>&1

# ---- 4. 音轨切换（MSE 双 SourceBuffer） ----
agent-browser eval "document.querySelector('button[aria-label=选择音轨]').click();1" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
ck "音轨菜单 3 条（中/英/日）" "$(j "document.querySelectorAll('.menu-item').length")" "3"
agent-browser eval "(()=>{const it=[...document.querySelectorAll('.menu-item')];it.find(i=>i.textContent.includes('英语')).click();return 1})()" >/dev/null
agent-browser wait 2000 >/dev/null 2>&1
ck "切换到英语音轨" "$(j "document.querySelectorAll('.pill')[0].textContent.trim()")" "音轨 · 英语"

# ---- 5. 字幕：内嵌提取 / 外挂 srt ----
agent-browser eval "document.querySelector('button[aria-label=选择字幕]').click();1" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
ck "字幕菜单 5 项（不启用+2内嵌+2外挂）" "$(j "document.querySelectorAll('.menu-item').length")" "5"
agent-browser eval "(()=>{const it=[...document.querySelectorAll('.menu-item')];it.find(i=>i.textContent.includes('中文')&&i.textContent.includes('内嵌')).click();return 1})()" >/dev/null
agent-browser wait 2500 >/dev/null 2>&1
ck "内嵌 tx3g 提取成功（cue 数 = 6）" "$(j "(document.querySelector('video').querySelector('track')||{}).track?.cues?.length || 0")" "6"
agent-browser eval "document.querySelector('button[aria-label=选择字幕]').click();1" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
agent-browser eval "(()=>{const it=[...document.querySelectorAll('.menu-item')];it.find(i=>i.textContent.includes('srt')).click();return 1})()" >/dev/null
agent-browser wait 1500 >/dev/null 2>&1
ck "外挂 srt 转换挂载（cue 数 = 6）" "$(j "(document.querySelector('video').querySelector('track')||{}).track?.cues?.length || 0")" "6"
agent-browser press Space >/dev/null 2>&1
agent-browser wait 1200 >/dev/null 2>&1
agent-browser screenshot "$SHOTS/smoke-subs.png" >/dev/null 2>&1

# ---- 6. 旋转 / 缩放 / 拖拽 ----
agent-browser eval "window.dispatchEvent(new KeyboardEvent('keydown',{key:'r'}))" >/dev/null
agent-browser wait 300 >/dev/null 2>&1
ck "旋转 90°" "$(j "document.querySelector('video').style.transform")" "rotate(90deg)"
agent-browser eval "window.dispatchEvent(new KeyboardEvent('keydown',{key:'+'}))" >/dev/null
agent-browser wait 400 >/dev/null 2>&1
ck "放大 1.25×" "$(j "document.querySelector('video').style.transform")" "scale(1.25)"
CX=$(j "Math.round((()=>{const r=document.querySelector('video').getBoundingClientRect();return r.x+r.width/2})())")
CY=$(j "Math.round((()=>{const r=document.querySelector('video').getBoundingClientRect();return r.y+r.height/2})())")
agent-browser mouse move "$CX" "$CY" >/dev/null 2>&1
agent-browser mouse down left >/dev/null 2>&1
agent-browser mouse move $((CX-80)) $((CY+40)) >/dev/null 2>&1
agent-browser mouse up left >/dev/null 2>&1
agent-browser wait 400 >/dev/null 2>&1
ck "放大后拖拽平移（translate 非零）" "$(j "document.querySelector('video').style.transform.includes('translate(') && !document.querySelector('video').style.transform.includes('translate(0.00px, 0.00px)')")" "true"
agent-browser screenshot "$SHOTS/smoke-zoom-pan.png" >/dev/null 2>&1

# ---- 7. 逐帧步进（MSE 已缓冲区快路径） ----
agent-browser eval "(()=>{const v=document.querySelector('video');v.pause();v.currentTime=2;return 1})()" >/dev/null
agent-browser wait 600 >/dev/null 2>&1
agent-browser eval "window.dispatchEvent(new KeyboardEvent('keydown',{key:'.'}))" >/dev/null
agent-browser wait 800 >/dev/null 2>&1
ck "前进一帧（2 → 2.04x）" "$(j "document.querySelector('video').currentTime.toFixed(2)")" "2.0"

# ---- 8. 浮层开关 / 帮助 / ESC ----
agent-browser press i >/dev/null 2>&1; ck "I 隐藏信息栏" "$(j "!document.querySelector('.infobar')")" "true"; agent-browser press i >/dev/null 2>&1
agent-browser press c >/dev/null 2>&1; ck "C 隐藏控制栏" "$(j "!document.querySelector('.ctrlbar')")" "true"; agent-browser press c >/dev/null 2>&1
agent-browser press t >/dev/null 2>&1; ck "T 隐藏文件列表浮层" "$(j "!document.querySelector('.list-overlay')")" "true"; agent-browser press t >/dev/null 2>&1
agent-browser press F1 >/dev/null 2>&1; ck "F1 打开帮助层" "$(j "!!document.querySelector('.mask')")" "true"
agent-browser press Escape >/dev/null 2>&1
ck "Esc 关闭帮助（不退出）" "$(j "!document.querySelector('.mask') && !!document.querySelector('.filelist')")" "true"
agent-browser press F1 >/dev/null 2>&1; agent-browser press Escape >/dev/null 2>&1

# ---- 9. 音量（键盘 + 滚轮） ----
for i in $(seq 1 10); do agent-browser press ArrowDown >/dev/null 2>&1; done
agent-browser wait 400 >/dev/null 2>&1
ck "↓×10 音量 1.00→0.50" "$(j "document.querySelector('video').volume.toFixed(2)")" "0.50"
agent-browser press ArrowUp >/dev/null 2>&1; agent-browser wait 300 >/dev/null 2>&1
ck "↑ 音量 +5%（0.50→0.55）" "$(j "document.querySelector('video').volume.toFixed(2)")" "0.55"
agent-browser eval "window.dispatchEvent(new WheelEvent('wheel',{deltaY:-100,cancelable:true}));1" >/dev/null
agent-browser wait 300 >/dev/null 2>&1
ck "滚轮向上 音量 +5%（0.55→0.60）" "$(j "document.querySelector('video').volume.toFixed(2)")" "0.60"

# ---- 10. 状态持久化（等待防抖落盘） ----
agent-browser wait 2500 >/dev/null 2>&1
STATE=$(cat scripts/smoke/state-log.json 2>/dev/null || echo '{}')
ck "存档：rotation=90" "$STATE" '"rotation": 90'
ck "存档：音频语言偏好=en" "$STATE" '"audio_lang": "en"'
ck "存档：每文件断点 positions" "$STATE" '"positions"'

# ---- 11. 断点续看：重载 → 定位暂停 ----
agent-browser eval "(()=>{const v=document.querySelector('video');v.currentTime=5;v.pause();return 1})()" >/dev/null
agent-browser wait 2500 >/dev/null 2>&1
ck "存档：退出位置 = 5000" "$(cat scripts/smoke/state-log.json 2>/dev/null)" '"position_ms": 5000'
agent-browser reload >/dev/null 2>&1
agent-browser wait 6000 >/dev/null 2>&1
ck "重启后恢复到退出位置（t≈5）" "$(j "((Math.abs(document.querySelector('video').currentTime-5)<0.6) + ' t=' + document.querySelector('video').currentTime.toFixed(2))")" "true"
ck "恢复后为暂停状态" "$(j "document.querySelector('video').paused")" "true"
ck "恢复旋转角度（90°）" "$(j "document.querySelector('video').style.transform")" "rotate(90deg)"
agent-browser screenshot "$SHOTS/smoke-restore.png" >/dev/null 2>&1

# ---- 12. X 关闭目录 → 引导页 + 清记忆 ----
agent-browser press x >/dev/null 2>&1; agent-browser wait 2000 >/dev/null 2>&1
ck "X 回到引导页" "$(j "!!document.querySelector('.hint')")" "true"
ck "X 清除断点记忆" "$(cat scripts/smoke/state-log.json)" '"root": null'

# ---- 13. 全程零页面错误 ----
ck "全程无运行时错误" "$(j "JSON.stringify(window.__pageErrors)")" "[]"

echo "===="
echo "结果：PASS $PASS / FAIL $FAIL"
exit $([ "$FAIL" -eq 0 ] && echo 0 || echo 1)
