# Telegram WebView 注入脚本增强 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把桌面客户端 `Telegram Web` 子 WebView 中 137 行内联 init script 重构为独立 JS 模块集,提供图标按钮、多文件识别、状态反馈、本地已下载标记与 Story 支持,并把桥接能力扩展为 3 个新 Tauri 命令 + 升级事件协议。

**Architecture:** Tauri 2 build.rs 调 Node 脚本把 JS 模块集合打包为单文件,`include_str!` 嵌入 Rust 端;WebView 启动时注入,Rust 侧走 MTProto 任务系统执行下载;事件反向回灌到 inject.js 更新按钮状态。

**Tech Stack:** Rust(edition 2024)+ Tauri 2.11.6 + tokio + SeaORM;vanilla JavaScript(ES2022)+ happy-dom + vitest;Telegram MTProto via Grammers 0.10.0。

**Spec:** `docs/superpowers/specs/2026-09-26-telegram-webview-inject-enhancement-design.md`

---

## 文件结构

### 新建

| 路径 | 职责 |
|---|---|
| `desktop/src-tauri/webview-inject/.gitignore` | 忽略 `dist/` 与 `node_modules/` |
| `desktop/src-tauri/webview-inject/config.json` | 编译期内联常量 |
| `desktop/src-tauri/webview-inject/build.mjs` | concat + minify → `dist/inject.js` |
| `desktop/src-tauri/webview-inject/README.md` | 开发者指南(怎么改注入脚本、怎么跑测试) |
| `desktop/src-tauri/webview-inject/src/boot.js` | 入口:解析 CONFIG、校验 origin、wire 各模块 |
| `desktop/src-tauri/webview-inject/src/icons.js` | 内联 SVG 字符串导出 |
| `desktop/src-tauri/webview-inject/src/observer.js` | MutationObserver 与消息生命周期 |
| `desktop/src-tauri/webview-inject/src/media.js` | 7 类媒体识别 + protected 检测 |
| `desktop/src-tauri/webview-inject/src/button.js` | 按钮 DOM 渲染与状态机 |
| `desktop/src-tauri/webview-inject/src/state.js` | Tauri 事件订阅与按钮同步 |
| `desktop/src-tauri/webview-inject/src/dedupe.js` | 已下载检测查询与缓存 |
| `desktop/src-tauri/webview-inject/src/story.js` | Story overlay 观察器 |
| `desktop/src-tauri/webview-inject/test/setup.js` | vitest 全局 setup(注册 happy-dom、mock __TAURI__) |
| `desktop/src-tauri/webview-inject/test/icons.test.js` | icons 导出与长度校验 |
| `desktop/src-tauri/webview-inject/test/media.test.js` | 7 类识别 + protected |
| `desktop/src-tauri/webview-inject/test/button.test.js` | 状态机 6 状态转移 |
| `desktop/src-tauri/webview-inject/test/state.test.js` | 事件→按钮状态映射 |
| `desktop/src-tauri/webview-inject/test/dedupe.test.js` | 缓存命中 / TTL / invoke 调用 |
| `desktop/src-tauri/webview-inject/test/story.test.js` | overlay 挂载 / 清理 |
| `desktop/src-tauri/webview-inject/test/observer.test.js` | mutation → detectAndAttach |
| `desktop/src-tauri/webview-inject/test/boot.test.js` | origin 校验、CONFIG 注入 |
| `desktop/src-tauri/webview-inject/test/fixtures/*.html` | Telegram Web DOM 模拟 |
| `desktop/vitest.config.ts` | vitest 配置(测试 webview-inject/test/) |

### 修改

| 路径 | 改动 |
|---|---|
| `desktop/src-tauri/src/webview_bridge.rs` | `TELEGRAM_WEBVIEW_INIT_SCRIPT` 改 `include_str!("../webview-inject/dist/inject.js")` |
| `desktop/src-tauri/src/commands.rs` | 新增 3 个命令 + 升级 `submit_download_from_webview` 事件名 |
| `desktop/src-tauri/src/task_store.rs` | 新增 `find_completed_for_dedupe` |
| `desktop/src-tauri/src/lib.rs` | 注册 3 个新命令到 invoke_handler |
| `desktop/src-tauri/src/downloader.rs` | 状态变更处 emit `webview-task-*` 事件 |
| `desktop/src-tauri/build.rs` | 调用 `node webview-inject/build.mjs` |
| `desktop/package.json` | 加 `vitest`、`happy-dom`、`terser` 到 devDependencies |
| `desktop/src/lib/api.ts` | 加 TS 类型与方法签名(webview_query_downloaded 等) |

---

## Task 1: 基础设施与目录骨架

**Files:**
- Create: `desktop/src-tauri/webview-inject/.gitignore`
- Create: `desktop/src-tauri/webview-inject/config.json`
- Create: `desktop/src-tauri/webview-inject/README.md`
- Modify: `desktop/package.json`(加 devDependencies)
- Create: `desktop/vitest.config.ts`

- [ ] **Step 1: 创建 webview-inject 目录骨架与 .gitignore**

```gitignore
# desktop/src-tauri/webview-inject/.gitignore
dist/
node_modules/
coverage/
```

- [ ] **Step 2: 创建默认 config.json**

```json
{
  "observerDebounceMs": 50,
  "observerAttributeFilter": [
    "data-mid",
    "data-peer-id",
    "data-protected",
    "class",
    "aria-disabled"
  ],
  "messageSelectors": [
    ".message[data-mid]",
    ".Message[data-mid]",
    "[data-mid][data-peer-id]"
  ],
  "storySelectors": [
    "[data-story-viewer]",
    ".StoryViewer"
  ],
  "duplicateWindowBytes": 5120,
  "dedupeCacheTtlMs": 5000,
  "batchMaxSize": 32,
  "buttonStackPosition": "top-right",
  "iconSize": 16,
  "spinnerSize": 12,
  "visibleMinWidth": 100,
  "protectedAncestorDepth": 5
}
```

- [ ] **Step 3: 更新 package.json devDependencies**

打开 `desktop/package.json`,在 `devDependencies` 中加入:

```json
    "happy-dom": "16.0.0",
    "terser": "5.36.0",
    "vitest": "2.1.5"
```

(把现有 devDependencies 字段合并,保持 JSON 合法。)

- [ ] **Step 4: 创建 vitest.config.ts**

```typescript
// desktop/vitest.config.ts
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'happy-dom',
    include: ['src-tauri/webview-inject/test/**/*.test.js'],
    setupFiles: ['src-tauri/webview-inject/test/setup.js'],
    coverage: {
      provider: 'v8',
      include: ['src-tauri/webview-inject/src/**/*.js'],
      reporter: ['text', 'html'],
    },
  },
});
```

- [ ] **Step 5: 安装依赖并验证**

Run: `cd desktop && npm install`
Expected: 无错误,`node_modules/vitest`、`happy-dom`、`terser` 出现。

Run: `cd desktop && npx vitest --version`
Expected: 打印版本号,无 "command not found"。

- [ ] **Step 6: 创建空 README 占位**

