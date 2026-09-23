/**
 * 极速看片 —— 页面侧 Tauri IPC mock（无头冒烟测试用）
 *
 * 在应用脚本加载之前注入 window.__TAURI_INTERNALS__：
 * - invoke：plugin:event|* 在页面内本地处理（listen/unlisten + __mockEmit），
 *   其余命令 POST /__invoke 交给 Node 侧 mock 后端；
 * - convertFileSrc：/files/<encodeURIComponent(path)>；
 * - metadata：单窗口 "main"（与 tauri.conf.json 一致）。
 *
 * 测试辅助：
 * - window.__mockEmit(event, payload) —— 触发 tauri 事件（拖拽 / 关闭请求）
 * - window.__invokeLog() —— 页面侧命令调用记录
 * - window.__pageErrors[] —— 运行时错误收集
 */
(() => {
  const callbacks = new Map();
  let cbSeq = 0;
  const listeners = []; // { event, callbackId }
  const log = [];

  window.__pageErrors = [];
  window.addEventListener('error', (e) => window.__pageErrors.push(String(e.message)));
  window.addEventListener('unhandledrejection', (e) =>
    window.__pageErrors.push(`unhandledrejection: ${e.reason}`)
  );

  function transformCallback(cb) {
    const id = ++cbSeq;
    callbacks.set(id, cb);
    return id;
  }

  async function invoke(cmd, args = {}) {
    log.push(cmd);
    if (cmd === 'plugin:event|listen') {
      listeners.push({ event: args.event, callbackId: args.handler });
      return listeners.length; // eventId
    }
    if (cmd === 'plugin:event|unlisten') return null;
    const res = await fetch('/__invoke', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ cmd, args })
    });
    if (cmd === 'read_range') {
      if (!res.ok) throw new Error(`read_range 失败：${res.status}`);
      return await res.arrayBuffer();
    }
    const text = await res.text();
    let data = null;
    try {
      data = text ? JSON.parse(text) : null;
    } catch {
      data = text;
    }
    if (!res.ok) throw new Error((data && data.error) || `invoke ${cmd} 失败：${res.status}`);
    return data;
  }

  window.__TAURI_INTERNALS__ = {
    metadata: {
      currentWindow: { label: 'main' },
      currentWebview: { windowLabel: 'main', label: 'main' }
    },
    plugins: {},
    transformCallback,
    unregisterCallback: (id) => callbacks.delete(id),
    invoke,
    // 与真实 Tauri 一致：必须返回绝对 URL（asset:// 是绝对协议），
    // 否则 hls.js / <track> 会以 blob: 为基解析出错误地址
    convertFileSrc: (filePath) => `${location.origin}/files/${encodeURIComponent(filePath)}`
  };
  // 无头环境里 Wake Lock API 实际可用且无法观测，这里用自有 getter
  // 遮蔽（wakeLock 定义在 Navigator.prototype 上，delete 删不掉），
  // 强制走「Rust 后备命令」路径 —— 后备走 invoke，可被 mock 记录 /
  // 断言，使防休眠测试完全确定性。真机上 WebView2 / 较新 WKWebView
  // 会优先用 Wake Lock（wakelock.js）。
  try {
    Object.defineProperty(navigator, 'wakeLock', {
      configurable: true,
      get: () => undefined
    });
  } catch {
    /* 忽略 */
  }
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: () => {}
  };

  /** 触发一个 tauri 事件（payload 会包装成 {event, id, payload}） */
  window.__mockEmit = (event, payload) => {
    let seq = 0;
    for (const l of listeners) {
      if (l.event === event) {
        const cb = callbacks.get(l.callbackId);
        if (cb) cb({ event, id: ++seq, payload });
      }
    }
  };
  window.__invokeLog = () => [...log];
})();
