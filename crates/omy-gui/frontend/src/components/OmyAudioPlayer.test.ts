/** 用原生媒体属性模拟状态；布局和真实解码留给 WebView 实测。 */
import { mount } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import OmyAudioPlayer from './OmyAudioPlayer.vue';
vi.mock('../i18n', () => ({
  __v_isRef: false,
  t: (key: string) => key,
  te: (key: string) => key,
  formatDuration: (ms: number) => String(ms / 1000),
}));
const wrappers: ReturnType<typeof mount>[] = [];
beforeEach(() => {
  vi.spyOn(HTMLMediaElement.prototype, 'load').mockImplementation(() => {});
  vi.spyOn(HTMLMediaElement.prototype, 'pause').mockImplementation(() => {});
  vi.spyOn(HTMLMediaElement.prototype, 'play').mockResolvedValue();
});
afterEach(() => { wrappers.splice(0).forEach(w => w.unmount()); vi.restoreAllMocks(); });
function setup() {
  const w = mount(OmyAudioPlayer, { props: { src: 'http://omystream.localhost/file/test', name: '<script>.mp3', autoplay: false } });
  wrappers.push(w);
  const el = w.find('audio').element as HTMLAudioElement;
  Object.defineProperties(el, {
    duration: { configurable: true, value: 120 },
    paused: { configurable: true, get: () => false },
    buffered: { configurable: true, value: { length: 2, start: (i: number) => i ? 70 : 0, end: (i: number) => i ? 100 : 30 } },
  });
  return { w, el };
}
describe('OmyAudioPlayer', () => {
  it('受控 URL 和跨源属性不丢，文件名不执行 HTML', () => {
    const { w, el } = setup();
    // 不这样会怎样：协议 URL 被改成全量 Blob，或文件名成为脚本入口。
    expect(el.getAttribute('src')).toBe('http://omystream.localhost/file/test');
    expect(el.getAttribute('crossorigin')).toBe('anonymous');
    expect(el.preload).toBe('metadata');
    expect(w.find('h2').text()).toBe('<script>.mp3');
    expect(w.find('script').exists()).toBe(false);
  });
  it('时间轴和快进实际写入 currentTime，边界不越过文件', async () => {
    const { w, el } = setup();
    await w.find('audio').trigger('loadedmetadata');
    await w.find('.audio-seek').setValue('115');
    // 不这样会怎样：界面时间变了但音频并没跳转。
    expect(el.currentTime).toBe(115);
    await w.findAll('.audio-jump')[1].trigger('click');
    expect(el.currentTime).toBe(120);
    await w.find('.audio-seek').setValue('3');
    await w.findAll('.audio-jump')[0].trigger('click');
    expect(el.currentTime).toBe(0);
    // 不这样会怎样：远端跳读的两段缓冲会被画成中间也已下载。
    expect(w.findAll('.audio-buffer')).toHaveLength(2);
  });
  it('无时长时禁止 seek，倍速与音量作用于真实元素', async () => {
    const { w, el } = setup();
    // 不这样会怎样：未知时长被当零，输入进度会产生非法 seek。
    expect(w.find('.audio-seek').attributes('disabled')).toBeDefined();
    await w.find('select').setValue('1.5');
    expect(el.playbackRate).toBe(1.5);
    await w.find('.audio-volume input').setValue('0.4');
    expect(el.volume).toBe(0.4);
    await w.find('.audio-volume button').trigger('click');
    expect(el.muted).toBe(true);
  });
  it('错误保留重试入口，并撤销正在播放状态', async () => {
    const { w, el } = setup();
    Object.defineProperty(el, 'error', { configurable: true, value: { code: 4 } });
    await w.find('audio').trigger('error');
    // 不这样会怎样：不兼容音频只剩一行错误，且自动锁定被永久豁免。
    expect(w.find('.audio-error').text()).toContain('audio.compatibility_hint');
    expect(w.emitted('pause')).toBeTruthy();
    await w.find('.audio-error button').trigger('click');
    expect(w.find('.audio-error').exists()).toBe(false);
  });
  it('旧文件的异步播放失败不能污染新文件', async () => {
    let rejectOld: (error: Error) => void = () => {};
    vi.mocked(HTMLMediaElement.prototype.play).mockImplementationOnce(() => new Promise<void>((_, reject) => { rejectOld = reject; }));
    const w = mount(OmyAudioPlayer, { props: { src: 'http://omystream.localhost/file/old', name: 'old.mp3' } });
    wrappers.push(w);
    await w.setProps({ src: 'http://omystream.localhost/file/new', name: 'new.mp3' });
    rejectOld(new Error('旧文件已经关闭'));
    await Promise.resolve();
    await w.vm.$nextTick();
    // 不这样会怎样：快速切换两个文件，旧 Promise 会让新文件显示播放失败。
    expect(w.find('.audio-error').exists()).toBe(false);
    expect(w.find('audio').attributes('src')).toBe('http://omystream.localhost/file/new');
  });
  it('系统拒绝自动播放时仍允许用户点击播放', async () => {
    vi.mocked(HTMLMediaElement.prototype.play).mockRejectedValueOnce(new DOMException('需要用户手势', 'NotAllowedError'));
    const w = mount(OmyAudioPlayer, { props: { src: 'http://omystream.localhost/file/test', name: 'a.mp3' } });
    wrappers.push(w);
    await Promise.resolve();
    await w.vm.$nextTick();
    // 不这样会怎样：正常的系统策略被报成损坏文件，播放按钮也被禁用。
    expect(w.find('.audio-error').exists()).toBe(false);
    expect(w.find('.audio-toggle').attributes('disabled')).toBeUndefined();
  });
  it('停止和换源都释放旧 src，不让解密缓冲继续存活', async () => {
    const { w, el } = setup();
    await w.setProps({ src: 'http://omystream.localhost/file/next' });
    // 不这样会怎样：复用组件仍在播放上一个文件，关闭后继续读密文。
    expect(el.getAttribute('src')).toBe('http://omystream.localhost/file/next');
    w.unmount();
    wrappers.pop();
    expect(el.getAttribute('src')).toBeNull();
    expect(HTMLMediaElement.prototype.pause).toHaveBeenCalled();
    expect(HTMLMediaElement.prototype.load).toHaveBeenCalled();
  });
});
