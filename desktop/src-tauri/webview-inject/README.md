# Telegram WebView 注入脚本

> 本目录实现 Telegram Web 子 WebView 中的下载按钮注入逻辑。

## 开发

修改 `src/*.js` 后,运行:

```bash
cd desktop
node src-tauri/webview-inject/build.mjs
```

或 `cargo build` / `npx tauri dev`(会自动触发 build.rs)。

## 测试

```bash
cd desktop
npx vitest run
```

## 安全约束

永不读: `src` / `href` / `cookie` / `localStorage` / `__TAURI_INTERNALS__`。
仅读: `data-*` / `aria-disabled` / 可见性 / `tagName` / `className`。
