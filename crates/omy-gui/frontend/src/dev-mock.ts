/** 浏览器里预览界面用的 IPC 假数据。
 *
 * # 为什么需要它
 *
 * 动效与移动端布局要在浏览器里反复看、反复调：开 DevTools 的设备模拟、慢放
 * 动画、切 prefers-reduced-motion。每次都 `cargo run` 起一个 Tauri 窗口既慢，
 * 又没有这些工具。没有后端时界面只剩一个空壳，什么都看不到。
 *
 * # 只在开发态、且不在 Tauri 里时启用
 *
 * 入口（main.ts）用 `import.meta.env.DEV` 守住动态 import：生产构建里这个分支
 * 被常量折叠掉，本文件连同假数据都不会进包。另外还要求
 * `window.__TAURI_INTERNALS__` 不存在——`cargo run` 时 Vite dev server 也在跑，
 * 真窗口里绝不能被假数据顶替。
 *
 * # 数据不追求覆盖全部命令
 *
 * 只覆盖首屏与常用交互（浏览、预览、右键、远程位置列表、传输页）。没覆盖的
 * 命令统一 resolve(null)：组件自己有兜底，结果是「空列表」而不是报错弹窗。
 * 新增 mock 时按命令名加 case，不要在组件里判断「是不是 mock」。
 */

import { mockIPC, mockWindows } from '@tauri-apps/api/mocks';

const SEP = '/';
const ROOT = '/mock/home';

type MockEntry = {
  path: string;
  name: string;
  is_dir: boolean;
  size: number | null;
  is_encrypted: boolean;
  unlocked: boolean;
  real_name: string | null;
  entry_id: string | null;
  ext: string | null;
  token: string | null;
  preview: string | null;
  mime: string | null;
  is_container: boolean;
  is_encrypted_dir?: boolean;
};

function file(dir: string, name: string, size: number, preview: string | null = null): MockEntry {
  const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : null;
  return {
    path: dir + SEP + name, name, is_dir: false, size, is_encrypted: false, unlocked: false,
    real_name: null, entry_id: null, ext, token: 'tok:' + name, preview, mime: null, is_container: false,
  };
}

function folder(dir: string, name: string, encrypted = false): MockEntry {
  return {
    path: dir + SEP + name, name, is_dir: true, size: null, is_encrypted: false, unlocked: false,
    real_name: null, entry_id: null, ext: null, token: null, preview: null, mime: null,
    is_container: false, is_encrypted_dir: encrypted,
  };
}

function locked(dir: string, name: string, size: number): MockEntry {
  return {
    path: dir + SEP + name, name, is_dir: false, size, is_encrypted: true, unlocked: false,
    real_name: null, entry_id: 'e:' + name, ext: 'omy', token: 'tok:' + name, preview: null,
    mime: null, is_container: false,
  };
}

/** 按目录给出内容。只有几层，够演示进入/返回的转场。 */
function listDir(dir: string): MockEntry[] {
  if (dir === ROOT) {
    return [
      folder(ROOT, '照片'),
      folder(ROOT, '工作文档'),
      folder(ROOT, '私密相册', true),
      folder(ROOT, 'Downloads'),
      file(ROOT, '旅行计划.md', 4_812, 'text'),
      file(ROOT, '海边日落.jpg', 3_482_112, 'image'),
      file(ROOT, '家庭聚会.mp4', 412_993_331, 'video'),
      file(ROOT, '播客 第 12 期.mp3', 48_220_011, 'audio'),
      file(ROOT, '年度报表.xlsx', 220_481),
      file(ROOT, '合同扫描件.pdf', 1_934_002),
      locked(ROOT, 'a3f9c1.omy', 92_310_442),
      locked(ROOT, '7b21de.omy', 1_204_993),
      file(ROOT, '源码备份.zip', 88_102_331),
      file(ROOT, '屏幕录制 2026-10-01.mov', 902_331_221, 'video'),
    ];
  }
  const out: MockEntry[] = [];
  for (let i = 1; i <= 18; i++) {
    out.push(file(dir, `IMG_${String(2000 + i)}.jpg`, 1_000_000 + i * 73_331, 'image'));
  }
  out.unshift(folder(dir, '2025'), folder(dir, '2026'));
  return out;
}

