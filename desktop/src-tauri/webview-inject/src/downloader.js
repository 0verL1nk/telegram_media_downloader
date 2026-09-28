// desktop/src-tauri/webview-inject/src/downloader.js
// 页面内分块抓取管线:探测大小 → 计划 → 并发 fetch(与脚本 #446342 同款 Range 方式)
// → IPC 推送分块 → 完成。落盘/记账/提交全部在 Rust。
// 诊断:每个阶段经 diag 上报(见 diag.js)。
import { diag } from './diag.js';
import { queryTaskState } from './task-state.js';

const RETRY_BASE_MS = 300;
const RETRY_MAX_MS = 3000;
const PROBE_TIMEOUT_SECONDS = 15;
/// 页面侧心跳间隔:抓取在途但暂无完整分块时,告诉 Rust"页面还活着",
/// 避免 Rust 看门狗把慢连接误判成页面消失。
const HEARTBEAT_MS = 10000;
/// 停滞判定:超过该时长既无分块入账,即认为抓取已经卡死(媒体切换/连接中断)。
/// 必须长于单次 fetch 超时(45s),否则慢而健康的连接会被误判;心跳已保证
/// Rust 侧不会因为"字节来得慢"而误暂停,这里的职责是把真正的死局变成明确失败。
const STALL_LIMIT_MS = 60000;
const STALL_CHECK_MS = 5000;
const STALL_MESSAGE = '下载停滞(媒体可能已切换或网络中断)。重新打开该媒体后再点下载即可从断点继续。';
/// URL 缓存上限:文件名 → { url, fileType, source }。URL 从不进入 Rust:
/// 它只保存在 Telegram 页面自己的 localStorage 里,重启后由页面自己恢复。
const URL_CACHE_LIMIT = 200;
const URL_STORE_KEY = 'tmd.mediaUrls.v1';
/// 持久化条目的保留期(天):只用于控制缓存规模,不代表"链接有效期"——
/// 链接是否失效由服务端决定,页面按 401/403 走既有的"重新打开媒体"提示。
const URL_STORE_TTL_DAYS = 30;

/** taskId → AbortController,由 webview-download-abort 事件触发中止。 */
const controllers = new Map();

/** 文件名 → 媒体 URL(内存态;写入时同步持久化到 localStorage)。 */
const mediaUrls = new Map();

/// 同一媒体短时间内只自动续传一次(避免 500ms 轮询重复开跑)。
const autoResumeAt = new Map();
const AUTO_RESUME_GUARD_MS = 15000;

let lastPersistAt = 0;

function persistMediaUrls() {
  const now = Date.now();
  if (now - lastPersistAt < 2000) return;
  lastPersistAt = now;
  try {
    const entries = [...mediaUrls.entries()].map(([fileName, entry]) => ({ fileName, ...entry }));
    localStorage.setItem(URL_STORE_KEY, JSON.stringify(entries.slice(-URL_CACHE_LIMIT)));
  } catch (_error) {
    /* 隐私模式或配额不足:只影响重启后的自动续传 */
  }
}

/** 启动时恢复持久化的 URL 缓存。 */
export function restoreMediaUrls() {
  try {
    const raw = localStorage.getItem(URL_STORE_KEY);
    if (!raw) return;
    const entries = JSON.parse(raw);
    if (!Array.isArray(entries)) return;
    const cutoff = Date.now() - URL_STORE_TTL_DAYS * 86400_000;
    for (const entry of entries) {
      if (!entry || typeof entry.fileName !== 'string' || typeof entry.url !== 'string') continue;
      if (typeof entry.savedAt === 'number' && entry.savedAt < cutoff) continue;
      mediaUrls.set(entry.fileName, {
        url: entry.url,
        fileType: entry.fileType || 'file',
        source: entry.source || 'viewer',
        savedAt: entry.savedAt,
      });
    }
    if (mediaUrls.size > 0) diag(`restore: ${mediaUrls.size} cached media url(s)`);
  } catch (_error) {
    /* 缓存损坏:忽略 */
  }
}

/** 记住当前/最近一次解析到的媒体 URL(查看器打开时每个 tick 都会刷新)。 */
export function rememberMedia(fileName, url, fileType, source) {
  if (!fileName || !url) return;
  const previous = mediaUrls.get(fileName);
  if (previous && previous.url === url) return;
  if (mediaUrls.size >= URL_CACHE_LIMIT && !mediaUrls.has(fileName)) {
    const oldest = mediaUrls.keys().next().value;
    if (oldest !== undefined) mediaUrls.delete(oldest);
  }
  mediaUrls.set(fileName, {
    url,
    fileType: fileType || 'file',
    source: source || 'viewer',
    savedAt: Date.now(),
  });
  persistMediaUrls();
}

