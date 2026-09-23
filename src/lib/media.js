/**
 * 极速看片 —— 媒体元信息工具（语言标签 / 扩展名分类 / 字幕同名匹配）
 */

/** 需要 mp4box.js 分析（多音轨 / 字幕流探测）的扩展名 */
export const MP4_FAMILY = ['mp4', 'm4v', 'mov'];

/** HLS 播放列表 */
export const HLS_FAMILY = ['m3u8'];

/** 取小写扩展名（无扩展名返回 ''） */
export function extOf(name) {
  const i = String(name || '').lastIndexOf('.');
  if (i <= 0) return '';
  return name.slice(i + 1).toLowerCase();
}

/**
 * ISO 639-2/639-1 → 中文标签（常见语言覆盖即可，未收录回退原代码）。
 * mdhd 里多是 3 字母代码（chi/zho），hdlr/elng 偶见 2 字母（zh）。
 */
const LANG_LABELS = {
  zh: '中文', chi: '中文', zho: '中文', 'zh-cn': '简体中文', 'zh-tw': '繁体中文',
  cmn: '普通话',
  en: '英语', eng: '英语',
  ja: '日语', jpn: '日语',
  ko: '韩语', kor: '韩语',
  yue: '粤语',
  fr: '法语', fra: '法语', fre: '法语',
  de: '德语', deu: '德语', ger: '德语',
  es: '西班牙语', spa: '西班牙语',
  ru: '俄语', rus: '俄语',
  it: '意大利语', ita: '意大利语',
  pt: '葡萄牙语', por: '葡萄牙语',
  th: '泰语', tha: '泰语',
  vi: '越南语', vie: '越南语',
  hi: '印地语', hin: '印地语',
  ar: '阿拉伯语', ara: '阿拉伯语',
  und: '未标注'
};

/** hdlr 里的通用处理器名（ffmpeg 等工具的默认值），不是真正的轨道名 */
const GENERIC_TRACK_NAMES = new Set([
  'soundhandler', 'videohandler', 'texthandler', 'subtitlehandler',
  '', 'und', 'null'
]);

/** 语言代码 → 展示标签；带名称时优先用名称（如 "国语"） */
export function langLabel(code, name) {
  const n = String(name || '').trim();
  if (n && !GENERIC_TRACK_NAMES.has(n.toLowerCase())) return n;
  const c = String(code || '').toLowerCase().trim();
  return LANG_LABELS[c] || (c ? c.toUpperCase() : '未标注');
}

/** 语言代码是否属于中文（默认界面语言） */
export function isChineseLang(code) {
  const c = String(code || '').toLowerCase().trim();
  return c === 'zh' || c === 'chi' || c === 'zho' || c.startsWith('zh-') || c === 'cmn' || c === 'yue';
}

/**
 * 为一个视频文件匹配同目录的外挂字幕（.vtt / .srt）。
 * 匹配规则（宽松同名）：
 *   movie.mp4 → movie.vtt / movie.srt / movie.zh.vtt / movie.chs.srt …
 *   多语言后缀（zh/chs/cht/chi/en/eng/jpn…）用来标注字幕语言。
 * 返回 [{path, ext, lang}]，同前缀多语言时全部返回。
 */
export function matchSubs(videoName, subsInDir) {
  const stem = String(videoName || '').replace(/\.[^.]+$/, '');
  const out = [];
  for (const s of subsInDir || []) {
    if (s.stem === stem) {
      out.push({ path: s.path, ext: s.ext, lang: 'und' });
      continue;
    }
    if (s.stem.toLowerCase().startsWith(stem.toLowerCase() + '.')) {
      // movie.zh.vtt → 后缀 zh
      const tag = s.stem.slice(stem.length + 1).toLowerCase();
      out.push({ path: s.path, ext: s.ext, lang: normLangTag(tag) });
    }
  }
  return out;
}

/** 字幕文件名里的语言标记（chs/cht/sc/tc/zhs/zht/zh/en…）→ ISO 代码 */
function normLangTag(tag) {
  const t = String(tag || '').toLowerCase();
  if (['zh', 'chs', 'cht', 'zhs', 'zht', 'sc', 'tc', 'cn', 'tw', 'hk', 'gb', 'big5', 'chi', 'zho'].includes(t)) return 'zh';
  if (['en', 'eng', 'us', 'uk'].includes(t)) return 'en';
  if (['ja', 'jpn', 'jp'].includes(t)) return 'ja';
  if (['ko', 'kor', 'kr'].includes(t)) return 'ko';
  if (['yue', 'cantonese'].includes(t)) return 'yue';
  if (['fr', 'fra', 'fre'].includes(t)) return 'fr';
  if (['de', 'deu', 'ger'].includes(t)) return 'de';
  if (['es', 'spa'].includes(t)) return 'es';
  if (['ru', 'rus'].includes(t)) return 'ru';
  return 'und';
}

/** 语言代码规范化（轨道 mdhd 语言 / 文件名标记通用） */
export const normLang = normLangTag;

/** srt 文本 → WebVTT 文本（时间戳逗号→点、加 WEBVTT 头） */
export function srtToVtt(srt) {
  const body = String(srt || '')
    .replace(/\r+\n/g, '\n')
    .replace(/^\uFEFF/, '') // BOM
    .replace(/(\d{2}:\d{2}:\d{2}),(\d{3})/g, '$1.$2');
  return `WEBVTT\n\n${body.replace(/\n{3,}/g, '\n\n').trim()}\n`;
}

/**
 * 重写 HLS 播放列表里的相对引用为绝对 asset URL。
 *
 * 为什么必须重写：Tauri 的 convertFileSrc 产出的 asset URL 把整个文件
 * 路径整体编码（%2F 不是路径分隔符），浏览器 / hls.js 按 URL 规则解析
 * 「相对分片名」时会丢失目录信息（/files/<全部编码的路径>/seg000.ts
 * 的「目录」只是 /files/）。因此拿到 m3u8 文本后，把每个非注释行
 * （分片名）与 #EXT-X-KEY / #EXT-X-MAP 的 URI="…" 属性按**文件系统
 * 路径**解析成绝对路径，再各自 convertFileSrc。
 *
 * @param {string} text m3u8 全文
 * @param {string} playlistPath m3u8 的绝对文件路径
 * @param {(absPath: string) => string} toUrl 路径 → asset URL
 */
export function rewriteHlsPlaylist(text, playlistPath, toUrl) {
  const dirParts = String(playlistPath || '')
    .replace(/[\\/]+$/, '')
    .split(/[\\/]/)
    .slice(0, -1); // 播放列表所在目录
  const isWin = /^[a-zA-Z]:[\\/]/.test(playlistPath || '');

  const resolve = (rel) => {
    if (/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(rel)) return rel; // 已是绝对 URL
    const leading = rel.startsWith('/');
    const stack = leading ? [] : [...dirParts];
    for (const p of rel.split('/')) {
      if (p === '' || p === '.') continue;
      if (p === '..') stack.pop();
      else stack.push(p);
    }
    const abs = stack.join('/');
    return leading || !isWin ? `/${abs}` : abs; // 绝对路径带根 /
  };

  return String(text || '')
    .split(/\r?\n/)
    .map((line) => {
      const t = line.trim();
      if (!t) return line;
      if (t.startsWith('#')) {
        // 重写 #EXT-X-KEY / #EXT-X-MAP 等标签里的 URI="…"
        return line.replace(/URI="([^"]+)"/g, (m, uri) => `URI="${toUrl(resolve(uri))}"`);
      }
      return toUrl(resolve(t)); // 分片行
    })
    .join('\n');
}
