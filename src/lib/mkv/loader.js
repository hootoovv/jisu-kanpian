/**
 * 极速看片 —— matroska-subtitles 浏览器 bundle 按需加载器
 *
 * 为什么是动态 <script> 而不是 ESM import：
 * - 选用的 matroska-subtitles@3.x 官方浏览器产物是 webpack 打的
 *   IIFE（dist/matroska-subtitles.min.js），顶层 var 挂 window，
 *   无模块导出——只能以经典脚本方式消费；
 * - 它内联了 ebml-stream / readable-stream / buffer / pako 的全部
 *   浏览器 polyfill（147KB），一次加载处处可用，且只在第一次遇到
 *   MKV 时才加载（其它格式零开销）。
 *
 * 方案调研记录（详见 docs/架构设计.md §3.6）：
 * - npm `matroska`：纯 Node（fs/http/util 依赖），无 browser 字段，弃用；
 * - npm `ebml` / `ebml-block`：仅底层解码，轨道语义全要自己写，且同为
 *   Node 流式 API；不如官方浏览器 bundle 完整；
 * - npm `libass-wasm`（7.7MB，CJK 还需另配字体）：太重，本项目的
 *   ASS 处理走「ass-compiler 解析 + 纯文本富标签 VTT」轻量路线。
 */

let loader = null; // 单例：多次调用共享同一次加载

export function loadMatroskaSubtitles() {
  if (loader) return loader;
  loader = new Promise((resolve, reject) => {
    if (typeof window !== 'undefined' && window.MatroskaSubtitles) {
      resolve(window.MatroskaSubtitles);
      return;
    }
    const s = document.createElement('script');
    // Vite 的 new URL(..., import.meta.url) 资产引用：构建时复制进
    // dist/assets 并哈希化，与代码分包天然一致
    s.src = new URL('./vendor/matroska-subtitles.min.js', import.meta.url).href;
    s.async = true;
    s.onload = () => {
      if (window.MatroskaSubtitles) resolve(window.MatroskaSubtitles);
      else reject(new Error('matroska-subtitles 加载异常（无导出）'));
    };
    s.onerror = () => {
      loader = null; // 允许下次重试
      reject(new Error('matroska-subtitles 脚本加载失败'));
    };
    document.head.appendChild(s);
  });
  return loader;
}
