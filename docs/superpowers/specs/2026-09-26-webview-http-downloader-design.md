# WebView HTTP 下载器 — 设计规范

- 日期: 2026-09-26
- 状态: 待用户审阅
- 范围: 把桌面客户端下载核心从 MTProto 切到 Telegram Web HTTP API(基于 WebView cookie + 注入脚本送 URL)。同时精简桌面客户端,**删除**上传/转发/rclone/Bot/MTProto Session。
- 锚定参考: [Greasy Fork #446342](https://greasyfork.org/zh-CN/scripts/446342-telegram-media-downloader)(Neet-Nestor)。本 spec 的设计目标是**沿用脚本的下载机制**+ 叠加桌面客户端的任务管理、断点续传、分块并发、状态反馈。脚本有的能力本 spec 必须达到;脚本不做的能力(协议层操作、上传/转发)本 spec 不做。
- **架构分工**:浏览器侧(Telegram Web + 注入脚本)**只负责**抓文件 URL — 这部分是"参考浏览器怎么下载"的复制;下载执行、任务管理、状态反馈、断点续传、分块并发、文件校验**全部由 Rust 客户端承担**。注入脚本是桥梁,不下载;Rust 是核心,不直接接触 Telegram Web DOM。

## 1. 摘要

桌面客户端当前的下载路径走 Grammers/MTProto 直连 Telegram DC,需要用户输入 API ID/手机号/验证码才能用 — 体验和油猴脚本(浏览器已登录就能用)差距很大。本 spec 把下载核心换成:

- **注入脚本**从 Telegram Web DOM 里读媒体元素的 `src`/`currentSrc`,作为 URL 传给 Rust
- **Rust** 用 reqwest + HTTP Range 头做分块下载、断点续传、并发(沿用现有的 chunk_writer/task_store 基础设施)
- **Session** 复用 WebView2 的 Telegram Web 登录 cookie;不重新登录、不存加密 Session 文件

**关键:这是把 Greasy Fork #446342 的下载机制搬到桌面客户端的 Rust 后端**,UI 由我们的注入脚本承担,任务管理/状态反馈由我们的 Rust 调度承担,WebView 自动登录 + 持久化由 Tauri WebView2 承担。脚本本身没有队列/断点续传/分块,这些是我们叠加的能力。

同时删除:MTProto/Grammers、Telegram 上传、单条转发、rclone 云上传、Bot 能力、Python `module/bot.py` 等。桌面客户端定位回归到"Telegram Web + 本地下载管理"的聚焦产品。

## 1.5 与油猴脚本 #446342 的对标

脚本做的事:
- 浏览器里检测 Telegram Web 消息 DOM 中的媒体元素(img / video / audio / animation / sticker / voice / Story)
- 给每条媒体加"下载"按钮
- 点击触发浏览器下载(`<a download>` 或 `fetch` + Blob)
- 仅在浏览器已登录时工作,无独立鉴权
- 安装后用户零成本使用

本 spec 与脚本的差异:
- 下载执行位置:脚本在浏览器(用户机器),本 spec 在 Rust 后端(同台机器,独立进程)
- 任务管理:脚本没有,本 spec 加队列/进度/暂停/重试/历史
- 并发/续传/分块:脚本没有,本 spec 加
- 状态反馈:脚本只有浏览器原生进度条,本 spec 加 5 状态按钮(就绪/提交/排队/下载/完成/失败)
- 已下载标记:脚本没有,本 spec 加 ✓
- Story 支持:脚本支持,本 spec 支持(对齐)
- 多文件消息:脚本支持(部分),本 spec 支持主 + 子按钮(更明确)

**本 spec 不做的(脚本也不做)**:Telegram 上传、消息转发、rclone 云上传、Bot 能力、跨设备同步、多账号。

## 2. 现状摘要

- `desktop/src-tauri/src/telegram.rs`(20KB):MTProto 登录、消息读取
- `desktop/src-tauri/src/secure_session.rs`:Grammers Session 加密存储
- `desktop/src-tauri/src/credentials.rs`:Windows 凭据管理器
- `desktop/src-tauri/src/downloader.rs`(64KB):分块并发 + BLAKE3 校验 + 原子提交 + 进度事件
- `desktop/src-tauri/src/task_store.rs`(36KB):SeaORM/SQLite 任务持久化
- `desktop/src-tauri/src/chunk_writer.rs`(11KB):乱序写入、缺块检测
- `desktop/src-tauri/src/atomic_file.rs`:临时文件原子提交
- `desktop/src-tauri/src/transfers.rs`(34KB):上传/转发调度
- `desktop/src-tauri/src/cloud_upload.rs`(22KB):rclone 集成
- `desktop/src-tauri/webview-inject/`:Web 注入脚本(scaffolding + icons.js 已落地)
- `Cargo.toml`:Grammers 0.10.0、chacha20poly1305、keyring、sea-orm、sqlx 等

## 3. 设计目标

1. 用户零登录成本(只用 WebView 里手动登 Telegram Web)
2. 下载性能持平或超过原 MTProto 路径(分块并发 + 断点续传 + 校验全部保留)
3. HTTP API 支持禁保存媒体的下载(脚本 #446342 也支持,本 spec 对齐)
4. 自动支持 CDN 大文件(HTTP API 透明处理 redirect)
5. Rust 端不再依赖 Grammers、二进制 Session、凭据管理器

## 4. 非目标

- Telegram 上传(发文档、发到频道) — 删
- 单条消息转发 — 删
- rclone 云盘上传 — 删
- Bot 能力(`bot.py` / `bot_token` / 白名单)— 删
- 旧 Python 模块(`module/`、`utils/` 中非必要部分)— 删
- MTProto Session 持久化 — 删
- 设置页"输入 API ID/Hash + 手机号登录"流程 — 删

## 5. 架构

### 5.1 模块布局

```
desktop/src-tauri/src/
├── commands.rs           重构:删除所有 *_upload*/forward/bot/cloud/transfers 命令
├── http_downloader.rs    新:HttpDownloadManager + ChunkedFetcher + UrlValidator
├── task_store.rs         保留(查 + 写 task 记录)
├── chunk_writer.rs       保留(乱序写入、BLAKE3 校验、缺块检测)
├── atomic_file.rs        保留
├── webview_bridge.rs     保留(注入脚本位置不变)
├── webview-inject/
│   ├── src/
│   │   ├── inject.js          修改:点击按钮时读 src/currentSrc
│   │   ├── button.js          修改:URL 提取 + payload 扩展
│   │   └── ... (其他模块不变)
│   └── ...
├── app_state.rs          调整:删除 telegram() 字段
└── ...

Cargo.toml:
+ reqwest = { version = "0.12", default-features = false, features = ["stream", "rustls-tls"] }
- grammers-client
- grammers-session
- chacha20poly1305
- keyring
- (其他 MTProto 间接依赖)

删除整个文件:
- telegram.rs
- secure_session.rs
- transfers.rs
- cloud_upload.rs
- credentials.rs(无 MTProto 用)
```

### 5.2 数据流(端到端)

```
用户在 WebView 看消息
        ↓ 点下载按钮
inject.js button.click
        ↓ 读 mediaEl.src / currentSrc
        ↓ 调 invoke('submit_download_from_webview', { request: {chatId, messageId, mediaType, mediaUrl, fileName, fileSize} })
        ▼
Rust commands.rs
        ↓ validate URL (https + 域名白名单 + 大小写敏感)
        ↓ create_http_task(...)
        ▼
http_downloader.rs::HttpDownloadManager
        ↓ 入队 + 创建 task_store 记录
        ↓ emit('webview-task-submitted', {taskId, chatId, messageId, mediaIndex})
        ▼
ChunkedFetcher
        ↓ HEAD 检查 URL 有效性 + 拿 Content-Length
        ↓ 范围:offset=0..total, 步长 = chunk_size
        ↓ 派 N 个并发 worker (N = perFileChunks)
        ▼
reqwest::Client.get(url).header(Range, "bytes=offset-end")
        ↓ 流式读取 → chunk_writer::write_chunks_bounded
        ▼
chunk_writer 乱序写入临时文件 + BLAKE3 校验每块
        ▼
所有块完成 → atomic_file::commit(temp, output)
        ↓
emit('webview-task-completed', {taskId, outputPath})
```

### 5.3 组件

#### 5.3.1 `http_downloader::HttpDownloadManager`

沿用现有 `DownloadManager` 的并发模型(任务级 + 文件级 + 分块级三档并发)。删除所有 MTProto 引用,改为持有 `reqwest::Client` + `TaskStore` + 设置状态。

#### 5.3.2 `http_downloader::ChunkedFetcher`

- `async fn fetch_chunk(url, offset, length, token) -> Result<Vec<u8>>`
- HEAD 一次拿 `Content-Length`(失败 → bail "无法获取文件大小")
- Range 请求 + 流式读取 → `Vec<u8>`
- 重试 + 超时沿用现有 `RequestLimiter` / `BandwidthLimiter` / `FloodGate`(后者无 FloodWait,可降级为通用 backoff)
- 错误码映射:
  - 200(忽略 Range,服务器不支持)— 退化为单流下载整文件
  - 206 Partial Content — 正常分块
  - 401/403 → "URL 已过期或无权限,请重新打开 Telegram Web 触发该消息后再试"
  - 404/410 → "文件已删除或 URL 失效"
  - 429/5xx → 重试,指数退避
  - 网络错误 → 重试

#### 5.3.3 `http_downloader::UrlValidator`

- 协议必须是 `https`
- 域名在白名单(从设置读取,默认 `["web.telegram.org", "*.cdn-telegram.org", "*.telegram.org"]`)
- 拒绝 `localhost` / `127.0.0.1` / 私有 IP(防 SSRF,即使来自 WebView 也兜底)
- 拒绝非 GET-able URL(不允许 `file:` / `data:` / `blob:` 等)

#### 5.3.4 复用现有基础设施

- `chunk_writer.rs`:不动,直接接 HTTP 流式数据
- `task_store.rs`:不动,只换 Rust 字段(file_size 来源从 MTProto media.size() 改为 Content-Length)
- `atomic_file.rs`:不动
- `app_state.rs`:删除 `telegram: Arc<TelegramAdapter>` 字段,替换为 `http_downloader: Arc<HttpDownloadManager>`

## 6. 接口契约

### 6.1 注入脚本 → Rust

```typescript
interface SubmitDownloadRequest {
  chatId: string | number;        // 现有,DOM data-peer-id
  messageId: number;              // 现有,DOM data-mid
  mediaType: 'photo' | 'video' | 'audio' | 'voice' | 'sticker' | 'animation' | 'document' | 'story';
  mediaUrl: string;               // 新增,从 mediaEl.src / currentSrc 读取
  fileName?: string | null;       // 新增,可选,优先 <a download> 属性 / data-name
  fileSize?: number | null;       // 新增,可选,HEAD 请求之前由 Rust 校验
}

invoke('submit_download_from_webview', { request: SubmitDownloadRequest });
```

### 6.2 Rust 命令

`submit_download_from_webview` / `submit_batch_download_from_webview` / `webview_task_action` / `webview_query_downloaded` 全部保留(命名不变),payload 一律增 `mediaUrl` / `fileName` / `fileSize` 字段。

事件 schema 不变(`webview-task-submitted` / `updated` / `completed` / `failed`)。

### 6.3 删除的命令

以下 Tauri 命令整条删除 + 从 `tauri::generate_handler!` 移除 + 从 `build.rs` AppManifest 移除:

- `upload_completed_download`
- `forward_telegram_message`
- `list_telegram_transfers`
- `telegram_transfer_action`
- `queue_cloud_upload`
- `list_cloud_uploads`
- `cloud_upload_action`
- `login_request_code` / `login_submit_code` / `login_submit_password` / `logout_session`
- `telegram()` / 任何 `state.telegram()` 调用

## 7. 安全边界(注入脚本)

| 操作 | 现状 | 本 spec |
|---|---|---|
| `data-mid` / `data-peer-id` / `data-protected` | ✅ 读 | ✅ 读(保留)|
| `aria-disabled` | ✅ 读 | ✅ 读(保留)|
| `tagName` / `className` | ✅ 读 | ✅ 读(保留)|
| `getBoundingClientRect` / `getComputedStyle` | ✅ 读 | ✅ 读(保留)|
| **媒体元素 `src` / `currentSrc`** | ❌ **永不许** | ✅ **允许**(限 `<video>`/`<audio>`/`<img>`/`<a download>`) |
| `<a>` 元素的 `download` / `href` | ❌ 永不许 | ⚠️ **仅读 `download` 属性**(用于 fileName);`href` 永不许 |
| `cookie` / `localStorage` / `sessionStorage` / `indexedDB` | ❌ 永不许 | ❌ 永不许(不变)|
| `window.__TAURI_INTERNALS__` | ❌ 永不许 | ❌ 永不许(不变)|
| `keydown` / `paste` / `copy` / `beforeunload` | ❌ 不订阅 | ❌ 不订阅(不变)|
| `window.__TAURI__.event` payload 内部字段 | ❌ 不读 | ❌ 不读(不变)|

**放宽理由**:Telegram Web 文件 URL 是**签名 + 短期有效**的,把 URL 传给 Rust 不会泄露 cookie/session。URL 在 `UrlValidator` 里再过白名单 + 协议检查二次保险。

Rust 侧 `is_trusted_telegram_webview` 校验保持(同源 + label 校验),不读 WebView cookie 也能下载,无须新增 cookie 读取 API。

## 8. URL 过期与错误处理

### 8.1 URL 有效期

Telegram Web 文件 URL 通常 1 小时内有效(基于内部签名 + token)。本 spec **不**实现主动续期(避免请求风暴)。

### 8.2 错误码与用户提示

| HTTP 状态 | 用户提示 | 后端动作 |
|---|---|---|
| 200(忽略 Range) | (退化处理,正常)| Range 不支持,改整文件 GET |
| 206 | (正常)| 写入对应分块 |
| 301/302 | (正常,reqwest 自动跟随)| 跟随 |
| 401 / 403 | "URL 已过期或无权限,请重新打开 Telegram Web 触发该消息后再试" | 任务失败 |
| 404 | "文件已删除或 URL 失效" | 任务失败 |
| 410 | "文件已过期" | 任务失败 |
| 416(范围越界)| (正常,文件已读完)| 跳过,正常完成 |
| 429 | (重试)| backoff 后重试,最多 N 次 |
| 5xx | (重试)| backoff 后重试 |
| 网络错误 / 连接重置 | (重试)| backoff 后重试 |

### 8.3 重试策略

- 单分块最多重试 3 次,指数 backoff(200ms / 1s / 3s)
- 整个任务不重试(用户手动)
- 不做 FloodWait 等待(Telegram Web HTTP API 不返回 FloodWait)

## 9. 数据模型

`task_store.rs` 的 `tasks` 表 `file_size` 字段在创建时**未**知(URL HEAD 未发),下载过程中由 `Content-Length` 头填入并 update 任务记录。`started_at` 用下载开始时间,`completed_at` 用原子提交成功时间。

新增 `media_url: String` 字段存任务的源 URL(用于排查 + 重新触发)。`media_token` / `cdn_dc_id` 等 MTProto 字段删除。

## 10. 设置页

- 删除"输入 API ID/Hash"表单
- 删除"登录"按钮
- 删除"上传"页
- 删除"云盘上传"页
- 删除"代理"设置(MTProto 专属)
- **保留**:下载目录、文件名模板、媒体过滤、并发/分块/超时/重试、限速、WebView2 profile、设置导入(部分字段)
- **新增**:HTTP 域名白名单(高级设置,默认隐藏;展开后可见白名单列表,可编辑)
- **新增**:WebView2 数据根(已有,保留)

## 11. 测试

### 11.1 Rust 单元/集成

`cargo test --locked`:
- `UrlValidator` 各组合(host 白名单 / 协议 / 私有 IP / 拒绝列表)
- `ChunkedFetcher` 用 mock HTTP server(用 `axum` 或 `wiremock`)模拟:
  - 200 + Content-Length 正常分块
  - 206 Partial Content
  - Range 不支持退化
  - 401 / 404 / 410 错误码
  - 429 + 重试耗尽
  - 网络错误 + 重试
- `HttpDownloadManager` 调度:并发上限、取消、续传命中
- 命令校验:URL 非白名单 → 拒;chat_id/message_id 非法 → 拒

### 11.2 Init script 单元

- `media.js`:从 `src` 读取后送 invoke,payload 含 `mediaUrl`
- `button.js`:URL 提取逻辑(空 src / blob: / data: / cdn 域名各种)
- 边界:src 为空、blob URL、无 src 属性 → button 拒绝点击并提示

### 11.3 手工端到端

`npx tauri dev`:
1. WebView 中登录 Telegram Web(扫描二维码)
2. 普通消息点下载,正常完成
3. 禁保存消息点下载,正常完成
4. **大文件**(>10MB)点下载,正常完成(关键验证 — HTTP API 透明处理 CDN)
5. 杀掉客户端,重启,未完成任务自动从断点恢复
6. URL 过期(等 1 小时后)再次点击,失败提示明确

## 12. 风险与缓解

| 风险 | 缓解 |
|---|---|
| Telegram Web 文件 URL 过期(用户停留后回来点) | 错误信息明确指向"重新打开该消息";不自动重试 |
| Range 请求不被服务端支持 | 退化到整文件 GET(走 `accept-ranges: none` 检测) |
| 域名白名单过严,真实 URL 被拒 | 默认白名单含 `web.telegram.org` + `*.cdn-telegram.org` + `*.telegram.org`;可由用户编辑 |
| 域名白名单过松,被 SSRF 利用 | 同时拒绝私有 IP(127.0.0.1, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, ::1) |
| reqwest 间接依赖把包大小撑大 | 用 `default-features = false, features = ["stream", "rustls-tls"]`,禁用 native-tls |
| WebView 关闭后 cookie 失效 | WebView2 profile 持久化,与现有行为一致;不增加复杂度 |
| CDN 大文件 Range 不支持 | 单流下载整文件,自动原子提交;并发模型仍生效(多文件并行)|

## 13. 验收标准

- [ ] `cargo test --locked` 全部通过(含新增 HTTP 下载器测试)
- [ ] `cargo clippy --locked --all-targets` 无 warning
- [ ] `desktop/src/lib/api.ts` 的 TS 类型与命令签名一致
- [ ] `npm run build` 通过
- [ ] `npx tauri build` 产出 MSI/NSIS
- [ ] 真实账号端到端:
  - 普通消息下载 ✓
  - 禁保存消息下载 ✓(HTTP API 直接支持)
  - **大文件(>10MB)下载 ✓**(CDN 透明处理)
  - 杀掉客户端重启,断点续传 ✓
  - WebView 关闭后再打开,WebView2 profile 保留登录 ✓
- [ ] Rust 端零 Grammers 依赖(`grep grammers Cargo.lock` 返回空)
- [ ] Rust 端无 chacha20poly1305 / keyring / sea-orm 上传字段
- [ ] 桌面客户端所有 Tauri 命令无 `*_upload*` / `*_forward*` / `*_bot*` / `*_cloud*` / `*_transfer*`

## 14. 后续

- CDN:HTTP API 透明处理,本 spec 不需要单独 CDN 工作(已被解决)
- 多账号:Telegram Web 不支持(浏览器一样),不做
- 跨设备同步:不做
- 性能基准:跑通后另起 spec

## 15. 参考

- 现有 MTProto 路径下载核心:`desktop/src-tauri/src/downloader.rs:680-760`
- 现有事件协议:已通过 inject plan Task 13 升级到 webview-task-*
- 现有 chunk_writer:`desktop/src-tauri/src/chunk_writer.rs`
- Telegram CDN 协议说明(本次不实现,HTTP API 已透明):`docs/TELEGRAM_CDN_ZH.md`
- inject plan:本次 spec 完成后,**修订** `docs/superpowers/plans/2026-09-26-telegram-webview-inject-enhancement.md` 的 Task 11/12/13/16 与 Rust 端命令