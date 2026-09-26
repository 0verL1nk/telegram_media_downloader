// desktop/src-tauri/webview-inject/src/button.js
// 查看器下载按钮:官方按钮接管(webk)+ 原生风格新建按钮 + 状态机。
//
// 按钮标记逐项取自验证脚本 #446342(Neet-Nestor/Telegram-Media-Downloader,
// src/tel_download.js,下文 L 行号即该文件),不自造版面样式——要么原地接管官方
// 按钮(受限媒体的下载按钮被 Telegram 加 .hide 藏起,脚本 L740-752 的做法),
// 要么用 Telegram 原生按钮类(webk 的 btn-icon/tgico 字体、webz 的 Button/
// translucent-white)新建,自动融入 UI。
// 本模块只叠加状态视觉(tel-download-done / -failed / [data-tel-state])。
//
// 与脚本的关键差异:脚本解除官方按钮的 .hide 后直接 btn.click(),触发浏览器自带
// 下载(L750-753);我们改为"克隆接管"——解除 .hide、克隆官方按钮(克隆不携带
// Telegram 的事件监听)、克隆挂我们的点击、原按钮内联隐藏。位置与外观 100% 官方,
// 但点击一律回调 onDownload / onRetry,由 inject.js 经 Tauri IPC 交给 Rust,
// 绝不触发官方/浏览器下载。
//
// 导出(其他模块依赖):ensureButton / setButtonState / injectButtonStyles

const DOWNLOAD_ICON = '\ue979'; // L39:Telegram km-font 下载字形(私有区字符)

// 状态 → 按钮 title(状态机见 spec §4.5:ready→submitting→queued→downloading→completed/failed)
const STATE_TITLES = {
  ready: '下载',
  submitting: '提交中…',
  queued: '已加入队列',
  completed: '已下载',
  failed: '重试',
};

// 去重标记:所有注入按钮都带此类(ensureButton 与 watcher 均按 '.tel-download' 查询)
const MARKER = 'tel-download';

export function ensureButton(version, cfg, detected, onDownload, onRetry) {
  const container = detected && detected.container;
  if (!container) return null;

  // 作用域 = 检测容器(+ 查看器顶栏 / webz Actions 条):既查我们自己已注入的
  // 按钮(含接管克隆),也查官方按钮可能出现的位置(webk 视频时两者不同)
  const scopes = buttonScopes(version, cfg, detected, container);
  for (const scope of scopes) {
    const existing = scope.querySelector('.' + MARKER);
    if (existing) return existing;
  }

  // 查看器:优先接管官方下载按钮(受限媒体即被 .hide 藏起的那个)
  if (!detected.source || detected.source === 'viewer') {
    for (const scope of scopes) {
      const official = findOfficialButton(version, scope);
      if (official) return takeOverOfficial(official, onDownload, onRetry);
    }
  }

  // 没有官方按钮(或 Story/置顶音频):按脚本标记新建原生风格按钮
  const btn = createNativeButton(version, cfg, detected);
  bindClick(btn, onDownload, onRetry);
  insertButton(version, cfg, detected, container, btn);
  return btn;
}

// 状态视觉常量(done/failed 覆盖原生按钮颜色)
const DONE_COLOR = '#4dcd5e';
const FAILED_COLOR = '#e53935';
const PULSE_OPACITY = 0.5;

export function setButtonState(btn, state, payload) {
  if (!btn) return;
  btn.dataset.telState = state;
  let title = STATE_TITLES[state];
  if (state === 'downloading') {
    const progress = payload && payload.progress;
    title = typeof progress === 'number'
      ? `下载中 ${Math.round(progress * 100)}%`
      : '下载中';
  }
  btn.title = title || STATE_TITLES.ready;
  btn.classList.toggle('tel-download-progress', state === 'downloading');
  btn.classList.toggle('tel-download-done', state === 'completed');
  btn.classList.toggle('tel-download-failed', state === 'failed');
}

