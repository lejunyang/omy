# omy 前端

GUI 的界面源码。Vue 3 + Vite，构建产物输出到 `../dist/`，由 Tauri 加载。

## 为什么有构建步骤

重构前是手写的 `index.html` + `app.js` + `i18n.js`，直接放在 `dist/` 里
运行时加载。到 20 KB 单文件时几个问题同时出现：

- 所有渲染都是字符串拼 HTML，每处动态文本都要记得手动 `esc()`，
  漏一处就是注入面。
- 搜索框每敲一个字整页重绘，输入框会失焦，只能手写「只重绘内容区」
  的特例，然后重新绑定其中的事件。
- 切语言必须记得在末尾补一次全量重绘，漏了就是半个界面还是旧语言。

这些都是模板引擎解决过的问题。加上打包步骤之后，`{{ }}` 天然转义、
diff 算法保持焦点、响应式自动重渲染，那些特例逻辑连同注释一起删掉了。

## 严格 CSP 下的约束

应用的 CSP 是 `script-src 'self'`，不允许 `eval` 与内联脚本。这带来
三条硬约束，改构建配置时要留意：

1. **必须用预编译模板**。Vue 的运行时模板编译器内部用 `new Function`，
   在这个 CSP 下直接抛错。SFC 由 `@vitejs/plugin-vue` 在构建期编译成
   渲染函数，运行时只需 `vue.runtime.*`——这正是加构建步骤换来的东西。
2. **不能内联资源**。`assetsInlineLimit: 0` 关掉小文件转 data URI。
3. **不生成 modulepreload polyfill**。它是内联脚本，会被 CSP 挡掉。

`spikes/verify-gui-build.ps1` 会检查产物里没有 `eval` / `new Function`、
没有内联脚本、没有打进完整版 Vue。

## 目录

| 路径 | 说明 |
|---|---|
| `src/main.js` | 入口。启动顺序有依赖：主题 → 协议前缀 → 语言 → 挂载 |
| `src/api.js` | 与 Rust 后端的全部通信。用官方 `@tauri-apps/api` |
| `src/store.js` | 会话与文件列表状态 |
| `src/i18n.js` | 多语言。响应式，切语言自动重渲染 |
| `src/theme.js` | 明暗主题 |
| `src/components/` | 界面组件 |
| `src/styles/app.css` | 全局样式。色值取自 `docs/research/08-ui-ux-design.md` |
| `public/locales/` | 文案。运行时 `fetch` 加载，原样复制到产物 |

## 开发

平时不需要单独跑这里的命令——`cargo build -p omy-gui` 会在产物缺失或
前端源码变化时自动构建（见 `../build.rs`）。

需要单独操作时：

```bash
pnpm install     # 装依赖
pnpm build       # 构建到 ../dist
pnpm dev         # Vite dev server（注意：浏览器里没有 Tauri IPC，
                 # 界面会明确报错。真正开发仍应 cargo run -p omy-gui）
```

## 产物不入库

`../dist/` 是构建产物，不进版本库——否则每次改前端都会在 diff 里混进
一堆压缩后的 JS。`build.rs` 在产物缺失时会自动跑一次构建，所以全新
clone 之后直接 `cargo run -p omy-gui` 就能用，前提是装了 Node 20+ 与 pnpm。
