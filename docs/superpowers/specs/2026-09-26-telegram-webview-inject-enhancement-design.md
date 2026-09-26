# Telegram WebView 注入脚本(查看器方案) — 设计规范

- 日期: 2026-09-26(2026-09-26 修订:由"聊天列表消息按钮"改为"媒体查看器工具栏按钮")
- 状态: 待实现
- 锚定实现: [Greasy Fork #446342](https://greasyfork.org/zh-CN/scripts/446342-telegram-media-downloader) — 本规范的所有 selector、检测方式、URL 提取逻辑均来自该脚本的验证过的实现,不自行发明。

## 0.1 唯一增量:下载交给 Rust 管理

**浏览器侧的一切 — 查看器检测、按钮注入、样式、URL 提取、文件名解析 — 全部照抄验证脚本 #446342 的做法,不自行发明。**

本项目唯一的、也是刻意的差异:

| | 验证脚本 | 本方案 |
|---|---|---|
| 点击下载后 | 浏览器 fetch + Range 拼接 + `<a download>` 保存 | **Tauri IPC 把 `{mediaUrl, fileName, fileType, source}` 交给 Rust** |
| 任务管理 | 无(浏览器原生下载) | **Rust:队列 / 进度 / 暂停 / 重试 / 断点续传 / 并发分块 / 历史** |
| 进度显示 | 页面右下角浮动进度条 | 按钮状态(5 态)+ 客户端任务页(Rust 事件驱动) |

其余能力对照准则:

- **脚本有的,我们照做**:查看器路径、webk/webz 双版本、Story、置顶音频、禁保存媒体下载、原生按钮样式融入。
- **脚本没有的,我们不加**(浏览器的活):批量、右键菜单、拖拽、下载历史(浏览器侧)。
- **Rust 侧的活是增量**:凡是"任务管理"范畴的能力(queue/resume/retry/progress)都放 Rust;注入脚本只负责"找到 URL、交给 Rust、显示状态"。

## 0. 为什么是查看器方案

聊天列表里的图片是**缩略图预览**(`img.thumbnail`),原始文件 URL 只在**媒体查看器加载时**才出现在 DOM。因此:

- 聊天列表按钮只能下到糊图 — 技术上不可行
- 查看器打开时,`<video>.currentSrc` / `img.src` / `audio.src` 就是原始文件 URL
- 验证过的脚本(downloader #446342,23.9 万安装)全程只做查看器路径

本方案照做。

## 1. 架构

```
Telegram Web 子 WebView (web.telegram.org/k 或 /a)
┌────────────────────────────────────────────────────┐
│ inject.js(注入脚本)                                 │
│  ├─ boot.js     入口:origin 校验 + 启动 watcher      │
│  ├─ watcher.js  500ms 轮询:查看器/Story/置顶音频出现? │
│  ├─ detect.js   在打开的查看器里找到媒体元素与类型     │
│  ├─ button.js   向查看器工具栏注入按钮 + 5 状态机      │
│  ├─ extract.js  从媒体元素读 URL;从 URL 解析文件名     │
│  ├─ task-state.js  任务状态查询(按文件名)+ TTL 缓存      │
│  ├─ state.js    Tauri 事件订阅 → 按钮状态             │
│  ├─ icons.js    内联 SVG(已完成)                    │
│  └─ config.json webk/webz 两套 selector + 参数        │
└────────────────┬───────────────────────────────────┘
                 │ window.__TAURI__.core.invoke
                 │ ('submit_download_from_webview', {request:{mediaUrl, fileName, fileType, source}})
                 ▼
        Rust 客户端(见 webview-http-downloader-design.md)
        下载执行 / 任务管理 / 断点续传 / 全部由 Rust 承担
```

## 2. 支持范围

| 场景 | 支持 | 说明 |
|---|---|---|
| 媒体查看器(图片/视频/GIF) | ✅ | 主路径,webk + webz |
| Story(故事) | ✅ | 独立查看器 |
| 置顶音频(pinned audio) | ✅ | webk 专属,唯一带 `data-mid` 的场景 |
| 语音消息(查看器内) | ✅ | audio 元素 |
| 聊天列表内联按钮 | ❌ | 技术不可行(缩略图) |
| 批量下载 | ❌ v1 | 方向已确认(消息菜单:仅视频/全部),暂缓,见 §10 |

## 3. Selector 表(逐条来自验证脚本)

### 3.1 webk(`web.telegram.org/k/`)

| 目标 | Selector |
|---|---|
| 媒体查看器容器 | `.media-viewer-whole` |
| 媒体元素容器 | `.media-viewer-movers .media-viewer-aspecter` |
| 查看器按钮栏 | `.media-viewer-topbar .media-viewer-buttons` |
| 视频元素 | `.media-viewer-aspecter` 内 `video` |
| 图片元素 | `img.thumbnail` |
| 视频控制条 | `.default__controls.ckin__controls`,右侧 `.bottom-controls .right-controls` |
| Story 容器 | `#stories-viewer` |
| Story 头部按钮区 | `[class^='_ViewerStoryHeaderRight']` |
| Story 底部按钮区 | `[class^='_ViewerStoryFooterRight']` |
| Story 视频 | `video.media-video` |
| Story 图片 | `img.media-photo` |
| 置顶音频容器 | `.pinned-audio` |
| 置顶音频工具区 | `.pinned-container-wrapper-utils` |
| 音频元素(全局) | `audio-element`(自定义元素),内部 `.audio`(HTMLAudioElement) |

### 3.2 webz(`web.telegram.org/a/`)

| 目标 | Selector |
|---|---|
| 媒体查看器 | `#MediaViewer .MediaViewerSlide--active` |
| 查看器按钮栏 | `#MediaViewer .MediaViewerActions` |
| 视频容器 | `.MediaViewerContent > .VideoPlayer`,内部 `video` |
| 图片 | `.MediaViewerContent > div > img` |
| 视频控制条 | `.VideoPlayerControls .buttons`(在 `.spacer` 前插入) |
| Story 容器 | `#StoryViewer` |
| Story 头部 | `.GrsJNw3y`(找不到时用 `.DropdownMenu` 的父节点) |
| Story 视频 | 容器内 `video` |
| Story 图片 | 容器内最后一个 `img.PVZ8TOWS` |

**版本判定**:脚本用 URL 路径区分(`location.pathname.startsWith('/k/')` vs `/a/`,或 hostname `webk.telegram.org` / `webz.telegram.org`)。两套 selector 互不混用。

## 4. 模块设计

### 4.1 boot.js

- 校验 `window.top === window` 且 origin 为 `https://web.telegram.org`(含 webk/webz 子域)
- 解析版本:`webk` | `webz`
- 启动 `watcher.js` 轮询
- 启动 `state.js` 事件订阅

### 4.2 watcher.js

- `setInterval(500ms)`(验证脚本的 `REFRESH_DELAY`)
- 每轮依次检查(按优先级):
  1. Story 查看器是否打开 → 有则确保 Story 按钮存在
  2. 媒体查看器是否打开 → 有则确保查看器按钮存在
  3. 置顶音频是否存在 → 有则确保置顶按钮存在
- 检查"按钮是否已存在":容器内 `querySelector('.tel-download')` 为空才注入
- 查看器关闭后按钮自然随 DOM 移除,无需清理逻辑

防抖细节:同一个查看器内媒体切换(左右滑动)时,容器不变、媒体元素变。按钮保留,点击时**实时读取**当前媒体元素(不缓存 URL)。

### 4.3 detect.js

```ts
type MediaKind = 'photo' | 'video' | 'animation' | 'audio' | 'voice' | 'story';

interface Detected {
  kind: MediaKind;
  element: Element;   // video/img/audio 元素本身
  container: Element; // 按钮注入目标
  source: 'viewer' | 'story' | 'pinned-audio';
}
```

- `detectViewer(version, config)` → `Detected | null`
- `detectStory(version, config)` → `Detected | null`
- `detectPinnedAudio(version, config)` → `Detected | null`
- animation 判定:`<video>` 无 `controls` 或容器带 GIF 特征(看验证脚本:未加载的视频走 `video` 路径,不细分;我们的 `animation` 类型主要用于图标选择,判定用 `video.loop && video.muted`)

### 4.4 extract.js

**URL 提取**(逐条对应验证脚本):

| 场景 | 读法 |
|---|---|
| webk 视频 | `mediaAspecter.querySelector('video').src` |
| webz 视频 | `videoPlayer.querySelector('video').currentSrc` |
| webk 图片 | `img.thumbnail.src` |
| webz 图片 | `img.src` |
| Story 视频 | `video.src \|\| video.currentSrc \|\| video.querySelector('source')?.src` |
| 音频 | `audioEl.getAttribute('src')` |

**文件名解析**(验证脚本的做法):

1. 从 URL 尾部 JSON 解析:Telegram 文件 URL 的最后一段常为 URL-encoded JSON
   ```js
   const metadata = JSON.parse(decodeURIComponent(url.split('/').pop()));
   if (metadata.fileName) fileName = metadata.fileName;
   ```
   解析失败静默跳过(URL 形状随版本变化)
2. 兜底:`hashCode(url).toString(36) + '.' + ext`(ext 从 MIME/类型推断)
3. URL 读取为**允许行为**(HTTP 下载器 spec §7 已定);仍不读 cookie/storage

### 4.5 button.js

**按钮样式 — 融入原生 UI**(验证脚本的做法):

- webk:元素 `<button class="btn-icon tgico-download tel-download">`,内嵌 `<span class="tgico button-icon">` 图标(字形 ``);插入 `.media-viewer-buttons` 最前(prepend);视频控制条变体为 `btn-icon default__button tgico-download tel-download`,插到 `.bottom-controls .right-controls` 最前
- webz:元素 `<button class="Button smaller translucent-white round tel-download">`,内嵌 `<i class="icon icon-download">`;`prepend` 进 `.MediaViewerActions`,视频时插到 `.VideoPlayerControls .buttons` 内 `.spacer` 之后
- Story:同风格,插头部按钮区(webk `btn-icon rp tel-download` + ripple;webz `Button TkphaPyQ tiny translucent-white round tel-download`)
- 置顶音频:webk `btn-icon tgico-download _tel_download_button_pinned_container`(脚本自带防重类,另加 `tel-download`)

**官方按钮接管 — 受限媒体(禁存频道)的核心路径**(脚本 L740-752 的检测逻辑 + 我们的克隆接管):

- 检测:webk 官方按钮是 `button.btn-icon`,受限时带 `.hide` 且按字形文本匹配(`textContent === ""`,脚本 L743-749);可见态带 `tgico-download` 类(脚本查重条件,L789)。webz 按 `button[title="Download"]` 识别(脚本 L582/L597)
- 接管(webk 与 webz 同策略):解除 `.hide`(脚本 L744)→ `cloneNode(true)`(克隆不携带 Telegram 的内联事件监听)→ 克隆加 `tel-download` 标记、绑定我们的点击 → 插到原按钮之后 → 原按钮 `style.display = 'none'` 隐藏。位置与外观 100% 保持官方
- **绝不**执行脚本的 `btn.click()`(L750-753):克隆与新建按钮的点击一律 `onDownload`/`onRetry` → Tauri IPC → Rust,不触发官方/浏览器下载
- 回退路径:未找到官方按钮(或 Story/置顶音频)→ 按上述原生标记新建(插入位置同上)
- 幂等:每轮先查 `scope.querySelector('.tel-download')`(检测容器 + 官方按钮实际所在的顶栏/Actions 条);Telegram 重渲染清掉克隆时,下一轮自动重新接管(自愈)

**状态机**(我们的增强,验证脚本只有浏览器原生进度条):

```
ready → submitting → queued → downloading → completed
                                          ↘ failed → (点击重试)
```

- 状态在按钮的 `data-tel-state` 属性上;`downloading` 时按钮内嵌小圆环进度
- 状态由 `state.js` 按 `taskId` 更新
- 点击时实时 `extract` 当前媒体 → `invoke` → 记录 `taskId → button` 映射
- 重复点击(ready 时)= 强制重下,由 Rust 端 `duplicatePolicy` 决定落地行为

### 4.6 task-state.js(任务状态感知)

**目的**:打开查看器时,用户立刻看到这个文件在 Rust 里的状态 — 从没下过 / 正在下载 / 已下载过 / 上次失败。

- 查询键 = **文件名**(`extract` 解析出的 `metadata.fileName`);查不到文件名时跳过查询,按钮直接 ready
- `invoke('webview_query_task_state', {fileName})` → `{ state, taskId?, progress?, fileSize?, completedAt? }`
  - `state`: `'queued' | 'downloading' | 'completed' | 'failed' | 'none'`(取该文件名**最近一条**任务;已取消视为 none)
- 5s TTL 缓存(同一文件名查过一次短时间内不重复查)
- 按结果设置按钮初始状态:
  | Rust 状态 | 按钮初始状态 |
  |---|---|
  | `queued` | `queued`(已加入队列) |
  | `downloading` | `downloading` + 进度 |
  | `completed` | `completed`(✓) |
  | `failed` | `failed`(可点击重试) |
  | `none` / 查询失败 | `ready` |
- **关键**:若状态为 `queued`/`downloading`,必须把 `taskId` 注册进 `state.js` 的映射(`registerTask`),这样后续进度/完成事件能继续更新这个按钮 — 即"重开查看器仍能看到实时进度"

### 4.7 state.js

- `listen('webview-task-submitted' | 'updated' | 'completed' | 'failed')`
- 事件 payload 均带 `taskId`;按 `taskId → button` 映射更新
- 无需 chatId/messageId 匹配(查看器方案下不存在)

### 4.8 config.json(编译期内联)

```json
{
  "refreshDelayMs": 500,
  "dedupeCacheTtlMs": 5000,
  "trustedOrigins": ["https://web.telegram.org", "https://webk.telegram.org", "https://webz.telegram.org"],
  "webk": {
    "viewerRoot": ".media-viewer-whole",
    "mediaAspecter": ".media-viewer-movers .media-viewer-aspecter",
    "viewerButtons": ".media-viewer-topbar .media-viewer-buttons",
    "videoControlsRight": ".bottom-controls .right-controls",
    "imgSelector": "img.thumbnail",
    "storyRoot": "#stories-viewer",
    "storyHeader": "[class^='_ViewerStoryHeaderRight']",
    "storyFooter": "[class^='_ViewerStoryFooterRight']",
    "storyVideo": "video.media-video",
    "storyImage": "img.media-photo",
    "pinnedAudio": ".pinned-audio",
    "pinnedAudioUtils": ".pinned-container-wrapper-utils",
    "buttonClass": "btn-icon tgico-download tel-download"
  },
  "webz": {
    "viewerRoot": "#MediaViewer",
    "activeSlide": ".MediaViewerSlide--active",
    "viewerActions": ".MediaViewerActions",
    "videoPlayer": ".MediaViewerContent > .VideoPlayer",
    "imgSelector": ".MediaViewerContent > div > img",
    "videoControls": ".VideoPlayerControls .buttons",
    "storyRoot": "#StoryViewer",
    "storyHeader": ".GrsJNw3y",
    "storyHeaderFallback": ".DropdownMenu",
    "storyImage": "img.PVZ8TOWS",
    "buttonClass": "Button smaller translucent-white round tel-download"
  }
}
```

## 5. Tauri 接口

### 5.1 提交下载

```ts
window.__TAURI__.core.invoke('submit_download_from_webview', {
  request: {
    mediaUrl: string,        // 核心字段
    fileName: string | null, // extract 解析结果,可空
    fileType: 'photo' | 'video' | 'animation' | 'audio' | 'voice' | 'story',
    source: 'viewer' | 'story' | 'pinned-audio',
  }
})
→ 返回 TaskRecord(含 taskId)
```

### 5.2 查询任务状态

```ts
window.__TAURI__.core.invoke('webview_query_task_state', { fileName })
→ {
    state: 'queued' | 'downloading' | 'completed' | 'failed' | 'none',
    taskId?: string,      // queued/downloading 时必填(注册给 state.js 用)
    progress?: number,    // downloading 时 0..1
    fileSize?: number | null,
    completedAt?: string | null,
  }
```

取该文件名**最近一条**任务的状态;已取消(cancelled)的任务视为 `none`。

### 5.3 任务事件(反向)

| 事件 | payload | 触发 |
|---|---|---|
| `webview-task-submitted` | `{taskId}` | 任务创建后 |
| `webview-task-updated` | `{taskId, progress}` | 下载中(节流) |
| `webview-task-completed` | `{taskId, outputPath}` | 完成 |
| `webview-task-failed` | `{taskId, error}` | 失败 |

## 6. 安全边界

**允许**(HTTP 下载器 spec §7 已定):

- 读媒体元素 `src` / `currentSrc`(URL 为签名短期有效,转交 Rust 不泄露会话)
- 读 URL 尾部 JSON 解析文件名

**永不**:

- `cookie` / `localStorage` / `sessionStorage` / `indexedDB`
- `window.__TAURI_INTERNALS__`
- `<a href>` 属性
- 订阅 `keydown` / `paste` / `copy` / `beforeunload`
- 任何到非 `web.telegram.org` 域的数据外发

**Rust 侧**:URL 过白名单(`*.telegram.org` / `*.cdn-telegram.org`)+ 拒私有 IP + 仅 https。

## 7. 验证方式(不做单元测试)

合成 DOM fixture 无法代表真实 Telegram Web(虚拟列表、canvas、版本相关类名),**本方案不做单测**。验证 = 真机:

1. `npx tauri dev`,WebView 内登录真实账号
2. 依次打开:图片、视频、GIF、语音、Story、置顶音频
3. 确认按钮出现在查看器工具栏、样式与原生融合
4. 点击 → Rust 任务创建 → 下载完成 → 文件可播放/可打开
5. 禁保存频道的内容同样验证
6. 大文件(>10MB)验证 HTTP Range 路径
7. 杀掉客户端重启,未完成任务断点续传

## 8. 风险与缓解

| 风险 | 缓解 |
|---|---|
| Telegram Web DOM 结构变化,selector 失效 | selector 集中于 `config.json`,改一处;验证脚本社区会先发现变化 |
| webk/webz 结构差异大 | 两套 selector 独立维护,照验证脚本 |
| 查看器内切换媒体时按钮残留 | 按钮不缓存 URL,点击时实时 extract |
| Story 头部 class 是 hash(`.GrsJNw3y`) | 提供多个 fallback(`.DropdownMenu` 父节点);失效时真机快速定位 |
| 文件名解析失败 | 静默兜底到 hash 文件名;不影响下载 |

## 9. 验收标准

- [ ] 图片 / 视频 / GIF / 语音 / Story / 置顶音频 六类,按钮全部出现
- [ ] 按钮样式与 Telegram Web 原生按钮视觉一致
- [ ] 5 状态切换正确(提交中/已加入/下载中/已完成/失败)
- [ ] 点击下载,文件完整、可播放
- [ ] 禁保存频道内容可下载
- [ ] 已下载文件重新打开查看器时按钮显示 ✓
- [ ] Rust 端零 MTProto 依赖(见 HTTP 下载器 spec)

## 10. 后续(v1 之后,暂缓)— 消息上的批量下载

用户已确认的方向,**本批不实现**:

- 聊天列表的多媒体消息(相册)上显示"下载"菜单:
  - `仅下载视频 (N)` — 遍历消息内 `<video>` 元素抓真文件 URL
  - `下载全部 (N)` — 视频直接抓;图片必须走查看器自动遍历(列表内是缩略图)
- 入队方式:JS 循环调 `submit_download_from_webview`,Rust 侧无需改动
- 选择状态需存脚本内存(聊天列表虚拟滚动会回收 DOM,滚回时恢复勾选)

**实现前必须真机验证的两个假设**:

1. 聊天列表视频元素的 `src`:未播放时是否已挂载(懒加载?)→ 决定是否需要先触发加载
2. `src` 是否等于原文件(而非降质预览流)→ 比对同一视频"列表抓取"与"查看器抓取"的文件大小
3. 查看器导航(上一张/下一张)与位置指示的 selector → 自动遍历用

## 11. 参考

- 验证脚本源码: https://greasyfork.org/zh-CN/scripts/446342-telegram-media-downloader/code
- 下载执行与任务管理: `docs/superpowers/specs/2026-09-26-webview-http-downloader-design.md`
- 本方案替代旧文 `telegram-webview-inject-enhancement-design.md` 的"聊天列表按钮"部分(已验证不可行:聊天列表只有缩略图)
