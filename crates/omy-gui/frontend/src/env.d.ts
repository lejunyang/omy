/// <reference types="vite/client" />

/**
 * 让 TS 认识 .vue 单文件组件的默认导出（Vue 官方 SFC shim）。
 *
 * 没有这个声明，`import App from './App.vue'` 在 vue-tsc 下会报
 * “找不到模块或其相应的类型声明”。运行时由 Vite 的 Vue 插件解析，
 * 这里只负责编译期类型。
 */
declare module '*.vue' {
  import type { DefineComponent } from 'vue';
  // 组件的 props/emit 等由 <script setup lang="ts"> 自行推导；
  // 这个兜底用宽松类型，不强制每个 SFC 都写显式泛型。
  const component: DefineComponent<Record<string, unknown>, Record<string, unknown>, unknown>;
  export default component;
}
