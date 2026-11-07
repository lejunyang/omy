<script setup lang="ts">
/** 主题色选择：一排色块，单选。
 *
 * 用色块而不是下拉框：颜色要看见才选得出，「青绿」「琥珀」这类名字
 * 读了也想象不出具体是哪一种。名字仍作为 title / aria-label 提供给读屏。
 *
 * 每个色块挂着自己的 `data-accent`，渐变两端读 --brand-1 / --brand-2：
 * app.css 里预设的品牌色写在 `[data-accent=…]` 上而不只写在 :root 上，
 * 所以色块就地拿到该预设的颜色。色值只在 app.css 一处，这里不再抄一份。
 */
import * as i18n from '../i18n';
import { ACCENTS, type Accent } from '../theme';

defineProps<{ modelValue: string; labelledby?: string }>();
const emit = defineEmits<{ (e: 'update:modelValue', v: Accent): void }>();
</script>

<template>
  <div class="accentpick" role="radiogroup" :aria-labelledby="labelledby">
    <button
      v-for="a in ACCENTS"
      :key="a"
      type="button"
      class="swatch"
      role="radio"
      :data-accent="a"
      :data-sf="`accent_${a}`"
      :aria-checked="modelValue === a"
      :aria-label="i18n.t(`settings.accent_${a}`)"
      :title="i18n.t(`settings.accent_${a}`)"
      @click="emit('update:modelValue', a)"
    >
      <AppIcon v-if="modelValue === a" name="check" />
    </button>
  </div>
</template>

<style scoped>
.accentpick {
  display: flex;
  flex-wrap: wrap;
  gap: calc(var(--sp) * 2);
}
.swatch {
  width: 28px;
  height: 28px;
  padding: 0;
  border-radius: 50%;
  border: 2px solid transparent;
  background: linear-gradient(135deg, var(--brand-1) 0%, var(--brand-2) 100%);
  color: #fff;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  transition: transform var(--dur-1) var(--ease-out);
}
.swatch:hover {
  transform: scale(1.08);
}
/* 选中态除了对勾再加一圈外环：只靠白色对勾，在亮色块（琥珀）上不够显眼 */
.swatch[aria-checked='true'] {
  box-shadow: 0 0 0 2px var(--bg), 0 0 0 4px var(--brand-1);
}
.swatch :deep(svg) {
  width: 16px;
  height: 16px;
}
@media (--mobile) {
  /* 触控下限 44：色块本身 36，间距补足 */
  .swatch {
    width: 36px;
    height: 36px;
  }
  .accentpick {
    gap: calc(var(--sp) * 3);
  }
}
</style>
