# 功能迁移与验收表

> 基线审查日期：2026-09-26。旧 Python 文件和测试保留为迁移参考。原仓库 `master`、本地 `origin/master` 与本地 `upstream/master` 当时均为 `fe7ccdc`；没有 fetch、合并或推送。
>
> 本表区分代码实现、自动化验证和真实 Telegram 验证。仅有 UI、可编译或单元测试通过，不代表 Telegram 网络功能已经端到端通过。

## 状态定义

- **已验证**：有明确的自动化测试或可复现的本机 UI/构建实测。
- **已实现，未端到端验证**：代码与界面已存在，但还缺真实账号、网络或外部服务验证。
- **部分实现**：核心路径已实现，同时存在本表列出的功能缺口。
- **未实现**：未迁移或尚无可验收实现。
- **排除**：Bot 专属能力，按产品范围不进入桌面客户端。

## 非 Bot 功能迁移矩阵

| 范围 | 新客户端状态 | 验收证据与限制 |
|---|---|---|
| 普通用户 MTProto 登录与 Session | 已实现，未端到端验证 | 有手机号、验证码、2FA UI；Grammers Session 加密后保存，密钥放 Windows 凭据管理器；WebView 登录不复用 MTProto Session。未使用真实账号验证登录、重启续登、注销和凭据清理。 |
| Telegram Web 主窗口与登录态保存 | 已验证（Windows 开发构建冒烟） | Tauri 2.11.6 子 WebView 在主窗口 Telegram 页面加载出 Telegram 官方登录二维码；WebView2 profile 位于用户选择的数据根。未登录账号、未验证已有登录态重启恢复。Tauri 的 `WebviewBuilder` 与 `Window::add_child` 标为 unstable，升级有 API 风险。 |
| 聊天/频道选择、搜索、历史分页 | 已实现，未端到端验证 | 有聊天搜索/分页、历史消息加载和空/错误状态；必须先完成 MTProto 授权。未用真实账号覆盖频道、群组、私聊和服务端错误。 |
| 消息范围与续扫 | 部分实现 | 支持起止消息 ID 和按页加载；没有日期范围选择器，也没有可独立验收的自动续扫基准。 |
| 媒体类别、扩展名和消息过滤 | 部分实现 | UI 与 Rust 过滤器可按媒体类别、扩展名和旧表达式筛选；支持的旧表达式子集有长度/复杂度限制，Rust 正则不支持 Python lookaround/反向引用；消息字段与 Telegram 线上边界未实测。 |
| 路径、文件名、日期格式、说明和纯文本消息 | 已实现，迁移与路径单测覆盖 | 设置页可编辑目录/命名模板和日期格式；旧 `date_format`、`file_name_prefix_split` 与 `media_datetime` 模板迁移，并测试日期格式应用于输出目录/文件名。支持说明 sidecar、纯文本消息及重复文件 skip/rename/overwrite。尚未逐项对照全部旧命名模板输出。 |
| 下载队列、状态、详情和历史 | 已验证（自动化） | SeaORM/SQLite 保存任务；有进度、速度、剩余量、开始时间、暂停/继续/取消/重试、错误详情、历史和打开位置。任务状态迁移及中断后重新入队有单测。 |
| 分块下载、断点恢复和校验 | 已验证（模拟/单测） | 有界队列、文件并发与单文件 raw 分块并发；乱序写入、缺块检测、块重试、BLAKE3 本地块校验、可用时服务端 SHA-256 校验、临时文件验证和原子提交有测试。未用 Telegram 网络做大文件中断恢复。 |
| MTProto CDN 路径 | 未实现 | 服务端返回 CDN 重定向时任务明确失败；没有实现 CDN 密钥流解密与 CDN hash 校验。不会把 CDN 密文当作成功文件。 |
| 媒体组 | 部分实现 | 下载以消息为单位保存并记录 group ID；未实现原项目媒体组批量转发/上传、顺序与 caption entity 兼容。 |
| 并发、FloodWait、带宽与代理 | 部分实现 | 文件并发和分块并发分别有界可调；请求超时、重试、限速、节奏退避已实现。当前只支持 SOCKS5；HTTP/SOCKS4 不迁移。未测代理网络、FloodWait 实际触发行为或最大带宽。 |
| Telegram 上传与转发 | 部分实现 | 支持把已完成文件作为文档上传，以及单条消息转发；没有持久化传输队列、上传进度回调、媒体组上传、caption entity、广告替换规则及自动上传调度。 |
| 云盘 / rclone | 部分实现 | 有持久 rclone 队列、状态、进度解析、重试/取消、可选压缩和上传后清理；有路径/进度解析单测，未连接真实远端完成端到端测试。Aligo 未迁移。 |
| 日志与运行状态 | 已验证（自动化） | 日志轮转、保留期和敏感字段脱敏有单测；界面提供日志和状态页。未收集真实网络故障诊断样本。 |
| 旧配置迁移 | 部分实现 | 设置页导入旧 `config.yaml`，迁移客户端支持的字段；包含旧 `date_format`、`file_name_prefix_split`、路径/文件名 token、媒体筛选与运行设置。Bot Token/用户名单不导入，敏感 API Hash/代理密码进凭据存储。HTTP/SOCKS4 与 Aligo 项会提示并跳过；并非所有 Python 配置字段均可迁移。 |
| 数据位置、备份与升级 | 已实现，未做安装升级实测 | 默认选有空间的非系统盘，允许在客户端更改；数据库、分块、Session、WebView profile、日志随数据根迁移并验证复制。若没有其他可用卷才回退系统提供目录。未执行 MSI/NSIS 覆盖升级和回滚演练。 |
| 完整桌面 UI | 已实现，部分实测 | React 页面包含总览、Telegram、下载任务、云端上传、日志状态、设置和存储迁移；本机实测总览与 Telegram Web 登录页。其它页面未逐屏人工验收。 |
| Windows 构建与 CI | 本地构建已验证；Actions 未验证 | `.github/workflows/desktop.yml` 配置 Windows x64 前端/Rust 检查和 MSI/NSIS artifacts。本机可构建两个 bundle；没有实际 GitHub Actions run、签名或依赖 NOTICE 审计。 |
| 吞吐基准 | 未验证 | 尚无原 Python 与 Rust 的同机、同文件集 Telegram 网络基准，因此不报告吞吐、CPU、内存峰值或“接近带宽上限”结论。 |