/** 客户端"继续/重试"通知:URL 已知(当前媒体或持久化缓存)时直接续传。 */
export async function resumeFromCache(taskId, fileName) {
  if (!taskId || !fileName || controllers.has(taskId)) return;
  const cached = mediaUrls.get(fileName);
  if (!cached) {
    diag(`resume: no cached url for ${fileName}`);
    return;
  }
  diag(`resume: starting pipeline for ${fileName}`);
  try {
    await runPipeline({ ...cached, fileName, cfg: globalThis.__INJECT_CONFIG__ || {} });
  } catch (error) {
    diag(`resume: failed — ${error && error.message ? error.message : String(error)}`);
  }
}

/**
 * 打开媒体时的自动续传入口(watcher 在每个 tick 调用,带 15 秒去重):
 * 该文件若处于排队状态,直接开跑 —— "排队中"从此意味着"打开该媒体即继续"。
 */
export function maybeAutoResume(fileName, state, cfg) {
  if (!fileName || !state || state.state !== 'queued' || !state.taskId) return;
  if (controllers.has(state.taskId)) return;
  const cached = mediaUrls.get(fileName);
  if (!cached) return;
  const last = autoResumeAt.get(fileName) ?? 0;
  if (Date.now() - last < AUTO_RESUME_GUARD_MS) return;
  autoResumeAt.set(fileName, Date.now());
  diag(`auto-resume: ${fileName}`);
  void runPipeline({ ...cached, fileName, cfg: cfg || globalThis.__INJECT_CONFIG__ || {} }).catch(() => undefined);
}

/** 启动扫描:缓存里对得上、且任务处于排队的文件,全部开跑。
 * 每个任务的并发受"分块并发上限"限制,Rust 端全局流预算会按活跃任务数自动均分,
 * 这里的唯一约束是"确实缓存里有 URL"——启动时让所有排队任务都自愈比人为设一个
 * 数字更符合"任务一旦排队就继续跑"的语义。
 */
