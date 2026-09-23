/**
 * 极速看片 —— 展示格式化工具（信息栏 / 控制栏 / 文件列表共用）
 *
 * 时间字段由 <video> 元素返回（秒），显示格式统一在这里处理，
 * 便于调整样式。
 */

/** 数值钳制 */
export function clamp(v, lo, hi) {
  return Math.max(lo, Math.min(hi, v));
}

/** 秒 → "HH:MM:SS"（不足一小时也补足两位小时，如 00:12:34） */
export function formatTime(sec) {
  if (sec === null || sec === undefined || !isFinite(sec) || sec < 0) return '--:--:--';
  const s = Math.floor(sec);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(Math.floor(s / 3600))}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}`;
}

/** 秒 → 进度百分比字符串（"37.2%"）；时长未知时返回 "--" */
export function formatPercent(sec, duration) {
  if (!duration || !isFinite(duration) || duration <= 0) return '--';
  return `${((sec / duration) * 100).toFixed(1)}%`;
}

/** 毫秒 → 断点短时间（"12:34"；满一小时 "1:02:33"），用于列表徽标 */
export function formatPosShort(ms) {
  if (!ms || ms <= 0) return '';
  const s = Math.floor(ms / 1000);
  const p = (n) => String(n).padStart(2, '0');
  const h = Math.floor(s / 3600);
  return h > 0 ? `${h}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}` : `${p(Math.floor(s / 60))}:${p(s % 60)}`;
}

/** 缩放倍数 → 短标签（"1.00×" / "1.25×"） */
export function formatZoom(z) {
  if (!isFinite(z) || z <= 0) return '1.00×';
  return `${z.toFixed(2)}×`;
}
