# Telegram WebView 下载管线 — 设计规范

- 日期: 2026-09-26(2026-09-26 修订:由"Rust reqwest 拉取"改为"**页面 fetch + IPC 分块接收**",与验证脚本 #446342 的下载方式一致)
- 状态: 待实现
- 上游: `docs/superpowers/specs/2026-09-26-telegram-webview-inject-enhancement-design.md`(注入脚本;按钮点击后进入本管线)

## 0. 修订要点

第一版把下载执行放在 Rust(reqwest + Range)。**已修正**:下载的网络请求必须**在页面上下文里做**(和油猴脚本 #446342 完全一致的方式):

- 页面内 `fetch(url, {headers:{Range}})` 拿分块 — cookie/session/Referer 自然带上,和脚本行为逐字节对齐
- 分块经 Tauri IPC 交给 Rust — **Rust 只做管理**:落盘、任务库、进度、暂停/取消、原子提交、断点续传记账
- **URL 永不离开页面**:Rust 端收不到 URL,只收字节。reqwest 依赖撤销,URL 白名单/SSRF 问题随之消失

## 1. 分工

| 环节 | 执行者 | 说明 |
|---|---|---|
| 探测文件大小 | 页面 JS | `fetch` Range 0-0 → 解析 `Content-Range` 的 total(脚本同款) |
| 分块抓取 | 页面 JS | `fetch` Range,可并发 N 路;超时/重试/中止都在 JS |
| 分块落盘 | Rust | 复用 `chunk_writer`(乱序写入、缺块检测、BLAKE3 校验) |
| 任务登记/状态 | Rust | 现有 `task_store`(SQLite) |
| 进度/速度/事件 | Rust | 现有 `task-updated` / `stats-updated`,另发 `webview-task-*` 给按钮 |
| 暂停/取消/重试 | Rust 决策,JS 执行中止 | Rust 发 `webview-download-abort` 事件,JS abort 掉 fetch |
| 断点续传 | 双端协作 | Rust 记哪些块已完成;重开媒体后 JS 拿新 URL,只补缺块 |
| 原子提交 | Rust | 现有 `atomic_file` + 重复策略检查 |

## 2. 端到端数据流

```
用户点下载按钮(注入脚本,已有)
   │  JS 侧 downloader.js:
   ├─ 1. invoke('start_webview_download', {fileName, fileType, source})
   │      ← {taskId}(Rust: 建记录、算输出路径、重复策略、发 webview-task-submitted)
   ├─ 2. 探测:fetch(url, Range: "bytes=0-0") → 从 Content-Range 得 totalBytes
   ├─ 3. invoke('plan_chunks', {taskId, totalBytes})
   │      ← {chunkSizeBytes, concurrency, timeoutSeconds, retries, missing:[{offset,length}...]}
   │        (Rust: 配置分块表、校验本地已有块、占一个文件槽位、启动 writer、发 webview-task-updated)
   ├─ 4. 对每个 missing 分块(并发 ≤ concurrency):
   │      fetch(url, Range: "bytes=o-(o+len-1)") → 重试/超时/abort
   │      → invoke('push_chunk', {taskId, offset}, rawBytes)   ← 二进制 IPC
   │        (Rust: mpsc 入队 → chunk_writer 落盘 → 节流发进度事件;mpsc 满则背压)
   ├─ 5. 全部完成 → invoke('finish_download', {taskId})
   │      ← TaskRecord(Rust: 校验分块完整 → 原子提交 → webview-task-completed)
   └─ 失败 → invoke('fail_download', {taskId, error})
          (Rust: 保留部分块,标 failed,发 webview-task-failed)

暂停/取消:Rust task_action → 发 `webview-download-abort {taskId}` → JS abort → JS 调
   fail_download(取消时 Rust 端按 preserve_partial_files 清理临时块)
```

## 3. Tauri 命令(新增)

| 命令 | 入参 | 返回 | 说明 |
|---|---|---|---|
| `start_webview_download` | `{fileName, fileType, source}` | `{taskId}` | 校验文件名(≤512 字符、无控制字符);重复策略 skip 时直接报"已存在";建记录(queued) |
| `plan_chunks` | `{taskId, totalBytes}` | `{chunkSizeBytes, concurrency, timeoutSeconds, retries, missing}` | 幂等:可重复调用(续传重入)。与已存 total 不符 → 报错停止;占文件槽位;启动 writer |
| `push_chunk` | `taskId` + `offset`(路径参数或 header)+ **raw bytes**(body) | `()` | 二进制 IPC(`tauri::ipc::Request` 的 raw body);实现若受阻可退化为 base64 参数并在报告中注明吞吐 |
| `finish_download` | `{taskId}` | `TaskRecord` | 校验全块 → 原子提交 |
| `fail_download` | `{taskId, error}` | `TaskRecord` | 保留已校验块;标 failed |

保留:`webview_query_task_state`(查看器打开时查状态,含**任务在进行中**时按钮直接显示进度)、`webview_task_action`、`webview_task_action` 的既有语义。

新增事件:`webview-download-abort {taskId}`(Rust → JS,暂停/取消信号)。

**`webview_query_task_state` 扩展**:状态为 `queued|downloading|paused` 时额外返回 `{resumable: true}` — JS 据此走 `plan_chunks` 续传路径(只补缺块)而不是重新开始。

## 4. Rust 侧设计(重写 `downloader.rs` 为 WebView 下载管理器)

旧的 MTProto 下载机制(dispatch/TaskControl/RequestLimiter/FloodGate/BandwidthLimiter/`fetch_chunk`)全部删除 — 它们服务于 Rust 主动拉取,本设计不再适用。新的职责:

```
WebviewDownloadManager
├── slots: Semaphore(max_files)           // plan_chunks 时获取,finish/fail/取消时释放
├── channels: Map<taskId, mpsc::Sender<IncomingChunk>>   // push_chunk 的入口
├── writers: Map<taskId, JoinHandle>      // chunk_writer::write_chunks_bounded 任务
├── last_activity: Map<taskId, Instant>   // 看门狗用
└── watchdog 定时器(10s 一跳):
      status=downloading 且 30s 无 push/finish → 标 paused + 发 webview-download-abort
      (覆盖 WebView 关闭/刷新/网络掉线等 JS 侧消失的情形)
```

- `push_chunk` → `channels[taskId].send(IncomingChunk)`(await — mpsc 缓冲满时自然背压)
- writer 完成(全部块齐)→ `finish_download` 触发 join + 校验 + 提交;**JS 未调 finish 但块已齐** → 看门狗 30s 后也走提交校验(兜底)
- 进度事件:writer 的 progress broadcast → 350ms 节流 → `task-updated` + `webview-task-updated`
- 复用不动:`chunk_writer.rs`、`atomic_file.rs`、`task_store.rs`、事件/统计
- 魔法数字全部命名:`SLOT_*`、`PROGRESS_TICK`、`WATCHDOG_*`、`ACTIVITY_TIMEOUT` 常量或进 `Settings`

## 5. JS 侧设计(新增 `downloader.js`)

职责(全部在页面上下文):

- `probeTotal(url, timeout)` — Range 0-0 → 解析 `Content-Range: bytes 0-0/TOTAL`(脚本同款;无 Content-Range 时回退 `Content-Length`)
- `fetchChunk(url, offset, length, {timeout, signal})` → ArrayBuffer(状态 206/200;416 视作已到尾部)
- `runDownload({url, taskId, plan})` — 并发闸 `concurrency`;每块重试 `retries` 次(指数退避);`AbortController` 响应 `webview-download-abort`;`max_bandwidth_kib > 0` 时对分块**启动**限速(粗粒度近似,注释说明)
- 全部分块 push 完 → `finish_download`;任何块重试耗尽 → `fail_download`
- 408/401/403 → 直接 fail("URL 过期,请重新打开该媒体")

`watcher.js` 点击流程改为:start → probe → plan → runDownload(→ finish/fail);按钮状态继续由 `webview-task-*` 事件驱动,不变。

## 6. 安全边界

- **URL 永不传给 Rust**(比第一版更强):Rust 只接收 `{fileName, fileType, source}` 与分块字节
- 注入脚本原有约束不变:不读 cookie/storage;JSON 尾部解析文件名;仅查看器内元素
- `start_webview_download` 的文件名做过长/控制字符校验(防路径注入);输出路径 = `download_root` + 净化文件名,不接受页面传路径
- IPC 体积上限:单块 ≤ 1 MiB(chunk_size 配置上界),Rust 侧校验 `offset+bytes.len()` 不越界

## 7. 错误与恢复

| 情形 | 行为 |
|---|---|
| URL 过期(401/403/404) | JS fail_download;错误文案"重新打开该媒体后再试";已下分块保留 |
| WebView 关闭/刷新 | 看门狗 30s 无活动 → 标 paused;重开媒体 → 查状态 `resumable` → plan_chunks 只补缺块 |
| 页面网络抖动 | JS 块级重试(指数退避),耗尽才 fail |
| 用户暂停/取消 | Rust 发 abort → JS 停止 → Rust 按设置保留/清理临时块 |
| 分块哈希不符(本地校验) | chunk_writer 现有逻辑丢弃该块并计入 missing,JS 侧下次 plan 重取 |
| 总量变化(服务器换了文件) | plan_chunks 校验 total 与已存不符 → 报错停止,保护已有分块 |

## 8. 数据模型

- `tasks` 表沿用;新增迁移 **删除 `media_url` 列**(000004 加的,本设计不再存 URL — 一行迁移 + entities 同步)
- `file_name` 为续传/去重键(现有 `find_latest_by_file_name`)
- 分块表沿用 `chunk_writer` 的既有结构

## 9. 设置页(客户端可用性硬约束)

**必须保留且可用**:

- 下载任务页:队列、进度、速度、剩余、暂停/继续/取消/重试、历史、打开文件位置
- 设置:下载目录(含"选择目录"与存储迁移)、文件名净化、重复策略(skip/rename/overwrite)、并发数(`max_files`、`per_file_chunks`)、分块大小(64-1024 KiB)、请求超时、重试、限速(JS 近似)、WebView2 profile 路径

并发/分块设置现在作用于 **JS 抓取**(plan_chunks 返回给 JS),超时/重试同理 — 设置改动即时生效(下次 plan)。

## 10. 验证

### 10.1 Rust 侧(cargo test,非 DOM 模拟)

- `start_webview_download`:文件名校验、重复策略 skip 报错、任务记录字段
- `plan_chunks`:幂等;total 不符报错;并发槽位等待;missing 列表正确(含"块已在本地"跳过)
- `push_chunk`:乱序写入正确、越界拒绝、mpsc 背压生效
- `finish_download`:缺块拒绝;齐块原子提交;重复 finish 幂等
- 看门狗:无活动超时 → paused + abort 事件
- `find_latest_by_file_name`(已有)

### 10.2 真机端到端(主要验证)

1. 图片/视频/GIF/语音/Story/置顶音频 → 按钮 → 页面抓取 → Rust 落盘 → 文件可打开
2. 大文件(>50MB):IPC 吞吐、进度、背压(观察是否卡 UI)
3. 下载中杀掉客户端重开 → 重开媒体 → 只补缺块(观察日志/进度起点)
4. 下载中关闭 WebView → 看门狗 → 任务 paused;重开后恢复
5. 暂停/取消/重试 → 行为与任务页状态一致
6. 禁保存频道、URL 过期(等 1 小时)各验证一次

## 11. 风险

| 风险 | 缓解 |
|---|---|
| **IPC 吞吐** 成为瓶颈(Tauri 2 二进制 IPC 走 WebView2 postMessage) | 分块 64KiB-1MiB 可调;必要时增大 chunk_size;实测后定默认值 |
| WebView2 回收/刷新导致 JS 下载中断 | 看门狗 + 续传;失败文案指向"重新打开媒体" |
| 页面 fetch 被 Telegram Web 的 CSP/ServiceWorker 影响 | 脚本在页面上下文 fetch 属同源正常请求(脚本 #446342 已验证可行) |
| JS 并发抓取触发 Telegram 限流 | 并发数默认保守(4);块级退避 |
| 看门狗误判慢速下载为中断 | "活动"定义 = 收到任意 push/finish;30s 阈值 + 慢速场景下进度事件也算活动 |

## 11.5 自适应并发(v0.2.2 已实现)— BBR 式投递率探测 + 乘性退避

把"单文件并发分块数"当作拥塞窗口来调,决策全部在 Rust(`src/adaptive.rs` 纯状态机,单元测试覆盖),
页面线程池只按 `webview-task-concurrency` 事件软调整宽度(缩小不打断在途分块)。参数:

- 采样周期 `T = 2s`;投递率 = 窗口内新增完成字节 ÷ 窗口时长;整窗零进展不作为样本(停滞交给看门狗)
- **提升 ≥3% → 乘性增长 ×1.25**(BBR ProbeBW 增益);已顶到 ceiling 时只允许 +1,由增益证明抬升 ceiling
- **回落 ≥20% → 乘性退避 ×0.75**,并把退避前宽度记为该任务的 ceiling(之后没有增益证明不再越过,消除锯齿)
- 平台期每 6s 探测 +1;连续 2 次探测无收益即停止探测;空闲 20s 后重新允许探测(链路容量可能已变化)
- 初始 4 路,下限 2 路,上限 = 设置页"分块并发上限"(Rust 侧封顶 16);设置页开关"自适应分块并发",默认开
- 不把丢包/重试当拥塞信号(页面侧已有逐块重试),只看速率本身 —— 与 BBR 一致
- 完成日志附"共 X MB,用时 Y 秒,平均 Z MB/s";每次并发调整也会写日志,可直接判断是否跑满链路

文献依据:

- Cardwell et al., *BBR: Congestion-Based Congestion Control*(ACM Queue 2016;现行 IETF draft-ietf-ccwg-bbr)——
  投递率窗口最大滤波 + ProbeBW 的 1.25×/0.75× 乘性探测与退避;速率建模优先于丢包信号
- Netflix, *concurrency-limits* + *Performance Under Load*(Netflix Tech Blog 2018)——
  把并发度当 TCP 拥塞窗口的 Vegas/Gradient2 自适应限流,是"并发维自适应"的工程先例
- NVIDIA NeMo Data Designer 工程笔记(AIMD 自适应并发)——
  **ceiling 稳定化**:退避后记录已探明的上限,加性增长不冲回配置上限,消除锯齿式反复撞墙
- Yildirim/Kosar et al., *Dynamically Tuning Level of Parallelism in Wide Area Data Transfers*(DADC'07)——
  "逐步加大并发、不再提升即停";Zhang et al., *Reasons Not to Parallelize TCP*(IEICE'05)——
  并行连接互相竞争,必须带上限与回退

手动固定模式仍保留:关闭开关后 `per_file_chunks` 即固定并发(旧行为),便于对照与排障。

## 11.6 自适应分块大小与全局并发预算(v0.2.3)

### BDP 分块选档

分块小于单路 BDP(带宽时延积)时,一条流会在两块之间的空档里丢掉带宽;分块过大又拖累
末段进度粒度并放大页面内存与 IPC 拷贝。做法:

- 页面在探测请求(bytes=0-0)上顺带测量首字节时间 ≈ RTT,随 `plan_chunks` 传给 Rust
- 每次下载结束,把实测"单路峰值速率"(2s 采样窗口的聚合速率 ÷ 当时并发,取峰值)
  持久化进设置(后端学习值,设置表单回传不覆盖)
- 下个任务按 `分块 = clamp(单路峰值速率 × RTT, 256 KiB, 1 MiB)`,就近对齐到支持档位
  (64/128/256/512/1024 KiB);无学习数据时回退设置值;固定并发模式不启用
- 依据:BBR 的 BDP 模型(速率 × 时延决定"在途数据量"的下界);GridFTP/DADC 系列并行传输
  工作中"块大小匹配带宽时延积、避免流水线空转"是同一结论
- 决策写日志:"任务 X 开始分块下载…分块 1024 KiB × 并发 4(上限 16)"与
  "分块大小自适应"(第二条在首次学到数据后的任务里出现)

### 全局并发预算

每个文件的自适应控制器只看得见自己:3 文件 × 16 路 = 48 条流足以触发服务端限流,
之后再集体退避来回震荡。进程内设总闸 `GLOBAL_MAX_STREAMS = 24`:

- `plan` 时按剩余额度决定页面初始宽度(至少 1 路,保证任务能起步)
- 控制器增长需向预算申请(超额不放行;控制器随后按"无增益"自行撤回探测,不会误判)
- 任务结束(完成/暂停/失败)释放本任务占用的额度

## 12. 验收标准

- [ ] `cargo test --locked` 全绿(含新管理器测试)
- [ ] `cargo clippy --locked --all-targets` 无 warning;`cargo fmt` 干净
- [ ] `npm run build` 通过;`npx tauri build` 产出 MSI/NSIS
- [ ] **客户端可用性**(硬约束):
  - 下载任务页:队列/进度/速度/暂停/继续/取消/重试/历史/打开位置 全部可用
  - 设置:下载目录选择 + 存储迁移可用;并发/分块/超时/重试/限速改动生效
  - 日志页、Telegram Web 窗口正常;无死按钮、无到已删功能的入口
- [ ] 真机端到端(§10.2 全部通过)
- [ ] Rust 端零 Grammers 依赖;`grep grammers src/` 为空
- [ ] 无 `upload/forward/cloud/transfer/login` 命令残留

## 13. 参考

- 验证脚本源码(下载方式逐条对齐): https://greasyfork.org/zh-CN/scripts/446342-telegram-media-downloader/code
- 注入脚本规范: `docs/superpowers/specs/2026-09-26-telegram-webview-inject-enhancement-design.md`
- `chunk_writer` / `task_store` / `atomic_file`:现有实现,原样复用