export async function autoResumeQueued(cfg) {
  const config = cfg || globalThis.__INJECT_CONFIG__ || {};
  const cutoff = Date.now() - URL_STORE_TTL_DAYS * 86400_000;
  const candidates = [...mediaUrls.entries()]
    .filter(([, entry]) => !entry.savedAt || entry.savedAt >= cutoff)
    .sort((a, b) => (b[1].savedAt ?? 0) - (a[1].savedAt ?? 0));
  let resumed = 0;
  for (const [fileName, entry] of candidates) {
    const state = await queryTaskState(fileName, config).catch(() => null);
    if (state && state.state === 'queued' && state.taskId && !controllers.has(state.taskId)) {
      diag(`boot auto-resume: ${fileName}`);
      resumed += 1;
      autoResumeAt.set(fileName, Date.now());
      void runPipeline({ ...entry, fileName, cfg: config }).catch(() => undefined);
    }
  }
  if (resumed > 0) diag(`boot auto-resume: started ${resumed} task(s)`);
}

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
  const startedAt = performance.now();
  const res = await fetchWithTimeout(url, 'bytes=0-0', PROBE_TIMEOUT_SECONDS, signal);
  // 响应头到达耗时 ≈ RTT;Rust 端用它做 BDP 分块估算(单路速率 × RTT)。
  const probeRttMs = Math.max(1, Math.round(performance.now() - startedAt));
  if (res.status === 200) {
    // 服务器忽略 Range:立刻取消响应体,用 Content-Length 当总大小
    res.body?.cancel?.().catch?.(() => {});
    const len = Number(res.headers.get('Content-Length'));
    if (Number.isSafeInteger(len) && len > 0) return { totalBytes: len, probeRttMs };
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
  return { totalBytes: total, probeRttMs };
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

/** taskId → 并发宽度控制器 { current, max, pump }。 */
const widthControllers = new Map();

/** 自适应并发事件入口(webview-task-concurrency):按 Rust 决策调整线程池宽度。 */
export function setTaskConcurrency(taskId, width) {
  const ctl = widthControllers.get(taskId);
  if (!ctl) return;
  const next = Math.max(1, Math.min(Number(width) || ctl.current, ctl.max));
  if (next === ctl.current) return;
  ctl.current = next;
  ctl.pump?.();
}

/**
 * 可变宽度线程池:并发度由 ctl.current 决定。
 * 缩小时不打断在途分块(等 worker 自然结束);放大时立即补开 worker。
 */
async function runAdaptivePool(items, ctl, worker) {
  const queue = items.slice();
  const running = new Set();
  let failure = null;
  let notifyIdle = null;

  const spawn = () => {
    while (
      !failure &&
      queue.length > 0 &&
      running.size < Math.max(1, Math.min(ctl.current, queue.length + running.size))
    ) {
      const run = async () => {
        while (!failure && queue.length > 0) {
          const item = queue.shift();
          await worker(item);
        }
      };
      const task = run()
        .catch((error) => { failure = error; })
        .finally(() => {
          running.delete(task);
          if (running.size === 0) notifyIdle?.();
        });
      running.add(task);
    }
  };

  ctl.pump = spawn;
  try {
    spawn();
    while (running.size > 0) {
      const idle = new Promise((resolve) => { notifyIdle = resolve; });
      if (running.size > 0) await idle;
    }
    if (failure) throw failure;
  } finally {
    ctl.pump = null;
  }
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
  rememberMedia(fileName, url, fileType, source);

  const controller = new AbortController();
  controllers.set(taskId, controller);
  let stallReason = null;
  let lastProgressAt = Date.now();
  let heartbeatFailures = 0;
  const heartbeat = setInterval(() => {
    void window.__TAURI__.core.invoke('webview_download_heartbeat', { taskId })
      .then(() => {
        if (heartbeatFailures > 0) {
          diag(`pipeline: heartbeat recovered after ${heartbeatFailures} failure(s) task=${taskId}`);
          heartbeatFailures = 0;
        }
      })
      .catch((error) => {
        heartbeatFailures += 1;
        if (heartbeatFailures === 1 || heartbeatFailures % 6 === 0) {
          const message = error && error.message ? error.message : String(error);
          diag(`pipeline: heartbeat failed count=${heartbeatFailures} task=${taskId} — ${message}`);
        }
      });
  }, HEARTBEAT_MS);
  // 停滞自检:媒体切换/连接中断时,宁可给出明确的失败原因,也不要无声挂死
  // (无声挂死时由停滞检测显式失败,不依赖 Rust 看门狗超时)。
  const stallWatchdog = setInterval(() => {
    if (!controller.signal.aborted && Date.now() - lastProgressAt >= STALL_LIMIT_MS) {
      stallReason = STALL_MESSAGE;
      controller.abort();
    }
  }, STALL_CHECK_MS);
  try {
    const { totalBytes, probeRttMs } = await probeTotal(url, controller.signal);
    diag(`pipeline: total=${totalBytes} rtt=${probeRttMs}ms`);
    const plan = await window.__TAURI__.core.invoke('plan_chunks', {
      taskId,
      totalBytes,
      probeRttMs,
    });
    const missingCount = plan.missing ? plan.missing.length : 0;
    diag(`pipeline: missing=${missingCount} concurrency=${plan.concurrency}/${plan.maxConcurrency}`);

    if (!plan.missing || plan.missing.length === 0) {
      await window.__TAURI__.core.invoke('finish_download', { taskId });
      diag(`pipeline: finished task=${taskId} (no missing chunks)`);
      return taskId;
    }

    // 宽度控制器:初值来自 plan,后续由 Rust 的 webview-task-concurrency 事件调整。
    const widthCtl = {
      current: Math.max(1, plan.concurrency || 1),
      max: Math.max(1, plan.maxConcurrency || plan.concurrency || 1),
      pump: null,
    };
    widthControllers.set(taskId, widthCtl);

    await runAdaptivePool(plan.missing, widthCtl, async ({ offset, length }) => {
      const buffer = await fetchChunk(url, offset, length, {
        timeoutSeconds: plan.timeoutSeconds,
        retries: plan.retries,
        signal: controller.signal,
      });
      try {
        await window.__TAURI__.core.invoke('push_chunk', new Uint8Array(buffer), {
          headers: { 'x-task-id': taskId, 'x-offset': String(offset) },
        });
        lastProgressAt = Date.now();
      } catch (pushError) {
        diag(`pipeline: push failed offset=${offset} — ${pushError && pushError.message ? pushError.message : String(pushError)}`);
        throw pushError;
      }
    });

    await window.__TAURI__.core.invoke('finish_download', { taskId });
    diag(`pipeline: finished task=${taskId}`);
    return taskId;
  } catch (error) {
    if (stallReason) {
      diag(`pipeline: failed — ${stallReason}`);
      await window.__TAURI__.core.invoke('fail_download', { taskId, error: stallReason, permanent: false }).catch(() => {});
      throw new Error(stallReason);
    }
    if (controller.signal.aborted || error?.name === 'AbortError') {
      // Rust 侧已暂停/取消(收到 abort 事件);不要覆盖它的状态
      diag(`pipeline: aborted task=${taskId}`);
      throw new DOMException('已中止', 'AbortError');
    }
    controller.abort(); // 停止其余在途 fetch,避免向已失败任务继续推送
    const message = friendlyFetchError(error);
    diag(`pipeline: failed — ${message}`);
    // 永久性失败(URL 过期/文件删除)重试没有意义;其余交给客户端自动重试。
    await window.__TAURI__.core.invoke('fail_download', {
      taskId,
      error: message,
      permanent: error instanceof PermanentError,
    }).catch(() => {});
    throw error instanceof Error ? error : new Error(message);
  } finally {
    clearInterval(heartbeat);
    clearInterval(stallWatchdog);
    controllers.delete(taskId);
    widthControllers.delete(taskId);
  }
}

/** 网络类报错(Failed to fetch 等)对用户没有意义,统一换成可行动的文案。 */
function friendlyFetchError(error) {
  const text = error instanceof Error ? error.message : String(error);
  if (error instanceof TypeError || /failed to fetch|networkerror|network error|load failed/i.test(text)) {
    return '网络中断或连接失败。网络恢复后在客户端点「重试」即可从断点继续(无需重新打开媒体)。';
  }
  return text;
}
