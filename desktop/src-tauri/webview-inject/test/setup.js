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