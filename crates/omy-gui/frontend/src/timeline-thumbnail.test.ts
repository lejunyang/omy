import { describe, expect, it } from 'vitest';
import { timelineLayout } from './timeline-thumbnail';

describe('时间轴缩略图布局', () => {
  it('短视频至少采 12 帧且取时间桶中点', () => {
    const actual = timelineLayout(12, 1920, 1080);
    // 少于 12 帧时拖动几乎看不出画面变化；取端点又容易撞上黑帧。
    expect(actual.count).toBe(12);
    expect(actual.times[0]).toBeCloseTo(0.5);
    expect(actual.times.at(-1)).toBeCloseTo(11.5);
  });

  it('长视频最多采 36 帧以限制解码和内存开销', () => {
    const actual = timelineLayout(10_800, 1920, 1080);
    // 不设上限会让三小时视频打开预览时产生数千次 Range 请求。
    expect(actual.count).toBe(36);
    expect(actual.col * actual.row).toBeGreaterThanOrEqual(actual.count);
  });

  it('保持横屏和竖屏视频的宽高比', () => {
    const landscape = timelineLayout(60, 1920, 1080);
    const portrait = timelineLayout(60, 1080, 1920);
    // 不能把竖屏视频硬塞进 16:9，否则进度条预览会严重拉伸。
    expect(landscape.width).toBe(160);
    expect(landscape.height).toBe(90);
    expect(portrait.width).toBe(90);
    expect(portrait.height).toBe(160);
  });

  it('异常元数据仍返回有界布局', () => {
    const actual = timelineLayout(Number.NaN, 0, 0);
    // 元数据暂不可用时不能产生 NaN 尺寸或无限循环。
    expect(actual).toMatchObject({ count: 12, width: 160, height: 90 });
    expect(actual.times.every((time) => time === 0)).toBe(true);
  });
});
