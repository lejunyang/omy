import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath, URL } from 'node:url';

/**
 * omy 前端构建配置。
 *
 * # 产物必须能在严格 CSP 下运行
 *
 * 应用的 CSP 是 `script-src 'self'`：不允许 `eval`，也不允许内联
 * `<script>`。这带来三条硬约束：
 *
 * 1. **必须用预编译的模板**。Vue 的运行时模板编译器内部用 `new Function`，
 *    在这个 CSP 下会直接抛错。SFC 经由 `@vitejs/plugin-vue` 在构建期
 *    编译成渲染函数，运行时只需 `vue.runtime.*`——这正是加构建步骤
 *    换来的东西。别名 `vue` → `vue/dist/vue.runtime.esm-bundler.js`
 *    是显式保险，防止误引入带编译器的完整版。
 *
 * 2. **不能内联任何资源**。`assetsInlineLimit: 0` 关掉小文件转 data URI；
 *    虽然 img-src 允许 data:，但 CSS 里的 data URI 在某些 WebView 上
 *    仍会被 style-src 拦，不如统一走文件。
 *
 * 3. **不生成 modulepreload polyfill**。它是一段内联脚本，会被 CSP 挡掉，
 *    而我们只有一个入口 chunk，本来也用不上预加载。
 *
 * # 为什么输出到 ../dist 而不是默认的 dist
 *
 * `tauri.conf.json` 里 `frontendDist` 指向 crate 根的 `dist`，Rust 侧
 * 已经按这个路径打包资源。保持不变，重构就完全不触碰后端。
 */
export default defineConfig({
  plugins: [vue()],

  // frontend/ 是源码，产物回到 crate 根的 dist/
  root: fileURLToPath(new URL('.', import.meta.url)),

  // 相对路径而非默认的 `/`：i18n 用 `fetch('locales/...')` 取文案，
  // 那是相对当前文档的。如果脚本用绝对 `/app.js` 而文案用相对路径，
  // 一旦页面不在根路径（自定义协议在某些平台会带前缀）两者就会失配。
  // 统一成相对路径，整个产物可以整体搬到任何前缀下
  base: './',

  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      // 只用运行时版本：带编译器的完整版会在 CSP 下崩溃
      vue: 'vue/dist/vue.runtime.esm-bundler.js',
    },
  },

  build: {
    outDir: fileURLToPath(new URL('../dist', import.meta.url)),
    emptyOutDir: true,
    // 目标跟随 Tauri 的 WebView 下限：
    // Windows 是 WebView2（常青，跟随 Edge），macOS 12+ 的 WKWebView，
    // Linux 的 WebKitGTK 2.36+。es2022 在这三者上都完整支持。
    target: 'es2022',
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
    // 出问题时能对回源码。生产包里 sourcemap 是独立文件，
    // 不会增加运行时体积，但能让崩溃栈可读
    sourcemap: true,
    rollupOptions: {
      output: {
        // 固定文件名，不带 hash：Tauri 从本地加载，没有 CDN 缓存问题，
        // 而固定名字让 index.html 的引用稳定、diff 也干净
        entryFileNames: 'app.js',
        chunkFileNames: 'chunk-[name].js',
        assetFileNames: (info) =>
          info.names?.[0]?.endsWith('.css') ? 'app.css' : 'assets/[name][extname]',
      },
    },
  },

  server: {
    // 浏览器里跑 dev server 时没有 Tauri IPC，界面会明确报错而不是白屏。
    // 真正开发仍应通过 `cargo run -p omy-gui` 启动
    port: 5173,
    strictPort: true,
  },
});
