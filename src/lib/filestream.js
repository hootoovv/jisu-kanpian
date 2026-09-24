/**
 * 极速看片 —— 顺序流式文件读取器（Rust read_range 驱动）
 *
 * 为什么需要它：MKV 内嵌字幕散布在容器的每一个 Cluster 里，提取必须
 * 顺序扫完整个文件。v1.0.3 及之前的做法是把整个文件一次性读进一个
 * JS ArrayBuffer 再「假装分块」喂给解析器——V8 单个 ArrayBuffer 有约
 * 4GB 硬上限，且整个文件常驻内存有 OOM 风险，只能加 4GB 预检把大文件
 * 直接拒之门外（用户实测 5GB MKV 报「文件过大」即此护栏）。
 *
 * 本模块改用真正的流式读取：循环调用 Rust 的二进制 IPC 命令
 * read_range（commands.rs，单次上限 8MB），拿到一块喂一块。内存占用
 * 恒定在「单块 + 解析状态 + 已收集的字幕文本」，与文件大小完全无关
 * ——几十 GB 的文件也能提取，耗时只受磁盘顺序读速度限制。
 *
 * 用法（异步生成器，消费方 break 即取消）：
 *   for await (const chunk of readFileChunks(path, size)) parser.write(chunk);
 */
import { invoke } from '@tauri-apps/api/core';

/**
 * 默认单块大小（4MB）：IPC 往返次数与主线程单次解析耗时的折中。
 * 5GB 文件 ≈ 1280 块；每块之间有一次 IPC await 天然让出主线程，
 * 播放中提取也不会冻结 UI。
 */
export const STREAM_CHUNK_BYTES = 4 * 1024 * 1024;

/** 单块上限须与 commands.rs 的 MAX_RANGE_LEN（8MB）对齐 */
const IPC_MAX_BYTES = 8 * 1024 * 1024;

/**
 * 顺序分块读取本地文件（异步生成器）。
 * - 块大小内部钳制到 [64KB, IPC 上限]，传入越界值不会破坏读取；
 * - 短读（块不足请求数）或空块按「到文件尾」正常结束；
 * - read_range 本身失败（文件被删 / 网络盘断开）则向上抛错。
 * @param {string} path 文件绝对路径
 * @param {number} totalBytes 文件总大小（stat_file 结果；<=0 直接结束）
 * @param {number} [chunkBytes] 单块大小（默认 4MB；冒烟测试传小值
 *   验证解析器跨块边界的正确性）
 * @yields {Uint8Array}
 */
export async function* readFileChunks(path, totalBytes, chunkBytes = STREAM_CHUNK_BYTES) {
  if (!path || !(totalBytes > 0)) return;
  // 下限 16KB 防退化（1 字节块会放大 IPC 次数）；上限对齐 Rust 端
  const step = Math.max(16 * 1024, Math.min(Number(chunkBytes) || STREAM_CHUNK_BYTES, IPC_MAX_BYTES));
  let offset = 0;
  while (offset < totalBytes) {
    const want = Math.min(step, totalBytes - offset);
    const buf = await invoke('read_range', { path, offset, length: want });
    const chunk = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
    if (!chunk.byteLength) break; // 越界读 / 文件被截断：按到尾处理
    yield chunk;
    offset += chunk.byteLength;
    if (chunk.byteLength < want) break; // 短读 = 到尾
  }
}
