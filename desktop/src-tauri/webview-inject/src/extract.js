// desktop/src-tauri/webview-inject/src/extract.js
// 从查看器媒体元素提取 URL;从 URL 尾部 JSON 解析文件名。
// URL 读取为允许行为(见 HTTP 下载器 spec §7);不读 cookie/storage。

export function getMediaUrl(el, kind) {
  if (!el) return null;
  if (kind === 'video' || kind === 'animation') {
    return el.currentSrc || el.src || el.querySelector?.('source')?.src || null;
  }
  if (kind === 'audio') {
    return el.getAttribute('src') || el.src || null;
  }
  // img
  return el.currentSrc || el.src || null;
}

export function parseFileNameFromUrl(url) {
  if (!url) return null;
  try {
    const tail = url.split('/').pop();
    const decoded = decodeURIComponent(tail);
    const meta = JSON.parse(decoded);
    if (meta && typeof meta.fileName === 'string' && meta.fileName.length > 0) {
      return meta.fileName;
    }
  } catch (_e) {
    // URL 尾部不是 JSON,正常情况,静默
  }
  return null;
}

export function hashCode(str) {
  let h = 0;
  for (let i = 0; i < str.length; i++) {
    h = (h << 5) - h + str.charCodeAt(i);
    h |= 0;
  }
  return h;
}

export function fallbackFileName(url, kind) {
  const ext = kind === 'video' || kind === 'animation' ? 'mp4'
    : kind === 'audio' || kind === 'voice' ? 'ogg'
    : 'jpg';
  return Math.abs(hashCode(url || 'x')).toString(36) + '.' + ext;
}

export function resolveFileName(url, kind) {
  return parseFileNameFromUrl(url) || fallbackFileName(url, kind);
}
