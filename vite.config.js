import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [svelte()],

  // Tauri 开发模式下 vite 由 tauri CLI 拉起，不要清屏，方便看日志
  clearScreen: false,

  server: {
    port: 5173,
    strictPort: true,
    watch: {
      // 忽略 Rust 后端目录，避免双向重启
      ignored: ['**/src-tauri/**']
    }
  },

  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
    // hls.js 经动态 import 单独分块（约 590KB gzip 185KB），仅在播放
    // m3u8 时加载；这是刻意设计而非需要修复的告警
    chunkSizeWarningLimit: 700
  }
});
