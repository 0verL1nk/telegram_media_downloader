// desktop/src-tauri/webview-inject/src/state.js
// Tauri 任务事件订阅:taskId → 按钮映射,事件驱动按钮状态。
import { setButtonState } from './button.js';

const byTask = new Map();
const activeTaskByButton = new WeakMap();

export function registerTask(taskId, btn) {
  if (!taskId || !btn) return;
  const prev = activeTaskByButton.get(btn);
  if (prev && prev !== taskId) byTask.delete(prev);
  byTask.set(taskId, btn);
  activeTaskByButton.set(btn, taskId);
}

export function releaseButton(btn) {
  const prev = activeTaskByButton.get(btn);
  if (prev) {
    byTask.delete(prev);
    activeTaskByButton.delete(btn);
  }
}

export function bindEvents() {
  const listen = window.__TAURI__.event.listen;

  const update = (event) => {
    const btn = byTask.get(event.payload?.taskId);
    if (!btn) return;
    setButtonState(btn, 'downloading', { progress: event.payload?.progress });
  };

  return Promise.all([
    listen('webview-task-submitted', (event) => {
      const btn = byTask.get(event.payload?.taskId);
      if (btn) setButtonState(btn, 'queued');
    }),
    listen('webview-task-updated', update),
    listen('webview-task-completed', (event) => {
      const btn = byTask.get(event.payload?.taskId);
      if (btn) setButtonState(btn, 'completed');
    }),
    listen('webview-task-failed', (event) => {
      const btn = byTask.get(event.payload?.taskId);
      if (btn) setButtonState(btn, 'failed');
    }),
  ]);
}
