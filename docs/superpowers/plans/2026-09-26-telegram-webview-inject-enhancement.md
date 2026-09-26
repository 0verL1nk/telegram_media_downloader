# Telegram WebView 下载器(HTTP 方案) — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> ## ⚠️ 2026-09-26 修订(第二次)
>
> 第一版计划(聊天列表消息按钮 + MTProto)已作废。原因:
> 1. 聊天列表只有缩略图,拿不到原图 URL — 技术不可行
> 2. 验证过的油猴脚本(#446342,23.9 万安装)全程只做**媒体查看器路径**
> 3. 下载后端改为 HTTP(Telegram Web 文件 URL + Range 分块),不再走 MTProto
>
> **不做单元测试**(JS 侧)。验证 = 真机 `npx tauri dev` 手工逐项确认。
> Rust 侧保留 `cargo test` 惯例(纯逻辑 + 本地 mock server)。
>
> 已完成且保留:
> - Task A(旧):脚手架 `webview-inject/` 目录 ✅
> - `icons.js`(11 个内联 SVG)✅
> - 已删除:vitest 基础设施、media.js(消息检测,方案作废)、fixtures
>
> 规范: `docs/superpowers/specs/2026-09-26-telegram-webview-inject-enhancement-design.md`(注入脚本)
>       `docs/superpowers/specs/2026-09-26-webview-http-downloader-design.md`(下载后端)

**Goal:** 把验证过的油猴脚本做法(#446342)搬进桌面客户端:查看器工具栏按钮 → 抓原始文件 URL → Rust HTTP 分块下载(任务队列/断点续传/并发/校验)。

**Architecture:** 注入脚本 500ms 轮询检测媒体查看器/Story/置顶音频,向原生工具栏注入下载按钮;点击时读取媒体元素 `src`/`currentSrc` 与文件名,经 Tauri IPC 交给 Rust;Rust 用 reqwest + Range 头分块下载,复用现有 chunk_writer/task_store;事件回灌按钮状态。

**Tech Stack:** Rust(edition 2024)+ Tauri 2.11.6 + tokio + SeaORM + reqwest;vanilla JavaScript(ES2022);Node + terser(仅打包)。

---

## 文件结构

### JS 注入脚本(desktop/src-tauri/webview-inject/)

| 路径 | 职责 | 状态 |
|---|---|---|
| `src/icons.js` | 11 个内联 SVG | ✅ 已有 |
| `config.json` | webk/webz 双套 selector + 参数 | 改写 |
| `src/boot.js` | origin 校验 + 版本判定 + 启动 | 新建 |
| `src/watcher.js` | 500ms 轮询:查看器/Story/置顶音频 | 新建 |
| `src/detect.js` | 查看器内媒体检测(webk/webz) | 新建 |
| `src/extract.js` | URL 提取 + 文件名解析 | 新建 |
| `src/button.js` | 原生风格按钮 + 状态机 | 重写 |
| `src/state.js` | Tauri 事件订阅(taskId → 按钮) | 重写 |
| `src/dedupe.js` | 按文件名去重 + TTL 缓存 | 重写 |
| `src/inject.js` | 组装入口 | 重写 |
| `build.mjs` | concat + terser → dist/inject.js | 新建 |
| `README.md` | 已有(已更新验证方式) | ✅ |

删除:`src/media.js`(消息检测方案作废)。

### Rust(desktop/src-tauri/src/)

| 路径 | 改动 | 说明 |
|---|---|---|
| `downloader.rs` | 改造 | 保留调度骨架(TaskControl/限流/分块编排/进度事件);`fetch_chunk` 换 reqwest;删 `refresh_location`/`verify_telegram_hashes`;任务创建改为 URL 基 |
| `commands.rs` | 重构 | `submit_download_from_webview` 新 payload;删 batch/upload/forward/cloud/login 命令 |
| `task_store.rs` | 扩展 | 新增 `media_url` / `file_name` 列 + `find_completed_by_file_name` |
| `db_migration.rs` | 扩展 | 新列迁移 |
| `app_state.rs` | 调整 | 删 `telegram()`;接线新下载器 |
| `webview_bridge.rs` | 调整 | `include_str!` 打包后的 inject.js |
| `build.rs` | 扩展 | 调 `node webview-inject/build.mjs` |
| `url_validator.rs` | 新建 | https + 白名单 + 拒私有 IP |
| 删除 | — | `telegram.rs`、`secure_session.rs`、`transfers.rs`、`cloud_upload.rs`、`credentials.rs`、`legacy_config.rs`(迁移逻辑依赖旧配置,删除) |
| `Cargo.toml` | 依赖 | +reqwest;−grammers-client/−grammers-session/−chacha20poly1305/−keyring |

### 前端(desktop/src/)

| 路径 | 改动 |
|---|---|
| `lib/api.ts` | 删 upload/cloud/transfer/chat/message 类型与调用;加新 download 类型 |
| `App.tsx` | 删上传页/云盘页/聊天浏览页;保留任务列表/日志/设置/Telegram Web |
| `app.css` | 删对应样式(可选) |

---

## Part 1 — 注入脚本(查看器方案)

### Task 1: 删除作废产物 + 更新 config.json

**Files:**
- Delete: `desktop/src-tauri/webview-inject/src/media.js`
- Rewrite: `desktop/src-tauri/webview-inject/config.json`

- [ ] **Step 1: 删除 media.js**

```bash
git rm desktop/src-tauri/webview-inject/src/media.js
```

理由:消息检测方案作废(聊天列表只有缩略图),查看器检测由新 `detect.js` 承担。

- [ ] **Step 2: 重写 config.json**

按 spec §4.8 的完整内容写入(webk/webz 两套 selector + `refreshDelayMs: 500` + `dedupeCacheTtlMs: 5000` + `trustedOrigins`)。

- [ ] **Step 3: Commit**

```bash
git add desktop/src-tauri/webview-inject/
git commit -m "chore(desktop): drop message-detection module; add viewer-based selector config"
```

---

### Task 2: extract.js — URL 提取与文件名解析

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/extract.js`

按 spec §4.4 实现,以下为关键代码:

```javascript
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
```

- [ ] **Step 1: 写 extract.js**(上面完整内容)
- [ ] **Step 2: 人工检查**:`node -e` 快速验证 `parseFileNameFromUrl` 对样例 URL 的行为(URL 尾部为 JSON 时返回 fileName,否则 null)
- [ ] **Step 3: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/extract.js
git commit -m "feat(desktop): add URL extraction and filename parsing"
```

---

### Task 3: detect.js — 查看器检测(webk/webz)

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/detect.js`

按 spec §3 selector 表与 §4.3 实现。关键骨架:

```javascript
// desktop/src-tauri/webview-inject/src/detect.js

export function detectVersion() {
  const host = location.hostname;
  if (host === 'webk.telegram.org') return 'webk';
  if (host === 'webz.telegram.org') return 'webz';
  if (location.pathname.startsWith('/k/')) return 'webk';
  return 'webz';
}

export function detectViewer(version, cfg) {
  return version === 'webk' ? detectViewerWebk(cfg) : detectViewerWebz(cfg);
}

function detectViewerWebk(cfg) {
  const wk = cfg.webk;
  const root = document.querySelector(wk.viewerRoot);
  if (!root) return null;
  const aspecter = root.querySelector(wk.mediaAspecter);
  if (!aspecter) return null;
  const video = aspecter.querySelector('video');
  if (video) {
    const container = root.querySelector(wk.videoControlsRight)?.parentElement
      || root.querySelector(wk.viewerButtons);
    if (!container) return null;
    return { kind: video.loop && video.muted ? 'animation' : 'video', element: video, container, source: 'viewer' };
  }
  const img = aspecter.querySelector(wk.imgSelector);
  if (img) {
    const container = root.querySelector(wk.viewerButtons);
    if (!container) return null;
    return { kind: 'photo', element: img, container, source: 'viewer' };
  }
  return null;
}

function detectViewerWebz(cfg) {
  const wz = cfg.webz;
  const slide = document.querySelector(wz.activeSlide);
  if (!slide) return null;
  const video = slide.querySelector('video');
  if (video) {
    const container = document.querySelector(wz.videoControls)
      || document.querySelector(wz.viewerActions);
    if (!container) return null;
    return { kind: video.loop && video.muted ? 'animation' : 'video', element: video, container, source: 'viewer' };
  }
  const img = slide.querySelector(wz.imgSelector) || slide.querySelector('img');
  if (img) {
    const container = document.querySelector(wz.viewerActions);
    if (!container) return null;
    return { kind: 'photo', element: img, container, source: 'viewer' };
  }
  return null;
}

export function detectStory(version, cfg) {
  const vc = version === 'webk' ? cfg.webk : cfg.webz;
  const root = document.querySelector(vc.storyRoot);
  if (!root) return null;
  const video = version === 'webk'
    ? root.querySelector(vc.storyVideo)
    : root.querySelector('video');
  if (video) {
    const header = document.querySelector(vc.storyHeader);
    if (!header) return null;
    return { kind: 'video', element: video, container: header, source: 'story' };
  }
  const img = version === 'webk'
    ? root.querySelector(vc.storyImage)
    : lastStoryImage(root, vc.storyImage);
  if (img) {
    const header = document.querySelector(vc.storyHeader);
    if (!header) return null;
    return { kind: 'photo', element: img, container: header, source: 'story' };
  }
  return null;
}

function lastStoryImage(root, selector) {
  const imgs = root.querySelectorAll(selector || 'img');
  return imgs.length ? imgs[imgs.length - 1] : null;
}

export function detectPinnedAudio(version, cfg) {
  if (version !== 'webk') return null; // 置顶音频为 webk 特性
  const wk = cfg.webk;
  const pinned = document.querySelector(wk.pinnedAudio);
  if (!pinned) return null;
  const audio = pinned.querySelector('audio') || document.querySelector('audio-element .audio');
  if (!(audio instanceof HTMLAudioElement)) return null;
  const container = document.querySelector(wk.pinnedAudioUtils);
  if (!container) return null;
  return { kind: 'voice', element: audio, container, source: 'pinned-audio' };
}
```

- [ ] **Step 1: 写 detect.js**(上面完整内容 + spec 中列出的所有 selector 走 config)
- [ ] **Step 2: 语法检查**:`node --check desktop/src-tauri/webview-inject/src/detect.js`
- [ ] **Step 3: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/detect.js
git commit -m "feat(desktop): add viewer/story/pinned-audio detection for webk and webz"
```

---

### Task 4: button.js — 原生风格按钮 + 状态机

**Files:**
- Rewrite: `desktop/src-tauri/webview-inject/src/button.js`

按 spec §4.5。要点:
- webk:`<button class="btn-icon tgico-download tel-download">` + 内嵌 `<span class="tgico">`(用 Telegram 原生图标字体,自动融入)
- webz:`<button class="Button smaller translucent-white round tel-download">` + 内嵌 `<i class="icon icon-download">`
- 注入:`container.prepend(btn)`;已存在 `.tel-download` 则跳过
- 状态:按钮 `data-tel-state`;`downloading` 时叠加小圆环(注入 8 行 CSS,`tel-download-progress`)
- 点击:回调 `onDownload()`(由 inject.js 提供,实时 extract 当前媒体)
- 状态机:`ready → submitting → queued → downloading → completed / failed`;`failed` 点击触发 retry 回调

关键骨架:

```javascript
// desktop/src-tauri/webview-inject/src/button.js
export function ensureButton(version, cfg, detected, onDownload, onRetry) {
  const { container } = detected;
  if (container.querySelector('.tel-download')) return container.querySelector('.tel-download');
  const btn = version === 'webk' ? createWebkButton() : createWebzButton();
  btn.addEventListener('click', (e) => {
    e.preventDefault(); e.stopPropagation();
    if (btn.dataset.telState === 'failed') { onRetry?.(btn); return; }
    onDownload(btn);
  });
  container.prepend(btn);
  return btn;
}

function createWebkButton() {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'btn-icon tgico-download tel-download';
  b.dataset.telState = 'ready';
  b.title = '下载';
  b.setAttribute('aria-label', '下载');
  b.innerHTML = '<span class="tgico"></span>';
  return b;
}

function createWebzButton() {
  const b = document.createElement('button');
  b.type = 'button';
  b.className = 'Button smaller translucent-white round tel-download';
  b.dataset.telState = 'ready';
  b.title = '下载';
  b.setAttribute('aria-label', '下载');
  b.innerHTML = '<i class="icon icon-download"></i>';
  return b;
}

export function setButtonState(btn, state, payload) {
  if (!btn) return;
  btn.dataset.telState = state;
  btn.title = { ready: '下载', submitting: '提交中…', queued: '已加入队列',
    downloading: payload?.progress != null ? `下载中 ${Math.round(payload.progress * 100)}%` : '下载中',
    completed: '已下载', failed: '重试' }[state] || '下载';
  btn.classList.toggle('tel-download-progress', state === 'downloading');
  btn.classList.toggle('tel-download-done', state === 'completed');
  btn.classList.toggle('tel-download-failed', state === 'failed');
}

// 注入微型样式(状态视觉)
export function injectButtonStyles() {
  if (document.getElementById('tel-download-style')) return;
  const style = document.createElement('style');
  style.id = 'tel-download-style';
  style.textContent = `
    .tel-download[data-tel-state="downloading"] { animation: tel-pulse 1s infinite; }
    .tel-download.tel-download-done { color: #4dcd5e !important; }
    .tel-download.tel-download-failed { color: #e53935 !important; }
    @keyframes tel-pulse { 50% { opacity: .5; } }
  `;
  (document.head || document.documentElement).appendChild(style);
}
```

- [ ] **Step 1: 写 button.js**
- [ ] **Step 2: 语法检查**:`node --check .../button.js`
- [ ] **Step 3: Commit**

---

### Task 5: state.js + dedupe.js(重写)

**Files:**
- Rewrite: `desktop/src-tauri/webview-inject/src/state.js`
- Rewrite: `desktop/src-tauri/webview-inject/src/dedupe.js`

state.js:taskId → button 映射;订阅 4 个事件更新状态,不再依赖 chatId/messageId。

```javascript
// state.js
import { setButtonState } from './button.js';

const byTask = new Map();
const activeByButton = new WeakMap();

export function registerTask(taskId, btn) {
  byTask.set(taskId, btn);
  activeByButton.set(btn, taskId);
}

export function taskIdFor(btn) { return activeByButton.get(btn) || null; }

export async function bindEvents(onCompleted) {
  const listen = window.__TAURI__.event.listen;
  await listen('webview-task-submitted', (e) => {
    const btn = byTask.get(e.payload.taskId);
    if (btn) setButtonState(btn, 'queued');
  });
  await listen('webview-task-updated', (e) => {
    const btn = byTask.get(e.payload.taskId);
    if (btn) setButtonState(btn, 'downloading', { progress: e.payload.progress });
  });
  await listen('webview-task-completed', (e) => {
    const btn = byTask.get(e.payload.taskId);
    if (btn) setButtonState(btn, 'completed');
    onCompleted?.(e.payload);
  });
  await listen('webview-task-failed', (e) => {
    const btn = byTask.get(e.payload.taskId);
    if (btn) setButtonState(btn, 'failed');
  });
}
```

dedupe.js:按文件名查询 + 5s TTL 缓存。

```javascript
// dedupe.js
const cache = new Map();

export async function isDownloaded(fileName, cfg) {
  if (!fileName) return null;
  const ttl = cfg.dedupeCacheTtlMs ?? 5000;
  const now = Date.now();
  const hit = cache.get(fileName);
  if (hit && now - hit.at < ttl) return hit.value;
  let value = null;
  try {
    value = await window.__TAURI__.core.invoke('webview_query_downloaded', { fileName });
  } catch (_e) { value = null; }
  cache.set(fileName, { at: now, value });
  return value;
}
```

- [ ] **Step 1: 写 state.js 与 dedupe.js**
- [ ] **Step 2: `node --check` 两者**
- [ ] **Step 3: Commit**

---

### Task 6: watcher.js + inject.js + build.mjs(装配)

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/watcher.js`
- Rewrite: `desktop/src-tauri/webview-inject/src/inject.js`
- Create: `desktop/src-tauri/webview-inject/build.mjs`

watcher.js:

```javascript
// watcher.js
import { detectVersion, detectViewer, detectStory, detectPinnedAudio } from './detect.js';
import { getMediaUrl, resolveFileName } from './extract.js';
import { ensureButton, setButtonState, injectButtonStyles } from './button.js';
import { registerTask, taskIdFor } from './state.js';
import { isDownloaded } from './dedupe.js';

export function startWatcher(cfg) {
  const version = detectVersion();
  injectButtonStyles();

  const onDownload = async (btn) => {
    const detected = detectViewer(version, cfg) || detectStory(version, cfg) || detectPinnedAudio(version, cfg);
    if (!detected) { setButtonState(btn, 'failed'); return; }
    const url = getMediaUrl(detected.element, detected.kind);
    if (!url) { setButtonState(btn, 'failed'); return; }
    const fileName = resolveFileName(url, detected.kind);
    setButtonState(btn, 'submitting');
    try {
      const task = await window.__TAURI__.core.invoke('submit_download_from_webview', {
        request: { mediaUrl: url, fileName, fileType: detected.kind, source: detected.source },
      });
      registerTask(task.taskId, btn);
      setButtonState(btn, 'queued');
    } catch (_e) {
      setButtonState(btn, 'failed');
    }
  };

  const onRetry = async (btn) => {
    setButtonState(btn, 'ready');
    await onDownload(btn);
  };

  setInterval(async () => {
    const detected = detectViewer(version, cfg) || detectStory(version, cfg) || detectPinnedAudio(version, cfg);
    if (!detected) return;
    const btn = ensureButton(version, cfg, detected, onDownload, onRetry);
    if (!btn || btn.dataset.telState !== 'ready' || btn.dataset.telDeduped === '1') return;
    const url = getMediaUrl(detected.element, detected.kind);
    const fileName = resolveFileName(url, detected.kind);
    const hit = await isDownloaded(fileName, cfg);
    btn.dataset.telDeduped = '1';
    btn.dataset.telFileName = fileName;
    if (hit) setButtonState(btn, 'completed');
  }, cfg.refreshDelayMs);
}
```

注意:每轮轮询都重新 `detectXXX`(实时读当前媒体);`telDeduped` 标记防止每 500ms 重复查询。

inject.js:

```javascript
// inject.js
import { startWatcher } from './watcher.js';
import { bindEvents } from './state.js';

(function main() {
  'use strict';
  if (window.top !== window) return;
  const cfg = globalThis.__INJECT_CONFIG__ || {};
  const trusted = cfg.trustedOrigins || ['https://web.telegram.org'];
  if (!trusted.includes(location.origin)) return;
  bindEvents().catch(() => {});
  startWatcher(cfg);
})();
```

build.mjs:按依赖序 concat(boot 无,顺序:icons, extract, detect, button, state, dedupe, watcher, inject),注入 `globalThis.__INJECT_CONFIG__ = {...}`,terser minify,输出 `dist/inject.js`。

- [ ] **Step 1: 写 watcher.js / inject.js / build.mjs**
- [ ] **Step 2: 跑打包**:`cd desktop && node src-tauri/webview-inject/build.mjs` → 产出 `dist/inject.js`
- [ ] **Step 3: 语法+基本 sanity**:`node --check dist/inject.js`
- [ ] **Step 4: Commit**

---

### Task 7: webview_bridge.rs 切换到打包脚本 + build.rs 自动构建

**Files:**
- Modify: `desktop/src-tauri/src/webview_bridge.rs`
- Modify: `desktop/src-tauri/build.rs`

- [ ] **Step 1: webview_bridge.rs 替换内联脚本**

```rust
pub const TELEGRAM_WEBVIEW_INIT_SCRIPT: &str =
    include_str!("../webview-inject/dist/inject.js");
```

删除原 137 行内联脚本字符串(保留文件内其他函数与注释,更新顶部文档注释描述新方案)。

- [ ] **Step 2: build.rs 增加打包步骤**

在 `main()` 中 `tauri_build::...build();` 之后追加:

```rust
let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
let build_mjs = std::path::Path::new(&manifest_dir).join("webview-inject/build.mjs");
if build_mjs.exists() {
    let src_dir = std::path::Path::new(&manifest_dir).join("webview-inject/src");
    let dist = std::path::Path::new(&manifest_dir).join("webview-inject/dist/inject.js");
    let needs = !dist.exists()
        || std::fs::read_dir(&src_dir).ok().map(|entries| {
            entries.flatten().any(|e| {
                e.metadata().and_then(|m| m.modified()).ok()
                    > dist.metadata().and_then(|m| m.modified()).ok()
            })
        }).unwrap_or(false);
    if needs {
        let _ = std::process::Command::new("node").arg(&build_mjs).status();
    }
    println!("cargo:rerun-if-changed=webview-inject/src");
    println!("cargo:rerun-if-changed=webview-inject/build.mjs");
    println!("cargo:rerun-if-changed=webview-inject/config.json");
}
```

- [ ] **Step 3: 编译验证**:`cd desktop/src-tauri && cargo check --locked`
  (dist/inject.js 不存在时 build.rs 会先生成;若无 Node 则警告但用已有 dist)
- [ ] **Step 4: Commit**

---

## Part 2 — Rust HTTP 下载

### Task 8: reqwest 依赖 + url_validator.rs

**Files:**
- Modify: `desktop/src-tauri/Cargo.toml`
- Create: `desktop/src-tauri/src/url_validator.rs`

- [ ] **Step 1: Cargo.toml**

```toml
reqwest = { version = "=0.12.9", default-features = false, features = ["stream", "rustls-tls"] }
```

(其余 MTProto 依赖此阶段先保留,Task 12 统一删。)

- [ ] **Step 2: url_validator.rs**

```rust
//! 媒体 URL 白名单校验。仅允许 https + Telegram 域;拒绝私网地址(SSRF 兜底)。

use anyhow::{bail, Result};
use url::Url;

const ALLOWED_SUFFIXES: &[&str] = &[
    ".telegram.org",
    ".cdn-telegram.org",
];

pub fn validate_media_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).map_err(|e| anyhow::anyhow!("URL 无法解析:{e}"))?;
    if url.scheme() != "https" {
        bail!("仅允许 https URL");
    }
    let host = url.host_str().ok_or_else(|| anyhow::anyhow!("URL 缺少主机名"))?;
    let host_lower = host.to_ascii_lowercase();
    let allowed = host_lower == "telegram.org"
        || ALLOWED_SUFFIXES.iter().any(|suffix| host_lower.ends_with(suffix));
    if !allowed {
        bail!("URL 域名不在 Telegram 白名单内:{host}");
    }
    if is_private_host(&host_lower) {
        bail!("拒绝私网地址");
    }
    Ok(url)
}

fn is_private_host(host: &str) -> bool {
    if host == "localhost" || host == "::1" { return true; }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn accepts_telegram_https() {
        assert!(validate_media_url("https://web.telegram.org/file/x").is_ok());
        assert!(validate_media_url("https://cdn-telegram.org/a/b").is_ok());
        assert!(validate_media_url("https://abc.cdn-telegram.org/a/b").is_ok());
    }
    #[test] fn rejects_non_telegram() {
        assert!(validate_media_url("https://evil.com/x").is_err());
        assert!(validate_media_url("https://telegram.org.evil.com/x").is_err());
    }
    #[test] fn rejects_non_https_and_private() {
        assert!(validate_media_url("http://web.telegram.org/x").is_err());
        assert!(validate_media_url("https://127.0.0.1/x").is_err());
        assert!(validate_media_url("file:///etc/passwd").is_err());
    }
}
```

- [ ] **Step 3: 测试通过**:`cd desktop/src-tauri && cargo test --locked url_validator`
- [ ] **Step 4: Commit**

---

### Task 9: downloader.rs 换 HTTP(S)(改造现有调度骨架)

**Files:**
- Modify: `desktop/src-tauri/src/downloader.rs`

保留:dispatch、TaskControl、pause/cancel/resume/retry、RequestLimiter、BandwidthLimiter、FloodGate、chunk 编排、进度事件、原子提交、BLAKE3 缺块恢复。
替换:`fetch_chunk` 的协议实现;删除 `refresh_location`、`verify_telegram_hashes`、`is_file_reference_error`、`flood_wait_seconds`(MTProto 专属);任务创建改为 URL 基。

- [ ] **Step 1: 新 fetch_chunk(HTTP Range)**

```rust
async fn fetch_chunk_http(
    client: &reqwest::Client,
    url: &str,
    offset: u64,
    length: u64,
    retries: u32,
    timeout: Duration,
    token: &CancellationToken,
    limiter: &Arc<RequestLimiter>,
) -> Result<Vec<u8>> {
    let end = offset + length - 1;
    let mut attempt = 0_u32;
    loop {
        if token.is_cancelled() { bail!("任务已取消"); }
        let _permit = limiter.acquire(token).await?;
        let response = tokio::select! {
            _ = token.cancelled() => bail!("任务已取消"),
            r = tokio::time::timeout(timeout, client
                .get(url)
                .header(reqwest::header::RANGE, format!("bytes={offset}-{end}"))
                .send()) => match r {
                    Ok(result) => result,
                    Err(_) => { attempt += 1; if attempt > retries { bail!("HTTP 请求超时(偏移 {offset})"); } tokio::time::sleep(backoff(attempt)).await; continue; }
                }
        };
        match response {
            Ok(resp) => {
                let status = resp.status().as_u16();
                if status == 206 || status == 200 {
                    let bytes = resp.bytes().await.context("读取响应体失败")?.to_vec();
                    limiter.success();
                    return Ok(bytes);
                }
                if status == 401 || status == 403 {
                    bail!("URL 已过期或无权限,请重新打开该媒体后再试");
                }
                if status == 404 || status == 410 {
                    bail!("文件已删除或 URL 失效");
                }
                if status == 416 {
                    return Ok(Vec::new()); // 已到文件尾
                }
                attempt += 1;
                if attempt > retries { bail!("HTTP 下载失败(状态 {status},偏移 {offset})"); }
                tokio::time::sleep(backoff(attempt)).await;
            }
            Err(error) => {
                attempt += 1;
                if attempt > retries { bail!("HTTP 网络错误(偏移 {offset}):{error}"); }
                tokio::time::sleep(backoff(attempt)).await;
            }
        }
    }
}
```

- [ ] **Step 2: 探测函数(HEAD / Range 探测)**

```rust
pub async fn probe_remote(client: &reqwest::Client, url: &str, timeout: Duration) -> Result<u64> {
    let resp = client.head(url).timeout(timeout).send().await.context("HEAD 请求失败")?;
    if !resp.status().is_success() {
        bail!("URL 探测失败(状态 {})", resp.status());
    }
    let len = resp.content_length().context("服务器未返回 Content-Length")?;
    Ok(len)
}
```

Range 不支持(HEAD 无 `accept-ranges: bytes` 且首块返回 200 整文件)时,退化为单流整文件写入(单 worker、chunk_size = 总长)。

- [ ] **Step 3: 任务创建改 URL 基**

```rust
pub async fn create_http_task(
    shared: &Arc<SharedState>,
    manager: &DownloadManager,
    media_url: &str,
    file_name: &str,
    file_type: &str,
    source: &str,
) -> Result<TaskRecord> { /* validate url → probe → insert task_store(media_url, file_name, file_type, total) → enqueue */ }
```

- [ ] **Step 4: 全文件检索,删除所有 MTProto 调用与 `grammers_client::` 引用**

`cd desktop/src-tauri && cargo check --locked`(此时 telegram.rs 仍在,会报未使用,属预期;Task 12 删除)

- [ ] **Step 5: Commit**

---

### Task 10: task_store.rs 新增列 + 去重查询

**Files:**
- Modify: `desktop/src-tauri/src/task_store.rs`
- Modify: `desktop/src-tauri/src/db_migration.rs`
- Modify: `desktop/src-tauri/src/models.rs`

- [ ] **Step 1: 迁移新增列**

`tasks` 表加 `media_url TEXT`、`file_name TEXT`;`chat_id`/`message_id` 保留列定义(避免 SQLite 迁移复杂度),新任务不写入。

- [ ] **Step 2: `find_completed_by_file_name`**

```rust
pub async fn find_completed_by_file_name(&self, file_name: &str) -> Result<Option<DownloadedMatch>> {
    // SELECT task_id, file_size, completed_at, output_path
    //   FROM tasks
    //  WHERE file_name = ? AND status = 'completed'
    //  ORDER BY completed_at DESC LIMIT 1
}
```

`DownloadedMatch` 保持 `{task_id, file_size, completed_at, output_path}`。

- [ ] **Step 3: `cargo test --locked` + Commit**

---

### Task 11: commands.rs 新 payload + app_state 接线

**Files:**
- Modify: `desktop/src-tauri/src/commands.rs`
- Modify: `desktop/src-tauri/src/app_state.rs`
- Modify: `desktop/src-tauri/src/lib.rs`

- [ ] **Step 1: 新 `WebviewDownloadRequest`**

```rust
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebviewDownloadRequest {
    pub media_url: String,
    pub file_name: Option<String>,
    pub file_type: String,
    pub source: String,
}
```

- [ ] **Step 2: `submit_download_from_webview` 重写**

```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn submit_download_from_webview(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    request: WebviewDownloadRequest,
) -> Result<TaskRecord, String> {
    if !webview_bridge::is_trusted_telegram_webview(&webview) {
        return Err("下载请求必须来自受信任的 Telegram WebView 页面".into());
    }
    crate::url_validator::validate_media_url(&request.media_url).map_err(command_error)?;
    let file_name = request.file_name.unwrap_or_else(|| "telegram_media.bin".into());
    downloader::create_http_task(&state.shared, &state.downloads, &request.media_url,
        &file_name, &request.file_type, &request.source)
        .await.map_err(command_error)
}
```

- [ ] **Step 3: `webview_query_downloaded` 参数改 `fileName`**

- [ ] **Step 4: 删除命令**:`submit_batch_download_from_webview`、全部 `*_upload*`/`*_forward*`/`*_transfer*`/`*_cloud*`/login 命令;`app_state.rs` 删 `telegram` 字段与 `telegram()`;`lib.rs` invoke_handler 同步。

- [ ] **Step 5: build.rs AppManifest 命令列表同步**(删 batch 等)

- [ ] **Step 6: `cargo check --locked`(暂时会有 telegram.rs 未使用警告)+ Commit**

---

### Task 12: 删除 MTProto 模块 + Cargo 依赖清理

**Files:**
- Delete: `telegram.rs`、`secure_session.rs`、`transfers.rs`、`cloud_upload.rs`、`credentials.rs`、`legacy_config.rs`
- Modify: `Cargo.toml`、`lib.rs`、`models.rs`(删关联类型)

- [ ] **Step 1: 删文件 + 清理引用**

```bash
git rm desktop/src-tauri/src/telegram.rs \
       desktop/src-tauri/src/secure_session.rs \
       desktop/src-tauri/src/transfers.rs \
       desktop/src-tauri/src/cloud_upload.rs \
       desktop/src-tauri/src/credentials.rs \
       desktop/src-tauri/src/legacy_config.rs
