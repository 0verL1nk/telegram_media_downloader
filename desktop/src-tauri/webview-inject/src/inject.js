// desktop/src-tauri/webview-inject/src/inject.js
// 组装入口:DOM 标记 + 启动诊断 → 顶层 frame + origin 校验 → 事件订阅 → 启动轮询。
import { diag } from './diag.js';
import { detectVersion } from './detect.js';
import { bindEvents } from './state.js';
import { startWatcher } from './watcher.js';
import { restoreMediaUrls, autoResumeQueued } from './downloader.js';

(function main() {
  'use strict';
  try { document.documentElement.setAttribute('data-tmd-inject', 'v0.3.3'); } catch (_e) { /* 忽略 */ }
  diag(`boot: origin=${location.origin} href=${location.pathname} top=${window.top === window} hasTauri=${typeof window.__TAURI__ !== 'undefined'}`);
  if (window.top !== window) { diag('boot: skip — not top frame'); return; }
  const cfg = globalThis.__INJECT_CONFIG__ || {};
  const trusted = cfg.trustedOrigins || ['https://web.telegram.org'];
  if (!trusted.includes(location.origin)) { diag(`boot: skip — untrusted origin ${location.origin}`); return; }
  diag(`boot: ok — version=${detectVersion()}`);
  bindEvents().catch((e) => diag(`bindEvents error: ${e && e.message}`));
  try {
    startWatcher(cfg);
  } catch (error) {
    diag(`startWatcher threw: ${error && error.message ? error.message : String(error)}`);
  }
  // URL 缓存持久化在页面自己的 localStorage:恢复后,排队中的任务可以自动续传。
  restoreMediaUrls();
  setTimeout(() => {
    void autoResumeQueued(cfg).catch((error) => diag(`autoResumeQueued error: ${error && error.message}`));
  }, 3000);
})();