// 注入微型状态样式(幂等);基础外观全部来自原生类,此处不接管。
// 返回 false 表示 DOM 尚未就绪(document.head 与 document.documentElement 都缺失),
// 由调用方在轮询中重试;本函数自身绝不抛出。
export function injectButtonStyles() {
  if (document.getElementById('tel-download-style')) return true;
  const root = document.head || document.documentElement;
  if (!root) return false; // DOM 尚未就绪,由调用方在轮询中重试
  const style = document.createElement('style');
  style.id = 'tel-download-style';
  style.textContent = `
    .tel-download[data-tel-state="downloading"] { animation: tel-pulse 1s infinite; }
    .tel-download.tel-download-done { color: ${DONE_COLOR} !important; }
    .tel-download.tel-download-failed { color: ${FAILED_COLOR} !important; }
    @keyframes tel-pulse { 50% { opacity: ${PULSE_OPACITY}; } }
  `;
  root.appendChild(style);
  return true;
}

// ---------------------------------------------------------------------------
// 内部:官方按钮接管 + 按 版本/来源(viewer/story/pinned-audio)复刻脚本标记与插入位置
// ---------------------------------------------------------------------------

function bindClick(btn, onDownload, onRetry) {
  btn.addEventListener('click', (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (btn.dataset.telState === 'failed') {
      onRetry?.(btn);
      return;
    }
    onDownload?.(btn);
  });
}

// 查找范围:检测容器,外加官方按钮实际所在的工具条
// (webk 视频:容器是播放器控制条,官方按钮在 .media-viewer-topbar .media-viewer-buttons;
//  webz 视频:容器是 .VideoPlayerControls .buttons,官方按钮在 .MediaViewerActions)
function buttonScopes(version, cfg, detected, container) {
  const scopes = [container];
  if (detected.source && detected.source !== 'viewer') return scopes;
  const vcfg = perVersion(cfg, version);
  const toolbarSel = version === 'webk' ? vcfg.viewerButtons : vcfg.viewerActions;
  const toolbar = toolbarSel ? document.querySelector(toolbarSel) : null;
  if (toolbar && toolbar !== container) scopes.push(toolbar);
  return scopes;
}

// 定位官方下载按钮。
// webk:脚本在 button.btn-icon 中按字形文本匹配隐藏按钮(textContent === DOWNLOAD_ICON,
//       L743-749);已可见的官方按钮带 tgico-download 类(脚本查重条件,L789)。
// webz:脚本按 title="Download" 识别官方按钮(L582 / L597)。
function findOfficialButton(version, scope) {
  if (!scope || !scope.querySelectorAll) return null;
  if (version === 'webk') {
    for (const btn of scope.querySelectorAll('button.btn-icon')) {
      if (btn.classList.contains(MARKER)) continue;
      if (btn.textContent === DOWNLOAD_ICON || btn.classList.contains('tgico-download')) {
        return btn;
      }
    }
    return null;
  }
  for (const btn of scope.querySelectorAll('button[title="Download"]')) {
    if (btn.classList.contains(MARKER)) continue;
    return btn;
  }
  return null;
}

// 接管官方按钮:解除 .hide(L744)→ 克隆(克隆不携带 Telegram 的事件监听)→
// 加标记、绑我们的点击、插到原按钮之后 → 原按钮内联隐藏。
// 位置与外观 100% 保持官方,但点击永远走 onDownload/onRetry(Rust),
// 不做脚本 L750-753 的 btn.click()(官方/浏览器下载)。
function takeOverOfficial(official, onDownload, onRetry) {
  official.classList.remove('hide');
  const clone = official.cloneNode(true);
  clone.classList.remove('hide');
  clone.classList.add(MARKER);
  if (clone.textContent === DOWNLOAD_ICON) {
    clone.classList.add('tgico-download'); // 镜像脚本 L749:为字形补上图标类
  }
  clone.style.display = ''; // 官方按钮若被内联隐藏,克隆不应继承
  bindClick(clone, onDownload, onRetry);
  official.after(clone);
  official.style.display = 'none'; // 原按钮留位不可见,避免出现重复按钮
  setButtonState(clone, 'ready');
  return clone;
}

