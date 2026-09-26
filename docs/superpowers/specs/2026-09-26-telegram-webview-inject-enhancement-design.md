# Telegram WebView 注入脚本增强 — 设计规范

- 日期: 2026-09-26
- 状态: 待用户审阅
- 范围: 单人完整版,约 2 周。不含拖拽下载、不含设置页 UI、不含 i18n、不含批量选择面板、不含 CDN。

## 1. 摘要

把现有 `Telegram Web` 子 WebView 中的 137 行内联 init script 重构为独立 JS 模块集,提供:

- 图标化按钮 + hover 高亮 + 视觉融入 Telegram Web
- 单消息识别全部可见媒体(photo / video / audio / voice / sticker / animation / document / story)
- 多文件消息渲染 1 个主按钮 + N 个子按钮
- 点击后状态实时反馈(提交中 / 已入队 / 进行中 / 完成 / 失败重试)
- 本地已下载文件标记。匹配键:`chat_id + message_id`(`file_size` 不参与匹配,仅取自同一消息最近一次已完成下载用于显示「✓ 已下载 · 3.4 MB」)
- Story 下载走 MTProto 任务系统

桥接只增加 3 个新 Tauri 命令。现有 `submit_download_from_webview` 行为不变,仅事件名升级。

## 2. 现状摘要

- `desktop/src-tauri/src/webview_bridge.rs:147` 嵌 137 行 `TELEGRAM_WEBVIEW_INIT_SCRIPT`,单按钮、文本样式
- `desktop/src-tauri/src/commands.rs:1115` `submit_download_from_webview` 接 `{chatId, messageId, mediaType}`,走 `create_message_task` → MTProto 任务系统
- 已发出 `webview-download-submitted` 事件但前端未监听
- 任务系统(`downloader.rs` + `task_store.rs`)能力完整:分块、断点续传、多线程、并发、文件校验
- 安全:`is_trusted_telegram_webview` 校验 label + URL;init script 不读 URL/cookie/storage

## 3. 目标

1. 按钮视觉与 Telegram Web 融合(图标 + hover + 紧凑)
2. 单消息识别全部可见媒体,主按钮 + 子按钮呈现
3. 5 状态实时反馈(ready / submitting / queued / downloading / completed / failed)
4. 已下载文件标记,点击强制重下
5. Story 支持(走 MTProto 任务)
6. 失败可重试(主流程)

## 4. 非目标

- 拖拽下载
- 批量选择 UI(消息内多文件 OK,跨消息批量不提供)
- i18n(仅中文)
- 设置页 UI(配置以编译期常量形式存在于 `config.js`,改配置 = 改 Rust 重 build)
- 多账号切换
- 插件化媒体检测
- CDN 传输(独立 spec)

## 5. 架构

### 5.1 模块布局

```
desktop/src-tauri/
├── webview-inject/
│   ├── src/
│   │   ├── observer.js   MutationObserver 与消息生命周期
│   │   ├── media.js      媒体识别(7 类 + story)
│   │   ├── button.js     按钮渲染与状态机
│   │   ├── state.js      任务事件订阅与按钮同步
│   │   ├── dedupe.js     已下载检测查询与缓存
│   │   ├── story.js      Story overlay 观察器
│   │   ├── icons.js      内联 SVG 字符串集合
│   │   └── config.json   编译期常量(JSON 文件,build.mjs 读后内联)
│   ├── build.mjs         简易打包(concat + minify)
│   ├── dist/
│   │   └── inject.js     最终嵌入文件(进 .gitignore)
│   ├── test/             vitest + happy-dom 测试
│   └── README.md
├── src/
│   ├── webview_bridge.rs 改用 include_str!("../webview-inject/dist/inject.js")
│   ├── commands.rs       新增 3 个命令
│   └── task_store.rs     新增 dedupe 查询
└── build.rs              调 node build.mjs 重新生成 dist/inject.js
```

### 5.2 嵌入方式

```rust
// webview_bridge.rs
pub const TELEGRAM_WEBVIEW_INIT_SCRIPT: &str =
    include_str!("../webview-inject/dist/inject.js");
```

