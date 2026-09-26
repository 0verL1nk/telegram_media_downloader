# Telegram WebView 下载器(HTTP 方案) — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> ## ⚠️ 2026-09-26 修订(第三次)
>
> 1. 第一版(聊天列表消息按钮 + MTProto)作废:聊天列表只有缩略图,拿不到原图 URL;验证脚本(#446342,23.9 万安装)全程只做**媒体查看器路径**
> 2. 第二版(Rust reqwest 拉取)作废:下载的网络请求必须**在页面里做**(与脚本逐字节一致),Rust 只收字节做管理
> 3. **URL 永不进 Rust**;reqwest / url_validator / MTProto 全部不保留
>
> **不做单元测试**(JS 侧)。验证 = 真机 `npx tauri dev` 手工逐项确认。
> Rust 侧保留 `cargo test` 惯例(纯逻辑)。
>
> 已完成且保留:脚手架、icons.js、extract.js、detect.js、button.js(含隐藏按钮接管)、state.js、task-state.js、watcher.js、inject.js、build.mjs、webview_bridge 打包嵌入、task_store 状态查询
>
> 规范: `docs/superpowers/specs/2026-09-26-telegram-webview-inject-enhancement-design.md`(注入脚本)
>       `docs/superpowers/specs/2026-09-26-webview-http-downloader-design.md`(下载管线:页面抓取 + IPC 分块)

**Goal:** 把验证过的油猴脚本做法(#446342)搬进桌面客户端:查看器工具栏按钮 → 页面内抓取分块 → IPC 交给 Rust 落盘/记账/提交(任务队列/断点续传/并发/校验由 Rust 管理)。

**Architecture:** 注入脚本 500ms 轮询检测媒体查看器/Story/置顶音频,向原生工具栏注入下载按钮;点击后 JS 探测大小 → 向 Rust 申请分块计划 → 页面内并发 `fetch` Range → 分块二进制经 Tauri IPC 推送 → Rust 用现有 chunk_writer 落盘、事件回灌按钮状态。

**Tech Stack:** Rust(edition 2024)+ Tauri 2.11.6 + tokio + SeaORM(无 HTTP 客户端);vanilla JavaScript(ES2022);Node + terser(仅打包)。

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
| `src/task-state.js` | 按文件名查任务状态(queued/downloading/completed/failed)+ TTL 缓存 | 新建 |
| `src/inject.js` | 组装入口 | 重写 |
| `build.mjs` | concat + terser → dist/inject.js | 新建 |
| `README.md` | 已有(已更新验证方式) | ✅ |

删除:`src/media.js`(消息检测方案作废)。

### Rust(desktop/src-tauri/src/)

| 路径 | 改动 | 说明 |
|---|---|---|
| `downloader.rs` | 重写 | 删 MTProto 拉取机制;新:槽位/通道/writer/看门狗(见 R2) |
| `commands.rs` | 重构 | 新命令集(start/plan/push/finish/fail);删 batch/upload/forward/cloud/login/chats |
| `task_store.rs` | ✅ 已有 | `find_latest_by_file_name`(状态查询)已落地 |
| `db_migration.rs` | R1 | 000005 drop `media_url`(页面抓取不存 URL) |
| `app_state.rs` | R2/R4 | 接线新管理器;R4 删 telegram/uploads/transfers 字段 |
| `webview_bridge.rs` | ✅ 已有 | `include_str!` 打包后的 inject.js |
| `build.rs` | ✅ 已有 | 调 `node webview-inject/build.mjs` |
| 删除 | R4 | `telegram.rs`、`secure_session.rs`、`transfers.rs`、`cloud_upload.rs`、`credentials.rs`、`legacy_config.rs`、`filter.rs` |
| `Cargo.toml` | R1+R4 | R1 删 reqwest/url_validator;R4 删 grammers-*/chacha20poly1305/keyring |

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

### Task 5: state.js + task-state.js(重写)

**Files:**
- Rewrite: `desktop/src-tauri/webview-inject/src/state.js`
- Create: `desktop/src-tauri/webview-inject/src/task-state.js`(替代原 dedupe.js)

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

task-state.js:按文件名查任务状态 + 5s TTL 缓存。返回 `{state, taskId?, progress?, fileSize?, completedAt?}`;`state ∈ 'queued'|'downloading'|'completed'|'failed'|'none'`。

```javascript
// desktop/src-tauri/webview-inject/src/task-state.js
const cache = new Map();

export async function queryTaskState(fileName, cfg) {
  if (!fileName) return { state: 'none' };
  const ttl = cfg.dedupeCacheTtlMs ?? 5000;
  const now = Date.now();
  const hit = cache.get(fileName);
  if (hit && now - hit.at < ttl) return hit.value;
  let value = { state: 'none' };
  try {
    value = await window.__TAURI__.core.invoke('webview_query_task_state', { fileName });
  } catch (_e) {
    value = { state: 'none' };
  }
  cache.set(fileName, { at: now, value });
  return value;
}
```

- [ ] **Step 1: 写 state.js 与 task-state.js**
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
import { queryTaskState } from './task-state.js';

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
    if (!btn || btn.dataset.telState !== 'ready' || btn.dataset.telChecked === '1') return;
    const url = getMediaUrl(detected.element, detected.kind);
    const fileName = resolveFileName(url, detected.kind);
    const st = await queryTaskState(fileName, cfg);
    btn.dataset.telChecked = '1';
    btn.dataset.telFileName = fileName;
    if (st.state === 'completed') setButtonState(btn, 'completed');
    else if (st.state === 'failed') setButtonState(btn, 'failed');
    else if (st.state === 'downloading') { setButtonState(btn, 'downloading', { progress: st.progress }); if (st.taskId) registerTask(st.taskId, btn); }
    else if (st.state === 'queued') { setButtonState(btn, 'queued'); if (st.taskId) registerTask(st.taskId, btn); }
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

build.mjs:按依赖序 concat(顺序:icons, extract, detect, button, state, task-state, watcher, inject),注入 `globalThis.__INJECT_CONFIG__ = {...}`,terser minify,输出 `dist/inject.js`。

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

## Part 2 — Rust 下载管线(页面 fetch + IPC 分块,2026-09-26 二次修订)

规范:`docs/superpowers/specs/2026-09-26-webview-http-downloader-design.md`。要点:**下载的网络请求在页面里做(与脚本 #446342 一致),Rust 只收字节、落盘、记账、提交;URL 永不进 Rust;reqwest/url_validator 撤销;MTProto 不保留**。

### Task R1: 清 reqwest/url_validator + drop media_url 迁移

**Files:** `Cargo.toml`、`Cargo.lock`、`src/db_migration.rs`、`src/lib.rs`(+`src/entities.rs` 检查)

- 删 `reqwest` 依赖;`cargo check`(不加 --locked,刷新锁文件)
- `git rm src/url_validator.rs`;lib.rs 删 `mod url_validator;`
- 新迁移 `m20260926_000005_drop_tasks_media_url`(drop column,按现有迁移模块样式)
- 验证:`cargo check --locked` + `cargo test --locked`(评测数变化)+ fmt
- Commit: `chore(desktop): drop reqwest and url_validator; add migration dropping tasks.media_url`

### Task R2: downloader.rs 重写 + commands 新命令集

**Files:** `src/downloader.rs`(重写主体)、`src/commands.rs`、`src/app_state.rs`、`src/lib.rs`、`build.rs`

**删除**(MTProto 拉取机制不再适用):dispatch/TaskControl/RequestLimiter/BandwidthLimiter/FloodGate/fetch_chunk/refresh_location/verify_telegram_hashes/save_text_task/create_task_for_message*/create_text_task*/create_chat_tasks/batch_limit_reached/write_sidecars/matches_file_format
**保留复用**:chunk_writer 全部、atomic_file、task_store、`output_path`(简化为 download_root + 净化文件名)、`apply_duplicate_policy`、`temporary_output_path`、`verified_final_file_matches_chunks`、`chunk_map_is_complete`、`join_download_children`、`safe_component`

**新增管理面**(spec §4):
- `slots: Semaphore(max_files)`;`channels: Map<taskId, mpsc::Sender<IncomingChunk>>`;`writers: Map<taskId, JoinHandle>`;`last_activity: Map<taskId, Instant>`
- watchdog:10s 一跳;`downloading` 且 30s 无活动 → 标 paused + 发 `webview-download-abort`

**新命令**(spec §3):
- `start_webview_download {fileName, fileType, source} → {taskId}`(文件名净化/长度校验、重复策略)
- `plan_chunks {taskId, totalBytes} → {chunkSizeBytes, concurrency, timeoutSeconds, retries, missing[]}`(幂等;total 不符报错;占槽位;启 writer)
- `push_chunk`(taskId+offset + **raw bytes** body)→ mpsc 入队(背压)
- `finish_download {taskId} → TaskRecord`;`fail_download {taskId, error} → TaskRecord`
- `webview_query_task_state` 扩展 `resumable` 字段;`task_action` 重写(pause/cancel → abort 事件;retry → queued)
- 事件:`webview-task-submitted/updated/completed/failed` + 新 `webview-download-abort`
- **常量命名**(消灭魔法数字):`PROGRESS_TICK`(350ms)、`WATCHDOG_TICK`(10s)、`ACTIVITY_TIMEOUT`(30s)、`SLOT_WAIT_*`

**删除的旧命令**:`submit_download_from_webview`(旧 payload)、`create_download_task`、`create_chat_download`(依赖已删的 downloader 函数)

- 验证:`cargo test --locked`(新命令测试:文件名校验、plan 幂等/total 不符、push 乱序/越界/背压、finish 缺块拒绝/原子提交、watchdog 超时)+ fmt + clippy
- Commit: `feat(desktop): rewrite downloader as webview page-fetch manager with IPC chunk ingest`

### Task R3: JS downloader.js + watcher/state 接线

**Files:** `webview-inject/src/downloader.js`(新)、`watcher.js`、`state.js`、`build.mjs`

- downloader.js(spec §5):`probeTotal`(Range 0-0 → Content-Range total)、`fetchChunk`(206/200/416 处理)、`runDownload`(并发闸/块级重试退避/AbortController/带宽节流近似)→ `push_chunk`(二进制)→ `finish`/`fail_download`
- 响应 `webview-download-abort` → abort 对应任务的全部在途 fetch
- watcher 点击流程:start → probe → plan → run;state.js 增加 abort 订阅
- build.mjs ORDER 插入 downloader.js;`node build.mjs` 重打包 + 冒烟
- Commit: `feat(desktop): page-context chunked fetcher pushing bytes to Rust over IPC`

### Task R4: 删 MTProto 模块 + 依赖 + 残留

**Files:** 删除 `telegram.rs`、`secure_session.rs`、`transfers.rs`、`cloud_upload.rs`、`credentials.rs`、`legacy_config.rs`、`filter.rs`(确认为孤儿后);`Cargo.toml`(−grammers-client/−grammers-session/−chacha20poly1305/−keyring/−glass_pumpkin);`app_state.rs`、`commands.rs`、`lib.rs`、`models.rs`、`storage.rs`(如引用)

- 删除命令:`login_*`/`logout_session`/`list_chats`/`get_chat_messages`/`upload_completed_download`/`forward_telegram_message`/`list_telegram_transfers`/`telegram_transfer_action`/`queue_cloud_upload`/`list_cloud_uploads`/`cloud_upload_action`/`import_legacy_config`
- 保留面(硬约束):任务页/设置(下载目录+迁移)/日志/WebView;`change_storage_root` 清掉对已删 manager 的等待
- 验证:`cargo check --locked` + `cargo test --locked` + clippy + fmt;`grep -ri grammers src/ Cargo.toml` 为空
- Commit: `refactor(desktop): remove MTProto, transfers, cloud upload, and credentials modules`

### Task R5: 前端清理

**Files:** `src/lib/api.ts`、`src/App.tsx`、`src/app.css`

- 删:上传页/云盘页/聊天浏览页/登录卡片与相关类型、调用
- 保留:总览、Telegram Web 窗口、下载任务页(队列/进度/速度/暂停/继续/取消/重试/历史/打开位置)、日志、设置(下载目录选择+存储迁移、并发/分块/超时/重试/限速、WebView2 profile)
- 验证:`npm run check` + `npm run build`
- Commit: `refactor(desktop): remove dead pages and types from the client UI`

### Task R6: 真机端到端验收

- `npx tauri dev`:登录 Telegram Web → 六类媒体 + Story + 置顶音频 下载 → 大文件 IPC 吞吐 → 杀客户端续传 → 关 WebView 看门狗恢复 → 暂停/取消/重试 → 禁保存 + URL 过期
- 客户端可用性清单(硬约束):任务页全功能、设置生效、无死页面
- 失败逐项修复,通过后提交

## 依赖关系

```
已完成:Task 1-7(注入脚本全部)+ Task 10(store 状态查询)
R1(清依赖/迁移) ─ R2(下载管理器+命令) ─ R3(JS 抓取接线) ─ R4(删 MTProto) ─ R5(前端清理) ─ R6(真机验收)
R4 依赖 R2(命令形态定稿);R5 依赖 R4(API 集合定稿);R6 依赖全部
```
