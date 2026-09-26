# Telegram CDN 支持调查

调查对象是锁定的 Grammers `0.10.0` 与普通用户账号有权读取的 Telegram 媒体。当前桌面客户端仍把 Telegram CDN 标为不支持：直连下载收到 CDN 重定向时明确失败，不把密文写入文件，也不报告下载成功。

## 协议要求

Telegram 的 CDN 方法位于 `upload.*` 命名空间。`upload.getFile` 可以返回 `upload.fileCdnRedirect`；后续需要在受信任 CDN 数据中心调用 `upload.getCdnFile`。主数据中心还需要处理 `upload.reuploadCdnFile` 与 `upload.getCdnFileHashes`。[Telegram CDN 协议](https://core.telegram.org/cdn#schema)、[`upload.getFile`](https://core.telegram.org/method/upload.getFile)

CDN 返回的是 AES-256-CTR 密文。客户端必须先从主数据中心获得并验证对应密文分段的 SHA-256 `FileHash`，再按分块偏移计算 CTR IV 并解密；缺哈希、哈希不匹配或 token/key 变化都必须使该分块失败。CDN 的 RSA key 来自 `help.getCdnConfig`，需与受信任指纹匹配；CDN DC 配置也必须来自 Telegram 的可信配置。[Telegram CDN 说明](https://core.telegram.org/cdn)、[`help.getCdnConfig`](https://core.telegram.org/method/help.getCdnConfig)

`upload.getCdnFile` 的 offset 与 limit 有 4096 字节对齐、1 MiB 边界和单次请求限制；文件末尾需要单独处理短块。重定向、reupload、hash 覆盖范围、并发乱序、文件引用刷新、取消和断点恢复必须共同正确，不能仅把 `cdn_supported` 改成 `true`。[CDN 请求限制](https://core.telegram.org/cdn#restrictions-on-uploadgetfile-and-uploadgetcdnfile-parameters)、[文件引用刷新](https://core.telegram.org/api/file-references)

## Grammers `0.10.0` 评估

- 高层下载器把 `cdn_supported` 设为 `false`，其 CDN 重定向分支没有完整处理；直接调用它不能安全获得 CDN 下载。
- 锁定版生成的 TL 类型包含 `upload.getCdnFile`、`upload.reuploadCdnFile` 和 `upload.getCdnFileHashes`，`Client::invoke_in_dc` 能按 DC 发 raw API 调用；Grammers 文档也提醒 raw API 没有稳定语义版本保证。[Grammers 0.10.0 Client API](https://docs.rs/grammers-client/0.10.0/grammers_client/client/struct.Client.html)
- TL 调用能力不等同于 CDN 传输能力。对锁定依赖源码的审查发现，Grammers MTProto 握手使用内部 RSA fingerprint 列表，没有公开接口把 `help.getCdnConfig` 返回的 CDN RSA key 注入新 CDN auth key 握手。高层适配层也没有 CDN 专用连接池、CDN 重传和哈希到解密的完整管线。

因此当前版本保持失败关闭。以后实现前需要先增加独立 CDN 传输适配器或扩展 Grammers 的 CDN auth-key 建立能力，并提供可控 mock 覆盖 redirect、分块乱序、密文哈希、CTR 偏移、重传、过期 token、取消和损坏数据。Telegram test DC 与真实 CDN 也需单独验收。CDN 只用于 Telegram 正常授权的媒体传输；受保护、限时或无权限消息仍不提供下载入口。

这份调查没有运行真实 CDN 下载或吞吐基准，不代表 CDN 在每个账号、区域或文件上都会启用。
