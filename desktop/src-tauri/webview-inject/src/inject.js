// desktop/src-tauri/webview-inject/src/inject.js
// 组装入口:顶层 frame + origin 校验 → 事件订阅 → 启动轮询。
import { bindEvents } from './state.js';
import { startWatcher } from './watcher.js';

(function main() {
  'use strict';
  if (window.top !== window) return;
  const cfg = globalThis.__INJECT_CONFIG__ || {};
  const trusted = cfg.trustedOrigins || ['https://web.telegram.org'];
  if (!trusted.includes(location.origin)) return;
  bindEvents().catch(() => {});
  startWatcher(cfg);
})();
