/**
 * 文件类型判定。网格与列表共用这一份，判错一类就两处一起错。
 */
import { describe, it, expect } from 'vitest';
import { fileKind } from './file-icons';

describe('fileKind', () => {
  it('锁定的加密文件只给「锁」，不泄露类型', () => {
    // 不这样会怎样：锁定条目按磁盘名或残留字段显示成视频图标，等于告诉旁人它是视频
    expect(fileKind({ is_encrypted: true, unlocked: false, name: 'a1b2.omy', ext: 'omy', preview: 'video' })).toBe('locked');
  });

  it('已解锁的加密文件按真名判类型，而不是 .omy', () => {
    expect(fileKind({ is_encrypted: true, unlocked: true, name: 'a1b2.omy', ext: 'omy', real_name: '旅行.MP4' })).toBe('video');
  });

  it('普通文件按扩展名，认不出时退回后端的预览类别', () => {
    expect(fileKind({ name: 'x.jpeg', ext: 'jpeg' })).toBe('image');
    expect(fileKind({ name: 'x.weird', ext: 'weird', preview: 'audio' })).toBe('audio');
    expect(fileKind({ name: 'x.bin', ext: 'bin' })).toBe('other');
  });

  it('目录区分是否为加密目录', () => {
    expect(fileKind({ is_dir: true })).toBe('folder');
    expect(fileKind({ is_dir: true, is_encrypted_dir: true })).toBe('folder-locked');
  });
});
