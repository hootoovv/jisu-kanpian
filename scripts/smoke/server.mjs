/**
 * 极速看片 —— 无头冒烟测试服务器
 *
 * 以「mock Tauri API」方式加载真实构建产物（dist/）：
 * - GET /            → dist/index.html（注入 tauri-mock.js，先于应用脚本执行）
 * - GET /assets/*    → dist 静态资源
 * - GET /tauri-mock.js → 页面侧 Tauri IPC mock
 * - GET /files/<enc> → convertFileSrc 映射出的本地文件（视频 / 字幕 / HLS）
 * - POST /__invoke   → {cmd, args} 路由到 JS 版后端：
 *     scan_directory / read_range / stat_file / load_state / save_state /
 *     exit_app / plugin:dialog|open / plugin:window|*
 *
 * scan 的自然排序 / 扩展名 / 递归规则与 src-tauri/src/scanner.rs 保持一致。
 * save_state 的载荷会落盘 state-log.json，供测试断言持久化内容。
 */
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DIST = path.resolve(__dirname, '../../dist');
const TEST_DIR = process.env.SMOKE_DIR || '/home/z/my-project/testmedia';
const PORT = Number(process.env.PORT || 4174);
const STATE_LOG = path.resolve(__dirname, 'state-log.json');

const VIDEO_EXTS = ['mp4', 'm4v', 'mov', '3gp', 'webm', 'ogv', 'ogg', 'mkv', 'avi', 'm3u8'];
// 与 src-tauri/src/scanner.rs 的 SUB_EXTS 保持一致（v1.0.3 起含 ass/ssa）
const SUB_EXTS = ['vtt', 'srt', 'ass', 'ssa'];

/* ---------- 与 scanner.rs 一致的自然排序 ---------- */
function naturalCmp(a, b) {
  const la = [...a.toLowerCase()];
  const lb = [...b.toLowerCase()];
  let i = 0;
  let j = 0;
  while (i < la.length && j < lb.length) {
    if (/[0-9]/.test(la[i]) && /[0-9]/.test(lb[j])) {
      const sa = i;
      while (i < la.length && /[0-9]/.test(la[i])) i++;
      const sb = j;
      while (j < lb.length && /[0-9]/.test(lb[j])) j++;
      const da = la.slice(sa, i).join('').replace(/^0+/, '');
      const db = lb.slice(sb, j).join('').replace(/^0+/, '');
      const c = da.length !== db.length ? (da.length < db.length ? -1 : 1) : (da < db ? -1 : da > db ? 1 : 0);
      if (c !== 0) return c;
    } else {
      if (la[i] < lb[j]) return -1;
      if (la[i] > lb[j]) return 1;
      i++;
      j++;
    }
  }
  return (la.length - i) - (lb.length - j);
}

function splitExt(name) {
  const dot = name.lastIndexOf('.');
  if (dot <= 0) return null;
  const ext = name.slice(dot + 1).toLowerCase();
  if (!ext) return null;
  return [name.slice(0, dot), ext];
}

function scanDir(abs, rel, out, depth = 0) {
  if (depth > 64) return;
  let entries;
  try {
    entries = fs.readdirSync(abs, { withFileTypes: true });
  } catch {
    return;
  }
  const files = [];
  const subs = [];
  const subdirs = [];
  for (const e of entries) {
    const isSymlink = e.isSymbolicLink();
    if (e.isDirectory() && !isSymlink) subdirs.push(e.name);
    else if (e.isFile() && !isSymlink) {
      const se = splitExt(e.name);
      if (!se) continue;
      if (VIDEO_EXTS.includes(se[1])) files.push(e.name);
      else if (SUB_EXTS.includes(se[1])) subs.push(e.name);
    }
  }
  files.sort(naturalCmp);
  subs.sort(naturalCmp);
  subdirs.sort(naturalCmp);
  if (files.length) {
    out.push({
      path: abs,
      rel,
      files: files.map((n) => ({
        path: path.join(abs, n),
        rel: rel ? `${rel}/${n}` : n,
        name: n
      })),
      subs: subs
        .map((n) => {
          const [stem, ext] = splitExt(n);
          return { path: path.join(abs, n), rel: rel ? `${rel}/${n}` : n, stem, ext };
        })
    });
  }
  for (const d of subdirs) {
    scanDir(path.join(abs, d), rel ? `${rel}/${d}` : d, out, depth + 1);
  }
}

