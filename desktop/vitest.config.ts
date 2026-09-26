import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'happy-dom',
    include: ['src-tauri/webview-inject/test/**/*.test.js'],
    setupFiles: ['src-tauri/webview-inject/test/setup.js'],
    coverage: {
      provider: 'v8',
      include: ['src-tauri/webview-inject/src/**/*.js'],
      reporter: ['text', 'html'],
    },
  },
});