## Bot 排除边界

下列旧功能保持在原 Python 参考代码，不进入 Rust 客户端：`module/bot.py::DownloadBot`、Bot Token/API 登录、Bot 命令与回调、Bot 私聊/消息监听、Bot 专属状态上报、Bot 用户白名单及 Bot 页面/配置。共享下载队列、用户 MTProto 会话、历史读取、过滤、命名和普通用户上传能力单独评估，不按 Python 文件名整体排除。

## Pyrogram fork 补丁审计

依赖声明在 `requirements.txt`，指向 `tangyoha/pyrogram` 可移动的 `patch` 分支 zip，并非固定 revision。审查时该分支指向 `51a100c5e2745471ee89c1dd96dd69962973108b`（`fix: can not use proxy`）。本仓库的扩展依赖 `HookSession.start_timeout`、`HookClient.connect/start`、`Client.max_concurrent_transmissions`、`save_file_semaphore`、`get_file_semaphore`，并自行使用 raw `messages.GetHistory`、上传媒体组与转发请求。fork 的 `Client.get_file` 在 semaphore 内顺序读取 1 MiB 区块；其多 worker `save_file` 是并发上传，不是并发下载。

桌面端将其职责分别放入 Telegram 适配层、下载器和传输模块，不迁移 Pyrogram 私有对象或 Bot 处理器。Telegram raw API 调用范围集中在 Grammers 适配层与下载器；升级时需重新检查 `upload.GetFile` 参数、对齐限制、file reference 刷新、DC/CDN 处理和错误类型。

## 固定版本与官方资料

- Tauri `2.11.6`：[`WebviewBuilder`](https://docs.rs/tauri/2.11.6/tauri/webview/struct.WebviewBuilder.html)、[`Window::add_child`](https://docs.rs/tauri/2.11.6/tauri/window/struct.Window.html#method.add_child)。本客户端使用远程 URL 子 WebView、独立数据目录、初始化脚本、导航来源限制和只允许 `submit_download_from_webview` 的远程 capability；两个子 WebView API 当前标为 unstable。
- Grammers `0.10.0`：[`grammers-client`](https://docs.rs/grammers-client/0.10.0/grammers_client/)、[`Client`](https://docs.rs/grammers-client/0.10.0/grammers_client/client/struct.Client.html)。下载/登录/历史封装在 `telegram.rs`；raw `upload.GetFile` 用于有界分块并发，底层 Telegram API 变化需单独审查。
- 注入脚本只检查 Telegram Web 页面中可见的消息标识和媒体类型。Rust 端重新获取消息并复核权限。不会读取 URL 属性、Cookie、localStorage、Session、验证码或隐藏媒体链接；不提供受保护、限时或无权限内容入口，也不实现加密/付费保护绕过。

## 本地验证记录

- Windows x64：`npx tauri build` 可生成 NSIS 与 MSI bundle；最终产物路径见根 README/客户端 README。GitHub Actions 尚未运行。
- Rust：`cargo fmt --all -- --check`、`cargo check --locked --lib`、`cargo test --locked`；单测覆盖任务状态、配置迁移、过滤、日志脱敏、分块顺序写入/恢复/损坏检测和下载调度边界。
- 前端：`npm run build` 与 `npx tsc --noEmit -p tsconfig.json`。
- GUI：Windows Tauri 开发运行中 Telegram Web 官方登录二维码显示在 Telegram 页面子 WebView。没有扫描二维码或使用真实 Telegram 账号。
- 不包含真实 Telegram 网络下载/上传、代理/FloodWait、rclone 远端、大文件恢复与性能对照实测。
