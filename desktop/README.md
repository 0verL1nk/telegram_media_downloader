# Telegram Media Downloader 桌面客户端

这是仓库新的 Windows 优先桌面客户端，使用 Tauri 2、Rust、React、TypeScript、Tailwind CSS、Radix UI 和 shadcn 风格组件。旧 Python 版本仍保留在仓库中作迁移参考；桌面客户端不依赖 Python，也不包含 Telegram Bot 功能。

## 当前功能

- 在主窗口中打开 Telegram Web；WebView2 profile 与下载核心的 MTProto Session 分开保存。
- 支持普通 Telegram 账号的手机号、验证码和两步验证登录。首次使用时需在客户端设置页填写 Telegram API ID 和 API Hash；这两个值通过 Windows 凭据管理器保护，不写入配置 JSON 或日志。
- 浏览有权访问的聊天和频道，分页查看历史消息，按消息 ID 范围、媒体类别、文件格式及筛选表达式创建任务。
- 在本机 SeaORM/SQLite 任务库保存下载状态、分块完成摘要和重试信息；支持排队、速度和剩余量、暂停、继续、取消、重试、历史列表及打开文件位置。
- Telegram 原生下载使用有界队列、单文件分块并发、请求节奏控制、重试、file reference 刷新、分块摘要校验和临时文件原子提交。
- 为可下载消息注入“下载”按钮；桥接只接受聊天 ID、消息 ID 与媒体类型，Rust 重新读取消息并检查访问权限。
- 支持 rclone 云上传队列、进度、重试、取消、可选压缩以及按设置清理本地副本。
- 可将已完成文件发送为 Telegram 文档，也可将单条可转发消息转发到数字 ID 指定的聊天。
- 设置页管理下载位置、目录和文件名模板、媒体过滤、重复文件策略、并发、分块、超时、重试、限速、SOCKS5 代理、rclone 和数据目录迁移。
- 可从旧 `config.yaml` 导入桌面可用设置和失败消息 ID；Bot Token、Bot 用户名单等字段不会迁入桌面客户端。

## 安装与首次使用

Windows 安装包由 GitHub Actions 生成，构建成功后可在 workflow 的 artifact 下载 `.msi` 或 NSIS `.exe`。本地构建产物位于 `src-tauri/target/release/bundle/msi/` 和 `src-tauri/target/release/bundle/nsis/`。应用使用系统 WebView2 Runtime 显示 Telegram Web。

首次启动时：

1. 在“设置 → Telegram 账号”填写 Telegram API ID 与 API Hash；设置页不会要求用户编辑配置文件。
2. 在 Telegram 页面发起 MTProto 登录，输入手机号、验证码和必要的两步验证密码。Telegram Web 需要在同一页面中单独登录；WebView2 会保存自己的登录 profile。
3. 在设置页确认数据和下载目录。应用优先选择可用空间最大的非 C 盘；若没有其他本机卷，则使用系统提供的应用数据目录。可在设置里主动迁移到其他目录。
4. 登录后选择聊天，浏览历史或填写消息 ID 范围，选媒体类型并创建下载任务。
5. 已完成文件可以加入 rclone 队列，或显式发送到 Telegram。Telegram 目标使用数字聊天 ID；在聊天列表可查看源聊天 ID。

桌面客户端中的 MTProto 登录与 Telegram Web 登录是独立会话，无法互相导入或自动同步。MTProto Session 使用加密文件保存，密钥保存在 Windows 凭据管理器。WebView2 profile 包含其自身登录状态和站点数据，放在选定的数据目录；应用不读取或导出其中 Cookie、localStorage 或验证码。安全退出可以注销 MTProto；勾选清理时会删除本机 MTProto Session 与加密密钥。Telegram Web profile 可通过应用数据目录备份/迁移。

## 旧配置导入

在“设置 → 从旧版本迁移”选择旧版 `config.yaml`。导入会保存一份桌面设置，并只迁移本客户端理解的字段，例如 API ID、下载目录、媒体类型/格式、路径和文件名模板、`date_format`、`file_name_prefix_split`、并发、代理、rclone、Telegram 目标及失败消息 ID。日期格式也能在“设置 → 文件管理”里直接调整，`{media_datetime}` 会应用该格式。`api_hash` 和代理密码存入 Windows 凭据管理器；Bot Token、Bot API 设置和 Bot 用户名单会明确忽略。导入不会删除或修改源 YAML。

配置 JSON 带 `schemaVersion`，未知的更高版本不会被降级覆盖。任务数据库由 SeaORM 版本迁移管理。更新客户端前应关闭客户端并备份所选数据目录；回滚时安装旧客户端并恢复整份备份。下载文件位于“下载目录”，不会随卸载自动清理。

