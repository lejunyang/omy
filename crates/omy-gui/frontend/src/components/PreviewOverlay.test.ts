/**
 * 预览里的相册式翻看与图片缩放。
 *
 * 不这样会怎样：看下一张照片得关掉预览、回列表找、再打开；手机上图片也
 * 放不大，看不清细节。
 */
import { describe, it, expect, vi } from 'vitest';
import { mount } from '@vue/test-utils';

vi.mock('../api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

import PreviewOverlay from './PreviewOverlay.vue';

const img = { id: 't1', name: 'a.jpg', kind: 'image' };
const stubs = { AppIcon: true, OmyVideoPlayer: true, OmyAudioPlayer: true };

describe('PreviewOverlay 翻看', () => {
  it('方向键切换上一个 / 下一个，没有的那一侧不响应', async () => {
    const w = mount(PreviewOverlay, { props: { file: img, plain: true, hasNext: true }, global: { stubs } });
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight' }));
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowLeft' }));
    expect(w.emitted('next')).toHaveLength(1);
    expect(w.emitted('prev')).toBeUndefined();
    w.unmount();
  });

  it('视频不接管方向键（播放器用它快进快退）', async () => {
    const w = mount(PreviewOverlay, {
      props: { file: { ...img, kind: 'video' }, plain: true, hasNext: true },
      global: { stubs },
    });
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'ArrowRight' }));
    expect(w.emitted('next')).toBeUndefined();
    w.unmount();
  });

  it('向左快滑翻到下一个', async () => {
    const w = mount(PreviewOverlay, { props: { file: img, plain: true, hasNext: true }, global: { stubs } });
    const body = w.find('.overlay-body');
    await body.trigger('pointerdown', { pointerId: 1, clientX: 300, clientY: 200 });
    await body.trigger('pointerup', { pointerId: 1, clientX: 150, clientY: 210 });
    expect(w.emitted('next')).toHaveLength(1);
    w.unmount();
  });

  it('双击图片放大，再双击还原', async () => {
    const w = mount(PreviewOverlay, { props: { file: img, plain: true }, global: { stubs } });
    const im = w.find('img');
    await im.trigger('dblclick');
    expect(im.attributes('style')).toContain('scale(2.5)');
    await im.trigger('dblclick');
    expect(im.attributes('style')).toContain('scale(1)');
    w.unmount();
  });
});