function scanDirectory(root) {
  let dir = root;
  try {
    const st = fs.statSync(root);
    if (!st.isDirectory()) dir = path.dirname(root);
  } catch {
    throw new Error(`路径不存在：${root}`);
  }
  const directories = [];
  scanDir(dir, '', directories);
  return {
    root: dir,
    totalFiles: directories.reduce((s, d) => s + d.files.length, 0),
    directories
  };
}

/* ---------- 模拟后端状态 ---------- */
let savedState = null;
const invokeLog = [];

function handleInvoke(cmd, args) {
  invokeLog.push({ cmd, t: Date.now() });
  switch (cmd) {
    case 'scan_directory':
      return { json: scanDirectory(args.root) };
    case 'stat_file':
      return { json: fs.statSync(args.path).size };
    case 'read_range': {
      const { path: p, offset, length } = args;
      const MAX = 8 * 1024 * 1024;
      const st = fs.statSync(p);
      let len = Math.min(Number(length) || 0, MAX);
      if (offset >= st.size) return { bin: Buffer.alloc(0) };
      len = Math.min(len, st.size - Number(offset));
      const fh = fs.openSync(p, 'r');
      const buf = Buffer.alloc(len);
      fs.readSync(fh, buf, 0, len, Number(offset));
      fs.closeSync(fh);
      return { bin: buf };
    }
    case 'load_state':
      return {
        json:
          savedState || {
            root: null,
            file: null,
            position_ms: null,
            volume: 1,
            rotation: 0,
            zoom: 1,
            audio_lang: 'zh',
            subtitle_lang: 'zh',
            info_visible: true,
            ctrl_visible: true,
            list_visible: true,
            positions: {}
          }
      };
    case 'save_state':
      savedState = args.state;
      fs.writeFileSync(STATE_LOG, JSON.stringify(args.state, null, 2));
      return { json: null };
    case 'set_keep_awake':
      // 防屏保 / 休眠后备命令（真机见 src-tauri/src/power.rs）
      console.log('[mock] set_keep_awake:', args.active);
      return { json: null };
    case 'exit_app':
      console.log('[mock] exit_app called');
      return { json: null };
    case 'plugin:dialog|open':
      return { json: TEST_DIR };
    case 'plugin:window|set_title':
      console.log('[mock] setTitle:', args.value);
      return { json: null };
    case 'plugin:window|destroy':
      console.log('[mock] window destroy (退出程序)');
      return { json: null };
    case 'plugin:window|set_fullscreen':
      console.log('[mock] setFullscreen:', args.value);
      return { json: null };
    default:
      if (cmd.startsWith('plugin:')) return { json: null };
      throw new Error(`未知命令：${cmd}`);
  }
}