`build.rs` 在 `cargo build` 之前用 `std::process::Command::new("node")` 调 `build.mjs`,若 `src/` 任一文件 `mtime` > `dist/inject.js` 则重新打包。`build.mjs` 读 `config.json`,序列化为顶层 `const CONFIG = {...}` 字面量,再 concat `observer.js` 等模块 + minify 输出单文件 `dist/inject.js`。

无 Node 时打包脚本 `no-op`(发警告,不阻断)。Rust 端不强制 Node 依赖,改注入脚本的人自己装 Node 跑一次。

### 5.3 运行时结构

```
Telegram Web (child WebView)
│
├── inject.js
│   ├── boot()         解析 config, 校验 origin + top frame
│   ├── observer       单一 MutationObserver, 50ms 防抖
│   ├── messageHandlers
│   │   ├── detectMessage(msg) → MediaDetection | null
│   │   ├── attachButtons(msg, det)
│   │   └── removeButtons(msg)
│   ├── storyHandlers
│   │   ├── observeStoryViewer()
│   │   └── cleanupStory()
│   ├── dedupeCache    Map<chatId+messageId, {downloaded, fileSize, fetchedAt}>
│   └── eventBridge
│       ├── submit({chatId, messageId, mediaType})
│       ├── submitBatch([...])
│       ├── queryDownloaded({chatId, messageId})
│       └── action({taskId, action})
└── (DOM 渲染按钮堆栈,绝对定位)
       │
       │ invoke('submit_*' | 'webview_*')
       ▼
Tauri commands.rs → task_store / downloader
       │
       │ emit('webview-task-*')
       ▼
state.js → listen() → 更新按钮状态
```

## 6. 组件设计

### 6.1 observer.js

单一 `MutationObserver`,挂在 `document` 上:

- `subtree: true, childList: true`
- `attributes: true, attributeFilter: ['data-mid', 'data-peer-id', 'data-protected', 'class', 'aria-disabled']`

回调入 `records` 数组,50ms 防抖后批处理:

```
records.flatMap(r => collectAddedElements(r))
       .filter(uniqueMessageElements)
       .forEach(detectAndAttach)
```

`collectAddedElements` 处理三种情况:`childList` 的 `addedNodes`、属性变更节点自身、属性变更节点的最近消息祖先。

### 6.2 media.js

`detectMessage(messageEl)` 返回 `null | MediaDetection`:

```ts
type MediaType =
  | 'photo' | 'video' | 'audio' | 'voice'
  | 'sticker' | 'animation' | 'document' | 'story';

interface MediaItem {
  type: MediaType;
  // 仅内部 element 引用,不传出
}

interface MediaDetection {
  chatId: string;
  messageId: number;
  protected: boolean;
  media: MediaItem[];
}
```

识别规则(按优先级,先匹配先返回):

| 类型 | 命中条件 |
|---|---|
| `story` | 元素在 `[data-story-viewer]` / `.StoryViewer` 子树 |
| `photo` | `IMG`,非 `avatar` class,`getBoundingClientRect().width > 100` |
| `video` | `VIDEO`,非 `[loop][muted][autoplay]`(否则降级 animation) |
| `animation` | `VIDEO[loop][muted][autoplay]`,或 `<video class*="animation">` |
| `voice` | `AUDIO` 且 class 含 `voice-note` / `bubble-audio`,或最近祖先含 `voice` |
| `audio` | `AUDIO`(非 voice) |
| `sticker` | class 含 `sticker`,或最近祖先 `[data-sticker]` |
| `document` | `[data-media-type="document"]`,或 `.document-icon`,或 `<a href*="api.tlgr" download>`(仅读 tagName + class,不读 href) |

`protected`:自身或 5 层祖先内含 `data-protected` 属性、`aria-disabled="true"`,或 class 含 `protected`(大小写不敏感)。

多文件消息:同一 message 节点下识别多个不同元素,顺序按 DOM 顺序。

### 6.3 button.js

`attachButtons(messageEl, detection)`:

- 主按钮:浮在消息节点右上角(`position: absolute; top: 8px; right: 8px; z-index: 2`)
- 子按钮:每个媒体对应一个,浮在对应媒体元素右上角
- 主按钮显示当前媒体类型图标 + 数字徽标(若 N > 1)

DOM 结构:

