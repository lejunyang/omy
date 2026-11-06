/**
 * 右键菜单的「移到回收站」在没有系统回收站的平台上必须禁用。
 *
 * 不这样会怎样：Android 上点它只会报 trash_not_supported，紧挨着的「永久删除」
 * 就成了唯一能用的删除——用户会以为两者一样，误删无法找回。
 */
import { describe, it, expect, vi, afterEach } from 'vitest';

vi.mock('./api.js', () => ({ errCode: () => undefined, uiLog: vi.fn() }));

async function trashItem() {
  vi.resetModules();
  const store = await import('./store');
  store.state.entries = [{ path: '/d/a.txt', name: 'a.txt', is_dir: false, is_encrypted: false, token: 't' }];
  store.state.selected = ['/d/a.txt'];
  store.openContextMenu({ entry: store.state.entries[0], x: 0, y: 0 });
  return store.ctxItems.value.find((it) => it.key === 'trash');
}

describe('回收站菜单项与平台能力', () => {
  afterEach(() => {
    delete (window as any).omyAndroid;
  });

  it('Android 上禁用并给出原因', async () => {
    (window as any).omyAndroid = { setVideoLandscape() {} };
    const it_ = await trashItem();
    expect(it_?.disabled).toBe(true);
    expect(it_?.hint).toBeTruthy();
  });

  it('桌面上可用', async () => {
    const it_ = await trashItem();
    expect(it_?.disabled).toBeFalsy();
  });
});

describe('菜单按选中项类型裁掉永远用不上的项', () => {
  async function keysFor(entries: any[]) {
    vi.resetModules();
    const store = await import('./store');
    store.state.entries = entries;
    store.state.selected = entries.map((e) => e.path);
    store.openContextMenu({ entry: entries[0], x: 0, y: 0 });
    return store.ctxItems.value.map((it) => it.key);
  }

  it('普通文件没有密码与恢复码项', async () => {
    // 不这样会怎样：手机弹层里摆着四个永远是灰的项，删除被挤出屏幕
    const keys = await keysFor([{ path: '/d/a.txt', name: 'a.txt', token: 't' }]);
    expect(keys).toContain('encrypt');
    expect(keys).not.toContain('manage-key');
    expect(keys).not.toContain('recovery-generate');
    expect(keys).not.toContain('restore');
  });

  it('加密文件没有「加密」项，但保留密码相关项', async () => {
    const keys = await keysFor([{ path: '/d/x.omy', name: 'x.omy', is_encrypted: true, unlocked: false, token: 't' }]);
    expect(keys).not.toContain('encrypt');
    expect(keys).toContain('manage-key');
    expect(keys).toContain('recovery-restore');
  });
});
