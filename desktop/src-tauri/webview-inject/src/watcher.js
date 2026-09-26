// desktop/src-tauri/webview-inject/src/watcher.js
// 500ms 轮询:检测查看器/Story/置顶音频,确保按钮存在;首次见到媒体时查询任务状态。
// 媒体切换(查看器内滑动)时重置按钮并解绑旧任务。
import { detectVersion, detectViewer, detectStory, detectPinnedAudio } from './detect.js';
import { getMediaUrl, resolveFileName } from './extract.js';
import { ensureButton, setButtonState, injectButtonStyles } from './button.js';
import { registerTask, releaseButton } from './state.js';
import { queryTaskState } from './task-state.js';
import { runPipeline } from './downloader.js';

export function startWatcher(cfg) {
  const version = detectVersion();
  injectButtonStyles();

  const detectAny = () =>
    detectViewer(version, cfg) || detectStory(version, cfg) || detectPinnedAudio(version, cfg);

  const onDownload = async (btn) => {
    const detected = detectAny();
    if (!detected) { setButtonState(btn, 'failed'); return; }
    const url = getMediaUrl(detected.element, detected.kind);
    if (!url) { setButtonState(btn, 'failed'); return; }
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
