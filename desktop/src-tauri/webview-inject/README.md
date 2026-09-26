# Telegram WebView 注入脚本

> 本目录实现 Telegram Web 子 WebView 中的下载按钮注入逻辑。

## 开发

修改 `src/*.js` 后,运行:

```bash
cd desktop
node src-tauri/webview-inject/build.mjs
```

或 `cargo build` / `npx tauri dev`(会自动触发 build.rs)。

## 验证

本目录**不做单元测试**(合成 DOM fixture 无法代表真实 Telegram Web 结构)。验证方式是:

```bash
cd desktop
npx tauri dev
```

在开发窗口中登录真实 Telegram Web,逐类消息(图片/视频/语音/贴纸/GIF/文档/Story)确认按钮出现、点击生效、状态正确。真实 selector 以真机表现唯一准绳。

## 安全约束

永不读: `src` / `href` / `cookie` / `localStorage` / `__TAURI_INTERNALS__`(HTTP 下载器 spec 落地后 `src` 将受限放开)。
仅读: `data-*` / `aria-disabled` / 可见性 / `tagName` / `className`。
