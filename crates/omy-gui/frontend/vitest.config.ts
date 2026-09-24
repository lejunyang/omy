/**
 * vitest 配置（UI/逻辑测试专用，不参与生产构建）。
 *
 * 与 vite.config.js 保持同样的两处别名：
 * - '@' -> src；
 * - vue -> 仅运行时版本，和 CSP `script-src 'self'` 下的产物一致，测试也不许
 *   偷用带模板编译器的完整版本。
 *
 * DOM 用 happy-dom：组件测试只需要挂载/事件/响应式，不需要真实渲染管线，
 * jsdom 之外的更轻选择。滚动容器的几何尺寸（scrollHeight/scrollTop 等）
 * happy-dom 不做布局，相关用例一律传入手造的假元素对象，不依赖真实测量。
 */
import { defineConfig } from 'vitest/config';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath, URL } from 'node:url';

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': fileURLToPath(new URL('./src', import.meta.url)),
      vue: 'vue/dist/vue.runtime.esm-bundler.js',
    },
  },
  test: {
    environment: 'happy-dom',
    include: ['src/**/*.test.ts'],
  },
});
