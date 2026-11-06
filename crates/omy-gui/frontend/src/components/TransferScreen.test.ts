/**
 * 传输行的字节进度与预计剩余时间。
 *
 * 速度是前端按两次快照算的，这里用假计时器驱动两次轮询，直接看行上的文字。
 * 载入真实的英文语言包：没有语言包时 t() 原样返回键名，断言就看不到算出来的时间。
 */
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, it, expect, vi, beforeAll, afterAll, beforeEach, afterEach } from 'vitest';
import { flushPromises, mount } from '@vue/test-utils';

const snapshots: any[][] = [];
vi.mock('../api', () => ({
  transferList: vi.fn(async () => snapshots.shift() ?? []),
  transferCancel: vi.fn(),
  transferRetry: vi.fn(),
  transferPause: vi.fn(),
  transferPauseAll: vi.fn(),
  transferClearDone: vi.fn(),
  errCode: () => undefined,
  uiLog: vi.fn(),
}));

import TransferScreen from './TransferScreen.vue';
import { formatDuration, formatSize, load } from '../i18n';

const MB = 1024 * 1024;
const task = (done: number, extra: Record<string, unknown> = {}) => ({
  id: 7, kind: 'upload', name: 'big.mkv', target: 'tg', state: 'running', done, total: 10 * MB, ...extra,
});

async function mountWith(first: any[], second: any[]) {
  snapshots.length = 0;
  snapshots.push(first, second);
  const w = mount(TransferScreen, {
    global: { stubs: { AppShell: { template: '<div><slot /></div>' }, AppIcon: true } },
  });
  await flushPromises();
  // 第二次轮询：1.2 秒后（与组件里的 setInterval 一致）
  await vi.advanceTimersByTimeAsync(1200);
  await flushPromises();
  return w;
}

beforeAll(async () => {
  vi.stubGlobal('fetch', async (url: string) => ({
    json: async () => JSON.parse(readFileSync(resolve(process.cwd(), 'public', String(url)), 'utf8')),
  }));
  await load('en');
});
afterAll(() => vi.unstubAllGlobals());

describe('传输行的字节与剩余时间', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('显示已传/总量，并按速度估出剩余时间', async () => {
    // 1.2 秒传了 1.2 MB → 1 MB/s；还剩 7.8 MB → 约 7.8 秒
    const w = await mountWith([task(1 * MB)], [task(2.2 * MB)]);
    const meta = w.find('[data-xf=bytes]');
    // 不这样会怎样：只有进度条，用户看不出 10 GB 的文件还剩多少、还要等多久
    expect(meta.text()).toContain(`${formatSize(2.2 * MB)} / ${formatSize(10 * MB)}`);
    expect(meta.text()).toContain(`${formatDuration(7.8 * 1000)} left`);
    w.unmount();
  });

  it('暂停的任务只显示字节，不给剩余时间', async () => {
    // 不这样会怎样：暂停后速度还停在旧值，界面上会一直挂着一个不会兑现的「剩余 0:07」
    const w = await mountWith([task(1 * MB)], [task(2.2 * MB, { state: 'paused' })]);
    const meta = w.find('[data-xf=bytes]');
    expect(meta.exists()).toBe(true);
    expect(meta.text()).not.toContain('left');
    w.unmount();
  });

  it('分组任务的 done/total 是文件个数，不当成字节显示', async () => {
    // 不这样会怎样：一个 3/12 个文件的分组会显示成「3 B / 12 B」
    const group = { id: 9, kind: 'upload', name: 'album', target: 'tg', state: 'running', is_group: true, done: 3, total: 12, children: [] };
    const w = await mountWith([group], [{ ...group, done: 4 }]);
    expect(w.find('[data-xf=bytes]').exists()).toBe(false);
    w.unmount();
  });
});
