/**
 * 断点只有一个来源：样式写 `@media (--mobile)`，构建时展开成 breakpoint.ts 的值。
 */
import { readFileSync, readdirSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { describe, it, expect } from 'vitest';
import { MOBILE_MAX, MOBILE_MEDIA, mobileMediaPlugin } from './breakpoint';

function styleSources(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((d) => {
    const p = join(dir, d.name);
    if (d.isDirectory()) return styleSources(p);
    return /\.(vue|css)$/.test(d.name) ? [p] : [];
  });
}

describe('移动端断点', () => {
  it('插件把别名展开成与 matchMedia 相同的条件', () => {
    // 不这样会怎样：@media (--mobile) 原样留在产物里是无效查询，浏览器整块丢弃，
    // 手机上就只剩桌面布局，而 JS 仍判定为移动端
    const at = { params: '(--mobile)' };
    mobileMediaPlugin().AtRule.media(at);
    expect(at.params).toBe(MOBILE_MEDIA);
    expect(MOBILE_MEDIA).toBe(`(max-width: ${MOBILE_MAX}px)`);

    const combined = { params: '(--mobile) and (orientation: landscape)' };
    mobileMediaPlugin().AtRule.media(combined);
    expect(combined.params).toBe(`${MOBILE_MEDIA} and (orientation: landscape)`);
  });

  it('样式里不再手写断点数值', () => {
    // 不这样会怎样：新加的组件又写死 768，改断点时漏掉它，那一块在
    // 临界宽度上和 JS 的 isMobile 对不上
    const literal = new RegExp(`@media[^{]*width:\s*${MOBILE_MAX}px`);
    const offenders = styleSources(resolve(process.cwd(), 'src'))
      .filter((f) => literal.test(readFileSync(f, 'utf8')));
    expect(offenders).toEqual([]);
  });
});
