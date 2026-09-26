// desktop/src-tauri/webview-inject/src/diag.js
// 诊断上报:写入应用日志(client.jsonl 的 inject 目标)。任何失败都静默,绝不影响主流程。
let sent = 0;

export function diag(message) {
  try {
    const invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
    if (typeof invoke !== 'function') return;
    sent += 1;
    if (sent > 500) return; // 硬上限,防刷屏
    invoke('webview_log', { message: String(message).slice(0, 480) }).catch(() => {});
  } catch (_e) { /* 静默 */ }
}
