/**
 * 极速看片 —— ASS/SSA 字幕 → WebVTT cue 文本转换
 *
 * 用 ass-compiler（npm，浏览器安全 ESM）把 ASS 解析成结构化
 * dialogue（slices → fragments，带 tag 继承样式），再映射成 WebVTT
 * 支持的行内富文本（<i>/<b>/<u>）：
 * - 覆盖标签 {\i1}/{b1}/{u1} → 行内标签；
 * - \N / \n 硬换行 → 真换行；
 * - 绘图指令（{\p1}m 0 0 l …{\p0}）→ 整段丢弃（矢量图形无法转文本）；
 * - 其余覆盖标签（颜色 / 位移 / 卡拉OK \k）→ 丢弃（WebVTT 不支持）。
 *
 * 方案取舍：libass-wasm 可完整还原 ASS 特效，但 7.7MB wasm + CJK
 * 字体问题对本项目过重；纯文本 + 基础富标签覆盖绝大多数字幕内容
 * （对话本体），排版特效舍弃。
 */
import { compile } from 'ass-compiler';

/** 秒 → ASS 时间戳 "H:MM:SS.CC"（重组 Dialogue 行用） */
function assStamp(sec) {
  if (!isFinite(sec) || sec < 0) sec = 0;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  const s = Math.floor(sec % 60);
  const cs = Math.round((sec - Math.floor(sec)) * 100);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  return `${h}:${p(m)}:${p(s)}.${p(cs)}`;
}

/**
 * 从 matroska-subtitles 收集到的块（{text,time,duration,layer,style,…}）
 * + 轨道 CodecPrivate 里的 ASS 头，重组出一份完整 ASS 文本。
 * 为什么要重组而不是直接用块 text：块 text 只是 Dialogue 的最后一个
 * 字段，样式 / 图层等信息在其它字段里，丢了它们 ass-compiler 无法
 * 正确解析；重组后 compile() 一次性吃下全部上下文。
 */
export function assDocFromBlocks(header, blocks) {
  const lines = [
    String(header || '').replace(/\s+$/, ''),
    '',
    '[Events]',
    'Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text'
  ];
  const sorted = [...blocks].sort((a, b) => a.time - b.time);
  for (const b of sorted) {
    const start = (b.time || 0) / 1000;
    const end = start + (b.duration || 0) / 1000;
    const text = String(b.text || '').replace(/\r/g, '');
    lines.push(
      `Dialogue: ${b.layer || 0},${assStamp(start)},${assStamp(end)},${b.style || 'Default'},` +
        `${b.name || ''},${b.marginL || 0},${b.marginR || 0},${b.marginV || 0},${b.effect || ''},${text}`
    );
  }
  return lines.join('\n') + '\n';
}

/** 片段文本 → WebVTT 行内文本（\N → 换行，富标签映射，丢弃绘图） */
function fragmentToVtt(fragment, openTags) {
  if (fragment.drawing) return ''; // 绘图指令：无法转文本
  let text = String(fragment.text || '');
  if (!text) return '';
  // \N（硬换行）/ \n（软换行，仅 wrapStyle=2 时换）→ 统一换行保守处理
  text = text.replace(/\\[Nn]/g, '\n');
  // \h 是不间断空格
  text = text.replace(/\\h/g, '\u00a0');
  const tag = fragment.tag || {};
  const out = [];
  if (tag.i && !openTags.i) { out.push('<i>'); openTags.i = true; }
  if (tag.b && !openTags.b) { out.push('<b>'); openTags.b = true; }
  if (tag.u && !openTags.u) { out.push('<u>'); openTags.u = true; }
  out.push(text);
  if (!tag.i && openTags.i) { out.push('</i>'); openTags.i = false; }
  if (!tag.b && openTags.b) { out.push('</b>'); openTags.b = false; }
  if (!tag.u && openTags.u) { out.push('</u>'); openTags.u = false; }
  return out.join('');
}

/** 关闭行内富标签（行尾收口，避免跨 cue 泄漏） */
function closeTags(openTags) {
  let s = '';
  if (openTags.u) s += '</u>';
  if (openTags.b) s += '</b>';
  if (openTags.i) s += '</i>';
  openTags.i = openTags.b = openTags.u = false;
  return s;
}

/**
 * 一份完整 ASS/SSA 文本 → cue 列表 [{start,end,text}]（text 可含
 * <i>/<b>/<u> 与换行，直接喂给 cuesToVtt）。
 * 解析失败时返回空数组（调用方提示无法显示）。
 */
export function assToCues(assText) {
  let compiled;
  try {
    compiled = compile(String(assText || ''), {});
  } catch {
    return [];
  }
  const cues = [];
  for (const d of compiled.dialogues || []) {
    const openTags = { i: false, b: false, u: false };
    let text = '';
    for (const slice of d.slices || []) {
      for (const frag of slice.fragments || []) {
        text += fragmentToVtt(frag, openTags);
      }
    }
    text += closeTags(openTags);
    text = text.replace(/\n{3,}/g, '\n\n').trim();
    if (text && d.end > d.start) {
      cues.push({ start: d.start, end: d.end, text });
    }
  }
  return cues;
}
