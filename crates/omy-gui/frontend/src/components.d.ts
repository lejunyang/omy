import type AppIcon from './components/AppIcon.vue';

declare module 'vue' {
  export interface GlobalComponents {
    AppIcon: typeof AppIcon;
  }
}

export {};
