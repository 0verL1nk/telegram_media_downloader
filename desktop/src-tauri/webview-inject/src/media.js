// desktop/src-tauri/webview-inject/src/media.js
// 单条消息的媒体识别。返回 chatId/messageId/protected/media[]。
//
// 安全约束:只读取 DOM 结构信息 —— tagName、className、data-* 属性、
// 计算样式(display/visibility/opacity/width)、bounding rect 与 width/height 属性。
// 绝不读取 src / href / currentSrc / cookie / localStorage。

function classTokens(el) {
  const cls = el && el.className;
  return (typeof cls === 'string' ? cls : '').toLowerCase();
}

function parseSize(value) {
  const n = parseFloat(value);
  return Number.isFinite(n) ? n : NaN;
}

// 解析元素的渲染宽度。优先用 rect;元素未参与布局时 rect 为 0,
// 回退到内联样式 width 或 width 属性。
function renderedWidth(el) {
  if (el.getBoundingClientRect) {
    const rect = el.getBoundingClientRect();
    if (rect && rect.width > 0) return rect.width;
  }
  const inline = parseSize(el.style && el.style.width);
  if (Number.isFinite(inline) && inline > 0) return inline;
  const attr = parseSize(el.getAttribute && el.getAttribute('width'));
  if (Number.isFinite(attr) && attr > 0) return attr;
  return 0;
}

// 是否被显式隐藏。注意:
// - 计算 opacity 可能为空字符串,必须用 parseFloat 并检查 isFinite,
//   否则 Number('') === 0 会误判全部元素为不可见。
// - 无 controls 的 <audio> 由 UA 样式计算为 display:none,但它仍是可下载媒体,
//   由调用方对 audio 单独处理,不走此门。
function isHidden(el) {
  if (!el.isConnected) return true;
  const style = getComputedStyle(el);
  if (style.display === 'none' || style.visibility === 'hidden') return true;
  const opacity = parseFloat(style.opacity);
  if (Number.isFinite(opacity) && opacity === 0) return true;
  return false;
}

// 具备可视尺寸的媒体元素(img/video):未隐藏且宽度达到阈值。
function isSizedMedia(el, minWidth) {
  if (isHidden(el)) return false;
  return renderedWidth(el) >= minWidth;
}

// 文档/图标类元素:未隐藏即可(document-icon 常为文字/图标,无独立尺寸)。
function isRenderableMarker(el) {
  return !isHidden(el);
}

function isAvatar(el) {
  return classTokens(el).includes('avatar');
}

function ancestorHasClass(el, needle, maxDepth) {
  for (let n = el, i = 0; n && i < maxDepth; n = n.parentElement, i++) {
    if (classTokens(n).includes(needle)) return true;
  }
  return false;
}

function classifyMedia(el, minWidth) {
  const tag = el.tagName;

  if (tag === 'VIDEO') {
    if (!isSizedMedia(el, minWidth)) return null;
    if (el.loop && el.muted && el.autoplay) return 'animation';
    return 'video';
  }

  if (tag === 'AUDIO') {
    // audio 无视觉盒子,不做尺寸门;由父级 class 判定 voice / 否则 audio。
    if (ancestorHasClass(el, 'voice', 4) || ancestorHasClass(el, 'bubble-audio', 4)) {
      return 'voice';
    }
    return 'audio';
  }

  if (tag === 'IMG') {
    if (isAvatar(el)) return null;
    if (!isSizedMedia(el, minWidth)) return null;
    if (ancestorHasClass(el, 'sticker', 3)) return 'sticker';
    return 'photo';
  }

  if (el.hasAttribute && el.hasAttribute('data-media-type')) {
    const t = el.getAttribute('data-media-type');
    if (t === 'document' || t === 'file') {
      return isRenderableMarker(el) ? 'document' : null;
    }
  }

  const cls = classTokens(el);
  if (cls.includes('document-icon') || cls.includes('attachment')) {
    return isRenderableMarker(el) ? 'document' : null;
  }

  return null;
}

function detectFromMessage(el, minWidth) {
  const media = [];
  const candidates = el.querySelectorAll(
    'video, audio, img, [data-media-type="document"], [data-media-type="file"], .document-icon, .attachment'
  );
  for (const c of candidates) {
    const type = classifyMedia(c, minWidth);
    if (type) media.push({ type, element: c });
  }
  return media;
}

function isProtected(el, depth) {
  for (let node = el, i = 0; node && i < depth; node = node.parentElement, i++) {
    if (node.hasAttribute && node.hasAttribute('data-protected')) return true;
    if (node.getAttribute && node.getAttribute('aria-disabled') === 'true') return true;
    if (classTokens(node).includes('protected')) return true;
  }
  return false;
}

export function detectMessage(el, config) {
  if (!el) return null;
  const cfg = config || {};
  const minWidth = Number.isFinite(cfg.visibleMinWidth) ? cfg.visibleMinWidth : 100;
  const depth = Number.isFinite(cfg.protectedAncestorDepth) ? cfg.protectedAncestorDepth : 5;

  const rawMid = el.getAttribute && el.getAttribute('data-mid');
  if (!rawMid || !/^[1-9]\d{0,18}$/.test(rawMid)) return null;
  const messageId = Number(rawMid);
  if (!Number.isSafeInteger(messageId)) return null;

  let peer = el;
  for (let i = 0; peer && i < 6 && !peer.hasAttribute('data-peer-id'); peer = peer.parentElement, i++) {}
  const chatId = peer && peer.getAttribute('data-peer-id');
  if (!chatId || !/^-?[1-9]\d{0,19}$/.test(chatId)) return null;

  const media = detectFromMessage(el, minWidth);
  if (media.length === 0) return null;

  return { chatId, messageId, protected: isProtected(el, depth), media };
}
