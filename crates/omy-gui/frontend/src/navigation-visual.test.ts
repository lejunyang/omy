import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

describe('本地文件导航与目录视觉', () => {
  it('上一级按钮使用左箭头而不是上传图标', () => {
    const source = readFileSync('src/components/MainScreen.vue', 'utf8');
    const upButton = source.match(/:title="i18n\.t\('nav\.up'\)"[\s\S]*?<\/button>/)?.[0] || '';
    // 不这样会怎样：上传箭头和“上一级”动作语义相反，用户会误以为按钮会传文件。
    expect(upButton).toContain('<AppIcon name="arrowLeft" />');
    expect(upButton).not.toContain('<AppIcon name="upload" />');
  });

  it('文件夹缩略图不重复绘制选中边框', () => {
    const css = readFileSync('src/styles/app.css', 'utf8');
    const directoryRule = css.match(/\.card \.thumb\.dir\s*\{[^}]*\}/s)?.[0] || '';
    const selectedRule = css.match(/\.card\.sel \.thumb\.dir\s*\{[^}]*\}/s)?.[0] || '';
    // 不这样会怎样：所有目录都带 accent 蓝底，看起来像整页已进入批量选择。
    expect(directoryRule).toContain('background: var(--bg2)');
    expect(directoryRule).not.toContain('var(--accent)');
    // 真正选中时外层 `.card.sel` 已有 accent 边框，内层再画会变成双重选中框。
    expect(selectedRule).toContain('background: color-mix');
    expect(selectedRule).toContain('border-color: transparent');
    expect(selectedRule).not.toMatch(/border-color:[^;]*var\(--accent\)/);
  });
});