/* ---------- HTTP 服务 ---------- */
const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.mp4': 'video/mp4',
  '.m4v': 'video/mp4',
  '.mkv': 'video/x-matroska',
  '.webm': 'video/webm',
  '.ogv': 'video/ogg',
  '.ts': 'video/mp2t',
  '.m3u8': 'application/vnd.apple.mpegurl',
  '.vtt': 'text/vtt; charset=utf-8',
  '.srt': 'text/plain; charset=utf-8',
  '.ass': 'text/plain; charset=utf-8',
  '.ssa': 'text/plain; charset=utf-8'
};

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://localhost:${PORT}`);
  if (req.method === 'POST' && url.pathname === '/__invoke') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      try {
        const { cmd, args } = JSON.parse(body || '{}');
        const out = handleInvoke(cmd, args || {});
        if (out.bin) {
          res.writeHead(200, { 'Content-Type': 'application/octet-stream' });
          res.end(out.bin);
        } else {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify(out.json ?? null));
        }
      } catch (e) {
        res.writeHead(500, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: String(e.message || e) }));
      }
    });
    return;
  }

  let fp = null;
  if (url.pathname === '/') fp = path.join(DIST, 'index.html');
  else if (url.pathname === '/tauri-mock.js') fp = path.join(__dirname, 'tauri-mock.js');
  else if (url.pathname === '/mp4box.mjs') {
    fp = path.resolve(__dirname, '../../node_modules/mp4box/dist/mp4box.all.mjs');
  }
  else if (/^\/(rolldown-runtime|styp|all)-[^/]+\.mjs$/.test(url.pathname)) {
    // mp4box.all.mjs 的同目录依赖块（调试路由用）
    fp = path.resolve(__dirname, '../../node_modules/mp4box/dist', '.' + url.pathname);
  }
  else if (url.pathname.startsWith('/assets/')) fp = path.join(DIST, decodeURIComponent(url.pathname));
  else if (url.pathname.startsWith('/files/')) fp = decodeURIComponent(url.pathname.slice('/files/'.length));

  if (!fp || !fs.existsSync(fp) || !fs.statSync(fp).isFile()) {
    res.writeHead(404);
    res.end('not found');
    return;
  }
  // index.html 注入 mock（先于应用模块执行）
  if (fp.endsWith('index.html')) {
    let html = fs.readFileSync(fp, 'utf8');
    html = html.replace(
      /<script type="module"[^>]*src="([^"]+)"[^>]*><\/script>/,
      '<script src="/tauri-mock.js"></script>\n    <script type="module" src="$1"></script>'
    );
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Cache-Control': 'no-store' });
    res.end(html);
    return;
  }
  const ext = path.extname(fp).toLowerCase();
  // ---- 静态文件：带 Range / Content-Length 的完整 HTTP 语义 ----
  // 为什么必须支持 Range：原生 <video>（asset 协议映射到本服务）的
  // seek 依赖字节范围请求；没有 Range / Content-Length 时 Chromium 把
  // 响应当 progressive 流，seek 失效（currentTime 卡住不触发 ended）。
  // v1.0.3 起测试媒体含原生播放的 MKV，此前 MSE/HLS 全走 blob 不受影响。
  const stat = fs.statSync(fp);
  const total = stat.size;
  const range = req.headers.range;
  const baseHead = {
    'Content-Type': MIME[ext] || 'application/octet-stream',
    'Cache-Control': 'no-store',
    'Accept-Ranges': 'bytes'
  };
  if (range) {
    // 例：Range: bytes=100- / bytes=100-199 / bytes=-100（后缀）
    const m = /^bytes=(\d*)-(\d*)$/.exec(String(range).trim());
    if (!m) {
      res.writeHead(416, { 'Content-Range': `bytes */${total}` });
      res.end();
      return;
    }
    let start = m[1] === '' ? NaN : Number(m[1]);
    let end = m[2] === '' ? NaN : Number(m[2]);
    if (Number.isNaN(start) && Number.isNaN(end)) {
      // 后缀范围：bytes=-N → 最后 N 字节
      start = Math.max(0, total - Number(String(range).trim().slice(7)));
      end = total - 1;
    } else if (Number.isNaN(start)) {
      start = Math.max(0, total - (Number.isNaN(end) ? 0 : end + 1));
      end = total - 1;
    } else if (Number.isNaN(end)) {
      end = total - 1;
    }
    if (start > end || start >= total) {
      res.writeHead(416, { 'Content-Range': `bytes */${total}` });
      res.end();
      return;
    }
    end = Math.min(end, total - 1);
    res.writeHead(206, {
      ...baseHead,
      'Content-Range': `bytes ${start}-${end}/${total}`,
      'Content-Length': String(end - start + 1)
    });
    fs.createReadStream(fp, { start, end }).pipe(res);
    return;
  }
  res.writeHead(200, { ...baseHead, 'Content-Length': String(total) });
  fs.createReadStream(fp).pipe(res);
});

server.listen(PORT, () => {
  console.log(`[smoke] http://localhost:${PORT}/  (测试目录 ${TEST_DIR})`);
});