function createNativeButton(version, cfg, detected) {
  const { className, innerHTML } = buttonMarkup(version, perVersion(cfg, version), detected);
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = className;
  btn.dataset.telState = 'ready';
  btn.title = STATE_TITLES.ready;
  btn.setAttribute('aria-label', '下载');
  btn.innerHTML = innerHTML;
  return btn;
}

function buttonMarkup(version, vcfg, detected) {
  const source = detected.source || 'viewer';

  if (version === 'webz') {
    const icon = '<i class="icon icon-download"></i>'; // L550-551
    if (source === 'story') {
      // L494-496:story 为 tiny + CSS-module 哈希类
      return {
        className: `Button TkphaPyQ tiny translucent-white round ${MARKER}`,
        innerHTML: icon,
      };
    }
    // L550-554:查看器顶栏(视频时容器为 .VideoPlayerControls .buttons,类不变)
    return {
      className: vcfg.buttonClass || `Button smaller translucent-white round ${MARKER}`,
      innerHTML: icon,
    };
  }

  // ---- webk ----
  if (source === 'story') {
    // L689-691:无 tgico-download 类,字形来自内嵌 span;带 c-ripple 层
    return {
      className: `btn-icon rp ${MARKER}`,
      innerHTML: `<span class="tgico">${DOWNLOAD_ICON}</span><div class="c-ripple"></div>`,
    };
  }
  if (source === 'pinned-audio') {
    // L648-650:_tel_download_button_pinned_container 为脚本自身的防重标记
    return {
      className: `btn-icon tgico-download _tel_download_button_pinned_container ${MARKER}`,
      innerHTML: `<span class="tgico button-icon">${DOWNLOAD_ICON}</span>`,
    };
  }
  if (within(detected.container, vcfg.videoControlsRight)) {
    // L770-773:查看器视频控制条变体(default__button)
    return {
      className: `btn-icon default__button tgico-download ${MARKER}`,
      innerHTML: `<span class="tgico">${DOWNLOAD_ICON}</span>`,
    };
  }
  // L793-795 / L816-818:查看器顶栏(photo / 未加载 video / GIF)
  return {
    className: vcfg.buttonClass || `btn-icon tgico-download ${MARKER}`,
    innerHTML: `<span class="tgico button-icon">${DOWNLOAD_ICON}</span>`,
  };
}

function insertButton(version, cfg, detected, container, btn) {
  const source = detected.source || 'viewer';

  if (source === 'pinned-audio') {
    container.appendChild(btn); // 脚本:appendChild 进 .pinned-container-wrapper-utils
    return;
  }
  if (source === 'story') {
    if (version === 'webz') {
      // L525-527:insertBefore 头部第一个 button;无 button 时保底 prepend
      const first = container.querySelector('button');
      if (first) container.insertBefore(btn, first);
      else container.prepend(btn);
      return;
    }
    container.prepend(btn); // L717-718(头部)/ L724-725(底部)
    return;
  }

  // 查看器:webz 视频控制条插到 .spacer 之后(L566-571),其余一律 prepend
  if (version === 'webz' && within(container, perVersion(cfg, version).videoControls)) {
    const spacer = container.querySelector('.spacer');
    if (spacer) {
      spacer.after(btn);
      return;
    }
  }
  container.prepend(btn); // L600 / L633(webz);L784 / L806 / L829(webk)
}

function perVersion(cfg, version) {
  return (cfg && cfg[version]) || {};
}

function within(el, selector) {
  return !!(el && selector && el.closest && el.closest(selector));
}
