// desktop/src-tauri/webview-inject/src/detect.js
// 查看器/Story/置顶音频检测。所有 selector 来自 config.json(取自验证脚本 #446342)。
// 返回 { kind, element, container, source } 或 null。
// kind: 'photo' | 'video' | 'animation' | 'voice'
// source: 'viewer' | 'story' | 'pinned-audio'

export function detectVersion() {
  const host = location.hostname;
  if (host === 'webk.telegram.org') return 'webk';
  if (host === 'webz.telegram.org') return 'webz';
  if (location.pathname.startsWith('/k/')) return 'webk';
  return 'webz';
}

function kindOfVideo(video) {
  return video.loop && video.muted ? 'animation' : 'video';
}

export function detectViewer(version, cfg) {
  return version === 'webk' ? detectViewerWebk(cfg.webk) : detectViewerWebz(cfg.webz);
}

function detectViewerWebk(wk) {
  const root = document.querySelector(wk.viewerRoot);
  if (!root) return null;
  const aspecter = root.querySelector(wk.mediaAspecter);
  if (!aspecter) return null;

  const video = aspecter.querySelector('video');
  if (video) {
    const container = root.querySelector(wk.videoControlsRight)
      || root.querySelector(wk.viewerButtons);
    if (!container) return null;
    return { kind: kindOfVideo(video), element: video, container, source: 'viewer' };
  }

  const img = aspecter.querySelector(wk.imgSelector);
  if (img) {
    const container = root.querySelector(wk.viewerButtons);
    if (!container) return null;
    return { kind: 'photo', element: img, container, source: 'viewer' };
  }
  return null;
}

function detectViewerWebz(wz) {
  const slide = document.querySelector(wz.activeSlide);
  if (!slide) return null;

  const video = slide.querySelector('video');
  if (video) {
    const container = document.querySelector(wz.videoControls)
      || document.querySelector(wz.viewerActions);
    if (!container) return null;
    return { kind: kindOfVideo(video), element: video, container, source: 'viewer' };
  }

  const img = slide.querySelector(wz.imgSelector);
  if (img) {
    const container = document.querySelector(wz.viewerActions);
    if (!container) return null;
    return { kind: 'photo', element: img, container, source: 'viewer' };
  }
  return null;
}

export function detectStory(version, cfg) {
  return version === 'webk' ? detectStoryWebk(cfg.webk) : detectStoryWebz(cfg.webz);
}

function detectStoryWebk(wk) {
  const root = document.querySelector(wk.storyRoot);
  if (!root) return null;
  const container = document.querySelector(wk.storyHeader)
    || document.querySelector(wk.storyFooter);
  if (!container) return null;

  const video = root.querySelector(wk.storyVideo) || root.querySelector('video');
  if (video) {
    return { kind: kindOfVideo(video), element: video, container, source: 'story' };
  }
  const img = root.querySelector(wk.storyImage) || lastImage(root);
  if (img) {
    return { kind: 'photo', element: img, container, source: 'story' };
  }
  return null;
}

function detectStoryWebz(wz) {
  const root = document.querySelector(wz.storyRoot);
  if (!root) return null;
  const container = document.querySelector(wz.storyHeader)
    || (wz.storyHeaderFallback && root.querySelector(wz.storyHeaderFallback)?.parentElement);
  if (!container) return null;

  const video = root.querySelector('video');
  if (video) {
    return { kind: kindOfVideo(video), element: video, container, source: 'story' };
  }
  const imgs = root.querySelectorAll(wz.storyImage);
  const img = imgs.length ? imgs[imgs.length - 1] : lastImage(root);
  if (img) {
    return { kind: 'photo', element: img, container, source: 'story' };
  }
  return null;
}

function lastImage(root) {
  const imgs = root.querySelectorAll('img');
  return imgs.length ? imgs[imgs.length - 1] : null;
}

export function detectPinnedAudio(version, cfg) {
  if (version !== 'webk') return null; // 置顶音频为 webk 特性
  const wk = cfg.webk;
  const pinned = document.querySelector(wk.pinnedAudio);
  if (!pinned) return null;
  const audioEl = pinned.querySelector('audio-element') || document.querySelector('audio-element');
  const audio = (audioEl && audioEl.querySelector('.audio')) || pinned.querySelector('audio');
  if (!(audio instanceof HTMLAudioElement)) return null;
  const container = document.querySelector(wk.pinnedAudioUtils) || pinned;
  return { kind: 'voice', element: audio, container, source: 'pinned-audio' };
}
