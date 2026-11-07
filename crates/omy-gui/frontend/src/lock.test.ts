/**
 * 锁定后的界面清理。
 *
 * 锁定的保证在后端，但界面上已经渲染出来的解密内容（真名、选择、容器）只有
 * 前端能收。这里钉住两条「失败也要清」的路径：后端 lock 报错、重载列表失败。
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';

vi.mock('./api.js', () => ({
  lock: vi.fn(),
  browseDirectory: vi.fn(),
  remotePlaceList: vi.fn(),
  virtualPlaces: vi.fn(),
  deviceStatus: vi.fn(),
  shareStatus: vi.fn(),
  scanDirectory: vi.fn(),
  errCode: () => undefined,
  uiLog: vi.fn(),
}));

import * as api from './api';
import { state, lock, applyLockedState } from './store';

function seedUnlocked() {
  Object.assign(state, {
    cwd: '/d',
    credentials: 2,
    container: { entryId: 'e1', name: '相册', items: [], cwd: '' },
    selected: ['/d/a.omy'],
    entries: [
      { path: '/d/a.omy', name: 'a.omy', is_encrypted: true, unlocked: true, real_name: '真名.jpg' },
      { path: '/d/dir', name: 'MFRGG', is_dir: true, is_encrypted_dir: true, unlocked: true, real_name: '私密' },
      { path: '/d/plain.txt', name: 'plain.txt', is_encrypted: false, unlocked: false, real_name: null },
    ],
  });
}

describe('锁定后的界面清理', () => {
  beforeEach(() => {
    vi.mocked(api.remotePlaceList).mockResolvedValue([]);
    vi.mocked(api.virtualPlaces).mockResolvedValue([]);
    vi.mocked(api.deviceStatus).mockResolvedValue({ opened: false });
    vi.mocked(api.shareStatus).mockResolvedValue({ running: false });
    seedUnlocked();
  });

  it('重载列表失败时也不留解密出来的真名', async () => {
    // 不这样会怎样：navigate 失败会保留旧列表，屏幕上继续显示真名
    vi.mocked(api.browseDirectory).mockRejectedValue(new Error('io'));
    await applyLockedState();
    const names = state.entries.map((e) => e.real_name).filter(Boolean);
    expect(names).toEqual([]);
    expect(state.entries.every((e) => !(e.is_encrypted || e.is_encrypted_dir) || !e.unlocked)).toBe(true);
    expect(state.credentials).toBe(0);
    expect(state.container).toBeNull();
    expect(state.selected).toEqual([]);
  });

  it('后端 lock 报错时界面仍然切到锁定态', async () => {
    // 不这样会怎样：lock() 抛出后直接返回，凭据数与真名都留在界面上（fail-open）
    vi.mocked(api.lock).mockRejectedValue(new Error('boom'));
    vi.mocked(api.browseDirectory).mockResolvedValue([]);
    await expect(lock()).rejects.toThrow('boom');
    expect(state.credentials).toBe(0);
    expect(state.container).toBeNull();
  });

  it('并发触发的两次清理只重载一次列表', async () => {
    // 主动锁定与后端事件几乎同时到达，不合并的话每次锁定都要重载两遍目录
    vi.mocked(api.browseDirectory).mockResolvedValue([]);
    vi.mocked(api.browseDirectory).mockClear();
    await Promise.all([applyLockedState(), applyLockedState()]);
    expect(api.browseDirectory).toHaveBeenCalledTimes(1);
  });
});
