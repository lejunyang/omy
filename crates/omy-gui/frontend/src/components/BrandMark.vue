<script setup lang="ts">
/** omy 品牌标（内联 SVG）。
 *
 * 一把锁，锁体上是播放键：「加密之后依然能直接用」是这个应用最核心的一句话。
 * 与应用图标同源（crates/omy-gui/icons/logo.svg），改形状时两处一起改。
 * 内联而不是 <img>：CSP 下不想为一个图标多开资源，且渐变 id 加了前缀，
 * 页面上同时出现多个也不会互相串色。
 */
defineProps({
  size: { type: Number, default: 22 },
});
</script>

<template>
  <svg
    class="brandmark"
    :width="size"
    :height="size"
    viewBox="0 0 64 64"
    aria-hidden="true"
    focusable="false"
  >
    <defs>
      <linearGradient id="omy-brand-g" x1="0" y1="0" x2="1" y2="1">
        <stop offset="0" stop-color="#2fd0b9" />
        <stop offset="1" stop-color="#0f766e" />
      </linearGradient>
    </defs>
    <rect x="2" y="2" width="60" height="60" rx="16" fill="url(#omy-brand-g)" />
    <path
      d="M22.5 29v-5.5a9.5 9.5 0 0 1 19 0V29"
      fill="none"
      stroke="#fff"
      stroke-width="5"
      stroke-linecap="round"
    />
    <rect x="15" y="27" width="34" height="26" rx="8" fill="#fff" />
    <path d="M29 33.6 39.4 40 29 46.4Z" fill="#0f766e" stroke="#0f766e" stroke-width="2" stroke-linejoin="round" />
  </svg>
</template>
