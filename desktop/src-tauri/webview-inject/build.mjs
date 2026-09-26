// desktop/src-tauri/webview-inject/build.mjs
// 把 src/*.js 按依赖序拼接、剥离 ESM import/export、内联 config.json、terser 压缩,
// 输出 dist/inject.js(由 Rust 侧 include_str! 嵌入)。
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { minify } from 'terser';

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = resolve(here, 'src');
const distDir = resolve(here, 'dist');

const config = JSON.parse(readFileSync(resolve(here, 'config.json'), 'utf-8'));

const ORDER = [
  'diag.js',
  'icons.js',
  'extract.js',
  'detect.js',
  'button.js',
  'task-state.js',
  'downloader.js',
  'state.js',
  'watcher.js',
  'inject.js',
];

const modules = ORDER.map((name) => {
  let code = readFileSync(join(srcDir, name), 'utf-8');
  // concat 后处于同一作用域,剥离模块语法(本目录只用单行 import 与具名导出)
  code = code
    .replace(/^import\s+[^;]*;?\s*$/gm, '')
    .replace(/^export\s+(async\s+function|function|const|let|class)/gm, '$1');
  return `// === ${name} ===\n${code}`;
});

const source = `(() => { 'use strict';
globalThis.__INJECT_CONFIG__ = ${JSON.stringify(config)};
${modules.join('\n')}
})();`;

const result = await minify(source, {
  compress: { passes: 2 },
  mangle: true,
  format: { comments: false },
});
if (!result.code) throw new Error('terser produced no output');

mkdirSync(distDir, { recursive: true });
writeFileSync(join(distDir, 'inject.js'), result.code, 'utf-8');
console.log(`wrote ${join(distDir, 'inject.js')} (${result.code.length} bytes)`);