## 数据位置与清理

应用数据目录包括加密 MTProto Session、WebView2 profile、SQLite 任务数据库、分块临时文件、日志和缓存。应用会在系统配置目录仅保存一个活动数据根路径指针。新装默认选择最大的可用非 C 盘；缺失或离线的已配置磁盘会显示错误，客户端不会静默重置指针到 C 盘。

通过“设置 → 存储与迁移”选择新目录并确认后，客户端会校验目录、等待活动任务停止、关闭 MTProto 和 Telegram WebView，再复制并逐文件校验。原始目录会保留作备份；完成后重启客户端以使用新 WebView2 profile 路径。清理任务临时文件前请先取消或完成对应任务；不要手动删除 `TaskData` 中仍可恢复的 `.part` 文件。

## 下载设置与限制

- 文件并发、单文件分块并发、分块大小、请求超时、重试次数和带宽上限分别设置。队列、请求数和每个文件的待写分块都有上界。
- 分块按 64/128/256/512/1024 KiB 选择，且满足 Telegram 1 MiB 边界对齐要求；已完成分块会验证本地 BLAKE3 摘要后复用，存在 Telegram `GetFileHashes` 时额外验证服务端 SHA-256。
- Telegram 当前返回 CDN 重定向时，任务会失败并说明原因；本版本没有实现 Telegram CDN 密钥流解密和 CDN hash 校验，因此不会尝试保存未经验证的 CDN 内容。
- Telegram 上传 API 的 Grammers 公开 `upload_stream` 接口没有进度回调，界面不会伪造上传百分比；传输命令在 Telegram 确认后返回结果。
- Telegram 上传目前发送单个完成文件为文档；普通消息转发只支持单条消息。旧版媒体组批量转发、原格式媒体上传、caption 实体、广告替换规则和上传进度尚未完整迁移。
- HTTP/SOCKS4 代理未接入 Grammers 下载连接；当前只支持 SOCKS5。受保护、限时、无权限或 Telegram 明确拒绝的消息会禁用入口或返回清晰错误，不会读取隐藏媒体 URL、解密内容或绕过 Telegram 限制。
- 真实 Telegram 网络吞吐取决于账号、DC/CDN、代理、磁盘与 Telegram 服务端限制。当前仓库没有可用于如实报告对照带宽/CPU/内存的凭据或基准结果，不能把设置中的并发上限当作实测性能。

## 开发和验证

需要 Node.js 24、Rust stable、Windows MSVC Build Tools、WebView2 Runtime、NSIS 与 WiX Toolset（MSI 打包需要）。依赖版本由 `package-lock.json` 与 `Cargo.lock` 锁定。

```powershell
cd desktop
npm ci
npm run build
npx tauri dev
npx tauri build
```

Rust 核心检查：

```powershell
cd desktop/src-tauri
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
```

GitHub Actions 工作流位于 `.github/workflows/desktop.yml`，在 Windows x64 上运行前端构建、Rust 格式检查与测试，并发布 MSI/NSIS workflow artifacts。以后扩展 macOS/Linux 时，为新增 runner 添加系统 WebKit/GTK 开发依赖并启用相应 bundle target；不要把 Telegram 凭据或签名证书提交到仓库。

## 故障排查

- **Telegram Web 没有加载：**检查网络和 WebView2 Runtime，在页面工具栏重试；WebView profile 与 MTProto Session 相互独立。
- **下载提示未登录/API 凭据缺失：**先保存 API ID/API Hash，再完成 MTProto 登录；不要把 Telegram Web 登录视为 MTProto 授权。
- **旧 Session 无法解密：**检查 Windows 凭据管理器中的客户端密钥项是否仍在，或从原数据备份恢复 Session 文件和数据目录。删除密钥而保留加密 Session 会导致 Session 无法解密。
- **磁盘离线或空间不足：**连接原数据磁盘后重试，或使用设置页迁移到有足够空间的空目录。下载目录可以与应用数据目录分开设置。
- **任务因 CDN 重定向失败：**当前版本不支持 CDN 加密/哈希验证；重试可能仍受相同服务器路由限制。
- **rclone 失败：**检查可执行文件路径、`remote:folder` 名称、rclone profile/权限和日志页给出的进程错误。凭据应配置在 rclone 自己的受保护配置中，不写进客户端任务参数。

## 许可证

桌面客户端代码按仓库根目录 MIT 许可证发布。第三方 Rust/JavaScript 依赖保留其各自许可证；发布前应从锁定依赖清单生成并审查随安装包分发的 NOTICE/许可证清单。
