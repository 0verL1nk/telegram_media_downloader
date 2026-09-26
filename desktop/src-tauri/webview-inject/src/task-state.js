// desktop/src-tauri/webview-inject/src/task-state.js
// 按文件名查询 Rust 侧任务状态(queued/downloading/completed/failed/none)。
// 5s TTL 缓存,避免查看器轮询重复查询。

const cache = new Map();
const DEFAULT_TTL_MS = 5000;

export async function queryTaskState(fileName, cfg) {
  if (!fileName) return { state: 'none' };
  const ttl = (cfg && cfg.dedupeCacheTtlMs) ?? DEFAULT_TTL_MS;
  const now = Date.now();
  const hit = cache.get(fileName);
  if (hit && now - hit.at < ttl) return hit.value;
  let value = { state: 'none' };
  try {
    value = await window.__TAURI__.core.invoke('webview_query_task_state', { fileName });
  } catch (_e) {
    value = { state: 'none' };
  }
  cache.set(fileName, { at: now, value });
  return value;
}
