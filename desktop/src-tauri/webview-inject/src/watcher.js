// desktop/src-tauri/webview-inject/src/watcher.js
// 500ms 轮询:检测查看器/Story/置顶音频,确保按钮存在;首次见到媒体时查询任务状态。
// 媒体切换(查看器内滑动)时重置按钮并解绑旧任务。
// 诊断:检测/附加/点击关键节点经 diag 上报(见 diag.js),未见媒体时每 ~10s 一次心跳。
import { diag } from './diag.js';
import { detectVersion, detectViewer, detectStory, detectPinnedAudio } from './detect.js';
import { getMediaUrl, resolveFileName } from './extract.js';
import { ensureButton, setButtonState, injectButtonStyles } from './button.js';
import { registerTask, releaseButton } from './state.js';
import { queryTaskState } from './task-state.js';
import { runPipeline } from './downloader.js';

const TICK_INTERVAL_MS = 10000;

export function startWatcher(cfg) {
  const version = detectVersion();
  injectButtonStyles();

  // 诊断状态:每个容器只报一次检测结果;首次检测成功前每 ~10s 报一次心跳。
  const loggedContainers = new WeakSet();
  let sawDetection = false;
  let lastTickAt = Date.now();

  const detectAny = () => {
    const detected =
      detectViewer(version, cfg) || detectStory(version, cfg) || detectPinnedAudio(version, cfg);
    if (detected) {
      sawDetection = true;
      const container = detected.container;
      if (container && !loggedContainers.has(container)) {
        loggedContainers.add(container);
        diag(`viewer detected: kind=${detected.kind} source=${detected.source} container=${container.className || container.tagName}`);
      }
    } else if (!sawDetection) {
      const now = Date.now();
      if (now - lastTickAt >= TICK_INTERVAL_MS) {
        lastTickAt = now;
        diag('tick: viewer=false story=false pinned=false');
      }
    }
    return detected;
  };

  const onDownload = async (btn) => {
    diag('click: starting pipeline');
    const detected = detectAny();
    if (!detected) { setButtonState(btn, 'failed'); diag('click: failed — no media detected'); return; }
    const url = getMediaUrl(detected.element, detected.kind);
    if (!url) { setButtonState(btn, 'failed'); diag('click: failed — no media url'); return; }
    const fileName = resolveFileName(url, detected.kind);
    setButtonState(btn, 'submitting');
    try {
      await runPipeline({
        url, fileName, fileType: detected.kind, source: detected.source, cfg,
        onTaskId: (taskId) => { registerTask(taskId, btn); setButtonState(btn, 'queued'); },
      });
    } catch (error) {
      if (error && error.name === 'AbortError') { setButtonState(btn, 'ready'); return; }
      setButtonState(btn, 'failed');
      diag(`click: failed — ${error && error.message ? error.message : String(error)}`);
    }
  };

  const onRetry = async (btn) => {
    setButtonState(btn, 'ready');
    await onDownload(btn);
  };

  let running = false;
  setInterval(async () => {
    if (running) return;
    running = true;
    try {
      const detected = detectAny();
      if (!detected) return;
      const btn = ensureButton(version, cfg, detected, onDownload, onRetry);
      if (!btn) return;
      if (btn.dataset.telDiagLogged !== '1') {
        btn.dataset.telDiagLogged = '1';
        diag(`button attached: state=${btn.dataset.telState}`);
      }
      const url = getMediaUrl(detected.element, detected.kind);
      if (!url) return;
      const fileName = resolveFileName(url, detected.kind);

      // 媒体切换:文件名变化 → 解绑旧任务,重置为 ready 重新查询
      if (btn.dataset.telFileName && btn.dataset.telFileName !== fileName) {
        releaseButton(btn);
        btn.dataset.telChecked = '';
        btn.dataset.telState = 'ready';
        setButtonState(btn, 'ready');
      }
      btn.dataset.telFileName = fileName;

      if (btn.dataset.telState !== 'ready' || btn.dataset.telChecked === '1') return;

      const st = await queryTaskState(fileName, cfg);
      btn.dataset.telChecked = '1';
      if (st.state === 'completed') setButtonState(btn, 'completed');
      else if (st.state === 'failed') setButtonState(btn, 'failed');
      else if (st.state === 'downloading') {
        setButtonState(btn, 'downloading', { progress: st.progress });
        if (st.taskId) registerTask(st.taskId, btn);
      } else if (st.state === 'queued') {
        setButtonState(btn, 'queued');
        if (st.taskId) registerTask(st.taskId, btn);
      }
    } finally {
      running = false;
    }
  }, cfg.refreshDelayMs);
}
