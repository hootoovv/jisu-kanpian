/**
 * 播放期间防止屏保 / 系统休眠（keep-awake）。
 *
 * 双层策略：
 * 1. 首选 Wake Lock API（Chromium 系 WebView / 较新 WKWebView 原生
 *    支持；页面隐藏时系统会自动释放，恢复可见后需重新申请）；
 * 2. WebView 不支持或申请失败时，退回 Rust 后备命令 set_keep_awake
 *    （Windows SetThreadExecutionState / macOS caffeinate /
 *    Linux systemd-inhibit，均为进程退出自动解除，无残留）。
 *
 * 对外接口：
 * - initWakeLock()：挂载页面可见性监听（App onMount 调一次）
 * - setKeepAwake(playing)：true = 持锁（正在播放），false = 解锁
 *
 * 两个函数都安全可重入：播放状态抖动（缓冲停顿等）时多余的
 * 申请 / 释放会被幂等处理，不会泄漏锁。
 */
import { invoke } from '@tauri-apps/api/core';

let wanted = false;    // 当前是否需要保持唤醒（播放中）
let jsLock = null;     // Wake Lock 哨兵（可能被系统释放，置空后可重申请）
let rustOn = false;    // Rust 后备已开启
let rustUsable = true; // 后备命令不可用（旧后端）时本进程内永久跳过
let seq = 0;           // 异步序号：丢弃过期请求的结果（快速连点防错序）

async function releaseJsLock() {
  const lock = jsLock;
  jsLock = null;
  if (lock) {
    try {
      await lock.release();
    } catch {
      /* 已被系统释放 */
    }
  }
}

async function acquire() {
  const my = ++seq;
  // ---- 第 1 层：Wake Lock API ----
  // （用真值判断而非 'wakeLock' in navigator：测试 / 嵌入环境可能
  //  用 getter 遮蔽出 undefined，此时应直接走后备而不是抛 TypeError）
  if (navigator.wakeLock) {
    try {
      const lock = await navigator.wakeLock.request('screen');
      if (my !== seq) {
        // 过期结果（期间已发生暂停）：立即放掉
        try {
          await lock.release();
        } catch {
          /* 忽略 */
        }
        return;
      }
      jsLock = lock;
      // 系统主动释放（页面隐藏等）→ 可见时自动重新申请
      lock.addEventListener?.('release', () => {
        if (jsLock === lock) jsLock = null;
        if (wanted && document.visibilityState === 'visible') acquire();
      });
      return; // 申请成功：无需 Rust 后备
    } catch {
      /* 不支持 / 被策略拒绝 → 落到后备 */
    }
  }
  if (my !== seq) return;
  // ---- 第 2 层：Rust 后备命令 ----
  if (!rustOn && rustUsable) {
    try {
      await invoke('set_keep_awake', { active: true });
      rustOn = true;
    } catch {
      rustUsable = false; // 旧后端没有此命令：本进程内不再尝试
    }
  }
}

async function release() {
  seq++; // 使所有在途 acquire 过期
  await releaseJsLock();
  if (rustOn) {
    rustOn = false;
    try {
      await invoke('set_keep_awake', { active: false });
    } catch {
      rustUsable = false;
    }
  }
}

/** 播放状态变化时调用：true = 保持唤醒，false = 恢复系统默认 */
export function setKeepAwake(playing) {
  wanted = !!playing;
  if (wanted) {
    void acquire();
  } else {
    void release();
  }
}

/** 挂载页面可见性监听（Wake Lock 在页面隐藏时被系统自动释放） */
export function initWakeLock() {
  document.addEventListener('visibilitychange', () => {
    if (wanted && document.visibilityState === 'visible' && !jsLock) {
      void acquire();
    }
  });
}

// 冒烟测试 / 现场排查辅助：当前持锁状态（不参与任何逻辑）
if (typeof window !== 'undefined') {
  window.__wakeLockState = () => ({ wanted, jsLock: !!jsLock, rustOn });
}