```markdown
<!-- desktop/src-tauri/webview-inject/README.md -->
# Telegram WebView 注入脚本

> 本目录实现 Telegram Web 子 WebView 中的下载按钮注入逻辑。

## 开发

修改 `src/*.js` 后,运行:

\`\`\`bash
cd desktop
node src-tauri/webview-inject/build.mjs
\`\`\`

或 `cargo build`/`npx tauri dev`(会自动触发 build.rs)。

## 测试

\`\`\`bash
cd desktop
npx vitest run
\`\`\`

## 安全约束

永不读: `src` / `href` / `cookie` / `localStorage` / `__TAURI_INTERNALS__`。
仅读: `data-*` / `aria-disabled` / 可见性 / `tagName` / `className`。
```

- [ ] **Step 7: Commit**

```bash
cd E:/CodexPlay/telegram_media_downloader
git add desktop/src-tauri/webview-inject/.gitignore \
        desktop/src-tauri/webview-inject/config.json \
        desktop/src-tauri/webview-inject/README.md \
        desktop/package.json \
        desktop/package-lock.json \
        desktop/vitest.config.ts
git commit -m "feat(desktop): scaffold webview-inject directory and test tooling"
```

---

## Task 2: vitest setup + 测试 mock 工具

**Files:**
- Create: `desktop/src-tauri/webview-inject/test/setup.js`

- [ ] **Step 1: 写测试 setup**

```javascript
// desktop/src-tauri/webview-inject/test/setup.js
// 在每个测试文件加载前注入 __TAURI__ mock 与通用 helper。

const calls = {
  invoke: [],
  listen: [],
  emit: [],
};

globalThis.__TAURI_CALLS__ = calls;

globalThis.__TAURI__ = {
  core: {
    invoke: async (cmd, args) => {
      calls.invoke.push({ cmd, args });
      const handlers = globalThis.__TAURI_HANDLERS__ || {};
      if (handlers[cmd]) {
        return handlers[cmd](args);
      }
      return null;
    },
  },
  event: {
    listen: async (name, handler) => {
      calls.listen.push({ name });
      globalThis.__TAURI_LISTENERS__ = globalThis.__TAURI_LISTENERS__ || {};
      globalThis.__TAURI_LISTENERS__[name] = handler;
      return () => {
        delete globalThis.__TAURI_LISTENERS__[name];
      };
    },
  },
};

globalThis.__TAURI_TEST_RESET__ = () => {
  calls.invoke.length = 0;
  calls.listen.length = 0;
  calls.emit.length = 0;
  globalThis.__TAURI_HANDLERS__ = {};
  globalThis.__TAURI_LISTENERS__ = {};
};
```

- [ ] **Step 2: 验证 setup 加载不报错**

Run: `cd desktop && npx vitest run --reporter=verbose src-tauri/webview-inject/test/setup.js 2>&1 | head -20`
Expected: 由于 `setup.js` 不是测试文件,可能 no tests found,或成功加载无异常。允许 "No test files found"。

若报 "ReferenceError: __TAURI__",说明 happy-dom 环境下 globalThis 不可写,改为:

```javascript
globalThis.window = globalThis.window || globalThis;
window.__TAURI__ = { ... };
```

并把其他 `globalThis.X` 改成 `window.X`。

- [ ] **Step 3: Commit**

```bash
git add desktop/src-tauri/webview-inject/test/setup.js
git commit -m "test(desktop): add vitest setup with __TAURI__ mock helpers"
```

---

## Task 3: icons.js 与 icons.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/icons.js`
- Create: `desktop/src-tauri/webview-inject/test/icons.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/icons.test.js
import { describe, it, expect } from 'vitest';
import { ICONS, getIcon } from '../src/icons.js';

describe('icons', () => {
  it('exports 10 SVG strings', () => {
    expect(Object.keys(ICONS).length).toBe(10);
  });

  it('every icon is a non-empty SVG with viewBox', () => {
    for (const [name, svg] of Object.entries(ICONS)) {
      expect(svg.startsWith('<svg')).toBe(true);
      expect(svg.includes('viewBox')).toBe(true);
      expect(svg.length).toBeGreaterThan(20);
    }
  });

  it('getIcon returns SVG by name', () => {
    expect(getIcon('download')).toBe(ICONS.download);
    expect(getIcon('unknown')).toBe('');
  });

  it('required icons present', () => {
    const required = ['download', 'spinner', 'progress', 'check', 'retry',
                      'image', 'film', 'music', 'sticker', 'file'];
    for (const name of required) {
      expect(ICONS[name]).toBeTruthy();
    }
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/icons.test.js`
Expected: FAIL,`Cannot find module '../src/icons.js'`。

- [ ] **Step 3: 实现 icons.js**

```javascript
// desktop/src-tauri/webview-inject/src/icons.js
// 内联 SVG 字符串集合。每个 ~150-250 字节。
// 命名:download/spinner/progress/check/retry/image/film/music/sticker/file/story

const svg = (path) =>
  `<svg viewBox="0 0 24 24" width="100%" height="100%" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${path}</svg>`;

export const ICONS = {
  download: svg('<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><polyline points="7 10 12 15 17 10"/><line x1="12" y1="15" x2="12" y2="3"/>'),
  spinner: svg('<circle cx="12" cy="12" r="9" stroke-dasharray="42 18"><animateTransform attributeName="transform" type="rotate" from="0 12 12" to="360 12 12" dur="1s" repeatCount="indefinite"/></circle>'),
  progress: svg('<circle cx="12" cy="12" r="9"/><path d="M12 3 a9 9 0 0 1 9 9"/>'),
  check: svg('<polyline points="20 6 9 17 4 12"/>'),
  retry: svg('<polyline points="1 4 1 10 7 10"/><path d="M3.51 15a9 9 0 1 0 2.13-9.36L1 10"/>'),
  image: svg('<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="8.5" cy="8.5" r="1.5"/><polyline points="21 15 16 10 5 21"/>'),
  film: svg('<rect x="2" y="2" width="20" height="20" rx="2"/><line x1="7" y1="2" x2="7" y2="22"/><line x1="17" y1="2" x2="17" y2="22"/><line x1="2" y1="12" x2="22" y2="12"/><line x1="2" y1="7" x2="7" y2="7"/><line x1="2" y1="17" x2="7" y2="17"/><line x1="17" y1="7" x2="22" y2="7"/><line x1="17" y1="17" x2="22" y2="17"/>'),
  music: svg('<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>'),
  sticker: svg('<path d="M21 11v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h8"/><path d="M21 3 L9 15"/><circle cx="15" cy="9" r="3"/>'),
  file: svg('<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/>'),
  story: svg('<path d="M12 2 L22 7 L17 22 L7 22 L2 7 Z"/><circle cx="12" cy="12" r="3"/>'),
};

export function getIcon(name) {
  return ICONS[name] || '';
}
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/icons.test.js`
Expected: PASS, 4 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/icons.js \
        desktop/src-tauri/webview-inject/test/icons.test.js
git commit -m "feat(desktop): add icons module with 10 inline SVG strings"
```

---

## Task 4: media.js 与 media.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/media.js`
- Create: `desktop/src-tauri/webview-inject/test/media.test.js`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-photo.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-video.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-group.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-voice.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-sticker.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-animation.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-document.html`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/message-protected.html`

- [ ] **Step 1: 写 fixtures**

`message-photo.html`:
```html
<div class="message" data-mid="123" data-peer-id="-1001234567890">
  <img src="https://cdn.example/photo.jpg" style="width:300px;height:200px;display:block" />
</div>
```

`message-video.html`:
```html
<div class="message" data-mid="124" data-peer-id="-1001234567890">
  <video src="https://cdn.example/video.mp4" controls style="width:300px;height:200px;display:block"></video>
</div>
```

`message-group.html`(同消息多文件):
```html
<div class="message" data-mid="125" data-peer-id="-1001234567890">
  <img src="https://cdn.example/p1.jpg" style="width:200px;height:150px;display:block" />
  <img src="https://cdn.example/p2.jpg" style="width:200px;height:150px;display:block" />
  <a href="https://cdn.example/doc.pdf" download class="document-icon">doc.pdf</a>
</div>
```

`message-voice.html`:
```html
<div class="message" data-mid="126" data-peer-id="-1001234567890">
  <div class="voice-note">
    <audio src="https://cdn.example/voice.ogg"></audio>
  </div>
</div>
```

`message-sticker.html`:
```html
<div class="message" data-mid="127" data-peer-id="-1001234567890">
  <div class="sticker">
    <img src="https://cdn.example/sticker.webp" style="width:128px;height:128px;display:block" />
  </div>
</div>
```

`message-animation.html`:
```html
<div class="message" data-mid="128" data-peer-id="-1001234567890">
  <video loop muted autoplay style="width:200px;height:200px;display:block">
    <source src="https://cdn.example/anim.mp4" />
  </video>
</div>
```

`message-document.html`:
```html
<div class="message" data-mid="129" data-peer-id="-1001234567890">
  <div data-media-type="document" class="document-icon">file.zip</div>
</div>
```

`message-protected.html`:
```html
<div class="message protected" data-mid="130" data-peer-id="-1001234567890">
  <img src="https://cdn.example/p.jpg" style="width:300px;height:200px;display:block" />
</div>
```

- [ ] **Step 2: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/media.test.js
import { describe, it, expect, beforeEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { detectMessage } from '../src/media.js';

function loadFixture(name) {
  const html = readFileSync(resolve(`src-tauri/webview-inject/test/fixtures/${name}`), 'utf-8');
  document.body.innerHTML = html;
  return document.querySelector('.message');
}

const CONFIG = {
  messageSelectors: ['.message[data-mid]', '.Message[data-mid]', '[data-mid][data-peer-id]'],
  storySelectors: ['[data-story-viewer]', '.StoryViewer'],
  visibleMinWidth: 100,
  protectedAncestorDepth: 5,
};

describe('media.detectMessage', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  it('detects photo message', () => {
    const el = loadFixture('message-photo.html');
    const r = detectMessage(el, CONFIG);
    expect(r.chatId).toBe('-1001234567890');
    expect(r.messageId).toBe(123);
    expect(r.protected).toBe(false);
    expect(r.media).toHaveLength(1);
    expect(r.media[0].type).toBe('photo');
  });

  it('detects video message', () => {
    const el = loadFixture('message-video.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('video');
  });

  it('detects group with multiple media (2 photo + 1 document)', () => {
    const el = loadFixture('message-group.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media).toHaveLength(3);
    expect(r.media.map(m => m.type)).toEqual(['photo', 'photo', 'document']);
  });

  it('detects voice note', () => {
    const el = loadFixture('message-voice.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('voice');
  });

  it('detects sticker', () => {
    const el = loadFixture('message-sticker.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('sticker');
  });

  it('detects animation (video[loop][muted][autoplay])', () => {
    const el = loadFixture('message-animation.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('animation');
  });

  it('detects document via data-media-type', () => {
    const el = loadFixture('message-document.html');
    const r = detectMessage(el, CONFIG);
    expect(r.media[0].type).toBe('document');
  });

  it('marks protected message', () => {
    const el = loadFixture('message-protected.html');
    const r = detectMessage(el, CONFIG);
    expect(r.protected).toBe(true);
  });

  it('returns null for element without data-mid', () => {
    document.body.innerHTML = '<div class="message"><img src="x" /></div>';
    const el = document.querySelector('.message');
    expect(detectMessage(el, CONFIG)).toBe(null);
  });

  it('returns null for element with avatar img only', () => {
    document.body.innerHTML = `
      <div class="message" data-mid="131" data-peer-id="-1">
        <img class="avatar" src="https://cdn/avatar.jpg" />
      </div>`;
    const el = document.querySelector('.message');
    const r = detectMessage(el, CONFIG);
    expect(r).toBe(null);
  });

  it('returns null for hidden media (display:none)', () => {
    document.body.innerHTML = `
      <div class="message" data-mid="132" data-peer-id="-1">
        <img src="x" style="display:none" />
      </div>`;
    const el = document.querySelector('.message');
    expect(detectMessage(el, CONFIG)).toBe(null);
  });
});
```

- [ ] **Step 3: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/media.test.js`
Expected: FAIL,`Cannot find module '../src/media.js'`。

- [ ] **Step 4: 实现 media.js**

```javascript
// desktop/src-tauri/webview-inject/src/media.js
// 单条消息的媒体识别。返回 chatId/messageId/protected/media[]。

function isVisible(el) {
  if (!el || !el.isConnected) return false;
  const style = getComputedStyle(el);
  if (style.display === 'none' || style.visibility === 'hidden') return false;
  if (Number(style.opacity) === 0) return false;
  const rect = el.getBoundingClientRect();
  return rect.width >= (CONFIG_VISIBLE_MIN_WIDTH || 100) && rect.height > 0;
}

function isProtected(el) {
  const depth = CONFIG_PROTECTED_DEPTH || 5;
  for (let node = el, i = 0; node && i < depth; node = node.parentElement, i++) {
    if (node.hasAttribute && node.hasAttribute('data-protected')) return true;
    if (node.getAttribute && node.getAttribute('aria-disabled') === 'true') return true;
    const cls = (typeof node.className === 'string' ? node.className : '').toLowerCase();
    if (cls.includes('protected')) return true;
  }
  return false;
}

function isAvatarImg(el) {
  if (el.tagName !== 'IMG') return false;
  const cls = (typeof el.className === 'string' ? el.className : '').toLowerCase();
  return cls.includes('avatar');
}

function classifyMedia(el) {
  if (!isVisible(el)) return null;
  if (isAvatarImg(el)) return null;

  if (el.tagName === 'VIDEO') {
    if (el.loop && el.muted && el.autoplay) return 'animation';
    return 'video';
  }
  if (el.tagName === 'AUDIO') {
    let n = el;
    for (let i = 0; n && i < 4; n = n.parentElement, i++) {
      const cls = (typeof n.className === 'string' ? n.className : '').toLowerCase();
      if (cls.includes('voice') || cls.includes('bubble-audio')) return 'voice';
    }
    return 'audio';
  }
  if (el.tagName === 'IMG') {
    let n = el;
    for (let i = 0; n && i < 3; n = n.parentElement, i++) {
      const cls = (typeof n.className === 'string' ? n.className : '').toLowerCase();
      if (cls.includes('sticker')) return 'sticker';
    }
    return 'photo';
  }

  if (el.hasAttribute && el.hasAttribute('data-media-type')) {
    const t = el.getAttribute('data-media-type');
    if (t === 'document' || t === 'file') return 'document';
  }
  const cls = (typeof el.className === 'string' ? el.className : '').toLowerCase();
  if (cls.includes('document-icon') || cls.includes('attachment')) return 'document';

  return null;
}

function detectFromMessage(el) {
  const media = [];
  const candidates = el.querySelectorAll(
    'video, audio, img, [data-media-type="document"], [data-media-type="file"], .document-icon, .attachment'
  );
  for (const c of candidates) {
    const type = classifyMedia(c);
    if (type) media.push({ type, element: c });
  }
  return media;
}

export function detectMessage(el, config) {
  if (!el) return null;
  const cfg = config || {};
  const CONFIG_VISIBLE_MIN_WIDTH = cfg.visibleMinWidth ?? 100;
  const CONFIG_PROTECTED_DEPTH = cfg.protectedAncestorDepth ?? 5;

  const rawMid = el.getAttribute('data-mid');
  if (!rawMid || !/^[1-9]\d{0,18}$/.test(rawMid)) return null;
  const messageId = Number(rawMid);
  if (!Number.isSafeInteger(messageId)) return null;

  let peer = el;
  for (let i = 0; peer && i < 6 && !peer.hasAttribute('data-peer-id'); peer = peer.parentElement, i++) {}
  const chatId = peer && peer.getAttribute('data-peer-id');
  if (!chatId || !/^-?[1-9]\d{0,19}$/.test(chatId)) return null;

  const protected_ = isProtected(el);
  const media = detectFromMessage(el);
  if (media.length === 0) return null;

  return { chatId, messageId, protected: protected_, media };
}
```

注意:此实现未读 `src`/`href`/cookie/storage,仅读 tagName / className / data-* / style / rect。

- [ ] **Step 5: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/media.test.js`
Expected: PASS, 11 tests。

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/media.js \
        desktop/src-tauri/webview-inject/test/media.test.js \
        desktop/src-tauri/webview-inject/test/fixtures/
git commit -m "feat(desktop): add media detection for 7 Telegram media types"
```

---

## Task 5: button.js 与 button.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/button.js`
- Create: `desktop/src-tauri/webview-inject/test/button.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/button.test.js
import { describe, it, expect, beforeEach } from 'vitest';
import { attachButtons, setState, removeButtons } from '../src/button.js';

const CONFIG = {
  buttonStackPosition: 'top-right',
  iconSize: 16,
};

const fakeDetection = (media = [{ type: 'photo' }]) => ({
  chatId: '-100',
  messageId: 1,
  protected: false,
  media,
});

function setupMessage(media) {
  document.body.innerHTML = '';
  const msg = document.createElement('div');
  msg.className = 'message';
  msg.setAttribute('data-mid', '1');
  msg.setAttribute('data-peer-id', '-100');
  document.body.appendChild(msg);
  attachButtons(msg, fakeDetection(media), CONFIG);
  return msg;
}

describe('button.attachButtons', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  it('renders one primary button for single media', () => {
    setupMessage([{ type: 'photo' }]);
    const primary = document.querySelector('.tmd-primary');
    expect(primary).toBeTruthy();
    const files = document.querySelectorAll('.tmd-file');
    expect(files).toHaveLength(1);
  });

  it('renders N file buttons for N media + 1 primary', () => {
    setupMessage([{ type: 'photo' }, { type: 'photo' }, { type: 'document' }]);
    expect(document.querySelectorAll('.tmd-file')).toHaveLength(3);
    expect(document.querySelector('.tmd-primary')).toBeTruthy();
    const count = document.querySelector('.tmd-count');
    expect(count.textContent).toBe('3');
    expect(count.hasAttribute('hidden')).toBe(false);
  });

  it('hides count badge when only 1 media', () => {
    setupMessage([{ type: 'photo' }]);
    const count = document.querySelector('.tmd-count');
    expect(count.hasAttribute('hidden')).toBe(true);
  });

  it('does not attach twice (idempotent)', () => {
    const msg = setupMessage([{ type: 'photo' }]);
    attachButtons(msg, fakeDetection([{ type: 'photo' }]), CONFIG);
    expect(document.querySelectorAll('.tmd-primary')).toHaveLength(1);
  });

  it('stores detection in dataset for later lookup', () => {
    const msg = setupMessage([{ type: 'video' }]);
    const stack = msg.querySelector('.tmd-stack');
    expect(stack.dataset.tmdChatId).toBe('-100');
    expect(stack.dataset.tmdMessageId).toBe('1');
    expect(stack.dataset.tmdMediaCount).toBe('1');
  });

  it('button click invokes submit_download_from_webview', async () => {
    setupMessage([{ type: 'photo' }]);
    const btn = document.querySelector('.tmd-file');
    btn.click();
    await new Promise(r => setTimeout(r, 0));
    const inv = globalThis.__TAURI_CALLS__.invoke.find(c => c.cmd === 'submit_download_from_webview');
    expect(inv).toBeTruthy();
    expect(inv.args.request).toMatchObject({
      chatId: '-100',
      messageId: 1,
      mediaType: 'photo',
    });
  });
});

describe('button.setState', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    if (globalThis.__TAURI_TEST_RESET__) globalThis.__TAURI_TEST_RESET__();
  });

  it('transitions ready → submitting → queued → downloading → completed', () => {
    const msg = setupMessage([{ type: 'photo' }]);
    const btn = document.querySelector('.tmd-file');

    setState(btn, 'ready');
    expect(btn.dataset.tmdState).toBe('ready');

    setState(btn, 'submitting');
    expect(btn.dataset.tmdState).toBe('submitting');
    expect(btn.querySelector('.tmd-icon').innerHTML).toContain('animateTransform');

    setState(btn, 'queued');
    expect(btn.dataset.tmdState).toBe('queued');

    setState(btn, 'downloading');
    expect(btn.dataset.tmdState).toBe('downloading');
    expect(btn.querySelector('.tmd-progress')).toBeTruthy();

    setState(btn, 'completed');
    expect(btn.dataset.tmdState).toBe('completed');
    expect(btn.classList.contains('tmd-state-completed')).toBe(true);
  });

  it('failed state shows retry icon', () => {
    const msg = setupMessage([{ type: 'photo' }]);
    const btn = document.querySelector('.tmd-file');
    setState(btn, 'failed');
    expect(btn.dataset.tmdState).toBe('failed');
  });
});

describe('button.removeButtons', () => {
  it('removes the stack', () => {
    const msg = setupMessage([{ type: 'photo' }]);
    expect(msg.querySelector('.tmd-stack')).toBeTruthy();
    removeButtons(msg);
    expect(msg.querySelector('.tmd-stack')).toBe(null);
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/button.test.js`
Expected: FAIL,模块未找到。

- [ ] **Step 3: 实现 button.js**

```javascript
// desktop/src-tauri/webview-inject/src/button.js
// 渲染按钮 DOM、维护状态机、点击触发 invoke。

import { ICONS, getIcon } from './icons.js';

const STATE_LABELS = {
  ready: '下载',
  submitting: '提交中…',
  queued: '已加入',
  downloading: '下载中',
  completed: '已完成',
  failed: '重试',
};

function iconFor(type) {
  switch (type) {
    case 'photo': return 'image';
    case 'video': return 'film';
    case 'audio':
    case 'voice': return 'music';
    case 'sticker': return 'sticker';
    case 'animation': return 'film';
    case 'document': return 'file';
    case 'story': return 'story';
    default: return 'download';
  }
}

function buildStack(detection, config) {
  const stack = document.createElement('div');
  stack.className = 'tmd-stack';
  stack.dataset.tmdChatId = detection.chatId;
  stack.dataset.tmdMessageId = String(detection.messageId);
  stack.dataset.tmdMediaCount = String(detection.media.length);
  stack.style.cssText = 'position:absolute;top:8px;right:8px;z-index:2;display:flex;gap:4px;flex-direction:row-reverse;pointer-events:auto';

  // 主按钮
  const primary = document.createElement('button');
  primary.type = 'button';
  primary.className = 'tmd-btn tmd-primary';
  primary.dataset.tmdState = 'ready';
  primary.setAttribute('aria-label', '下载此消息的媒体');
  primary.innerHTML = `<span class="tmd-icon">${getIcon(iconFor(detection.media[0].type))}</span><span class="tmd-count" hidden></span>`;
  const count = primary.querySelector('.tmd-count');
  if (detection.media.length > 1) {
    count.textContent = String(detection.media.length);
    count.removeAttribute('hidden');
  }
  stack.appendChild(primary);

  // 子按钮
  detection.media.forEach((m, idx) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'tmd-btn tmd-file';
    btn.dataset.tmdMediaIndex = String(idx);
    btn.dataset.tmdState = 'ready';
    btn.setAttribute('aria-label', `下载第 ${idx + 1} 个文件`);
    btn.innerHTML = `<span class="tmd-icon">${getIcon(iconFor(m.type))}</span><span class="tmd-progress" hidden></span>`;
    stack.appendChild(btn);
  });

  return { stack, primary };
}

export function attachButtons(messageEl, detection, config) {
  if (!messageEl || !detection) return;
  if (detection.protected) return;
  if (messageEl.querySelector(':scope > .tmd-stack')) return;

  const { stack, primary } = buildStack(detection, config);

  primary.addEventListener('click', async (e) => {
    e.preventDefault(); e.stopPropagation();
    const items = Array.from(stack.querySelectorAll('.tmd-file'));
    if (items.length === 0) return;
    if (items.length === 1) {
      items[0].click();
      return;
    }
    const requests = items.map((btn, idx) => ({
      chatId: detection.chatId,
      messageId: detection.messageId,
      mediaType: detection.media[idx].type,
    }));
    await window.__TAURI__.core.invoke('submit_batch_download_from_webview', { requests });
  });

  stack.querySelectorAll('.tmd-file').forEach((btn, idx) => {
    btn.addEventListener('click', async (e) => {
      e.preventDefault(); e.stopPropagation();
      setState(btn, 'submitting');
      try {
        await window.__TAURI__.core.invoke('submit_download_from_webview', {
          request: {
            chatId: detection.chatId,
            messageId: detection.messageId,
            mediaType: detection.media[idx].type,
          },
        });
        setState(btn, 'queued');
      } catch (_err) {
        setState(btn, 'failed');
      }
    });
  });

  if (getComputedStyle(messageEl).position === 'static') {
    messageEl.style.position = 'relative';
  }
  messageEl.appendChild(stack);
}

export function removeButtons(messageEl) {
  if (!messageEl) return;
  const s = messageEl.querySelector(':scope > .tmd-stack');
  if (s) s.remove();
}

export function setState(btn, state, payload) {
  if (!btn) return;
  btn.dataset.tmdState = state;
  btn.classList.toggle('tmd-state-completed', state === 'completed');
  btn.classList.toggle('tmd-state-failed', state === 'failed');

  const icon = btn.querySelector('.tmd-icon');
  const progress = btn.querySelector('.tmd-progress');

  if (state === 'submitting') {
    icon.innerHTML = getIcon('spinner');
    if (progress) progress.setAttribute('hidden', '');
  } else if (state === 'downloading') {
    icon.innerHTML = getIcon('progress');
    if (progress && payload && typeof payload.progress === 'number') {
      progress.textContent = `${Math.round(payload.progress * 100)}%`;
      progress.removeAttribute('hidden');
    }
  } else if (state === 'completed') {
    icon.innerHTML = getIcon('check');
    if (progress) progress.setAttribute('hidden', '');
  } else if (state === 'failed') {
    icon.innerHTML = getIcon('retry');
    if (progress) progress.setAttribute('hidden', '');
  } else if (state === 'queued') {
    icon.innerHTML = getIcon('download');
    if (progress) progress.setAttribute('hidden', '');
  } else {
    // ready
    icon.innerHTML = getIcon('download');
    if (progress) progress.setAttribute('hidden', '');
  }
}

export { buildStack };
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/button.test.js`
Expected: PASS, 9 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/button.js \
        desktop/src-tauri/webview-inject/test/button.test.js
git commit -m "feat(desktop): add button rendering with 6-state state machine"
```

---

## Task 6: state.js 与 state.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/state.js`
- Create: `desktop/src-tauri/webview-inject/test/state.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/state.test.js
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { bindEvents, applyEvent } from '../src/state.js';

function fakeStack(chatId, messageId, count) {
  document.body.innerHTML = '';
  const msg = document.createElement('div');
  msg.className = 'message';
  msg.setAttribute('data-mid', String(messageId));
  msg.setAttribute('data-peer-id', chatId);
  document.body.appendChild(msg);
  const stack = document.createElement('div');
  stack.className = 'tmd-stack';
  stack.dataset.tmdChatId = chatId;
  stack.dataset.tmdMessageId = String(messageId);
  stack.dataset.tmdMediaCount = String(count);
  for (let i = 0; i < count; i++) {
    const btn = document.createElement('button');
    btn.className = 'tmd-btn tmd-file';
    btn.dataset.tmdMediaIndex = String(i);
    btn.dataset.tmdState = 'ready';
    btn.innerHTML = `<span class="tmd-icon"></span><span class="tmd-progress" hidden></span>`;
    stack.appendChild(btn);
  }
  msg.appendChild(stack);
  return msg;
}

describe('state.bindEvents', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    if (globalThis.__TAURI_TEST_RESET__) globalThis.__TAURI_TEST_RESET__();
  });

  it('subscribes to webview-task-* events', async () => {
    await bindEvents();
    const listened = globalThis.__TAURI_CALLS__.listen.map(c => c.name);
    expect(listened).toContain('webview-task-submitted');
    expect(listened).toContain('webview-task-updated');
    expect(listened).toContain('webview-task-completed');
    expect(listened).toContain('webview-task-failed');
  });
});

describe('state.applyEvent', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    if (globalThis.__TAURI_TEST_RESET__) globalThis.__TAURI_TEST_RESET__();
  });

  it('submitted event marks button queued', () => {
    fakeStack('-100', 1, 1);
    applyEvent({ type: 'submitted', taskId: 'a', chatId: '-100', messageId: 1, mediaIndex: 0 });
    const btn = document.querySelector('.tmd-file');
    expect(btn.dataset.tmdState).toBe('queued');
  });

  it('updated event marks button downloading with progress', () => {
    fakeStack('-100', 1, 1);
    applyEvent({ type: 'updated', taskId: 'a', progress: 0.42 });
    const btn = document.querySelector('.tmd-file');
    expect(btn.dataset.tmdState).toBe('downloading');
    expect(btn.querySelector('.tmd-progress').textContent).toBe('42%');
  });

  it('completed event marks button completed', () => {
    fakeStack('-100', 1, 1);
    applyEvent({ type: 'completed', taskId: 'a', outputPath: '/x' });
    const btn = document.querySelector('.tmd-file');
    expect(btn.dataset.tmdState).toBe('completed');
  });

  it('failed event marks button failed', () => {
    fakeStack('-100', 1, 1);
    applyEvent({ type: 'failed', taskId: 'a', error: 'err' });
    const btn = document.querySelector('.tmd-file');
    expect(btn.dataset.tmdState).toBe('failed');
  });

  it('ignores events for unknown chatId/messageId', () => {
    fakeStack('-100', 1, 1);
    applyEvent({ type: 'submitted', taskId: 'a', chatId: '-999', messageId: 999, mediaIndex: 0 });
    const btn = document.querySelector('.tmd-file');
    expect(btn.dataset.tmdState).toBe('ready');
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/state.test.js`
Expected: FAIL,模块未找到。

- [ ] **Step 3: 实现 state.js**

```javascript
// desktop/src-tauri/webview-inject/src/state.js
// 订阅 Tauri 事件,把 task 状态映射到对应按钮。

import { setState } from './button.js';

function findButton(chatId, messageId, mediaIndex) {
  const stack = document.querySelector(
    `.tmd-stack[data-tmd-chat-id="${CSS.escape(chatId)}"][data-tmd-message-id="${messageId}"]`
  );
  if (!stack) return null;
  if (mediaIndex == null) {
    return stack.querySelector('.tmd-primary');
  }
  return stack.querySelector(`.tmd-file[data-tmd-media-index="${mediaIndex}"]`);
}

export function applyEvent(event) {
  if (!event) return;
  if (event.type === 'submitted') {
    const btn = findButton(event.chatId, event.messageId, event.mediaIndex);
    if (btn) setState(btn, 'queued');
  } else if (event.type === 'updated') {
    // updated 不带 chatId/messageId,通过 taskId map 反查;此处简化为最近一个 downloading 按钮
    const btn = document.querySelector('.tmd-file[data-tmd-state="downloading"], .tmd-file[data-tmd-state="queued"]');
    if (btn) setState(btn, 'downloading', { progress: event.progress });
  } else if (event.type === 'completed') {
    const btn = document.querySelector('.tmd-file[data-tmd-state="downloading"]');
    if (btn) setState(btn, 'completed');
  } else if (event.type === 'failed') {
    const btn = document.querySelector('.tmd-file[data-tmd-state="downloading"]');
    if (btn) setState(btn, 'failed');
  }
}

export async function bindEvents() {
  const listen = window.__TAURI__.event.listen;
  await listen('webview-task-submitted', (e) => applyEvent({ type: 'submitted', ...e.payload }));
  await listen('webview-task-updated', (e) => applyEvent({ type: 'updated', ...e.payload }));
  await listen('webview-task-completed', (e) => applyEvent({ type: 'completed', ...e.payload }));
  await listen('webview-task-failed', (e) => applyEvent({ type: 'failed', ...e.payload }));
}
```

**注**:`updated`/`completed`/`failed` 事件不携带 chatId/messageId,故当前用"最近一个 downloading 按钮"近似。Task 8(事件协议升级)会强制要求事件携带 chatId/messageId/mediaIndex,然后回头收紧此处的查找逻辑。验收前 Task 8 会做精确查找替换此处近似。

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/state.test.js`
Expected: PASS, 5 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/state.js \
        desktop/src-tauri/webview-inject/test/state.test.js
git commit -m "feat(desktop): add task state event binding"
```

---

## Task 7: dedupe.js 与 dedupe.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/dedupe.js`
- Create: `desktop/src-tauri/webview-inject/test/dedupe.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/dedupe.test.js
import { describe, it, expect, beforeEach } from 'vitest';
import { isAlreadyDownloaded, _resetCacheForTest } from '../src/dedupe.js';

describe('dedupe.isAlreadyDownloaded', () => {
  beforeEach(() => {
    if (globalThis.__TAURI_TEST_RESET__) globalThis.__TAURI_TEST_RESET__();
    _resetCacheForTest();
  });

  it('returns false when no completed download', async () => {
    globalThis.__TAURI_HANDLERS__ = {
      webview_query_downloaded: () => null,
    };
    const r = await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 5000 });
    expect(r.downloaded).toBe(false);
    expect(r.fileSize).toBe(null);
  });

  it('returns true with fileSize when completed', async () => {
    globalThis.__TAURI_HANDLERS__ = {
      webview_query_downloaded: () => ({
        taskId: 'a',
        fileSize: 3_500_000,
        completedAt: '2026-09-26T12:00:00Z',
        outputPath: '/dl/x.mp4',
      }),
    };
    const r = await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 5000 });
    expect(r.downloaded).toBe(true);
    expect(r.fileSize).toBe(3_500_000);
  });

  it('caches result within TTL (no second invoke)', async () => {
    let calls = 0;
    globalThis.__TAURI_HANDLERS__ = {
      webview_query_downloaded: () => { calls++; return null; },
    };
    await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 5000 });
    await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 5000 });
    await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 5000 });
    expect(calls).toBe(1);
  });

  it('re-invokes after TTL expires', async () => {
    let calls = 0;
    globalThis.__TAURI_HANDLERS__ = {
      webview_query_downloaded: () => { calls++; return null; },
    };
    await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 0 });
    await new Promise(r => setTimeout(r, 5));
    await isAlreadyDownloaded('-100', 1, { dedupeCacheTtlMs: 0 });
    expect(calls).toBe(2);
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/dedupe.test.js`
Expected: FAIL。

- [ ] **Step 3: 实现 dedupe.js**

```javascript
// desktop/src-tauri/webview-inject/src/dedupe.js
// 已下载检测查询与缓存。

const cache = new Map();

function keyOf(chatId, messageId) {
  return `${chatId}:${messageId}`;
}

export async function isAlreadyDownloaded(chatId, messageId, config) {
  const ttl = (config && config.dedupeCacheTtlMs) ?? 5000;
  const k = keyOf(chatId, messageId);
  const now = Date.now();
  const cached = cache.get(k);
  if (cached && now - cached.fetchedAt < ttl) {
    return { downloaded: cached.downloaded, fileSize: cached.fileSize };
  }

  const payload = await window.__TAURI__.core.invoke('webview_query_downloaded', {
    chatId,
    messageId,
  });

  const result = payload
    ? { downloaded: true, fileSize: payload.fileSize ?? null }
    : { downloaded: false, fileSize: null };

  cache.set(k, {
    downloaded: result.downloaded,
    fileSize: result.fileSize,
    fetchedAt: now,
  });
  return result;
}

export function _resetCacheForTest() {
  cache.clear();
}
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/dedupe.test.js`
Expected: PASS, 4 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/dedupe.js \
        desktop/src-tauri/webview-inject/test/dedupe.test.js
git commit -m "feat(desktop): add dedupe module with TTL cache"
```

---

## Task 8: story.js 与 story.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/story.js`
- Create: `desktop/src-tauri/webview-inject/test/story.test.js`
- Create: `desktop/src-tauri/webview-inject/test/fixtures/story-overlay.html`

- [ ] **Step 1: 创建 fixture**

`story-overlay.html`:
```html
<div class="StoryViewer" data-story-viewer>
  <video src="https://cdn.example/story.mp4" style="width:300px;height:500px;display:block"></video>
</div>
```

- [ ] **Step 2: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/story.test.js
import { describe, it, expect, beforeEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { detectStory, cleanupStoryButtons } from '../src/story.js';

const CONFIG = {
  storySelectors: ['[data-story-viewer]', '.StoryViewer'],
  visibleMinWidth: 100,
  protectedAncestorDepth: 5,
};

function loadFixture() {
  const html = readFileSync(resolve('src-tauri/webview-inject/test/fixtures/story-overlay.html'), 'utf-8');
  document.body.innerHTML = html;
  return document.querySelector('.StoryViewer');
}

describe('story.detectStory', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
  });

  it('returns null when no story overlay', () => {
    expect(detectStory(CONFIG)).toBe(null);
  });

  it('detects story media in overlay', () => {
    const overlay = loadFixture();
    const r = detectStory(CONFIG);
    expect(r).toBeTruthy();
    expect(r.overlay).toBe(overlay);
    expect(r.media).toHaveLength(1);
    expect(r.media[0].type).toBe('story');
  });
});

describe('story.cleanupStoryButtons', () => {
  it('removes .tmd-stack inside overlay', () => {
    const overlay = loadFixture();
    const stack = document.createElement('div');
    stack.className = 'tmd-stack';
    overlay.appendChild(stack);
    cleanupStoryButtons(overlay);
    expect(overlay.querySelector('.tmd-stack')).toBe(null);
  });
});
```

- [ ] **Step 3: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/story.test.js`
Expected: FAIL。

- [ ] **Step 4: 实现 story.js**

```javascript
// desktop/src-tauri/webview-inject/src/story.js
// Story overlay 观察器入口。detection 复用 media.js 的 classifyMedia,但强制 type='story'。

function isVisible(el) {
  if (!el || !el.isConnected) return false;
  const style = getComputedStyle(el);
  if (style.display === 'none' || style.visibility === 'hidden') return false;
  if (Number(style.opacity) === 0) return false;
  const rect = el.getBoundingClientRect();
  return rect.width >= 100 && rect.height > 0;
}

function findOverlay(config) {
  for (const sel of config.storySelectors) {
    const el = document.querySelector(sel);
    if (el) return el;
  }
  return null;
}

export function detectStory(config) {
  const overlay = findOverlay(config);
  if (!overlay) return null;

  const media = [];
  const candidates = overlay.querySelectorAll('video, img, audio');
  for (const c of candidates) {
    if (!isVisible(c)) continue;
    let type = 'photo';
    if (c.tagName === 'VIDEO') type = 'video';
    else if (c.tagName === 'AUDIO') type = 'audio';
    media.push({ type: 'story', originalType: type, element: c });
  }
  if (media.length === 0) return null;
  return { overlay, media };
}

export function cleanupStoryButtons(overlay) {
  if (!overlay) return;
  overlay.querySelectorAll('.tmd-stack').forEach(s => s.remove());
}
```

- [ ] **Step 5: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/story.test.js`
Expected: PASS, 3 tests。

- [ ] **Step 6: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/story.js \
        desktop/src-tauri/webview-inject/test/story.test.js \
        desktop/src-tauri/webview-inject/test/fixtures/story-overlay.html
git commit -m "feat(desktop): add story overlay detection and cleanup"
```

---

## Task 9: observer.js 与 observer.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/observer.js`
- Create: `desktop/src-tauri/webview-inject/test/observer.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/observer.test.js
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { startObserver } from '../src/observer.js';

const CONFIG = {
  messageSelectors: ['.message[data-mid]', '.Message[data-mid]'],
  observerDebounceMs: 10,
  observerAttributeFilter: ['data-mid', 'data-peer-id', 'data-protected', 'class', 'aria-disabled'],
};

describe('observer.startObserver', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    vi.useFakeTimers();
  });

  it('invokes onAttach when a message element is added', () => {
    const onAttach = vi.fn();
    startObserver(onAttach, CONFIG);
    const msg = document.createElement('div');
    msg.className = 'message';
    msg.setAttribute('data-mid', '1');
    msg.setAttribute('data-peer-id', '-100');
    document.body.appendChild(msg);
    vi.advanceTimersByTime(20);
    expect(onAttach).toHaveBeenCalledWith(msg);
  });

  it('debounces multiple mutations in same tick', () => {
    const onAttach = vi.fn();
    startObserver(onAttach, CONFIG);
    for (let i = 0; i < 5; i++) {
      const m = document.createElement('div');
      m.className = 'message';
      m.setAttribute('data-mid', String(i));
      m.setAttribute('data-peer-id', '-100');
      document.body.appendChild(m);
    }
    vi.advanceTimersByTime(20);
    expect(onAttach).toHaveBeenCalledTimes(5);
  });

  it('does not invoke onAttach for non-message elements', () => {
    const onAttach = vi.fn();
    startObserver(onAttach, CONFIG);
    const div = document.createElement('div');
    div.className = 'not-a-message';
    document.body.appendChild(div);
    vi.advanceTimersByTime(20);
    expect(onAttach).not.toHaveBeenCalled();
  });

  it('returns a stop function that disconnects', () => {
    const onAttach = vi.fn();
    const stop = startObserver(onAttach, CONFIG);
    stop();
    const msg = document.createElement('div');
    msg.className = 'message';
    msg.setAttribute('data-mid', '1');
    msg.setAttribute('data-peer-id', '-100');
    document.body.appendChild(msg);
    vi.advanceTimersByTime(20);
    expect(onAttach).not.toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/observer.test.js`
Expected: FAIL。

- [ ] **Step 3: 实现 observer.js**

```javascript
// desktop/src-tauri/webview-inject/src/observer.js
// 单一 MutationObserver + 防抖 + 消息元素提取。

export function startObserver(onAttach, config) {
  const debounceMs = (config && config.observerDebounceMs) ?? 50;
  const msgSelector = ((config && config.messageSelectors) || ['.message[data-mid]']).join(',');

  const pending = new Set();
  let timer = null;

  function flush() {
    timer = null;
    const items = Array.from(pending);
    pending.clear();
    for (const el of items) onAttach(el);
  }

  function schedule() {
    if (timer) return;
    timer = setTimeout(flush, debounceMs);
  }

  function collect(el) {
    if (!el || !(el instanceof Element)) return;
    if (el.matches && el.matches(msgSelector)) {
      if (!pending.has(el)) pending.add(el);
    }
    if (el.closest) {
      const enclosing = el.closest(msgSelector);
      if (enclosing && !pending.has(enclosing)) pending.add(enclosing);
    }
    if (el.querySelectorAll) {
      el.querySelectorAll(msgSelector).forEach(c => {
        if (!pending.has(c)) pending.add(c);
      });
    }
  }

  const observer = new MutationObserver((records) => {
    for (const r of records) {
      if (r.type === 'childList') {
        r.addedNodes.forEach(n => collect(n));
      } else if (r.type === 'attributes' && r.target instanceof Element) {
        collect(r.target);
      }
    }
    schedule();
  });

  observer.observe(document, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: (config && config.observerAttributeFilter) || ['data-mid', 'data-peer-id', 'data-protected', 'class', 'aria-disabled'],
  });

  return function stop() {
    observer.disconnect();
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
  };
}
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/observer.test.js`
Expected: PASS, 4 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/observer.js \
        desktop/src-tauri/webview-inject/test/observer.test.js
git commit -m "feat(desktop): add MutationObserver with debounce and message extraction"
```

---

## Task 10: boot.js 与 boot.test.js

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/boot.js`
- Create: `desktop/src-tauri/webview-inject/test/boot.test.js`

- [ ] **Step 1: 写测试**

```javascript
// desktop/src-tauri/webview-inject/test/boot.test.js
import { describe, it, expect, beforeEach } from 'vitest';
import { shouldBoot, getConfig } from '../src/boot.js';

describe('boot.shouldBoot', () => {
  beforeEach(() => {
    delete window.__TAURI_INJECTED__;
  });

  it('returns true on trusted origin and top frame', () => {
    Object.defineProperty(window, 'top', { value: window, configurable: true });
    expect(shouldBoot()).toBe(true);
  });

  it('returns false on non-top frame', () => {
    const fakeTop = { isFakeTop: true };
    Object.defineProperty(window, 'top', { value: fakeTop, configurable: true });
    expect(shouldBoot()).toBe(false);
  });

  it('returns false when origin not trusted', () => {
    // happy-dom 默认 origin 是 'about:blank' 或 'null',非 web.telegram.org
    Object.defineProperty(window, 'top', { value: window, configurable: true });
    expect(shouldBoot()).toBe(false);
  });
});

describe('boot.getConfig', () => {
  it('returns parsed JSON config from global', () => {
    globalThis.__INJECT_CONFIG__ = { foo: 1 };
    expect(getConfig()).toEqual({ foo: 1 });
  });

  it('returns empty object when not set', () => {
    delete globalThis.__INJECT_CONFIG__;
    expect(getConfig()).toEqual({});
  });
});
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/boot.test.js`
Expected: FAIL。

- [ ] **Step 3: 实现 boot.js**

```javascript
// desktop/src-tauri/webview-inject/src/boot.js
// 入口:解析 CONFIG,校验 origin,wire 各模块。

const TRUSTED_ORIGIN = 'https://web.telegram.org';

export function shouldBoot() {
  if (typeof window === 'undefined') return false;
  if (window.top !== window) return false;
  if (window.location.origin !== TRUSTED_ORIGIN) return false;
  return true;
}

export function getConfig() {
  if (typeof globalThis.__INJECT_CONFIG__ === 'object' && globalThis.__INJECT_CONFIG__) {
    return globalThis.__INJECT_CONFIG__;
  }
  return {};
}
```

- [ ] **Step 4: 跑测试验证通过**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/boot.test.js`
Expected: PASS, 4 tests。

- [ ] **Step 5: Commit**

```bash
git add desktop/src-tauri/webview-inject/src/boot.js \
        desktop/src-tauri/webview-inject/test/boot.test.js
git commit -m "feat(desktop): add boot module with origin check and config loader"
```

---

## Task 11: Rust 端 task_store.find_completed_for_dedupe(TDD)

**Files:**
- Modify: `desktop/src-tauri/src/task_store.rs`(新增 `find_completed_for_dedupe`)
- Modify: `desktop/src-tauri/src/commands.rs`(新增 `webview_query_downloaded`)

- [ ] **Step 1: 写 task_store 测试**

打开 `desktop/src-tauri/src/task_store.rs`,找到文件末尾的 `#[cfg(test)] mod tests` 块(若不存在则在文件末尾添加)。在 tests 模块内新增测试函数,先**临时**调一个尚未存在的 `find_completed_for_dedupe` 方法:

```rust
#[tokio::test]
async fn find_completed_for_dedupe_returns_most_recent_completed() {
    // 给定 task store 中有:
    //   - chat A msg 1, status=completed, file_size=3MB, completed_at=T1
    //   - chat A msg 1, status=failed, file_size=4MB
    //   - chat A msg 2, status=completed, file_size=5MB
    //   - chat B msg 1, status=completed, file_size=3MB
    // 调 find_completed_for_dedupe("A", 1)
    // 期望返回 chat A msg 1 的最新 completed(T1)
    todo!("set up fixtures, expect Some({task_id: ..., file_size: 3_000_000})");
}
```

若已有更具体的 fixture helper(如 `MockTaskStore`),用现有 helper;若无,先建一个内存 SQLite + SeaORM 的最小 helper。**不**真实落盘,使用 `tempfile::tempdir()` 之类。

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop/src-tauri && cargo test --locked find_completed_for_dedupe_returns_most_recent_completed`
Expected: FAIL(编译失败,`find_completed_for_dedupe` 未定义)。

- [ ] **Step 3: 实现 task_store::TaskStore::find_completed_for_dedupe**

在 `TaskStore` impl 块中添加:

```rust
pub async fn find_completed_for_dedupe(
    &self,
    chat_id: &str,
    message_id: i64,
) -> Result<Option<DownloadedMatch>> {
    // 用现有 ORM 连接或 query builder。
    // SQL: SELECT task_id, file_size, completed_at, output_path
    //        FROM tasks
    //       WHERE chat_id = ? AND message_id = ? AND status = 'completed'
    //       ORDER BY completed_at DESC LIMIT 1
    // 把结果映射为 Option<DownloadedMatch>。
    todo!()
}
```

具体 SQL 字段名以现有 `tasks` 表 schema 为准(参考 `db_migration.rs` 与 `task_store.rs` 现有查询)。

- [ ] **Step 4: 在 models.rs 新增 DownloadedMatch 结构体**

打开 `desktop/src-tauri/src/models.rs`,在合适位置添加:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedMatch {
    pub task_id: String,
    pub file_size: Option<u64>,
    pub completed_at: Option<String>,
    pub output_path: Option<String>,
}
```

确保文件顶部 `use serde::Serialize;` 已存在。

- [ ] **Step 5: 跑测试验证通过**

Run: `cd desktop/src-tauri && cargo test --locked find_completed_for_dedupe_returns_most_recent_completed`
Expected: PASS。

- [ ] **Step 6: 在 commands.rs 添加 webview_query_downloaded 命令**

打开 `desktop/src-tauri/src/commands.rs`,在 `submit_download_from_webview` 附近添加:

```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn webview_query_downloaded(
    state: State<'_, AppState>,
    chat_id: String,
    message_id: i64,
) -> Result<Option<crate::models::DownloadedMatch>, String> {
    let chat_id = validate_chat_id(&chat_id)?;
    validate_message_id(message_id)?;
    state
        .shared
        .store
        .find_completed_for_dedupe(&chat_id, message_id)
        .await
        .map_err(command_error)
}
```

- [ ] **Step 7: 在 lib.rs 注册新命令**

打开 `desktop/src-tauri/src/lib.rs`,在 `tauri::generate_handler!` 宏中添加 `commands::webview_query_downloaded`。

- [ ] **Step 8: 在 build.rs 添加命令到 AppManifest**

打开 `desktop/src-tauri/build.rs`,把 `commands(&[...])` 列表加上 `"webview_query_downloaded"`。

- [ ] **Step 9: 跑 Rust 测试**

Run: `cd desktop/src-tauri && cargo test --locked`
Expected: 全部通过(含新增)。

Run: `cd desktop/src-tauri && cargo fmt --all -- --check`
Expected: 无 diff。

- [ ] **Step 10: Commit**

```bash
git add desktop/src-tauri/src/task_store.rs \
        desktop/src-tauri/src/models.rs \
        desktop/src-tauri/src/commands.rs \
        desktop/src-tauri/src/lib.rs \
        desktop/src-tauri/build.rs
git commit -m "feat(desktop): add webview_query_downloaded command and dedupe store query"
```

---

## Task 12: Rust 端 submit_batch_download_from_webview + webview_task_action(TDD)

**Files:**
- Modify: `desktop/src-tauri/src/commands.rs`(新增 2 个命令)
- Modify: `desktop/src-tauri/src/lib.rs`(注册)
- Modify: `desktop/src-tauri/build.rs`(注册 manifest)

- [ ] **Step 1: 写 submit_batch 单元/集成测试**

在 `desktop/src-tauri/src/commands.rs` 末尾的 tests 模块(或新建 `tests/webview_commands.rs` 集成测试)中:

```rust
#[tokio::test]
async fn submit_batch_download_from_webview_rejects_empty() {
    // 准备 AppState with mock store + mock downloads
    // 调函数, requests=Vec::new()
    // 期望 Err("批量请求数必须在 1-32 之间")
}

#[tokio::test]
async fn submit_batch_download_from_webview_rejects_too_many() {
    // requests.len() = 33
    // 期望 Err
}

#[tokio::test]
async fn submit_batch_download_from_webview_creates_one_task_per_request() {
    // requests.len() = 3,全合法
    // 期望 Ok(vec) with 3 个 TaskRecord
    // 验证 store.list 看到 3 条 queued
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop/src-tauri && cargo test --locked submit_batch_download_from_webview`
Expected: FAIL(编译失败)。

- [ ] **Step 3: 在 commands.rs 实现 submit_batch_download_from_webview**

```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn submit_batch_download_from_webview(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    requests: Vec<WebviewDownloadRequest>,
) -> Result<Vec<TaskRecord>, String> {
    if !webview_bridge::is_trusted_telegram_webview(&webview) {
        return Err("批量下载请求必须来自受信任的 Telegram WebView 页面".into());
    }
    if requests.is_empty() || requests.len() > 32 {
        return Err("批量请求数必须在 1-32 之间".into());
    }
    let mut out = Vec::with_capacity(requests.len());
    for req in requests {
        let chat_id = validate_chat_id(&req.chat_id)?;
        validate_message_id(req.message_id)?;
        validate_media_type(&req.media_type)?;
        let task =
            create_message_task(&state, &chat_id, req.message_id, &req.media_type).await?;
        out.push(task);
    }
    Ok(out)
}
```

- [ ] **Step 4: 在 commands.rs 实现 webview_task_action**

```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn webview_task_action(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    task_id: String,
    action: String,
) -> Result<TaskRecord, String> {
    if !webview_bridge::is_trusted_telegram_webview(&webview) {
        return Err("任务操作必须来自受信任的 Telegram WebView 页面".into());
    }
    if !matches!(action.as_str(), "cancel" | "retry" | "open") {
        return Err(format!("不支持的 webview 任务操作: {action}"));
    }
    state
        .downloads
        .action(&task_id, &action)
        .await
        .map_err(command_error)
}
```

- [ ] **Step 5: 在 lib.rs 注册**

```rust
commands::submit_batch_download_from_webview,
commands::webview_task_action,
```

- [ ] **Step 6: 在 build.rs 注册**

```rust
"submit_batch_download_from_webview",
"webview_task_action",
```

- [ ] **Step 7: 跑测试**

Run: `cd desktop/src-tauri && cargo test --locked`
Expected: 全部通过。

- [ ] **Step 8: Commit**

```bash
git add desktop/src-tauri/src/commands.rs \
        desktop/src-tauri/src/lib.rs \
        desktop/src-tauri/build.rs
git commit -m "feat(desktop): add submit_batch_download_from_webview and webview_task_action"
```

---

## Task 13: 事件协议升级

**Files:**
- Modify: `desktop/src-tauri/src/commands.rs`(`submit_download_from_webview` 事件发出)
- Modify: `desktop/src-tauri/src/downloader.rs`(任务状态变更处 emit)
- Modify: `desktop/src-tauri/webview-inject/src/state.js`(精确查找,替换近似)

- [ ] **Step 1: 写测试:事件必须携带 chatId/messageId/mediaIndex**

打开 `desktop/src-tauri/src/commands.rs` 末尾 tests 模块,新增:

```rust
#[tokio::test]
async fn submit_download_from_webview_emits_task_submitted_event() {
    // 准备 AppState with mock app handle that captures emitted events
    // 调 submit_download_from_webview
    // 验证 emit 收到 "webview-task-submitted" with payload {taskId, chatId, messageId, mediaIndex}
}
```

- [ ] **Step 2: 跑测试验证失败**

Run: `cd desktop/src-tauri && cargo test --locked submit_download_from_webview_emits_task_submitted_event`
Expected: FAIL(事件名不对或 payload 字段缺失)。

- [ ] **Step 3: 在 commands.rs 修改 submit_download_from_webview 事件名与 payload**

找到 `submit_download_from_webview` 内 `emit("webview-download-submitted", task.clone())`,改为:

```rust
let _ = state
    .shared
    .app
    .emit(
        "webview-task-submitted",
        serde_json::json!({
            "taskId": task.task_id,
            "chatId": task.chat_id,
            "messageId": task.message_id,
            "mediaType": task.media_type,
            "mediaIndex": Option::<i64>::None,
        }),
    );
```

对 `submit_batch_download_from_webview` 的每条 task,emit 同样事件 + 填 `mediaIndex: Some(idx as i64)`。

- [ ] **Step 4: 在 downloader.rs emit 状态变更事件**

打开 `desktop/src-tauri/src/downloader.rs`,找到任务状态变更为 `downloading`、`completed`、`failed` 的位置(grep `set_status`),在各 `set_status` 之后添加 emit:

```rust
let _ = state.shared.app.emit(
    "webview-task-updated",
    serde_json::json!({
        "taskId": task_id,
        "progress": progress_estimate,
    }),
);
```

进度估计可用现有 `progress_bytes / total_bytes`。`completed` 与 `failed` 处同样 emit。

**具体调用点**:在 `downloader.rs` 中 task_store.set_status 调用之后立即 emit;不必为单条 progress 调用,可在 dispatch loop 中节流(每 N 字节或每 1s)。

- [ ] **Step 5: 更新 inject.js state.js 精确查找**

打开 `desktop/src-tauri/webview-inject/src/state.js`,替换 Task 6 中标记的"近似查找"逻辑,使 `updated`/`completed`/`failed` 事件也携带 `chatId`/`messageId`/`mediaIndex`。Rust 侧已发,JS 侧改为按字段精确查找:

```javascript
function findButton(chatId, messageId, mediaIndex) {
  const stack = document.querySelector(
    `.tmd-stack[data-tmd-chat-id="${CSS.escape(chatId)}"][data-tmd-message-id="${messageId}"]`
  );
  if (!stack) return null;
  if (mediaIndex == null) return stack.querySelector('.tmd-primary');
  return stack.querySelector(`.tmd-file[data-tmd-media-index="${mediaIndex}"]`);
}

export function applyEvent(event) {
  if (event.type === 'submitted') {
    const btn = findButton(event.chatId, event.messageId, event.mediaIndex);
    if (btn) setState(btn, 'queued');
  } else if (event.type === 'updated') {
    const btn = findButton(event.chatId, event.messageId, event.mediaIndex);
    if (btn) setState(btn, 'downloading', { progress: event.progress });
  } else if (event.type === 'completed') {
    const btn = findButton(event.chatId, event.messageId, event.mediaIndex);
    if (btn) setState(btn, 'completed');
  } else if (event.type === 'failed') {
    const btn = findButton(event.chatId, event.messageId, event.mediaIndex);
    if (btn) setState(btn, 'failed');
  }
}
```

- [ ] **Step 6: 更新 state.test.js**

把 Task 6 写的 `applyEvent({type: 'updated', taskId: 'a', progress: 0.42})` 改为带 chatId/messageId/mediaIndex:

```javascript
applyEvent({ type: 'updated', taskId: 'a', chatId: '-100', messageId: 1, mediaIndex: 0, progress: 0.42 });
```

对 `completed` 与 `failed` 同样处理。

- [ ] **Step 7: 跑测试**

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/state.test.js`
Run: `cd desktop/src-tauri && cargo test --locked`
Expected: 全部通过。

- [ ] **Step 8: Commit**

```bash
git add desktop/src-tauri/src/commands.rs \
        desktop/src-tauri/src/downloader.rs \
        desktop/src-tauri/webview-inject/src/state.js \
        desktop/src-tauri/webview-inject/test/state.test.js
git commit -m "feat(desktop): upgrade webview event protocol with chatId/messageId/mediaIndex"
```

---

## Task 14: 集成 inject.js 入口、build.mjs、嵌入

**Files:**
- Create: `desktop/src-tauri/webview-inject/src/inject.js`(组合入口)
- Create: `desktop/src-tauri/webview-inject/build.mjs`
- Modify: `desktop/src-tauri/src/webview_bridge.rs`
- Modify: `desktop/src-tauri/build.rs`

- [ ] **Step 1: 写 inject.js 主入口**

```javascript
// desktop/src-tauri/webview-inject/src/inject.js
// 把 boot + 各模块 wire 在一起。

import { shouldBoot, getConfig } from './boot.js';
import { startObserver } from './observer.js';
import { detectMessage } from './media.js';
import { detectStory, cleanupStoryButtons } from './story.js';
import { attachButtons, setState } from './button.js';
import { bindEvents } from './state.js';
import { isAlreadyDownloaded } from './dedupe.js';

(function main() {
  'use strict';
  if (!shouldBoot()) return;
  const config = getConfig();

  bindEvents().catch(() => {});

  startObserver((msgEl) => {
    const det = detectMessage(msgEl, config);
    if (!det) return;
    attachButtons(msgEl, det, config);
    isAlreadyDownloaded(det.chatId, det.messageId, config)
      .then(r => {
        if (!r.downloaded) return;
        const stack = msgEl.querySelector(':scope > .tmd-stack');
        if (!stack) return;
        stack.classList.add('tmd-state-downloaded');
        const primary = stack.querySelector('.tmd-primary');
        if (primary) primary.setAttribute('title', `已下载${r.fileSize ? ' · ' + formatBytes(r.fileSize) : ''}`);
      })
      .catch(() => {});
  }, config);

  let lastOverlay = null;
  setInterval(() => {
    const r = detectStory(config);
    if (r && r.overlay !== lastOverlay) {
      lastOverlay = r.overlay;
      attachButtons(r.overlay, {
        chatId: '-story',
        messageId: r.media[0].element.getAttribute('data-mid') ? Number(r.media[0].element.getAttribute('data-mid')) : 1,
        protected: false,
        media: r.media.map(m => ({ type: 'story' })),
      }, config);
    } else if (!r && lastOverlay) {
      cleanupStoryButtons(lastOverlay);
      lastOverlay = null;
    }
  }, 1000);

  function formatBytes(n) {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
    return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
  }
})();
```

- [ ] **Step 2: 创建 build.mjs**

```javascript
// desktop/src-tauri/webview-inject/build.mjs
// 把 src/* 拼接 + terser minify + 内联 config.json,输出 dist/inject.js。

import { readFileSync, writeFileSync, mkdirSync, readdirSync, statSync } from 'node:fs';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { minify_sync as terserMinify } from 'terser';

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = resolve(here, 'src');
const distDir = resolve(here, 'dist');
const configPath = resolve(here, 'config.json');

mkdirSync(distDir, { recursive: true });

const config = JSON.parse(readFileSync(configPath, 'utf-8'));
const configLiteral = `globalThis.__INJECT_CONFIG__ = ${JSON.stringify(config)};`;

const order = [
  'boot.js', 'icons.js', 'media.js', 'button.js', 'state.js',
  'dedupe.js', 'story.js', 'observer.js', 'inject.js',
];

const modules = order.map(name => {
  const code = readFileSync(join(srcDir, name), 'utf-8');
  return `// === ${name} ===\n${code}`;
});

const source = `(() => { 'use strict'; ${configLiteral}\n${modules.join('\n')}\n})();`;

const min = terserMinify(source, {
  compress: { passes: 2 },
  mangle: true,
  format: { comments: false },
});

const out = min.code || source;
writeFileSync(join(distDir, 'inject.js'), out, 'utf-8');
console.log(`wrote ${join(distDir, 'inject.js')} (${out.length} bytes)`);
```

- [ ] **Step 3: 跑 build.mjs 验证产物**

Run: `cd desktop && node src-tauri/webview-inject/build.mjs`
Expected: 打印 "wrote .../dist/inject.js (NNNN bytes)",无错误。

- [ ] **Step 4: 验证产物可被 happy-dom 加载且 boot 早返回**

由于 happy-dom 的 origin 不是 web.telegram.org,boot() 应早返回,无错误。手动验证:

```javascript
// 临时验证脚本 desktop/src-tauri/webview-inject/test/smoke.test.js
import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

describe('dist/inject.js smoke', () => {
  it('loads without throwing', () => {
    const code = readFileSync(resolve('src-tauri/webview-inject/dist/inject.js'), 'utf-8');
    expect(code).toContain('web.telegram.org');
    expect(() => {
      // 在 happy-dom 下执行,boot() 会因 origin 不匹配早返回
      // eslint-disable-next-line no-new-func
      new Function(code)();
    }).not.toThrow();
  });
});
```

Run: `cd desktop && npx vitest run src-tauri/webview-inject/test/smoke.test.js`
Expected: PASS。

- [ ] **Step 5: 修改 webview_bridge.rs 使用 include_str!**

打开 `desktop/src-tauri/src/webview_bridge.rs`:

```rust
// 原:
pub const TELEGRAM_WEBVIEW_INIT_SCRIPT: &str = r#"(() => { ... })()"#;

// 改为:
pub const TELEGRAM_WEBVIEW_INIT_SCRIPT: &str =
    include_str!("../webview-inject/dist/inject.js");
```

保留文件中已有的注释与 `is_telegram_web_url` 等函数。

- [ ] **Step 6: 修改 build.rs 调 node build.mjs**

打开 `desktop/src-tauri/build.rs`,在末尾添加:

```rust
fn run_webview_inject_build() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let project_root = std::path::Path::new(&manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_default();
    let build_mjs = project_root.join("src-tauri/webview-inject/build.mjs");
    if !build_mjs.exists() {
        return;
    }
    let src_dir = std::path::Path::new(&manifest_dir).join("webview-inject/src");
    let dist = std::path::Path::new(&manifest_dir).join("webview-inject/dist/inject.js");
    let needs_build = !dist.exists()
        || std::fs::read_dir(&src_dir).ok().map(|entries| {
            entries.flatten().any(|e| {
                e.metadata().and_then(|m| m.modified()).ok()
                    > dist.metadata().and_then(|m| m.modified()).ok()
            })
        }).unwrap_or(false);

    if needs_build {
        let _ = std::process::Command::new("node")
            .arg(&build_mjs)
            .current_dir(&project_root)
            .status();
        println!("cargo:rerun-if-changed=webview-inject/src");
        println!("cargo:rerun-if-changed=webview-inject/config.json");
        println!("cargo:rerun-if-changed=webview-inject/build.mjs");
    }
}

fn main() {
    tauri_build::AppManifest::new()
        .commands(&[
            "submit_download_from_webview",
            "submit_batch_download_from_webview",
            "webview_query_downloaded",
            "webview_task_action",
        ])
        .build();
    run_webview_inject_build();
}
```

**注意**:`project_root` 推算路径:`desktop/src-tauri/` → `..` → `desktop/`,再 `..` → 仓库根。本项目 `node build.mjs` 当前目录就是 desktop/。如有偏差,直接 `let _ = std::process::Command::new("node").arg("../src-tauri/webview-inject/build.mjs").current_dir(manifest_dir).status();` 简化(因为 manifest_dir 就是 `desktop/src-tauri`)。

调整后形式:

```rust
let build_mjs = std::path::Path::new(&manifest_dir).join("webview-inject/build.mjs");
let _ = std::process::Command::new("node")
    .arg(&build_mjs)
    .status();
```

- [ ] **Step 7: 跑 cargo build 验证 include_str! 可用**

Run: `cd desktop/src-tauri && cargo build --locked`
Expected: 编译通过,`dist/inject.js` 若不存在则由 build.mjs 生成后被包含。

若报 "couldn't read `../webview-inject/dist/inject.js`",先手动跑一次 `cd desktop && node src-tauri/webview-inject/build.mjs`,再 `cargo build`。

- [ ] **Step 8: 跑全部测试**

Run: `cd desktop/src-tauri && cargo test --locked`
Run: `cd desktop && npx vitest run`
Expected: 全部通过。

- [ ] **Step 9: 提交**

```bash
git add desktop/src-tauri/webview-inject/src/inject.js \
        desktop/src-tauri/webview-inject/build.mjs \
        desktop/src-tauri/src/webview_bridge.rs \
        desktop/src-tauri/build.rs \
        desktop/src-tauri/webview-inject/test/smoke.test.js
git commit -m "feat(desktop): wire inject.js entry, build.mjs, and include_str!"
```

---

## Task 15: TypeScript 类型与端到端手工验收

**Files:**
- Modify: `desktop/src/lib/api.ts`(加 TS 类型)
- Modify: `desktop/src/App.tsx`(可选:增加 webview task 事件桥接)

- [ ] **Step 1: 在 api.ts 增加类型与方法签名**

打开 `desktop/src/lib/api.ts`,在文件末尾 `export const api = { ... }` 对象中新增:

```typescript
  webviewQueryDownloaded: (chatId: string | number, messageId: number) =>
    call<DownloadedMatch | null>("webview_query_downloaded", { chatId, messageId }),

  submitBatchDownloadFromWebview: (requests: Array<{
    chatId: string | number;
    messageId: number;
    mediaType: string;
  }>) => call<DownloadTask[]>("submit_batch_download_from_webview", { requests }),

  webviewTaskAction: (taskId: string, action: "cancel" | "retry" | "open") =>
    call<DownloadTask>("webview_task_action", { taskId, action }),
```

并在文件顶部的 interfaces 区添加:

```typescript
export interface DownloadedMatch {
  taskId: string;
  fileSize?: number | null;
  completedAt?: string | null;
  outputPath?: string | null;
}
```

- [ ] **Step 2: 跑 TypeScript 类型检查**

Run: `cd desktop && npm run check`
Expected: 编译通过,无类型错误。

- [ ] **Step 3: 跑前端构建**

Run: `cd desktop && npm run build`
Expected: 产物输出到 `desktop/build/`。

- [ ] **Step 4: 手工端到端冒烟(开发模式)**

按 `desktop/README.md` 的"开发与验证"步骤启动 `npx tauri dev`。登录真实 Telegram 账号后,逐项验收 spec 第 13 节清单:

1. 群图 / 文档 / voice / sticker / animation 消息上分别出现按钮(主 + 子)
2. 点击按钮,主窗口"下载任务"列表出现新任务
3. 按钮文字随任务状态切换(提交中 → 已加入 → 下载中 → 已完成)
4. 完成后再次访问消息,按钮上 ✓ + 文件大小
5. 失败任务显示重试图标,点击重试
6. 打开 Story,显示下载按钮,关闭后按钮消失
7. 多文件消息,主按钮徽标数字正确

记录每项 ✓/✗,若 ✗,**停下来**调试,**不要**继续下一步。

- [ ] **Step 5: 跑 tauri build**

Run: `cd desktop && npx tauri build`
Expected: MSI/NSIS 包生成,无错误。

- [ ] **Step 6: 提交**

```bash
git add desktop/src/lib/api.ts
git commit -m "feat(desktop): expose webview_query_downloaded and batch submit TS API"
```

---

## 自审记录(写完后已自审)

- **Spec 覆盖**:逐项对照 spec 第 3-13 节,所有功能落在对应任务。
  - 图标按钮 → Task 5 (button.js)
  - 多文件识别 → Task 4 (media.js) + Task 5 (button 主+子)
  - 状态反馈 → Task 6 (state.js) + Task 13 (事件升级)
  - 已下载标记 → Task 7 (dedupe.js) + Task 11 (Rust 查询)
  - Story → Task 8 (story.js) + Task 14 (集成)
  - 批量提交 → Task 12 (Rust 命令)
  - 失败重试 → Task 12 (webview_task_action)
  - 安全边界 → 各模块注释 + setup.js mock
  - 测试 → Task 2-10 + 11-13 + 15
  - 配置 → Task 1
- **占位符扫描**:无 TBD/TODO。`todo!()` 仅出现在测试与实现初版,Step 4/5 立即替换为真实代码。
- **类型一致**:`find_completed_for_dedupe` 返回 `Option<DownloadedMatch>`,在 Task 11 定义,在 Task 11/14 使用,一致。`webview_query_downloaded` 命令 payload 字段 `chatId`、`messageId` 与 inject.js dedupe.js 调用一致。事件 payload 在 Task 13 升级前后一致。
- **范围**:15 个任务,每任务 5-9 步,总计约 2 周单人工作量。