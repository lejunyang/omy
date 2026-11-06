/**
 * 本地目录浏览的竞态：后发先至时，界面必须停在最后一次点击的目录。
 *
 * 不这样会怎样：双击进 A、紧接着点面包屑回 B，A 的读目录晚回来，
 * 界面显示 A 的内容，而用户以为自己在 B——随后的删除、加密都作用在错的目录上。
 */
import { describe, it, expect, vi } from 'vitest';

vi.mock('./api.js', () => ({
  browseDirectory: vi.fn(),
  scanDirectory: vi.fn(async () => []),
  configGet: vi.fn(async () => ({ ui: {} })),
  configSet: vi.fn(),
  errCode: () => undefined,
  uiLog: vi.fn(),
}));

import * as api from './api';
import { state, navigate } from './store';

describe('navigate 竞态', () => {
  it('先发的请求后回来时被丢弃', async () => {
    let releaseA: (v: any) => void = () => {};
    vi.mocked(api.browseDirectory).mockImplementation((dir: string) =>
      dir === '/a'
        ? new Promise((r) => { releaseA = r; })
        : Promise.resolve([{ path: '/b/x', name: 'x' }]),
    );
    const a = navigate('/a');
    await navigate('/b');
    releaseA([{ path: '/a/old', name: 'old' }]);
    await a;
    expect(state.cwd).toBe('/b');
    expect(state.entries.map((e) => e.path)).toEqual(['/b/x']);
    expect(state.busy).toBe(false);
  });
});