```html
<div class="tmd-stack" data-tmd-chat-id={chatId} data-tmd-message-id={messageId}>
  <button class="tmd-btn tmd-primary" data-tmd-state="ready" aria-label="下载此消息的媒体">
    <svg class="tmd-icon">{primaryIcon}</svg>
    <span class="tmd-count" hidden>{N}</span>
  </button>
  <button class="tmd-btn tmd-file" data-tmd-media-index="0" data-tmd-state="ready" aria-label="下载此文件">
    <svg class="tmd-icon">{fileIcon}</svg>
  </button>
  ...
</div>
```

状态机(主按钮聚合状态,子按钮独立):

```
            submit           submit success
   ready ───────────► submitting ──────────► queued
    ▲                                           │
    │ retry                                     │ progress event
    │                                           ▼
   failed ◄──────────── failed event ◄── downloading
    │                                           │
    │                                           │ complete event
    └─────────────────────────────────────────► completed (✓)
```

视觉:

- `ready`:实色背景(#3390ec),白色图标
- `submitting`:同 ready,opacity 0.7,内嵌 spinner SVG
- `queued`:同 ready,文字 "已加入"
- `downloading`:蓝底 + 内嵌 progress bar
- `completed`:绿底(#4dcd5e)+ ✓
- `failed`:红底(#e53935)+ 重试图标

子按钮在 `downloading` 时同步显示该文件进度。

### 6.4 state.js

`bindEvents()` 启动后调用一次,订阅 Tauri 事件:

| 事件 | payload | 处理 |
|---|---|---|
| `webview-task-submitted` | `{taskId, chatId, messageId, mediaIndex?}` | 找到对应按钮,状态 `submitting` → `queued` |
| `webview-task-updated` | `{taskId, progress}` | `downloading` + 显示百分比 |
| `webview-task-completed` | `{taskId, outputPath}` | `completed` + ✓ |
| `webview-task-failed` | `{taskId, error}` | `failed` + 错误提示 |

内部 `taskToButtonIndex = Map<taskId, {chatId, messageId, mediaIndex}>`,由 submit 阶段填,事件回调时查。

按钮 ↔ 任务映射:`chatId + messageId + mediaIndex` 唯一确定。
- `mediaIndex` = 子按钮序号(0..N-1)
- 主按钮对应所有子按钮的聚合状态,无需单独 taskId

### 6.5 dedupe.js

`isAlreadyDownloaded(chatId, messageId)`:

- 查 `dedupeCache: Map<chatKey, {downloaded, fileSize, fetchedAt}>`(chatKey = `${chatId}:${messageId}`)
- 命中且 `now - fetchedAt < TTL`(默认 5s):返回缓存
- 未命中或过期:调 `invoke('webview_query_downloaded', {chatId, messageId})`
- 写入缓存,返回结果

按钮初始渲染 `ready` 状态;`isAlreadyDownloaded` 返回 true 时改 `ready (downloaded)`,显示 ✓ + 文件大小(如 "✓ 3.4 MB")。**多文件消息**:只要任一子文件已 completed 即视为"已下载"——显示最近一次完成的 file_size。点击仍可强制重下,走 `submit_batch_download_from_webview`,由 `duplicatePolicy`(`skip`/`rename`/`overwrite`)决定落地行为。

### 6.6 story.js

独立 observer,启动时机:`document.body` 出现 `[data-story-viewer]` 或 `.StoryViewer` 子树时。

- `attachStoryButtons(storyRoot)` 同消息处理,但 chatId 用特殊标记 `-story`
- `cleanupStory()`:`storyRoot` 从 DOM 移除时清按钮

Story 提交走普通 `submit_download_from_webview`,`mediaType='story'`,Rust 侧 `create_message_task` 接受并交给 MTProto(`message_by_id` 拉 Story)。

### 6.7 icons.js

9 个内联 SVG 字符串,每个 ~150-250 字节:

| 名称 | 用途 |
|---|---|
| `download` | ready 状态 |
| `spinner` | submitting |
| `progress` | downloading |
| `check` | completed / downloaded |
| `retry` | failed |
| `image` | photo |
| `film` | video / animation |
| `music` | audio / voice |
| `sticker` | sticker |
| `file` | document |
| `story` | story |

## 7. Rust 侧改动

### 7.1 commands.rs 新增

```rust
#[tauri::command(rename_all = "camelCase")]
pub async fn submit_batch_download_from_webview(
    webview: Webview<Wry>,
    state: State<'_, AppState>,
    requests: Vec<WebviewDownloadRequest>,
) -> Result<Vec<TaskRecord>, String> {
    if !webview_bridge::is_trusted_telegram_webview(&webview) {
        return Err("批量请求必须来自受信任的 Telegram WebView 页面".into());
    }
    if requests.is_empty() || requests.len() > 32 {
        return Err("批量请求数必须在 1-32 之间".into());
    }
    let mut out = Vec::with_capacity(requests.len());
    for req in requests {
        let chat_id = validate_chat_id(&req.chat_id)?;
        validate_message_id(req.message_id)?;
        validate_media_type(&req.media_type)?;
        let task = create_message_task(&state, &chat_id, req.message_id, &req.media_type).await?;
        out.push(task);
    }
    Ok(out)
}

#[tauri::command(rename_all = "camelCase")]
pub async fn webview_query_downloaded(
    state: State<'_, AppState>,
    chat_id: String,
    message_id: i64,
) -> Result<Option<DownloadedMatch>, String> {
    let chat_id = validate_chat_id(&chat_id)?;
    validate_message_id(message_id)?;
    state.shared.store
        .find_completed_for_dedupe(&chat_id, message_id)
        .await
        .map_err(command_error)
}

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
    if !["cancel", "retry", "open"].contains(&action.as_str()) {
        return Err("不支持的 webview 任务操作".into());
    }
    state.downloads.action(&task_id, &action).await.map_err(command_error)
}
```

### 7.2 数据模型

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DownloadedMatch {
    task_id: String,
    file_size: Option<u64>,
    completed_at: Option<String>, // RFC3339
    output_path: Option<String>,
}
```

`WebviewDownloadRequest`(已有):`{chat_id: String, message_id: i64, media_type: String}`

### 7.3 task_store.rs 新增查询

```rust
pub async fn find_completed_for_dedupe(
    &self,
    chat_id: &str,
    message_id: i64,
) -> Result<Option<DownloadedMatch>> {
    // SELECT task_id, file_size, completed_at, output_path
    //   FROM tasks
    //  WHERE chat_id = ? AND message_id = ? AND status = 'completed'
    //  ORDER BY completed_at DESC LIMIT 1
}
```

### 7.4 事件协议升级

替换 `webview-download-submitted`(单条)为统一事件命名:

| 旧 | 新 |
|---|---|
| `webview-download-submitted` (single, payload = TaskRecord) | `webview-task-submitted` (payload = `{taskId, chatId, messageId, mediaIndex?}`) |
| — | `webview-task-updated` (payload = `{taskId, progress}`) |
| — | `webview-task-completed` (payload = `{taskId, outputPath}`) |
| — | `webview-task-failed` (payload = `{taskId, error}`) |

事件发出点:`downloader.rs` 中任务状态变更处。`mediaIndex` 由 `create_message_task` 在批量创建时填。

### 7.5 build.rs

```rust
fn main() {
    tauri_build::AppManifest::new()
        .commands(&[
            "submit_download_from_webview",
            "submit_batch_download_from_webview",
            "webview_query_downloaded",
            "webview_task_action",
        ])
        .build();

    let dist = Path::new("webview-inject/dist/inject.js");
    let src_dir = Path::new("webview-inject/src");
    let needs_build = if !dist.exists() { true }
        else {
            let dist_mtime = fs::metadata(dist).and_then(|m| m.modified()).ok();
            fs::read_dir(src_dir).ok()
                .map(|entries| entries.flatten()
                    .filter_map(|e| e.metadata().and_then(|m| m.modified()).ok())
                    .any(|t| dist_mtime.map_or(true, |d| t > d)))
                .unwrap_or(false)
        };
    if needs_build {
        if Command::new("node").args(["webview-inject/build.mjs"]).status().is_ok() {
            println!("cargo:rerun-if-changed=webview-inject/src");
        } else {
            println!("cargo:warning=node not found or build failed; using stale inject.js");
        }
    }
}
```

## 8. 数据流

### 8.1 初次加载

1. 子 WebView 打开 `https://web.telegram.org/`
2. inject.js 启动:解析 `config`,校验 `location.origin === 'https://web.telegram.org' && window.top === window`
3. 校验失败立即返回(`return`)
4. observer 启动 MutationObserver
5. 已存在的消息:`document.querySelectorAll(MESSAGE_SELECTOR).forEach(detectAndAttach)`
6. 每条命中消息异步调 `dedupe.isAlreadyDownloaded` → 按钮初始 `ready` 或 `ready (downloaded)`

### 8.2 用户点子按钮(单文件)

1. button.js 拦截 click → `invoke('submit_download_from_webview', request)`
2. Rust:校验 webview + 参数 → `create_message_task` → 返回 TaskRecord
3. Rust:在任务创建时 `emit('webview-task-submitted', {taskId, chatId, messageId, mediaIndex})`
4. inject.js `state.js` 收到事件 → 找到子按钮 → 状态 `submitting` → `queued`
5. 下载进行中 `webview-task-updated` → 状态 `downloading` + 进度
6. 完成 `webview-task-completed` → `completed` + ✓

### 8.3 用户点主按钮(多文件)

1. button.js → `invoke('submit_batch_download_from_webview', [r1, r2, ...])`
2. Rust 循环创建 N 个任务,逐个 emit `webview-task-submitted`
3. 主按钮聚合状态:任一失败 → `failed`,全部完成 → `completed`,部分完成 → `downloading` + 数字徽标显示完成数

### 8.4 失败重试

1. 用户点 `failed` 按钮
2. button.js → `invoke('webview_task_action', {taskId, action: 'retry'})`
3. Rust → `downloader.action(taskId, 'retry')` → 任务重新入队
4. emit `webview-task-submitted`(新一轮)
5. state.js 重置按钮状态

### 8.5 Story

1. 用户打开 Story
2. story.js 检测 `[data-story-viewer]` 出现 → 调 media.js 识别媒体
3. attachStoryButtons 在 Story overlay 内渲染按钮
4. 点击 → 普通 submit 流程,mediaType='story'
5. Rust 端 `message_by_id(chat_id='-story', message_id)` 拉取 Story,MTProto 走相同路径
6. Story 关闭:observer 检测 DOM 移除 → cleanupStory 清按钮

## 9. 安全边界

注入脚本**仅读**:

- `data-mid` / `data-peer-id` / `data-protected` 属性
- `aria-disabled` 属性
- 元素 `className`(仅匹配,不上传)
- 元素 `tagName`
- `getBoundingClientRect()` + `getComputedStyle().display/visibility/opacity`

注入脚本**永不读**:

- `src` / `href` / `currentSrc` / `poster` / `srcset`
- `cookie` / `localStorage` / `sessionStorage` / `indexedDB`
- `window.__TAURI_INTERNALS__`
- 任何 clipboard / keyboard / mouse 事件(不订阅 `keydown` / `keyup` / `paste` / `copy` / `beforeunload` / `visibilitychange`)

Rust 侧命令**永不接收**:路径、URL、文件名、token、cookie、session 字符串、`__TAURI__` payload 内部字段。仅 `chat_id`(整数 ID 字符串)、`message_id`(整数)、`media_type`(白名单字符串)。

每条命令入口**重做** `is_trusted_telegram_webview` 校验。

## 10. 测试

### 10.1 Rust 单元/集成

`cargo test --locked` 新增:

- `submit_batch_download_from_webview` 接受 1-32 条;拒绝 0 / >32;非 trusted webview 拒绝
- `webview_query_downloaded` 命中已 completed、未命中返回 None;chat_id/message_id 非法返回错误
- `webview_task_action` 三种 action 路由正确;非法 action 拒绝
- 事件 payload schema:序列化后字段名 camelCase,字段类型正确

### 10.2 Init script 单元(webview-inject/test/)

vitest + happy-dom:

- fixture:HTML 字符串模拟 Telegram Web DOM(消息、群图、Story overlay 各一份)
- mock `window.__TAURI__.core.invoke` 返回预设值
- 用例:
  - observer 在 mutation 后调用 attach
  - media.js 识别 photo/video/voice/sticker/animation/document/story
  - dedupe.js 缓存命中不重复 invoke,TTL 过期重新 invoke
  - button.js 状态机正确转移(5 状态)
  - 多文件消息渲染 1 + N 按钮,主按钮徽标数字正确
  - Story 关闭清理按钮
  - origin 不匹配 / 非顶层 frame 时 boot 提前返回

### 10.3 手工(开发模式)

`npx tauri dev` 登录真实账号:

- 普通消息 / 群图 / 文档 / voice / sticker / animation / Story 各下载一次
- 重复点击强制重下
- 关闭客户端重启,已下载标记是否保留
- 主按钮 / 子按钮视觉与 Telegram Web 融合度

### 10.4 Lint

- ESLint (vanilla config) 作用于 `webview-inject/src/`
- `cargo clippy --locked --all-targets`

## 11. 配置(`config.json`,编译期内联到 `inject.js` 顶部)

```js
{
  observerDebounceMs: 50,
  observerAttributeFilter: [
    'data-mid', 'data-peer-id', 'data-protected', 'class', 'aria-disabled'
  ],
  messageSelectors: [
    '.message[data-mid]',
    '.Message[data-mid]',
    '[data-mid][data-peer-id]'
  ],
  storySelectors: [
    '[data-story-viewer]',
    '.StoryViewer'
  ],
  duplicateWindowBytes: 5120,
  dedupeCacheTtlMs: 5000,
  batchMaxSize: 32,
  buttonStackPosition: 'top-right',
  iconSize: 16,
  spinnerSize: 12,
  visibleMinWidth: 100,
  protectedAncestorDepth: 5
}
```

## 12. 风险与缓解

| 风险 | 缓解 |
|---|---|
| Telegram Web DOM 结构变化,selector 失效 | happy-dom fixture 覆盖已知 DOM 版本;真机回归;selector 集中于 `config.js`,快速调整 |
| Grammers `0.10.0` Story API 支持不全 | 实施首日先做 Story MTProto 端到端冒烟;失败则 Story 降级为"快捷入口"(打开/复制链接),不进任务系统 |
| 长聊天列表 MutationObserver 触发频繁 | 50ms 防抖 + 属性白名单 + 内部 ID 缓存避免重复处理 |
| dedupe 缓存 5s 内不感知新完成任务 | 用户重新滚动触发新 mutation 时自然更新;首次进入页面无影响 |
| 多文件识别歧义(同消息 document + img) | document 优先(体积更大,通常更值得下载);可在 `config.js` 调优先级 |
| Node 不可用导致打包失败 | build.rs 警告不阻断;dist 已存在则跳过;CI 单独验证 |

## 13. 验收标准

- [ ] `cargo test --locked` 全部通过
- [ ] `cd desktop && npm run check && npm run build` 通过
- [ ] `cd desktop/src-tauri && cargo clippy --locked --all-targets` 无 warning
- [ ] `webview-inject/test/` vitest 通过,行覆盖率 ≥ 80%
- [ ] `npx tauri dev` 真实账号登录,7 类媒体 + Story 全部能识别并入队
- [ ] 主按钮 + 子按钮视觉与 Telegram Web 融合(肉眼无明显违和)
- [ ] 状态机 5 状态正确切换(录屏或日志验证)
- [ ] 多文件消息渲染 1 + N 按钮,数字徽标正确
- [ ] 已下载消息显示 ✓ + 文件大小;点击后走 `duplicatePolicy='rename'`
- [ ] 失败按钮显示错误原因,点击走 retry
- [ ] 安全审计脚本不读 `src`/`cookie`/`localStorage`,不订阅敏感事件
- [ ] Story overlay 关闭后按钮自动清理
- [ ] 中文 UI 文案一致,每条 ≤ 8 字

## 14. 参考

- 现有 init script: `desktop/src-tauri/src/webview_bridge.rs:147`
- 现有命令: `desktop/src-tauri/src/commands.rs:1115`
- 任务系统: `desktop/src-tauri/src/downloader.rs`、`task_store.rs`
- Tauri 事件订阅: `@tauri-apps/api/event::listen`
- Greasy Fork 同类产品: [#446342](https://greasyfork.org/zh-CN/scripts/446342-telegram-media-downloader)
- 项目文档: `docs/FEATURE_MIGRATION_ZH.md`、`docs/TELEGRAM_CDN_ZH.md`