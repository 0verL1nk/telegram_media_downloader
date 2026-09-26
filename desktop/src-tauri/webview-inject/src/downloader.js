// desktop/src-tauri/webview-inject/src/downloader.js
// 页面内分块抓取管线:探测大小 → 计划 → 并发 fetch(与脚本 #446342 同款 Range 方式)
// → IPC 推送分块 → 完成。落盘/记账/提交全部在 Rust。
// 诊断:每个阶段经 diag 上报(见 diag.js)。
import { diag } from './diag.js';
import { queryTaskState } from './task-state.js';

const RETRY_BASE_MS = 300;
const RETRY_MAX_MS = 3000;
const PROBE_TIMEOUT_SECONDS = 15;

/** taskId → AbortController,由 webview-download-abort 事件触发中止。 */
const controllers = new Map();

/** 永久性失败(URL 过期/文件消失),不重试。 */
class PermanentError extends Error {}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function fetchWithTimeout(url, range, timeoutSeconds, outerSignal) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutSeconds * 1000);
  const onOuterAbort = () => controller.abort();
  outerSignal?.addEventListener('abort', onOuterAbort, { once: true });
  try {
    return await fetch(url, { headers: { Range: range }, signal: controller.signal });
  } finally {
    clearTimeout(timer);
    outerSignal?.removeEventListener('abort', onOuterAbort);
  }
}

export async function probeTotal(url, signal) {
  const res = await fetchWithTimeout(url, 'bytes=0-0', PROBE_TIMEOUT_SECONDS, signal);
  if (res.status === 200) {
    // 服务器忽略 Range:立刻取消响应体,用 Content-Length 当总大小
    res.body?.cancel?.().catch?.(() => {});
    const len = Number(res.headers.get('Content-Length'));
    if (Number.isSafeInteger(len) && len > 0) return len;
    throw new Error('服务器未返回文件大小');
  }
  if (res.status !== 206) {
    if (res.status === 401 || res.status === 403) {
      throw new PermanentError('URL 已过期或无权限,请重新打开该媒体后再试');
    }
    if (res.status === 404 || res.status === 410) {
      throw new PermanentError('文件已删除或 URL 失效');
    }
    throw new Error(`探测文件大小失败(HTTP ${res.status})`);
  }
  const contentRange = res.headers.get('Content-Range') || '';
  res.body?.cancel?.().catch?.(() => {});
  const match = /^bytes \d+-\d+\/(\d+)$/.exec(contentRange);
  const total = match ? Number(match[1]) : NaN;
  if (!Number.isSafeInteger(total) || total <= 0) {
    throw new Error('服务器返回的文件大小无效');
  }
  return total;
}

async function fetchChunk(url, offset, length, opts) {
  const { timeoutSeconds, retries, signal } = opts;
  let attempt = 0;
  for (;;) {
    if (signal?.aborted) throw new DOMException('已中止', 'AbortError');
    try {
      const res = await fetchWithTimeout(url, `bytes=${offset}-${offset + length - 1}`, timeoutSeconds, signal);
      if (res.status === 206) {
        const buf = await res.arrayBuffer();
        if (buf.byteLength !== length) {
          throw new Error(`分块长度不符(期望 ${length},收到 ${buf.byteLength})`);
        }
        return buf;
      }
      if (res.status === 401 || res.status === 403) {
        throw new PermanentError('URL 已过期或无权限,请重新打开该媒体后再试');
      }
      if (res.status === 404 || res.status === 410) {
        throw new PermanentError('文件已删除或 URL 失效');
      }
      res.body?.cancel?.().catch?.(() => {});
      throw new Error(`HTTP ${res.status}`);
    } catch (error) {
      if (error instanceof PermanentError || signal?.aborted || error?.name === 'AbortError') {
        throw error;
      }
      attempt += 1;
      if (attempt > retries) throw error;
      await sleep(Math.min(RETRY_BASE_MS * 2 ** (attempt - 1), RETRY_MAX_MS));
    }
  }
}

async function runPool(items, limit, worker) {
  const queue = items.slice();
  const count = Math.max(1, Math.min(limit, queue.length));
  const workers = [];
  for (let i = 0; i < count; i += 1) {
    workers.push((async () => {
      while (queue.length > 0) {
        const item = queue.shift();
        await worker(item);
      }
    })());
  }
  await Promise.all(workers);
}

/** webview-download-abort 的入口:中止该任务全部在途 fetch。 */
export function abortTask(taskId) {
  const controller = controllers.get(taskId);
  if (controller) controller.abort();
}

/**
 * 完整下载管线。可续任务(同文件名、resumable)复用 taskId,只补缺块。
 * onTaskId 在任务确定后立刻回调(注册到按钮)。
 */
export async function runPipeline({ url, fileName, fileType, source, cfg, onTaskId }) {
  let taskId = null;
  const existing = await queryTaskState(fileName, cfg).catch(() => null);
  if (existing && existing.taskId && existing.resumable) {
    taskId = existing.taskId;
    diag(`pipeline: resume task=${taskId}`);
  } else {
    const record = await window.__TAURI__.core.invoke('start_webview_download', {
      fileName,
      fileType,
      source,
    });
    taskId = record.taskId;
    diag(`pipeline: created task=${taskId}`);
  }
  onTaskId?.(taskId);

  const controller = new AbortController();
  controllers.set(taskId, controller);
  try {
    const totalBytes = await probeTotal(url, controller.signal);
    diag(`pipeline: total=${totalBytes}`);
    const plan = await window.__TAURI__.core.invoke('plan_chunks', { taskId, totalBytes });
    const missingCount = plan.missing ? plan.missing.length : 0;
    diag(`pipeline: missing=${missingCount} concurrency=${plan.concurrency}`);

    if (!plan.missing || plan.missing.length === 0) {
      await window.__TAURI__.core.invoke('finish_download', { taskId });
      diag(`pipeline: finished task=${taskId} (no missing chunks)`);
      return taskId;
    }

    await runPool(plan.missing, plan.concurrency, async ({ offset, length }) => {
      const buffer = await fetchChunk(url, offset, length, {
        timeoutSeconds: plan.timeoutSeconds,
        retries: plan.retries,
        signal: controller.signal,
      });
      try {
        await window.__TAURI__.core.invoke('push_chunk', new Uint8Array(buffer), {
          headers: { 'x-task-id': taskId, 'x-offset': String(offset) },
        });
      } catch (pushError) {
        diag(`pipeline: push failed offset=${offset} — ${pushError && pushError.message ? pushError.message : String(pushError)}`);
        throw pushError;
      }
    });

    await window.__TAURI__.core.invoke('finish_download', { taskId });
    diag(`pipeline: finished task=${taskId}`);
    return taskId;
  } catch (error) {
    if (controller.signal.aborted || error?.name === 'AbortError') {
      // Rust 侧已暂停/取消(收到 abort 事件);不要覆盖它的状态
      diag(`pipeline: aborted task=${taskId}`);
      throw new DOMException('已中止', 'AbortError');
    }
    controller.abort(); // 停止其余在途 fetch,避免向已失败任务继续推送
    const message = error instanceof Error ? error.message : String(error);
    diag(`pipeline: failed — ${message}`);
    await window.__TAURI__.core.invoke('fail_download', { taskId, error: message }).catch(() => {});
    throw error;
  } finally {
    controllers.delete(taskId);
  }
}