const places = [
  { name: 'home', path: ROOT },
  { name: 'desktop', path: ROOT + '/Desktop' },
  { name: 'documents', path: ROOT + '/工作文档' },
  { name: 'downloads', path: ROOT + '/Downloads' },
  { name: 'pictures', path: ROOT + '/照片' },
  { name: 'videos', path: ROOT + '/Videos' },
];

const caps = { read: true, write: true, delete: true, rename: true, search: true, server_search: false };
const remotePlaces = [
  { id: 'p1', name: '坚果云', kind: 'webdav', caps },
  { id: 'p2', name: 'Telegram · 我的收藏', kind: 'telegram', caps: { ...caps, server_search: true }, encrypted: true, unlocked: false },
];

const transfers = [
  { id: 1, parent_id: null, kind: 'upload', name: '家庭聚会.mp4', state: 'running', done: 182_331_000, total: 412_993_331, speed: 6_221_331 },
  { id: 2, parent_id: null, kind: 'download', name: '合同扫描件.pdf', state: 'done', done: 1_934_002, total: 1_934_002, speed: 0 },
  { id: 3, parent_id: null, kind: 'copy', name: '照片（214 项）', state: 'paused', done: 30_000_000, total: 210_000_000, speed: 0 },
];

const config = {
  ui: { language: 'zh-CN', theme: 'dark', view: 'grid', startup: 'home', thumbnails: true },
  security: { auto_lock_secs: 900 },
  remote: { telegram_proxy_mode: 'system', cache_limit: 2_000_000_000 },
  encrypt: {},
  playback: {},
};

/** 模拟网络延迟：没有延迟就看不到加载态与骨架的过渡。 */
function delay<T>(v: T, ms = 260): Promise<T> {
  return new Promise((r) => setTimeout(() => r(v), ms));
}

export function installMockIpc(): void {
  mockWindows('main');
  mockIPC((cmd, args: any) => {
    switch (cmd) {
      case 'stream_base': return 'http://mock.invalid';
      case 'get_language': return 'zh-CN';
      case 'config_get': return structuredClone(config);
      case 'config_set': return null;
      case 'config_paths': return { config: '/mock/omy/config.toml', data: '/mock/omy' };
      case 'app_about': return { app_version: '0.0.1-mock' };
      case 'credential_count': return 1;
      case 'storage_access': return { granted: true, mode: 'not-applicable' };
      case 'list_places': return places;
      case 'browse_directory': return delay(listDir(args?.dir || ROOT));
      case 'parent_of': {
        const d: string = args?.path || '';
        if (d === ROOT || !d.startsWith(ROOT)) return null;
        return d.slice(0, d.lastIndexOf(SEP)) || ROOT;
      }
      case 'scan_directory': return [];
      case 'probe_one': return { is_encrypted: false };
      case 'remote_place_list': return remotePlaces;
      case 'virtual_places': return [{ id: 'v1', name: '精选合集', encrypted: false, unlocked: false }];
      case 'transfer_list': return transfers;
      case 'device_status': return { opened: true, paired_count: 2 };
      case 'paired_devices': return [];
      case 'share_status': return { running: false };
      case 'password_manager_status':
        return { provider: 'keepassxc', installed: false, running: false, database_open: false, associated: false, database_hash: null, version: null, detail: null };
      case 'remote_page_size': return 100;
      case 'remote_cache_usage': return { used: 812_331_221, limit: 2_000_000_000 };
      case 'remote_browse': return delay({ entries: [], caps });
      case 'list_file_associations': return [];
      case 'ui_log': return null;
      default:
        return null;
    }
  }, { shouldMockEvents: true });
}