```

- [ ] **Step 2: Cargo.toml 删依赖**

删:`grammers-client`、`grammers-session`、`chacha20poly1305`、`keyring`、`glass_pumpkin`(如无其他使用)。

- [ ] **Step 3: 全量编译 + 测试**

```bash
cd desktop/src-tauri && cargo check --locked && cargo test --locked && cargo clippy --locked --all-targets
```

预期:全部通过;`grep -ri grammers src/` 为空。

- [ ] **Step 4: Commit**

---

### Task 13: 前端清理(api.ts / App.tsx)

**Files:**
- Modify: `desktop/src/lib/api.ts`
- Modify: `desktop/src/App.tsx`

- [ ] **Step 1: api.ts**

删:upload/transfer/cloud 相关类型与函数、`ChatSummary`/`ChatMessage`、login 系列、`submitBatchDownloadFromWebview`。
改:`webviewQueryDownloaded(fileName)`;`DownloadedMatch` 不变。
加:`DownloadTask` 增 `fileName?: string | null`。

- [ ] **Step 2: App.tsx**

删:上传页、云盘上传页、聊天浏览页、Telegram 登录卡片、设置页内的 API ID/Hash/代理/上传/云盘区块。
保留:总览、Telegram Web(内嵌窗口)、下载任务、日志状态、设置(下载目录/命名/并发/限速/存储迁移)。

- [ ] **Step 3: 类型检查 + 构建**

```bash
cd desktop && npm run check && npm run build
```

- [ ] **Step 4: Commit**

---

### Task 14: 真机端到端验收

**Files:** 无(纯验证)

- [ ] **Step 1: 启动**

```bash
cd desktop && npx tauri dev
```

- [ ] **Step 2: 登录 Telegram Web**(WebView 内扫描二维码)

- [ ] **Step 3: 逐项验收**(每项通过打勾)

- [ ] 图片查看器 → 按钮出现在工具栏 → 点击 → 下载完成,文件可打开
- [ ] 视频查看器 → 大文件(>10MB)→ Range 分块下载 → 文件可播放
- [ ] GIF → 按钮 → 下载完成
- [ ] 语音(置顶音频)→ 按钮 → 下载完成
- [ ] Story → 按钮 → 下载完成
- [ ] 禁保存频道的内容 → 可下载
- [ ] 已下载文件重新打开 → 按钮显示已完成状态
- [ ] 下载中杀掉客户端 → 重启 → 任务从断点恢复
- [ ] webk(/k/)与 webz(/a/)两个版本各验证一遍
- [ ] Rust 端:`grep -ri grammers desktop/src-tauri/src/` 为空

- [ ] **Step 4: 记录失败项并修复**(逐项循环,不通过不结项)

- [ ] **Step 5: 最终提交**

```bash
git add -A
git commit -m "chore(desktop): finalize HTTP viewer downloader E2E verification"
```

---

## 依赖关系

```
Task 1 (清理+config)
Task 2 (extract) ─┬─ Task 3 (detect) ─ Task 4 (button) ─ Task 5 (state+dedupe) ─ Task 6 (watcher+inject+build) ─ Task 7 (bridge+build.rs)
Task 8 (reqwest+validator) ─ Task 9 (downloader HTTP) ─ Task 10 (store) ─ Task 11 (commands) ─ Task 12 (删 MTProto)
Task 13 (前端) 依赖 Task 11
Task 14 (E2E) 依赖全部
```

JS 链(Task 1-7)与 Rust 链(Task 8-12)可并行推进;Task 14 需两者都完成。
